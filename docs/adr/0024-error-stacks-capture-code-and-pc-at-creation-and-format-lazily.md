# 24. Error stacks capture code and instruction index at creation, format on first read in the V8 format, and map through host-supplied source maps

Date: 2026-11-11

## Status

Accepted

## Context

TASK-103.17 gives errors a `stack`. Code written for Node and browsers reads it, parses it (test runners, loggers, error reporters) and sometimes rewrites it through `Error.prepareStackTrace`. Bundled or transpiled code is only useful to debug when positions are reported in the original source, so the embedder must be able to hand the engine a source map along with the generated code.

Three constraints shape the design:

- The hot loop must not pay. Most errors are thrown and caught without anyone reading `stack` (control flow, feature tests), and code that never creates an error must cost nothing at all.
- A source map is host data that can be large, stale or hostile. It must never panic the engine or the C API (ADR 0021).
- The format must be the one the ecosystem parses: V8's.

## Decision

**At error creation the engine records each frame as a code block reference and an instruction index; it computes positions and text only when `stack` is first read, and caches the text.**

- Every path that creates an Error object (the constructors, `Vm::make_error` for runtime errors, `SuppressedError` from disposal) attaches an `ErrorStack` to the object's `Class::Error` slot. The slot is `None` for errors created with `Error.stackTraceLimit` not a Number, and for snapshot-restored errors whose stack was never read.
- A frame is `(code, pc, this, callee, flags)`: no strings, no positions. Positions come from the code block's position table at format time, with a small cache of line starts per source.
- Code that creates no error does nothing new: there is no per-call bookkeeping, and no shadow stack.

**The text follows V8.**

- The header is `name: message`, computed as `Error.prototype.toString` would without running user code.
- A frame line is `    at <what> (<file>:<line>:<column>)`, or `    at <file>:<line>:<column>` for top-level script, module and eval code and anonymous functions. Lines are 1-based and columns count UTF-16 units, 1-based.
- `<what>` is `new F` for constructor calls, `async f` for async frames, `Type.method` when the receiver is an object whose constructor name is found without side effects, `Type.<anonymous>` for anonymous methods, else the function's name.
- Property reads and method calls are positioned at the property name, as V8 reports them. This is a compiler position change only; the bytecode format is unchanged.
- Natives have no frames in this engine, so V8's `at Array.map (<anonymous>)` lines are absent.

**The API surface follows V8 and the error-stack-accessor proposal.**

- `Error.prototype.stack` is an accessor pair, as in the proposal (test262 feature `error-stack-accessor`, 35/35). Assigning `stack` defines an own data property on the receiver.
- `Error.stackTraceLimit` (default 10, clamped to 1000) bounds the frames; a non-Number captures nothing.
- `Error.captureStackTrace(obj, fn)` captures the current stack, dropping frames down to and including the topmost call of `fn`. On an error without an own `stack` it replaces the captured trace; on any other object it defines a non-enumerable own `stack` data property.
- `Error.prepareStackTrace(err, callSites)` is called at first read, when present, with V8-style CallSite objects (`getFunctionName`, `getFileName`, `getLineNumber`, `getColumnNumber`, `isToplevel`, `isConstructor`, `isAsync`, `toString` and the rest). Its result is the stack. A re-entrancy guard formats nested reads in the default format. CallSite objects cannot be snapshotted.

**Async frames are read off the promise graph at capture, not tracked while code runs.**

- When the innermost frame belongs to an async function or async generator, the capture follows its result promise to the reactions awaiting it. A reaction that resumes an `await` names the awaiting generator, whose saved frame becomes an `async` frame, and the walk continues from there.
- Only `await` continues the chain, as in V8. `then` callbacks do not; the debugger's async stacks (ADR 0017) cover those.

**Source maps come from the host, keyed by script name, and are decoded lazily.**

- `Engine::set_source_map(script, Some(map))` registers a Source Map v3 for a script name, and `None` removes it. A module loader can return `ModuleSource::SourceWithMap { source, source_map }` to register one with the module. The C API has `agnijs_set_source_map` and `agnijs_sink_set_source_map` (ABI 1.2).
- The map text is stored as given. It is decoded at the first format that needs it; a map that fails to decode is ignored, and positions stay generated.
- Mapping applies to `Error.stack`, call sites, the host's `Exception::stack` and the debugger. The debugger gets the host map in `ScriptInfo::source_map`, where it takes precedence over a `sourceMappingURL` comment. CDP sends it to clients as a base64 `data:` URL in `scriptParsed`.
- The decoder is bounded: 64 MiB of text, 2^20 lines, 2^22 segments, and every VLQ value and running sum in the 32-bit range. Anything past those limits is an error, not a panic. A deterministic fuzz test covers it.

**The host's `Exception::stack` is resolved only when a throw escapes to the host.** During unwinding the engine records `(code, pc)` pairs, at most 64, and turns them into positions in `fault_to_error`. Mapping is applied at the same point.

## Consequences

- Creating an error costs one allocation and one `Rc` clone per frame (up to `stackTraceLimit`). Reading the stack costs a position lookup per frame once. Code that creates no errors is unaffected; the JS benchmark geomean is unchanged.
- A captured error keeps its frames' code blocks, `this` values and callees alive until the stack is formatted. The frames are dropped after formatting.
- The JIT shares the interpreter's frames (ADR 0019), so positions resolve the same through compiled code.
- A snapshot drops captured, unformatted traces: a restored error reads `stack` as `undefined` unless the stack was read (formatted and cached in the slot) or assigned as an own property before the snapshot. The cached value is written with the error (snapshot layout `agnijs-snap-2`). CallSite objects refuse to snapshot.
- Hosts that run transpiled code get original positions without the engine fetching anything. The engine never reads a `sourceMappingURL` comment for `Error.stack`, so it reads no files and makes no network calls. Only the CDP adapter follows comments, through its own loader.
