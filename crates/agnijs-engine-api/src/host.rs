//! Hooks the embedder (the host) supplies to an engine.
//!
//! The engine never has an event loop, a clock or a module file system of its
//! own: everything it needs from outside arrives through [`Host`]. A host is
//! given to the engine at construction and lives as long as the engine.

use crate::engine::Engine;
use crate::error::Result;
#[cfg(doc)]
use crate::error::Error;
use crate::value::{RealmId, Value};
use std::any::Any;

/// An import attribute (`with { type: "json" }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportAttribute {
    pub key: String,
    pub value: String,
}

/// What the host hands back for a module specifier.
#[derive(Clone, Debug)]
pub enum ModuleSource {
    /// JavaScript module source text.
    Source(String),
    /// JavaScript module source text generated from another language (a
    /// TypeScript transpile, a bundle) with the Source Map that leads back
    /// to the original: as [`ModuleSource::Source`], then as
    /// [`Engine::set_source_map`] under the resolved specifier.
    SourceWithMap { source: String, source_map: String },
    /// A JSON module: the engine parses it and exports the value as default.
    Json(String),
    /// Precompiled bytecode previously produced by [`Engine::compile_module`].
    Bytecode(Vec<u8>),
}

/// Console levels. The engine provides the `console` global and routes every
/// call here; the host decides where messages go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// Promise-rejection tracking (`HostPromiseRejectionTracker`).
#[derive(Clone, Debug)]
pub enum RejectionEvent {
    /// A promise was rejected with no handler attached.
    Reject { promise: Value, reason: Value },
    /// A handler was attached to a promise reported earlier as unhandled.
    Handle { promise: Value },
}

/// Which regular-expression matcher an engine may use. Both give the same
/// results; they differ in worst-case time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RegexMatcher {
    /// The engine picks per pattern and input. Patterns whose backtracking
    /// worst case is exponential or too large for the input run on a
    /// linear-time matcher when the engine has one, so a hostile pattern or
    /// input cannot stall the thread. The default.
    #[default]
    Auto,
    /// Always backtrack (an embedder opt-out). Pathological patterns can
    /// take exponential time; [`Host::interrupt`], which the matcher polls
    /// every 65,536 steps, is the way out.
    BacktrackingOnly,
    /// Run every pattern the linear-time matcher supports on it, whatever
    /// its cost estimate: for hosts that take untrusted patterns and want
    /// no estimate in the way, and for measuring the matchers. Patterns it
    /// cannot run (backreferences, lookaround) still backtrack.
    LinearWhenEligible,
}

/// Hooks the engine calls. Every method has a default so hosts implement only
/// what they use; the conformance kit's host implements all of them.
pub trait Host {
    /// Resolve `specifier` as imported from `referrer` (None for a root load)
    /// into the canonical specifier the loader will receive.
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> std::result::Result<String, String> {
        let _ = (referrer, attributes);
        Ok(specifier.to_string())
    }

    /// Load the module behind a resolved specifier.
    fn load_module(&self, resolved: &str) -> std::result::Result<ModuleSource, String> {
        Err(format!("module not found: {resolved}"))
    }

    /// Populate `import.meta` for a module (`HostGetImportMetaProperties`).
    fn import_meta(
        &self,
        engine: &mut dyn Engine,
        realm: RealmId,
        module: &str,
        meta: &Value,
    ) -> Result<()> {
        let _ = (engine, realm, module, meta);
        Ok(())
    }

    /// `HostPromiseRejectionTracker`.
    fn promise_rejection(&self, engine: &mut dyn Engine, realm: RealmId, event: RejectionEvent) {
        let _ = (engine, realm, event);
    }

    /// `HostEnsureCanCompileStrings`: whether `eval` and `new Function` may
    /// compile source text. Default allows.
    fn can_compile_strings(&self, realm: RealmId) -> bool {
        let _ = realm;
        true
    }

    /// A `console` call. Arguments are already converted to display strings.
    fn console(&self, realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        let _ = (realm, level, args);
    }

