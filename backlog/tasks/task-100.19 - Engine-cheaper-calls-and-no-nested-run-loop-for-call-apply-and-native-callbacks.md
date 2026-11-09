---
id: TASK-100.19
title: >-
  Engine: cheaper calls and no nested run loop for call, apply and native
  callbacks
status: In Progress
assignee:
  - '@fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T13:46:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 253000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Calls are expensive. The `args_vec!` macro (`interp/step.rs`) copies the arguments into a new `Vec<Value>` for every Call, CallMethod and Construct; in rxjs-operators 107 of the 115 allocation samples under the interpreter come from it, and in calls-recursive the allocator is about 9%. `push_function_frame` is 7 to 13% inclusive across the Gable set (it clones the code Rc, builds a 160-byte Frame, resizes `regs` and copies parameters). A native that calls JavaScript goes `call_object` -> `run_function` -> `in_native` -> a fresh `Vm::run` loop: array-methods does 2.4 M such re-entries and method-calls 300 K (one per getter). `Function.prototype.call` (`proto_call`) is under 94 to 100% of the samples in every Gable benchmark because bundled code calls through it, so all of that code runs nested and under a native frame (which also blocks the collector, TASK-100.16).

Reproduction (ES module, 1 M iterations, Gable vs QuickJS-NG ms): `f(s)` 276 vs 63; `f.call(null, s)` 361 vs 96; `f.apply(null, [s])` 548 vs 197; `Math.abs(1)` 286 vs 80; `arr.forEach` 1 M callbacks 244 vs 45; getter `p.x` 351 vs 89.

Benchmarks to prove it: calls-recursive, method-calls, array-methods, closures (micro) and rxjs-operators, rxjs-subject-fanout, signal-churn (Gable).

