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

#[cfg(feature = "temporal")]
#[test]
fn durations_construct_read_and_print() {
    check("new Temporal.Duration(1, 2, 3, 4, 5, 6, 7, 8, 9, 10).toString()", "P1Y2M3W4DT5H6M7.00800901S");
    check("Temporal.Duration.from({ hours: -36 }).toString()", "-PT36H");
    check("Temporal.Duration.from('P1DT12H').hours + ''", "12");
    check("var d = Temporal.Duration.from('-PT1M'); [d.sign, d.blank, d.negated().minutes].join()", "-1,false,1");
    check("new Temporal.Duration().blank + ''", "true");
    check("Object.prototype.toString.call(new Temporal.Duration())", "[object Temporal.Duration]");
    check("JSON.stringify({ d: Temporal.Duration.from({ seconds: 90 }) })", "{\"d\":\"PT90S\"}");
}

#[cfg(feature = "temporal")]
#[test]
fn duration_arithmetic_rounding_and_totals() {
    check("Temporal.Duration.from('PT1H').add('PT30M').toString()", "PT1H30M");
    check("Temporal.Duration.from('PT1H53M').round({ smallestUnit: 'minutes', roundingIncrement: 15 }).toString()", "PT2H");
    check("Temporal.Duration.from('PT90M').round('hour').toString()", "PT2H");
    check("Temporal.Duration.from('P1M').total({ unit: 'day', relativeTo: '2024-02-01' }) + ''", "29");
    check("Temporal.Duration.from('PT36H').total('day') + ''", "1.5");
    check("Temporal.Duration.compare('PT1H', 'PT60M') + ''", "0");
    check("Temporal.Duration.compare('P1M', 'P30D', { relativeTo: '2024-02-01' }) + ''", "-1");
    check("Temporal.Duration.from('PT1.123456789S').toString({ fractionalSecondDigits: 3 })", "PT1.123S");
}

#[cfg(feature = "temporal")]
#[test]
fn duration_errors_have_the_specified_types() {
    let kind = |src: &str| format!("try {{ {src}; 'no error' }} catch (e) {{ e.constructor.name }}");
    check(&kind("Temporal.Duration(1)"), "TypeError");
    check(&kind("new Temporal.Duration(1.5)"), "RangeError");
    check(&kind("new Temporal.Duration(1, -1)"), "RangeError");
    check(&kind("new Temporal.Duration(2 ** 32)"), "RangeError");
    check(&kind("new Temporal.Duration(0, 0, 0, 0, 0, 0, 0, 0, 0, 1e300)"), "RangeError");
    check(&kind("Temporal.Duration.from('P')"), "RangeError");
    check(&kind("Temporal.Duration.from({})"), "TypeError");
    check(&kind("Temporal.Duration.from(5)"), "TypeError");
    check(&kind("Temporal.Duration.from('P1Y').add('P1D')"), "RangeError");
    check(&kind("Temporal.Duration.from('P1D').round({ smallestUnit: 'fortnight' })"), "RangeError");
    check(&kind("Temporal.Duration.from('P1D').round()"), "TypeError");
    check(&kind("Temporal.Duration.from('P1D').total({})"), "RangeError");
    check(&kind("Temporal.Duration.from('P1D') < 1"), "TypeError");
    check(&kind("Temporal.Duration.prototype.years"), "TypeError");
    check(&kind("Temporal.Duration.prototype.toString.call({})"), "TypeError");
}

#[cfg(feature = "temporal")]
#[test]
fn duration_options_are_read_once_each_in_specification_order() {
    check(
        "var log = []; var bag = {}; \
         ['largestUnit', 'relativeTo', 'roundingIncrement', 'roundingMode', 'smallestUnit'].forEach(function (k) { \
           Object.defineProperty(bag, k, { get: function () { log.push(k); return undefined; } }); }); \
         try { Temporal.Duration.from('PT1H').round(bag); } catch (e) {} \
         log.join()",
        "largestUnit,relativeTo,roundingIncrement,roundingMode,smallestUnit",
    );
    check(
        "var log = []; var bag = {}; \
         ['years', 'months', 'weeks', 'days', 'hours', 'minutes', 'seconds', 'milliseconds', 'microseconds', 'nanoseconds'].forEach(function (k) { \
           Object.defineProperty(bag, k, { get: function () { log.push(k); return 1; } }); }); \
         Temporal.Duration.from(bag); log.join()",
        "days,hours,microseconds,milliseconds,minutes,months,nanoseconds,seconds,weeks,years",
    );
}

#[cfg(feature = "temporal")]
#[test]
fn a_snapshot_refuses_a_temporal_object_and_names_it() {
    let mut vm = Vm::new(TestHost::new());
    let realm = vm.main_realm();
    vm.eval_script(realm, "t.js", "globalThis.d = new Temporal.Duration(1);").unwrap();
    match vm.snapshot(realm) {
        Err(agnijs_engine_api::Error::SnapshotUnsupported(m)) => {
            assert!(m.contains("a Temporal object") && m.contains("globalThis.d"), "{m}")
        }
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("the snapshot was taken"),
    }
}

#[cfg(feature = "temporal")]
#[test]
fn temporal_objects_count_against_the_memory_limit() {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    vm.collect_garbage();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + 8 * 1024 * 1024));
    let realm = vm.main_realm();
    let r = vm.eval_script(
        realm,
        "t.js",
        "{ const a = []; for (let i = 0; i < 1e6; i++) a.push(new Temporal.Duration(0, 0, 0, i)); }",
    );
    assert!(matches!(r, Err(agnijs_engine_api::Error::OutOfMemory)), "{r:?}");
    vm.collect_garbage();
    assert_eq!(vm.eval_script(realm, "t.js", "Temporal.Duration.from('P1D').days").unwrap(), Value::Number(1.0));
}
