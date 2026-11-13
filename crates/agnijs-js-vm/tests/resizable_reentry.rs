//! Built-ins that convert their arguments, or call a species constructor,
//! after they have read a buffer's length must read it again before they
//! touch the bytes: user code can shrink a resizable buffer in between
//! (TASK-103.41, 103.42, 103.43), and `transfer` must keep a resizable
//! buffer's length within its maximum (TASK-103.45).
//!
//! Each script runs under `catch_unwind`, so a panic fails its own test
//! with the script named instead of taking the whole suite down.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// The string `src` evaluates to, or a test failure naming `src` if it
/// panics or throws past the script.
fn run(src: &str) -> String {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut vm = Vm::new(TestHost::new());
        let realm = vm.main_realm();
        vm.eval_script(realm, "t.js", src)
    }));
    match r {
        Ok(Ok(Value::String(s))) => s.to_string_lossy(),
        Ok(other) => panic!("{src}\nexpected a string, got {other:?}"),
        Err(_) => panic!("{src}\npanicked"),
    }
}

/// `src` with a helper that names the error a function throws, or "ok".
fn check(src: &str, expected: &str) {
    let prelude =
        "function outcome(f) { try { return 'ok ' + f(); } catch (e) { return e.name; } }\n";
    assert_eq!(run(&format!("{prelude}{src}")), expected, "{src}");
}

// ---- TypedArray.prototype.copyWithin (TASK-103.41) -----------------------------------------------

/// The review's repro: the end argument shrinks the buffer under a
/// length-tracking view, so the source start is past the new end and
/// nothing is copied.
#[test]
fn copy_within_end_shrink_copies_nothing_past_the_new_length() {
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Uint8Array(rab); ta.set([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
         outcome(() => { ta.copyWithin(0, 10, { valueOf() { rab.resize(2); return 16; } }); return ta.length + ':' + ta.join(); })",
        "ok 2:1,2",
    );
}

#[test]
fn copy_within_target_and_start_shrink_never_panic() {
    // The target conversion shrinks: the source start (10) is past the end.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Uint8Array(rab); ta.fill(7);
         outcome(() => { ta.copyWithin({ valueOf() { rab.resize(2); return 0; } }, 10); return ta.join(); })",
        "ok 7,7",
    );
    // The start conversion shrinks: the target (12) is past the end.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Uint8Array(rab); ta.fill(7);
         outcome(() => { ta.copyWithin(12, { valueOf() { rab.resize(2); return 0; } }); return ta.join(); })",
        "ok 7,7",
    );
    // Both indices land exactly on the new end.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Uint8Array(rab); ta.fill(7);
         outcome(() => { ta.copyWithin(4, { valueOf() { rab.resize(4); return 4; } }); return ta.join(); })",
        "ok 7,7,7,7",
    );
}

#[test]
fn copy_within_wide_elements_and_an_offset_view_clamp_to_the_new_length() {
    // Int32, a length-tracking view at byte offset 4: elements [1,2,3]
    // shrink to [1,2]; copying from index 1 to 0 then copies only one.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Int32Array(rab, 4); ta.set([1, 2, 3]);
         outcome(() => { ta.copyWithin(0, 1, { valueOf() { rab.resize(12); return 3; } }); return ta.join(); })",
        "ok 2,2",
    );
    // BigInt64, start shrinks the view below the target.
    check(
        "var rab = new ArrayBuffer(32, { maxByteLength: 32 });
         var ta = new BigInt64Array(rab); ta.set([1n, 2n, 3n, 4n]);
         outcome(() => { ta.copyWithin(3, { valueOf() { rab.resize(16); return 0; } }); return ta.join(); })",
        "ok 1,2",
    );
}

#[test]
fn copy_within_on_a_fixed_length_view_that_goes_out_of_bounds_throws() {
    for arg in [
        "{ valueOf: evil }, 0, 1",
        "0, { valueOf: evil }, 3",
        "0, 1, { valueOf: evil }",
    ] {
        check(
            &format!(
                "var rab = new ArrayBuffer(16, {{ maxByteLength: 16 }});
                 var ta = new Uint8Array(rab, 0, 16);
                 function evil() {{ rab.resize(2); return 2; }}
                 outcome(() => ta.copyWithin({arg}))"
            ),
            "TypeError",
        );
    }
}

// ---- ArrayBuffer.prototype.slice (TASK-103.42) ----------------------------------------------------

#[test]
fn slice_after_a_shrink_in_the_end_argument_is_zero_filled() {
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 }); new Uint8Array(rab).fill(7);
         outcome(() => { var r = rab.slice(8, { valueOf() { rab.resize(2); return 16; } }); return r.byteLength + ':' + new Uint8Array(r).join(''); })",
        "ok 8:00000000",
    );
}

