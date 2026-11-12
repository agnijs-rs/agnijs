//! The native stack budget (TASK-103.27): an engine made through the Rust
//! API refuses deeply nested input with an error instead of overflowing the
//! host thread's stack, which would end the process. It does so by default,
//! on a thread with the 2 MiB Rust default stack and on a 1 MiB one (the
//! Windows default for a main thread), and the host can change the budget
//! or opt out.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, RealmId, Value};
use agnijs_js_parser::stack::{DEFAULT_BUDGET, HEADROOM};
use agnijs_js_vm::Vm;

const DEEP: usize = 300_000;
const EXHAUSTED: &str = "Maximum call stack size exceeded";

/// Run `f` on a fresh thread with a `bytes` stack.
fn on_stack<T: Send + 'static>(bytes: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(bytes)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn nested(open: &str, middle: &str, close: &str, depth: usize) -> String {
    format!("{}{middle}{}", open.repeat(depth), close.repeat(depth))
}

/// The scripts that recurse on the native stack: while parsing and
/// compiling, and in JSON.parse. With `run_time`, also scripts that build
/// deep data in loops and recurse over it in built-ins (left out where
/// AGNIJS_GC_STRESS would collect at each of their 300,000 iterations).
fn hostile(run_time: bool) -> Vec<(String, &'static str)> {
    let mut cases = vec![
        (nested("[", "1", "]", DEEP), "SyntaxError"),
        (nested("function f(){", "", "}", DEEP / 10), "SyntaxError"),
        (nested("(", "1", ")", DEEP), "SyntaxError"),
        (nested("a=>", "0", "", DEEP), "SyntaxError"),
        (
            format!("JSON.parse('['.repeat({DEEP}) + ']'.repeat({DEEP}))"),
            "RangeError",
        ),
        (
            format!("JSON.parse('{{\"a\":'.repeat({DEEP}) + '1' + '}}'.repeat({DEEP}))"),
            "RangeError",
        ),
        (
            format!("eval('['.repeat({DEEP}) + ']'.repeat({DEEP}))"),
            "SyntaxError",
        ),
        (
            format!("Function('return ' + '('.repeat({DEEP}) + '1' + ')'.repeat({DEEP}))"),
            "SyntaxError",
        ),
        (
            format!("new RegExp('('.repeat({DEEP}) + ')'.repeat({DEEP}))"),
            "SyntaxError",
        ),
    ];
    if !run_time {
        return cases;
    }
    cases.extend([
        (
            format!("let o = {{}}; for (let i = 0; i < {DEEP}; i++) o = {{o}}; JSON.stringify(o)"),
            "RangeError",
        ),
        (
            format!("let a = []; for (let i = 0; i < {DEEP}; i++) a = [a]; a.flat(Infinity)"),
            "RangeError",
        ),
        (
            format!("let p = {{}}; for (let i = 0; i < {DEEP}; i++) p = new Proxy(p, {{}}); p.x"),
            "RangeError",
        ),
    ]);
    cases
}

/// Every hostile script fails with the expected error, and the engine still
/// runs ordinary code afterwards.
fn refuses_hostile_input(e: &mut dyn Engine, realm: RealmId, run_time: bool) {
    for (src, want) in hostile(run_time) {
        let what = &src[..src.len().min(48)];
        match e.eval_script(realm, "deep.js", &src) {
            Err(Error::Exception(x)) => {
                assert_eq!(
                    (x.name.as_str(), x.message.as_str()),
                    (want, EXHAUSTED),
                    "{what}"
                );
            }
            other => panic!("{what}: {other:?}"),
        }
        let v = e
            .eval_script(realm, "after.js", "[[[1]]].flat(Infinity)[0] + 1")
            .unwrap();
        assert!(
            matches!(v, Value::Number(n) if n == 2.0),
            "after {what}: {v:?}"
        );
    }
}

fn vm_engine() -> (Box<dyn Engine>, RealmId) {
    let mut vm = Vm::new(TestHost::new());
    // The run-time cases loop 300,000 times; under AGNIJS_GC_STRESS each
    // iteration would collect.
    vm.heap.stress = false;
    let realm = vm.main_realm();
    (Box::new(vm), realm)
}

fn made_engine() -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine(TestHost::new());
    let realm = e.main_realm();
    (e, realm)
}

#[test]
fn vm_new_refuses_deep_input_on_a_2_mib_thread() {
    on_stack(2 << 20, || {
        let (mut e, realm) = vm_engine();
        refuses_hostile_input(&mut *e, realm, true);
    });
}

