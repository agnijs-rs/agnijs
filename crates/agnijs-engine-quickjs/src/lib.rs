//! The interim engine: QuickJS-NG (through `rquickjs`) behind the engine
//! interface.
//!
//! Design notes (see ADR 0006):
//!
//! - All state lives in an `Rc<Shared>` with interior mutability, so host
//!   callbacks that run while QuickJS is on the stack can build a fresh
//!   `QuickJsEngine` view and hand the host `&mut dyn Engine` without
//!   aliasing an outer `&mut`.
//! - `rquickjs::Context::with` takes a non-reentrant runtime lock. Inside a
//!   callback a `Ctx` already exists for this thread, so the adapter
//!   re-derives one from the raw context pointer instead of locking again
//!   (`with_ctx`). The thread-local `DEPTH` tracks whether that is the case.
//! - Persistent handles are `rquickjs::Persistent` values in a
//!   `HandleRegistry`; dropping a `Handle` queues the slot, and every entry
//!   point reclaims queued slots before touching QuickJS.
//! - QuickJS-NG has no `HostEnsureCanCompileStrings` hook. When the host
//!   forbids string compilation, the adapter swaps the realm's `eval` and
//!   `Function` for throwing wrappers (so a direct `eval(...)` becomes an
//!   ordinary call to the wrapper) and restores the intrinsics when the host
//!   allows it again.
//!
//! `unsafe` here is FFI into QuickJS for the few operations rquickjs does not
//! expose (bytecode read and write, UTF-16 strings, property enumeration,
//! promise state). Each use states the invariant it relies on.

use agnijs_engine_api::{
    bytecode, next_engine_id, CallContext, ConsoleLevel, Engine, EngineId, EngineInfo, Error,
    ErrorKind, Exception, HandleRegistry, HeapKind, Host, HostClassDef, HostClassId, HostFunction,
    HostObject, ImportAttribute, JsBigInt, JsString, MemoryUsage, ModuleSource, PromiseState,
    PropertyKey, RealmId, RejectionEvent, Result, StackFrame, TypeOf, Value,
};
use rquickjs::class::{JsClass, Trace, Tracer as QjsTracer};
use rquickjs::function::{Rest, This};
use rquickjs::loader::{Loader, Resolver};
use rquickjs::module::{Declared, Module};
use rquickjs::{qjs, Context, Ctx, Function, JsLifetime, Object, Persistent, Runtime};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The bytecode format tag: engine name plus the QuickJS-NG version the
/// bytes came from, since QuickJS bytecode is version-specific.
pub const BYTECODE_FORMAT: &str = "quickjs-ng-0.16";

thread_local! {
    /// > 0 while a `Context::with` (and therefore a live `Ctx`) is on this
    /// thread's stack.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

type PersistentValue = Persistent<rquickjs::Value<'static>>;

/// Field order matters: Rust drops fields in declaration order, and every
/// persistent JS value must be freed before the contexts and the runtime.
struct Shared {
    #[allow(dead_code)]
    id: EngineId,
    handles: RefCell<HandleRegistry<PersistentValue>>,
    /// Saved intrinsics per realm: (eval, Function).
    intrinsics: RefCell<Vec<Option<(PersistentValue, PersistentValue)>>>,
    realms: RefCell<Vec<Rc<Context>>>,
    rt: Runtime,
    host: Rc<dyn Host>,
    sources: RefCell<HashMap<(u32, String), String>>,
    classes: RefCell<Vec<HostClassDef>>,
    /// Host objects whose wrappers were collected; their finalizers run from
    /// `run_jobs`, never during collection.
    finalizers: Rc<RefCell<Vec<(HostClassId, Box<dyn HostObject>)>>>,
    /// Per realm: whether the throwing `eval`/`Function` wrappers are in place.
    compile_blocked: RefCell<Vec<bool>>,
    /// The first error the module loader hit, surfaced after the call.
    loader_error: RefCell<Option<String>>,
}

/// QuickJS-NG behind the engine interface.
pub struct QuickJsEngine {
    shared: Rc<Shared>,
}

// ---- host object wrapper class ----------------------------------------

/// Every host object is an instance of this one QuickJS class; the host
/// class id and `Symbol.toStringTag` distinguish host classes.
struct HostCell {
    class: HostClassId,
    data: Option<Box<dyn HostObject>>,
    finalizers: Weak<RefCell<Vec<(HostClassId, Box<dyn HostObject>)>>>,
}

impl Drop for HostCell {
    fn drop(&mut self) {
        if let (Some(data), Some(queue)) = (self.data.take(), self.finalizers.upgrade()) {
            queue.borrow_mut().push((self.class, data));
        }
    }
}

// SAFETY (JsLifetime): HostCell holds no JS values, so it is the same type
// under every lifetime.
unsafe impl<'js> JsLifetime<'js> for HostCell {
    type Changed<'to> = HostCell;
}

impl<'js> Trace<'js> for HostCell {
    fn trace<'a>(&self, _tracer: QjsTracer<'a, 'js>) {}
}

impl<'js> JsClass<'js> for HostCell {
    const NAME: &'static str = "HostObject";
    type Mutable = rquickjs::class::Writable;
    fn prototype(_ctx: &Ctx<'js>) -> rquickjs::Result<Option<Object<'js>>> {
        Ok(None)
    }
    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }
}

// ---- module loading -----------------------------------------------------

struct HostResolver {
    shared: Weak<Shared>,
}

impl Resolver for HostResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        let shared = self.shared.upgrade().ok_or(rquickjs::Error::Unknown)?;
        let mut attrs = Vec::new();
        if let Some(a) = attributes {
            for key in a.keys().flatten() {
                if let Ok(Some(value)) = a.get(&key) {
                    attrs.push(ImportAttribute { key, value });
                }
            }
        }
        let referrer = if base.is_empty() || base == ROOT_REFERRER {
            None
        } else {
            Some(base)
        };
        match shared.host.resolve_module(referrer, name, &attrs) {
            Ok(resolved) => Ok(resolved),
            Err(message) => {
                *shared.loader_error.borrow_mut() = Some(message.clone());
                Err(rquickjs::Error::new_resolving_message(base, name, message))
            }
        }
    }
}

struct HostLoader {
    shared: Weak<Shared>,
}

