//! What an engine does after a panic unwinds out of it (TASK-103.31).
//! Nothing in the VM catches a panic, so one raised by a host function or a
//! `Host` hook unwinds through the interpreter to the embedder, leaving the
//! frame stack, the native depth and the roots mid-update. An embedder that
//! catches it gets a poisoned engine: every later call that runs code or
//! touches a realm fails with `Error::Poisoned`, and dropping it is safe.

use agnijs_engine_api::{CallContext, Engine, Error, Host, RealmId, Value};
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

/// Panics from its interrupt hook once `panic_on_interrupt` is set.
#[derive(Default)]
struct PanicHost {
    panic_on_interrupt: Cell<bool>,
}

impl Host for PanicHost {
    fn interrupt(&self) -> bool {
        if self.panic_on_interrupt.get() {
            panic!("the host's interrupt hook panicked");
        }
        false
    }
}

fn engine() -> (Rc<PanicHost>, Box<dyn Engine>, RealmId) {
    let host = Rc::new(PanicHost::default());
    let e = agnijs_js_vm::make_engine(host.clone());
    let realm = e.main_realm();
    (host, e, realm)
}

fn install(e: &mut dyn Engine, realm: RealmId, name: &str, f: Rc<agnijs_engine_api::HostFunction>) {
    let func = e.new_function(realm, name, f).unwrap();
    let global = e.global_object(realm).unwrap();
    e.set(realm, &global, &name.into(), &func).unwrap();
}

/// Run `f`, which must panic, with the panic message silenced.
fn expect_panic(f: impl FnOnce()) {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    assert!(r.is_err(), "the call did not panic");
}

/// Every entry that runs code or touches a realm is refused.
fn assert_poisoned(e: &mut dyn Engine, realm: RealmId) {
    assert!(e.is_poisoned());
    let r = e.eval_script(realm, "after.js", "1 + 1");
    assert!(matches!(r, Err(Error::Poisoned)), "{r:?}");
    assert!(matches!(e.global_object(realm), Err(Error::Poisoned)));
    assert!(matches!(e.run_jobs(realm), Err(Error::Poisoned)));
    assert!(matches!(e.create_realm(), Err(Error::Poisoned)));
    assert!(matches!(e.new_object(realm), Err(Error::Poisoned)));
    assert!(matches!(e.snapshot(realm), Err(Error::Poisoned)));
    assert!(matches!(
        e.compile_script(realm, "c.js", "1"),
        Err(Error::Poisoned)
    ));
    // The calls that cannot fail stay memory-safe, whatever they return.
    let _ = e.memory_usage();
    e.collect_garbage();
    e.reclaim_handles();
}

#[test]
fn a_panic_in_a_host_function_poisons_the_engine() {
    let (_host, mut e, realm) = engine();
    install(
        &mut *e,
        realm,
        "boom",
        Rc::new(|_: &mut dyn Engine, _: CallContext| panic!("a host function panicked")),
    );
    assert!(!e.is_poisoned());
    // Deep inside script frames and native calls, so the unwind leaves the
    // frame stack and the native depth mid-update.
    expect_panic(|| {
        let _ = e.eval_script(
            realm,
            "t.js",
            "function f(n) { return n ? [1].map(() => f(n - 1))[0] : boom(); } f(5)",
        );
    });
    assert_poisoned(&mut *e, realm);
    drop(e);
}

#[test]
fn a_panic_in_a_host_hook_poisons_the_engine() {
    let (host, mut e, realm) = engine();
    host.panic_on_interrupt.set(true);
    expect_panic(|| {
        let _ = e.eval_script(realm, "t.js", "for (;;) {}");
    });
    host.panic_on_interrupt.set(false);
    assert_poisoned(&mut *e, realm);
}

#[test]
fn a_panic_in_a_nested_call_back_into_the_engine_poisons_it() {
    let (_host, mut e, realm) = engine();
    install(
        &mut *e,
        realm,
        "boom",
        Rc::new(|_: &mut dyn Engine, _: CallContext| panic!("a nested host function panicked")),
    );
    // `outer` calls back into the engine, whose script calls `boom`.
    install(
        &mut *e,
        realm,
        "outer",
        Rc::new(|e: &mut dyn Engine, cx: CallContext| {
            e.eval_script(cx.realm, "inner.js", "boom()")
        }),
    );
    expect_panic(|| {
        let _ = e.eval_script(realm, "t.js", "outer()");
    });
    assert_poisoned(&mut *e, realm);
}

/// A host function that catches the panic of its own nested call still
/// finds the engine poisoned, and so does the script it returns to.
#[test]
fn a_panic_caught_inside_a_host_function_still_poisons_the_engine() {
    let (_host, mut e, realm) = engine();
    install(
        &mut *e,
        realm,
        "boom",
        Rc::new(|_: &mut dyn Engine, _: CallContext| panic!("a nested host function panicked")),
    );
    install(
        &mut *e,
        realm,
        "outer",
        Rc::new(|e: &mut dyn Engine, cx: CallContext| {
            let hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let r = catch_unwind(AssertUnwindSafe(|| {
                e.eval_script(cx.realm, "inner.js", "boom()")
            }));
            std::panic::set_hook(hook);
            assert!(r.is_err());
            assert!(e.is_poisoned());
            e.eval_script(cx.realm, "again.js", "1")
        }),
    );
    let r = e.eval_script(realm, "t.js", "try { outer() } catch (err) { 'caught' }");
    assert!(matches!(r, Err(Error::Poisoned)), "{r:?}");
    assert_poisoned(&mut *e, realm);
}

#[test]
fn errors_and_exceptions_do_not_poison_the_engine() {
    let (_host, mut e, realm) = engine();
    install(
        &mut *e,
        realm,
        "fail",
        Rc::new(|_: &mut dyn Engine, _: CallContext| Err(Error::Host("refused".into()))),
    );
    assert!(e.eval_script(realm, "t.js", "fail()").is_err());
    assert!(e
        .eval_script(realm, "t.js", "throw new Error('x')")
        .is_err());
    assert!(e.eval_script(realm, "t.js", "(").is_err());
    assert!(!e.is_poisoned());
    let v = e.eval_script(realm, "t.js", "1 + 1").unwrap();
    assert!(matches!(v, Value::Number(n) if n == 2.0), "{v:?}");
}
