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

#[cfg(feature = "temporal")]
#[test]
fn instants_construct_convert_and_print() {
    check("new Temporal.Instant(0n).toString()", "1970-01-01T00:00:00Z");
    check("Temporal.Instant.from('2020-01-01T00:00:00.5+01:00').toString()", "2019-12-31T23:00:00.5Z");
    check("String(Temporal.Instant.fromEpochMilliseconds(1e12).epochNanoseconds === 10n ** 18n)", "true");
    check("Temporal.Instant.fromEpochNanoseconds(-1n).epochMilliseconds + ''", "-1");
    check("typeof Temporal.Instant.fromEpochNanoseconds(5n).epochNanoseconds", "bigint");
    check("Temporal.Instant.compare('1970-01-01T00:00Z', '1970-01-01T00:00:01Z') + ''", "-1");
    check("Temporal.Instant.from('1970-01-01T00:00Z').equals(new Temporal.Instant(0n)) + ''", "true");
    check("Object.prototype.toString.call(new Temporal.Instant(0n))", "[object Temporal.Instant]");
    check("JSON.stringify([new Temporal.Instant(1000000000n)])", "[\"1970-01-01T00:00:01Z\"]");
    check("var i = new Temporal.Instant(0n); Temporal.Instant.from(i) !== i ? 'copy' : 'same'", "copy");
}

#[cfg(feature = "temporal")]
#[test]
fn instant_arithmetic_differences_rounding_and_strings() {
    let at = |s: &str| format!("Temporal.Instant.from('{s}')");
    check(&format!("{}.add({{ hours: 1 }}).toString()", at("2020-01-01T00:00Z")), "2020-01-01T01:00:00Z");
    check(&format!("{}.subtract('PT1S').toString()", at("2020-01-01T00:00Z")), "2019-12-31T23:59:59Z");
    check(&format!("{}.until('2020-01-02T01:30Z').toString()", at("2020-01-01T00:00Z")), "PT91800S");
    check(
        &format!("{}.until('2020-01-02T01:30Z', {{ largestUnit: 'hours' }}).toString()", at("2020-01-01T00:00Z")),
        "PT25H30M",
    );
    check(
        &format!("{}.since('2020-01-01T00:00Z', {{ smallestUnit: 'hour' , roundingMode: 'halfExpand' }}).toString()", at("2020-01-01T01:30Z")),
        "PT2H",
    );
    check(&format!("{}.round({{ smallestUnit: 'hour' }}).toString()", at("2020-01-01T00:29:59Z")), "2020-01-01T00:00:00Z");
    check(&format!("{}.round('minute').toString()", at("2020-01-01T00:00:30Z")), "2020-01-01T00:01:00Z");
    check(
        &format!("{}.toString({{ fractionalSecondDigits: 3 }})", at("2020-01-01T00:00:00.123456789Z")),
        "2020-01-01T00:00:00.123Z",
    );
    check(&format!("{}.toString({{ timeZone: '+05:30' }})", at("2020-01-01T00:00Z")), "2020-01-01T05:30:00+05:30");
    check(
        &format!("{}.toString({{ timeZone: 'America/New_York' }})", at("2020-01-01T00:00Z")),
        "2019-12-31T19:00:00-05:00",
    );
}

