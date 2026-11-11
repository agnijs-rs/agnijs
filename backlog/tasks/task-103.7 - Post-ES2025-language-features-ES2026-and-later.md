---
id: TASK-103.7
title: Post-ES2025 language features (ES2026 and later)
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-11T07:18:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 242000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implement the post-ES2025 features the engine currently excludes (see native/engine/crates/gable-js-test262/post-es2025-features.txt): Array.fromAsync, Error.isError, Math.sumPrecise, Uint8Array base64 and hex, JSON.parse source text access, iterator sequencing, Atomics.pause, upsert (Map getOrInsert), iterator chunking, iterator includes, joint iteration, and explicit resource management (using, DisposableStack). Temporal is separate. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each listed feature passes its test262 tests at the pinned commit, is removed from post-es2025-features.txt, and the gable and --precompile baselines show no regressions
- [x] #2 Features still at Stage 3 are skipped with a recorded reason; the list is reviewed when the test262 pin moves
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Self-contained built-ins first, test262 as the oracle: Error.isError, Math.sumPrecise, upsert, iterator sequencing, chunking, includes, joint iteration, Array.fromAsync, Uint8Array base64/hex (Atomics.pause already passed).
2. JSON.parse source text access: parse records when a reviver is given, JSON.rawJSON / isRawJSON, stringify of raw JSON objects.
3. Explicit resource management built-ins: DisposableStack, AsyncDisposableStack, SuppressedError, Symbol.dispose / asyncDispose, the iterator prototype dispose methods.
4. `using` / `await using` syntax: parser early errors, a finally-style disposal wrapper in the compiler, new dispose instructions in the VM, bytecode format bump (ADR 0010).
5. Remove each feature from post-es2025-features.txt as it passes; record the Stage 3 note (AC #2).
6. Gates per commit: test262 default, --precompile and --snapshot, intl402; jit and no-intl builds; agnijs-js-bench geomean within 1%.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): split by difficulty. Sonnet: Error.isError, Atomics.pause, Array.fromAsync, upsert (Map getOrInsert), iterator sequencing, iterator chunking, iterator includes, joint iteration, Uint8Array base64 and hex (self-contained built-ins, test262 is the oracle). Opus: Math.sumPrecise (exact-arithmetic algorithm), JSON.parse source text access and explicit resource management (they touch the parser or compiler).

Implemented in three commits on main: 954f705 (ten built-ins), 6a0fa8a (JSON.parse source text access), 451171f (explicit resource management).

Design:
- Math.sumPrecise sums exactly (ExactSum accumulator) and rounds once.
- Iterator.zip / zipKeyed and Iterator.concat mark the helper as executing while they close their iterators, so a re-entrant `return` throws as the spec requires.
- JSON.parse with a reviver builds JSON Parse Records (source text of each primitive; element and entry records). The reviver's context gets `source` only while the value at that position is still the parsed one. JSON.rawJSON objects are a payload-free Class::RawJSON (frozen, null prototype), which stringify emits verbatim. Snapshots do not carry them.
- One dispose capability type, Class::DisposableStack, serves DisposableStack, AsyncDisposableStack and the internal stack of a `using` scope. disposeAsync is driven natively.
- A statement list that declares `using` (block, function, module, static block, the classic-for head, each for-of iteration) compiles under a finally-style wrapper that reuses the try/finally completion dispatch, now factored out. Disposal uses six new instructions (DisposeStackNew, DisposeAdd, DisposeBegin, DisposeStep, DisposeError, DisposeEnd). They write only acc, so the JIT treats them as generic instructions. They check that the capability register holds a capability and throw a TypeError otherwise, so they never panic.
- Bytecode format moves to agnijs-bc-8 (ADR 0010 addendum; the tests are updated).
- A module whose top level has `await using` or `for await` is marked as using top-level await.

Verification:
- test262 (agnijs, at the pin): 42231 before, 42741 after the built-ins, 42762 after JSON source text, 43192 after explicit resource management. All pass with 0 regressions in the default, --precompile and --snapshot modes. intl402: 981/981.
- Staging explicit-resource-management tests (outside the gate): 53/53.
- The jit, no-intl (linear-regex) and C API builds check clean. The parser, compiler and VM test suites pass.

Bench: agnijs-js-bench (agnijs, 7 iterations), d2e504a against 451171f, geomean new/old 0.983 (0.984 without regex-long-input). json-parse ran 6% slower in the full run but within noise on reruns (base 43.9 and 48.3 ms, new 45.4 and 44.6 ms).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
All twelve post-ES2025 features in the ticket are implemented. post-es2025-features.txt has no ES2026 entry left: only Temporal, canonical-tz and Intl.DurationFormat remain.

What changed:
- Ten built-ins: Error.isError, Math.sumPrecise, upsert, iterator sequencing, chunking, includes, joint iteration, Array.fromAsync and Uint8Array base64/hex. Atomics.pause already passed.
- JSON.parse source text access.
- Explicit resource management, both the built-ins and the using / await using syntax. The syntax adds six bytecode instructions and moves the format to agnijs-bc-8 (ADR 0010 addendum).

AC #2: none of these features is Stage 3 at the pin; all are in the Standard section of features.txt. The runner skips Stage 3 features (the Proposed section) as proposed. The header of post-es2025-features.txt records this and asks for a review of both lists when the pin moves.

Verification:
- test262: 42231 to 43192 passing, with 0 regressions in the default, --precompile and --snapshot modes.
- intl402: 981/981.
- jit, no-intl and C API builds are clean, and the crate tests pass.
- Bench geomean 0.983 (new/old), within the 1% gate.

Commits: 954f705, 6a0fa8a, 451171f.
<!-- SECTION:FINAL_SUMMARY:END -->
