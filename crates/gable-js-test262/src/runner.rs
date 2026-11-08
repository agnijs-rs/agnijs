//! Runs one test262 test against an engine behind the engine interface,
//! following INTERPRETING.md: harness preamble, strict double run, module
//! loading, negative expectations, async completion through `print`.

use crate::metadata::Metadata;
use gable_engine_api::{
    CallContext, Engine, Error, Host, HostFunction, ImportAttribute, ModuleSource, PromiseState,
    RealmId, TypeOf, Value,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Builds a fresh engine for one test run.
pub type Factory = Arc<dyn Fn(Rc<dyn Host>) -> Box<dyn Engine> + Send + Sync>;

/// How a test is run: through a full engine, or parsed only.
#[derive(Clone)]
pub enum Mode {
    /// Evaluate with a fresh engine from the factory.
    Engine(Factory),
    /// Parse with `gable-js-parser` only: positive tests must parse, tests
    /// negative in the `parse` phase must fail to parse, nothing runs.
    ParseOnly,
    /// Parse and compile to bytecode without running (checks the compiler
    /// against every in-scope test).
    CompileOnly,
}

/// Harness files by name (`assert.js`, ...).
pub type HarnessFiles = HashMap<String, String>;

pub struct TestCase {
    pub path: PathBuf,
    /// Path relative to the test262 root, forward slashes (`test/...`).
    pub rel: String,
    pub meta: Metadata,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
}

#[derive(Debug, Clone)]
pub struct RunResult {
    /// `Some(true)` for the strict run, `Some(false)` sloppy, `None` when
    /// the test runs once (module or raw).
    pub strict: Option<bool>,
    pub outcome: Outcome,
}

const DONOTEVALUATE: &str = "Test262: This statement should not be evaluated.";
const ASYNC_COMPLETE: &str = "Test262:AsyncTestComplete";
const ASYNC_FAILURE: &str = "Test262:AsyncTestFailure:";

/// The host a test runs under: `print` capture, module loading from the
/// test directory, and a deadline enforced through the interrupt hook.
struct Test262Host {
    prints: RefCell<Vec<String>>,
    deadline: Instant,
    root: PathBuf,
}

impl Host for Test262Host {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> std::result::Result<String, String> {
        let base: PathBuf = match referrer {
            Some(r) => Path::new(r)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        };
        let mut joined = if specifier.starts_with("./") || specifier.starts_with("../") {
            base.join(specifier)
        } else {
            PathBuf::from(specifier)
        };
        // A referrer that is not a file (an `eval` source, say) resolves
        // against the test directory.
        if !joined.exists() {
            let fallback = self.root.join(specifier);
            if fallback.exists() {
                joined = fallback;
            }
        }
        Ok(normalize(&joined))
    }

    fn load_module(&self, resolved: &str) -> std::result::Result<ModuleSource, String> {
        let text = std::fs::read_to_string(resolved)
            .map_err(|e| format!("cannot load module {resolved}: {e}"))?;
        if resolved.ends_with(".json") {
            Ok(ModuleSource::Json(text))
        } else {
            Ok(ModuleSource::Source(text))
        }
    }

    fn interrupt(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

fn normalize(p: &Path) -> String {
    // Resolve `.` and `..` textually; test262 fixtures never cross symlinks.
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        use std::path::Component::*;
        match c {
            CurDir => {}
            ParentDir => {
                parts.pop();
            }
            Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            RootDir => parts.push(String::new()),
            Prefix(pre) => parts.push(pre.as_os_str().to_string_lossy().into_owned()),
        }
    }
    let s = parts.join("/");
    s.replace("//", "/")
}

/// Run a test (once or twice as its flags require).
pub fn run_test(
    case: &TestCase,
    mode: &Mode,
    harness: &HarnessFiles,
    timeout: Duration,
) -> Vec<RunResult> {
    let factory = match mode {
        Mode::Engine(f) => f,
        Mode::ParseOnly => return parse_test(case, harness, false),
        Mode::CompileOnly => return parse_test(case, harness, true),
    };
    let m = &case.meta;
    if m.is_module() || m.is_raw() {
        return vec![RunResult {
            strict: None,
            outcome: run_once(case, false, factory, harness, timeout),
        }];
    }
    let mut runs = Vec::new();
    if !m.has_flag("onlyStrict") {
        runs.push(RunResult {
            strict: Some(false),
            outcome: run_once(case, false, factory, harness, timeout),
        });
    }
    if !m.has_flag("noStrict") {
        runs.push(RunResult {
            strict: Some(true),
            outcome: run_once(case, true, factory, harness, timeout),
        });
    }
    runs
}

/// Parse-only run (see `Mode::ParseOnly`).
fn parse_test(case: &TestCase, harness: &HarnessFiles, compile: bool) -> Vec<RunResult> {
    let m = &case.meta;
    if m.is_module() || m.is_raw() {
        return vec![RunResult {
            strict: None,
            outcome: parse_once(case, false, harness, compile),
        }];
    }
    let mut runs = Vec::new();
    if !m.has_flag("onlyStrict") {
        runs.push(RunResult {
            strict: Some(false),
            outcome: parse_once(case, false, harness, compile),
        });
    }
    if !m.has_flag("noStrict") {
        runs.push(RunResult {
            strict: Some(true),
            outcome: parse_once(case, true, harness, compile),
        });
    }
    runs
}

fn parse_once(case: &TestCase, strict: bool, harness: &HarnessFiles, compile: bool) -> Outcome {
    if compile {
        return compile_once(case, strict, harness);
    }
    let meta = &case.meta;
    if !meta.is_raw() {
        let mut files: Vec<&str> = vec!["assert.js", "sta.js"];
        if meta.is_async() {
            files.push("doneprintHandle.js");
        }
        files.extend(meta.includes.iter().map(String::as_str));
        for f in files {
            let Some(src) = harness.get(f) else {
                return Outcome::Fail(format!("missing harness file {f}"));
            };
            if let Err(e) = gable_js_parser::parse_script(src) {
                return Outcome::Fail(format!("harness {f} failed to parse: {e}"));
            }
        }
    }
    let result = if meta.is_module() {
        gable_js_parser::parse_module(&case.source).map(|_| ())
    } else if strict {
        gable_js_parser::parse_script(&format!(
            "\"use strict\";
{}",
            case.source
        ))
        .map(|_| ())
    } else {
        gable_js_parser::parse_script(&case.source).map(|_| ())
    };
    let expects_parse_error = matches!(&meta.negative, Some(n) if n.phase == "parse");
    match (expects_parse_error, result) {
        (false, Ok(())) => Outcome::Pass,
        (false, Err(e)) => Outcome::Fail(format!("SyntaxError: {e}")),
        (true, Err(_)) => Outcome::Pass,
        (true, Ok(())) => {
            Outcome::Fail("expected SyntaxError in phase parse but the source parsed".into())
        }
    }
}

fn compile_once(case: &TestCase, strict: bool, harness: &HarnessFiles) -> Outcome {
    let meta = &case.meta;
    if !meta.is_raw() {
        let mut files: Vec<&str> = vec!["assert.js", "sta.js"];
        if meta.is_async() {
            files.push("doneprintHandle.js");
        }
        files.extend(meta.includes.iter().map(String::as_str));
        for f in files {
            let Some(src) = harness.get(f) else {
                return Outcome::Fail(format!("missing harness file {f}"));
            };
            if let Err(e) = gable_js_compiler::compile_script(src, f) {
                return Outcome::Fail(format!("harness {f} failed to compile: {e}"));
            }
        }
    }
    let name = case.path.to_string_lossy().to_string();
    let result = std::panic::catch_unwind(|| {
        if meta.is_module() {
            gable_js_compiler::compile_module(&case.source, &name).map(|_| ())
        } else if strict {
            gable_js_compiler::compile_script(&format!("\"use strict\";\n{}", case.source), &name)
                .map(|_| ())
        } else {
            gable_js_compiler::compile_script(&case.source, &name).map(|_| ())
        }
    });
    let result = match result {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            return Outcome::Fail(format!("compiler panic: {msg}"));
        }
    };
    let expects_parse_error = matches!(&meta.negative, Some(n) if n.phase == "parse");
    match (expects_parse_error, result) {
        (false, Ok(())) => Outcome::Pass,
        (false, Err(e)) => Outcome::Fail(format!("{e}")),
        (true, Err(_)) => Outcome::Pass,
        (true, Ok(())) => {
            Outcome::Fail("expected SyntaxError in phase parse but the source compiled".into())
        }
    }
}

fn run_once(
    case: &TestCase,
    strict: bool,
    factory: &Factory,
    harness: &HarnessFiles,
    timeout: Duration,
) -> Outcome {
    let host = Rc::new(Test262Host {
        prints: RefCell::new(Vec::new()),
        deadline: Instant::now() + timeout,
        root: case
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
    });
    let mut engine = factory(host.clone());
    let realm = engine.main_realm();
    if let Err(e) = install_262(&mut *engine, realm, &host) {
        return Outcome::Fail(format!("installing $262: {e}"));
    }
    let meta = &case.meta;
    if !meta.is_raw() {
        let mut files: Vec<&str> = vec!["assert.js", "sta.js"];
        if meta.is_async() {
            files.push("doneprintHandle.js");
        }
        files.extend(meta.includes.iter().map(String::as_str));
        for f in files {
            let Some(src) = harness.get(f) else {
                return Outcome::Fail(format!("missing harness file {f}"));
            };
            if let Err(e) = engine.eval_script(realm, &format!("harness/{f}"), src) {
                return Outcome::Fail(format!("harness {f} failed: {e}"));
            }
        }
    }

    let completion: Result<(), (String, String)> = if meta.is_module() {
        run_module(&mut *engine, realm, case)
    } else {
        let source = if strict {
            format!("\"use strict\";\n{}", case.source)
        } else {
            case.source.clone()
        };
        // The script is named by its absolute path so dynamic `import()`
        // from a script resolves relative to the test file.
        match engine.eval_script(realm, &normalize(&case.path), &source) {
            Ok(_) => drain_jobs(&mut *engine, realm),
            Err(e) => Err(describe_error(&mut *engine, realm, e)),
        }
    };

    match (&meta.negative, completion) {
        (None, Ok(())) => {
            if meta.is_async() {
                async_outcome(&host.prints.borrow())
            } else {
                Outcome::Pass
            }
        }
        (None, Err((name, message))) => Outcome::Fail(format!("{name}: {message}")),
        (Some(neg), Err((name, message))) => {
            if message.contains(DONOTEVALUATE) {
                return Outcome::Fail(format!(
                    "expected early {} but the code ran ({name}: {message})",
                    neg.kind
                ));
            }
            if name == neg.kind {
                Outcome::Pass
            } else {
                Outcome::Fail(format!(
                    "expected {} in phase {}, got {name}: {message}",
                    neg.kind, neg.phase
                ))
            }
        }
        (Some(neg), Ok(())) => Outcome::Fail(format!(
            "expected {} in phase {} but the test completed",
            neg.kind, neg.phase
        )),
    }
}

fn run_module(
    engine: &mut dyn Engine,
    realm: RealmId,
    case: &TestCase,
) -> Result<(), (String, String)> {
    let specifier = normalize(&case.path);
    let promise = match engine.load_module(realm, &specifier) {
        Ok(p) => p,
        Err(e) => return Err(describe_error(engine, realm, e)),
    };
    drain_jobs(engine, realm)?;
    match engine.promise_state(realm, &promise) {
        Ok(PromiseState::Fulfilled(_)) => Ok(()),
        Ok(PromiseState::Rejected(reason)) => Err(describe_value(engine, realm, &reason)),
        Ok(PromiseState::Pending) => {
            Err(("Timeout".into(), "module evaluation never settled".into()))
        }
        Err(e) => Err(describe_error(engine, realm, e)),
    }
}

fn drain_jobs(engine: &mut dyn Engine, realm: RealmId) -> Result<(), (String, String)> {
    // Jobs may enqueue more jobs; the host-driven loop keeps going until
    // the queue is empty or the deadline interrupts.
    let mut rounds = 0;
    while engine.has_pending_jobs() {
        if let Err(e) = engine.run_jobs(realm) {
            return Err(describe_error(engine, realm, e));
        }
        rounds += 1;
        if rounds > 1_000_000 {
            return Err(("Timeout".into(), "job queue never drained".into()));
        }
    }
    Ok(())
}

fn async_outcome(prints: &[String]) -> Outcome {
    for line in prints {
        if line.starts_with(ASYNC_FAILURE) {
            return Outcome::Fail(line.clone());
        }
    }
    if prints.iter().any(|l| l == ASYNC_COMPLETE) {
        Outcome::Pass
    } else {
        Outcome::Fail(format!(
            "async test did not report completion; output: {prints:?}"
        ))
    }
}

fn describe_error(engine: &mut dyn Engine, realm: RealmId, e: Error) -> (String, String) {
    match e {
        Error::Exception(ex) => {
            // Prefer the live object's name: a thrown subclass instance
            // reports its constructor's name through `name`.
            if matches!(ex.value, Value::Object(_)) {
                return describe_value(engine, realm, &ex.value);
            }
            (ex.name, ex.message)
        }
        Error::Interrupted => ("Timeout".into(), "test exceeded its time limit".into()),
        Error::OutOfMemory => ("OutOfMemory".into(), "memory limit".into()),
        Error::Host(m) => ("HostError".into(), m),
        other => ("EngineError".into(), other.to_string()),
    }
}

fn describe_value(engine: &mut dyn Engine, realm: RealmId, v: &Value) -> (String, String) {
    match engine.type_of(v) {
        TypeOf::Object | TypeOf::Function => {
            // Test262Error defines no `name`, so fall back to the
            // constructor's name, which is what INTERPRETING.md compares.
            let name = engine
                .get(realm, v, &"name".into())
                .ok()
                .filter(|n| !n.is_undefined())
                .or_else(|| {
                    let ctor = engine.get(realm, v, &"constructor".into()).ok()?;
                    engine
                        .get(realm, &ctor, &"name".into())
                        .ok()
                        .filter(|n| !n.is_undefined())
                })
                .and_then(|n| engine.to_string(realm, &n).ok())
                .unwrap_or_else(|| "Object".into());
            let message = engine
                .get(realm, v, &"message".into())
                .ok()
                .filter(|n| !n.is_undefined())
                .and_then(|n| engine.to_string(realm, &n).ok())
                .unwrap_or_default();
            (name, message)
        }
        t => {
            let message = engine.to_string(realm, v).unwrap_or_default();
            (format!("{t:?}").to_lowercase(), message)
        }
    }
}

/// Install `print` and the `$262` host object into `realm`.
fn install_262(
    engine: &mut dyn Engine,
    realm: RealmId,
    host: &Rc<Test262Host>,
) -> gable_engine_api::Result<()> {
    let global = engine.global_object(realm)?;
    let h = host.clone();
    let print: Rc<HostFunction> = Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
        let mut parts = Vec::new();
        for a in &cx.args {
            parts.push(engine.to_string(cx.realm, a)?);
        }
        h.prints.borrow_mut().push(parts.join(" "));
        Ok(Value::Undefined)
    });
    let print_fn = engine.new_function(realm, "print", print)?;
    engine.set(realm, &global, &"print".into(), &print_fn)?;
    let obj = make_262(engine, realm, host)?;
    engine.set(realm, &global, &"$262".into(), &obj)
}

