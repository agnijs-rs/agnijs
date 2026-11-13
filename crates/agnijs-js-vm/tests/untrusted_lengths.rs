//! A length or size that a script chooses must not size a native
//! allocation before anything is read: `BigInt.asIntN` / `asUintN` with a
//! huge bit count (TASK-103.44), the argument list of `apply`,
//! `Reflect.apply` and `Reflect.construct` (TASK-103.48), and the capture
//! list read from a custom `exec` result (TASK-103.49). Each used to abort
//! the process on a failed reservation, which nothing can catch.
//!
//! A red run of these tests aborts rather than fails; run them one at a
//! time (`cargo test --test untrusted_lengths <name>`) to see which.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, Host, RealmId, Value};
use agnijs_js_vm::Vm;
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Generous for a debug build on a busy machine. Every case below either
/// returns at once or would allocate (or loop) for far longer.
const BOUND: Duration = Duration::from_secs(5);

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> Result<Value, Error> {
    let realm = vm.main_realm();
    let r = catch_unwind(AssertUnwindSafe(|| vm.eval_script(realm, "t.js", src)));
    r.unwrap_or_else(|_| panic!("{src}\npanicked"))
}

/// `src` (an expression) evaluates, quickly, to the string `expected`. A
/// thrown error shows as its name.
fn check(src: &str, expected: &str) {
    let mut vm = vm();
    let wrapped =
        format!("(() => {{ try {{ return String({src}); }} catch (e) {{ return e.name; }} }})()");
    let t = Instant::now();
    let r = eval(&mut vm, &wrapped);
    let took = t.elapsed();
    assert_eq!(r.ok(), Some(Value::string(expected)), "{src}");
    assert!(took < BOUND, "{src}: took {took:?}");
}

/// A VM whose limit is `limit` bytes above what a fresh realm uses.
fn limited(limit: usize) -> Vm {
    let mut vm = vm();
    vm.collect_garbage();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + limit));
    vm
}

/// `src` stops with the memory-limit error and the engine carries on.
fn out_of_memory(limit: usize, src: &str) {
    let mut vm = limited(limit);
    let r = eval(&mut vm, src);
    assert!(matches!(r, Err(Error::OutOfMemory)), "{src}: {r:?}");
    vm.collect_garbage();
    assert_eq!(
        eval(&mut vm, "'a' + 'b'").ok(),
        Some(Value::string("ab")),
        "after {src}"
    );
}

struct StopHost {
    stop: Cell<bool>,
}

impl Host for StopHost {
    fn interrupt(&self) -> bool {
        self.stop.get()
    }
}

/// `setup` runs normally, then `hot` with the interrupt hook saying stop:
/// it is interrupted within [`BOUND`] and the engine carries on.
fn interrupted(setup: &str, hot: &str) {
    let host = Rc::new(StopHost {
        stop: Cell::new(false),
    });
    let mut e = agnijs_js_vm::make_engine(host.clone());
    let realm: RealmId = e.main_realm();
    e.eval_script(realm, "setup.js", setup)
        .unwrap_or_else(|err| panic!("{err}: {setup}"));
    host.stop.set(true);
    let t = Instant::now();
    let r = catch_unwind(AssertUnwindSafe(|| e.eval_script(realm, "hot.js", hot)))
        .unwrap_or_else(|_| panic!("{hot}\npanicked"));
    let took = t.elapsed();
    let got: String = format!("{r:?}").chars().take(80).collect();
    assert!(
        matches!(r, Err(Error::Interrupted)),
        "{hot}: expected an interrupt, got {got} after {took:?}"
    );
    assert!(took < BOUND, "{hot}: the interrupt took {took:?}");
    host.stop.set(false);
    assert_eq!(
        e.eval_script(realm, "after.js", "1 + 1").unwrap(),
        Value::Number(2.0)
    );
}

// ---- BigInt.asIntN and BigInt.asUintN (TASK-103.44) -----------------------------------------------

