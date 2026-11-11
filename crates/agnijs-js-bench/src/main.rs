//! `bench` command line: run the engine micro-benchmarks, print and compare
//! results files. Build with `--release`; debug numbers mean little.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use agnijs_js_bench::compare::{compare, Verdict};
use agnijs_js_bench::machine::Machine;
use agnijs_js_bench::memory::peak_rss_bytes;
use agnijs_js_bench::results::{Config, Entry, ResultFile, SCHEMA};
use agnijs_js_bench::runner::{run, EngineKind, RunConfig};
use agnijs_js_bench::stats::Summary;
use agnijs_js_bench::workloads::{registry, select};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "bench", about = "Engine micro-benchmarks on every engine behind the engine interface")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run workloads; each (engine, workload) pair runs in its own process.
    Run {
        /// Engines to run (`agnijs`, `quickjs`). Repeatable; default both.
        #[arg(long = "engine")]
        engines: Vec<String>,
        /// Workload name substring or category. Repeatable; default all.
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        #[arg(long, default_value_t = 1)]
        warmup: usize,
        /// Multiplies every workload's work count.
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Write the results file here.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Free-form label stored in the results file (e.g. a commit id).
        #[arg(long)]
        label: Option<String>,
    },
    /// Run one pair in this process and print it as JSON (used by `run`).
    #[command(hide = true)]
    Child {
        #[arg(long)]
        engine: String,
        #[arg(long)]
        workload: String,
        #[arg(long)]
        iterations: usize,
        #[arg(long)]
        warmup: usize,
        #[arg(long)]
        scale: f64,
    },
    /// Print a results file as a table.
    Report { file: PathBuf },
    /// Compare two results files (new over old, per workload and engine).
    Compare {
        old: PathBuf,
        new: PathBuf,
        /// Fractional change treated as real.
        #[arg(long, default_value_t = 0.10)]
        threshold: f64,
        /// Exit with status 1 when any pair is slower beyond the threshold.
        #[arg(long)]
        fail_on_regression: bool,
    },
    /// List the workloads.
    List,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run { engines, filters, iterations, warmup, scale, output, label } => {
            let engines = if engines.is_empty() {
                EngineKind::ALL.to_vec()
            } else {
                engines
                    .iter()
                    .map(|e| EngineKind::parse(e).with_context(|| format!("unknown engine {e}")))
                    .collect::<Result<_>>()?
            };
            let all = registry();
            let chosen = select(&all, &filters);
            if chosen.is_empty() {
                bail!("no workload matches {filters:?}");
            }
            if cfg!(debug_assertions) {
                eprintln!("warning: debug build; run with --release for meaningful numbers");
            }
            let created_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let exe = std::env::current_exe()?;
            let mut results = Vec::new();
            for w in &chosen {
                for engine in &engines {
                    if !engine.supports(w) {
                        continue;
                    }
                    eprint!("{:<20} {:<8} ", w.name, engine.name());
                    let out = Command::new(&exe)
                        .args(["child", "--engine", engine.name(), "--workload", w.name])
                        .args(["--iterations", &iterations.to_string(), "--warmup", &warmup.to_string()])
                        .args(["--scale", &scale.to_string()])
                        .output()?;
                    if !out.status.success() {
                        bail!("{} on {} failed:\n{}", w.name, engine.name(), String::from_utf8_lossy(&out.stderr));
                    }
                    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
                    let samples_ns: Vec<u64> = serde_json::from_value(v["samples_ns"].clone())?;
                    let summary = Summary::from_samples(&samples_ns).context("no samples")?;
                    eprintln!("{}", ms(summary.median_ns));
                    results.push(Entry {
                        workload: w.name.into(),
                        category: w.category.name().into(),
                        engine: engine.name().into(),
                        summary,
                        samples_ns,
                        checksum: v["checksum"].as_str().unwrap_or_default().into(),
                        heap_bytes: v["heap_bytes"].as_u64().unwrap_or(0) as usize,
                        object_count: v["object_count"].as_u64().unwrap_or(0) as usize,
                        peak_rss_bytes: v["peak_rss_bytes"].as_u64(),
                    });
                }
            }
            let file = ResultFile {
                schema: SCHEMA.into(),
                created_unix,
                label,
                machine: Machine::detect(),
                config: Config { scale, warmup, iterations },
                results,
            };
            print_report(&file);
            if let Some(path) = output {
                std::fs::write(&path, file.to_json() + "\n").with_context(|| format!("write {}", path.display()))?;
                eprintln!("wrote {}", path.display());
            }
            let mismatched = checksum_mismatches(&file);
            if !mismatched.is_empty() {
                bail!("engines disagree on checksums: {mismatched:?}");
            }
        }
        Cmd::Child { engine, workload, iterations, warmup, scale } => {
            let engine = EngineKind::parse(&engine).with_context(|| format!("unknown engine {engine}"))?;
            let w = registry().into_iter().find(|w| w.name == workload).with_context(|| format!("unknown workload {workload}"))?;
            let m = run(engine, &w, &RunConfig { scale, warmup, iterations })?;
            println!(
                "{}",
                json!({
                    "samples_ns": m.samples_ns,
                    "checksum": m.checksum,
                    "heap_bytes": m.heap_bytes,
                    "object_count": m.object_count,
                    "peak_rss_bytes": peak_rss_bytes(),
                })
            );
        }
        Cmd::Report { file } => print_report(&read(&file)?),
        Cmd::Compare { old, new, threshold, fail_on_regression } => {
            let (old, new) = (read(&old)?, read(&new)?);
            if old.machine != new.machine {
                eprintln!("note: the two runs are from different machines or toolchains");
            }
            let cmp = compare(&old, &new, threshold);
            println!("{:<20} {:<8} {:>11} {:>11} {:>7}", "workload", "engine", "old", "new", "new/old");
            for c in &cmp.changes {
                let mark = match c.verdict {
                    Verdict::Slower => "  slower",
                    Verdict::Faster => "  faster",
                    Verdict::Unchanged => "",
                };
                println!(
                    "{:<20} {:<8} {:>11} {:>11} {:>7.2}{mark}",
                    c.workload,
                    c.engine,
                    ms(c.old_median_ns),
                    ms(c.new_median_ns),
                    c.ratio
                );
            }
            for p in &cmp.only_old {
                println!("{p}: only in the old file");
            }
            for p in &cmp.only_new {
                println!("{p}: only in the new file");
            }
            let slower = cmp.regressions().count();
            println!("{slower} slower beyond {:.0}%", threshold * 100.0);
            if fail_on_regression && slower > 0 {
                std::process::exit(1);
            }
        }
        Cmd::List => {
            for w in registry() {
                println!("{:<20} {:<16} {}", w.name, w.category.name(), w.description);
            }
        }
    }
    Ok(())
}

