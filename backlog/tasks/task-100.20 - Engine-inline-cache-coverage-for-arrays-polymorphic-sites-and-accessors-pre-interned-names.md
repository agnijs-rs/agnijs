---
id: TASK-100.20
title: >-
  Engine: inline-cache coverage for arrays, polymorphic sites and accessors;
  pre-interned names
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T14:33:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 254000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The inline cache works for plain own data properties but most hot reads miss it. Counters for one iteration: prop-monomorphic 2.0 M hits and 1.0 M misses (all `arr.length`); prop-polymorphic 0 hits, 3.0 M misses (2.0 M shape changes, 1.0 M arrays); loop-array-index 1.5 M misses, all arrays; method-calls 0.75 M hits, 0.90 M misses (0.30 M shape, 0.30 M accessor, 0.30 M array). `interp/ic.rs` is monomorphic and `is_plain` excludes arrays, strings, arguments, typed arrays, namespaces, proxies and dictionary objects. Every miss calls `name_key`, which re-interns the property name (hash plus intern lookup plus a clone of `code.names[i]`): 2 to 5% inclusive across the Gable set, 12 to 14% as Heap::atom in prop-polymorphic. GetKeyed has no dense-array fast path (it goes through to_property_key, get_value, get_prop, exotic_of and own_property).

Reproduction (ES module, 1 M reads, Gable vs QuickJS-NG ms): one shape `.a` 261 vs 57; four shapes 301 vs 57; `arr.length` 217 vs 42; a class getter 351 vs 89.

Benchmarks to prove it: prop-monomorphic, prop-polymorphic, loop-array-index, method-calls (micro; the worst ratios at 4.9x to 6.9x) and signal-churn, rxjs-operators (Gable).

Findings: docs/js-engine-performance-findings.md, finding 5. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Property names are interned once per CodeBlock (at compile or load time), so an inline-cache miss does no string hashing for the name
- [x] #2 Inline caches handle up to four shapes per site, array length, and methods found on the prototype of arrays and strings, with invalidation tests for shape, prototype and property attribute changes
- [x] #3 Accessor properties found through the cache call the getter or setter without the generic lookup
- [x] #4 Integer-index reads and writes of dense arrays take a fast path in GetKeyed and SetKeyed
- [x] #5 prop-polymorphic and loop-array-index are at least 1.5x faster than at the start of the task
- [x] #6 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #7 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Baseline with bench.exe, test262.exe and gable-harness.exe built from origin/main a1a361c, kept aside. Measure the reproduction (test262 eval, 1 M accesses in a function) and the micro and Gable benchmarks.
2. Red: extend tests/ic.rs with one scenario per cache path and its invalidation: four shapes, megamorphic, two-class methods, depth-two holders, proxies on the chain, getters and setters (including throwing and re-entrant ones), array length and methods, string length and methods, per-realm string prototypes, keyed reads and writes over holes and attributes, string constants across collections. Each runs with and without GC stress. Add tests/ic_stats.rs (feature `ic-stats`) to pin an all-hit steady state on each targeted pattern.
3. Green:
   - per-CodeBlock lazily allocated `Feedback` (IC sites, name keys, string constants) owned by the first VM to run the block;
   - four plain-data `IcEntry` per site with explicit empty/mono/poly/megamorphic states and a kind field;
   - prototype entries validated by `Heap::proto_epoch` plus watched objects;
   - accessor entries call the getter or setter directly;
   - array and string length and string-prototype entries;
   - pinned pre-interned name keys and atomised string constants;
   - dense-element fast paths for GetKeyed and SetKeyed in the dispatch loop.
4. Measure after, alternating binaries. Run cargo test --workspace, instr_count, regexp without default features, and the gc-stress subset (ic, calls, gc). Run test262 plain, --precompile, intl402, the RegExp suites under both policies and the GC-stress slice. Run the Gable suites and benchmarks via the path-patched harness, then restore it.
5. Write the ADR 0012 addendum, record the numbers, finalize, commit and push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J4, J5 apply. Keep feedback in the per-CodeBlock side table as plain data (shape ids, holder id plus shape, slot, kind) a JIT can read at fixed offsets without calling the VM. Make entry states explicit: empty, monomorphic, polymorphic (up to 4), megamorphic, with megamorphic sticky. Add a kind field (own data, proto data, accessor, array length, array/string proto method). Shapes must stay never-freed while an entry names them (J5). Pre-interned names are fine; they are not serialised (load empty, as today).