impl Loader for HostLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        let shared = self.shared.upgrade().ok_or(rquickjs::Error::Unknown)?;
        let source = match shared.host.load_module(name) {
            Ok(s) => s,
            Err(message) => {
                *shared.loader_error.borrow_mut() = Some(message.clone());
                return Err(rquickjs::Error::new_loading_message(name, message));
            }
        };
        let realm = realm_of(&shared, ctx);
        let module = match source {
            // QuickJS has no source maps: the map is dropped.
            ModuleSource::Source(text) | ModuleSource::SourceWithMap { source: text, .. } => {
                shared
                    .sources
                    .borrow_mut()
                    .insert((realm.0, name.to_string()), text.clone());
                Module::declare(ctx.clone(), name, text)?
            }
            ModuleSource::Json(json) => {
                // JSON is a subset of JavaScript expression syntax since
                // ES2019, so a default export of the literal is a faithful
                // JSON module.
                Module::declare(ctx.clone(), name, format!("export default {json};"))?
            }
            ModuleSource::Bytecode(bytes) => {
                let payload = bytecode::unwrap(BYTECODE_FORMAT, &bytes).map_err(|e| {
                    *shared.loader_error.borrow_mut() = Some(e.to_string());
                    rquickjs::Error::new_loading_message(name, e.to_string())
                })?;
                // SAFETY: the payload was produced by `compile_module` on
                // this same QuickJS-NG version, which the format tag checked
                // above guarantees; QuickJS validates the structure itself.
                unsafe { Module::load(ctx.clone(), payload)? }
            }
        };
        // import.meta: let the host populate it now, while the module is
        // declared but not yet evaluated.
        let meta = module.meta()?;
        let meta_value = {
            let mut engine = QuickJsEngine {
                shared: shared.clone(),
            };
            let meta_api = engine.to_api(ctx, meta.into_value());
            let r = shared.host.import_meta(&mut engine, realm, name, &meta_api);
            if let Err(e) = r {
                *shared.loader_error.borrow_mut() = Some(e.to_string());
                return Err(rquickjs::Error::new_loading_message(name, e.to_string()));
            }
            meta_api
        };
        drop(meta_value);
        Ok(module)
    }
}

/// The referrer QuickJS reports for a root import through `Module::import`.
const ROOT_REFERRER: &str = "<root>";

fn realm_of(shared: &Shared, ctx: &Ctx<'_>) -> RealmId {
    let raw = ctx.as_raw();
    let realms = shared.realms.borrow();
    for (i, c) in realms.iter().enumerate() {
        if c.as_raw() == raw {
            return RealmId(i as u32);
        }
    }
    RealmId(0)
}

// ---- engine -------------------------------------------------------------

impl QuickJsEngine {
    /// Build an engine with one realm around `host`.
    pub fn new(host: Rc<dyn Host>) -> Result<QuickJsEngine> {
        let rt = Runtime::new().map_err(|e| Error::Internal(e.to_string()))?;
        let id = next_engine_id();
        let shared = Rc::new(Shared {
            id,
            rt,
            realms: RefCell::new(Vec::new()),
            handles: RefCell::new(HandleRegistry::new(id)),
            host,
            sources: RefCell::new(HashMap::new()),
            classes: RefCell::new(Vec::new()),
            finalizers: Rc::new(RefCell::new(Vec::new())),
            compile_blocked: RefCell::new(Vec::new()),
            intrinsics: RefCell::new(Vec::new()),
            loader_error: RefCell::new(None),
        });
        let weak = Rc::downgrade(&shared);
        shared.rt.set_loader(
            HostResolver {
                shared: weak.clone(),
            },
            HostLoader {
                shared: weak.clone(),
            },
        );
        {
            let weak = weak.clone();
            shared.rt.set_interrupt_handler(Some(Box::new(move || {
                weak.upgrade().map(|s| s.host.interrupt()).unwrap_or(false)
            })));
        }
        {
            let weak = weak.clone();
            shared.rt.set_host_promise_rejection_tracker(Some(Box::new(
                move |ctx: Ctx<'_>,
                      promise: rquickjs::Value<'_>,
                      reason: rquickjs::Value<'_>,
                      handled: bool| {
                    let Some(shared) = weak.upgrade() else { return };
                    let realm = realm_of(&shared, &ctx);
                    let mut engine = QuickJsEngine {
                        shared: shared.clone(),
                    };
                    let promise = engine.to_api(&ctx, promise);
                    let event = if handled {
                        RejectionEvent::Handle { promise }
                    } else {
                        let reason = engine.to_api(&ctx, reason);
                        RejectionEvent::Reject { promise, reason }
                    };
                    shared.host.promise_rejection(&mut engine, realm, event);
                },
            )));
        }
        let mut engine = QuickJsEngine { shared };
        engine.create_realm()?;
        Ok(engine)
    }

    fn context(&self, realm: RealmId) -> Result<Rc<Context>> {
        self.shared
            .realms
            .borrow()
            .get(realm.0 as usize)
            .cloned()
            .ok_or(Error::UnknownRealm)
    }

    /// Run `f` with a `Ctx` for `realm`, re-entrantly.
    fn with_ctx<R>(&self, realm: RealmId, f: impl FnOnce(&Ctx<'_>) -> R) -> Result<R> {
        let context = self.context(realm)?;
        if DEPTH.with(|d| d.get()) > 0 {
            // SAFETY: DEPTH > 0 means a `Context::with` on this thread holds
            // the runtime lock for this runtime (all realms share it), and
            // `context` is kept alive by the Rc clone above for the duration
            // of `f`. QuickJS contexts are not thread-affine beyond the
            // runtime lock, so deriving a second `Ctx` to the same raw
            // context is sound as long as no `with` is attempted (it is not).
            let ctx = unsafe { Ctx::from_raw(context.as_raw()) };
            Ok(f(&ctx))
        } else {
            DEPTH.with(|d| d.set(1));
            let r = context.with(|ctx| f(&ctx));
            DEPTH.with(|d| d.set(0));
            Ok(r)
        }
    }

    /// Entry-point housekeeping: free dropped handles, sync the
    /// compile-strings switch.
    fn enter(&mut self, realm: RealmId) -> Result<()> {
        self.reclaim_handles();
        let allowed = self.shared.host.can_compile_strings(realm);
        let blocked = self
            .shared
            .compile_blocked
            .borrow()
            .get(realm.0 as usize)
            .copied()
            .unwrap_or(false);
        if allowed == blocked {
            self.set_compile_blocked(realm, !allowed)?;
        }
        Ok(())
    }

    fn set_compile_blocked(&mut self, realm: RealmId, blocked: bool) -> Result<()> {
        let shared = self.shared.clone();
        self.with_ctx(realm, |ctx| -> rquickjs::Result<()> {
            let globals = ctx.globals();
            let mut intrinsics = shared.intrinsics.borrow_mut();
            let slot = &mut intrinsics[realm.0 as usize];
            if blocked {
                let eval: rquickjs::Value = globals.get("eval")?;
                let function: rquickjs::Value = globals.get("Function")?;
                if slot.is_none() {
                    *slot = Some((
                        Persistent::save(ctx, eval.clone()),
                        Persistent::save(ctx, function.clone()),
                    ));
                }
                let throwing_eval = Function::new(ctx.clone(), throwing_eval)?.with_name("eval")?;
                let throwing_function =
                    Function::new(ctx.clone(), throwing_function)?.with_name("Function")?;
                throwing_function.set_constructor(true);
                if let Some(proto) = function
                    .as_object()
                    .and_then(|f| f.get::<_, Object>("prototype").ok())
                {
                    throwing_function.set("prototype", proto)?;
                }
                globals.set("eval", throwing_eval)?;
                globals.set("Function", throwing_function)?;
            } else if let Some((eval, function)) = slot.clone() {
                globals.set("eval", eval.restore(ctx)?)?;
                globals.set("Function", function.restore(ctx)?)?;
            }
            Ok(())
        })?
        .map_err(|e| Error::Internal(e.to_string()))?;
        self.shared.compile_blocked.borrow_mut()[realm.0 as usize] = blocked;
        Ok(())
    }

    // ---- value conversion ---------------------------------------------

    fn to_api<'js>(&mut self, ctx: &Ctx<'js>, v: rquickjs::Value<'js>) -> Value {
        if v.is_undefined() {
            return Value::Undefined;
        }
        if v.is_null() {
            return Value::Null;
        }
        if let Some(b) = v.as_bool() {
            return Value::Bool(b);
        }
        if let Some(i) = v.as_int() {
            return Value::Number(i as f64);
        }
        if let Some(f) = v.as_float() {
            return Value::Number(f);
        }
        if v.is_string() {
            return Value::String(js_string_of(ctx, &v));
        }
        if v.is_big_int() {
            let s = unsafe_to_string(ctx, &v).unwrap_or_default();
            return Value::BigInt(JsBigInt(s));
        }
        let kind = if v.is_symbol() {
            HeapKind::Symbol
        } else {
            HeapKind::Object
        };
        let handle = self
            .shared
            .handles
            .borrow_mut()
            .insert(kind, Persistent::save(ctx, v));
        match kind {
            HeapKind::Symbol => Value::Symbol(handle),
            HeapKind::Object => Value::Object(handle),
        }
    }

