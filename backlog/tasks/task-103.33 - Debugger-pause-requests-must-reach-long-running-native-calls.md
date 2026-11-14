---
id: TASK-103.33
title: Debugger pause requests must reach long-running native calls
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T20:07:00Z'
updated_date: '2026-11-14T09:42:00Z'
labels:
  - enhancement
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 297000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Left over from TASK-103.28. Interrupts now reach regex matches and native built-in loops, but a CDP/debugger pause request is only taken at the next statement, so it lands after a long native call returns. Make pause polling share the native-loop poll points so Debugger.pause stops a long native call partway, or document why it cannot.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Red test: a pause request during a long native call (join over 3,000,000 elements, pathological regex) is taken within 2 s
- [x] #2 No benchmark regression beyond 1%; test262 0 regressions in all modes
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests (tests/debugger_native_pause.rs): a pause requested from the host interrupt hook during a 3,000,000-element join and a pathological regex must be taken at that poll, inside the call, within 2 s; a regex that finishes gives the same result when paused; a pause requested from another thread reaches a running regex.
2. Vm::ask_host_interrupt (the 1-in-4096 slow path of poll_interrupt, and JSON.parse's) takes a pending pause request when a native frame is innermost (native_depth > run_depth) and a JS frame exists; the interpreter keeps taking requests at its own instructions.
3. RegExp exec: the matcher's poll also stops on a pending pause request; the VM then pauses (no input borrowed) and reruns the match from its start (matching is side-effect free).
4. Gates: debugger and CDP tests, vm tests, test262 all modes, benchmarks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned (commit 'VM: a debugger pause request reaches a long native call'). Vm::ask_host_interrupt, after the host check, calls pause_in_native: it takes the request when a client is attached, no pause is in progress, native_depth > run_depth and a script frame exists, and pauses with the script frames as they are (the call's line). The regex poll checks the host first, then a pending request; after with_input returns, the VM pauses and reruns the match. tests/debugger_native_pause.rs: the deterministic tests request the pause from the 2nd host poll and see it at that poll (asked = 2, globalThis.done unset); the cross-thread test sees it well within 2 s. All 4 red before (the pause came after the call, or never for the regex). Debugger tests (21), agnijs-js-cdp tests, full vm suite and clippy on changed lines clean. test262 all 8 modes 0 regressions; Miri on agnijs-js-jit exec/calls tests 9/9; benchmarks: see TASK-103.32 notes (geomean 0.9997; regex workloads 0.99 to 1.02). Not covered: String searches (SearchPoll) poll host.interrupt() directly, so a pause during indexOf over a huge string comes after it returns; documented in docs/security-model.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Debugger.pause now reaches long native calls: the native loops' host poll (once per 4096 iterations) and the regex matchers' poll take a pending pause request when a native frame is innermost, so a join over 3,000,000 elements pauses partway and a pathological regex pauses, then reruns from its start when resumed (matches are side-effect free). Tests in tests/debugger_native_pause.rs, including a cross-thread request within 2 s. test262 0 regressions in all modes, benchmark geomean within 1%. String searches still pause only after they return (documented).
<!-- SECTION:FINAL_SUMMARY:END -->
