//! The lifetime contract of values, handles and realms (TASK-104.2): what a
//! handle keeps alive, what `reclaim_handles` and `collect_garbage`
//! guarantee, what happens to handles that outlive their engine, and that
//! misuse is an error, not a panic.
//!
//! Collection is observed through a host class finalizer, which the engine
//! runs from `run_jobs` after the collection that found the wrapper dead.

use agnijs::api::{
    CallContext, Engine, Error, Host, HostClassDef, HostClassId, HostObject, PropertyKey, RealmId,
    Value,
};
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

struct Quiet;
impl Host for Quiet {}

struct Probe;
impl HostObject for Probe {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// An engine with a host class whose finalizer counts collected wrappers.
fn setup() -> (Box<dyn Engine>, RealmId, HostClassId, Rc<Cell<usize>>) {
    let mut engine = agnijs::new_engine(Rc::new(Quiet));
    let realm = engine.main_realm();
    let finalized = Rc::new(Cell::new(0));
    let seen = finalized.clone();
    let class = engine.register_host_class(HostClassDef {
        name: "Probe".into(),
        finalize: Some(Box::new(move |_| seen.set(seen.get() + 1))),
    });
    (engine, realm, class, finalized)
}

/// A full collection, then the finalizers it queued.
fn collect(engine: &mut dyn Engine, realm: RealmId) {
    engine.collect_garbage();
    engine.run_jobs(realm).unwrap();
}

#[test]
fn a_held_handle_keeps_its_value_alive_across_collections() {
    let (mut engine, realm, class, finalized) = setup();
    let probe = engine
        .new_host_object(realm, class, Box::new(Probe), None)
        .unwrap();
    let key = PropertyKey::from("tag");
    engine
        .set(realm, &probe, &key, &Value::string("kept"))
        .unwrap();
    for _ in 0..3 {
        collect(&mut *engine, realm);
    }
    assert_eq!(finalized.get(), 0);
    assert_eq!(
        engine.get(realm, &probe, &key).unwrap(),
        Value::string("kept")
    );
}

#[test]
fn the_value_is_collected_once_the_last_clone_is_dropped() {
    let (mut engine, realm, class, finalized) = setup();
    let probe = engine
        .new_host_object(realm, class, Box::new(Probe), None)
        .unwrap();
    let copy = probe.clone();
    drop(probe);
    collect(&mut *engine, realm);
    assert_eq!(finalized.get(), 0, "a clone is still held");
    drop(copy);
    collect(&mut *engine, realm);
    assert_eq!(finalized.get(), 1);
}

#[test]
fn a_value_reachable_from_javascript_survives_dropping_its_handle() {
    let (mut engine, realm, class, finalized) = setup();
    let probe = engine
        .new_host_object(realm, class, Box::new(Probe), None)
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    engine
        .set(realm, &global, &PropertyKey::from("kept"), &probe)
        .unwrap();
    drop(probe);
    collect(&mut *engine, realm);
    assert_eq!(finalized.get(), 0);
    engine.eval_script(realm, "drop.js", "kept = null").unwrap();
    collect(&mut *engine, realm);
    assert_eq!(finalized.get(), 1);
}

#[test]
fn dropped_handles_are_reclaimed_at_the_next_call_into_the_engine() {
    let (mut engine, realm, _, _) = setup();
    engine.reclaim_handles();
    let base = engine.live_handles();
    let a = engine.new_object(realm).unwrap();
    let b = engine.new_object(realm).unwrap();
    assert_eq!(engine.live_handles(), base + 2);

    drop(a);
    // Dropping only queues the slot; nothing is freed until the engine is
    // entered or reclaim_handles is called.
    assert_eq!(engine.live_handles(), base + 2);
    engine.reclaim_handles();
    assert_eq!(engine.live_handles(), base + 1);

    drop(b);
    engine.collect_garbage();
    assert_eq!(
        engine.live_handles(),
        base,
        "collect_garbage reclaims first"
    );
}

#[test]
fn every_value_crossing_out_is_a_new_handle_to_the_same_object() {
    let (mut engine, realm, _, _) = setup();
    engine
        .eval_script(realm, "o.js", "globalThis.o = {}")
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    let a = engine.get(realm, &global, &PropertyKey::from("o")).unwrap();
    let b = engine.get(realm, &global, &PropertyKey::from("o")).unwrap();
    // Two slots: `==` on handles compares slots, not JavaScript identity.
    assert_ne!(a, b);
    assert!(engine.strict_equals(&a, &b));
    assert!(engine.same_object(a.as_handle().unwrap(), b.as_handle().unwrap()));
}

#[test]
fn collect_garbage_from_inside_a_host_function_is_deferred_and_safe() {
    let (mut engine, realm, class, finalized) = setup();
    let gc = engine
        .new_function(
            realm,
            "gc",
            Rc::new(|engine: &mut dyn Engine, _cx: CallContext| {
                // JavaScript frames are live: this only requests a
                // collection, which runs at the engine's next safe point.
                engine.collect_garbage();
                Ok(Value::Undefined)
            }),
        )
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    engine
        .set(realm, &global, &PropertyKey::from("gc"), &gc)
        .unwrap();
    let probe = engine
        .new_host_object(realm, class, Box::new(Probe), None)
        .unwrap();
    engine
        .set(realm, &global, &PropertyKey::from("p"), &probe)
        .unwrap();
    drop(probe);
    let v = engine
        .eval_script(
            realm,
            "inner.js",
            "const local = { n: 41 }; gc(); p = null; gc(); local.n + 1",
        )
        .unwrap();
    assert_eq!(v, Value::Number(42.0));
    collect(&mut *engine, realm);
    assert_eq!(finalized.get(), 1);
}

#[test]
fn handles_outlive_their_engine_safely() {
    let (mut engine, realm, class, finalized) = setup();
    let obj = engine.new_object(realm).unwrap();
    let probe = engine
        .new_host_object(realm, class, Box::new(Probe), None)
        .unwrap();
    let copy = obj.clone();
    drop(engine);
    // The engine dropped every value it owned and ran no finalizer.
    assert_eq!(finalized.get(), 0);
    // The handles are inert: they can be inspected, cloned and dropped.
    assert!(format!("{copy:?}").starts_with("Object(Handle"));
    let again = copy.clone();
    drop((obj, copy, again, probe));

    // Given to another engine, an orphaned handle is an error.
    let mut other = agnijs::new_engine(Rc::new(Quiet));
    let realm = other.main_realm();
    let stale = {
        let (mut first, r, _, _) = setup();
        first.new_object(r).unwrap()
    };
    assert!(matches!(
        other.get(realm, &stale, &PropertyKey::from("x")),
        Err(Error::ForeignHandle)
    ));
}

#[test]
fn a_handle_from_another_engine_is_an_error_not_a_panic() {
    let (mut a, realm_a, _, _) = setup();
    let (mut b, realm_b, _, _) = setup();
    let from_a = a.new_object(realm_a).unwrap();
    let key = PropertyKey::from("x");
    let one = Value::Number(1.0);
    assert!(matches!(
        b.get(realm_b, &from_a, &key),
        Err(Error::ForeignHandle)
    ));
    assert!(matches!(
        b.set(realm_b, &from_a, &key, &one),
        Err(Error::ForeignHandle)
    ));
    assert!(matches!(
        b.call(realm_b, &from_a, &Value::Undefined, &[]),
        Err(Error::ForeignHandle)
    ));
    let obj_b = b.new_object(realm_b).unwrap();
    assert!(matches!(
        b.set(realm_b, &obj_b, &key, &from_a),
        Err(Error::ForeignHandle)
    ));
    // The infallible queries answer as they would for `undefined`.
    assert!(!b.is_array(&from_a));
    assert!(!b.strict_equals(&from_a, &from_a));
    // Both engines are still usable.
    assert_eq!(
        a.eval_script(realm_a, "a.js", "1").unwrap(),
        Value::Number(1.0)
    );
    assert_eq!(
        b.eval_script(realm_b, "b.js", "2").unwrap(),
        Value::Number(2.0)
    );
}

#[test]
fn an_unknown_realm_is_an_error() {
    let (mut engine, _, _, _) = setup();
    let nowhere = RealmId(9_999);
    assert!(matches!(
        engine.eval_script(nowhere, "x.js", "1"),
        Err(Error::UnknownRealm)
    ));
    assert!(matches!(
        engine.global_object(nowhere),
        Err(Error::UnknownRealm)
    ));
    assert!(matches!(
        engine.new_object(nowhere),
        Err(Error::UnknownRealm)
    ));
    assert!(matches!(engine.run_jobs(nowhere), Err(Error::UnknownRealm)));
}

#[test]
fn handles_are_engine_wide_and_work_in_every_realm() {
    let (mut engine, main, _, _) = setup();
    let second = engine.create_realm().unwrap();
    let obj = engine
        .eval_script(main, "o.js", "({ from: 'main' })")
        .unwrap();
    let key = PropertyKey::from("from");
    assert_eq!(
        engine.get(second, &obj, &key).unwrap(),
        Value::string("main")
    );
    // Realms live as long as the engine; an object from one realm stays
    // usable in another after that realm's last value is gone.
    let global2 = engine.global_object(second).unwrap();
    engine
        .set(second, &global2, &PropertyKey::from("borrowed"), &obj)
        .unwrap();
    drop(obj);
    collect(&mut *engine, main);
    let v = engine
        .eval_script(second, "read.js", "borrowed.from")
        .unwrap();
    assert_eq!(v, Value::string("main"));
}
