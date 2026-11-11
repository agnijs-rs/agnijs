//! Rust futures as JavaScript promises, and JavaScript promises awaited from
//! Rust.
//!
//! The engine has no event loop (ADR 0006): the embedder drives it. This
//! module is a small single-threaded executor that lives inside [`Context`]:
//! an async host function returns a promise at once and parks its future
//! here; [`Context::turn`] polls ready futures, settles their promises and
//! runs the engine's jobs. Futures need not be `Send` and never see the
//! engine, so they cannot re-enter it. Wakers are thread-safe, so a future
//! may be woken from another thread (a timer, a socket reactor).

use crate::error::{Error, JsError, JsException, Result};
use crate::{convert, Context};
use agnijs_engine_api::{CallContext, Engine, ErrorKind, PromiseState, PropertyKey, Value};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value as Json;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskCx, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

pub(crate) type BoxFuture = Pin<Box<dyn Future<Output = std::result::Result<Json, JsError>>>>;

struct Flag {
    ready: AtomicBool,
    thread: Thread,
}

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

struct Task {
    fut: BoxFuture,
    flag: Arc<Flag>,
    resolve: Value,
    reject: Value,
}

pub(crate) struct Executor {
    tasks: RefCell<Vec<Task>>,
    thread: Thread,
    spawned: Cell<u64>,
}

impl Executor {
    pub(crate) fn new() -> Rc<Executor> {
        Rc::new(Executor {
            tasks: RefCell::new(Vec::new()),
            thread: thread::current(),
            spawned: Cell::new(0),
        })
    }
}

/// Make a promise plus its resolve and reject functions, through the
/// engine interface alone.
fn new_promise(
    engine: &mut dyn Engine,
    realm: agnijs_engine_api::RealmId,
) -> agnijs_engine_api::Result<(Value, Value, Value)> {
    let slot: Rc<RefCell<Option<(Value, Value)>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let executor = engine.new_function(
        realm,
        "executor",
        Rc::new(move |_e: &mut dyn Engine, cx: CallContext| {
            *slot2.borrow_mut() = Some((cx.arg(0), cx.arg(1)));
            Ok(Value::Undefined)
        }),
    )?;
    let global = engine.global_object(realm)?;
    let promise_ctor = engine.get(realm, &global, &"Promise".into())?;
    let promise = engine.construct(realm, &promise_ctor, &[executor])?;
    let (resolve, reject) = slot
        .borrow_mut()
        .take()
        .ok_or_else(|| agnijs_engine_api::Error::Internal("Promise executor did not run".into()))?;
    Ok((promise, resolve, reject))
}

fn error_value(
    engine: &mut dyn Engine,
    realm: agnijs_engine_api::RealmId,
    e: &JsError,
) -> agnijs_engine_api::Result<Value> {
    engine.new_error(realm, e.kind, &e.message)
}

impl Context {
    /// Expose `f` to JavaScript as a function returning a promise. The
    /// arguments are decoded as `A` (usually a tuple), the future runs on
    /// this context's executor, and its output resolves the promise. A
    /// bad argument rejects the promise.
    ///
    /// Nothing runs until the context is driven: call [`Context::turn`],
    /// [`Context::run_until_idle`] or [`Context::block_on`].
    pub fn register_async_fn<A, R, Fut>(
        &mut self,
        name: &str,
        f: impl Fn(A) -> Fut + 'static,
    ) -> Result<()>
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        Fut: Future<Output = R> + 'static,
    {
        self.register_async_raw(name, move |args| {
            let a: A = convert::decode_args(args)
                .map_err(|e| JsError::type_error(format!("arguments: {e}")))?;
            let fut = f(a);
            Ok(Box::pin(async move {
                serde_json::to_value(fut.await).map_err(|e| JsError::type_error(e.to_string()))
            }) as BoxFuture)
        })
    }

