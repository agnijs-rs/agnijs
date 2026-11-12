//! Behavior tests for the embedding API: serde round trips, host classes,
//! async functions (including errors and cancellation) and typed errors.

use agnijs::{host_methods, Context, Error, HostClass, JsError};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Nested {
    name: String,
    tags: Vec<String>,
    scores: BTreeMap<String, f64>,
    pair: (i32, bool),
    maybe: Option<u8>,
    child: Option<Box<Nested>>,
}

fn sample() -> Nested {
    Nested {
        name: "outer".into(),
        tags: vec!["a".into(), "b".into()],
        scores: BTreeMap::from([("x".to_string(), 1.5), ("y".to_string(), -2.0)]),
        pair: (-7, true),
        maybe: Some(200),
        child: Some(Box::new(Nested {
            name: "inner".into(),
            tags: vec![],
            scores: BTreeMap::new(),
            pair: (0, false),
            maybe: None,
            child: None,
        })),
    }
}

#[test]
fn serde_round_trip_through_the_engine() {
    let mut js = Context::new();
    let v = js.to_js(&sample()).unwrap();
    let back: Nested = js.from_js(&v).unwrap();
    assert_eq!(back, sample());
}

#[test]
fn rust_data_is_plain_js_data() {
    let mut js = Context::new();
    js.set_global("n", &sample()).unwrap();
    let s: String = js
        .eval("JSON.stringify([n.name, n.tags, n.pair, n.maybe, n.child.maybe, n.scores.y])")
        .unwrap();
    assert_eq!(s, r#"["outer",["a","b"],[-7,true],200,null,-2]"#);
    let names: Vec<String> = js.eval("[n.name, n.child.name]").unwrap();
    assert_eq!(names, ["outer", "inner"]);
}

#[test]
fn unconvertible_values_are_errors_not_data_loss() {
    let mut js = Context::new();
    for src in [
        "(() => 1)",
        "Symbol('s')",
        "10n",
        "(() => { const o = {}; o.me = o; return o; })()",
    ] {
        let e = js.eval::<serde_json::Value>(src).unwrap_err();
        assert!(matches!(e, Error::Convert(_)), "{src}: {e}");
    }
    assert_eq!(
        js.eval::<serde_json::Value>("undefined").unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(
        js.eval::<serde_json::Value>("NaN").unwrap(),
        serde_json::Value::Null
    );
    let e = js.eval::<i32>("'text'").unwrap_err();
    assert!(matches!(e, Error::Convert(_)));
}

#[test]
fn rust_functions_check_arguments_and_throw_typed_errors() {
    let mut js = Context::new();
    js.register_fn("add", |(a, b): (i32, i32)| a + b).unwrap();
    js.register_try_fn("kind", |(k,): (String,)| match k.as_str() {
        "type" => Err(JsError::type_error("tt")),
        "range" => Err(JsError::range_error("rr")),
        _ => Ok(k),
    })
    .unwrap();
    assert_eq!(js.eval::<i32>("add(1, 2)").unwrap(), 3);
    assert_eq!(
        js.eval::<String>("try { add('x', 1) } catch (e) { e.name }")
            .unwrap(),
        "TypeError"
    );
    assert_eq!(
        js.eval::<String>("try { kind('type') } catch (e) { e.name + ':' + e.message }")
            .unwrap(),
        "TypeError:tt"
    );
    assert_eq!(
        js.eval::<String>("try { kind('range') } catch (e) { e.name }")
            .unwrap(),
        "RangeError"
    );
    let e = js.eval::<String>("kind('range')").unwrap_err();
    assert!(e.is_js("RangeError"));
    assert_eq!(e.exception().unwrap().message, "rr");
    assert_eq!(js.call::<_, i32>("add", (20, 22)).unwrap(), 42);
}

#[derive(HostClass)]
#[agnijs(name = "Acc")]
struct Account {
    balance: i64,
    log: Vec<String>,
}

#[host_methods]
impl Account {
    #[constructor]
    fn open(initial: Option<i64>, owner: String) -> Result<Self, JsError> {
        if owner.is_empty() {
            return Err(JsError::type_error("owner required"));
        }
        Ok(Account {
            balance: initial.unwrap_or(0),
            log: vec![owner],
        })
    }
    fn deposit(&mut self, amount: i64) -> i64 {
        self.balance += amount;
        self.balance
    }
    fn history_len(&self) -> usize {
        self.log.len()
    }
    fn note(&mut self, text: String) {
        self.log.push(text);
    }
    fn names(&self) -> Vec<String> {
        self.log.clone()
    }
    fn helper() -> i32 {
        1
    }
}

#[test]
fn host_class_constructor_methods_and_errors() {
    let mut js = Context::new();
    js.register_class::<Account>().unwrap();
    assert_eq!(Account::helper(), 1);
    let r: Vec<serde_json::Value> = js
        .eval(
            "const a = new Acc(5, 'ann'); a.note('hello');
             [a.deposit(10), a.historyLen(), a.names(), a.note('x') === undefined,
              a instanceof Acc, a.constructor === Acc, Object.prototype.toString.call(a)]",
        )
        .unwrap();
    assert_eq!(r[0], 15);
    assert_eq!(r[1], 2);
    assert_eq!(r[2], serde_json::json!(["ann", "hello"]));
    assert_eq!(r[3], true);
    assert_eq!(r[4], true);
    assert_eq!(r[5], true);
    // Constructor errors and call-without-new throw.
    assert_eq!(
        js.eval::<String>("try { new Acc(1, '') } catch (e) { e.name + ':' + e.message }")
            .unwrap(),
        "TypeError:owner required"
    );
    assert!(
        js.eval::<String>("try { Acc(1, 'a') } catch (e) { e.name }")
            .unwrap()
            == "TypeError"
    );
    // A method on the wrong receiver throws instead of corrupting memory.
    assert_eq!(
        js.eval::<String>("try { a.deposit.call({}, 1) } catch (e) { e.name }")
            .unwrap(),
        "TypeError"
    );
    // Bad method arguments throw.
    assert_eq!(
        js.eval::<String>("try { a.deposit('x') } catch (e) { e.name }")
            .unwrap(),
        "TypeError"
    );
    // Rust-created instances share the class.
    let obj = js
        .wrap(Account {
            balance: 9,
            log: vec![],
        })
        .unwrap();
    js.set_global("fromRust", &serde_json::Value::Null).unwrap();
    let realm = js.realm();
    let g = js.engine().global_object(realm).unwrap();
    js.engine()
        .set(realm, &g, &"fromRust".into(), &obj)
        .unwrap();
    assert_eq!(js.eval::<i64>("fromRust.deposit(1)").unwrap(), 10);
    assert!(js.eval::<bool>("fromRust instanceof Acc").unwrap());
    assert_eq!(js.host_object::<Account>(&obj).unwrap().balance, 10);
}

#[test]
fn async_function_resolves_and_rejects() {
    let mut js = Context::new();
    js.register_async_fn("twice", |(n,): (i32,)| async move { n * 2 })
        .unwrap();
    js.register_try_async_fn("boom", |(n,): (i32,)| async move {
        if n > 0 {
            Err(JsError::range_error("too big"))
        } else {
            Ok(n)
        }
    })
    .unwrap();
    // Nothing runs before the context is driven.
    let p = js.eval_value("twice(21)").unwrap();
    assert_eq!(js.pending_tasks(), 1);
    assert_eq!(js.block_on::<i32>(&p).unwrap(), 42);
    assert_eq!(js.pending_tasks(), 0);

    let p = js.eval_value("boom(1)").unwrap();
    let e = js.block_on::<i32>(&p).unwrap_err();
    assert!(e.is_js("RangeError"), "{e}");
    assert_eq!(e.exception().unwrap().message, "too big");

    // A bad argument rejects (not throws) and the script can catch it.
    let r: String = {
        let p = js
            .eval_value("(async () => { try { await twice('x') } catch (e) { return e.name } })()")
            .unwrap();
        js.block_on(&p).unwrap()
    };
    assert_eq!(r, "TypeError");

    // await in JS composes several async calls.
    let p = js
        .eval_value("(async () => (await twice(1)) + (await twice(2)) + (await boom(0)))()")
        .unwrap();
    assert_eq!(js.block_on::<i32>(&p).unwrap(), 6);
}

struct DropFlag(Rc<Cell<bool>>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
fn cancellation_drops_futures_and_rejects_with_abort_error() {
    let mut js = Context::new();
    let dropped = Rc::new(Cell::new(false));
    let d = dropped.clone();
    js.register_async_fn("hang", move |(): ()| {
        let guard = DropFlag(d.clone());
        async move {
            let _g = guard;
            std::future::pending::<()>().await
        }
    })
    .unwrap();
    let p = js.eval_value("hang()").unwrap();
    js.run_until_idle().unwrap();
    assert_eq!(js.pending_tasks(), 1);
    assert!(!dropped.get());
    assert_eq!(js.cancel_pending().unwrap(), 1);
    assert!(dropped.get());
    assert_eq!(js.pending_tasks(), 0);
    let e = js.block_on::<()>(&p).unwrap_err();
    assert!(e.is_js("AbortError"), "{e}");
    // Cancelling with nothing pending is a no-op.
    assert_eq!(js.cancel_pending().unwrap(), 0);
}

#[test]
fn dropping_the_context_drops_pending_futures() {
    let dropped = Rc::new(Cell::new(false));
    {
        let mut js = Context::new();
        let d = dropped.clone();
        js.register_async_fn("hang", move |(): ()| {
            let guard = DropFlag(d.clone());
            async move {
                let _g = guard;
                std::future::pending::<()>().await
            }
        })
        .unwrap();
        js.eval_value("hang()").unwrap();
        js.run_until_idle().unwrap();
        assert!(!dropped.get());
    }
    assert!(dropped.get());
}

#[test]
fn block_on_times_out_and_detects_deadlock() {
    let mut js = Context::new();
    js.register_async_fn("hang", |(): ()| std::future::pending::<()>())
        .unwrap();
    let p = js.eval_value("hang()").unwrap();
    let e = js
        .block_on_within::<()>(&p, Duration::from_millis(30))
        .unwrap_err();
    assert!(matches!(e, Error::TimedOut), "{e}");
    js.cancel_pending().unwrap();
    let never = js.eval_value("new Promise(() => {})").unwrap();
    assert!(matches!(js.block_on::<()>(&never), Err(Error::Deadlock)));
}

#[test]
fn a_future_woken_from_another_thread_completes() {
    use std::sync::{Arc, Mutex};
    use std::task::{Poll, Waker};
    let mut js = Context::new();
    js.register_async_fn("later", |(n,): (i32,)| async move {
        let state: Arc<Mutex<(bool, Option<Waker>)>> = Arc::new(Mutex::new((false, None)));
        let s = state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let mut g = s.lock().unwrap();
            g.0 = true;
            if let Some(w) = g.1.take() {
                w.wake();
            }
        });
        std::future::poll_fn(move |cx| {
            let mut g = state.lock().unwrap();
            if g.0 {
                Poll::Ready(n + 1)
            } else {
                g.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    })
    .unwrap();
    let p = js.eval_value("later(1)").unwrap();
    assert_eq!(
        js.block_on_within::<i32>(&p, Duration::from_secs(10))
            .unwrap(),
        2
    );
}

#[test]
fn errors_carry_name_message_and_stack() {
    let mut js = Context::new();
    let e = js
        .eval::<()>("function f() { throw new TypeError('bad') }\nf()")
        .unwrap_err();
    let x = e.exception().unwrap();
    assert_eq!(x.name, "TypeError");
    assert_eq!(x.message, "bad");
    assert!(e.to_string().starts_with("TypeError: bad"));
    assert!(matches!(
        js.eval::<()>("let = ;").unwrap_err(),
        Error::Js(_)
    ));
}
