//! `test262` command line: fetch the pinned suite, run it, compare to a
//! baseline.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use agnijs_engine_api::{
    ConsoleLevel, Engine, Host, ImportAttribute, ModuleSource, RealmId, RegexMatcher,
    RejectionEvent, Value,
};
use agnijs_js_test262::exclusions::Exclusions;
use agnijs_js_test262::features::{parse_name_list, FeatureCut, FeatureList};
use agnijs_js_test262::runner::Mode;
use agnijs_js_test262::suite::{self, Baseline, RunOptions, Selection};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "test262", about = "Run test262 against a AgniJS engine")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Clone the pinned test262 commit into `test262`.
    Fetch {
        #[arg(long, default_value = "https://github.com/tc39/test262.git")]
        remote: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Run the suite.
    Run {
        /// Engine under test: `quickjs` (full run) or `parse` (agnijs-js-parser, parse only).
        #[arg(long, default_value = "quickjs")]
        engine: String,
        /// Path to the test262 checkout (default: `<crate>/../../test262`).
        #[arg(long)]
        test262: Option<PathBuf>,
        /// Only run tests under these prefixes (relative to the checkout,
        /// e.g. `test/built-ins/Array/`). Repeatable.
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long, default_value_t = 0)]
        threads: usize,
        /// Per-run time limit in milliseconds.
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
        /// Exclusions file (default: `<crate>/exclusions/<engine>.txt`).
        #[arg(long)]
        exclusions: Option<PathBuf>,
        /// Baseline to compare with (default: `<crate>/baselines/<engine>.json`).
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Rewrite the baseline from this run instead of comparing.
        #[arg(long)]
        update_baseline: bool,
        /// Skip the baseline comparison entirely.
        #[arg(long)]
        no_baseline: bool,
        /// Write the full JSON report here.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Also run `test/intl402`, reported in its own directory bucket.
        #[arg(long)]
        include_intl402: bool,
        /// Run only `test/intl402`, compared with (or, with
        /// `--update-baseline`, written to) its own baseline
        /// `<crate>/baselines/<engine>-intl402.json`.
        #[arg(long)]
        only_intl402: bool,
        /// Print every failure message.
        #[arg(long)]
        verbose: bool,
        /// Run every script and module from serialised bytecode
        /// (`compile_script`/`compile_module` then load), against the same
        /// exclusions and baseline. Engines with a bytecode format only.
        #[arg(long)]
        precompile: bool,
        /// The engine's regular-expression matcher policy (`Host::regex_matcher`):
        /// `auto` (the default), `backtracking-only` or `linear-when-eligible`. Same baseline either way.
        #[arg(long, default_value = "auto")]
        regex_matcher: String,
    },
    /// Run one JS file against an engine with the test262 harness loaded
    /// (for debugging; not part of the conformance run).
    Eval {
        #[arg(long, default_value = "agnijs")]
        engine: String,
        #[arg(long)]
        test262: Option<PathBuf>,
        /// Treat the file as a module.
        #[arg(long)]
        module: bool,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
        /// Print the compiled bytecode instead of running.
        #[arg(long)]
        dump: bool,
        file: PathBuf,
    },
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn default_test262_dir() -> PathBuf {
    crate_dir().join("../../test262")
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Eval {
            engine,
            test262,
            module,
            timeout_ms,
            dump,
            file,
        } => {
            if dump {
                let source = std::fs::read_to_string(&file)?;
                let code = if module {
                    agnijs_js_compiler::compile_module(&source, "dump")
                } else {
                    agnijs_js_compiler::compile_script(&source, "dump")
                };
                match code {
                    Ok(c) => dump_code(&c, 0),
                    Err(e) => println!("SyntaxError: {e}"),
                }
                return Ok(());
            }
            let root = test262.unwrap_or_else(default_test262_dir);
            let harness = suite::load_harness(&root)?;
            let Mode::Engine(factory) = factory_for(&engine, RegexMatcher::Auto)? else {
                bail!("eval needs a real engine");
            };
            let source = std::fs::read_to_string(&file)?;
            let path = if file.is_absolute() {
                file.clone()
            } else {
                std::env::current_dir()?.join(&file)
            };
            match agnijs_js_test262::runner::eval_file(
                &factory,
                &path,
                &source,
                &harness,
                module,
                Duration::from_millis(timeout_ms),
            ) {
                Ok(prints) => {
                    for p in prints {
                        println!("{p}");
                    }
                    Ok(())
                }
                Err((name, msg, prints)) => {
                    for p in prints {
                        println!("{p}");
                    }
                    println!("Uncaught {name}: {msg}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Fetch { remote, dir } => fetch(&remote, &dir.unwrap_or_else(default_test262_dir)),
        Cmd::Run {
            engine,
            test262,
            filters,
            threads,
            timeout_ms,
            exclusions,
            baseline,
            update_baseline,
            no_baseline,
            output,
            include_intl402,
            only_intl402,
            verbose,
            precompile,
            regex_matcher,
        } => run(RunArgs {
            engine,
            test262: test262.unwrap_or_else(default_test262_dir),
            filters,
            threads,
            timeout: Duration::from_millis(timeout_ms),
            exclusions,
            baseline,
            update_baseline,
            no_baseline,
            output,
            include_intl402,
            only_intl402,
            verbose,
            precompile,
            regex_matcher: parse_regex_matcher(&regex_matcher)?,
        }),
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .context("running git")?;
    if !status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(())
}

fn fetch(remote: &str, dir: &Path) -> Result<()> {
    let sha = agnijs_js_test262::pinned_commit();
    if dir.join(".git").exists() {
        let head = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()?;
        if String::from_utf8_lossy(&head.stdout).trim() == sha {
            println!("test262 already at {sha}");
            return Ok(());
        }
    } else {
        std::fs::create_dir_all(dir)?;
        git(dir, &["init", "-q"])?;
        // Tests such as Function/prototype/toString/line-terminator-normalisation-*
        // depend on the exact line endings, so never let core.autocrlf (the
        // Windows default) rewrite the checkout.
        git(dir, &["config", "core.autocrlf", "false"])?;
    }
    println!("fetching test262 {sha} from {remote}");
    git(dir, &["fetch", "-q", "--depth", "1", remote, sha])?;
    git(dir, &["checkout", "-q", "--detach", "FETCH_HEAD"])?;
    println!("test262 checked out at {}", dir.display());
    Ok(())
}

struct RunArgs {
    engine: String,
    test262: PathBuf,
    filters: Vec<String>,
    threads: usize,
    timeout: Duration,
    exclusions: Option<PathBuf>,
    baseline: Option<PathBuf>,
    update_baseline: bool,
    no_baseline: bool,
    output: Option<PathBuf>,
    include_intl402: bool,
    only_intl402: bool,
    verbose: bool,
    precompile: bool,
    regex_matcher: RegexMatcher,
}

fn parse_regex_matcher(name: &str) -> Result<RegexMatcher> {
    match name {
        "auto" => Ok(RegexMatcher::Auto),
        "backtracking-only" => Ok(RegexMatcher::BacktrackingOnly),
        "linear-when-eligible" => Ok(RegexMatcher::LinearWhenEligible),
        other => bail!("unknown --regex-matcher {other:?}; available: auto, backtracking-only, linear-when-eligible"),
    }
}

fn factory_for(engine: &str, regex: RegexMatcher) -> Result<Mode> {
    if regex != RegexMatcher::Auto {
        let Mode::Engine(inner) = factory_for(engine, RegexMatcher::Auto)? else {
            bail!("--regex-matcher needs an engine that runs code");
        };
        return Ok(Mode::Engine(Arc::new(move |host| {
            inner(Rc::new(MatcherHost { inner: host, regex }))
        })));
    }
    match engine {
        "quickjs" => Ok(Mode::Engine(Arc::new(|host| {
            agnijs_engine_quickjs::make_engine(host)
        }))),
        "agnijs" => Ok(Mode::Engine(Arc::new(|host| {
            agnijs_js_vm::make_engine(host)
        }))),
        "parse" => Ok(Mode::ParseOnly),
        "compile" => Ok(Mode::CompileOnly),
        other => bail!("unknown engine {other:?}; available: quickjs, agnijs, parse, compile"),
    }
}

fn run(args: RunArgs) -> Result<()> {
    let root = &args.test262;
    if !root.join("harness/assert.js").exists() {
        bail!(
            "no test262 checkout at {}; run `test262 fetch` first",
            root.display()
        );
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let pinned = agnijs_js_test262::pinned_commit();
    if commit != pinned {
        eprintln!("warning: checkout is at {commit}, PIN says {pinned}");
    }

    let factory = match (factory_for(&args.engine, args.regex_matcher)?, args.precompile) {
        (Mode::Engine(f), true) => Mode::Precompiled(f),
        (_, true) => bail!("--precompile needs an engine that runs code"),
        (mode, false) => mode,
    };
    let features = FeatureList::parse(&std::fs::read_to_string(root.join("features.txt"))?);
    let post = parse_name_list(&std::fs::read_to_string(
        crate_dir().join("post-es2025-features.txt"),
    )?);
    let cut = FeatureCut::new(&features, &post);
    for name in &cut.unknown_post_2025 {
        eprintln!(
            "warning: post-es2025-features.txt names {name:?}, which features.txt does not list"
        );
    }
    let exclusions_path = args
        .exclusions
        .unwrap_or_else(|| crate_dir().join(format!("exclusions/{}.txt", args.engine)));
    let exclusions = match std::fs::read_to_string(&exclusions_path) {
        Ok(text) => Exclusions::parse(&text)
            .map_err(|e| anyhow::anyhow!("{}: {e}", exclusions_path.display()))?,
        Err(_) => {
            eprintln!(
                "warning: no exclusions file at {}",
                exclusions_path.display()
            );
            Exclusions::default()
        }
    };

    let filters = if args.only_intl402 && args.filters.is_empty() {
        vec!["test/intl402/".to_string()]
    } else {
        args.filters.clone()
    };
    let sel = Selection {
        cut: &cut,
        exclusions: &exclusions,
        filters,
        include_intl402: args.include_intl402 || args.only_intl402,
    };
    let (cases, skipped) = suite::discover(root, &sel)?;
    let harness = suite::load_harness(root)?;
    let threads = if args.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        args.threads
    };
    eprintln!(
        "{} tests selected, {} skipped, {threads} threads",
        cases.len(),
        skipped.len()
    );

    let opts = RunOptions {
        timeout: args.timeout,
        threads,
        engine_name: args.engine.clone(),
        test262_commit: commit,
    };
    let started = std::time::Instant::now();
    let report = suite::run_all(&cases, skipped, factory, &harness, &opts, &|n, total| {
        if n % 500 == 0 || n == total {
            eprintln!("  {n}/{total}");
        }
    })?;
    eprintln!("finished in {:.1}s", started.elapsed().as_secs_f64());

    println!(
        "{:<12} {:>7} {:>7} {:>7} {:>8}",
        "directory", "tests", "passed", "failed", "skipped"
    );
    for (dir, t) in &report.directories {
        println!(
            "{:<12} {:>7} {:>7} {:>7} {:>8}",
            dir, t.tests, t.passed, t.failed, t.skipped
        );
    }
    let t = &report.totals;
    println!(
        "{:<12} {:>7} {:>7} {:>7} {:>8}",
        "total", t.tests, t.passed, t.failed, t.skipped
    );

    if args.verbose {
        for test in &report.results {
            for r in &test.runs {
                if let Some(m) = &r.message {
                    let mode = match r.strict {
                        Some(true) => "strict",
                        Some(false) => "sloppy",
                        None => "once",
                    };
                    println!("FAIL {} [{mode}] {m}", test.path);
                }
            }
        }
    }

    if let Some(out) = &args.output {
        suite::write_json(out, &report)?;
        println!("report written to {}", out.display());
    }

    let baseline_path = args
        .baseline
        .unwrap_or_else(|| {
            let suffix = if args.only_intl402 { "-intl402" } else { "" };
            crate_dir().join(format!("baselines/{}{}.json", args.engine, suffix))
        });
    if args.update_baseline {
        if !args.filters.is_empty() {
            bail!("--update-baseline needs a full run (no --filter)");
        }
        suite::write_json(&baseline_path, &Baseline::from_report(&report))?;
        println!("baseline written to {}", baseline_path.display());
        return Ok(());
    }
    if args.no_baseline {
        return Ok(());
    }
    let baseline: Baseline = match suite::read_json(&baseline_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("no usable baseline ({e}); pass --update-baseline to create one or --no-baseline to skip");
            std::process::exit(if report.totals.failed == 0 { 0 } else { 1 });
        }
    };
    let diff = baseline.compare(&report);
    for p in &diff.fixed {
        println!("FIXED {p} (remove it from the baseline with --update-baseline)");
    }
    for p in &diff.regressions {
        let msg = report
            .results
            .iter()
            .find(|t| &t.path == p)
            .and_then(|t| t.runs.iter().find_map(|r| r.message.clone()))
            .unwrap_or_default();
        println!("REGRESSION {p}: {msg}");
    }
    println!(
        "{} regressions, {} newly passing",
        diff.regressions.len(),
        diff.fixed.len()
    );
    if !diff.regressions.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

fn dump_code(code: &agnijs_js_compiler::bytecode::CodeBlock, depth: usize) {
    let pad = "  ".repeat(depth);
    println!(
        "{pad}== {} ({:?}/{:?}) strict={} regs={} params={} ",
        code.name,
        code.kind,
        code.function_kind,
        code.strict,
        code.register_count,
        code.param_count
    );
    for (i, ins) in code.instrs.iter().enumerate() {
        println!("{pad}{i:5}  {ins:?}");
    }
    for h in &code.handlers {
        println!("{pad}  handler {h:?}");
    }
    for f in &code.functions {
        dump_code(f, depth + 1);
    }
}

/// The runner's host with a different `regex_matcher` answer; every other
/// hook goes to the wrapped host.
struct MatcherHost {
    inner: Rc<dyn Host>,
    regex: RegexMatcher,
}

impl Host for MatcherHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> std::result::Result<String, String> {
        self.inner.resolve_module(referrer, specifier, attributes)
    }
    fn load_module(&self, resolved: &str) -> std::result::Result<ModuleSource, String> {
        self.inner.load_module(resolved)
    }
    fn import_meta(
        &self,
        engine: &mut dyn Engine,
        realm: RealmId,
        module: &str,
        meta: &Value,
    ) -> agnijs_engine_api::Result<()> {
        self.inner.import_meta(engine, realm, module, meta)
    }
    fn promise_rejection(&self, engine: &mut dyn Engine, realm: RealmId, event: RejectionEvent) {
        self.inner.promise_rejection(engine, realm, event)
    }
    fn can_compile_strings(&self, realm: RealmId) -> bool {
        self.inner.can_compile_strings(realm)
    }
    fn console(&self, realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        self.inner.console(realm, level, args)
    }
    fn interrupt(&self) -> bool {
        self.inner.interrupt()
    }
    fn now_ms(&self) -> f64 {
        self.inner.now_ms()
    }
    fn time_zone_offset_ms(&self, epoch_ms: f64, is_utc: bool) -> f64 {
        self.inner.time_zone_offset_ms(epoch_ms, is_utc)
    }
    fn time_zone_name(&self, epoch_ms: f64) -> String {
        self.inner.time_zone_name(epoch_ms)
    }
    fn default_locale(&self) -> String {
        self.inner.default_locale()
    }
    fn default_time_zone(&self) -> String {
        self.inner.default_time_zone()
    }
    fn regex_matcher(&self) -> RegexMatcher {
        self.regex
    }
}