    fn to_js<'js>(&self, ctx: &Ctx<'js>, v: &Value) -> Result<rquickjs::Value<'js>> {
        Ok(match v {
            Value::Undefined => rquickjs::Value::new_undefined(ctx.clone()),
            Value::Null => rquickjs::Value::new_null(ctx.clone()),
            Value::Bool(b) => rquickjs::Value::new_bool(ctx.clone(), *b),
            Value::Number(n) => rquickjs::Value::new_number(ctx.clone(), *n),
            Value::String(s) => new_js_string(ctx, s.units()),
            Value::BigInt(b) => {
                let src = format!("{}n", b.0);
                ctx.eval::<rquickjs::Value, _>(src)
                    .map_err(|e| Error::Internal(e.to_string()))?
            }
            Value::Symbol(h) | Value::Object(h) => {
                let handles = self.shared.handles.borrow();
                let p = handles.get(h).ok_or(Error::ForeignHandle)?.clone();
                drop(handles);
                p.restore(ctx).map_err(|_| Error::ForeignHandle)?
            }
        })
    }

    fn key_to_js<'js>(&self, ctx: &Ctx<'js>, key: &PropertyKey) -> Result<rquickjs::Atom<'js>> {
        let v = match key {
            PropertyKey::String(s) => new_js_string(ctx, s.units()),
            PropertyKey::Symbol(h) => self.to_js(ctx, &Value::Symbol(h.clone()))?,
        };
        rquickjs::Atom::from_value(ctx.clone(), &v).map_err(|e| Error::Internal(e.to_string()))
    }

    /// Turn an rquickjs error (usually "an exception is pending") into the
    /// interface error, pulling the pending exception out of the context.
    fn convert_error(&mut self, ctx: &Ctx<'_>, err: rquickjs::Error) -> Error {
        match err {
            rquickjs::Error::Exception => {
                let exc = ctx.catch();
                // QuickJS throws a bare null when it cannot even allocate
                // the out-of-memory error object.
                if exc.is_null() {
                    return Error::OutOfMemory;
                }
                if exc.is_uncatchable_error() || is_interrupt_or_oom(ctx, &exc) {
                    let msg = exception_message(ctx, &exc).to_lowercase();
                    if msg.contains("out of memory") {
                        return Error::OutOfMemory;
                    }
                    return Error::Interrupted;
                }
                self.exception_from_value(ctx, exc)
            }
            rquickjs::Error::Resolving { message, .. }
            | rquickjs::Error::Loading { message, .. } => Error::Host(message.unwrap_or_default()),
            other => {
                let loader = self.shared.loader_error.borrow_mut().take();
                match loader {
                    Some(m) => Error::Host(m),
                    None => Error::Internal(other.to_string()),
                }
            }
        }
    }

    fn exception_from_value<'js>(&mut self, ctx: &Ctx<'js>, exc: rquickjs::Value<'js>) -> Error {
        let (name, message, stack) = if let Some(obj) = exc.as_object() {
            let name: String = obj
                .get::<_, rquickjs::Value>("name")
                .ok()
                .map(|v| unsafe_to_string(ctx, &v).unwrap_or_default())
                .unwrap_or_else(|| "Error".into());
            let message: String = obj
                .get::<_, rquickjs::Value>("message")
                .ok()
                .map(|v| unsafe_to_string(ctx, &v).unwrap_or_default())
                .unwrap_or_default();
            let stack: String = obj
                .get::<_, rquickjs::Value>("stack")
                .ok()
                .map(|v| unsafe_to_string(ctx, &v).unwrap_or_default())
                .unwrap_or_default();
            (name, message, parse_stack(&stack))
        } else {
            let name = type_name(&exc).to_string();
            let message = unsafe_to_string(ctx, &exc).unwrap_or_default();
            (name, message, Vec::new())
        };
        let value = self.to_api(ctx, exc);
        Error::Exception(Exception {
            value,
            name,
            message,
            stack,
        })
    }

    fn eval_in<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        source: &str,
        flags: u32,
    ) -> Result<rquickjs::Value<'js>> {
        let cname = std::ffi::CString::new(name).map_err(|e| Error::Internal(e.to_string()))?;
        // JS_Eval takes a length, but QuickJS requires the buffer to be
        // NUL-terminated as well; source text may itself contain NUL.
        let mut csrc: Vec<u8> = Vec::with_capacity(source.len() + 1);
        csrc.extend_from_slice(source.as_bytes());
        csrc.push(0);
        // SAFETY: `ctx` is a live context; the source and name are valid
        // NUL-terminated buffers for the duration of the call; the returned
        // JSValue is owned by us and wrapped in a Value that frees it.
        let raw = unsafe {
            qjs::JS_Eval(
                ctx.as_raw().as_ptr(),
                csrc.as_ptr() as *const std::ffi::c_char,
                source.len() as _,
                cname.as_ptr(),
                flags as i32,
            )
        };
        // SAFETY: `raw` is a fresh owned JSValue from this context.
        let value = unsafe { rquickjs::Value::from_raw(ctx.clone(), raw) };
        if value.is_exception() {
            return Err(self.convert_error(ctx, rquickjs::Error::Exception));
        }
        Ok(value)
    }

    fn with_api<R>(
        &mut self,
        realm: RealmId,
        f: impl FnOnce(&mut QuickJsEngine, &Ctx<'_>) -> Result<R>,
    ) -> Result<R> {
        self.enter(realm)?;
        let shared = self.shared.clone();
        let mut view = QuickJsEngine { shared };
        self.with_ctx(realm, |ctx| f(&mut view, ctx))?
    }

    fn install_console(&mut self, realm: RealmId) -> Result<()> {
        let console = self.new_object(realm)?;
        for (name, level) in [
            ("log", ConsoleLevel::Log),
            ("info", ConsoleLevel::Info),
            ("warn", ConsoleLevel::Warn),
            ("error", ConsoleLevel::Error),
            ("debug", ConsoleLevel::Debug),
        ] {
            let host = self.shared.host.clone();
            let f: Rc<HostFunction> = Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                let mut parts = Vec::with_capacity(cx.args.len());
                for a in &cx.args {
                    parts.push(
                        engine
                            .to_string(cx.realm, a)
                            .unwrap_or_else(|_| "<unprintable>".into()),
                    );
                }
                host.console(cx.realm, level, parts);
                Ok(Value::Undefined)
            });
            let func = self.new_function(realm, name, f)?;
            self.set(realm, &console, &name.into(), &func)?;
        }
        let global = self.global_object(realm)?;
        self.set(realm, &global, &"console".into(), &console)
    }
}

