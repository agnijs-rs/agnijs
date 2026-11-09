# 17. The debugger is a capability beside the engine interface, and CDP lives in its own crate

Date: 2026-11-08

## Status

Accepted

## Context

The spec (section 9) asks for a debugger interface inside the engine that covers source positions per bytecode offset, breakpoints, stepping, frame and scope inspection, async stacks and script source. It also asks for a separate adapter to the Chrome DevTools Protocol (CDP), so that existing tools such as VS Code and Chrome DevTools can connect. The adapter's first version covers console, source-mapped exceptions, breakpoints, stepping and `evaluate`.

Three constraints apply:

- The engine interface (ADR 0006) is implemented by more than one engine.
- The workspace must stay embedder-neutral (ADR 0016).
- The dispatch loop is the hottest code in the engine, and an engine with no debugger attached must not get slower.

## Decision

**The debugger interface is a separate crate and trait.** `agnijs-js-debugger` defines `Debuggee` and `DebugClient`, which depend only on `agnijs-engine-api`. `Debuggee` is not a supertrait or an optional method set on `Engine`. An engine that can be debugged hands out a `Debuggee`, which also exposes its `Engine`, and `agnijs-js-vm::make_debuggee` builds one. Engines that cannot be debugged, such as the QuickJS-NG adapter, carry none of this.

The crate also holds the engine-neutral parts that every protocol adapter needs:

- value descriptions
- property listings
- a V3 source-map decoder
- `sourceMappingURL` discovery

**Pauses are synchronous calls into the client.** When the engine pauses, it calls `DebugClient::paused` on the engine thread with the VM stopped and its frames intact. The client can inspect frames, scopes and values, evaluate code in a frame, change breakpoints, and then return a `ResumeAction` (continue, step into, step over or step out). The engine needs no paused state machine and no event queue, and inspection works on live frames rather than copies. A `PauseRequester`, an atomic flag that can be sent between threads, lets another thread interrupt running code. The flag is checked at the next statement start.

**The cost when nothing is attached is a single flag.** The dispatch loop already has a slow path for interrupts and memory limits. The debugger adds one boolean, `debug_hot`, to that path's condition. The boolean is set exactly while a client is attached, and the slow path then checks for breakpoints, steps and pause requests. Other hook points also check `debug_hot`, or whether a client is attached, before doing any work:

- the `debugger` statement
- faults
- promise reactions
- `console`
- script registration

An interpreter micro-benchmark and the test262 wall time show no measurable change with no debugger attached.

**Positions come from statement starts in the bytecode's debug info.** Each block lists its statement starts as (instruction index, source offset) pairs (ADR 0010 addendum, `agnijs-bc-3`).

- Breakpoints resolve to the first statement start at or after the requested position, anywhere in the script's code tree, and are keyed by (code block, instruction).
- Stepping is decided at statement starts by comparing frame depth: into stops at the next statement anywhere, over stops at the same depth or shallower, and out stops only shallower.
- Lines and columns are reported in UTF-16 units, as the CDP expects.

**Async stacks are captured when a reaction is registered.** When `await` or `then` registers a promise reaction while a client wants async stacks, the current frames are captured as a segment labelled "await" or "Promise.then". The segment is linked to the async parent of the code that is running, and chains are capped at 8 segments. While the reaction runs, that chain is the current async parent. With async stack depth 0, nothing is captured.

**Scripts are kept by name so that late clients see them.** The VM keeps the latest compiled script or module for each name, whether it came from source or from bytecode. A client that attaches after code has run therefore still gets those scripts and their sources. `eval` and `Function` bodies are reported only while a client is attached. Keeping scripts uses memory, because a top-level code tree that would otherwise be garbage stays reachable until another script takes its name. It matches what DevTools users expect and is bounded by the number of distinct script names the host loads.

**The CDP adapter is its own crate: `agnijs-js-cdp`.** It depends on `agnijs-engine-api`, `agnijs-js-debugger` and `serde_json`, and not on the VM. It has two layers:

- **`Session`** maps CDP's `Runtime` and `Debugger` domains onto a `Debuggee` over a `Transport` trait. It does this with remote object ids, scope objects, `scriptParsed` replay, `consoleAPICalled`, and `exceptionThrown`. The description of `exceptionThrown` rewrites stack frames to their original positions through the script's source map, and its `stackTrace` stays in generated positions, which is what DevTools maps itself. A host supplies a loader for maps that are not inline `data:` URLs.
- **`CdpServer`** is a `Transport` over TCP. It serves the `/json/list` and `/json/version` discovery routes and accepts one WebSocket client at a time. The WebSocket framing, SHA-1 and base64 are written by hand, and their tests use the RFC 6455, FIPS 180-4 and RFC 4648 vectors. Socket threads pass messages to the engine thread through a channel. `Debugger.pause` also triggers the `PauseRequester` directly, so it can stop a script that is busy and not reading messages.

## Considered options

- **Adding debugger methods to `Engine`.** Every engine would have to implement them or stub them out, and the interface would grow a large surface that most embedders never use.
- **An asynchronous, message-based pause.** The engine would serialise its state at the pause and resume when it receives a message. That needs a re-entrant state machine in the VM, and inspection would see copies. The synchronous call keeps the VM simple, and the adapter's `serve`/`poll` loop provides the message pump.
- **A per-instruction hook, or a debug build of the dispatch loop.** Instruction-level stepping is not something CDP clients need. A second build of the loop doubles the hottest code. The single flag on the existing slow path costs nothing measurable.
- **A WebSocket crate such as `tungstenite`.** It would bring a dependency tree of TLS-capable and HTTP crates into the engine workspace. The debugging endpoint needs only one client on localhost and unfragmented server frames, which takes a few hundred lines with known test vectors.

## Consequences

- **What the first version leaves out.**
  - Breakpoints can be set by URL or script id, but not by `urlRegex`.
  - No `breakpointResolved` event is sent for scripts that load later. A URL breakpoint is still re-resolved and hit in those scripts.
  - There is no step-over across `await`: stepping over an `await` continues to the next statement that runs on this stack.
  - Profiler and heap profiler domains reply as no-ops, and unknown methods return the standard `-32601` error.
- **An approximation for "uncaught".** An exception counts as uncaught when no frame on the current stack has a handler for it. A rejection inside an async function therefore pauses as uncaught even if a caller further up the async chain would catch it. CDP clients show it under "uncaught", the same as V8 does when promise-rejection tracking is off.
- **`Runtime.evaluate` runs as a script named `""`.** It is reported to `scriptParsed` with an empty URL, as V8 reports console evaluations, so URL breakpoints never match it. Evaluations in a frame see register locals through a temporary environment, and the values are written back afterwards, so assignments made from the console take effect.
- **Embedders own the wiring.** An embedder decides when to listen, which address to bind, how to load source maps, and whether to wait for a client before running code (`Session::wait_until_ready`). None of that policy is in the engine.