## Result (commit 1d5da2e)

**Design** (ADR 0012 addendum).

Feedback storage:
- `CodeBlock::feedback` is a `OnceCell<Feedback>`, allocated by the first VM to run the block. That VM owns it (`Feedback::owner`); another VM running the same block takes the generic path, so no id is read against the wrong heap.
- The compiler records only `ic_count`. Feedback is never serialised, so decoded blocks start empty.

Site entries:
- Each site holds 4 `IcEntry` values: plain `#[repr(C)]` data with kind, flags, shape, proto, holder, slot and epoch.
- States are empty, monomorphic, polymorphic and megamorphic. Megamorphic is sticky.
- Kinds: own data, own accessor, prototype data and prototype accessor at any depth up to 16, array length, string length, and string-prototype data.
- `length` entries are flagged so they never apply to Array or String receivers.

Prototype validity:
- Filling a prototype entry marks the chain up to the holder as watched.
- `Heap::proto_epoch` is bumped when a watched object adds, replaces or removes a named property, goes to dictionary mode, changes its prototype, or is collected. Every prototype write now goes through `Heap::set_proto`.
- A prototype hit checks the receiver shape, its proto id and the epoch, then reads the holder slot live and checks it is still the recorded kind.

Names and constants (AC1):
- The encoded key of each NameIdx is cached in the block's feedback the first time the block uses it. The atom is pinned (`Heap::pinned`, a GC root), so every later miss does no hashing.
- This happens lazily per block rather than at compile time, because the compiler has no heap.
- String constants of up to 32 units are atoms cached per collection epoch. `super.m` and `o['k']` key loads neither hash nor allocate.

