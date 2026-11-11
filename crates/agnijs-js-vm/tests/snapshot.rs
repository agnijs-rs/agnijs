//! Realm snapshots (TASK-101.2): an initialised realm, built-ins plus an
//! embedder-run bootstrap script, written with `Engine::snapshot` and
//! restored with `make_engine_from_snapshot`, through the engine interface
//! only.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, RealmId, Value};

fn engine() -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine(TestHost::new());
    let realm = e.main_realm();
    (e, realm)
}

fn eval(e: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    e.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

fn snapshot_of(bootstrap: &str) -> Vec<u8> {
    let (mut e, realm) = engine();
    eval(&mut *e, realm, bootstrap);
    e.snapshot(realm).unwrap_or_else(|err| panic!("snapshot: {err}"))
}

fn restore(bytes: &[u8]) -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), bytes)
        .unwrap_or_else(|err| panic!("restore: {err}"));
    let realm = e.main_realm();
    (e, realm)
}

#[test]
fn a_bootstrapped_realm_survives_a_round_trip() {
    let bytes = snapshot_of(
        "var counter = 40;
         function bump(n) { counter += n; return counter; }
         globalThis.config = { name: 'app', sizes: [1, 2, 3] };",
    );
    let (mut e, realm) = restore(&bytes);
    assert_eq!(eval(&mut *e, realm, "bump(2)"), Value::Number(42.0));
    assert_eq!(
        eval(&mut *e, realm, "config.name + config.sizes.length"),
        Value::string("app3")
    );
    assert_eq!(eval(&mut *e, realm, "[1, 2].map(x => x * 2).join()"), Value::string("2,4"));
}

#[test]
fn a_snapshot_from_another_format_version_is_rejected() {
    let mut bytes = snapshot_of("var x = 1;");
    // The container's format tag follows the four-byte magic.
    let at = bytes.iter().position(|&b| b == b'-').unwrap();
    bytes[at] = b'_';
    match agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), &bytes) {
        Err(Error::BytecodeMismatch { .. }) => {}
        Err(other) => panic!("expected a version mismatch, got {other}"),
        Ok(_) => panic!("a mismatched snapshot was accepted"),
    }
}
