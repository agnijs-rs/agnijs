# AgniJS engine: specification

The spec for the work of building the **AgniJS engine** (the Gable engine until TASK-100.12 renamed it): an embeddable JavaScript engine written in Rust. It began inside Gable, whose **Host runtime** is its first embedder, and this spec consolidates the decisions made in Gable's design map (TASK-99): architecture (TASK-99.6), engine prior art (TASK-99.10) and engine design (TASK-99.11), plus the four implementation-shape decisions made when this spec was written. Gable's rationale for owning an engine is ADR 0004 in [the Gable repository](https://github.com/jcannon98188/gable/tree/main/docs/adr). Terms (Host runtime, Engine interface, AgniJS engine) are defined in `GLOSSARY.md`.

The implementation tasks are children of the Backlog task "Gable JS engine" and follow the milestones at the end of this document.

## 1. Goals and non-goals

Goals:

- Run full ES2025 TypeScript output and third-party code (RxJS and the like) correctly: the conformance gate is test262.
- Pure Rust, no JIT, one build on Windows, macOS, Linux and Android. No hardened-runtime JIT entitlement on macOS and no JIT restrictions on Android.
- Be an embeddable engine whose host-object, garbage-collection, module and debugging hooks fit Gable (retained native node tree, module re-loading for HMR).
- Replace the adopted interim engine (QuickJS-NG first, Boa as the alternative) when the switch-over rule in section 11 is met.

Non-goals for v1: a JIT; multi-threaded sharing of heaps; Workers; Stage 3 proposals; a DOM or any browser global (the engine never provides one); a profiler and heap profiler (later).

The maintainer cannot test on macOS. Everything is pure Rust with no platform-specific code in the engine, and macOS is verified by CI runners. Never add platform-specific engine code without a macOS path.

## 2. Language target

- Full ES2025 (ECMA-262 16th edition) plus Annex B, except `IsHTMLDDA`.
- `eval` and `new Function` are supported, with a host switch (`HostEnsureCanCompileStrings`) that can turn them off.
- Later editions and Stage 4 features arrive behind feature flags. Stage 3 proposals are out.
- **Intl**: the engine implements ECMA-402 (`DateTimeFormat`, `NumberFormat`, `PluralRules`, `Collator`, `Segmenter`, `ListFormat`, `RelativeTimeFormat`, `DisplayNames`, `Locale`) and the `toLocale*` methods, on ICU4X behind a seam (ICU4X is shared with Parley, the text stack). Locale data is trimmed to the app's locales. Gable core uses `DateTimeFormat`, `NumberFormat` and `PluralRules`; those come first.
- **Threads, `SharedArrayBuffer` and `Atomics`**: an engine instance is single-threaded with its own heap. test262's multi-agent tests need several agents sharing a buffer, so they are listed in the exclusions file with that reason. Multi-agent support (shared backing stores) is a later milestone. `SharedArrayBuffer` and `Atomics` single-agent behaviour is implemented.

## 3. Execution model

- Handwritten parser producing an AST; early errors implemented in the parser or a following pass.
- Single-pass compiler from AST to register-based bytecode with an accumulator (Ignition and Hermes style): fixed operand widths, compact instructions.
- Scope resolution at compile time into slots (closures capture slot environments); `with` and sloppy direct `eval` fall back to dynamic lookup.
- Object model: shapes (hidden classes, as in Boa, V8 and JSC) with inline caches in the interpreter.
- No JIT in v1; leave room in the bytecode and object model for a later tier. Section 14 lists what that room means (ADR 0019).
- Bytecode is serializable, so production bundles can ship precompiled code. The format is versioned and rejected on mismatch.
- Generators and async functions are implemented as resumable frames in the interpreter, not as threads or stack copies.

## 4. Strings

Dual representation: one-byte (Latin-1) and two-byte (UTF-16 code units) strings, as in V8, JSC and QuickJS. JS strings can contain lone surrogates, so WTF-8 is rejected (it makes `length` and indexing expensive). Concatenation builds ropes that flatten on demand. Property keys are interned atoms. Symbols and BigInts are separate heap types.

## 5. Garbage collection

- Precise, non-moving mark-sweep on a single thread with size-class allocation.
- Embedder values are rooted through a scoped root stack: host code holds values only inside a root scope or through an explicit persistent handle.
- Incremental slices, scheduled by the host between its own tasks (for Gable, between flushes) so collection does not interrupt a frame.
- `WeakRef` and `FinalizationRegistry` are supported; finalization callbacks run as host tasks, never during collection.
- Module records are heap objects and are collected when unreachable (needed for HMR).
- The heap interface hides the collector's strategy so a generational or moving collector can follow.
- Host classes register trace hooks so the collector sees references held by host objects.
- `unsafe` is allowed only in the heap and GC modules. Every use carries a comment stating the invariant it relies on. CI runs the test suite under Miri where it is practical and under address and undefined-behaviour sanitizers.

## 6. Regular expressions

ECMAScript regex semantics (backtracking, lookbehind, named groups, the `v` flag, Unicode property escapes, Annex B legacy syntax) differ from Rust's `regex` crate. The engine's regex support lives in its own crate behind a seam.

The first task of that crate is an evaluation of `regress` (the ES-compliant Rust regex crate Boa uses) against the relevant test262 tests (`built-ins/RegExp`, `language/literals/regexp`, `annexB`) and the performance of pathological patterns. If it passes, adopt it behind the seam; otherwise write our own engine. The decision is recorded as a short ADR.

## 7. Engine interface

The boundary every embedder codes against (Gable's Host runtime was the first). It is satisfied by the AgniJS engine and by the adopted interim engine (a QuickJS-NG adapter first). It lives in its own crate, with a conformance test kit that both adapters must pass.

Required capabilities:

- Evaluate scripts and modules. A module loader hook resolves static imports from the app bundle and dynamic `import()`.
- A host-driven job queue: the engine enqueues promise jobs and the host runs them after every host task (for example, a Gable flush ends with a microtask checkpoint).
- A promise-rejection tracker (`HostPromiseRejectionTracker`) routed to the host.
- Host-supplied timers: the engine has no event loop and no clock-driven callbacks of its own.
- Interrupt and memory limits.
- Realm creation.
- Bytecode load and save.
- Host classes with trace hooks, and host functions callable from JS.
- Host-side handles (such as a retained native node tree's nodes) can be plain integers, never GC references, so that state lives in Rust and JS holds integer handles.
- A hook for `import.meta` (HMR needs `import.meta.hot` provided by the host).
- Module re-loading under a new specifier, so a changed module can be evaluated again while the old record is collected once unreachable. The engine has no hot-update primitive: HMR logic lives in JavaScript.
- Source positions per bytecode offset and script source retrieval, for the debugger and for source-mapped exceptions.

The Rust API shapes (traits, handle types, error types) are the implementing agent's to design in milestone M0, recorded as an ADR, and reviewed against both the AgniJS engine and the QuickJS-NG adapter before M2 starts.

## 8. Host globals

Provided by the host layer, never the engine:

- The WinterCG baseline: `console`, `setTimeout`, `setInterval`, `queueMicrotask`, `URL`, `URLSearchParams`, `fetch`, `AbortController`, `AbortSignal`, `TextEncoder`, `TextDecoder`, `performance`, `structuredClone`, `crypto.getRandomValues`.
- Plus `globalThis`, `self`, `EventTarget`, `Event`, `CustomEvent`, `MessageChannel` and a small `navigator` (`userAgent` identifying the host, `language`, `languages`, `onLine`, `hardwareConcurrency`).
- `window`, `document` and `location` are not defined by default: they would imply a second node tree behind the Renderer, DOM-scale size, and browser-detection libraries misfire on them. An opt-in DOM-compat package is future work (fog on the design map) and is not part of this spec.

The engine crate itself contains no host globals beyond the ECMAScript standard library (and `console` hooks supplied through the engine interface). Host globals are implemented in the host-runtime effort, not here; this spec only requires the engine interface to be sufficient for them.

## 9. Debugging

- A debugger interface in the engine: source positions per bytecode offset, breakpoints, stepping, frame and scope inspection, async stacks, script source.
- A separate adapter to the Chrome DevTools Protocol in its own crate, independent of any embedder.
- First version covers: console, source-mapped exceptions, breakpoints, stepping and `evaluate`. Profiler and heap profiler later.
- The interim engine has no inspector; interim debugging is handled by the dev-loop work (TASK-99.13), not here.

## 10. Crate layout

A Cargo workspace at the repository root (it was `native/engine/` in the Gable repository until TASK-100.12; Gable depends on it by pinned revision):

| Crate | Contents |
| --- | --- |
| `agnijs-engine-api` | The engine interface (section 7) and its conformance test kit. Shared with the QuickJS-NG adapter. |
| `agnijs-js-parser` | Lexer, parser, AST, early errors, source maps input. |
| `agnijs-js-compiler` | AST to bytecode, scope analysis, bytecode format and serialization. |
| `agnijs-js-vm` | Interpreter, object model and shapes, heap and GC, built-ins, jobs, modules. |
| `agnijs-js-regex` | The regex seam and its engine (adopted `regress` or our own). |
| `agnijs-js-intl` | The ICU4X bridge and ECMA-402 built-ins support. |
| `agnijs-js-debugger` | The debugger interface and its implementation hooks. |
| `agnijs-js-cdp` | The CDP adapter. |
| `agnijs-js-test262` | The test262 runner, harness glue and exclusions file handling. |
| `agnijs-engine-quickjs` | The interim adapter (may live in the host-runtime effort; its conformance-kit pass is an M0 deliverable). |

**Boundary rule (ADR 0016).** The engine stands alone. No engine crate depends on a Gable crate, package or file; `agnijs-engine-api` is the only surface shared with an embedder such as Gable, and Gable-specific adaptation (Renderer handles, the error handler, HMR policy, host globals) lives on Gable's side of the `Host` trait. Engine docs and public identifiers speak of the host or the embedder, not Gable's framework vocabulary. `crates/agnijs-js-test262/tests/boundary.rs` and the `standalone` CI job (which builds and tests an export of the tracked files alone) enforce it.

Crate boundaries may be adjusted when implementation shows a better split; record the change in an ADR. `unsafe` is confined as in section 5. Minor implementation choices (number formatting and parsing algorithms, Date parsing, BigInt arithmetic, hashing, allocator details, the exact bytecode instruction set) belong to the implementing agent: choose, and record each non-obvious choice as a short ADR in `docs/adr/`.

## 11. Conformance gate and switch-over

test262 is the correctness gate:

- Pin a test262 commit (a submodule or a fetched pinned revision) and derive the feature list for the ES2025 cut from `features.txt` plus the pinned commit, because `features.txt` has no edition field.
- Run each test as the suite requires: strict mode twice (once sloppy, once strict, except `onlyStrict`, `noStrict` and `raw` flags).
- Included: `test/built-ins`, `test/language`, `test/annexB`. Excluded: `test/staging`. `test/intl402` is reported separately; the first switch-over gate covers only the Intl subset Gable uses.
- Exclusions live in one checked-in file with a reason per entry (for example `SharedArrayBuffer multi-agent`, `IsHTMLDDA`, a Stage 3 feature). An exclusion without a reason fails the runner.
- The pass rate may never regress: CI compares against a checked-in baseline.
- CI runs on Windows, Linux and macOS (the engine is pure Rust, so the same suite runs everywhere), on every pull request that touches the engine.

test262 does not measure performance, memory, the debugger or embedding behaviour, so the switch-over rule adds its own gates. The AgniJS engine becomes the default as soon as it is usable: when conditions 1, 2 and 4 hold, it is switched in without any further sign-off (the maintainer has committed to adopting it; performance gaps are fixed after the switch). Performance is reported, not gated:

1. Every included test262 test passes except documented exclusions, with a target of zero such exceptions.
2. Gable's own test suites (core, router, forms, store and the rest) pass under it.
3. (Reported, not a gate.) A Gable benchmark set exists and the interim engine's baseline is recorded: signal churn, RxJS pipelines, the template interpreter, list diffing, and gable-ui bundle startup. The gap to the interim engine is reported and investigated (TASK-100.14), and the fixes are scheduled as ordinary work after the switch.
4. The debugger covers the first version (section 9).
5. The interim engine stays selectable for one release after the switch.

## 12. Milestones

Each milestone is a Backlog task under "Gable JS engine", with acceptance criteria tied to the test262 gate where it applies. They are ordered; a later milestone depends on the one before it unless noted.

| # | Milestone | Done when |
| --- | --- | --- |
| M0 | Workspace, engine interface crate, test262 runner, CI | The workspace builds on three OSes; the runner pins test262, applies feature filtering and the exclusions file, and reports per-directory pass rates against a baseline; the engine interface exists with a conformance kit that the QuickJS-NG adapter passes. |
| M1 | Parser and AST | The parser accepts every test262 parse-positive test in scope and rejects every parse-negative (early-error) test in scope. |
| M2 | Compiler, interpreter core and GC | Statements, expressions, functions, closures, exceptions and the garbage collector run the `language` tests that need only core built-ins; GC stress and Miri jobs run in CI. |
| M3 | Objects, shapes and core built-ins | Inline caches work; `Object`, `Function`, `Array`, `String`, `Number`, `Boolean`, `Symbol`, `Math`, `JSON`, `Error` and the reflection built-ins pass their test262 directories. |
| M4 | Promises, jobs, generators, async functions, modules | Promise jobs run through the host-driven queue; iterators, generators, async functions, async generators, static and dynamic modules, top-level await and `import.meta` pass their test262 directories; module re-loading and module-record collection are tested. |
| M5 | Regex, Date, collections, typed arrays, Proxy, Reflect, BigInt | The regex seam decision is recorded (ADR); `RegExp`, `Date`, `Map`, `Set`, `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry`, `ArrayBuffer`, typed arrays, `DataView`, `Proxy`, `Reflect`, `BigInt` and `SharedArrayBuffer`/`Atomics` (single-agent) pass their test262 directories. |
| M6 | Intl on ICU4X | The ECMA-402 built-ins and `toLocale*` methods pass the `intl402` directories for `DateTimeFormat`, `NumberFormat` and `PluralRules` first, then the rest; locale-data trimming is configurable. |
| M7 | Engine-interface adapter and bytecode serialization | The AgniJS engine passes the engine-interface conformance kit; precompiled bytecode round-trips and a version mismatch is rejected; the host can drive jobs, timers, limits and realms. |
| M8 | Debugger and CDP adapter | Breakpoints, stepping, scope inspection, async stacks, source-mapped exceptions, console and `evaluate` work through the CDP adapter, verified with a CDP client test. |
| M9 | Gable test suites and the benchmark gate | The benchmark set exists and the interim baseline is recorded; Gable's own test suites run under the engine in CI; the switch-over rule in section 11 is evaluated and reported; if conditions 1, 2 and 4 hold the engine becomes the default in this milestone, with the interim engine still selectable. |

Performance work (interpreter dispatch, inline caches, string and shape tuning, GC pacing) runs alongside M2 to M9; the benchmarks are introduced in M9 but the agent should keep micro-benchmarks as each area lands.

## 13. Working agreements for the implementing agent

- Write the failing test262 tests first where possible; the runner is the ground truth.
- Do not claim a milestone done while its test262 directories have unexplained failures. List remaining failures in the exclusions file with a reason, or fix them.
- Record non-obvious choices as short ADRs in `docs/adr/`. Use the project glossary's terms.
- Never edit Backlog files directly; use the `backlog` CLI.
- Nothing in the engine may depend on a platform; if a platform-specific path seems necessary, stop and raise it, because the maintainer cannot test on macOS.
- Prior-art research is on the branch `research/js-engine-design-and-test262` (`research/js-engine-design-and-test262.md`): Boa, Nova, Brimstone, Ignition, Hermes, JSC LLInt, QuickJS, and test262 runner mechanics.

## 14. JIT readiness

Later tiers (TASK-101: snapshots, a baseline JIT, an optimizing tier, AOT) are optional and post-v1. The interpreter stays the default and the fallback. The baseline JIT keeps the interpreter's frames, so leaving machine code means continuing in the interpreter at the frame's `pc`. ADR 0019 gives the rationale, the tiering and deoptimization strategy, the platform story and the code generator choice (Cranelift). Every engine change keeps these constraints:

| | Constraint |
| --- | --- |
| J1 | One frame stack and one register file for every tier. `pc`, `acc` and the register window are written back before any call, throw, safe point, debugger stop or suspension. Registers are addressed by base index. |
| J2 | `pc` is an instruction index. It is the single key for handlers, inline-cache sites, debugger positions, deoptimization and on-stack replacement. |
| J3 | Each opcode's generic semantics is a callable `Vm` function. Each instruction is atomic for deoptimization: all observable effects come before one result write or one branch. |
| J4 | Feedback is plain data in per-`CodeBlock` side tables, indexed by a compiler-allocated slot. Entry states are explicit, and megamorphic sticks. Feedback is never serialised and belongs to one heap. |
| J5 | Shapes are never freed while feedback or code names them. `Value` and the object header that the inline-cache hit path reads get a defined layout before TASK-101.3. No raw heap pointer outlives a runtime call. |
| J6 | The collector is precise and non-moving. Roots are explicit, and natives and JIT helpers share one rooting mechanism. There are no stack maps and no stack scanning. Any write barrier is a single `Heap` function. |
| J7 | One attention word (interrupt, memory limit, GC, stress, debugger), polled at function entry, back-edges and call returns. |
| J8 | One entry into JavaScript and one frame-push path. Arguments are a slice of the caller's register window. Faults are returned values. Generators suspend to `SavedFrame`. |
| J9 | An attached debugger forces the interpreter. Hot update creates new code blocks and never patches code. |
| J10 | `agnijs-engine-api` names no interpreter type, and the tier is an engine option. The JIT sits behind the `jit` cargo feature, off by default, in `agnijs-js-jit`, the only engine crate with `unsafe`. Runtime helpers are `extern "C"`. |
| J11 | A realm's heap serialises as data: native functions by registry index, no shared `Rc<Cell<_>>` state, and host objects through a hook or an error. |

Current designs that break a constraint, checked at `59fb104`, with the fix and where it lands:

| Constraint | Current design | Proposed fix | Where |
| --- | --- | --- | --- |
| J1 | Code addresses `acc` and `pc` through `self.frames[fi]` on every instruction. This is not a violation in itself, but TASK-100.17 moves them into locals and must keep the write-back points. | Write back at the observation points listed in J1. | TASK-100.17 (AC #1) |
| J3 | Most opcode bodies call `ops.rs` helpers, but some arms put their behaviour together inline in `Vm::step`. For example, `GetNamed` combines the cache probe, the fill and the generic lookup in the arm, and `Call` builds its arguments there. | When an arm is inlined or moved, its generic path is also a named function. | TASK-100.17, TASK-100.18 |
| J4 | `IcEntry` is a single monomorphic entry that a miss overwrites, so its state cannot be told apart. | Explicit empty, monomorphic, polymorphic and megamorphic states, with a kind field. | TASK-100.20 |
| J5 | `Value`, `PropSlot` and `ObjectData` use the Rust layout, and arenas are `Vec<Option<T>>`. Machine code cannot read a shape or a slot at a known offset. | `#[repr(C, u8)]` (or NaN-boxed) `Value`, a `#[repr(C)]` object header with `offset_of!` constants, and contiguous slot storage. | TASK-100.21 where it touches the layout; otherwise a prerequisite of TASK-101.3 |
| J6 | The collector is deferred whenever a native frame is on the stack (`native_depth != 0`). | One explicit rooting mechanism for natives (a root or handle stack) that JIT helpers can use too. | TASK-100.16 |
| J6 | About 100 sites write values into objects through `Heap::obj_mut`, so there is no single write path for a barrier. | No change while the collector is stop-the-world. Funnel the stores through one function before an incremental or generational collector starts. | Not ticketed |
| J7 | The run loop tests four flags per instruction. The interrupt hook is polled only on backward jumps, so recursion without loops is never interrupted. | One attention word (TASK-100.17). Poll at function entry. | TASK-100.17, TASK-100.33 |
| J8 | Five places call `Vm::run` (calls, globals, generator resumption twice, scripts), and natives nest run loops. Every call allocates an arguments `Vec`. | One JavaScript entry point. Arguments as a register-window slice. | TASK-100.19 |
| J10 | No crate forbids `unsafe`, so nothing enforces the confinement. | `#![forbid(unsafe_code)]` on the engine crates. | TASK-100.32 |
| J11 | `NativeKind::Fn` holds Rust function pointers. `NativeData` shares `Rc<Cell<bool>>` and `Rc<Cell<usize>>` state. `HostObject` has no serialisation hook. | A native registry indexed by id, heap-resident shared state, and a host serialisation hook or a refusal. | TASK-101.2 |
