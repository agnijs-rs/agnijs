//! What the process really allocates when a 16 MiB memory limit stops a
//! script (TASK-103.25): 256 MiB of ArrayBuffers, a Map of 2,000,000
//! entries and a single 1 GiB `repeat` each fail without the engine
//! allocating anywhere near that much. A global allocator tracks the bytes
//! this thread holds and their peak. It also refuses any request that
//! would take the thread past `CAP`, so a regression aborts the test
//! instead of allocating gigabytes.
//!
//! This lives in the benchmark crate because a global allocator needs
//! `unsafe`, which every engine crate forbids. The behaviour itself is
//! tested in `agnijs-js-vm/tests/memory_limit.rs`.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error};
use agnijs_js_vm::Vm;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ptr::null_mut;

const MIB: usize = 1 << 20;
/// More than any scenario below may allocate, less than any one of them
/// allocates without the limit's checks.
const CAP: usize = 192 * MIB;

struct Peak;

thread_local! {
    static HELD: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn grow(by: usize) -> bool {
    HELD.with(|h| {
        let now = h.get() + by as isize;
        if now > CAP as isize {
            return false;
        }
        h.set(now);
        PEAK.with(|p| p.set(p.get().max(now)));
        true
    })
}

fn shrink(by: usize) {
    HELD.with(|h| h.set(h.get() - by as isize));
}

// SAFETY: forwards every request unchanged to the system allocator, or
// returns null (an allocation failure) past `CAP`. The counters are
// const-initialised thread locals, which never allocate. A block freed on
// another thread than the one that allocated it skews the counts, which
// only these tests read.
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !grow(layout.size()) {
            return null_mut();
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !grow(layout.size()) {
            return null_mut();
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() && !grow(new_size - layout.size()) {
            return null_mut();
        }
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            if new_size > layout.size() {
                shrink(new_size - layout.size());
            }
        } else if new_size < layout.size() {
            shrink(layout.size() - new_size);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Peak = Peak;

/// The bytes this thread allocated, at their peak, while `src` ran under
/// a 16 MiB limit, beyond what it held before; `src` must fail with the
/// memory-limit error.
fn peak_while_refused(src: &str) -> usize {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    vm.collect_garbage();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + 16 * MIB));
    let realm = vm.main_realm();
    let before = HELD.with(Cell::get);
    PEAK.with(|p| p.set(before));
    let r = vm.eval_script(realm, "t.js", src);
    let peak = PEAK.with(Cell::get) - before;
    assert!(matches!(r, Err(Error::OutOfMemory)), "{src}: {r:?}");
    peak.max(0) as usize
}

/// The counted heap may pass the limit by what the last collection had not
/// yet reclaimed and by one block the size of the request that took it
/// over, and the Rust allocator rounds blocks up: three times the limit
/// leaves room for that and none for the hundreds of megabytes below.
const BOUND: usize = 48 * MIB;

#[test]
fn a_16_mib_limit_stops_256_mib_of_array_buffers_without_allocating_them() {
    let peak = peak_while_refused(
        "const bufs = []; for (let i = 0; i < 64; i++) bufs.push(new ArrayBuffer(4 << 20)); bufs.length",
    );
    assert!(peak < BOUND, "peak {} MiB", peak / MIB);
}

#[test]
fn a_16_mib_limit_stops_a_map_of_two_million_entries_without_allocating_it() {
    let peak = peak_while_refused(
        "const m = new Map(); for (let i = 0; i < 2e6; i++) m.set(i, i); m.size",
    );
    assert!(peak < BOUND, "peak {} MiB", peak / MIB);
}

#[test]
fn a_16_mib_limit_stops_a_one_gib_repeat_without_allocating_it() {
    let peak = peak_while_refused("'x'.repeat(2 ** 30).length");
    assert!(peak < BOUND, "peak {} MiB", peak / MIB);
    let peak = peak_while_refused("new ArrayBuffer(4294967295).byteLength");
    assert!(peak < BOUND, "peak {} MiB", peak / MIB);
}

/// Set/delete churn reuses a Map's and a Set's slots (TASK-103.32): two
/// million rounds, which once kept a 32-byte slot each, allocate almost
/// nothing. No limit is set, so this measures the collections themselves.
#[test]
fn set_delete_churn_does_not_grow_the_process() {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    vm.collect_garbage();
    let realm = vm.main_realm();
    let before = HELD.with(Cell::get);
    PEAK.with(|p| p.set(before));
    let r = vm.eval_script(
        realm,
        "t.js",
        "const m = new Map(), s = new Set();
         for (let i = 0; i < 2e6; i++) { m.set(i, i); m.delete(i); s.add(i); s.delete(i); }
         m.size + s.size",
    );
    let peak = PEAK.with(Cell::get) - before;
    assert!(r.is_ok(), "{r:?}");
    assert!(peak < 8 * MIB as isize, "peak {} MiB", peak / MIB as isize);
}

// ---- native loops (TASK-103.53) ---------------------------------------------------------------
//
// Native loops that allocate from a bare length, a native iterator or one
// value referenced many times. Nothing counted bounds what they make, so
// they check the limit as they go (`Vm::poll`). Without that check each of
// these passes `CAP`, and the allocator's refusal aborts the test binary:
// run them one at a time to see which.

/// A native call cannot collect, so it may count twice the limit, garbage
/// included, before its poll refuses more (`Heap::would_exceed`); a vector
/// that doubles as it grows can hold twice what it counts. Four times the
/// limit is what that allows (64 MiB measured for `fill`, `Array.from` and
/// `Promise.all`); five leaves room for rounding and none for the
/// gigabytes these allocated before TASK-103.53.
const NATIVE_LOOP_BOUND: usize = 80 * MIB;

fn native_loop_peak(src: &str) {
    let peak = peak_while_refused(src);
    assert!(peak < NATIVE_LOOP_BOUND, "{src}: peak {} MiB", peak / MIB);
}

#[test]
fn native_loop_fill() {
    native_loop_peak("new Array(2e7).fill(0).length");
}

#[test]
fn native_loop_array_from_a_bare_length() {
    native_loop_peak("Array.from({ length: 2e7 }).length");
}

#[test]
fn native_loop_to_sorted_of_a_bare_length() {
    native_loop_peak("Array.prototype.toSorted.call({ length: 2e7 }).length");
}

#[test]
fn native_loop_apply_of_a_bare_length() {
    native_loop_peak("Array.apply(null, { length: 4e6 }).length");
}

#[test]
fn native_loop_spread_of_a_native_iterator() {
    native_loop_peak("[...new Array(5e6).keys()].length");
}

#[test]
fn native_loop_promise_all_of_a_native_iterator() {
    native_loop_peak("Promise.all(new Array(3e6).keys()); 0");
}

#[test]
fn native_loop_object_entries_of_a_typed_array() {
    native_loop_peak("Object.entries(new Uint8Array(4e6)).length");
}

#[test]
fn native_loop_list_format_of_one_string_many_times() {
    native_loop_peak("new Intl.ListFormat('en').format(new Array(6e5).fill('a'.repeat(1000))).length");
}

// ---- strings whose length is known before they are built (TASK-103.54) -----------------------
//
// These built-ins know their result's length from their input, so they
// check it before building: past the maximum string length, they throw
// with no allocation the size of the result. Before TASK-103.54 each built
// the whole result in Rust first (toHex of 512 MiB peaked at 2.57 GiB).

/// The bytes this thread allocated, at their peak, while `src` ran with the
/// maximum string length lowered to 1,000 units, after `setup` made its
/// input; `src` must throw the RangeError for a string that is too long.
fn peak_while_too_long(setup: &str, src: &str) -> usize {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    vm.set_max_string_length(1000);
    let realm = vm.main_realm();
    vm.eval_script(realm, "setup.js", setup).unwrap();
    let before = HELD.with(Cell::get);
    PEAK.with(|p| p.set(before));
    let r = vm.eval_script(realm, "t.js", src);
    let peak = PEAK.with(Cell::get) - before;
    match r {
        Err(Error::Exception(x)) => assert_eq!(
            (x.name.as_str(), x.message.as_str()),
            ("RangeError", "Invalid string length"),
            "{src}"
        ),
        r => panic!("{src}: {r:?}"),
    }
    peak.max(0) as usize
}

/// The 16 MiB input is made before the peak is measured and is not copied;
/// the results these no longer build are 22 to 32 MiB.
const LENGTH_KNOWN_BOUND: usize = 4 * MIB;

#[test]
fn to_hex_and_to_base64_check_the_length_before_building() {
    let setup = "globalThis.u = new Uint8Array(2 ** 24);";
    for src in ["u.toHex()", "u.toBase64()", "u.toBase64({ omitPadding: true })"] {
        let peak = peak_while_too_long(setup, src);
        assert!(peak < LENGTH_KNOWN_BOUND, "{src}: peak {} MiB", peak / MIB);
    }
}

/// The input of these is a string, which cannot pass the lowered maximum,
/// so the memory limit refuses them instead: 8 MiB of quotes, and a BigInt
/// of 2^26 bits, each under 16 MiB.
#[test]
fn html_methods_and_big_int_to_string_check_the_memory_before_building() {
    // `"` expands to `&quot;`, six units for one: 48 MiB.
    let peak = peak_while_refused("const q = '\"'.repeat(2 ** 23); 'x'.anchor(q).length");
    assert!(peak < BOUND, "anchor: peak {} MiB", peak / MIB);
    // 64 MiB of binary digits.
    let peak = peak_while_refused("const b = 1n << (1n << 26n); b.toString(2).length");
    assert!(peak < BOUND, "BigInt: peak {} MiB", peak / MIB);
}
