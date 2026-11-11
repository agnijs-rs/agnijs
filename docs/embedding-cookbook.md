# Embedding AgniJS in Rust: a cookbook

Every code block on this page is compiled and run as a doctest of the `agnijs` crate (`cargo test -p agnijs --doc`), so the recipes cannot rot. The design behind them is in [rust-embedding-api.md](design/rust-embedding-api.md).

## From `cargo add` to JavaScript in ten lines

```text
cargo add agnijs serde --features serde/derive
```

```rust
use agnijs::Context;

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.eval_value("function greet(who) { return `hello, ${who}!`; }")?;
    let greeting: String = js.call("greet", ("Rust",))?;
    assert_eq!(greeting, "hello, Rust!");
    let total: i32 = js.eval("[1, 2, 3].reduce((a, b) => a + b)")?;
    assert_eq!(total, 6);
    Ok(())
}
```

`eval` runs a script and converts its completion value to any type that implements `serde::Deserialize`. `call` looks up a global function and passes a tuple (or array, or `Vec`) as its arguments; `()` passes none.

## Pass structured data across the boundary with serde

`set_global` and `get_global`, and the `to_value` and `from_value` functions behind them, convert any `Serialize` / `DeserializeOwned` type. Structs and maps become plain objects, sequences and tuples become arrays, `Option::None` is `null`.

```rust
use agnijs::Context;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Order {
    id: u32,
    items: Vec<String>,
    note: Option<String>,
}

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.set_global("order", &Order { id: 7, items: vec!["tea".into()], note: None })?;
    js.eval_value("order.items.push('milk'); order.note = 'rush';")?;
    let back: Order = js.get_global("order")?;
    assert_eq!(back, Order { id: 7, items: vec!["tea".into(), "milk".into()], note: Some("rush".into()) });
    Ok(())
}
```

The mapping is JSON's. Integers beyond 2^53 lose precision, `undefined` and non-finite numbers read back as `null`, and functions, symbols, BigInts and cyclic values are conversion errors ([`Error::Convert`](agnijs::Error::Convert)) rather than silent data loss.

## Expose a Rust function

Arguments arrive as a tuple, so arity and types are checked in one place. A mismatch throws a `TypeError` into the calling script. Use the `try` variant when the function can fail; `Err` is thrown as a JavaScript `Error`.

```rust
use agnijs::{Context, JsError};

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.register_fn("add", |(a, b): (i32, i32)| a + b)?;
    js.register_try_fn("parse", |(s,): (String,)| s.parse::<i32>())?;
    js.register_try_fn("checked", |(n,): (i32,)| {
        if n < 0 { Err(JsError::range_error("negative")) } else { Ok(n) }
    })?;

    assert_eq!(js.eval::<i32>("add(2, 3)")?, 5);
    assert_eq!(js.eval::<i32>("parse('41') + 1")?, 42);
    let name: String = js.eval("try { checked(-1) } catch (e) { e.name }")?;
    assert_eq!(name, "RangeError");
    Ok(())
}
```

## Expose a Rust struct as a JavaScript class

`#[derive(HostClass)]` marks the struct; `#[host_methods]` on an `impl` block turns `new` (or any function marked `#[constructor]`) into the constructor and each `&self` / `&mut self` method into a prototype method, renamed to camelCase. Parameters and results go through serde; a `Result` return throws on `Err`.

```rust
use agnijs::{Context, HostClass, JsError, host_methods};

#[derive(HostClass)]
struct Counter {
    count: i32,
}

#[host_methods]
impl Counter {
    fn new(start: Option<i32>) -> Self {
        Counter { count: start.unwrap_or(0) }
    }
    fn increment(&mut self, by: i32) -> i32 {
        self.count += by;
        self.count
    }
    fn checked_sub(&mut self, by: i32) -> Result<i32, JsError> {
        if by > self.count {
            return Err(JsError::range_error("would go negative"));
        }
        self.count -= by;
        Ok(self.count)
    }
}

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.register_class::<Counter>()?;
    let n: i32 = js.eval("const c = new Counter(10); c.increment(5); c.checkedSub(3)")?;
    assert_eq!(n, 12);
    let msg: String = js.eval("try { c.checkedSub(99) } catch (e) { e.message }")?;
    assert_eq!(msg, "would go negative");

    // Rust can also create instances and look inside them.
    let obj = js.wrap(Counter { count: 100 })?;
    assert_eq!(js.host_object::<Counter>(&obj).unwrap().count, 100);
    Ok(())
}
```

Keep the struct's fields to plain Rust data. A field that holds a JavaScript `Value` is a strong root for as long as the object lives (ADR 0006); implement `HostObject::trace` by hand for such classes.

## Call an async Rust function from JavaScript

`register_async_fn` exposes a future as a function that returns a promise. The engine has no event loop, so the embedder drives it: `block_on` runs ready futures and promise jobs, and parks the thread while a future waits to be woken. Futures may be woken from other threads, which is how timers and sockets plug in. The example waits on a thread-based timer.