#[test]
fn slice_after_a_shrink_in_the_start_argument_is_zero_filled() {
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 }); new Uint8Array(rab).fill(7);
         outcome(() => { var r = rab.slice({ valueOf() { rab.resize(2); return 8; } }, 16); return r.byteLength + ':' + new Uint8Array(r).join(''); })",
        "ok 8:00000000",
    );
}

#[test]
fn slice_after_a_shrink_in_the_species_constructor_copies_what_is_left() {
    // Shrunk below the start: nothing to copy.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 }); new Uint8Array(rab).fill(7);
         rab.constructor = { [Symbol.species]: function (n) { rab.resize(2); return new ArrayBuffer(n); } };
         outcome(() => { var r = rab.slice(8, 16); return r.byteLength + ':' + new Uint8Array(r).join(''); })",
        "ok 8:00000000",
    );
    // Shrunk to the middle of the range: the bytes still there are copied.
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 }); new Uint8Array(rab).fill(7);
         rab.constructor = { [Symbol.species]: function (n) { rab.resize(12); return new ArrayBuffer(n); } };
         outcome(() => { var r = rab.slice(8, 16); return r.byteLength + ':' + new Uint8Array(r).join(''); })",
        "ok 8:77770000",
    );
}

// ---- Atomics (TASK-103.43) ------------------------------------------------------------------------

/// Every Atomics operation that reads or writes an element, with the
/// shrink in the index conversion and in each value conversion, on Int32
/// and BigInt64 length-tracking views: each throws a RangeError.
#[test]
fn atomics_after_a_shrink_in_a_conversion_throw_range_error() {
    let ops: [(&str, usize); 9] = [
        ("add", 1),
        ("sub", 1),
        ("and", 1),
        ("or", 1),
        ("xor", 1),
        ("exchange", 1),
        ("compareExchange", 2),
        ("store", 1),
        ("load", 0),
    ];
    for (ctor, one) in [("Int32Array", "1"), ("BigInt64Array", "1n")] {
        for (op, values) in ops {
            // Shrink in the index conversion, then in each value conversion.
            for shrink_at in 0..=values {
                let mut args = Vec::new();
                args.push(if shrink_at == 0 {
                    "{ valueOf() { rab.resize(4); return 3; } }".to_string()
                } else {
                    "3".to_string()
                });
                for v in 1..=values {
                    args.push(if shrink_at == v {
                        format!("{{ valueOf() {{ rab.resize(4); return {one}; }} }}")
                    } else {
                        one.to_string()
                    });
                }
                let src = format!(
                    "var rab = new ArrayBuffer(32, {{ maxByteLength: 32 }});
                     var ta = new {ctor}(rab);
                     outcome(() => Atomics.{op}(ta, {}))",
                    args.join(", ")
                );
                check(&src, "RangeError");
            }
        }
    }
}

/// An index still in bounds after the shrink works as before.
#[test]
fn atomics_after_a_shrink_that_keeps_the_index_in_bounds_still_work() {
    check(
        "var rab = new ArrayBuffer(16, { maxByteLength: 16 });
         var ta = new Int32Array(rab); ta[0] = 5;
         outcome(() => Atomics.add(ta, 0, { valueOf() { rab.resize(4); return 1; } }) + ',' + ta[0])",
        "ok 5,6",
    );
}

// ---- ArrayBuffer.prototype.transfer (TASK-103.45) -------------------------------------------------

#[test]
fn transfer_past_max_byte_length_throws_and_leaves_the_source_attached() {
    check(
        "var rab = new ArrayBuffer(4, { maxByteLength: 8 });
         var r = outcome(() => rab.transfer(100));
         r + ':' + rab.detached + ':' + rab.byteLength",
        "RangeError:false:4",
    );
    // Up to the maximum is fine, and keeps resizability.
    check(
        "var rab = new ArrayBuffer(4, { maxByteLength: 8 });
         outcome(() => { var t = rab.transfer(8); return t.byteLength + ':' + t.maxByteLength + ':' + t.resizable + ':' + rab.detached; })",
        "ok 8:8:true:true",
    );
}

#[test]
fn transfer_to_fixed_length_has_no_maximum() {
    check(
        "var rab = new ArrayBuffer(4, { maxByteLength: 8 });
         outcome(() => { var t = rab.transferToFixedLength(100); return t.byteLength + ':' + t.resizable + ':' + rab.detached; })",
        "ok 100:false:true",
    );
}
