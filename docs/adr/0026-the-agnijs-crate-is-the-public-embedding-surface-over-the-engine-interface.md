# 26. The `agnijs` crate is the public embedding surface; the engine interface is its full layer and implementor items stay outside it

Date: 2026-11-15

## Status

Accepted

## Context

AgniJS is meant to be embedded, but until now its embedding surface was whatever the in-family embedders happened to use. Three of them shaped it:

- **Gable's engine harness** (`native/engine-harness` in the Gable repository) runs Gable's test suites and benchmarks on either engine. It depends on `agnijs-engine-api` and calls `agnijs_js_vm::make_engine`. Its whole use of the interface is: `main_realm`, `global_object`, `eval_script`, `load_module`, `run_jobs` and `has_pending_jobs`, `new_function`, `new_object`, `get`, `set` and `own_keys`, `call`, `is_promise` and `promise_state`, `to_string` and `strict_equals`. Its `Host` implements `resolve_module`, `load_module`, `console`, `promise_rejection`, `interrupt` and `now_ms`. It keeps handles inside host-function closures and in its own structures, which is why ADR 0006 chose lifetime-free handles.
- **The QuickJS-NG adapter** (`agnijs-engine-quickjs`) implements the interface. It is the only code outside the VM that uses `HandleRegistry`, `next_engine_id`, the `bytecode` container helpers and the conformance kit. Bridging rquickjs's lifetime-bound values and non-reentrant `Context::with` to the interface is the cost ADR 0006 recorded; nothing in it asks for a different calling side.
- **The `agnijs` crate** (TASK-103.4) wraps the interface in a `Context` with serde conversion, derive-based host classes and async functions, and re-exports the interface as `agnijs::api`. The C API (ADR 0021) wraps the interface too.

So the calling side of `Engine` and the whole of `Host` have two independent consumers and have not needed reshaping. What was missing for a first external embedder:

- **A front door.** Constructing an engine meant depending on `agnijs-js-vm`, whose `pub` items are the VM's internals (the `Vm` struct, the debugger hooks, the JIT switch), not an API.
- **A boundary between embedders and implementors.** The interface crate exports items only an engine implementation uses next to the ones an embedder uses, with nothing to tell them apart.
- **Room to grow.** `Error` gained `OutOfFuel` and `Poisoned` in the last month; each was a breaking change for anyone who matched it exhaustively.

## Decision

**Embedders depend on one crate, `agnijs`.** It has two layers, and both are public:

- **`agnijs::Context`** and its helpers (serde conversion, `register_fn`, `#[derive(HostClass)]` with `#[host_methods]`, async functions, `Error`/`JsError`): the short path for the common cases, unchanged from TASK-103.4.
- **`agnijs::api`**, the re-exported `agnijs-engine-api`: the full interface, for modules, bytecode, realms, raw values and handles, host functions and classes that take or return handles, limits and the job loop. `agnijs::new_engine(host)` and `agnijs::new_engine_from_snapshot(host, bytes)` construct an AgniJS engine as a `Box<dyn api::Engine>`, so an embedder that needs the full layer still never names `agnijs-js-vm`. `Context::from_engine` puts the short path over such an engine; `Context::engine` reaches the full layer from a context.

**Public in `agnijs-engine-api`** (reached as `agnijs::api`), and so under the semver policy in `docs/stability.md`:

- the `Engine` trait, as a set of methods to call;
- the `Host` trait, as a set of hooks to implement;
- the value types: `Value`, `Handle`, `JsString`, `JsBigInt`, `PropertyKey`, `PromiseState`, `TypeOf`, `HeapKind`, `RealmId`, `EngineId`;
- the errors: `Error`, `Exception`, `StackFrame`, `Result`, `ErrorKind`;
- host functions and classes: `HostFunction`, `CallContext`, `HostObject`, `Tracer`, `HostClassDef`, `HostClassId`;
- hook data: `ImportAttribute`, `ModuleSource`, `ConsoleLevel`, `RejectionEvent`, `RegexMatcher`, `MemoryUsage`, `EngineInfo`.

