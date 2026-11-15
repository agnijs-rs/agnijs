//! The bounded fuzz runner (`docs/fuzzing.md`).
//!
//! ```text
//! agnijs-fuzz [TARGET | all]... [--iters N | --scale N | --time SECS] [--seed N] [--out DIR]
//! agnijs-fuzz --list
//! agnijs-fuzz --repro TARGET FILE...
//! agnijs-fuzz --seeds TARGET DIR
//! ```
//!
//! Each target runs its seeds, then mutated inputs. A fixed iteration
//! count and seed make a run reproducible, which is what CI uses; `--time`
//! trades that for a wall-clock budget per target. The first failing input
//! of a target is saved under `--out` and the run exits 1.

use agnijs_fuzz::{mutate, Shape, Target, TARGETS};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "usage:
  agnijs-fuzz [TARGET | all]... [--iters N | --scale N | --time SECS] [--seed N] [--out DIR]
  agnijs-fuzz --list
  agnijs-fuzz --repro TARGET FILE...
  agnijs-fuzz --seeds TARGET DIR";

/// Mutated inputs kept to mutate further, beside the seeds.
const POOL: usize = 256;

enum Budget {
    Iters(usize),
    Scale(usize),
    Time(Duration),
}

struct Options {
    targets: Vec<&'static Target>,
    budget: Budget,
    seed: u64,
    out: PathBuf,
}

fn main() -> ExitCode {
    // Inputs nest deeply; give the engine's stack budget room in debug
    // builds too.
    let worker = std::thread::Builder::new().stack_size(64 << 20).spawn(run);
    match worker.map(|w| w.join()) {
        Ok(Ok(code)) => code,
        _ => ExitCode::from(3),
    }
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("--list") => {
            for t in TARGETS {
                println!("{:<14} {:>6}  {}", t.name, t.default_iters, t.about);
            }
            ExitCode::SUCCESS
        }
        Some("--repro") => repro(&args[1..]),
        Some("--seeds") => write_seeds(&args[1..]),
        _ => match parse_options(&args) {
            Ok(o) => fuzz(&o),
            Err(e) => {
                eprintln!("{e}\n{USAGE}");
                ExitCode::from(2)
            }
        },
    }
}

fn find(name: &str) -> Result<&'static Target, String> {
    agnijs_fuzz::target(name).ok_or_else(|| format!("unknown target {name:?}; --list shows them"))
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut o = Options { targets: Vec::new(), budget: Budget::Scale(1), seed: 1, out: PathBuf::from("target/fuzz-inputs") };
    let mut it = args.iter();
    let number = |flag: &str, v: Option<&String>| -> Result<u64, String> {
        v.and_then(|v| v.parse().ok()).ok_or_else(|| format!("{flag} needs a number"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--iters" => o.budget = Budget::Iters(number(a, it.next())? as usize),
            "--scale" => o.budget = Budget::Scale(number(a, it.next())? as usize),
            "--time" => o.budget = Budget::Time(Duration::from_secs(number(a, it.next())?)),
            "--seed" => o.seed = number(a, it.next())?,
            "--out" => o.out = PathBuf::from(it.next().ok_or("--out needs a directory")?),
            "all" => o.targets.extend(TARGETS.iter()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            name => o.targets.push(find(name)?),
        }
    }
    if o.targets.is_empty() {
        o.targets.extend(TARGETS.iter());
    }
    Ok(o)
}

/// Run one input; the panic message is printed by the panic hook.
fn check(t: &Target, input: &[u8]) -> bool {
    catch_unwind(AssertUnwindSafe(|| (t.run)(input))).is_ok()
}

fn extension(t: &Target) -> &'static str {
    match t.shape {
        Shape::Text => "txt",
        Shape::Binary => "bin",
    }
}

fn save(o: &Options, t: &Target, label: &str, input: &[u8]) -> PathBuf {
    let path = o.out.join(format!("{}-{label}.{}", t.name, extension(t)));
    if let Err(e) = std::fs::create_dir_all(&o.out).and_then(|_| std::fs::write(&path, input)) {
        eprintln!("could not save the input to {}: {e}", path.display());
    }
    path
}

