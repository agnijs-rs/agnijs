# Embedding AgniJS: a guide

This page walks through embedding AgniJS in a Rust program through its full engine interface, from building an engine to the limits a host sets for untrusted code. It is for embedders that need more than the [cookbook](embedding-cookbook.md) covers: their own module loader, host objects with Rust data, bytecode, or the job loop under their own event loop.

Every Rust block on this page is compiled and run as a doctest of the `agnijs` crate (`cargo test -p agnijs --doc`). A complete program that puts the main steps together is [`examples/embed-minimal`](../examples/embed-minimal/src/main.rs); CI runs it and checks what it prints.

## Two layers

The `agnijs` crate is the only dependency an embedder needs ([ADR 0026](adr/0026-the-agnijs-crate-is-the-public-embedding-surface-over-the-engine-interface.md)). It has two layers:

- **`agnijs::Context`**: one engine and one realm, with serde conversion, `register_fn`, `#[derive(HostClass)]` and async functions. Start there if it covers what you need; the [cookbook](embedding-cookbook.md) shows each feature.
- **`agnijs::api`**: the engine interface itself. `agnijs::new_engine` returns a `Box<dyn Engine>` that gives you realms, values, modules, bytecode, snapshots, host functions and host objects, with a `Host` you write supplying modules, console output and the interrupt check.

The two mix: `Context::with_host` takes your `Host`, and `Context::engine` hands out the `Engine` underneath. The rest of this page uses the interface. The rustdoc of `Engine` and `Host` has a runnable example for each call shown here.

## Build an engine and run a script

```rust
use agnijs::api::{Engine, Error, Host, Value};
use std::rc::Rc;

/// Every hook has a default, so a host implements only what it uses.
struct App;
impl Host for App {}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    let sum = engine.eval_script(realm, "sum.js", "1 + 2")?;
    assert_eq!(sum, Value::Number(3.0));
    Ok(())
}
```

An engine owns one heap and any number of realms (`create_realm`); `main_realm` exists from the start. The name passed to `eval_script` appears in stack traces and the debugger. `agnijs::new_engine_from_snapshot` builds an engine from `Engine::snapshot` bytes instead, which starts faster when every engine runs the same setup code.

An engine is single-threaded: it, its values and its host are `!Send`. Give each thread its own engine and pass data between them as Rust values.

## Expose a Rust function

`new_function` turns a closure into a JavaScript function. It gets the engine, so it can call back in, and a `CallContext` with `this`, the arguments and the realm the function belongs to.

```rust
use agnijs::api::{CallContext, Engine, Error, Host, Value};
use std::rc::Rc;

struct App;
impl Host for App {}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    let add = engine.new_function(
        realm,
        "add",
        Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
            // A missing argument reads as undefined; to_number converts as `+x` does.
            let a = engine.to_number(cx.realm, &cx.arg(0))?;
            let b = engine.to_number(cx.realm, &cx.arg(1))?;
            Ok(Value::Number(a + b))
        }),
    )?;
    let global = engine.global_object(realm)?;
    engine.set(realm, &global, &"add".into(), &add)?;
    assert_eq!(engine.eval_script(realm, "add.js", "add(2, 40)")?, Value::Number(42.0));
    Ok(())
}
```

What a host function returns as `Err` reaches JavaScript by kind: an `Error::Exception` throws its value unchanged, so `?` on a call back into the engine rethrows what JavaScript threw; `Error::Host` and `Error::Internal` throw a catchable `Error` with the message; `Interrupted`, `OutOfMemory` and `OutOfFuel` stop the run and cannot be caught. Host functions are not constructors. The full contract, re-entrancy included, is on `HostFunction` in the rustdoc.

## Expose a Rust type as a host object

A host object is a JavaScript object with Rust data inside. Register a class once, create instances with `new_host_object`, and reach the data from methods with `host_object_data`.

