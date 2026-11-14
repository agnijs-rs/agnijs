//! What the memory limit counts (TASK-103.25). Besides objects, strings,
//! environments, symbols and BigInts, it counts ArrayBuffer and
//! SharedArrayBuffer backing stores and the entries of Map, Set, WeakMap
//! and WeakSet, and `memory_usage` reports them. A built-in that would
//! allocate a large block (a buffer, a repeated or padded string, a join,
//! a concatenation) checks the limit before it allocates, so one call
//! cannot overshoot it by gigabytes. `agnijs-js-bench/tests/memory_limit_peak.rs`
//! measures what the process really allocates in the same scenarios.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, Value};
use agnijs_js_vm::Vm;

const MIB: usize = 1 << 20;

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    // The loops below would collect at every step under AGNIJS_GC_STRESS.
    vm.heap.stress = false;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> Result<Value, Error> {
    let realm = vm.main_realm();
    vm.eval_script(realm, "t.js", src)
}

/// A VM whose limit is `limit` bytes above what a fresh realm uses.
fn limited(limit: usize) -> Vm {
    let mut vm = vm();
    vm.collect_garbage();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + limit));
    vm
}

/// `src` stops with the memory-limit error, and the engine runs ordinary
/// code afterwards. `src` runs in a block, so what its declarations hold
/// dies with it instead of staying in the global scope.
fn out_of_memory(vm: &mut Vm, src: &str) {
    let r = eval(vm, &format!("{{ {src} }}"));
    assert!(matches!(r, Err(Error::OutOfMemory)), "{src}: {r:?}");
    vm.collect_garbage();
    assert_eq!(
        eval(vm, "'ab' + 'cd'").unwrap(),
        Value::string("abcd"),
        "after {src}"
    );
}

fn heap_bytes(vm: &mut Vm) -> usize {
    vm.memory_usage().heap_bytes
}

// ---- ArrayBuffer and SharedArrayBuffer --------------------------------------------------------

/// The probe from the review: 64 buffers of 4 MiB under a 16 MiB limit.
#[test]
fn a_16_mib_limit_stops_256_mib_of_array_buffers() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "const bufs = []; for (let i = 0; i < 64; i++) bufs.push(new ArrayBuffer(4 << 20)); bufs.length",
    );
    out_of_memory(
        &mut vm,
        "const sabs = []; for (let i = 0; i < 64; i++) sabs.push(new SharedArrayBuffer(4 << 20)); sabs.length",
    );
    // Typed arrays allocate their buffer the same way.
    out_of_memory(
        &mut vm,
        "const tas = []; for (let i = 0; i < 64; i++) tas.push(new Float64Array(1 << 19)); tas.length",
    );
}

/// One buffer larger than the limit is refused before it is allocated.
#[test]
fn one_buffer_past_the_limit_is_refused() {
    let mut vm = limited(16 * MIB);
    out_of_memory(&mut vm, "new ArrayBuffer(4294967295).byteLength");
    out_of_memory(&mut vm, "new SharedArrayBuffer(1 << 30).byteLength");
    out_of_memory(&mut vm, "new Uint8Array(1 << 30).length");
    out_of_memory(
        &mut vm,
        "new ArrayBuffer(1 << 20).transfer(1 << 30).byteLength",
    );
}

#[test]
fn resizable_buffer_growth_counts() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "const b = new ArrayBuffer(0, { maxByteLength: 1 << 30 }); b.resize(1 << 30); b.byteLength",
    );
    out_of_memory(
        &mut vm,
        "const s = new SharedArrayBuffer(0, { maxByteLength: 1 << 30 }); s.grow(1 << 30); s.byteLength",
    );
    // Many small growths add up.
    out_of_memory(
        &mut vm,
        "const keep = [];
         for (let i = 0; i < 64; i++) {
             const b = new ArrayBuffer(0, { maxByteLength: 8 << 20 });
             b.resize(4 << 20);
             keep.push(b);
         }",
    );
    // Shrinking gives the bytes back.
    assert_eq!(
        eval(
            &mut vm,
            "const keep2 = [];
             for (let i = 0; i < 64; i++) {
                 const b = new ArrayBuffer(4 << 20, { maxByteLength: 8 << 20 });
                 b.resize(16);
                 keep2.push(b);
             }
             keep2.length"
        )
        .unwrap(),
        Value::Number(64.0)
    );
}

