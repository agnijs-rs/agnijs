//! Hooks the Host runtime supplies to an engine.
//!
//! The engine never has an event loop, a clock or a module file system of its
//! own: everything it needs from outside arrives through [`Host`]. A host is
//! given to the engine at construction and lives as long as the engine.

use crate::engine::Engine;
use crate::error::Result;
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

    /// Polled periodically while JavaScript runs. Return true to interrupt;
    /// the running call then fails with [`crate::Error::Interrupted`].
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
}

/// The arguments of a host function call.
pub struct CallContext {
    pub realm: RealmId,
    pub this: Value,
    pub args: Vec<Value>,
    /// Set when invoked with `new`.
    pub new_target: Option<Value>,
}

impl CallContext {
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undefined)
    }
}

/// A Rust function callable from JavaScript. It receives the engine so it
/// can call back in (re-entrancy is allowed).
pub type HostFunction = dyn Fn(&mut dyn Engine, CallContext) -> Result<Value>;

/// Receives the JS values a host object holds, so the collector can see them.
pub trait Tracer {
    fn trace(&mut self, value: &Value);
}

/// Rust data stored inside a JS object of a registered host class.
pub trait HostObject: Any {
    /// Report every JS value this object references. Values stored as
    /// [`Value`] handles are roots on their own; reporting them lets an
    /// engine treat them as traced edges instead, so cycles through host
    /// objects can be collected.
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
    /// Called when the wrapper object is collected. Runs as a host task after
    /// collection, never during it.
    pub finalize: Option<Box<dyn Fn(Box<dyn HostObject>)>>,
}

/// Identifies a registered host class.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HostClassId(pub u32);