Accessors (AC3): a cached getter or setter is called directly. An inlinable bytecode callee is pushed as a frame (a setter's frame has `discard_result`), so there is no generic lookup and no nested run loop.

Keyed access (AC4): GetKeyed and SetKeyed have a dispatch-loop fast path for:
- an ordinary or Array receiver;
- a Number key that is an integral array index;
- a present, non-hole dense element.

Dense elements are always writable data; elements with other attributes are sparse. Strings, arguments objects, typed arrays, namespaces and proxies take the generic path.

**Tests.**
- `tests/ic.rs`: 19 scenarios, each with and without GC stress, covering invalidation by shape change, prototype change, redefinition, freeze, deletion, dictionary mode and collection. The scenarios cover:
  - four shapes and megamorphic sites;
  - methods of two classes;
  - depth-two holders;
  - proxies on the chain;
  - getters and setters, including ones that throw or re-enter;
  - array and string length and methods;
  - per-realm string prototypes;
  - keyed reads and writes over holes, sparse elements, frozen arrays and getters on indices;
  - string constants across collections.
- `tests/ic_stats.rs` (feature `ic-stats`): four-shapes, array, string and the method-calls pattern all reach 0 misses in the steady state. The method-calls pattern is 651 get hits, 200 set hits and 100 keyed hits per call of f.
- `agnijs-js-compiler/tests/serialize.rs`: feedback is absent after decode, and a block claimed by one VM gives no feedback to another.

**Reproduction** (1 M accesses in a function, test262 eval, release, base a1a361c -> new, ms):

| case | base | new |
|---|---|---|
| empty loop | 34 | 34 |
| one shape `.a` | 87 | 68 to 72 |
| four shapes `.a` | 152 | 81 to 83 |
| `arr.length` | 102 | 45 |
| class getter | 233 | 158 to 162 |
| `str.length` | 134 | 69 |
| `str.charCodeAt(0)` | 212 | 115 |
| `arr[i]` | 76 | 59 |
| `arr[i] = v` | 97 | 41 |

**Micro-benchmarks** (bench.exe, 7 iterations, 2 warmup; base and new alternated, 2 runs each; best median, ms):

| workload | base | new | speedup |
|---|---|---|---|
| loop-arith | 108.1 | 107.1 | 1.01x |
| loop-array-index | 194.1 | 96.5 | 2.01x |
| prop-monomorphic | 151.0 | 84.9 | 1.78x |
| prop-polymorphic | 283.1 | 134.2 | 2.11x |
| prop-write-alloc | 261.7 | 210.0 | 1.25x |
| string-concat | 126.7 | 76.0 | 1.67x |
| string-methods | 60.1 | 58.0 | 1.04x |
| array-methods | 345.7 | 318.6 | 1.09x |
| map-set | 156.7 | 134.7 | 1.16x |
| closures | 156.0 | 154.5 | 1.01x |
| calls-recursive | 58.1 | 56.4 | 1.03x |
| method-calls | 212.8 | 123.9 | 1.72x |
| regex | 366.7 | 350.6 | 1.05x |
| regex-tokenize | 64.8 | 52.5 | 1.23x |
| regex-long-input | 7.6 | 7.6 | 1.00x |
| json-stringify | 176.8 | 175.3 | 1.01x |
| json-parse | 137.7 | 138.6 | 0.99x |
| promise-chain | 78.1 | 79.1 | 0.99x |
| async-await | 86.4 | 86.1 | 1.00x |
| compile-source | 21.9 | 21.5 | 1.01x |
| bytecode-load | 2.2 | 2.2 | 1.01x |
| engine-startup | 0.66 | 0.69 | 0.96x |

regex-redos (13 µs) first read 0.93x and engine-startup 0.96x. Both were rechecked with 31 iterations, alternated 3 times:

| workload | base runs | new runs |
|---|---|---|
| regex-redos (µs) | 12.9, 12.8, 12.8 | 12.8, 12.7, 12.6 |
| engine-startup (µs) | 705, 705, 674 | 676, 706, 701 |

That is noise; there is no regression.

**Gable benchmarks** (run-bench.mjs, 10 iterations, 2 warmup; path-patched harness, base built from a1a361c, alternated twice; best median, ms; checksums equal):

| benchmark | base | new | speedup |
|---|---|---|---|
| rxjs-operators | 219.0 | 177.8 | 1.23x |
| rxjs-higher-order | 422.9 | 367.4 | 1.15x |
| rxjs-subject-fanout | 177.4 | 142.8 | 1.24x |
| signal-churn | 495.4 | 481.2 | 1.03x |
| signal-create | 123.8 | 117.5 | 1.05x |
| template-mount | 103.6 | 86.4 | 1.20x |
| template-update | 58.0 | 47.8 | 1.21x |
| list-diff | 1095.8 | 944.5 | 1.16x |
| ui-bundle-startup | 147.0 | 144.5 | 1.02x |

**Test results.**
- cargo test --workspace passes (48 suites, including debugger and cdp).
- The instr_count tests pass, and so does the VM regexp test with no default features.
- Under AGNIJS_GC_STRESS=1, tests/ic.rs (19), tests/calls.rs (12) and tests/gc.rs (9) pass.
- test262:

| run | passed |
|---|---|
| agnijs | 42225/42225 |
| --precompile | 42225/42225 |
| --only-intl402 | 981/981 |
| RegExp suites, backtracking-only | 2505/2505 |
| RegExp suites, linear-when-eligible | 2505/2505 |
| GC-stress language slice (AGNIJS_GC_STRESS=16) | 5949/5949 |

- Gable suites via the path-patched harness: 759 passed, with 1 known timing failure (yjs perf). The harness manifest, lockfile and exe were restored afterwards.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The inline cache now covers the accesses that missed it (commit 1d5da2e).

What changed:
- Sites are polymorphic (4 entries, sticky megamorphic).
- Prototype entries work at any depth and are validated by a global epoch over watched objects.
- Getters and setters found through the cache are called directly.
- Array length, string length and string-prototype methods are cached.
- Names are interned once per CodeBlock and pinned, and string constants are cached atoms.
- GetKeyed and SetKeyed have a dense-element fast path.
- Feedback is one lazily allocated block per CodeBlock, owned by the first VM to run it and never serialised. The design is in the ADR 0012 addendum.

Results:
- prop-polymorphic is 2.11x faster and loop-array-index 2.01x.
- prop-monomorphic is 1.78x faster and method-calls 1.72x.
- Gable benchmarks are 1.02x to 1.24x faster.
- Nothing regresses by more than 5%.
- test262 passes 42225/42225 in both the plain and precompiled runs, intl402 passes 981/981, and the RegExp suites pass under both matcher policies.
- The Gable suites pass: 759, plus the 1 known timing failure.
- Each cache path has tests, including invalidation, with and without GC stress, plus `ic-stats` hit-rate tests.

Risk: correctness of prototype entries depends on every structural change to a watched object bumping `Heap::proto_epoch`. Any new code that writes `ObjectData::proto` directly, or edits a named property table outside `add/replace/remove_own_property` and `make_dictionary`, must go through `Heap::set_proto` or `Heap::note_structure_change`.
<!-- SECTION:FINAL_SUMMARY:END -->