const COMPILE_BLOCKED: &str =
    "new EvalError('code generation from strings is disallowed by the host')";

fn throwing_eval<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    match args.0.into_iter().next() {
        Some(v) if v.is_string() => {
            let e: rquickjs::Value = ctx.eval(COMPILE_BLOCKED)?;
            Err(ctx.throw(e))
        }
        Some(v) => Ok(v),
        None => Ok(rquickjs::Value::new_undefined(ctx)),
    }
}

fn throwing_function<'js>(
    ctx: Ctx<'js>,
    _args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let e: rquickjs::Value = ctx.eval(COMPILE_BLOCKED)?;
    Err(ctx.throw(e))
}

/// Build the QuickJS closure for a host function; a named generic function
/// pins the value lifetime to the realm's.
fn host_closure<'js>(
    shared: Weak<Shared>,
    f: Rc<HostFunction>,
) -> impl Fn(
    Ctx<'js>,
    This<rquickjs::Value<'js>>,
    Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>>
       + 'js {
    move |ctx: Ctx<'js>, this: This<rquickjs::Value<'js>>, args: Rest<rquickjs::Value<'js>>| {
        let Some(shared) = shared.upgrade() else {
            return Err(rquickjs::Error::Unknown);
        };
        let realm = realm_of(&shared, &ctx);
        let mut engine = QuickJsEngine {
            shared: shared.clone(),
        };
        let this_api = engine.to_api(&ctx, this.0);
        let args_api: Vec<Value> = args.0.into_iter().map(|a| engine.to_api(&ctx, a)).collect();
        let cx = CallContext {
            realm,
            this: this_api,
            args: args_api,
            new_target: None,
        };
        match f(&mut engine, cx) {
            Ok(v) => engine
                .to_js(&ctx, &v)
                .map_err(|err| engine.throw_api_error(&ctx, err)),
            Err(err) => Err(engine.throw_api_error(&ctx, err)),
        }
    }
}

fn type_name(v: &rquickjs::Value<'_>) -> &'static str {
    if v.is_string() {
        "string"
    } else if v.is_number() {
        "number"
    } else if v.is_bool() {
        "boolean"
    } else if v.is_symbol() {
        "symbol"
    } else if v.is_big_int() {
        "bigint"
    } else if v.is_undefined() {
        "undefined"
    } else if v.is_null() {
        "null"
    } else if v.is_function() {
        "function"
    } else {
        "object"
    }
}

fn is_interrupt_or_oom(ctx: &Ctx<'_>, exc: &rquickjs::Value<'_>) -> bool {
    if exc.is_uncatchable_error() {
        return true;
    }
    let Some(obj) = exc.as_object() else {
        return false;
    };
    let name = obj
        .get::<_, rquickjs::Value>("name")
        .ok()
        .map(|v| unsafe_to_string(ctx, &v).unwrap_or_default())
        .unwrap_or_default();
    if name != "InternalError" {
        return false;
    }
    let msg = exception_message(ctx, exc).to_lowercase();
    msg.contains("interrupted") || msg.contains("out of memory")
}

fn exception_message(ctx: &Ctx<'_>, exc: &rquickjs::Value<'_>) -> String {
    exc.as_object()
        .and_then(|o| o.get::<_, rquickjs::Value>("message").ok())
        .map(|v| unsafe_to_string(ctx, &v).unwrap_or_default())
        .unwrap_or_default()
}

/// `ToString` through QuickJS, as a Rust string (lossy on lone surrogates).
fn unsafe_to_string(ctx: &Ctx<'_>, v: &rquickjs::Value<'_>) -> Option<String> {
    // SAFETY: `v` is a live value of `ctx`; JS_ToString returns an owned
    // value which we wrap so it is freed.
    let s = unsafe {
        rquickjs::Value::from_raw(
            ctx.clone(),
            qjs::JS_ToString(ctx.as_raw().as_ptr(), v.as_raw()),
        )
    };
    if s.is_exception() {
        let _ = ctx.catch();
        return None;
    }
    Some(js_string_of(ctx, &s).to_string_lossy())
}

/// Read a JS string value as UTF-16 code units (keeps lone surrogates).
fn js_string_of(ctx: &Ctx<'_>, v: &rquickjs::Value<'_>) -> JsString {
    let mut len: qjs::size_t = 0;
    // SAFETY: `v` is a string value of `ctx`; the returned buffer stays
    // valid until JS_FreeCStringUTF16, which we call after copying.
    unsafe {
        let ptr = qjs::JS_ToCStringLenUTF16(ctx.as_raw().as_ptr(), &mut len, v.as_raw());
        if ptr.is_null() {
            let _ = ctx.catch();
            return JsString::from("");
        }
        let units = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        qjs::JS_FreeCStringUTF16(ctx.as_raw().as_ptr(), ptr);
        JsString::from_utf16(units)
    }
}

fn new_js_string<'js>(ctx: &Ctx<'js>, units: &[u16]) -> rquickjs::Value<'js> {
    // SAFETY: `units` is a valid slice for the call; the result is an owned
    // JSValue of `ctx`.
    unsafe {
        let raw = qjs::JS_NewStringUTF16(ctx.as_raw().as_ptr(), units.as_ptr(), units.len() as _);
        rquickjs::Value::from_raw(ctx.clone(), raw)
    }
}

/// Parse a QuickJS stack string (`    at f (file:line:col)` lines).
fn parse_stack(stack: &str) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    for line in stack.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("at ") else {
            continue;
        };
        let (function, location) = match rest.rfind(" (") {
            Some(i) if rest.ends_with(')') => (rest[..i].trim(), &rest[i + 2..rest.len() - 1]),
            _ => ("", rest),
        };
        let (script, line_no, column) = split_location(location);
        frames.push(StackFrame {
            function: function.to_string(),
            script,
            line: line_no,
            column,
        });
    }
    frames
}