/// Buffers that die are released from the count, so a loop that drops
/// each one runs to the end under a limit far below what it allocates.
#[test]
fn dead_buffers_are_released_from_the_count() {
    let mut vm = limited(16 * MIB);
    let v = eval(
        &mut vm,
        "let n = 0; for (let i = 0; i < 200; i++) n += new ArrayBuffer(1 << 20).byteLength; n",
    )
    .unwrap();
    assert_eq!(v, Value::Number((200 * MIB) as f64));
    let v = eval(
        &mut vm,
        "let m = 0;
         for (let i = 0; i < 100; i++) {
             const b = new ArrayBuffer(0, { maxByteLength: 4 << 20 });
             b.resize(2 << 20);
             m += b.byteLength;
         }
         m",
    )
    .unwrap();
    assert_eq!(v, Value::Number((200 * MIB) as f64));
}

#[test]
fn memory_usage_reports_array_buffers() {
    let mut vm = vm();
    vm.collect_garbage();
    let base = heap_bytes(&mut vm);
    eval(
        &mut vm,
        "globalThis.b = new ArrayBuffer(8 << 20); globalThis.s = new SharedArrayBuffer(8 << 20);",
    )
    .unwrap();
    vm.collect_garbage();
    let held = heap_bytes(&mut vm);
    assert!(held >= base + 16 * MIB, "{base} -> {held}");
    eval(&mut vm, "b.transfer(); b = null; s = null;").unwrap();
    vm.collect_garbage();
    let released = heap_bytes(&mut vm);
    assert!(released < base + MIB, "{base} -> {held} -> {released}");
}

// ---- Map, Set, WeakMap and WeakSet ------------------------------------------------------------

/// The probe from the review: a Map of 2,000,000 entries under 16 MiB.
#[test]
#[cfg_attr(miri, ignore = "two million entries are too slow under Miri")]
fn a_16_mib_limit_stops_a_map_of_two_million_entries() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "const m = new Map(); for (let i = 0; i < 2e6; i++) m.set(i, i); m.size",
    );
    out_of_memory(
        &mut vm,
        "const s = new Set(); for (let i = 0; i < 2e6; i++) s.add(i); s.size",
    );
}

/// Weak collections: a thousand keys in two thousand WeakMaps (or
/// WeakSets) is two million entries with only three thousand objects.
#[test]
#[cfg_attr(miri, ignore = "two million entries are too slow under Miri")]
fn weak_collection_entries_count() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "const keys = []; for (let i = 0; i < 1000; i++) keys.push({});
         const maps = [];
         for (let j = 0; j < 2000; j++) { const w = new WeakMap(); for (const k of keys) w.set(k, j); maps.push(w); }
         maps.length",
    );
    out_of_memory(
        &mut vm,
        "const keys = []; for (let i = 0; i < 1000; i++) keys.push({});
         const sets = [];
         for (let j = 0; j < 2000; j++) { const w = new WeakSet(); for (const k of keys) w.add(k); sets.push(w); }
         sets.length",
    );
}

/// docs/security-model.md quotes this size; on 64-bit targets the entry
/// slot is 40 bytes (the key and value and the slot's sequence number,
/// TASK-103.32) and the index entry 40, plus a control byte.
#[test]
#[cfg(target_pointer_width = "64")]
fn a_collection_entry_counts_81_bytes() {
    assert_eq!(agnijs_js_vm::builtins::collections::MAP_ENTRY_BYTES, 81);
}

/// String keys are copied into the key index, so their length counts.
#[test]
fn long_string_keys_count() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "const big = 'k'.repeat(1 << 20); const m = new Map();
         for (let i = 0; i < 64; i++) m.set(big + i, i);
         m.size",
    );
}

#[test]
fn memory_usage_reports_collection_entries_and_releases_them() {
    let mut vm = vm();
    vm.collect_garbage();
    let base = heap_bytes(&mut vm);
    eval(
        &mut vm,
        "globalThis.m = new Map(); for (let i = 0; i < 100000; i++) m.set(i, i);
         globalThis.s = new Set(); for (let i = 0; i < 100000; i++) s.add(i);",
    )
    .unwrap();
    vm.collect_garbage();
    let held = heap_bytes(&mut vm);
    // At least the two value slots of each entry.
    assert!(held >= base + 2 * 100_000 * 32, "{base} -> {held}");
    eval(&mut vm, "m = null; s = null;").unwrap();
    vm.collect_garbage();
    let released = heap_bytes(&mut vm);
    assert!(released < base + 100_000, "{base} -> {held} -> {released}");
}

