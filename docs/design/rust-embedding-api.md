# Design note: the Rust embedding surface (TASK-103.4)

Status: implemented in the `agnijs` and `agnijs-derive` crates. The cookbook is [embedding-cookbook.md](../embedding-cookbook.md).

## Problem

`agnijs-engine-api` (ADR 0006) is the complete, engine-neutral boundary, but it is written for a host runtime that keeps handles for years: values are an enum, objects are handles, every call takes a realm id, host functions get a `CallContext`. An embedder who wants to call one JavaScript function should not need any of that.

## What other Rust engines do

| | Boa | rquickjs | deno_core |
| --- | --- | --- | --- |
| Values | `JsValue`, lives in a `Context` it is passed to | `Value<'js>`, lifetime-bound to a `Ctx<'js>` closure | V8 `Local<'s, T>` behind scopes (`v8::HandleScope`) |
| Host functions | `NativeFunction::from_fn_ptr` / closures, manual `JsValue` arg handling | `Function::new`, argument tuples via `FromJs`/`IntoJs` traits | `#[op2]` proc macro, typed args and serde (`#[serde]`) |
| Host classes | `#[derive(Trace, Finalize, JsData)]` and manual `Class` impl | `#[class]` / `#[methods]` macros | ops plus `v8::cppgc`, mostly manual |
| serde | `JsValue::from_json` / `to_json` (JSON only) | `rquickjs::serde` module | `serde_v8` (direct, fast, the model for typed ops) |
| Async | job executor trait, futures via `JobExecutor` | `AsyncRuntime` / `AsyncContext`, `Promised`, futures bridged to promises | whole runtime is async; ops can be `async fn` |

Lessons taken: rquickjs and deno_core show that the ergonomic win is typed arguments and returns through traits or serde plus a macro for classes, not exposing values. Boa's closures are the lowest-friction function story. deno_core's async ops show that the promise is created at call time and the future is owned by the runtime, which is what `register_async_fn` does. rquickjs's lifetime-bound values are exactly what ADR 0006 rejected, and this layer does not bring them back.

## Decision

A new crate, `agnijs`, over the handle-based interface, with a `Context` that owns a `Box<dyn Engine>`, its main realm and a small executor. Nothing in the engine crates changes; the layer uses the public `Engine` trait only, so it works on any conforming engine. (It currently constructs the AgniJS engine; making it generic is a one-line change if the QuickJS adapter is ever wanted.)

- **serde by value tree.** `to_value` serializes to `serde_json::Value` and builds the JavaScript object; `from_value` walks the JavaScript value into a `serde_json::Value` and deserializes. The mapping is JSON's with numbers as JavaScript numbers. This costs one intermediate tree but is about 100 lines and cannot drift from serde's semantics. Lossy cases are errors or documented: functions, symbols, BigInts and cycles are `Error::Convert`; integers past 2^53 lose precision; `undefined` and non-finite numbers read as `null`. A direct `Serializer`/`Deserializer` over handles (deno_core's `serde_v8` route) is the performance upgrade if a profile ever asks for it.
- **Functions take tuples.** `register_fn(name, |(a, b): (i32, i32)| ...)`: one serde decode, one place for arity and type errors (a `TypeError` thrown into the script). `register_try_fn` is the fallible form. Two names instead of one overloaded trait, because `Result<T, E>` is itself `Serialize` and a blanket "into return" trait cannot tell them apart on stable Rust.
- **Classes by derive plus attribute.** `#[derive(HostClass)]` implements `HostObject` (the `Any` boilerplate) and a `NAME`; `#[host_methods]` on an `impl` generates the constructor and prototype methods. Two macros because a derive cannot see an `impl` block. The generated JavaScript class is a real script `class` whose constructor forwards to a host function that passes `new.target.prototype`: host functions are not constructors in the engine interface (ADR 0006's adapter limitation, also true of the AgniJS engine), and this keeps `new`-less calls, `instanceof` and subclassing native without changing engine semantics. Class state is plain Rust data; a class holding JavaScript values implements `HostObject::trace` by hand.
- **Async as an executor inside `Context`.** The engine has no event loop (ADR 0006), so the embedder drives it. An async host function creates a promise through the engine interface (the `Promise` constructor and a capturing executor), parks the future, and returns the promise. `turn` polls woken futures, settles promises and runs jobs; `block_on` loops that and parks the thread between wake-ups. Wakers are thread-safe (a flag plus `Thread::unpark`), futures need not be `Send` and never see the engine, so they cannot re-enter it. The reverse direction (awaiting a JavaScript promise from Rust) is `block_on` / `block_on_within`, with `Error::Deadlock` when nothing can settle the promise and `Error::TimedOut` on a deadline.
- **Cancellation** is embedder-side: `cancel_pending` drops every unfinished future (so its destructors run) and rejects its promise with an error named `AbortError`; dropping the `Context` drops them too. Per-call cancellation from JavaScript (an `AbortSignal`) is not provided: the engine has no such global and a host global is the embedder's business.
- **Typed errors.** `Error` separates `Js(JsException)`, `Convert`, `Deadlock`, `TimedOut` and `Engine(api::Error)`. `JsError` is what Rust throws; any `std::error::Error` converts into it, so `?` works in host functions. `JsError` deliberately does not implement `std::error::Error` itself (the blanket `From` would conflict), the same trade `anyhow::Error` makes.

## Not done, on purpose

Arguments and returns are data, not handles: a host function cannot receive a JavaScript function or return a promise it built itself. Code that needs that uses `Context::engine()` and the interface directly. Modules, bytecode and realms are likewise reached through `engine()` or `Context::with_host`. Free-function macros (`#[host_fn]`) were skipped: a closure with a tuple is already one line.

## Compatibility

The C API and the engine crates are untouched. The new crates are workspace members and add `syn`, `quote` and `proc-macro2` (build-time only) to the lock file.