#[test]
fn vm_new_refuses_deep_input_on_a_1_mib_thread() {
    on_stack(1 << 20, || {
        let (mut e, realm) = vm_engine();
        refuses_hostile_input(&mut *e, realm, true);
    });
}

#[test]
fn make_engine_refuses_deep_input_on_a_2_mib_thread() {
    on_stack(2 << 20, || {
        let (mut e, realm) = made_engine();
        refuses_hostile_input(&mut *e, realm, false);
    });
}

#[test]
fn make_engine_refuses_deep_input_on_a_1_mib_thread() {
    on_stack(1 << 20, || {
        let (mut e, realm) = made_engine();
        refuses_hostile_input(&mut *e, realm, false);
    });
}

/// The deepest array literal `e` accepts, doubling until one fails.
fn deepest_array(e: &mut dyn Engine, realm: RealmId) -> usize {
    let mut d = 1;
    loop {
        match e.eval_script(realm, "d.js", &nested("[", "1", "]", d * 2)) {
            Ok(_) => d *= 2,
            Err(Error::Exception(x)) => {
                assert_eq!(
                    (x.name.as_str(), x.message.as_str()),
                    ("SyntaxError", EXHAUSTED)
                );
                return d;
            }
            Err(other) => panic!("{other:?}"),
        }
    }
}

#[test]
fn the_budget_defaults_to_512_kib_and_can_be_changed() {
    assert_eq!(DEFAULT_BUDGET, 512 * 1024);
    assert_eq!(HEADROOM, 256 * 1024);
    let (small, large) = on_stack(16 << 20, || {
        let (mut e, realm) = vm_engine();
        assert_eq!(e.stack_budget(), Some(DEFAULT_BUDGET));
        let small = deepest_array(&mut *e, realm);
        e.set_stack_budget(Some(8 << 20)).unwrap();
        assert_eq!(e.stack_budget(), Some(8 << 20));
        let large = deepest_array(&mut *e, realm);
        // A tighter budget than the default refuses ordinary nesting.
        e.set_stack_budget(Some(0)).unwrap();
        let r = e.eval_script(realm, "t.js", "[1]");
        assert!(
            r.as_ref()
                .is_err_and(|x| x.is_exception_named("SyntaxError")),
            "{r:?}"
        );
        (small, large)
    });
    assert!(small >= 8, "{small}");
    assert!(large >= small * 8, "{small} {large}");
}

#[test]
fn opting_out_leaves_only_the_threads_own_stack() {
    on_stack(64 << 20, || {
        let (mut e, realm) = vm_engine();
        let small = deepest_array(&mut *e, realm);
        e.set_stack_budget(None).unwrap();
        assert_eq!(e.stack_budget(), None);
        // Nesting the default refuses now runs, on a stack with room for it.
        let v = e
            .eval_script(realm, "t.js", &nested("[", "1", "]", small * 4))
            .unwrap();
        assert!(matches!(v, Value::Object(_)), "{v:?}");
        // A budget the embedder installs around the call still applies.
        let _outer = agnijs_js_parser::stack::StackBudget::enter(DEFAULT_BUDGET);
        let r = e.eval_script(realm, "t.js", &nested("[", "1", "]", small * 4));
        assert!(
            r.as_ref()
                .is_err_and(|x| x.is_exception_named("SyntaxError")),
            "{r:?}"
        );
    });
}

/// A host function that calls back into the engine keeps the outer, tighter
/// limit: nesting entries never adds stack to the budget.
#[test]
fn reentry_from_a_host_function_keeps_the_outer_budget() {
    on_stack(1 << 20, || {
        let (mut e, realm) = vm_engine();
        let reenter = e
            .new_function(
                realm,
                "reenter",
                std::rc::Rc::new(|e: &mut dyn Engine, cx: agnijs_engine_api::CallContext| {
                    e.eval_script(cx.realm, "r.js", &nested("[", "reenter()", "]", 4))
                }),
            )
            .unwrap();
        let global = e.global_object(realm).unwrap();
        e.set(
            realm,
            &global,
            &agnijs_engine_api::PropertyKey::from("reenter"),
            &reenter,
        )
        .unwrap();
        let r = e.eval_script(realm, "t.js", "reenter()");
        assert!(
            r.as_ref().is_err_and(
                |x| x.is_exception_named("RangeError") || x.is_exception_named("SyntaxError")
            ),
            "{r:?}"
        );
        let v = e.eval_script(realm, "after.js", "1 + 1").unwrap();
        assert!(matches!(v, Value::Number(n) if n == 2.0), "{v:?}");
    });
}
