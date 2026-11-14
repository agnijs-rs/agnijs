//! IsArray, IsConstructor and GetFunctionRealm over chains of a million
//! proxies or bound functions (TASK-103.47): a correct answer or a
//! catchable RangeError, never a native stack overflow.
//!
//! A stack overflow ends the process, so each case runs in a child copy of
//! this test binary: a regression fails one test instead of aborting the
//! suite.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;
use std::process::Command;

const CHILD: &str = "AGNIJS_DEEP_NATIVE_CHAIN_CASE";
const LINKS: &str = "1e6";

/// (name, script, the results it may give). Each script catches what it
/// throws and evaluates to a string.
fn cases() -> Vec<(&'static str, String, &'static [&'static str])> {
    let proxies = format!("let p = []; for (let i = 0; i < {LINKS}; i++) p = new Proxy(p, {{}});");
    let bound =
        format!("let f = function () {{}}; for (let i = 0; i < {LINKS}; i++) f = f.bind();");
    let try_ = |body: &str| {
        format!("(() => {{ try {{ return String({body}); }} catch (e) {{ return e.name; }} }})()")
    };
    vec![
        ("Array.isArray over proxies", format!("{proxies} {}", try_("Array.isArray(p)")), &["true", "RangeError"]),
        (
            "Object.prototype.toString over proxies",
            format!("{proxies} {}", try_("Object.prototype.toString.call(p)")),
            &["[object Array]", "RangeError"],
        ),
        ("concat over proxies", format!("{proxies} {}", try_("[].concat(p).length")), &["0", "RangeError"]),
        ("new over bound functions", format!("{bound} {}", try_("typeof new f()")), &["object", "RangeError"]),
        (
            "Reflect.construct with a bound new.target",
            format!(
                "let g = function () {{}}; g.prototype = null; \
                 for (let i = 0; i < {LINKS}; i++) g = g.bind(); \
                 {}",
                try_("Array.isArray(Reflect.construct(Array, [], g))")
            ),
            &["true", "RangeError"],
        ),
        (
            "Reflect.construct with a proxied new.target",
            format!(
                "let h = function () {{}}; for (let i = 0; i < {LINKS}; i++) h = new Proxy(h, {{}}); \
                 h = new Proxy(h, {{ get() {{}} }}); \
                 {}",
                try_("Array.isArray(Reflect.construct(Array, [], h))")
            ),
            &["true", "RangeError"],
        ),
        ("instanceof over bound functions", format!("{bound} {}", try_("({}) instanceof f")), &["false", "RangeError"]),
        // With no prototypes there is no @@hasInstance to call, so each link
        // is followed by InstanceofOperator itself, not through a call.
        (
            "instanceof over bound functions without prototypes",
            format!(
                "let bind = Function.prototype.bind; let f = function () {{}}; \
                 for (let i = 0; i < {LINKS}; i++) {{ f = bind.call(f); Object.setPrototypeOf(f, null); }} {}",
                try_("({}) instanceof f")
            ),
            &["false", "RangeError"],
        ),
    ]
}

fn run(script: &str) -> String {
    let mut vm = Vm::new(TestHost::new());
    // A million iterations would each collect under AGNIJS_GC_STRESS.
    vm.heap.stress = false;
    let realm = vm.main_realm();
    match vm.eval_script(realm, "chain.js", script) {
        Ok(Value::String(s)) => s.to_string(),
        other => format!("{other:?}"),
    }
}

/// The child side: runs the case named in the environment, on a thread
/// with the default stack. Does nothing in a normal test run.
#[test]
fn child() {
    let Ok(name) = std::env::var(CHILD) else {
        return;
    };
    let (_, script, allowed) = cases()
        .into_iter()
        .find(|c| c.0 == name)
        .expect("unknown case");
    let got = std::thread::spawn(move || run(&script))
        .join()
        .expect("the case panicked");
    assert!(allowed.contains(&got.as_str()), "{name}: {got}");
}

#[test]
fn chains_of_a_million_give_an_answer_or_a_range_error() {
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for (name, _, _) in cases() {
        let out = Command::new(&exe)
            .args(["--exact", "child", "--nocapture", "--test-threads=1"])
            .env(CHILD, name)
            .output()
            .unwrap();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
            failed.push(format!("{name}: {:?} {tail}", out.status));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