fn split_location(loc: &str) -> (String, u32, u32) {
    let mut parts = loc.rsplitn(3, ':');
    let a = parts.next();
    let b = parts.next();
    let c = parts.next();
    match (a, b, c) {
        (Some(col), Some(line), Some(file)) => match (line.parse::<u32>(), col.parse::<u32>()) {
            (Ok(l), Ok(cn)) => (file.to_string(), l, cn),
            _ => (loc.to_string(), 0, 0),
        },
        (Some(line), Some(file), None) => match line.parse::<u32>() {
            Ok(l) => (file.to_string(), l, 0),
            Err(_) => (loc.to_string(), 0, 0),
        },
        _ => (loc.to_string(), 0, 0),
    }
}

impl Engine for QuickJsEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "quickjs-ng",
            bytecode_format: BYTECODE_FORMAT,
        }
    }

    fn create_realm(&mut self) -> Result<RealmId> {
        let context = if DEPTH.with(|d| d.get()) > 0 {
            // A host function is creating a realm ($262.createRealm does).
            // `Context::full` takes the runtime lock, which the enclosing
            // `Context::with` already holds, so build the context raw.
            // SAFETY: the lock is held by this thread for this runtime, the
            // new context belongs to it, and `from_raw` takes over the one
            // reference `JS_NewContext` returns.
            unsafe {
                let rt = qjs::JS_GetRuntime(self.context(RealmId(0))?.as_raw().as_ptr());
                let raw = qjs::JS_NewContext(rt);
                let raw = std::ptr::NonNull::new(raw).ok_or(Error::OutOfMemory)?;
                Context::from_raw(raw, self.shared.rt.clone())
            }
        } else {
            Context::full(&self.shared.rt).map_err(|e| Error::Internal(e.to_string()))?
        };
        let id = {
            let mut realms = self.shared.realms.borrow_mut();
            realms.push(Rc::new(context));
            RealmId((realms.len() - 1) as u32)
        };
        self.shared.compile_blocked.borrow_mut().push(false);
        self.shared.intrinsics.borrow_mut().push(None);
        // QuickJS-NG ships a few non-ECMAScript globals; the host supplies
        // those, so the realm starts without them.
        self.with_ctx(id, |ctx| {
            let globals = ctx.globals();
            for name in ["queueMicrotask", "performance", "print", "navigator"] {
                let _ = globals.remove(name);
            }
        })?;
        self.install_console(id)?;
        Ok(id)
    }

    fn main_realm(&self) -> RealmId {
        RealmId(0)
    }

    fn global_object(&mut self, realm: RealmId) -> Result<Value> {
        self.with_api(
            realm,
            |e, ctx| Ok(e.to_api(ctx, ctx.globals().into_value())),
        )
    }

    fn eval_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Value> {
        self.shared
            .sources
            .borrow_mut()
            .insert((realm.0, name.to_string()), source.to_string());
        self.with_api(realm, |e, ctx| {
            let v = e.eval_in(ctx, name, source, qjs::JS_EVAL_TYPE_GLOBAL)?;
            Ok(e.to_api(ctx, v))
        })
    }

    fn release_module(&mut self, realm: RealmId, _specifier: &str) -> Result<bool> {
        // QuickJS keeps every loaded module in its context for the
        // context's lifetime and offers no way to drop one; HMR on the
        // interim engine reloads into a fresh realm instead.
        self.with_api(realm, |_e, _ctx| Ok(false))
    }

    fn load_module(&mut self, realm: RealmId, specifier: &str) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            e.shared.loader_error.borrow_mut().take();
            match Module::import(ctx, specifier) {
                Ok(promise) => Ok(e.to_api(ctx, promise.into_value())),
                Err(err) => {
                    // Resolution or loading failed synchronously: hand back a
                    // rejected promise so the host sees one shape.
                    let error = e.convert_error(ctx, err);
                    let message = match &error {
                        Error::Exception(ex) => ex.message.clone(),
                        other => other.to_string(),
                    };
                    let (promise, _resolve, reject) =
                        ctx.promise().map_err(|e| Error::Internal(e.to_string()))?;
                    let reason = e.new_error_js(ctx, ErrorKind::Error, &message)?;
                    reject
                        .call::<_, ()>((reason,))
                        .map_err(|e| Error::Internal(e.to_string()))?;
                    Ok(e.to_api(ctx, promise.into_value()))
                }
            }
        })
    }

    fn run_jobs(&mut self, realm: RealmId) -> Result<usize> {
        self.enter(realm)?;
        let shared = self.shared.clone();
        let mut ran = 0usize;
        let mut first_error: Option<Error> = None;
        loop {
            // Finalizers of collected host objects run as host tasks.
            let pending: Vec<_> = std::mem::take(&mut *shared.finalizers.borrow_mut());
            for (class, data) in pending {
                ran += 1;
                let classes = shared.classes.borrow();
                if let Some(f) = classes
                    .get(class.0 as usize)
                    .and_then(|c| c.finalize.as_ref())
                {
                    f(data);
                }
            }
            if !shared.rt.is_job_pending() {
                break;
            }
            DEPTH.with(|d| d.set(1));
            let step = shared.rt.execute_pending_job();
            DEPTH.with(|d| d.set(0));
            match step {
                Ok(true) => ran += 1,
                Ok(false) => break,
                Err(job_exc) => {
                    ran += 1;
                    let ctx_rc = job_exc.0.clone();
                    let err = self.with_ctx_of(&ctx_rc, |e, ctx| {
                        e.convert_error(ctx, rquickjs::Error::Exception)
                    });
                    first_error.get_or_insert(err);
                    break;
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(ran),
        }
    }

    fn has_pending_jobs(&self) -> bool {
        self.shared.rt.is_job_pending() || !self.shared.finalizers.borrow().is_empty()
    }

    fn script_source(&mut self, realm: RealmId, name: &str) -> Option<String> {
        self.shared
            .sources
            .borrow()
            .get(&(realm.0, name.to_string()))
            .cloned()
    }

    fn compile_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Vec<u8>> {
        self.with_api(realm, |e, ctx| {
            let func = e.eval_in(
                ctx,
                name,
                source,
                qjs::JS_EVAL_TYPE_GLOBAL | qjs::JS_EVAL_FLAG_COMPILE_ONLY,
            )?;
            write_bytecode(ctx, &func)
        })
    }

    fn compile_module(&mut self, realm: RealmId, specifier: &str, source: &str) -> Result<Vec<u8>> {
        self.with_api(realm, |e, ctx| {
            let m = e.eval_in(
                ctx,
                specifier,
                source,
                qjs::JS_EVAL_TYPE_MODULE
                    | qjs::JS_EVAL_FLAG_STRICT
                    | qjs::JS_EVAL_FLAG_COMPILE_ONLY,
            )?;
            write_bytecode(ctx, &m)
        })
    }

    fn eval_bytecode(&mut self, realm: RealmId, bytes: &[u8]) -> Result<Value> {
        let payload = bytecode::unwrap(BYTECODE_FORMAT, bytes)?;
        self.with_api(realm, |e, ctx| {
            // SAFETY: payload length matches the slice; QuickJS validates the
            // bytecode structure and reports an exception on failure. The
            // returned values are owned and wrapped so they are freed.
            let obj = unsafe {
                rquickjs::Value::from_raw(
                    ctx.clone(),
                    qjs::JS_ReadObject(
                        ctx.as_raw().as_ptr(),
                        payload.as_ptr(),
                        payload.len() as _,
                        qjs::JS_READ_OBJ_BYTECODE as i32,
                    ),
                )
            };
            if obj.is_exception() {
                let _ = ctx.catch();
                return Err(Error::BytecodeCorrupt(
                    "QuickJS rejected the bytecode".into(),
                ));
            }
            // SAFETY: JS_EvalFunction takes ownership of one reference to the
            // bytecode value, so we hand it a duplicate and let the wrapper
            // (which also owns a context reference) drop normally.
            let result = unsafe {
                let raw = qjs::JS_DupValue(ctx.as_raw().as_ptr(), obj.as_raw());
                drop(obj);
                rquickjs::Value::from_raw(
                    ctx.clone(),
                    qjs::JS_EvalFunction(ctx.as_raw().as_ptr(), raw),
                )
            };
            if result.is_exception() {
                return Err(e.convert_error(ctx, rquickjs::Error::Exception));
            }
            Ok(e.to_api(ctx, result))
        })
    }

    fn set_memory_limit(&mut self, bytes: Option<usize>) {
        // QuickJS treats -1 (usize::MAX) as "no limit".
        self.shared.rt.set_memory_limit(bytes.unwrap_or(usize::MAX));
    }

    fn memory_usage(&mut self) -> MemoryUsage {
        let u = self.shared.rt.memory_usage();
        MemoryUsage {
            heap_bytes: u.memory_used_size.max(0) as usize,
            object_count: u.obj_count.max(0) as usize,
        }
    }

    fn collect_garbage(&mut self) {
        self.reclaim_handles();
        self.shared.rt.run_gc();
    }

    fn type_of(&mut self, value: &Value) -> TypeOf {
        match value {
            Value::Undefined => TypeOf::Undefined,
            Value::Null => TypeOf::Null,
            Value::Bool(_) => TypeOf::Boolean,
            Value::Number(_) => TypeOf::Number,
            Value::String(_) => TypeOf::String,
            Value::BigInt(_) => TypeOf::BigInt,
            Value::Symbol(_) => TypeOf::Symbol,
            Value::Object(_) => {
                let realm = self.main_realm();
                self.with_ctx(realm, |ctx| {
                    let v = self.to_js(ctx, value);
                    match v {
                        Ok(v) if v.is_function() => TypeOf::Function,
                        _ => TypeOf::Object,
                    }
                })
                .unwrap_or(TypeOf::Object)
            }
        }
    }

    fn strict_equals(&mut self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(_) | Value::Symbol(_), Value::Object(_) | Value::Symbol(_)) => {
                let realm = self.main_realm();
                self.with_ctx(realm, |ctx| {
                    let (Ok(a), Ok(b)) = (self.to_js(ctx, a), self.to_js(ctx, b)) else {
                        return false;
                    };
                    // SAFETY: both values are live values of this runtime.
                    unsafe { qjs::JS_IsStrictEqual(ctx.as_raw().as_ptr(), a.as_raw(), b.as_raw()) }
                })
                .unwrap_or(false)
            }
            (Value::Number(x), Value::Number(y)) => x == y,
            _ => a == b,
        }
    }

    fn to_string(&mut self, realm: RealmId, value: &Value) -> Result<String> {
        self.with_api(realm, |e, ctx| {
            let v = e.to_js(ctx, value)?;
            // SAFETY: `v` is a live value of `ctx`; the result is owned.
            let s = unsafe {
                rquickjs::Value::from_raw(
                    ctx.clone(),
                    qjs::JS_ToString(ctx.as_raw().as_ptr(), v.as_raw()),
                )
            };
            if s.is_exception() {
                return Err(e.convert_error(ctx, rquickjs::Error::Exception));
            }
            Ok(js_string_of(ctx, &s).to_string_lossy())
        })
    }

    fn to_number(&mut self, realm: RealmId, value: &Value) -> Result<f64> {
        self.with_api(realm, |e, ctx| {
            let v = e.to_js(ctx, value)?;
            let mut out = 0f64;
            // SAFETY: `v` is live; `out` is a valid f64 slot.
            let rc = unsafe { qjs::JS_ToFloat64(ctx.as_raw().as_ptr(), &mut out, v.as_raw()) };
            if rc < 0 {
                return Err(e.convert_error(ctx, rquickjs::Error::Exception));
            }
            Ok(out)
        })
    }

    fn new_object(&mut self, realm: RealmId) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let o = Object::new(ctx.clone()).map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, o.into_value()))
        })
    }

    fn new_array(&mut self, realm: RealmId, items: &[Value]) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let arr = rquickjs::Array::new(ctx.clone()).map_err(|err| e.convert_error(ctx, err))?;
            for (i, item) in items.iter().enumerate() {
                let v = e.to_js(ctx, item)?;
                arr.set(i, v).map_err(|err| e.convert_error(ctx, err))?;
            }
            Ok(e.to_api(ctx, arr.into_value()))
        })
    }

    fn new_error(&mut self, realm: RealmId, kind: ErrorKind, message: &str) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let err = e.new_error_js(ctx, kind, message)?;
            Ok(e.to_api(ctx, err))
        })
    }

    fn new_symbol(&mut self, realm: RealmId, description: &str) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let s = rquickjs::Symbol::with_description(ctx.clone(), description)
                .map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, s.into_value()))
        })
    }

    fn get(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let obj = e.to_js(ctx, object)?;
            let obj = obj.into_object().ok_or_else(|| e.not_an_object(ctx))?;
            let atom = e.key_to_js(ctx, key)?;
            let v: rquickjs::Value = obj.get(atom).map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, v))
        })
    }

    fn set(
        &mut self,
        realm: RealmId,
        object: &Value,
        key: &PropertyKey,
        value: &Value,
    ) -> Result<()> {
        self.with_api(realm, |e, ctx| {
            let obj = e.to_js(ctx, object)?;
            let obj = obj.into_object().ok_or_else(|| e.not_an_object(ctx))?;
            let atom = e.key_to_js(ctx, key)?;
            let v = e.to_js(ctx, value)?;
            obj.set(atom, v).map_err(|err| e.convert_error(ctx, err))
        })
    }

    fn has(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool> {
        self.with_api(realm, |e, ctx| {
            let obj = e.to_js(ctx, object)?;
            let obj = obj.into_object().ok_or_else(|| e.not_an_object(ctx))?;
            let atom = e.key_to_js(ctx, key)?;
            obj.contains_key(atom)
                .map_err(|err| e.convert_error(ctx, err))
        })
    }

    fn delete(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool> {
        self.with_api(realm, |e, ctx| {
            let obj = e.to_js(ctx, object)?;
            let obj = obj.into_object().ok_or_else(|| e.not_an_object(ctx))?;
            let atom = e.key_to_js(ctx, key)?;
            match obj.remove(atom) {
                Ok(()) => Ok(true),
                Err(rquickjs::Error::Exception) => {
                    Err(e.convert_error(ctx, rquickjs::Error::Exception))
                }
                Err(_) => Ok(false),
            }
        })
    }

    fn own_keys(&mut self, realm: RealmId, object: &Value) -> Result<Vec<PropertyKey>> {
        self.with_api(realm, |e, ctx| {
            let obj = e.to_js(ctx, object)?;
            let obj = obj.into_object().ok_or_else(|| e.not_an_object(ctx))?;
            let filter = rquickjs::object::Filter::new().string().enum_only();
            let mut keys = Vec::new();
            for key in obj.own_keys::<rquickjs::Value>(filter) {
                let key = key.map_err(|err| e.convert_error(ctx, err))?;
                keys.push(PropertyKey::String(js_string_of(ctx, &key)));
            }
            Ok(keys)
        })
    }

    fn call(
        &mut self,
        realm: RealmId,
        function: &Value,
        this: &Value,
        args: &[Value],
    ) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let f = e.to_js(ctx, function)?;
            let f = f
                .into_function()
                .ok_or_else(|| e.type_error(ctx, "not a function"))?;
            let this = e.to_js(ctx, this)?;
            let mut call_args = rquickjs::function::Args::new(ctx.clone(), args.len());
            call_args
                .this(this)
                .map_err(|err| e.convert_error(ctx, err))?;
            for a in args {
                let v = e.to_js(ctx, a)?;
                call_args
                    .push_arg(v)
                    .map_err(|err| e.convert_error(ctx, err))?;
            }
            let r: rquickjs::Value = f
                .call_arg(call_args)
                .map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, r))
        })
    }

    fn construct(&mut self, realm: RealmId, constructor: &Value, args: &[Value]) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let f = e.to_js(ctx, constructor)?;
            let f = f
                .into_function()
                .ok_or_else(|| e.type_error(ctx, "not a constructor"))?;
            let f = rquickjs::function::Constructor::from_value(f.into_value())
                .map_err(|err| e.convert_error(ctx, err))?;
            let mut call_args = rquickjs::function::Args::new(ctx.clone(), args.len());
            for a in args {
                let v = e.to_js(ctx, a)?;
                call_args
                    .push_arg(v)
                    .map_err(|err| e.convert_error(ctx, err))?;
            }
            let r: rquickjs::Value = f
                .construct_args(call_args)
                .map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, r))
        })
    }

    fn is_array(&mut self, value: &Value) -> bool {
        let realm = self.main_realm();
        self.with_ctx(realm, |ctx| {
            self.to_js(ctx, value)
                .map(|v| v.is_array())
                .unwrap_or(false)
        })
        .unwrap_or(false)
    }

    fn is_promise(&mut self, value: &Value) -> bool {
        let realm = self.main_realm();
        self.with_ctx(realm, |ctx| {
            self.to_js(ctx, value)
                .map(|v| v.is_promise())
                .unwrap_or(false)
        })
        .unwrap_or(false)
    }

    fn promise_state(&mut self, realm: RealmId, promise: &Value) -> Result<PromiseState> {
        self.with_api(realm, |e, ctx| {
            let v = e.to_js(ctx, promise)?;
            if !v.is_promise() {
                return Err(e.type_error(ctx, "not a promise"));
            }
            // SAFETY: `v` is a live promise of this runtime.
            let state = unsafe { qjs::JS_PromiseState(ctx.as_raw().as_ptr(), v.as_raw()) };
            Ok(match state {
                qjs::JSPromiseStateEnum_JS_PROMISE_PENDING => PromiseState::Pending,
                qjs::JSPromiseStateEnum_JS_PROMISE_FULFILLED
                | qjs::JSPromiseStateEnum_JS_PROMISE_REJECTED => {
                    // SAFETY: as above; the result is an owned value.
                    let result = unsafe {
                        rquickjs::Value::from_raw(
                            ctx.clone(),
                            qjs::JS_PromiseResult(ctx.as_raw().as_ptr(), v.as_raw()),
                        )
                    };
                    let api = e.to_api(ctx, result);
                    if state == qjs::JSPromiseStateEnum_JS_PROMISE_FULFILLED {
                        PromiseState::Fulfilled(api)
                    } else {
                        PromiseState::Rejected(api)
                    }
                }
                _ => return Err(e.type_error(ctx, "not a promise")),
            })
        })
    }

    fn detach_array_buffer(&mut self, realm: RealmId, buffer: &Value) -> Result<()> {
        self.with_api(realm, |e, ctx| {
            let v = e.to_js(ctx, buffer)?;
            // SAFETY: `v` is live; JS_IsArrayBuffer only inspects the tag.
            if !unsafe { qjs::JS_IsArrayBuffer(v.as_raw()) } {
                return Err(e.type_error(ctx, "not an ArrayBuffer"));
            }
            // SAFETY: `v` is a live ArrayBuffer of `ctx`.
            unsafe { qjs::JS_DetachArrayBuffer(ctx.as_raw().as_ptr(), v.as_raw()) };
            Ok(())
        })
    }

    fn new_function(&mut self, realm: RealmId, name: &str, f: Rc<HostFunction>) -> Result<Value> {
        self.with_api(realm, |e, ctx| {
            let shared = Rc::downgrade(&e.shared);
            let func = Function::new(ctx.clone(), host_closure(shared, f.clone()))
                .map_err(|err| e.convert_error(ctx, err))?;
            func.set_name(name)
                .map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, func.into_value()))
        })
    }

    fn register_host_class(&mut self, def: HostClassDef) -> HostClassId {
        let mut classes = self.shared.classes.borrow_mut();
        classes.push(def);
        HostClassId((classes.len() - 1) as u32)
    }

    fn new_host_object(
        &mut self,
        realm: RealmId,
        class: HostClassId,
        data: Box<dyn HostObject>,
        prototype: Option<&Value>,
    ) -> Result<Value> {
        let name = self
            .shared
            .classes
            .borrow()
            .get(class.0 as usize)
            .map(|c| c.name.clone())
            .ok_or_else(|| Error::Internal("unknown host class".into()))?;
        self.with_api(realm, |e, ctx| {
            let cell = HostCell {
                class,
                data: Some(data),
                finalizers: Rc::downgrade(&e.shared.finalizers),
            };
            let instance = rquickjs::Class::<HostCell>::instance(ctx.clone(), cell)
                .map_err(|err| e.convert_error(ctx, err))?;
            let obj: Object = instance.into_inner();
            let proto: Option<Object> = match prototype {
                Some(p) => e.to_js(ctx, p)?.into_object(),
                None => Some(
                    Object::new(ctx.clone())
                        .map_err(|err| e.convert_error(ctx, err))?
                        .get_prototype()
                        .unwrap(),
                ),
            };
            obj.set_prototype(proto.as_ref())
                .map_err(|err| e.convert_error(ctx, err))?;
            let tag: rquickjs::Symbol = ctx
                .eval("Symbol.toStringTag")
                .map_err(|err| e.convert_error(ctx, err))?;
            obj.prop(
                tag,
                rquickjs::object::Property::from(name.as_str()).configurable(),
            )
            .map_err(|err| e.convert_error(ctx, err))?;
            Ok(e.to_api(ctx, obj.into_value()))
        })
    }

    fn host_object_data(&mut self, value: &Value) -> Option<&mut dyn HostObject> {
        let realm = self.main_realm();
        let ptr: Option<*mut dyn HostObject> = self
            .with_ctx(realm, |ctx| {
                let v = self.to_js(ctx, value).ok()?;
                let cls = rquickjs::Class::<HostCell>::from_value(&v).ok()?;
                let mut cell = cls.borrow_mut();
                cell.data.as_mut().map(|d| &mut **d as *mut dyn HostObject)
            })
            .ok()
            .flatten();
        // SAFETY: the Box<dyn HostObject> lives inside the QuickJS object,
        // which the caller's `Value` handle keeps alive for at least as long
        // as the returned borrow of `self` (the handle outlives the borrow
        // because the caller owns it). Nothing else touches the cell while
        // the engine is mutably borrowed.
        ptr.map(|p| unsafe { &mut *p })
    }

    fn host_object_class(&mut self, value: &Value) -> Option<HostClassId> {
        let realm = self.main_realm();
        self.with_ctx(realm, |ctx| {
            let v = self.to_js(ctx, value).ok()?;
            let cls = rquickjs::Class::<HostCell>::from_value(&v).ok()?;
            let class = cls.borrow().class;
            Some(class)
        })
        .ok()
        .flatten()
    }

    fn reclaim_handles(&mut self) {
        let freed = self.shared.handles.borrow_mut().reclaim();
        if freed.is_empty() {
            return;
        }
        // Persistent values free their JSValue on drop, which needs the
        // runtime lock to be held (or not contended) on this thread.
        let realm = self.main_realm();
        let _ = self.with_ctx(realm, |_ctx| drop(freed));
    }

    fn live_handles(&self) -> usize {
        self.shared.handles.borrow().live()
    }
}