/// Entries whose keys died leave a WeakMap, and the count with them, so
/// a WeakMap that sees many short-lived keys stays small.
#[test]
fn weak_entries_whose_keys_die_leave_the_count() {
    let mut vm = limited(16 * MIB);
    let v = eval(
        &mut vm,
        "const w = new WeakMap();
         for (let i = 0; i < 300000; i++) w.set({}, i);
         'done'",
    )
    .unwrap();
    assert_eq!(v, Value::string("done"));
}

// ---- built-ins that allocate a large block ----------------------------------------------------

#[test]
fn a_16_mib_limit_stops_a_one_gib_repeat_before_it_allocates() {
    let mut vm = limited(16 * MIB);
    out_of_memory(&mut vm, "'x'.repeat(2 ** 30).length");
    out_of_memory(&mut vm, "'\\u0100'.repeat(2 ** 29).length");
}

#[test]
fn pad_start_and_pad_end_check_the_limit() {
    let mut vm = limited(16 * MIB);
    out_of_memory(&mut vm, "''.padStart(2 ** 30, 'ab').length");
    out_of_memory(&mut vm, "'x'.padEnd(2 ** 30).length");
}

#[test]
fn join_checks_the_limit() {
    let mut vm = limited(16 * MIB);
    // The separators alone.
    out_of_memory(&mut vm, "new Array(1 << 24).join('abcdefgh').length");
    // The pieces: 4096 references to one 64 KiB string make 256 MiB.
    out_of_memory(
        &mut vm,
        "const a = new Array(4096).fill('x'.repeat(1 << 16)); a.join('').length",
    );
    out_of_memory(
        &mut vm,
        "const t = new Array(4096).fill('x'.repeat(1 << 16)); t.toString().length",
    );
}

#[test]
fn concatenation_checks_the_limit() {
    let mut vm = limited(16 * MIB);
    // Doubling builds ropes, which allocate nothing until they are read;
    // the limit counts the flat string a rope will become.
    out_of_memory(
        &mut vm,
        "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 10; i++) s = s + s; s.length",
    );
    out_of_memory(
        &mut vm,
        "let t = 'y'.repeat(1 << 20); for (let i = 0; i < 10; i++) t = `${t}${t}`; t.length",
    );
    out_of_memory(
        &mut vm,
        "let u = 'z'.repeat(1 << 20); for (let i = 0; i < 10; i++) u = u.concat(u); u.length",
    );
}

/// Under the limit, the same built-ins work as before: the check refuses
/// only what does not fit.
#[test]
fn work_under_the_limit_is_unaffected() {
    let mut vm = limited(16 * MIB);
    let v = eval(
        &mut vm,
        "let total = 0;
         for (let i = 0; i < 50; i++) {
             total += 'x'.repeat(1 << 20).length;
             total += ''.padEnd(1 << 20, 'ab').length;
             total += new Array(1 << 10).fill('y'.repeat(1 << 10)).join('').length;
             let s = 'z'.repeat(1 << 16); for (let j = 0; j < 4; j++) s = s + s; total += s.length;
             total += new ArrayBuffer(2 << 20).byteLength;
             const m = new Map(); for (let j = 0; j < 1000; j++) m.set(j, j); total += m.size;
         }
         total",
    )
    .unwrap();
    let per = MIB + MIB + MIB + (1 << 20) + 2 * MIB + 1000;
    assert_eq!(v, Value::Number((50 * per) as f64));
}

/// Without a limit, nothing is refused.
#[test]
fn no_limit_means_no_check() {
    let mut vm = vm();
    let v = eval(
        &mut vm,
        "new ArrayBuffer(64 << 20).byteLength + 'x'.repeat(32 << 20).length",
    )
    .unwrap();
    assert_eq!(v, Value::Number((96 * MIB) as f64));
}

/// The error is the memory-limit error, which JavaScript cannot catch.
#[test]
fn the_error_cannot_be_caught() {
    let mut vm = limited(16 * MIB);
    out_of_memory(
        &mut vm,
        "let caught = false; try { new ArrayBuffer(1 << 30) } catch (e) { caught = true } caught",
    );
}