```rust
use agnijs::api::{CallContext, Engine, Error, Host, HostClassDef, HostObject, Value};
use std::any::Any;
use std::rc::Rc;

struct App;
impl Host for App {}

/// The Rust data behind each `Counter` object.
struct Counter {
    count: u32,
}

impl HostObject for Counter {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    let class = engine.register_host_class(HostClassDef {
        name: "Counter".into(),
        finalize: None,
    });

    // Methods live on a prototype and find their data through `this`.
    let proto = engine.new_object(realm)?;
    let increment = engine.new_function(
        realm,
        "increment",
        Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
            let counter = engine
                .host_object_data(&cx.this)
                .and_then(|data| data.as_any_mut().downcast_mut::<Counter>())
                .ok_or_else(|| Error::Host("increment needs a Counter".into()))?;
            counter.count += 1;
            Ok(Value::Number(counter.count.into()))
        }),
    )?;
    engine.set(realm, &proto, &"increment".into(), &increment)?;

    // Host functions are not constructors, so instances come from a factory.
    let make = engine.new_function(
        realm,
        "makeCounter",
        Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
            engine.new_host_object(cx.realm, class, Box::new(Counter { count: 0 }), Some(&proto))
        }),
    )?;
    let global = engine.global_object(realm)?;
    engine.set(realm, &global, &"makeCounter".into(), &make)?;

    let n = engine.eval_script(realm, "use.js", "const c = makeCounter(); c.increment(); c.increment()")?;
    assert_eq!(n, Value::Number(2.0));

    // Rust reads the same data.
    let c = engine.eval_script(realm, "c.js", "c")?;
    let count = engine
        .host_object_data(&c)
        .and_then(|data| data.as_any().downcast_ref::<Counter>())
        .map(|counter| counter.count);
    assert_eq!(count, Some(2));

    // Error::Host from a method is a catchable Error.
    let message = engine.eval_script(realm, "misuse.js", "try { c.increment.call({}) } catch (e) { e.message }")?;
    assert_eq!(message, Value::string("increment needs a Counter"));
    Ok(())
}
```

The `agnijs` crate's `#[derive(HostClass)]` builds the same thing, with a real constructor and argument conversion, on top of these calls.

A `Value` handle kept in host data keeps what it names alive for as long as the data holds it. AgniJS treats it as a root and does not trace through host data, so a handle that leads back to its own wrapper is a cycle that lives until the engine is dropped; store such references as JavaScript properties of the wrapper instead. A class's `finalize` runs with the data after the wrapper is collected, from the next `run_jobs`, and not at all when the engine itself is dropped. The details are on `HostObject` in the rustdoc.

## Load modules

The engine never touches the file system. It asks the host to resolve each specifier to a canonical name, then to load that name, once per canonical name:

```rust
use agnijs::api::{Engine, Error, Host, ImportAttribute, ModuleSource, PromiseState, Value};
use std::rc::Rc;

struct App;

impl Host for App {
    fn resolve_module(
        &self,
        _referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(format!("app/{}", specifier.trim_start_matches("./")))
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        let source = match resolved {
            "app/main.js" => "import { double } from './math.js'; export const answer = double(21);",
            "app/math.js" => "export const double = (n) => n * 2;",
            _ => return Err(format!("module not found: {resolved}")),
        };
        Ok(ModuleSource::Source(source.into()))
    }
}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    let promise = engine.load_module(realm, "main.js")?;
    engine.run_jobs(realm)?;
    match engine.promise_state(realm, &promise)? {
        PromiseState::Fulfilled(namespace) => {
            assert_eq!(engine.get(realm, &namespace, &"answer".into())?, Value::Number(42.0));
        }
        other => panic!("main.js did not load: {other:?}"),
    }
    Ok(())
}
```

`load_module` returns a promise for the module namespace. A module that cannot be loaded, a syntax error or an exception during evaluation rejects that promise; the call itself fails only for misuse. A host can also return `ModuleSource::Json`, source with a source map (`SourceWithMap`), or bytecode from `compile_module` (`Bytecode`). `import.meta` is filled by the `import_meta` hook. For Node-style resolution (`node_modules`, `package.json` exports, import maps), the optional `agnijs-js-resolve` crate implements these two hooks ([ADR 0023](adr/0023-node-style-module-resolution-is-an-optional-crate-outside-the-engine.md)).

### TypeScript modules

The engine runs JavaScript only. To load TypeScript, transpile it in `load_module` with the optional `agnijs-js-typescript` crate and return the JavaScript with its source map; the engine then reports TypeScript positions in `Error.stack`, `Exception::stack`, CallSite objects and the debugger. [`examples/embed-typescript`](../examples/embed-typescript/src/main.rs) is the complete program:

