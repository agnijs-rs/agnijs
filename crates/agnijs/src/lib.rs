//! Embed the AgniJS JavaScript engine in a Rust program.
//!
//! ```
//! let mut js = agnijs::Context::new();
//! let sum: i32 = js.eval("[1, 2, 3].reduce((a, b) => a + b)").unwrap();
//! assert_eq!(sum, 6);
//! ```
//!
//! This crate is a thin layer over the engine interface
//! (`agnijs-engine-api`, ADR 0006). Everything it does can be done with that
//! interface directly; this is the shortest path for the common cases:
//!
//! - **Evaluate and call**: [`Context::eval`], [`Context::call`],
//!   [`Context::set_global`], [`Context::get_global`].
//! - **serde interop**: any `Serialize` type goes in, any `DeserializeOwned`
//!   type comes out; see [`to_value`] and [`from_value`] for the mapping.
//! - **Rust functions**: [`Context::register_fn`] and friends.
//! - **Host classes**: `#[derive(HostClass)]` plus `#[host_methods]`.
//! - **Async**: [`Context::register_async_fn`] exposes a Rust future as a
//!   promise-returning function; [`Context::block_on`] awaits a JavaScript
//!   promise from Rust.
//! - **Typed errors**: [`Error`] for what reaches Rust, [`JsError`] for what
//!   Rust throws into JavaScript.
//!
//! The cookbook (`docs/embedding-cookbook.md`) walks through each; every
//! recipe in it is a test in this crate.
//!
//! A [`Context`] is single-threaded (`!Send`), like the engine it owns.

mod asynch;
mod class;
mod convert;
mod error;

pub use agnijs_derive::{host_methods, HostClass};
pub use agnijs_engine_api as api;
pub use agnijs_engine_api::{Host, HostObject, Value};
pub use class::{ClassBuilder, HostClass, HostClassMethods};
pub use convert::{from_value, to_value};
pub use error::{Error, JsError, JsException, Result};
/// The JSON value type constructors and methods exchange arguments in.
pub use serde_json::Value as Json;

#[doc(hidden)]
pub use class::private as __private;

use agnijs_engine_api::{
    CallContext, ConsoleLevel, Engine, Exception, HostClassId, PropertyKey, RealmId,
};
use serde::{de::DeserializeOwned, Serialize};
use std::any::TypeId;
use std::collections::HashMap;
use std::rc::Rc;

/// The default host: `console` writes to stdout (`log`, `info`, `debug`)
/// and stderr (`warn`, `error`); everything else keeps the engine's
/// defaults (UTC, `en-US`, the system clock, no modules).
struct DefaultHost;

impl Host for DefaultHost {
    fn console(&self, _realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        let line = args.join(" ");
        match level {
            ConsoleLevel::Warn | ConsoleLevel::Error => eprintln!("{line}"),
            _ => println!("{line}"),
        }
    }
}

/// Turn a [`JsError`] into the error an engine host function returns.
pub(crate) fn throw(engine: &mut dyn Engine, realm: RealmId, e: &JsError) -> api::Error {
    match engine.new_error(realm, e.kind, &e.message) {
        Ok(value) => api::Error::Exception(Exception {
            value,
            name: e.kind.name().to_string(),
            message: e.message.clone(),
            stack: Vec::new(),
        }),
        Err(other) => other,
    }
}

/// An AgniJS engine with one realm, plus the executor that runs async host
/// functions.
pub struct Context {
    pub(crate) engine: Box<dyn Engine>,
    pub(crate) realm: RealmId,
    pub(crate) exec: Rc<asynch::Executor>,
    pub(crate) classes: HashMap<TypeId, (HostClassId, Value)>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// A fresh engine with the default host (console to stdout/stderr).
    pub fn new() -> Context {
        Self::with_host(Rc::new(DefaultHost))
    }

    /// A fresh engine with your own [`Host`]: module loading, console
    /// sink, clock, interrupt polling and so on.
    pub fn with_host(host: Rc<dyn Host>) -> Context {
        let engine = agnijs_js_vm::make_engine(host);
        let realm = engine.main_realm();
        Context {
            engine,
            realm,
            exec: asynch::Executor::new(),
            classes: HashMap::new(),
        }
    }

    /// The engine interface, for anything this crate does not wrap.
    pub fn engine(&mut self) -> &mut dyn Engine {
        &mut *self.engine
    }

    /// The realm every method here works in.
    pub fn realm(&self) -> RealmId {
        self.realm
    }

    /// [`to_value`] in this context.
    pub fn to_js<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<Value> {
        to_value(&mut *self.engine, self.realm, value)
    }

    /// [`from_value`] in this context.
    pub fn from_js<T: DeserializeOwned>(&mut self, value: &Value) -> Result<T> {
        from_value(&mut *self.engine, self.realm, value)
    }

    /// Evaluate a script and convert its completion value.
    pub fn eval<T: DeserializeOwned>(&mut self, source: &str) -> Result<T> {
        let v = self.eval_value(source)?;
        from_value(&mut *self.engine, self.realm, &v)
    }

