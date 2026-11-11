//! Running one workload on one engine.

use crate::workloads::{generated_source, prelude, Kind, Workload};
use anyhow::{anyhow, Result};
use agnijs_engine_api::{Engine, Host, RealmId, RegexMatcher, Value};
use std::rc::Rc;
use std::time::Instant;

/// The engines behind the engine interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// The owned engine (`agnijs-js-vm`).
    Owned,
    /// The QuickJS-NG adapter.
    QuickJs,
    /// The owned engine with `RegexMatcher::BacktrackingOnly`, for the
    /// matcher comparison (ADR 0013 addendum). Not in [`EngineKind::ALL`].
    OwnedBacktracking,
    /// The owned engine with `RegexMatcher::LinearWhenEligible`. Not in
    /// [`EngineKind::ALL`].
    OwnedLinear,
}

impl EngineKind {
    pub const ALL: [EngineKind; 2] = [EngineKind::Owned, EngineKind::QuickJs];

    /// The engine's id, as the test262 runner's `--engine` spells it.
    pub fn name(self) -> &'static str {
        match self {
            EngineKind::Owned => "agnijs",
            EngineKind::QuickJs => "quickjs",
            EngineKind::OwnedBacktracking => "agnijs-backtracking",
            EngineKind::OwnedLinear => "agnijs-linear",
        }
    }

    pub fn parse(name: &str) -> Option<EngineKind> {
        [EngineKind::Owned, EngineKind::QuickJs, EngineKind::OwnedBacktracking, EngineKind::OwnedLinear]
            .into_iter()
            .find(|e| e.name() == name)
    }

    pub fn make(self) -> Box<dyn Engine> {
        let regex = match self {
            EngineKind::OwnedBacktracking => RegexMatcher::BacktrackingOnly,
            EngineKind::OwnedLinear => RegexMatcher::LinearWhenEligible,
            _ => RegexMatcher::Auto,
        };
        let host: Rc<dyn Host> = Rc::new(BenchHost { regex });
        match self {
            EngineKind::QuickJs => agnijs_engine_quickjs::make_engine(host),
            _ => agnijs_js_vm::make_engine(host),
        }
    }

    /// Whether this engine can run `w` (snapshots are AgniJS only).
    pub fn supports(self, w: &Workload) -> bool {
        !(self == EngineKind::QuickJs && matches!(w.kind, Kind::SnapshotStartup | Kind::BundleSnapshot))
    }

    /// An engine restored from a snapshot written by [`Engine::snapshot`].
    fn restore(self, bytes: &[u8]) -> Result<Box<dyn Engine>> {
        let host: Rc<dyn Host> = Rc::new(BenchHost { regex: RegexMatcher::Auto });
        match self {
            EngineKind::QuickJs => Err(anyhow!("QuickJS-NG has no snapshots")),
            _ => agnijs_js_vm::make_engine_from_snapshot(host, bytes).map_err(js),
        }
    }
}

