//! The public embedding surface (ADR 0026): an embedder that depends on
//! `agnijs` alone can build an AgniJS engine, fresh or from a snapshot,
//! and drive it through `agnijs::api`.

use agnijs::api::{self, Engine, Host, Value};
use std::rc::Rc;

struct Quiet;
impl Host for Quiet {}

#[test]
fn new_engine_gives_a_working_engine_with_a_main_realm() {
    let mut engine: Box<dyn Engine> = agnijs::new_engine(Rc::new(Quiet));
    let realm = engine.main_realm();
    let v = engine.eval_script(realm, "sum.js", "1 + 2").unwrap();
    assert_eq!(v, Value::Number(3.0));
    assert_eq!(engine.info().name, "agnijs");
}

#[test]
fn new_engine_from_snapshot_restores_the_realm() {
    let mut engine = agnijs::new_engine(Rc::new(Quiet));
    let realm = engine.main_realm();
    engine
        .eval_script(realm, "boot.js", "globalThis.greeting = 'hi'")
        .unwrap();
    let bytes = engine.snapshot(realm).unwrap();

    let mut restored = agnijs::new_engine_from_snapshot(Rc::new(Quiet), &bytes).unwrap();
    let realm = restored.main_realm();
    let v = restored.eval_script(realm, "read.js", "greeting").unwrap();
    assert_eq!(v, Value::string("hi"));
}

#[test]
fn a_damaged_snapshot_is_an_error_not_a_panic() {
    let err = agnijs::new_engine_from_snapshot(Rc::new(Quiet), b"not a snapshot")
        .err()
        .expect("refused");
    assert!(matches!(
        err,
        api::Error::BytecodeMismatch { .. } | api::Error::BytecodeCorrupt(_)
    ));
}

#[test]
fn a_context_can_be_built_over_a_constructed_engine() {
    let engine = agnijs::new_engine(Rc::new(Quiet));
    let mut cx = agnijs::Context::from_engine(engine);
    let n: i32 = cx.eval("6 * 7").unwrap();
    assert_eq!(n, 42);
}
