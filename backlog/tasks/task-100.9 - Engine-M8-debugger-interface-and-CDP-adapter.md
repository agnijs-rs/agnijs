---
id: TASK-100.9
title: 'Engine M8: debugger interface and CDP adapter'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-07T22:37:00Z'
updated_date: '2026-11-09T01:25:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.8
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 210000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Gable tooling needs breakpoints and stepping in the owned engine; the adopted engines have none. Spec section 9.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Breakpoints, stepping, frame and scope inspection, async stacks and script source work through the debugger interface
- [x] #2 A separate CDP adapter crate supports console, source-mapped exceptions, breakpoints, stepping and evaluate, verified with a CDP client test
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Test seams (chosen without user review, recorded here):
- Seam A, the debugger interface: gable_js_debugger::{Debuggee, DebugClient} as implemented by gable_js_vm::Vm, driven by a scripted test client in gable-js-vm/tests/debugger.rs. Catches breakpoints, stepping, frames, scopes, async stacks, script source, evaluate on a frame. Misses the wire protocol.
- Seam B, the CDP adapter over its real transport: gable-js-cdp/tests/cdp_client.rs connects a WebSocket CDP client over TCP to the adapter's server (HTTP /json/list then WebSocket upgrade) and drives Runtime/Debugger/console methods. Catches protocol shape, transport framing, pause message loop, source-mapped exception text. Misses DevTools UI quirks (checked manually with Node's built-in WebSocket client).
- Seam C, source maps: gable_js_debugger::SourceMap decode/lookup against worked-example mappings.

Implementation:
1. Compiler: per-CodeBlock debug info (statement-start pcs, register locals with pc ranges); serialise it; bump bytecode tag to gable-bc-3 (ADR 0010 addendum).
2. gable-js-debugger: engine-neutral interface (ScriptId, Location, BreakpointSpec, StepAction, PauseReason, FrameInfo, ScopeInfo, PropertyInfo, ValueDescription, AsyncStackTrace, Debuggee: Engine, DebugClient), source-map decoder.
3. gable-js-vm: Option<Box<DebugState>> on Vm; one extra flag in the dispatch-loop slow-path condition; script registry, breakpoints keyed by (code, pc), stepping by frame depth at statement starts, pause on debugger statement/exceptions/requests, frame and scope inspection, evaluate on frame via a temporary environment holding register locals, async stack capture at reaction registration when enabled, console and uncaught-exception notifications.
4. gable-js-cdp: transport-agnostic session (Runtime, Debugger, console, exceptions, source-mapped stack text, scriptParsed with sourceMapURL), WebSocket+HTTP server with handwritten RFC 6455 framing (no new deps), nested pause loop.
5. ADR 0017 for the debugger interface; verify cargo test --workspace --locked, boundary tests, test262 baselines (plain, --precompile, --only-intl402) and dispatch-loop perf with no debugger.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementation notes (M8):
- **Compiler.** Statement starts are recorded as (pc, statement source offset), so pauses report the statement's own column. Register locals are recorded with their pc ranges. Both are serialised in gable-bc-3 as LEB128 deltas and string-table indices, and decoded and verified out of line.
- **Measuring the bytecode change.** gable-js-bench (merged from TASK-100.13) first showed bytecode-load 17 to 19% slower. About half came from the compiler inlining the new decode into get_block. After packing, it is 4 to 12% slower (about 0.25 ms on 900 functions) with a 5% larger payload. That is recorded in the ADR 0010 addendum, and lazy decode is noted there as the next step.
- **VM.** Debug state is Option<Box<DebugState>>, and debug_hot is ORed into the existing dispatch-loop slow-path condition. Other hooks (the debugger statement, handle_fault, run_job, async_await, console, script registration) check the flag or whether a client is attached first.
- **Script registry.** Vm::scripts keeps the latest code per script name so a client that attaches late sees them. eval and Function bodies are registered only while a client is attached.
- **Async stacks.** They are captured at perform_then when depth > 0, with the description "await" or "Promise.then", capped at 8 segments. run_job sets the current async parent while a reaction runs.
- **Uncaught exceptions.** An exception counts as uncaught when no frame on the stack has a handler for it, so async-function rejections pause as uncaught.
- **CDP adapter.**
  - Session sits over a Transport trait. CdpServer serves /json/list and /json/version and accepts one WebSocket client at a time. RFC 6455 framing, SHA-1 and base64 are written by hand, with no new dependencies.
  - Debugger.pause is also routed through PauseRequester from the socket thread.
  - exceptionThrown's description is source-mapped. Its stackTrace stays in generated positions.
  - Breakpoints are by URL or script id. urlRegex is rejected and no breakpointResolved event is sent.
- **Verification.**
  - cargo test --workspace --locked passes, including the boundary tests, 14 debugger tests, 2 CDP client tests, 5 source-map tests and 3 WebSocket vector tests.
  - Clippy is clean on the new code.
  - test262 (release build): gable 42225/42225, --precompile 42225/42225, --only-intl402 981/981.
  - With no debugger attached: an interpreter micro-benchmark gave 1.152 s against 1.160 s for base, test262 wall time is within noise, and gable-js-bench run against a base build back to back shows every workload within noise except bytecode-load (above).
  - A manual run with Node 24's built-in WebSocket client against a host binary worked: discovery, a URL breakpoint inside an async function with an "await" async stack, evaluateOnCallFrame, stepOver, console and Runtime.evaluate.
- **ADRs.** New ADR 0017 (debugger capability and CDP crate). ADR 0010 gets an addendum for gable-bc-3.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added the engine's debugger interface and a separate CDP adapter, with no measurable cost to the dispatch loop when no debugger is attached.

**What changed**
- **gable-js-debugger.** An engine-neutral Debuggee/DebugClient capability next to the Engine trait. It covers:
  - script listing and source
  - URL and script-id breakpoints, with conditions
  - stepping into, over and out
  - pausing on exceptions and on request
  - frames, scopes and evaluate on a frame
  - value descriptions and properties
  - async stacks
  - a V3 source-map decoder
- **gable-js-vm.**
  - make_debuggee.
  - A single debug_hot flag on the existing slow path.
  - Statement-start offsets and register locals in the debug info. The bytecode tag moves to gable-bc-3 (packed), and gable-bc-2 payloads are now rejected as a format mismatch.
  - Async stacks captured when a reaction is registered.
  - A script registry so clients that attach late still see scripts.
- **gable-js-cdp.**
  - A Runtime/Debugger session over a Transport trait, covering console, source-mapped exception text, breakpoints, stepping, evaluate and callFunctionOn.
  - CdpServer, a TCP endpoint with the /json routes and a handwritten WebSocket implementation. It adds no dependencies.
- **ADRs.** New ADR 0017, and an addendum to ADR 0010.

**Verification**
- Workspace tests pass, including the boundary tests, the debugger-interface tests and the WebSocket CDP client tests.
- test262 is unchanged: 42225/42225 from source, 42225/42225 precompiled, and 981/981 for intl402.
- Interpreter and test262 timings are unchanged.
- bytecode-load is 4 to 12% slower because of the extra debug info.

**Follow-ups and limits**
- Not supported yet: urlRegex breakpoints, the breakpointResolved event, and stepping over across await.
- The uncaught heuristic treats async-function rejections as uncaught.
- Profiler domains reply as no-ops.
- Lazily decoding the bytecode debug info would remove the bytecode-load cost.
<!-- SECTION:FINAL_SUMMARY:END -->
