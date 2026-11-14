---
id: TASK-103.35
title: >-
  JIT: a panic in Runtime::fast or Runtime::global is swallowed instead of
  resumed by enter
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:38:00Z'
updated_date: '2026-11-14T04:46:00Z'
labels:
  - bug
  - jit
dependencies: []
references:
  - docs/jit-review-2026-11.md
  - crates/agnijs-js-jit/src/exec.rs
parent_task_id: TASK-103
priority: low
type: bug
ordinal: 300000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the JIT soundness review of 2026-11 (`docs/jit-review-2026-11.md`). Severity: low. Confirmed with a Rust test against a toy runtime (below); not reachable from JavaScript unless `JitRt::fast` or `JitRt::global` has a bug that panics.

## What goes wrong

`exec.rs` promises, in its module docs, that "no panic crosses machine code: each trampoline catches it, parks the payload in the `Shared`, and returns `EXIT_PANIC`; `enter` resumes the unwind once machine code has returned". `fast_trampoline` and `global_trampoline` do not do that. They run `Runtime::fast` / `Runtime::global` under `catch_unwind` and match only `Ok(Some(v))`; an `Err(payload)` falls through to `call_out(..., rt.slow(pc, acc))`. The payload is dropped, the instruction is run again by the generic path, and compiled code continues as if nothing had happened.

So a panic in the VM's fast paths for `LdaGlobal`, `LdaEnv`, `StaEnv` or `LdaCallee` (an index out of range, a failed `expect`) is silently swallowed under the JIT, where the interpreter would unwind to the host. The engine then keeps running on whatever state the panic left half-changed, and the bug that caused it is hidden. It also contradicts TASK-103.31, which is about defining what an engine is after a panic: here the host never learns there was one.

## Reproduction (commit fea680f)

A test beside `crates/agnijs-js-jit/tests/enter.rs`, with that file's `Val` type and a runtime whose `fast` panics and whose `slow` answers `Resume::Next { pc: pc + 1, acc: Val::Str(99) }`:

```rust
#[test]
fn a_panic_in_fast_leaves_enter_as_a_panic() {
    let script = compile_script("function f() { return someGlobal; }", "t.js").unwrap();
    let code = script.functions[0].clone();
    let compiled = Compiled::<Val>::compile(&code).expect("compiles");
    let mut rt = toy(&code, /* panic_in_fast */ true, /* panic_in_slow */ false);
    let r = catch_unwind(AssertUnwindSafe(|| compiled.enter(&mut rt, 0, Val::Undefined)));
    assert!(r.is_err(), "the panic in Runtime::fast was swallowed");
}
```

Command: `cargo test --locked -p agnijs-js-jit --test <file>`.

- Expected: `enter` resumes the panic, as it does for a panic in `slow` (the sibling test with `panic_in_slow` passes).
- Actual: `enter` returns `Exit::Return` with `acc == Str(99)`; `fast` was called once and `slow` once.

## Suggested direction

Give the two trampolines the same three-way treatment `call_out` has: `Ok(Some(v))` is the hit, `Ok(None)` goes to the generic path, `Err(payload)` is parked with `park_panic` and answered with `EXIT_PANIC`.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A panic in Runtime::fast or Runtime::global leaves Compiled::enter as that panic, as one in Runtime::slow does
- [x] #2 Tests against a toy runtime cover a panic in fast and in global, beside the existing ones in the JIT crate
- [x] #3 The exec.rs module docs and the two trampolines agree
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests against toy runtimes: a panic in Runtime::fast (tests/enter.rs, beside a sibling for slow) and in Runtime::global (tests/calls.rs, a toy that describes a frame stack so that code is compiled with global sites).
2. exec.rs: one helper for the two trampolines with call_out's three-way treatment: a hit continues, a miss goes to the generic path, a panic is parked and answered with EXIT_PANIC for enter to resume.
3. Check every other trampoline against the module docs' promise and make the docs say what the code does (arith has no context to park in).
4. Review status column.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first: `a_panic_in_fast_leaves_enter_as_a_panic` (tests/enter.rs) and `a_panic_in_global_leaves_enter_as_a_panic` (tests/calls.rs) both got `Ok(Return)`; the sibling for `slow` was green. The global test needs code compiled with global sites, so tests/calls.rs adds a toy runtime that describes a frame stack, function objects and a callee table (`CallLayout`, `HeapLayout`), which TASK-103.36 uses too.

Fix: `exec::hit_or_out`, one helper for `fast_trampoline` and `global_trampoline`, with `call_out`'s three answers: a hit continues at pc + 1, a miss calls out, a panic is parked (`park_panic`) and answered with EXIT_PANIC. Every other trampoline was read against the module docs: slow, call, returned, poll, deopt and left park the payload. `arith_trampoline` does not and cannot: it takes two numbers and no context. It answers NaN for a panic, which is the same class of swallowing; `Runtime::arith` is pure and documented not to panic, so it was left as it is and the module docs now name it as the one exception. Gates: with TASK-103.34's (JIT crate tests, clippy, Miri, VM suite plain and forced).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A panic in `Runtime::fast` or `Runtime::global` now leaves `Compiled::enter` as that panic. Both trampolines go through `exec::hit_or_out`, which parks the payload and answers EXIT_PANIC as the generic path does; before, the payload was dropped and the instruction run again.

Verified by toy-runtime tests, red before the fix: a panic in fast (tests/enter.rs) and in global (tests/calls.rs), each also checking the instruction is not run again. The exec.rs module docs list which trampolines park a panic and say why arith's does not (no context, pure, documented not to panic; it answers NaN). That one is still a swallowed panic by design and was not changed.
<!-- SECTION:FINAL_SUMMARY:END -->