```rust
use agnijs_js_typescript::{transpile, Options};

fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
    let source = read(resolved)?;
    // The settings come from the extension: .ts and .mts are modules,
    // .cts a script, .tsx a module with JSX. A .d.ts is None.
    let options = Options::for_path(resolved)
        .ok_or_else(|| format!("{resolved} is a declaration file"))?;
    // On a syntax error the text is the diagnostics, one per line with
    // its position, and the import rejects with it.
    let out = transpile(resolved, &source, &options).map_err(|e| e.to_string())?;
    Ok(ModuleSource::SourceWithMap { source: out.code, source_map: out.source_map })
}
```

An exception thrown in `account.ts` then reports `at app/account.ts:8:7`, and a file the crate refuses fails its import with `app/broken.ts:2:3: \`accessor\` fields are not supported`. Types are stripped, never checked: run `tsc --noEmit` in your build if you want them checked. `Options` also selects strip-only mode (erasable syntax only), JSX settings and legacy decorators (`experimental_decorators`, off by default). For a script, transpile with `SourceKind::Script` and pass the map to `set_source_map` before `eval_script`. The crate needs Rust 1.97 and runs Oxc on a worker thread with an input-sized stack; for untrusted TypeScript lower `max_source_len` ([security-model.md](security-model.md#typescript-transpiling)). What it supports and refuses is in [ADR 0029](adr/0029-typescript-is-transpiled-by-oxc-in-an-optional-crate-outside-the-engine.md) and the crate's rustdoc.

## Drive the job loop

The engine never runs promise reactions on its own. The host runs them with `run_jobs`, after the script or module it started and after every task its event loop runs (a timer firing, an I/O completion):

```rust
use agnijs::api::{Engine, Error, Host, RealmId, Value};
use std::rc::Rc;

struct App;
impl Host for App {}

/// Run jobs until none are left.
fn run_job_loop(engine: &mut dyn Engine, realm: RealmId) -> Result<usize, Error> {
    let mut total = 0;
    loop {
        match engine.run_jobs(realm)? {
            0 => return Ok(total),
            n => total += n,
        }
    }
}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    engine.eval_script(
        realm,
        "jobs.js",
        "globalThis.order = []; Promise.resolve().then(() => order.push('job')); order.push('script');",
    )?;
    assert_eq!(run_job_loop(&mut *engine, realm)?, 1);
    let order = engine.eval_script(realm, "order.js", "order.join(', ')")?;
    assert_eq!(order, Value::string("script, job"));
    Ok(())
}
```

`run_jobs` also runs the finalizers of collected host objects. An exception that escapes a job stops the drain and is returned; the jobs after it stay queued. Unhandled rejections are reported to the `promise_rejection` hook. Timers, `fetch` and the rest of a runtime are the host's to provide, as host functions that settle promises from later tasks; the [cookbook](embedding-cookbook.md#call-an-async-rust-function-from-javascript) shows async Rust functions doing this through `Context`.

## Errors

Every engine call returns `agnijs::api::Result`. The `Error` kinds an embedder handles:

| Error | Meaning | The engine afterwards |
| --- | --- | --- |
| `Exception(e)` | JavaScript threw, a syntax error included. `e.value` is the thrown value; `e.name`, `e.message` and `e.stack` (innermost frame first) are read out of it. | Usable |
| `Interrupted` | The `interrupt` hook returned true. | Usable |
| `OutOfMemory` | The memory limit was reached. | Usable |
| `OutOfFuel` | The fuel budget ran out. | Usable once fuel is set again |
| `ForeignHandle`, `UnknownRealm` | A handle or a realm id from another engine, or a realm id this engine never issued: a host bug. | Usable |
| `BytecodeMismatch`, `BytecodeCorrupt` | Bytecode or a snapshot from another build, or damaged. | Usable |
| `Host(message)` | A host hook failed. | Usable |
| `Poisoned` | A panic unwound through this engine earlier; every later call that runs code is refused. | Drop it |

`Error` is `#[non_exhaustive]`: new kinds may be added in a minor version, so a `match` on it needs a wildcard arm. So are the enums a host receives in its hooks (`ModuleSource`, `ConsoleLevel`, `RejectionEvent`, `RegexMatcher`, `ErrorKind`): give each `match` a wildcard arm that does something harmless, such as logging a console message of an unknown level as `Log`. A panic in a host function or hook is not caught; it unwinds to the caller and poisons the engine. The C API catches panics at its boundary instead ([c-api.md](c-api.md)).

```rust
use agnijs::api::{Engine, Error, Host};
use std::rc::Rc;

struct App;
impl Host for App {}

fn main() {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    let source = "function check(n) {\n  if (n < 0) throw new RangeError('negative');\n}\ncheck(-1);";
    match engine.eval_script(realm, "check.js", source) {
        Err(Error::Exception(e)) => {
            assert_eq!((e.name.as_str(), e.message.as_str()), ("RangeError", "negative"));
            let top = &e.stack[0];
            assert_eq!((top.function.as_str(), top.script.as_str(), top.line), ("check", "check.js", 2));
        }
        other => panic!("{other:?}"),
    }
}
```

## Limits for untrusted code

A fresh engine bounds string length (2^30 code units, always on), the interpreter's frame depth and native stack use, and routes regular expressions that could backtrack exponentially to a linear-time matcher. Everything else is the host's to set before it runs code it does not trust:

```rust
use agnijs::api::{Engine, Error, Host};
use std::rc::Rc;

struct App;
impl Host for App {}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();

    // An instruction budget, deterministic across runs and platforms.
    engine.set_fuel(Some(100_000))?;
    let run = engine.eval_script(realm, "spin.js", "for (;;) {}");
    assert!(matches!(run, Err(Error::OutOfFuel)));
    engine.set_fuel(None)?;

    // Native stack for one call into the engine, below the 512 KiB default.
    // Nesting past it throws a catchable RangeError instead of overflowing.
    engine.set_stack_budget(Some(256 << 10))?;
    let depth = engine.eval_script(realm, "deep.js", "let d = 0; function f() { d++; f(); } try { f() } catch (e) { e.name }")?;
    assert_eq!(engine.to_string(realm, &depth)?, "RangeError");

    // A heap limit: past it the running call fails with OutOfMemory.
    engine.set_memory_limit(Some(16 << 20));
    let run = engine.eval_script(realm, "grow.js", "const keep = []; for (;;) keep.push(new ArrayBuffer(1 << 20));");
    assert!(matches!(run, Err(Error::OutOfMemory)));
    Ok(())
}
```

For a wall-clock limit, implement `Host::interrupt` to check a deadline or a flag another thread sets; it is polled at function entries, loop back-edges and inside long-running built-ins. JavaScript cannot catch running out of memory or fuel, or an interrupt, and the engine stays usable after each, though a script that filled the heap still holds what it allocated until the host drops what refers to it (here, the global `keep`) or the engine. [The security model](security-model.md) says exactly what each limit covers, what the engine guarantees and what it leaves to the host, and ends with an embedder checklist. Realms are not a security boundary: code that must not reach other code needs an engine of its own.

## Handle lifetimes

Primitives (`Number`, `Bool`, strings, BigInts) are copied out of the engine. Objects and symbols come out as handles: a `Handle` inside a `Value` is a persistent root that keeps its value alive, whether or not JavaScript can still reach it, until the last clone is dropped. In short:

- Every value that crosses out is a new handle; `==` on two handles compares the handles, `Engine::strict_equals` compares the JavaScript values.
- Dropping the last clone releases the root the next time the engine is entered, so dropped handles cost nothing until then. `collect_garbage` releases them first.
- Handles belong to one engine, are valid in all of its realms and cannot cross threads. Given to another engine, a handle is `Error::ForeignHandle`, never a crash.
- Handles may outlive their engine; they are then inert and safe to drop.

The full contract is in the rustdoc of the `agnijs_engine_api::value` module, under "Lifetimes", and `crates/agnijs/tests/lifetimes.rs` tests each rule.

## Versioning

The `agnijs` crate, including everything under `agnijs::api`, follows the semver policy in [stability.md](stability.md#the-rust-api). Items hidden from the rustdoc are for engine implementors and are outside it.

## Security and fuzzing

- [docs/security-model.md](security-model.md): the threat model, the guarantees and limits, the non-guarantees, an embedder checklist and an example of granting capabilities one at a time.
- [SECURITY.md](../SECURITY.md): how to report a vulnerability privately.
- [docs/fuzzing.md](fuzzing.md): the fuzz targets (parser, bytecode, snapshots, regex, TypeScript, the C API and others), how to run them and what CI runs. The seed corpus for every target is in [`crates/agnijs-fuzz/src/seeds.rs`](../crates/agnijs-fuzz/src/seeds.rs); `agnijs-fuzz --seeds <target> <dir>` writes it out as a libFuzzer corpus. An embedder that feeds the engine its own kind of input can start a fuzz target from there.