impl QuickJsEngine {
    fn with_ctx_of<R>(
        &mut self,
        context: &Context,
        f: impl FnOnce(&mut QuickJsEngine, &Ctx<'_>) -> R,
    ) -> R {
        let shared = self.shared.clone();
        let mut view = QuickJsEngine { shared };
        if DEPTH.with(|d| d.get()) > 0 {
            // SAFETY: see `with_ctx`.
            let ctx = unsafe { Ctx::from_raw(context.as_raw()) };
            f(&mut view, &ctx)
        } else {
            DEPTH.with(|d| d.set(1));
            let r = context.with(|ctx| f(&mut view, &ctx));
            DEPTH.with(|d| d.set(0));
            r
        }
    }

    fn new_error_js<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        kind: ErrorKind,
        message: &str,
    ) -> Result<rquickjs::Value<'js>> {
        let ctor: rquickjs::Value = ctx
            .globals()
            .get(kind.name())
            .map_err(|e| Error::Internal(e.to_string()))?;
        let ctor = rquickjs::function::Constructor::from_value(ctor)
            .map_err(|e| Error::Internal(e.to_string()))?;
        let msg = new_js_string(ctx, &message.encode_utf16().collect::<Vec<_>>());
        ctor.construct::<_, rquickjs::Value>((msg,))
            .map_err(|e| self.convert_error(ctx, e))
    }

    fn type_error(&mut self, ctx: &Ctx<'_>, message: &str) -> Error {
        match self.new_error_js(ctx, ErrorKind::TypeError, message) {
            Ok(v) => self.exception_from_value(ctx, v),
            Err(e) => e,
        }
    }

    fn not_an_object(&mut self, ctx: &Ctx<'_>) -> Error {
        self.type_error(ctx, "not an object")
    }

    /// Convert an interface error coming back from a host function into a
    /// pending QuickJS exception.
    fn throw_api_error(&mut self, ctx: &Ctx<'_>, err: Error) -> rquickjs::Error {
        let value = match err {
            Error::Exception(ex) => self.to_js(ctx, &ex.value).unwrap_or_else(|_| {
                self.new_error_js(ctx, ErrorKind::Error, &ex.message)
                    .unwrap_or_else(|_| rquickjs::Value::new_undefined(ctx.clone()))
            }),
            Error::Interrupted => {
                // Propagate the interrupt as an uncatchable error.
                // SAFETY: ctx is live; JS_ThrowInternalError sets the pending exception.
                unsafe {
                    qjs::JS_ThrowInternalError(ctx.as_raw().as_ptr(), c"interrupted".as_ptr())
                };
                return rquickjs::Error::Exception;
            }
            other => self
                .new_error_js(ctx, ErrorKind::Error, &other.to_string())
                .unwrap_or_else(|_| rquickjs::Value::new_undefined(ctx.clone())),
        };
        ctx.throw(value)
    }
}