Findings: docs/js-engine-performance-findings.md, finding 4. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A JavaScript-to-JavaScript call allocates nothing on the Rust heap in the common case (no per-call arguments Vec), checked by a test or counter
- [x] #2 Function.prototype.call and Function.prototype.apply with a JavaScript target run the target in the current run loop, without a native frame
- [x] #3 Array iteration methods and accessor calls reach JavaScript callbacks without starting a new Vm::run loop per callback, or the task notes measure why that is not worth it
- [ ] #4 calls-recursive and method-calls are at least 1.3x faster than at the start of the task
- [x] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Baseline with a bench.exe/test262.exe built from origin/main (6dfdceb), kept aside: calls-recursive 84.1 ms, method-calls 208.5, closures 155.6, array-methods 451.8 (7 iterations after 2 warmup); reproduction module via test262 eval: empty loop 26, f(s) 126, f.call 212, f.apply 394, Math.abs 113, forEach 164, getter 170 ms.
2. Red: tests/calls.rs in agnijs-js-vm (a test native reports Vm::native_depth: f.call/f.apply with a bytecode target run with no native frame; exceptions through call/apply/getters reach the caller's handler; stack overflow through f.call recursion and a self-reading getter is a catchable RangeError and the VM stays usable; generators, async functions and class constructors through call/apply; arguments object and rest parameters through call/apply and spread; re-entrancy from natives (forEach/sort/JSON reviver calling f.call); every scenario with and without GC stress), tests/call_allocs.rs in agnijs-js-bench (a counting global allocator: fib(20) minus fib(15) allocates nothing per call), a debugger test stepping into a function reached through f.call.
3. Green: arguments as a slice of the caller's register window (Args::Regs/Slice) through one frame-push path push_function_frame; call_from_frame/construct_from_frame take window indices; natives called from bytecode get their arguments from a stack buffer (no Vec); Function.prototype.call/apply tagged natives (NativeTag) unwrapped in call_from_frame so a bytecode target pushes its frame in the current loop (fallback to the native for error cases and non-array-like apply); one Rust entry into the run loop (run_entry) for natives, hosts, jobs, generators and eval; GetNamed/SetNamed slow paths push a bytecode getter or setter's frame instead of nesting a run loop.
4. Measure after, alternating binaries; profile what remains for array iteration methods and decide (notes) whether a non-nesting callback path is worth it.
5. cargo test --workspace, instr-count tests, debugger/cdp, gc-stress subset (tests/calls.rs and tests/gc.rs under AGNIJS_GC_STRESS=1); test262 agnijs, --precompile, --only-intl402, RegExp suites; Gable suites and benchmarks via the path-patched harness; record numbers; finalize; commit; push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J1, J8 apply. Make one function the entry into JavaScript for natives, host calls, jobs and call/apply (that is where tier selection will live), and one frame-push path for JS-to-JS calls; today five places call Vm::run. Pass arguments as a slice of the caller's register window. Every frame, however cheap, stays a Frame record in Vm::frames over a Vm::regs window (J1); do not introduce frames that exist only on the Rust stack. Faults stay returned values (J8).

## Result (commit 0ea173e)

**Design.**
- Arguments are an `Args` value: either a run of the caller's registers (`Regs { start, len }`) or a slice. Every JS-to-JS call goes through one frame-push path, `push_function_frame`. It copies the arguments straight into the parameter registers and keeps a list only when the code needs one (`keeps_arguments`).
- Natives called from bytecode get their arguments from an 8-value stack buffer (`with_arg_slice`).
- `Function.prototype.call` and `apply` carry a `NativeTag`. `call_from_frame` unwraps them and pushes a bytecode target's frame in the current loop. For `apply`, a dense array is copied into a reused `arg_scratch` buffer; any other array-like goes through CreateListFromArrayLike. Error cases and non-bytecode targets still take the native.
- A primitive `this` is wrapped in the callee's realm. This was caught by test262 built-ins/Function/call-bind-this-realm-value.js.
- GetNamed and SetNamed slow paths:
  - A bytecode getter or setter is pushed as a frame. `Frame::discard_result` keeps the assigned value in acc on the setter's return.
  - An ordinary GetNamed miss now reads the value in the same single lookup (`ordinary_get`).
  - SetNamed uses `set_prop_with(.., inline_setter)`, so the setter lookup is not done twice.
- `Vm::run_entry` is the one Rust entry into JavaScript: `run_function`, `run_script`, eval code, generator resumption and async functions. `Vm::run` is private.
- JIT-readiness:
  - J1 holds: every frame is a Frame in Vm::frames over a regs window, and no frame lives only on the Rust stack.
  - J8 holds: faults stay returned values, and there is one entry and one frame-push path.

**Tests.**
- `agnijs-js-vm/tests/calls.rs`: 12 scenarios, each run with and without GC stress. A test native reports native_depth. The scenarios cover:
  - call/apply/getter/setter with no native frame;
  - this coercion;
  - arguments and rest;
  - apply TypeError;
  - exceptions;
  - stack overflow as a catchable RangeError, with the VM usable afterwards;
  - generators/async/class constructors through call/apply;
  - re-entrancy from natives;
  - deep recursion through call;
  - getter redefinition after a cache miss;
  - Set semantics on the setter path.
- `agnijs-js-bench/tests/call_allocs.rs`: a counting global allocator shows 0 Rust heap allocations per call for fib, plain, method, closure and native calls, f.call, f.apply and a class getter.
- `tests/debugger.rs`: stepping into call/apply targets and a getter.

**Micro-benchmarks** (bench.exe, 7 iterations, 2 warmup; origin/main 6dfdceb vs 0ea173e-equivalent build, run alternately twice; median ms base1/base2 -> new1/new2):

| workload | base | new | speedup |
|---|---|---|---|
| calls-recursive | 86.6 / 85.2 | 58.4 / 57.8 | 1.47x |
| method-calls | 218.7 / 220.3 | 216.1 / 214.3 | 1.02x |
| array-methods | 471.3 / 473.1 | 349.2 / 348.0 | 1.35x |
| closures | 176.6 / 177.3 | 158.0 / 165.3 | 1.12x |
| map-set | 188.9 / 190.9 | 161.9 / 162.5 | 1.17x |
| prop-polymorphic | 314.3 / 313.9 | 284.2 / 285.3 | 1.10x |
| prop-write-alloc | 267.4 / 267.9 | 270.9 / 272.0 (recheck after the SetNamed fix) | 0.98x |
| compile-source | 22.4 / 21.8 | 22.9 / 22.7 | 0.96x |

- All other workloads were within 0.98x to 1.06x.
- compile-source rechecked at 21.3 to 21.9 vs 21.7 to 22.2 ms, which is noise.
- An intermediate build regressed prop-write-alloc by 5 to 7%: the setter check walked the prototype chain a second time on every SetNamed miss. Folding it into set_prop fixed that.

**Reproduction** (1 M iterations in a function; best of 5; release; base -> new, ms):

| case | base | new |
|---|---|---|
| f(s) | 141 | 100 |
| f.call(null, s) | 224 | 114 |
| f.apply(null, args) | 286 | 137 to 191 (noisy) |
| Math.abs | 132 | 99 |
| arr.forEach, 1 M callbacks | 159 | 115 |
| map, 1 M callbacks | 180 | 137 |
| getter p.d | 195 | 175 |

The empty loop was 12 to 14 ms in both builds.

**Gable benchmarks** (run-bench.mjs, 5 iterations, 1 warmup; path-patched harness, base built from origin/main, alternated twice; median ms):

| benchmark | base | new | speedup |
|---|---|---|---|
| rxjs-operators | 314.6 / 310.9 | 232.6 / 218.4 | 1.42x |
| rxjs-higher-order | 489.5 / 490.8 | 425.9 / 420.1 | 1.17x |
| rxjs-subject-fanout | 227.1 / 227.0 | 177.3 / 178.4 | 1.28x |
| signal-churn | 618.0 / 604.9 | 495.2 / 499.2 | 1.22x |
| signal-create | 146.9 / 147.5 | 125.0 / 125.4 | 1.17x |
| template-mount | 112.7 / 113.4 | 113.0 / 104.2 | 1.08x |
| template-update | 66.7 / 65.9 | 58.5 / 58.3 | 1.13x |
| list-diff | 1175.5 / 1174.8 | 1096.3 / 1096.1 | 1.07x |
| ui-bundle-startup | 149.7 / 150.1 | 147.3 / 147.5 | 1.02x |

**AC3, array iteration callbacks.** forEach, map, sort and other natives still call a callback through `call_value -> run_function -> run_entry`, which is one nested `Vm::run` per callback. Measured, that nesting is not where the time goes:
- 1 M forEach callbacks take 115 ms.
- A plain bytecode loop making the same 1 M calls (`for (...) cb(arr[j], j, arr)`) takes 225 ms, because its GetKeyed and `arr.length` miss the inline cache (TASK-100.20).
- The callback itself costs about the same as a JS-to-JS call: f(s) is 100 ms per 1 M.
- array-methods is already 1.35x faster, because nested entries no longer allocate argument Vecs.

A non-nesting path would have to turn each array method into a resumable bytecode or native state machine. That is a large change, and its gain is bounded by the run-loop entry cost, which is a few ns per callback. It is not worth doing before the IC work. Accessor calls from GetNamed and SetNamed no longer nest (see the tests).

**AC4, method-calls.** This bar is missed: 1.02x. After the change, the profile shows calls are not the dominant cost:
- push_function_frame, call_from_frame and do_return/pop_frame together are about 20% inclusive.
- name_key / Heap::atom re-interning on every inline-cache miss is about 9% (TASK-100.20 AC1).
- Monomorphic inline-cache misses: the `area` and `n` sites alternate between Shape and Square, and `shapes.length` / `shapes[i]` are array reads (TASK-100.20).
- `super.area()` loads its key with LdaConst, which allocates a string every time (alloc_str_utf16; TASK-100.21).

Removing every call cost would still be short of 1.3x. calls-recursive meets the bar (1.47x).

**Test results.**
- cargo test --workspace passes, including debugger and cdp.
- instr-count tests pass.
- tests/calls.rs and tests/gc.rs pass under AGNIJS_GC_STRESS=1.
- test262:

| run | passed |
|---|---|
| agnijs | 42225/42225 |
| --precompile | 42225/42225 |
| --only-intl402 | 981/981 |
| RegExp, String regexp methods, annexB RegExp, regexp literals, under backtracking-only and under linear-when-eligible | 2488/2488 each |
| built-ins/RegExp under auto | 1879/1879 |

Auto is also covered by the full runs.

- Gable suites via the path-patched harness: 758 passed, with the 2 known timing failures (yjs perf, highlight linearity). The harness manifest and lockfile were restored afterwards.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Calls no longer allocate and call, apply and accessors run without a nested run loop (commit 0ea173e).

- Arguments are passed as a slice of the caller's register window through the one frame-push path.
- Function.prototype.call and apply with a bytecode target are unwrapped into a frame push in the current loop. So are bytecode getters and setters.
- Vm::run_entry is the one Rust entry into JavaScript.

Results:
- calls-recursive is 1.47x faster.
- array-methods is 1.35x faster.
- rxjs-operators is 1.42x faster, and the other Gable benchmarks are 1.02x to 1.28x faster.
- Nothing regresses by more than 5%.
- test262 passes 42225/42225 in both the plain and precompiled runs, and intl402 passes 981/981.
- The Gable suites pass: 758, plus the 2 known timing failures.

AC4 is not met for method-calls (1.02x). Its time is in inline-cache misses, name re-interning and string constant allocation (TASK-100.20, TASK-100.21), not in calls. The notes have the profile. The task is left In Progress for the maintainer to decide on AC4.
<!-- SECTION:FINAL_SUMMARY:END -->