    /// Polled periodically while JavaScript runs: at function entries,
    /// loop back-edges and call returns, every 65,536 steps of a regex
    /// match, and in native built-ins that loop over their input. Return
    /// true to interrupt; the running call then fails with
    /// [`crate::Error::Interrupted`].
    ///
    /// The interrupt is uncatchable: no `catch` or `finally` in the script
    /// runs. The engine stays usable afterwards; keep the hook cheap, since
    /// it is called often.
    ///
    /// ```
    /// use agnijs::api::{Engine, Error, Host, Value};
    /// use std::cell::Cell;
    /// use std::rc::Rc;
    ///
    /// /// Stops a run after a fixed number of polls. A real host would check
    /// /// a deadline or a flag set from another thread.
    /// struct Budget(Cell<u32>);
    /// impl Host for Budget {
    ///     fn interrupt(&self) -> bool {
    ///         let left = self.0.get();
    ///         self.0.set(left.saturating_sub(1));
    ///         left == 0
    ///     }
    /// }
    ///
    /// let host = Rc::new(Budget(Cell::new(10_000)));
    /// let mut engine = agnijs::new_engine(host.clone());
    /// let realm = engine.main_realm();
    ///
    /// let run = engine.eval_script(
    ///     realm,
    ///     "spin.js",
    ///     "try { for (;;) {} } catch (e) { globalThis.caught = true } finally { globalThis.cleaned = true }",
    /// );
    /// assert!(matches!(run, Err(Error::Interrupted)));
    ///
    /// host.0.set(10_000);
    /// let seen = engine.eval_script(realm, "after.js", "typeof caught + ',' + typeof cleaned")?;
    /// assert_eq!(seen, Value::string("undefined,undefined"));
    /// # Ok::<(), Error>(())
    /// ```
    fn interrupt(&self) -> bool {
        false
    }

    /// The clock behind `Date.now()` and `new Date()`: milliseconds since
    /// the Unix epoch. The default reads the system clock; a host may
    /// supply a frozen or scaled clock for tests and replay.
    fn now_ms(&self) -> f64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_millis() as f64).floor(),
            Err(e) => -((e.duration().as_millis() as f64).floor()),
        }
    }

    /// The seed behind `Math.random`, read once per engine, at its first
    /// draw. The default mixes in the system clock, so sequences differ
    /// between runs; a host that needs reproducible runs (tests, replay,
    /// deterministic mode, docs/determinism.md) returns a fixed value.
    /// Every `u64`, zero included, is a valid seed.
    fn random_seed(&self) -> u64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // Distinct engines created within one clock tick still differ.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    /// The local time zone offset from UTC in milliseconds (positive east
    /// of Greenwich) in effect at `epoch_ms`. When `is_utc` is false the
    /// instant is given in local time (the spec's `UTC(t)` direction). The
    /// engine has no time zone database of its own; the default is UTC
    /// and the host that knows the platform supplies the real zone.
    fn time_zone_offset_ms(&self, epoch_ms: f64, is_utc: bool) -> f64 {
        let _ = (epoch_ms, is_utc);
        0.0
    }

    /// The name `Date.prototype.toString` shows in parentheses for the zone
    /// in effect at `epoch_ms`.
    fn time_zone_name(&self, epoch_ms: f64) -> String {
        let _ = epoch_ms;
        "Coordinated Universal Time".to_string()
    }

    /// DefaultLocale for ECMA-402: the BCP 47 tag the `Intl` constructors
    /// fall back to. The engine canonicalizes it and substitutes `en-US`
    /// when the tag is invalid or has no locale data.
    fn default_locale(&self) -> String {
        "en-US".to_string()
    }

    /// DefaultTimeZone for ECMA-402: the IANA zone `Intl.DateTimeFormat`
    /// uses when none is given. Unknown names fall back to `UTC`.
    fn default_time_zone(&self) -> String {
        "UTC".to_string()
    }

    /// The regular-expression matcher policy. Read once, when the engine
    /// is created, and fixed for that engine's lifetime; other engines in
    /// the process are unaffected. Engines with a single matcher ignore it.
    fn regex_matcher(&self) -> RegexMatcher {
        RegexMatcher::Auto
    }
}

/// The arguments of a host function call.
///
/// Every heap value here is a fresh handle, rooted for as long as the host
/// keeps it: move or clone one out of the call to use it later.
pub struct CallContext {
    /// The realm the function was created in (its `[[Realm]]`), whichever
    /// realm the caller runs in. Create new values in it.
    pub realm: RealmId,
    /// The `this` value exactly as the caller passed it: `undefined` for a
    /// plain call, a primitive unboxed.
    pub this: Value,
    /// The arguments the caller passed, as many as it passed.
    pub args: Vec<Value>,
    /// Set when invoked with `new`. Functions from
    /// [`Engine::new_function`] are not constructors, so on AgniJS this is
    /// always `None`; a class built on a host function forwards
    /// `new.target` itself (as the `agnijs` crate's host classes do).
    pub new_target: Option<Value>,
}

impl CallContext {
    /// Argument `i`, or `undefined` when the caller passed fewer.
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undefined)
    }
}

