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