fn read(path: &PathBuf) -> Result<ResultFile> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    ResultFile::from_json(&text).with_context(|| format!("parse {}", path.display()))
}

fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1e6)
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map(|b| format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))).unwrap_or_else(|| "-".into())
}

fn checksum_mismatches(file: &ResultFile) -> Vec<String> {
    let mut out = Vec::new();
    for e in &file.results {
        let first = file.results.iter().find(|o| o.workload == e.workload).unwrap();
        if first.checksum != e.checksum && !out.contains(&e.workload) {
            out.push(e.workload.clone());
        }
    }
    out
}

fn print_report(file: &ResultFile) {
    let m = &file.machine;
    println!(
        "{} {} | {} | {} logical CPUs | {} | {} build | scale {} | {} iterations after {} warmup",
        m.os,
        m.arch,
        m.cpu,
        m.logical_cpus,
        m.rustc.as_deref().unwrap_or("rustc ?"),
        m.profile,
        file.config.scale,
        file.config.iterations,
        file.config.warmup
    );
    let (a, b) = (EngineKind::Owned.name(), EngineKind::QuickJs.name());
    println!(
        "{:<20} {:>11} {:>11} {:>8} {:>11} {:>11}",
        "workload",
        format!("{a} med"),
        format!("{b} med"),
        format!("{a}/{b}"),
        format!("{a} peak"),
        format!("{b} peak")
    );
    let mut seen = Vec::new();
    for e in &file.results {
        if seen.contains(&e.workload) {
            continue;
        }
        seen.push(e.workload.clone());
        let ea = file.entry(&e.workload, a);
        let eb = file.entry(&e.workload, b);
        let med = |x: Option<&Entry>| x.map(|x| ms(x.summary.median_ns)).unwrap_or_else(|| "-".into());
        let ratio = file.engine_ratio(&e.workload, a, b).map(|r| format!("{r:.2}x")).unwrap_or_else(|| "-".into());
        println!(
            "{:<20} {:>11} {:>11} {:>8} {:>11} {:>11}",
            e.workload,
            med(ea),
            med(eb),
            ratio,
            mib(ea.and_then(|x| x.peak_rss_bytes)),
            mib(eb.and_then(|x| x.peak_rss_bytes))
        );
    }
}