fn make_262(
    engine: &mut dyn Engine,
    realm: RealmId,
    host: &Rc<Test262Host>,
) -> gable_engine_api::Result<Value> {
    let obj = engine.new_object(realm)?;
    let global = engine.global_object(realm)?;
    engine.set(realm, &obj, &"global".into(), &global)?;

    let h = host.clone();
    let create_realm: Rc<HostFunction> =
        Rc::new(move |engine: &mut dyn Engine, _cx: CallContext| {
            let new_realm = engine.create_realm()?;
            install_262(engine, new_realm, &h)?;
            let g = engine.global_object(new_realm)?;
            engine.get(new_realm, &g, &"$262".into())
        });
    let f = engine.new_function(realm, "createRealm", create_realm)?;
    engine.set(realm, &obj, &"createRealm".into(), &f)?;

    // evalScript runs in the realm this $262 belongs to, whatever realm the
    // caller is in.
    let eval_script: Rc<HostFunction> = Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
        let src = engine.to_string(realm, &cx.arg(0))?;
        engine.eval_script(realm, "$262.evalScript", &src)
    });
    let f = engine.new_function(realm, "evalScript", eval_script)?;
    engine.set(realm, &obj, &"evalScript".into(), &f)?;

    let detach: Rc<HostFunction> = Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
        engine.detach_array_buffer(cx.realm, &cx.arg(0))?;
        Ok(Value::Null)
    });
    let f = engine.new_function(realm, "detachArrayBuffer", detach)?;
    engine.set(realm, &obj, &"detachArrayBuffer".into(), &f)?;

    let gc: Rc<HostFunction> = Rc::new(move |engine: &mut dyn Engine, _cx: CallContext| {
        engine.collect_garbage();
        Ok(Value::Undefined)
    });
    let f = engine.new_function(realm, "gc", gc)?;
    engine.set(realm, &obj, &"gc".into(), &f)?;

    // `agent` is deliberately absent: the engine is single-agent and the
    // multi-agent tests are excluded with that reason. `IsHTMLDDA` is absent
    // because the feature is out of scope.
    Ok(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dot_segments() {
        assert_eq!(normalize(Path::new("a/b/../c/./d.js")), "a/c/d.js");
    }

    #[test]
    fn async_outcomes() {
        assert_eq!(
            async_outcome(&["Test262:AsyncTestComplete".into()]),
            Outcome::Pass
        );
        assert!(matches!(
            async_outcome(&["Test262:AsyncTestFailure:boom".into()]),
            Outcome::Fail(_)
        ));
        assert!(matches!(async_outcome(&[]), Outcome::Fail(_)));
    }
}