**Implementor-only, hidden from the documentation and outside the semver policy:** `HandleRegistry`, `next_engine_id`, the `bytecode` container module and the `conformance` kit with its `conformance_tests!` macro. They are still `pub`, because the VM and the QuickJS-NG adapter are separate crates, but they are `#[doc(hidden)]`, which is also what `cargo-semver-checks` treats as not public. Implementing `Engine` outside this workspace is possible and not supported: a new `Engine` method may be added in any release, with a default body where one makes sense. A new `Host` hook is always added with a default, so an embedder's `Host` keeps compiling.

**Internal crates.** `agnijs-js-vm`, `agnijs-js-parser`, `agnijs-js-compiler`, `agnijs-js-regex`, `agnijs-js-intl`, `agnijs-js-jit`, `agnijs-engine-quickjs`, the benchmark, test262, fuzz and CLI crates, and the Rust items of `agnijs-capi` have no compatibility promise. `agnijs-derive` is public only through the macros `agnijs` re-exports. The C API is public through its header and ABI (ADR 0021), not through its Rust items. The debugger capability (ADR 0017) and `agnijs-js-cdp`, and the optional resolver `agnijs-js-resolve` (ADR 0023), are embedder-facing but not yet under the policy; each needs its own review before it is.

**What changes now:**

- `agnijs::new_engine` and `agnijs::new_engine_from_snapshot` are added.
- `api::Error` and `agnijs::Error` are `#[non_exhaustive]`, so a new failure kind is not a breaking change. Code outside the defining crate matches them with a wildcard arm.
- The implementor-only items above are `#[doc(hidden)]`.

**What deliberately stays:**

- Values stay lifetime-free handles (ADR 0006): the harness and the `agnijs` crate both keep them in closures and structures.
- Every call stays on `&mut self` with an explicit `RealmId`.
- `RealmId` stays a plain index: it is valid only on the engine that issued it.
- A handle stays a strong root, and `HostObject::trace` stays advisory: the AgniJS collector does not call it (TASK-104.3 documents this).

These are documented as the contract (TASK-104.2, TASK-104.3) rather than changed.

## Considered options

- **Make the interface crate the front door.** Embedders would need two dependencies (the interface and an engine crate) and would see the implementor items. The `agnijs` crate already re-exports the interface, so making it the one dependency costs one function per constructor.
- **Move the implementor items into a separate crate** (`agnijs-engine-impl`). Cleaner in the documentation, but it splits `Handle` from the registry that creates it, and `Handle`'s fields would have to become public for that crate to construct one. `#[doc(hidden)]` gives the same semver result without that.
- **Seal `Engine`** with a private supertrait. The QuickJS-NG adapter is a separate crate, so it would need the seal to be public, which is no seal at all.
- **Tag `RealmId` with its engine**, as `Handle` is, so a realm id given to the wrong engine is `UnknownRealm` instead of naming that engine's realm with the same index. It changes a public type that every call takes; recorded in TASK-104 as an owner decision, not done.
- **`#[non_exhaustive]` on the hook enums** (`ModuleSource`, `ConsoleLevel`, `RejectionEvent`, `RegexMatcher`, `ErrorKind`). They change less often than `Error` and are matched in eight crates of this workspace; recorded in TASK-104 as an owner decision for before the first release.

## Consequences

An external embedder's `Cargo.toml` names `agnijs` and nothing else. The semver check (TASK-104.6) runs on `agnijs` and `agnijs-engine-api` and sees exactly the surface listed here. Matches on `api::Error` in other workspace crates (the VM, the C API, the CLI) need a wildcard arm and lose the compiler's reminder when a variant is added. Each of those crates maps the wildcard to a generic failure: the VM throws an `Error` with the message, the C API returns `AGNIJS_ERR_ENGINE`. Gable's harness keeps working unchanged: it uses only public items and `agnijs_js_vm::make_engine`, which stays, and it may move to `agnijs::new_engine` when it next moves its pin.
