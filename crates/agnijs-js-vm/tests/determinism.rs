//! Deterministic mode (TASK-103.1, docs/determinism.md): with every
//! nondeterministic input supplied by the host, a program gives the same
//! bytes on every run, platform and build.
//!
//! The corpus is `tests/determinism/*.js`. Each program pushes lines onto a
//! global `out`; the harness runs it and its jobs under a fixed host and
//! compares `out.join("\n")` with the checked-in `.txt` beside it. CI runs
//! this on Linux, Windows and macOS, with and without the JIT, so the
//! golden files pin the output across platforms and builds. After a
//! deliberate change, regenerate them with `AGNIJS_BLESS=1`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use agnijs_engine_api::{Engine, Host, RealmId, Value};
use agnijs_js_vm::Vm;

/// A host that fixes every input the engine takes from the outside world.
struct Fixed {
    seed: u64,
}

impl Host for Fixed {
    fn random_seed(&self) -> u64 {
        self.seed
    }
    fn now_ms(&self) -> f64 {
        1_700_000_000_123.0
    }
    fn time_zone_offset_ms(&self, _epoch_ms: f64, _is_utc: bool) -> f64 {
        // UTC+05:30, so local time differs from UTC by a fraction of an hour.
        19_800_000.0
    }
    fn time_zone_name(&self, _epoch_ms: f64) -> String {
        "India Standard Time".to_string()
    }
    fn default_locale(&self) -> String {
        "en-US".to_string()
    }
    fn default_time_zone(&self) -> String {
        "UTC".to_string()
    }
}

fn engine(seed: u64) -> (Box<dyn Engine>, RealmId) {
    let vm = Vm::new(Rc::new(Fixed { seed }));
    let realm = vm.main_realm();
    (Box::new(vm), realm)
}

fn eval_string(e: &mut dyn Engine, realm: RealmId, src: &str) -> String {
    let v = e.eval_script(realm, "t.js", src).unwrap_or_else(|err| panic!("{err}: {src}"));
    e.to_string(realm, &v).unwrap()
}

/// Run one corpus program: its output, and with `metered` the fuel it
/// used. A budget keeps the program in the interpreter, so the golden
/// comparison runs unmetered, where the JIT (when built in and enabled
/// with `AGNIJS_JIT`) takes part.
fn run(name: &str, src: &str, metered: bool) -> (String, Option<u64>) {
    let (mut e, realm) = engine(0x5EED);
    let budget = 50_000_000;
    if metered {
        e.set_fuel(Some(budget)).unwrap();
    }
    let _: Value = e.eval_script(realm, name, src).unwrap_or_else(|err| panic!("{name}: {err}"));
    e.run_jobs(realm).unwrap_or_else(|err| panic!("{name} (jobs): {err}"));
    let used = e.fuel().map(|left| budget - left);
    e.set_fuel(None).unwrap();
    (eval_string(&mut *e, realm, "out.join('\\n')"), used)
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/determinism");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "js"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no corpus in {}", dir.display());
    files
}

#[test]
fn math_random_follows_the_host_seed() {
    let draw = |seed| {
        let (mut e, realm) = engine(seed);
        eval_string(&mut *e, realm, "Array.from({ length: 8 }, Math.random).join()")
    };
    assert_eq!(draw(7), draw(7));
    assert_ne!(draw(7), draw(8));
    // Seed zero is an ordinary seed, not "unseeded".
    assert_eq!(draw(0), draw(0));
    assert_ne!(draw(0), draw(1));
}

#[test]
fn every_value_math_random_gives_is_in_range() {
    let (mut e, realm) = engine(1);
    let ok = eval_string(
        &mut *e,
        realm,
        "let ok = true; for (let i = 0; i < 10000; i++) { const r = Math.random(); ok &&= r >= 0 && r < 1; } ok",
    );
    assert_eq!(ok, "true");
}

#[test]
fn the_host_can_reseed_math_random_mid_run() {
    let (mut e, realm) = engine(1);
    let first = eval_string(&mut *e, realm, "Math.random()");
    eval_string(&mut *e, realm, "Math.random(), Math.random()");
    e.set_random_seed(1).unwrap();
    assert_eq!(eval_string(&mut *e, realm, "Math.random()"), first);
}

#[test]
fn nan_is_stored_with_one_bit_pattern() {
    let (mut e, realm) = engine(1);
    let src = "const z = Number('0'); \
               const f = new Float64Array([z / z, -(z / z), NaN]); \
               const g = new Float32Array([z / z, -(z / z), NaN]); \
               [Array.from(new Uint8Array(f.buffer)), Array.from(new Uint8Array(g.buffer))].join(';')";
    assert_eq!(
        eval_string(&mut *e, realm, src),
        "0,0,0,0,0,0,248,127,0,0,0,0,0,0,248,127,0,0,0,0,0,0,248,127;0,0,192,127,0,0,192,127,0,0,192,127"
    );
}

#[test]
fn the_corpus_gives_the_checked_in_output() {
    let bless = std::env::var_os("AGNIJS_BLESS").is_some_and(|v| v != "0");
    let mut failures = Vec::new();
    for js in corpus() {
        let name = js.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&js).unwrap();
        let (got, _) = run(&name, &src, false);
        let golden = js.with_extension("txt");
        if bless {
            std::fs::write(&golden, format!("{got}\n")).unwrap();
            continue;
        }
        // Normalize line endings: a Windows checkout may have turned the
        // golden file's LFs into CRLFs.
        let want = std::fs::read_to_string(&golden)
            .unwrap_or_else(|_| panic!("{} is missing; run with AGNIJS_BLESS=1", golden.display()))
            .replace("\r\n", "\n");
        if want.strip_suffix('\n').unwrap_or(&want) != got {
            failures.push(format!("{name}:\n--- want\n{want}\n--- got\n{got}\n"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_corpus_program_repeats_exactly_and_costs_the_same_fuel() {
    for js in corpus() {
        let name = js.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&js).unwrap();
        let metered = run(&name, &src, true);
        assert_eq!(metered, run(&name, &src, true), "{name}");
        // Metering changes nothing a program can see.
        assert_eq!(metered.0, run(&name, &src, false).0, "{name}");
    }
}