#[cfg(feature = "temporal")]
#[test]
fn instant_errors_have_the_specified_types() {
    let kind = |src: &str| format!("try {{ {src}; 'no error' }} catch (e) {{ e.constructor.name }}");
    check(&kind("Temporal.Instant(0n)"), "TypeError");
    check(&kind("new Temporal.Instant(0)"), "TypeError");
    check(&kind("new Temporal.Instant(8640000000000000000001n)"), "RangeError");
    check(&kind("Temporal.Instant.from('2020-01-01')"), "RangeError");
    check(&kind("Temporal.Instant.from(5)"), "TypeError");
    check(&kind("Temporal.Instant.fromEpochMilliseconds(1.5)"), "RangeError");
    check(&kind("Temporal.Instant.fromEpochMilliseconds(8.64e15 + 1)"), "RangeError");
    check(&kind("new Temporal.Instant(0n).add({ years: 1 })"), "RangeError");
    check(&kind("new Temporal.Instant(0n).until(new Temporal.Instant(0n), { largestUnit: 'day' })"), "RangeError");
    check(&kind("new Temporal.Instant(0n).round()"), "TypeError");
    check(&kind("new Temporal.Instant(0n).round({})"), "RangeError");
    check(&kind("new Temporal.Instant(0n).toString({ smallestUnit: 'hour' })"), "RangeError");
    check(&kind("new Temporal.Instant(0n).toString({ timeZone: 'Mars/Olympus_Mons' })"), "RangeError");
    check(&kind("new Temporal.Instant(0n) < 1"), "TypeError");
    check(&kind("Temporal.Instant.prototype.toString.call(new Temporal.Duration())"), "TypeError");
    check(&kind("new Date(NaN).toTemporalInstant()"), "RangeError");
    check(&kind("Date.prototype.toTemporalInstant.call({})"), "TypeError");
}

#[cfg(feature = "temporal")]
#[test]
fn instant_options_are_read_in_specification_order() {
    let order = |call: &str, keys: &str| {
        format!(
            "var log = []; var bag = {{}}; \
             [{keys}].forEach(function (k) {{ \
               Object.defineProperty(bag, k, {{ get: function () {{ log.push(k); return undefined; }} }}); }}); \
             try {{ {call}; }} catch (e) {{}} \
             log.join()"
        )
    };
    let all = "'largestUnit', 'fractionalSecondDigits', 'roundingIncrement', 'roundingMode', 'smallestUnit', 'timeZone'";
    check(
        &order("new Temporal.Instant(0n).until(new Temporal.Instant(0n), bag)", all),
        "largestUnit,roundingIncrement,roundingMode,smallestUnit",
    );
    check(&order("new Temporal.Instant(0n).round(bag)", all), "roundingIncrement,roundingMode,smallestUnit");
    check(
        &order("new Temporal.Instant(0n).toString(bag)", all),
        "fractionalSecondDigits,roundingMode,smallestUnit,timeZone",
    );
}

#[cfg(feature = "temporal")]
#[test]
fn dates_convert_to_instants() {
    check("String(new Date(1500).toTemporalInstant().epochNanoseconds === 1500000000n)", "true");
    check("new Date(-1).toTemporalInstant().toString()", "1969-12-31T23:59:59.999Z");
}

/// A host with a pinned clock and time zone, the two inputs `Temporal.Now`
/// reads.
#[cfg(feature = "temporal")]
struct Pinned {
    zone: &'static str,
}

#[cfg(feature = "temporal")]
impl agnijs_engine_api::Host for Pinned {
    fn now_ms(&self) -> f64 {
        1_700_000_000_123.0
    }
    fn default_time_zone(&self) -> String {
        self.zone.to_string()
    }
}

#[cfg(feature = "temporal")]
fn eval_pinned(zone: &'static str, src: &str) -> Value {
    let mut vm = Vm::new(std::rc::Rc::new(Pinned { zone }));
    let realm = vm.main_realm();
    vm.eval_script(realm, "now.js", src).unwrap_or_else(|e| panic!("{e}"))
}

#[cfg(feature = "temporal")]
#[test]
fn now_reads_only_the_host_clock_and_zone() {
    assert_eq!(eval_pinned("UTC", "Temporal.Now.instant().epochMilliseconds"), Value::Number(1_700_000_000_123.0));
    assert_eq!(eval_pinned("UTC", "Temporal.Now.instant().toString()"), Value::string("2023-11-14T22:13:20.123Z"));
    assert_eq!(eval_pinned("america/new_york", "Temporal.Now.timeZoneId()"), Value::string("America/New_York"));
    assert_eq!(eval_pinned("+05:30", "Temporal.Now.timeZoneId()"), Value::string("+05:30"));
    // A zone the database does not know falls back to UTC.
    assert_eq!(eval_pinned("Not/A_Zone", "Temporal.Now.timeZoneId()"), Value::string("UTC"));
    check("Object.prototype.toString.call(Temporal.Now)", "[object Temporal.Now]");
    check("typeof Temporal.Now.instant", "function");
}
