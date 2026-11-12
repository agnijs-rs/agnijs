//! `Context` engines carry the native stack budget by default (TASK-103.27):
//! deeply nested source and JSON fail with an error on a 2 MiB thread and a
//! 1 MiB one instead of overflowing the stack and ending the process.

use agnijs::{Context, Host};
use std::rc::Rc;

const DEEP: usize = 300_000;

/// A host that keeps every default.
struct Plain;
impl Host for Plain {}

fn on_stack(bytes: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(bytes)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn refuses_deep_input(js: &mut Context) {
    let cases = [
        (
            format!("{}1{}", "[".repeat(DEEP), "]".repeat(DEEP)),
            "SyntaxError",
        ),
        (
            format!(
                "{}{}",
                "function f(){".repeat(DEEP / 10),
                "}".repeat(DEEP / 10)
            ),
            "SyntaxError",
        ),
        (
            format!("JSON.parse('['.repeat({DEEP}) + ']'.repeat({DEEP}))"),
            "RangeError",
        ),
    ];
    for (src, want) in cases {
        let e = js.eval::<()>(&src).unwrap_err();
        assert!(e.is_js(want), "{}: {e}", &src[..40]);
        assert!(
            e.to_string().contains("Maximum call stack size exceeded"),
            "{e}"
        );
        assert_eq!(js.eval::<i32>("1 + 1").unwrap(), 2);
    }
}

#[test]
fn context_new_refuses_deep_input_on_a_2_mib_thread() {
    on_stack(2 << 20, || refuses_deep_input(&mut Context::new()));
}

#[test]
fn context_new_refuses_deep_input_on_a_1_mib_thread() {
    on_stack(1 << 20, || refuses_deep_input(&mut Context::new()));
}

#[test]
fn context_with_host_refuses_deep_input_on_a_2_mib_thread() {
    on_stack(2 << 20, || {
        refuses_deep_input(&mut Context::with_host(Rc::new(Plain)))
    });
}

#[test]
fn context_with_host_refuses_deep_input_on_a_1_mib_thread() {
    on_stack(1 << 20, || {
        refuses_deep_input(&mut Context::with_host(Rc::new(Plain)))
    });
}

#[test]
fn a_context_can_change_or_drop_the_budget() {
    on_stack(32 << 20, || {
        let mut js = Context::new();
        // Debug builds use about 26 KiB of stack per level, release builds 2 KiB:
        // the default refuses 500 levels in both, 24 MiB takes them.
        let deep = format!("void {}1{}", "[".repeat(500), "]".repeat(500));
        assert!(js.eval::<()>(&deep).unwrap_err().is_js("SyntaxError"));
        js.engine().set_stack_budget(Some(24 << 20)).unwrap();
        js.eval::<()>(&deep).unwrap();
        js.engine().set_stack_budget(None).unwrap();
        js.eval::<()>(&deep).unwrap();
    });
}