/// A value that already fits comes back unchanged, whatever the bit count.
#[test]
fn as_int_n_with_a_huge_bit_count_returns_a_value_that_fits() {
    for bits in ["2 ** 53 - 1", "2 ** 40", "2 ** 31"] {
        for (x, out) in [
            ("-5n", "-5"),
            ("0n", "0"),
            ("5n", "5"),
            ("-(2n ** 100n)", "-1267650600228229401496703205376"),
        ] {
            check(&format!("BigInt.asIntN({bits}, {x})"), out);
        }
    }
}

#[test]
fn as_uint_n_with_a_huge_bit_count_returns_a_non_negative_value() {
    for bits in ["2 ** 53 - 1", "2 ** 40", "2 ** 31"] {
        for (x, out) in [
            ("0n", "0"),
            ("5n", "5"),
            ("2n ** 100n", "1267650600228229401496703205376"),
        ] {
            check(&format!("BigInt.asUintN({bits}, {x})"), out);
        }
    }
}

/// asUintN of a negative value has `bits` bits: past the largest BigInt
/// (the same 2^30 bits as `<<`) it is a RangeError, not an allocation.
#[test]
fn as_uint_n_of_a_negative_value_with_a_huge_bit_count_throws_range_error() {
    for bits in ["2 ** 53 - 1", "2 ** 40", "2 ** 31", "2 ** 30 + 1"] {
        for x in ["-5n", "-1n", "-(2n ** 100n)"] {
            check(&format!("BigInt.asUintN({bits}, {x})"), "RangeError");
        }
    }
}

/// The results the fast paths skip are unchanged.
#[test]
fn as_int_n_and_as_uint_n_wrap_as_before() {
    for (src, out) in [
        ("BigInt.asIntN(8, 255n)", "-1"),
        ("BigInt.asIntN(8, 128n)", "-128"),
        ("BigInt.asIntN(8, 127n)", "127"),
        ("BigInt.asIntN(8, -129n)", "127"),
        ("BigInt.asIntN(3, 25n)", "1"),
        ("BigInt.asIntN(3, 4n)", "-4"),
        ("BigInt.asIntN(1, 1n)", "-1"),
        ("BigInt.asIntN(0, 5n)", "0"),
        ("BigInt.asIntN(64, 2n ** 63n)", "-9223372036854775808"),
        ("BigInt.asIntN(64, -(2n ** 63n))", "-9223372036854775808"),
        ("BigInt.asIntN(65, -(2n ** 64n))", "-18446744073709551616"),
        ("BigInt.asIntN(65, 2n ** 64n)", "-18446744073709551616"),
        ("BigInt.asUintN(8, -1n)", "255"),
        ("BigInt.asUintN(3, -3n)", "5"),
        ("BigInt.asUintN(64, -1n)", "18446744073709551615"),
        ("BigInt.asUintN(64, 2n ** 64n + 7n)", "7"),
        ("BigInt.asUintN(0, -5n)", "0"),
        ("BigInt.asUintN(200, -1n) === 2n ** 200n - 1n", "true"),
        ("BigInt.asIntN(200, 2n ** 199n) === -(2n ** 199n)", "true"),
    ] {
        check(src, out);
    }
}

/// A large result is checked against the memory limit before it is built:
/// 2^27 bits is 16 MiB.
#[test]
fn a_large_as_uint_n_result_is_refused_by_the_memory_limit() {
    out_of_memory(1 << 20, "BigInt.asUintN(2 ** 27, -1n)");
}

// ---- apply, Reflect.apply, Reflect.construct (TASK-103.48) ----------------------------------------

#[test]
fn an_argument_list_with_a_huge_length_throws_range_error() {
    for len in ["2 ** 32 - 1", "2 ** 40", "2 ** 53 - 1"] {
        for call in [
            // The interpreter's own path for `f.apply(...)`.
            format!("Math.max.apply(null, {{ length: {len} }})"),
            // The native Function.prototype.apply.
            format!("Function.prototype.apply.call(Math.max, null, {{ length: {len} }})"),
            format!("Reflect.apply(Math.max, null, {{ length: {len} }})"),
            format!("Reflect.construct(function () {{}}, {{ length: {len} }})"),
            // A Proxy ownKeys trap result is read the same way.
            format!("Reflect.ownKeys(new Proxy({{}}, {{ ownKeys: () => ({{ length: {len} }}) }}))"),
        ] {
            check(&call, "RangeError");
        }
    }
}