    /// Evaluate a script and keep the raw [`Value`] (a promise, a function,
    /// a host object).
    pub fn eval_value(&mut self, source: &str) -> Result<Value> {
        self.eval_named("<eval>", source)
    }

    /// [`Context::eval_value`] with a script name for stack traces.
    pub fn eval_named(&mut self, name: &str, source: &str) -> Result<Value> {
        Ok(self.engine.eval_script(self.realm, name, source)?)
    }

    /// Convert a Rust value and bind it to a global.
    pub fn set_global<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<()> {
        let v = to_value(&mut *self.engine, self.realm, value)?;
        let global = self.engine.global_object(self.realm)?;
        self.engine
            .set(self.realm, &global, &PropertyKey::from(name), &v)?;
        Ok(())
    }

    /// Read a global and convert it.
    pub fn get_global<T: DeserializeOwned>(&mut self, name: &str) -> Result<T> {
        let global = self.engine.global_object(self.realm)?;
        let v = self
            .engine
            .get(self.realm, &global, &PropertyKey::from(name))?;
        from_value(&mut *self.engine, self.realm, &v)
    }

    /// Call a global function. `args` is spread when it serializes to an
    /// array (a tuple, a `Vec`, an array), is no argument for `()`, and is
    /// the single argument otherwise.
    pub fn call<A: Serialize, R: DeserializeOwned>(
        &mut self,
        function: &str,
        args: A,
    ) -> Result<R> {
        let v = self.call_value(function, args)?;
        from_value(&mut *self.engine, self.realm, &v)
    }

    /// [`Context::call`] keeping the raw result (for a promise to
    /// [`Context::block_on`]).
    pub fn call_value<A: Serialize>(&mut self, function: &str, args: A) -> Result<Value> {
        let json = serde_json::to_value(args).map_err(|e| Error::Convert(e.to_string()))?;
        let items = match json {
            Json::Array(items) => items,
            Json::Null => Vec::new(),
            single => vec![single],
        };
        let mut vals = Vec::with_capacity(items.len());
        for i in &items {
            vals.push(convert::json_to_js(&mut *self.engine, self.realm, i)?);
        }
        let global = self.engine.global_object(self.realm)?;
        let f = self
            .engine
            .get(self.realm, &global, &PropertyKey::from(function))?;
        Ok(self.engine.call(self.realm, &f, &Value::Undefined, &vals)?)
    }

    /// Expose a Rust function to JavaScript as a global. Arguments decode
    /// as `A` (a tuple: `(i32, String)`, `(Option<f64>,)`, `()`), the
    /// return value is serialized. Undecodable arguments throw a
    /// `TypeError`.
    pub fn register_fn<A, R>(&mut self, name: &str, f: impl Fn(A) -> R + 'static) -> Result<()>
    where
        A: DeserializeOwned,
        R: Serialize,
    {
        self.register_try_fn(name, move |a: A| Ok::<R, JsError>(f(a)))
    }

    /// Like [`Context::register_fn`] for a function that can fail: `Err`
    /// is thrown as an `Error` of the kind in the [`JsError`].
    pub fn register_try_fn<A, R, E>(
        &mut self,
        name: &str,
        f: impl Fn(A) -> std::result::Result<R, E> + 'static,
    ) -> Result<()>
    where
        A: DeserializeOwned,
        R: Serialize,
        E: Into<JsError>,
    {
        let func = self.engine.new_function(
            self.realm,
            name,
            Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                let run = |engine: &mut dyn Engine| -> std::result::Result<Json, JsError> {
                    let mut args = Vec::with_capacity(cx.args.len());
                    for a in &cx.args {
                        args.push(
                            convert::js_to_json(engine, cx.realm, a)
                                .map_err(|e| JsError::type_error(e.to_string()))?,
                        );
                    }
                    let a: A = convert::decode_args(args)
                        .map_err(|e| JsError::type_error(format!("arguments: {e}")))?;
                    let r = f(a).map_err(Into::into)?;
                    serde_json::to_value(r).map_err(|e| JsError::type_error(e.to_string()))
                };
                match run(engine) {
                    Ok(json) => convert::json_to_js(engine, cx.realm, &json).map_err(|e| match e {
                        Error::Engine(e) => e,
                        other => throw(engine, cx.realm, &JsError::type_error(other.to_string())),
                    }),
                    Err(e) => Err(throw(engine, cx.realm, &e)),
                }
            }),
        )?;
        let global = self.engine.global_object(self.realm)?;
        self.engine
            .set(self.realm, &global, &PropertyKey::from(name), &func)?;
        Ok(())
    }

    /// Run the engine's pending promise jobs (microtasks) to completion.
    pub fn run_jobs(&mut self) -> Result<usize> {
        Ok(self.engine.run_jobs(self.realm)?)
    }
}

/// The cookbook's code blocks, compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../../docs/embedding-cookbook.md")]
struct Cookbook;
