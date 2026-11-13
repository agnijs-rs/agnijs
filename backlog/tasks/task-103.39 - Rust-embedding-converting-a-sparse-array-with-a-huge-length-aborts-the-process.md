---
id: TASK-103.39
title: >-
  Rust embedding: converting a sparse array with a huge length aborts the
  process
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 304000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Untrusted script can abort the host process, which the memory limit does not prevent, through every Rust-embedding API that turns a JS value into Rust data.

Affected code: crates/agnijs/src/convert.rs, `walk` (about lines 85 to 93), used by `js_to_json` and `from_value`. Callers include `Context::eval::<T>`, `from_js`, `get_global`, `call`, and the argument decoding of every `register_fn` / `register_try_fn` / `register_async_fn` host function and `HostClass` method.

    let len = engine.get(realm, value, &"length".into())?;
    let len = len.as_number().unwrap_or(0.0) as u32;
    let mut items = Vec::with_capacity(len as usize);

A sparse array costs the engine almost nothing, but `length` reaches 2^32-1. Preallocating `length` slots of `serde_json::Value` (32 bytes each) asks for 137 GB before a single element is read.

Minimal repro:

    let mut ctx = agnijs::Context::new();
    ctx.register_fn("f", |_v: serde_json::Value| 0).unwrap();
    ctx.eval_value("var a = []; a.length = 4294967295; try { f(a) } catch (e) {}");
    // or: ctx.eval::<serde_json::Value>("var a = []; a.length = 4294967295; a");

Command: an integration test in crates/agnijs/tests, `cargo test -p agnijs --test <name>` (Windows, debug build).

Expected: a conversion error (`Error::Convert`, or a TypeError thrown into the script for a host-function argument). Or the conversion is bounded by the engine's memory limit.
Actual: `memory allocation of 137438953440 bytes failed`, and the process aborts (exit code 0xc0000409, STATUS_STACK_BUFFER_OVERRUN, the Windows abort status, on a clean debug build). `catch_unwind` cannot catch this, and the memory limit is never consulted. On a system that overcommits and grants the reservation, the loop that follows makes 2^32 `engine.get` calls from Rust with no interrupt or fuel check: an unbounded hang.

Root cause: a script-controlled length is trusted for a preallocation, and the element loop is a native loop with no limit.

Suggested fix: do not preallocate from `length`. Cap it with a sane conversion limit (refuse arrays longer than a fixed maximum, or longer than the remaining memory budget allows), and bound the total number of converted elements. Check the engine's interrupt or limit inside the loop, or document the cap. `Vec::with_capacity(len.min(SMALL))` alone fixes the abort but not the hang.

Test that should catch it: an embed test that passes `Object.assign([], { length: 2**32 - 1 })` to a registered function and to `eval::<serde_json::Value>`, and expects an error within a time bound.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Passing an array with length 2^32-1 to a registered host function gives a catchable error, not an abort
- [ ] #2 eval::<serde_json::Value> of such an array returns Error::Convert without allocating per the length
- [ ] #3 Conversion of a long array is bounded by a documented cap or the memory limit, and does not run unpolled for 2^32 steps
- [ ] #4 crates/agnijs/tests has a regression test for both paths
<!-- AC:END -->