/// A host with every hook at its default: workloads use only the standard
/// library.
struct BenchHost {
    regex: RegexMatcher,
}
impl Host for BenchHost {
    fn regex_matcher(&self) -> RegexMatcher {
        self.regex
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunConfig {
    /// Multiplies each workload's work count (`scaled(n)` in the JS).
    pub scale: f64,
    /// Untimed iterations before the measured ones.
    pub warmup: usize,
    pub iterations: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    /// Wall time of each measured iteration.
    pub samples_ns: Vec<u64>,
    /// The workload's result after the last iteration, as a string.
    pub checksum: String,
    /// Engine-reported heap size after the last iteration, before any
    /// explicit collection. Each engine counts differently.
    pub heap_bytes: usize,
    pub object_count: usize,
}

fn js(e: agnijs_engine_api::Error) -> anyhow::Error {
    anyhow!("{e:?}")
}

fn drain(engine: &mut dyn Engine, realm: RealmId) -> Result<()> {
    while engine.has_pending_jobs() {
        engine.run_jobs(realm).map_err(js)?;
    }
    Ok(())
}

fn stringify(engine: &mut dyn Engine, realm: RealmId, v: &Value) -> Result<String> {
    engine.to_string(realm, v).map_err(js)
}

/// Run `w` on a fresh engine of kind `kind`.
pub fn run(kind: EngineKind, w: &Workload, cfg: &RunConfig) -> Result<Measurement> {
    let total = cfg.warmup + cfg.iterations;
    let mut samples_ns = Vec::with_capacity(cfg.iterations);
    let mut record = |i: usize, start: Instant| {
        if i >= cfg.warmup {
            samples_ns.push(start.elapsed().as_nanos() as u64);
        }
    };
    let checksum;
    let mut engine;
    match w.kind {
        Kind::Startup => {
            let mut last = String::new();
            for i in 0..total {
                let start = Instant::now();
                let mut e = kind.make();
                let realm = e.main_realm();
                let v = e.eval_script(realm, "startup.js", "1 + 1").map_err(js)?;
                record(i, start);
                last = stringify(e.as_mut(), realm, &v)?;
            }
            checksum = last;
            engine = kind.make();
        }
        Kind::Script { setup, run, check } => {
            engine = kind.make();
            let realm = engine.main_realm();
            engine.eval_script(realm, "prelude.js", &prelude(cfg.scale)).map_err(js)?;
            engine.eval_script(realm, "setup.js", setup).map_err(js)?;
            drain(engine.as_mut(), realm)?;
            let mut last = Value::Undefined;
            for i in 0..total {
                let start = Instant::now();
                last = engine.eval_script(realm, "run.js", run).map_err(js)?;
                drain(engine.as_mut(), realm)?;
                record(i, start);
            }
            if let Some(check) = check {
                last = engine.eval_script(realm, "check.js", check).map_err(js)?;
            }
            checksum = stringify(engine.as_mut(), realm, &last)?;
        }
        Kind::SnapshotStartup => {
            let bytes = {
                let mut e = kind.make();
                let realm = e.main_realm();
                e.snapshot(realm).map_err(js)?
            };
            let mut last = String::new();
            for i in 0..total {
                let start = Instant::now();
                let mut e = kind.restore(&bytes)?;
                let realm = e.main_realm();
                let v = e.eval_script(realm, "startup.js", "1 + 1").map_err(js)?;
                record(i, start);
                last = stringify(e.as_mut(), realm, &v)?;
            }
            checksum = last;
            engine = kind.restore(&bytes)?;
        }
        Kind::BundleCold | Kind::BundleSnapshot => {
            let source = generated_source();
            let (bytecode, snapshot) = {
                let mut e = kind.make();
                let realm = e.main_realm();
                let b = e.compile_script(realm, "bundle.js", &source).map_err(js)?;
                let s = if w.kind == Kind::BundleSnapshot {
                    e.eval_bytecode(realm, &b).map_err(js)?;
                    Some(e.snapshot(realm).map_err(js)?)
                } else {
                    None
                };
                (b, s)
            };
            let mut last = String::new();
            let mut kept = None;
            for i in 0..total {
                let start = Instant::now();
                let mut e = match &snapshot {
                    Some(s) => kind.restore(s)?,
                    None => {
                        let mut e = kind.make();
                        let realm = e.main_realm();
                        e.eval_bytecode(realm, &bytecode).map_err(js)?;
                        e
                    }
                };
                let realm = e.main_realm();
                let v = e.eval_script(realm, "first-call.js", "f_7(1, 2) + acc").map_err(js)?;
                record(i, start);
                last = stringify(e.as_mut(), realm, &v)?;
                kept = Some(e);
            }
            checksum = last;
            engine = match kept {
                Some(e) => e,
                None => kind.make(),
            };
        }
        Kind::CompileSource | Kind::BytecodeLoad => {
            engine = kind.make();
            let realm = engine.main_realm();
            let source = generated_source();
            let bytecode = if w.kind == Kind::BytecodeLoad {
                Some(engine.compile_script(realm, "generated.js", &source).map_err(js)?)
            } else {
                None
            };
            let mut last = Value::Undefined;
            for i in 0..total {
                let start = Instant::now();
                last = match &bytecode {
                    Some(b) => engine.eval_bytecode(realm, b),
                    None => engine.eval_script(realm, "generated.js", &source),
                }
                .map_err(js)?;
                record(i, start);
            }
            checksum = stringify(engine.as_mut(), realm, &last)?;
        }
    }
    let usage = engine.memory_usage();
    Ok(Measurement { samples_ns, checksum, heap_bytes: usage.heap_bytes, object_count: usage.object_count })
}
