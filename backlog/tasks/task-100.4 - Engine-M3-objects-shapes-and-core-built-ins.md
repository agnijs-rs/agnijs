---
id: TASK-100.4
title: 'Engine M3: objects, shapes and core built-ins'
status: Done
assignee: []
created_date: '2026-11-07T22:36:00Z'
updated_date: '2026-11-08T16:38:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.3
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 205000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Property access speed and the bulk of the standard library. Spec sections 3 and 4.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shapes with inline caches in the interpreter; dual one-byte/two-byte strings with ropes and interned atoms
- [x] #2 Object, Function, Array, String, Number, Boolean, Symbol, Math, JSON, Error and the reflection built-ins pass their test262 directories or are excluded with reasons
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Inline caches (AC #1): `GetNamed` / `SetNamed` carry a `u16` index into a per-code-block side table (`CodeBlock::ic`, `Cell<IcEntry>`) that the compiler sizes and the VM fills. Entries are monomorphic and shape-based: an own data slot, or a data slot on the receiver's direct prototype (method calls), re-validated by receiver shape, holder identity and holder shape. Dictionary-mode and exotic receivers (arrays, strings, typed arrays, arguments, proxies, namespaces) and accessors take the generic path; writes hit only own writable data slots. Soundness argument in `crates/gable-js-vm/src/interp/ic.rs` and ADR 0012; `crates/gable-js-vm/tests/ic.rs` runs every scenario with and without GC stress. The dual one-byte/two-byte strings, ropes and interned atoms required by AC #1 were delivered in M2 (`string.rs`, `Heap::intern`); M3 added a rooted single-character string cache (`Heap::char_str`) so `s[i]`, `charAt` and single-argument `fromCharCode` do not allocate for Latin-1 characters.

Built-ins (AC #2): added ES2025 iterator helpers (`Iterator` constructor, `Iterator.from` with `%WrapForValidIteratorPrototype%`, lazy map/filter/take/drop/flatMap on `%IteratorHelperPrototype%`, eager reduce/toArray/forEach/some/every/find) in `builtins/iterator_helpers.rs`. Fixes found by the test262 directories: `String.prototype.{match,matchAll,search,replace,replaceAll,split}` only consult `@@match`-style methods on object arguments; `Promise.try` resolves through `PromiseResolve(C, value)`; `Promise.race` closes the iterator only for non-iterator faults; `ArraySetLength` re-reads length and writability after coercions; class constructors called without `new` throw in the callee realm and derived-constructor return checks in the caller realm; `Function.prototype.toString` of methods covers the MethodDefinition without `static`, and of class constructors the whole class; the parser records member spans after `static`.

test262 (pinned suite, ES2025 cut, `--engine gable`): 34702 run, 34702 pass, 0 fail with the committed exclusions; `baselines/gable.json` now records no failures, so CI fails on any regression. Of the 11160 tests in the M3 built-in directories (Object, Function, Array, String, Number, Boolean, Symbol, Math, JSON, Error, NativeErrors, AggregateError, Reflect, Iterator, Promise, BigInt, generator/async prototypes and the global functions) 10709 pass and 451 are excluded with per-test reasons, every one of which needs an M5 built-in: Date (190), the RegExp matcher and prototype (255), typed arrays/ArrayBuffer/DataView (184) or Map/Set/Weak* (45) across all directories. Directories whole milestones deliver (RegExp, Date, Map, Set, WeakMap, WeakSet, WeakRef, FinalizationRegistry, ArrayBuffer, SharedArrayBuffer, TypedArray*, DataView, Atomics, Proxy) are excluded as directories for M5. `test/language` is 22932/22932 with 144 M5-dependent tests deferred; `test/annexB` 958/958 with 67 deferred; `test/harness` 103/116 with 13 deferred. The exclusions file was regenerated from a run with only the permanent rules, so earlier deferred lines whose tests now pass were dropped.

Performance notes (not an M3 gate, see M9): 1M iterations of a local arithmetic loop ~0.3s net, array index ~100ns, IC property get/set ~50ns, function call ~200ns, string concat ~100ns, string index ~140ns after the character cache. Top-level `var` access goes through global-object lookups and is 2–3x slower than locals; a global-binding cache is a candidate for M9.

Known gaps carried forward: 3 engine-interface conformance tests still need M5 built-ins (detach ArrayBuffer, host manipulation of Map, WeakRef in the module-record collection check); only the three M5-dependent exclusions groups above remain in the M3 directories.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
M3 delivered: monomorphic shape-based inline caches on GetNamed/SetNamed (ADR 0012, tests under GC stress), a rooted single-character string cache, ES2025 iterator helpers, and spec fixes across String, Promise, Array, class constructors and Function.prototype.toString. test262: 34702/34702 pass with the committed exclusions; the 451 excluded tests in the M3 built-in directories all need Date, RegExp, typed arrays or collections (M5, TASK-100.6) and are listed per test with that reason. The baseline records zero failures.
<!-- SECTION:FINAL_SUMMARY:END -->