    /// Like [`Context::register_async_fn`], for a future that can fail: an
    /// `Err` rejects the promise with an `Error` of the given kind.
    pub fn register_try_async_fn<A, R, E, Fut>(
        &mut self,
        name: &str,
        f: impl Fn(A) -> Fut + 'static,
    ) -> Result<()>
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        E: Into<JsError> + 'static,
        Fut: Future<Output = std::result::Result<R, E>> + 'static,
    {
        self.register_async_raw(name, move |args| {
            let a: A = convert::decode_args(args)
                .map_err(|e| JsError::type_error(format!("arguments: {e}")))?;
            let fut = f(a);
            Ok(Box::pin(async move {
                let r = fut.await.map_err(Into::into)?;
                serde_json::to_value(r).map_err(|e| JsError::type_error(e.to_string()))
            }) as BoxFuture)
        })
    }

    fn register_async_raw(
        &mut self,
        name: &str,
        start: impl Fn(Vec<Json>) -> std::result::Result<BoxFuture, JsError> + 'static,
    ) -> Result<()> {
        let exec = self.exec.clone();
        let f = self.engine.new_function(
            self.realm,
            name,
            Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                let (promise, resolve, reject) = new_promise(engine, cx.realm)?;
                let args: std::result::Result<Vec<Json>, Error> = cx
                    .args
                    .iter()
                    .map(|a| convert::js_to_json(engine, cx.realm, a))
                    .collect();
                let fut: BoxFuture = match args
                    .map_err(|e| JsError::type_error(e.to_string()))
                    .and_then(&start)
                {
                    Ok(f) => f,
                    Err(e) => Box::pin(async move { Err(e) }),
                };
                exec.spawned.set(exec.spawned.get() + 1);
                exec.tasks.borrow_mut().push(Task {
                    fut,
                    flag: Arc::new(Flag {
                        ready: AtomicBool::new(true),
                        thread: exec.thread.clone(),
                    }),
                    resolve,
                    reject,
                });
                Ok(promise)
            }),
        )?;
        let global = self.engine.global_object(self.realm)?;
        self.engine
            .set(self.realm, &global, &PropertyKey::from(name), &f)?;
        Ok(())
    }

    /// Futures spawned by async functions that have not finished.
    pub fn pending_tasks(&self) -> usize {
        self.exec.tasks.borrow().len()
    }

    /// One scheduling round: poll every future that was woken, settle the
    /// promises of those that finished, then run the engine's jobs. Returns
    /// whether anything happened.
    pub fn turn(&mut self) -> Result<bool> {
        let mut progressed = false;
        let tasks = std::mem::take(&mut *self.exec.tasks.borrow_mut());
        let mut keep = Vec::with_capacity(tasks.len());
        for mut task in tasks {
            if !task.flag.ready.swap(false, Ordering::AcqRel) {
                keep.push(task);
                continue;
            }
            progressed = true;
            let waker = Waker::from(task.flag.clone());
            let mut cx = TaskCx::from_waker(&waker);
            match task.fut.as_mut().poll(&mut cx) {
                Poll::Pending => keep.push(task),
                Poll::Ready(result) => {
                    let (f, arg) = match result {
                        Ok(json) => (
                            task.resolve.clone(),
                            convert::json_to_js(&mut *self.engine, self.realm, &json)?,
                        ),
                        Err(e) => (
                            task.reject.clone(),
                            error_value(&mut *self.engine, self.realm, &e)?,
                        ),
                    };
                    self.engine
                        .call(self.realm, &f, &Value::Undefined, &[arg])?;
                }
            }
        }
        // Futures cannot spawn, but a settle can run no JS; still, keep any
        // task that was pushed meanwhile.
        let mut slot = self.exec.tasks.borrow_mut();
        keep.append(&mut slot);
        *slot = keep;
        drop(slot);
        if self.engine.has_pending_jobs() {
            self.engine.run_jobs(self.realm)?;
            progressed = true;
        }
        Ok(progressed)
    }

    /// Turn until nothing is ready: every future is finished or waiting for
    /// a wake-up, and the job queue is empty.
    pub fn run_until_idle(&mut self) -> Result<()> {
        while self.turn()? {}
        Ok(())
    }

    /// Reject the promise of every unfinished async call with an
    /// `AbortError` and drop its future (running its destructors first).
    /// Returns how many were cancelled.
    pub fn cancel_pending(&mut self) -> Result<usize> {
        let tasks = std::mem::take(&mut *self.exec.tasks.borrow_mut());
        let n = tasks.len();
        let mut rejects = Vec::with_capacity(n);
        for t in tasks {
            let Task { fut, reject, .. } = t;
            drop(fut);
            rejects.push(reject);
        }
        for reject in rejects {
            let err =
                self.engine
                    .new_error(self.realm, ErrorKind::Error, "the call was cancelled")?;
            self.engine.set(
                self.realm,
                &err,
                &"name".into(),
                &Value::string("AbortError"),
            )?;
            self.engine
                .call(self.realm, &reject, &Value::Undefined, &[err])?;
        }
        self.run_until_idle()?;
        Ok(n)
    }

    /// Wait for `promise` (or return a plain value at once), driving the
    /// executor and parking the thread while futures wait to be woken. A
    /// rejection is an [`Error::Js`]. Never returns if a future that is
    /// never woken is the only thing left; see [`Context::block_on_within`].
    pub fn block_on<T: DeserializeOwned>(&mut self, promise: &Value) -> Result<T> {
        self.wait(promise, None)
    }

    /// [`Context::block_on`] with a time limit ([`Error::TimedOut`]).
    pub fn block_on_within<T: DeserializeOwned>(
        &mut self,
        promise: &Value,
        limit: Duration,
    ) -> Result<T> {
        self.wait(promise, Some(Instant::now() + limit))
    }

    fn wait<T: DeserializeOwned>(
        &mut self,
        promise: &Value,
        deadline: Option<Instant>,
    ) -> Result<T> {
        if !self.engine.is_promise(promise) {
            return convert::from_value(&mut *self.engine, self.realm, promise);
        }
        loop {
            self.run_until_idle()?;
            match self.engine.promise_state(self.realm, promise)? {
                PromiseState::Fulfilled(v) => {
                    return convert::from_value(&mut *self.engine, self.realm, &v)
                }
                PromiseState::Rejected(r) => return Err(self.exception_from(&r)?),
                PromiseState::Pending => {}
            }
            if self.exec.tasks.borrow().is_empty() {
                return Err(Error::Deadlock);
            }
            let any_ready = self
                .exec
                .tasks
                .borrow()
                .iter()
                .any(|t| t.flag.ready.load(Ordering::Acquire));
            if !any_ready {
                match deadline {
                    None => thread::park(),
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            return Err(Error::TimedOut);
                        }
                        thread::park_timeout(d - now);
                    }
                }
            }
            if let Some(d) = deadline {
                if Instant::now() >= d
                    && !self
                        .exec
                        .tasks
                        .borrow()
                        .iter()
                        .any(|t| t.flag.ready.load(Ordering::Acquire))
                {
                    return Err(Error::TimedOut);
                }
            }
        }
    }

    /// Describe a thrown or rejected value as an [`Error::Js`].
    pub(crate) fn exception_from(&mut self, reason: &Value) -> Result<Error> {
        let (name, message) = match reason {
            Value::Object(_) => {
                let n = self.engine.get(self.realm, reason, &"name".into())?;
                let m = self.engine.get(self.realm, reason, &"message".into())?;
                (
                    self.engine.to_string(self.realm, &n)?,
                    self.engine.to_string(self.realm, &m)?,
                )
            }
            other => (
                "Error".to_string(),
                self.engine.to_string(self.realm, other).unwrap_or_default(),
            ),
        };
        Ok(Error::Js(JsException {
            name,
            message,
            stack: Vec::new(),
            value: reason.clone(),
        }))
    }
}