/// A Rust function callable from JavaScript, made into a function object by
/// [`Engine::new_function`].
///
/// # Contract
///
/// `crates/agnijs/tests/host_contract.rs` tests each rule on AgniJS.
///
/// - **Arguments** arrive as [`Value`]s in a [`CallContext`]: primitives
///   copied, objects and symbols as fresh handles. No conversion or arity
///   check is done; [`CallContext::arg`] reads a missing argument as
///   `undefined`.
/// - **The return value** becomes the call's result. A handle from another
///   engine there throws a `TypeError`.
/// - **Errors** returned as `Err` reach JavaScript as follows:
///   - [`Error::Exception`]: its `value` is thrown unchanged (any value, not
///     only an `Error` object; build one with [`Engine::new_error`]). The
///     `name`, `message` and `stack` fields are ignored on the way in.
///   - [`Error::Interrupted`], [`Error::OutOfMemory`] and
///     [`Error::OutOfFuel`]: the run stops. JavaScript cannot catch them,
///     no `catch` or `finally` runs, and the outermost engine call returns
///     the same error.
///   - [`Error::Host`] and [`Error::Internal`]: a catchable `Error` with the
///     message.
///   - [`Error::ForeignHandle`], [`Error::UnknownRealm`], the bytecode
///     errors and [`Error::Unsupported`]: a catchable `TypeError`.
///   - Any other: a catchable `Error` with the error's text.
///
///   So `?` on an engine call inside a host function rethrows what
///   JavaScript threw, unchanged, and passes the uncatchable stops through.
/// - **Panics** are not caught: one unwinds through the engine to the
///   embedder, and the engine is poisoned ([`Engine::is_poisoned`]).
/// - **Re-entrancy is allowed.** The function gets `&mut dyn Engine` and
///   may call any method on it: call JavaScript ([`Engine::call`],
///   [`Engine::eval_script`]), create values and host objects, run
///   [`Engine::run_jobs`] (which drains the queue then and there), and
///   nest host functions to any depth the stack budget allows
///   ([`Engine::set_stack_budget`]). [`Engine::collect_garbage`] only
///   requests a collection while JavaScript frames are live. Exceptions
///   cross nested host frames as `Err` values, so a host function can also
///   catch one with `match` and carry on.
/// - **The function is `Fn`, not `FnMut`,** because a re-entrant call can
///   run it again while it runs; keep mutable state in a `Cell` or
///   `RefCell` and do not hold a `RefCell` borrow across a call back into
///   the engine.
pub type HostFunction = dyn Fn(&mut dyn Engine, CallContext) -> Result<Value>;

/// Receives the JS values a host object holds, so the collector can see them.
///
/// Passed to [`HostObject::trace`]; call [`Tracer::trace`] once per held
/// value. Primitives may be reported too and are ignored.
pub trait Tracer {
    fn trace(&mut self, value: &Value);
}

/// Rust data stored inside a JS object of a registered host class.
///
/// # Contract
///
/// - **Ownership.** The engine owns the data from
///   [`Engine::new_host_object`] until the wrapper object is collected.
///   [`Engine::host_object_data`] borrows it in between.
/// - **Values it holds.** A [`Value`] handle stored in the data is a root
///   of its own: what it names stays alive as long as the data holds it,
///   whether or not JavaScript can reach it, and is released when the data
///   is dropped. A handle back to the wrapper itself, or to anything that
///   reaches the wrapper, is a cycle the collector cannot see, so the
///   wrapper is never collected while the engine lives. Store such a
///   reference as a JavaScript property of the wrapper instead, or drop the
///   handle when done.
/// - **Tracing** ([`HostObject::trace`]) is advisory: an engine may use it
///   to treat reported handles as edges instead of roots, which would let
///   such cycles be collected. AgniJS does not call it today, so its
///   behaviour is the root rule above; report every held value anyway, so
///   the data is ready for an engine that does.
/// - **Finalization.** When the collector finds the wrapper dead, it hands
///   the data to the class's [`HostClassDef::finalize`], never during the
///   collection itself: the call is queued and runs at the next
///   [`Engine::run_jobs`]. The finalizer receives the data by value and no
///   engine, so it cannot call back in; when it returns, the data (and the
///   handles in it) are dropped. Without a finalizer the data is just
///   dropped then. Data still alive when the engine is dropped is dropped
///   with it, and no finalizer runs.
///
/// `crates/agnijs/tests/host_contract.rs` tests each rule on AgniJS.
pub trait HostObject: Any {
    /// Report every JS value this object references. Values stored as
    /// [`Value`] handles are roots on their own; reporting them lets an
    /// engine treat them as traced edges instead, so cycles through host
    /// objects can be collected. AgniJS does not call this today (see the
    /// contract above).
    fn trace(&self, tracer: &mut dyn Tracer) {
        let _ = tracer;
    }
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A host class registered with [`Engine::register_host_class`].
pub struct HostClassDef {
    /// Shown by `Object.prototype.toString` and in debugger output.
    pub name: String,
    /// Called with the data when the wrapper object is collected. Runs from
    /// the next [`Engine::run_jobs`] after the collection, never during it,
    /// and not at all for data still alive when the engine is dropped.
    pub finalize: Option<Box<dyn Fn(Box<dyn HostObject>)>>,
}

/// Identifies a registered host class.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HostClassId(pub u32);