fn write_bytecode(ctx: &Ctx<'_>, obj: &rquickjs::Value<'_>) -> Result<Vec<u8>> {
    let mut len: qjs::size_t = 0;
    // SAFETY: `obj` is a live compiled function or module of `ctx`. The
    // buffer QuickJS returns is ours to free with js_free after copying.
    unsafe {
        let ptr = qjs::JS_WriteObject(
            ctx.as_raw().as_ptr(),
            &mut len,
            obj.as_raw(),
            qjs::JS_WRITE_OBJ_BYTECODE as i32,
        );
        if ptr.is_null() {
            let _ = ctx.catch();
            return Err(Error::Internal("JS_WriteObject failed".into()));
        }
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        qjs::js_free(ctx.as_raw().as_ptr(), ptr.cast());
        Ok(bytecode::wrap(BYTECODE_FORMAT, &bytes))
    }
}

/// Factory used by tests and the test262 runner.
pub fn make_engine(host: Rc<dyn Host>) -> Box<dyn Engine> {
    Box::new(QuickJsEngine::new(host).expect("QuickJS runtime"))
}

#[cfg(test)]
mod tests {
    use super::*;
    agnijs_engine_api::conformance_tests!(|host| make_engine(host));

    #[test]
    fn parses_quickjs_stack_lines() {
        let frames = parse_stack("    at f (test.js:2:9)\n    at <eval> (test.js:4:1)\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].function, "f");
        assert_eq!(frames[0].script, "test.js");
        assert_eq!((frames[0].line, frames[0].column), (2, 9));
    }
}