fn fuzz(o: &Options) -> ExitCode {
    let mut failed = Vec::new();
    for t in &o.targets {
        let started = Instant::now();
        let seeds = (t.seeds)();
        agnijs_fuzz::take_accepted();
        let mut rng = mutate::Rng::new(o.seed ^ hash(t.name));
        let (mut runs, mut slowest) = (0usize, Duration::ZERO);
        let mut timed = |input: &[u8]| {
            let at = Instant::now();
            let ok = check(t, input);
            slowest = slowest.max(at.elapsed());
            ok
        };
        let mut failure = None;
        for (i, s) in seeds.iter().enumerate() {
            runs += 1;
            if !timed(s) {
                failure = Some((format!("seed{i}"), s.clone()));
                break;
            }
        }
        // A target that counts accepted inputs keeps only mutants that got
        // past its loader or parser, so that edits accumulate on inputs
        // that reach the code behind it; one that does not count keeps a
        // random quarter.
        let mut accepted = agnijs_fuzz::take_accepted();
        let counts = accepted > 0;
        let iters = match o.budget {
            Budget::Iters(n) => n,
            Budget::Scale(k) => t.default_iters.saturating_mul(k.max(1)),
            Budget::Time(_) => usize::MAX,
        };
        let mut pool = seeds.clone();
        let mut i = 0;
        while failure.is_none() && i < iters && !pool.is_empty() {
            if let Budget::Time(limit) = o.budget {
                if started.elapsed() >= limit {
                    break;
                }
            }
            let base = &pool[rng.below(pool.len())];
            let input = match t.shape {
                Shape::Text => mutate::text(&mut rng, base, &pool),
                Shape::Binary => mutate::binary(&mut rng, base, &pool),
            };
            runs += 1;
            if !timed(&input) {
                failure = Some((format!("seed{}-iter{i}", o.seed), input));
                break;
            }
            let got = agnijs_fuzz::take_accepted();
            accepted += got;
            if if counts { got > 0 && rng.chance(2) } else { rng.chance(4) } {
                if pool.len() < seeds.len() + POOL {
                    pool.push(input);
                } else {
                    let slot = seeds.len() + rng.below(POOL);
                    pool[slot] = input;
                }
            }
            i += 1;
        }
        let elapsed = started.elapsed();
        let accepted = match accepted {
            0 => String::new(),
            n => format!(", {n} accepted"),
        };
        match failure {
            None => println!(
                "{:<14} ok    {runs:>7} inputs in {:>6.1} s, slowest {} ms{accepted}",
                t.name,
                elapsed.as_secs_f64(),
                slowest.as_millis()
            ),
            Some((label, input)) => {
                let path = save(o, t, &label, &input);
                println!("{:<14} FAIL  after {runs} inputs: saved {}", t.name, path.display());
                println!("               reproduce: agnijs-fuzz --repro {} {}", t.name, path.display());
                failed.push(t.name);
            }
        }
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        println!("failing targets: {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// FNV-1a, so each target gets its own stream from one `--seed`.
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

fn repro(args: &[String]) -> ExitCode {
    let Some((name, files)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let t = match find(name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let mut ok = true;
    for f in files {
        match std::fs::read(f) {
            Ok(input) => {
                let passed = check(t, &input);
                println!("{f}: {}", if passed { "ok" } else { "FAIL" });
                ok &= passed;
            }
            Err(e) => {
                eprintln!("{f}: {e}");
                ok = false;
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn write_seeds(args: &[String]) -> ExitCode {
    let [name, dir] = args else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let t = match find(name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let dir = Path::new(dir);
    let seeds = (t.seeds)();
    let written = std::fs::create_dir_all(dir).and_then(|_| {
        seeds.iter().enumerate().try_for_each(|(i, s)| std::fs::write(dir.join(format!("seed-{i:03}")), s))
    });
    match written {
        Ok(()) => {
            println!("wrote {} seeds to {}", seeds.len(), dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}
