//! Temporal (the `temporal` cargo feature, ADR 0027). test262's Temporal
//! suites are the conformance gate (`test262 run --only-temporal`); these
//! tests pin the engine-specific parts: the feature switch, the host
//! hooks Temporal reads, and the security limits.
//!
//! `cargo test -p agnijs-js-vm --features temporal --test temporal` checks
//! the feature; without it only the absence of the global is checked.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn eval(src: &str) -> Value {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    vm.eval_script(realm, "temporal.js", src)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn check(src: &str, expected: &str) {
    assert_eq!(eval(src), Value::string(expected), "{src}");
}

#[cfg(not(feature = "temporal"))]
#[test]
fn without_the_feature_there_is_no_temporal_global() {
    check("typeof Temporal", "undefined");
}

#[cfg(feature = "temporal")]
#[test]
fn the_temporal_namespace_is_an_ordinary_object_with_its_tag() {
    check("typeof Temporal", "object");
    check("Object.prototype.toString.call(Temporal)", "[object Temporal]");
    check("Object.getPrototypeOf(Temporal) === Object.prototype ? 'yes' : 'no'", "yes");
    check(
        "var d = Object.getOwnPropertyDescriptor(globalThis, 'Temporal'); [d.writable, d.enumerable, d.configurable].join()",
        "true,false,true",
    );
}