#[test]
fn ordinary_argument_lists_still_work() {
    check("Math.max.apply(null, { length: 100000 })", "NaN");
    check("Math.max.apply(null, { length: 3, 0: 1, 1: 9, 2: 4 })", "9");
    check("Reflect.apply(Math.max, null, [1, 9, 4])", "9");
    check(
        "Reflect.construct(Array, { length: 2, 0: 'a', 1: 'b' }).join()",
        "a,b",
    );
    // A dense array is already in memory, so its length is not capped.
    check("Math.max.apply(null, new Array(5000000).fill(1))", "1");
}

/// The list is checked against the memory limit before it is built:
/// 2^21 values are 32 MiB.
#[test]
fn a_long_argument_list_is_refused_by_the_memory_limit() {
    out_of_memory(4 << 20, "Math.max.apply(null, { length: 2 ** 21 })");
    out_of_memory(
        4 << 20,
        "Reflect.apply(Math.max, null, { length: 2 ** 21 })",
    );
}

/// The fill loop polls the interrupt.
#[test]
fn building_a_long_argument_list_is_interrupted() {
    interrupted(
        "globalThis.slow = new Proxy({ length: 4000000 }, {});",
        "Math.max.apply(null, slow)",
    );
    interrupted(
        "globalThis.slow = new Proxy({ length: 4000000 }, {});",
        "Reflect.apply(Math.max, null, slow)",
    );
}

// ---- RegExp exec results (TASK-103.49) ------------------------------------------------------------

#[test]
fn a_custom_exec_result_with_a_huge_length_does_not_abort_replace() {
    for len in ["2 ** 40", "2 ** 32 + 1", "2 ** 53 - 1"] {
        for repl in ["'b'", "'[$1]'", "() => 'b'"] {
            check(
                &format!(
                    "(() => {{ const re = /a/; re.exec = () => ({{ length: {len}, 0: 'a', index: 0 }}); return 'a'.replace(re, {repl}); }})()"
                ),
                "RangeError",
            );
        }
    }
}

/// Indices past 2^32 are read under their own keys, not truncated: a
/// result just under the cap with a capture at a high index still
/// substitutes the right captures.
#[test]
fn custom_exec_captures_are_read_by_their_own_index() {
    check(
        "(() => { const re = /a/; re.exec = () => ({ length: 3, 0: 'a', 1: 'x', 2: 'y', index: 0 }); return 'a'.replace(re, '[$2$1]'); })()",
        "[yx]",
    );
    check(
        "(() => { const re = /a/; re.exec = () => ({ length: 3, 0: 'a', 1: 'x', 2: 'y', index: 0 }); return 'a'.replace(re, (m, a, b) => b + a); })()",
        "yx",
    );
}

#[test]
fn a_long_capture_list_is_refused_by_the_memory_limit() {
    out_of_memory(
        4 << 20,
        "const re = /a/; re.exec = () => ({ length: 2 ** 21, 0: 'a', index: 0 }); 'a'.replace(re, 'b')",
    );
}

/// Reading the captures polls the interrupt, in replace and in split.
#[test]
fn reading_a_long_capture_list_is_interrupted() {
    interrupted(
        "globalThis.re = /a/; re.exec = () => new Proxy({ length: 4000000, 0: 'a', index: 0 }, {});",
        "'a'.replace(re, 'b')",
    );
    // split reads the captures of each match from its splitter's exec.
    interrupted(
        "globalThis.re = /a/;
         re.constructor = { [Symbol.species]: function () {
           let done = false;
           return { lastIndex: 0, flags: 'y', exec() {
             if (done) return null; done = true; this.lastIndex = 1;
             return new Proxy({ length: 4000000, 0: 'a', index: 0 }, {});
           } };
         } };",
        "re[Symbol.split]('ab')",
    );
}