```rust
use agnijs::Context;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

/// A future that completes after `ms` milliseconds, woken from a helper thread.
fn sleep(ms: u64) -> impl Future<Output = ()> {
    let state: Arc<Mutex<(bool, Option<Waker>)>> = Arc::new(Mutex::new((false, None)));
    let s = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        let mut g = s.lock().unwrap();
        g.0 = true;
        if let Some(w) = g.1.take() { w.wake(); }
    });
    std::future::poll_fn(move |cx| {
        let mut g = state.lock().unwrap();
        if g.0 { Poll::Ready(()) } else { g.1 = Some(cx.waker().clone()); Poll::Pending }
    })
}

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.register_async_fn("delayedDouble", |(n, ms): (i32, u64)| async move {
        sleep(ms).await;
        n * 2
    })?;
    js.register_try_async_fn("fetchUser", |(id,): (u32,)| async move {
        sleep(1).await;
        if id == 0 { Err(agnijs::JsError::error("no such user")) } else { Ok(format!("user-{id}")) }
    })?;

    let p = js.eval_value(
        "(async () => { const a = await delayedDouble(4, 5); const b = await delayedDouble(a, 1); return a + b; })()",
    )?;
    let sum: i32 = js.block_on_within(&p, Duration::from_secs(10))?;
    assert_eq!(sum, 8 + 16);

    // A rejection arrives in Rust as a typed error.
    let p = js.eval_value("fetchUser(0)")?;
    let err = js.block_on_within::<String>(&p, Duration::from_secs(10)).unwrap_err();
    assert!(err.is_js("Error"));
    assert_eq!(err.exception().unwrap().message, "no such user");
    Ok(())
}
```

## Await a JavaScript promise from Rust

`block_on` also takes any promise you get back from `eval_value` or `call_value`. A promise that nothing can settle fails with `Error::Deadlock` instead of hanging.

```rust
use agnijs::{Context, Error};

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    let p = js.eval_value("Promise.resolve(20).then(n => n + 22)")?;
    assert_eq!(js.block_on::<i32>(&p)?, 42);

    let never = js.eval_value("new Promise(() => {})")?;
    assert!(matches!(js.block_on::<()>(&never), Err(Error::Deadlock)));
    Ok(())
}
```

## Cancel pending async calls

`cancel_pending` drops every unfinished future (running its destructors) and rejects its promise with an `AbortError`. Dropping the `Context` drops them too.

```rust
use agnijs::Context;

fn main() -> agnijs::Result<()> {
    let mut js = Context::new();
    js.register_async_fn("never", |(): ()| std::future::pending::<()>())?;
    let p = js.eval_value("never().catch(e => e.name)")?;
    js.run_until_idle()?;
    assert_eq!(js.pending_tasks(), 1);

    assert_eq!(js.cancel_pending()?, 1);
    assert_eq!(js.block_on::<String>(&p)?, "AbortError");
    Ok(())
}
```

## Handle errors by type

What reaches Rust is an `agnijs::Error`: `Js` for a JavaScript exception (name, message, stack and the thrown value), `Convert` for data that cannot cross, `Deadlock` and `TimedOut` from `block_on`, and `Engine` for the rest (interrupt, memory limit). What Rust throws is a `JsError`, and every `std::error::Error` converts into one, so `?` works inside host functions.

```rust
use agnijs::{Context, Error};

fn main() {
    let mut js = Context::new();

    let e = js.eval::<i32>("null.x").unwrap_err();
    assert!(e.is_js("TypeError"));

    let e = js.eval::<i32>("(function(){})").unwrap_err();
    assert!(matches!(e, Error::Convert(_)));

    let e = js.eval::<i32>("let = ;").unwrap_err();
    assert!(e.is_js("SyntaxError"));
}
```

## Plug in your own host

`Context::with_host` takes the engine interface's `Host` trait: console sink, clock, time zone, module loading, interrupt polling. Here the console is captured and a runaway loop is interrupted.

```rust
use agnijs::{api::ConsoleLevel, Context, Error, Host};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
struct Sandbox {
    lines: RefCell<Vec<String>>,
    polls: Cell<u32>,
}

impl Host for Sandbox {
    fn console(&self, _: agnijs::api::RealmId, _: ConsoleLevel, args: Vec<String>) {
        self.lines.borrow_mut().push(args.join(" "));
    }
    fn interrupt(&self) -> bool {
        self.polls.set(self.polls.get() + 1);
        self.polls.get() > 50
    }
}

fn main() {
    let host = Rc::new(Sandbox::default());
    let mut js = Context::with_host(host.clone());
    js.eval_value("console.log('hi', 1 + 1)").unwrap();
    assert_eq!(host.lines.borrow()[0], "hi 2");

    let e = js.eval::<()>("while (true) {}").unwrap_err();
    assert!(matches!(e, Error::Engine(agnijs::api::Error::Interrupted)));
}
```
