# 9. The heap is a set of index arenas collected by precise mark-sweep at interpreter safe points

Date: 2026-11-08

## Status

Accepted

## Context

The spec (`docs/js-engine-spec.md`, section 5) asks for a precise, non-moving, single-threaded collector, scoped rooting for the embedder, no collection during a Gable frame, and `unsafe` confined to the heap and GC modules with Miri and sanitizer jobs in CI. The usual Rust designs are `Rc`/`Gc` smart pointers with tracing (Boa's `gc` crate), raw pointers behind a safe facade, or index-based arenas (Nova). Each trades allocator speed, collector precision, API ergonomics and the amount of `unsafe` differently.

## Decision

- **Index arenas, one per heap type** (`heap.rs`: objects, strings, symbols, BigInts, environments, shapes). A `Value` is a small `Copy` enum: primitives inline, heap values as `u32` arena indices. Every arena is a `Vec<Option<T>>` with a free list and a mark bit per slot. Access through a dead slot panics instead of aliasing, so a stale id is caught as an engine bug.
- **No `unsafe` in the heap, the collector or anywhere else in the VM.** The spec permits `unsafe` in these modules; we found we did not need it, and the invariants that would have justified it (ids valid only while reachable from a root, and only until the next collection) hold without it because the arenas are plain vectors. The only `unsafe` in the workspace is in the QuickJS adapter's FFI.
- **Precise mark-sweep at safe points only** (`gc.rs`). Roots are the frames and the shared register file, the scoped root stack used by built-ins, the host handle table, realms, interned names and well-known symbols, the symbol registry, the job queue and module records. Interned strings are weak. Shapes are traced rather than rooted (TASK-100.22): a live object marks its shape, a shape its parent and the keys it adds, and the inline caches of every block a live function or frame runs mark the shapes they name; the sweep frees the rest and reuses their ids, and clears the caches of untraced blocks before their next use. Marking runs over mark bits moved out of the arenas, records weak containers as it reaches them (so weak processing scans no arena), and the sweep hands dead objects' and scopes' small buffers to later allocations and totals the live size as it goes. Collection runs from `Vm::maybe_collect` between instructions and only when no native frame is on the Rust stack, so Rust locals in built-ins never observe a collection. Allocation never collects; it accumulates `allocated_since_gc` and the next safe point decides.
- **Pacing.** The threshold is twice the live size after a collection, with an 8 MiB floor; the host can request a collection (`gc_requested`) and set a memory limit that turns into an out-of-memory fault at the next safe point.
- **Stress mode.** `AGNIJS_GC_STRESS=1` collects at every safe point. It is the mode the GC CI job runs the unit tests and a test262 slice under; it exists to find missing roots, which are the only class of GC bug this design admits.
- **Incremental slices and host-scheduled collection** (spec section 5) are deferred: the arena design does not preclude them (mark bits are per slot, the mark stack is explicit), but they are not needed until the engine runs inside Gable's frame loop (M9).

## Consequences

- Heap values are not pointers, so there is an index-and-bounds check on every access; the object model offsets this with shapes and, in M3, inline caches. If profiling in M9 shows the arenas as a cost, the switch to a pointer-based heap is confined to `heap.rs`, `value.rs` and the tracer.
- Non-moving: fragmentation is handled by the free lists, not compaction. The spec's "leave room for a generational or moving collector" is satisfied by hiding slot identity behind the `*Id` types.
- Rust code that holds an id across an allocation is correct as long as no safe point intervenes; a built-in that calls back into JavaScript must root its ids on the scoped root stack. Miri exercises this discipline in CI together with the stress mode.
- `WeakRef`, `FinalizationRegistry` and module-record collection (M4, M5) plug into the sweep phase as weak tables; the finalization callbacks are queued as `Job::Call` host tasks, never run during the sweep.

## Addendum (TASK-100.21, 2026-11-09): inline slots, function and string constant reuse

An object, a function and a string constant each cost fewer heap blocks (performance findings, finding 6). The arenas and the collector are unchanged; what lives in an arena slot changes as follows.

- **The first four named slots are inline.** `ObjectData` is `#[repr(C)]`: the header (shape id, slot count, extensible and watched flags, prototype, dictionary pointer, class) is followed by `INLINE_SLOTS` (4) contiguous `PropSlot`s at a 16-byte-aligned offset, then the elements and an overflow `Vec` for slot indices 4 and up, taken from the slot pool when first needed. Slot `i` is inline exactly when `i < 4`. An unused or removed inline slot holds `undefined` data, so the tracer, which walks only the slots in use, and a stale reader never see a dead value. `PropSlot` is 16 bytes (an accessor's halves are `OptObj`, a `u32` object id or none), `Class` is 24 bytes (the two iterator-record variants are boxed) and sparse elements are boxed, so the object stays at 160 bytes, the `OBJECT_BYTES` the pacing already counted, with no slots block for an object of up to four properties.
- **A function is two blocks: the object and its `FunctionData` box.** The eager properties (`length`, `name`, for sloppy functions `caller` and `arguments`, then `prototype`) go straight into a precomputed, rooted shape (`Vm::fn_shapes`), and a constructor's prototype object gets its `constructor` the same way, so creating a function does no transition lookup and no key interning. Property order and attributes are as before. The sweep keeps dead functions' `FunctionData` boxes (up to the number of objects that fit under the threshold) for new functions; the pool is emptied when the next sweep starts, so a pooled box keeps its old code block alive until the next collection at most.
- **String constants and function names survive collections with their block.** The tracer marks each string a traced block's feedback materialised since the previous collection (its constants and the function's `name`) and restamps it for the new epoch, so a loop loads the same string after a collection instead of allocating it again. A block that was not traced loses its strings with everything else, and its stale epochs make the next load materialise them again.
