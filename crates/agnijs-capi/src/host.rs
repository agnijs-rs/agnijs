//! The host side: the C hooks behind [`api::Host`], host functions, host
//! objects and the sinks module hooks answer through. Every call into C runs
//! under `catch_unwind` (the hooks are `extern "C-unwind"`, so a Rust panic
//! in a hook written in Rust is caught; a C++ exception must not leave a
//! hook, see docs/c-api.md), and everything C hands back is validated.

use crate::guard::{drop_payload, ffi, last_error, panic_message, Fail, Res};
use crate::ptrs;
use crate::registry::{lend, with_state};
use crate::table::Entry;
use crate::types::*;
use agnijs_engine_api as api;
use agnijs_engine_api::Engine;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Call host code, catching a Rust panic that unwinds out of it.
fn guarded<R>(call: impl FnOnce() -> R) -> Result<R, String> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
        let msg = panic_message(&*payload);
        drop_payload(payload);
        msg
    })
}

/// A copy of `s` with a terminating NUL, for hooks that want C strings.
fn c_text(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len().saturating_add(1));
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// The name of a status, for messages.
pub fn status_name(s: AgnijsStatus) -> &'static str {
    match s {
        AGNIJS_OK => "AGNIJS_OK",
        AGNIJS_EXCEPTION => "AGNIJS_EXCEPTION",
        AGNIJS_INTERRUPTED => "AGNIJS_INTERRUPTED",
        AGNIJS_OUT_OF_MEMORY => "AGNIJS_OUT_OF_MEMORY",
        AGNIJS_OUT_OF_FUEL => "AGNIJS_OUT_OF_FUEL",
        AGNIJS_ERR_NULL_POINTER => "AGNIJS_ERR_NULL_POINTER",
        AGNIJS_ERR_INVALID_ENGINE => "AGNIJS_ERR_INVALID_ENGINE",
        AGNIJS_ERR_WRONG_THREAD => "AGNIJS_ERR_WRONG_THREAD",
        AGNIJS_ERR_INVALID_HANDLE => "AGNIJS_ERR_INVALID_HANDLE",
        AGNIJS_ERR_WRONG_ENGINE => "AGNIJS_ERR_WRONG_ENGINE",
        AGNIJS_ERR_INVALID_REALM => "AGNIJS_ERR_INVALID_REALM",
        AGNIJS_ERR_INVALID_ARGUMENT => "AGNIJS_ERR_INVALID_ARGUMENT",
        AGNIJS_ERR_INVALID_UTF8 => "AGNIJS_ERR_INVALID_UTF8",
        AGNIJS_ERR_TYPE_MISMATCH => "AGNIJS_ERR_TYPE_MISMATCH",
        AGNIJS_ERR_BUSY => "AGNIJS_ERR_BUSY",
        AGNIJS_ERR_POISONED => "AGNIJS_ERR_POISONED",
        AGNIJS_ERR_PANIC => "AGNIJS_ERR_PANIC",
        AGNIJS_ERR_BYTECODE => "AGNIJS_ERR_BYTECODE",
        AGNIJS_ERR_HOST => "AGNIJS_ERR_HOST",
        AGNIJS_ERR_ENGINE => "AGNIJS_ERR_ENGINE",
        AGNIJS_ERR_UNSUPPORTED => "AGNIJS_ERR_UNSUPPORTED",
        AGNIJS_ERR_BUFFER_TOO_SMALL => "AGNIJS_ERR_BUFFER_TOO_SMALL",
        AGNIJS_ERR_LIMIT => "AGNIJS_ERR_LIMIT",
        AGNIJS_ERR_NOT_FOUND => "AGNIJS_ERR_NOT_FOUND",
        _ => "an unknown status",
    }
}

// ---- sinks --------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum SinkKind {
    Resolve,
    Load,
}

enum SinkValue {
    Text(String),
    Module(api::ModuleSource),
}

struct SinkState {
    id: AgnijsSink,
    kind: SinkKind,
    value: Option<SinkValue>,
    error: Option<String>,
}

thread_local! {
    static SINKS: RefCell<Vec<SinkState>> = const { RefCell::new(Vec::new()) };
}
static NEXT_SINK: AtomicU64 = AtomicU64::new(1);

fn sink_open(kind: SinkKind) -> AgnijsSink {
    let id = NEXT_SINK.fetch_add(1, Ordering::Relaxed);
    let _ = SINKS.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.push(SinkState { id, kind, value: None, error: None });
        }
    });
    id
}

fn sink_close(id: AgnijsSink) -> Option<SinkState> {
    SINKS
        .try_with(|s| {
            let mut s = s.try_borrow_mut().ok()?;
            let pos = s.iter().position(|x| x.id == id)?;
            Some(s.remove(pos))
        })
        .ok()
        .flatten()
}

fn with_sink(id: AgnijsSink, f: impl FnOnce(&mut SinkState) -> Res<()>) -> Res<()> {
    SINKS
        .try_with(|s| {
            let mut s = s.try_borrow_mut().map_err(|_| Fail::new(AGNIJS_ERR_BUSY, "sink table in use"))?;
            let sink = s.iter_mut().find(|x| x.id == id).ok_or_else(|| {
                Fail::new(AGNIJS_ERR_INVALID_HANDLE, "unknown sink: a sink is valid only during the hook it was passed to")
            })?;
            f(sink)
        })
        .unwrap_or_else(|_| Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, "this thread is exiting")))
}

/// Answer a resolve hook with the canonical specifier (UTF-8).
///
/// # Safety
/// `text` must point at `len` readable bytes (or be NULL when `len` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_sink_set_string(sink: AgnijsSink, text: *const c_char, len: usize) -> AgnijsStatus {
    ffi("agnijs_sink_set_string", || {
        // SAFETY: caller contract above.
        let s = unsafe { ptrs::utf8(text, len, "text")? }.to_string();
        with_sink(sink, |st| {
            if st.kind != SinkKind::Resolve {
                return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, "this sink takes a module (agnijs_sink_set_module)"));
            }
            st.value = Some(SinkValue::Text(s));
            Ok(())
        })
    })
}

/// Answer a load hook: `kind` is `AGNIJS_MODULE_SOURCE` or `AGNIJS_MODULE_JSON`
/// (UTF-8 text) or `AGNIJS_MODULE_BYTECODE` (bytes from
/// `agnijs_compile_module`). The bytes are copied.
///
/// # Safety
/// `data` must point at `len` readable bytes (or be NULL when `len` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_sink_set_module(sink: AgnijsSink, kind: i32, data: *const u8, len: usize) -> AgnijsStatus {
    ffi("agnijs_sink_set_module", || {
        let module = match kind {
            AGNIJS_MODULE_SOURCE => {
                // SAFETY: caller contract above.
                api::ModuleSource::Source(unsafe { ptrs::utf8(data.cast(), len, "data")? }.to_string())
            }
            AGNIJS_MODULE_JSON => {
                // SAFETY: caller contract above.
                api::ModuleSource::Json(unsafe { ptrs::utf8(data.cast(), len, "data")? }.to_string())
            }
            AGNIJS_MODULE_BYTECODE => {
                // SAFETY: caller contract above.
                api::ModuleSource::Bytecode(unsafe { ptrs::bytes(data, len, "data")? }.to_vec())
            }
            other => return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("unknown module kind {other}"))),
        };
        with_sink(sink, |st| {
            if st.kind != SinkKind::Load {
                return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, "this sink takes a string (agnijs_sink_set_string)"));
            }
            st.value = Some(SinkValue::Module(module));
            Ok(())
        })
    })
}

/// Give a failing hook's error message (UTF-8; invalid UTF-8 is replaced).
///
/// # Safety
/// `text` must point at `len` readable bytes (or be NULL when `len` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_sink_set_error(sink: AgnijsSink, text: *const c_char, len: usize) -> AgnijsStatus {
    ffi("agnijs_sink_set_error", || {
        // SAFETY: caller contract above.
        let b = unsafe { ptrs::bytes(text.cast(), len, "text")? };
        let msg = String::from_utf8_lossy(b).into_owned();
        with_sink(sink, |st| {
            st.error = Some(msg);
            Ok(())
        })
    })
}

/// Run a module hook with a fresh sink and collect its answer.
fn run_sink_hook(kind: SinkKind, what: &str, call: impl FnOnce(AgnijsSink) -> AgnijsStatus) -> Result<SinkValue, String> {
    let sink = sink_open(kind);
    let status = guarded(|| call(sink));
    let state = sink_close(sink);
    let status = status.map_err(|m| format!("{what} hook panicked: {m}"))?;
    let state = state.ok_or_else(|| format!("{what} hook: sink lost"))?;
    if status != AGNIJS_OK {
        return Err(match state.error {
            Some(m) => m,
            None => format!("{what} hook failed with {}", status_name(status)),
        });
    }
    state.value.ok_or_else(|| format!("{what} hook returned AGNIJS_OK without an answer"))
}

// ---- the Host ---------------------------------------------------------------------

/// The engine's [`api::Host`], forwarding to the C hooks.
pub struct CHost {
    pub engine: AgnijsEngine,
    pub opts: AgnijsEngineOptions,
    pub interrupt_flag: Arc<AtomicBool>,
}

impl CHost {
    fn ud(&self) -> *mut c_void {
        self.opts.user_data
    }
}

/// Insert values into the engine's table for a callback; freed by the guard.
struct Borrowed {
    engine: AgnijsEngine,
    ids: Vec<u64>,
}

impl Borrowed {
    fn new(engine: AgnijsEngine, values: Vec<Option<api::Value>>) -> api::Result<(Borrowed, Vec<u64>)> {
        let ids = with_state(engine, true, |st| {
            let mut ids = Vec::with_capacity(values.len());
            for v in values {
                ids.push(match v {
                    Some(v) => st.table.insert(Entry::Value(v))?,
                    None => 0,
                });
            }
            Ok(ids)
        })
        .map_err(|f| api::Error::Host(f.message))?;
        Ok((Borrowed { engine, ids: ids.clone() }, ids))
    }
}

impl Drop for Borrowed {
    fn drop(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        let _ = with_state(self.engine, true, |st| {
            for id in ids {
                if id != 0 {
                    // Already freed or taken over by the host: ignore.
                    let _ = st.table.remove(id, "argument");
                }
            }
            Ok(())
        });
    }
}

/// Map the status a callback returned (host functions, `import.meta`).
fn callback_status(engine: AgnijsEngine, status: AgnijsStatus, what: &str) -> api::Result<()> {
    match status {
        AGNIJS_OK => Ok(()),
        AGNIJS_EXCEPTION => {
            // The value given to agnijs_throw, or else the exception a nested
            // call made during this callback left untaken (so a callback can
            // pass an exception on by returning the status it got).
            let thrown = with_state(engine, true, |st| {
                Ok(match st.pending_throw.take() {
                    Some(value) => {
                        Some(api::Exception { value, name: String::new(), message: String::new(), stack: Vec::new() })
                    }
                    None => st.exception.take(),
                })
            })
            .ok()
            .flatten();
            match thrown {
                Some(ex) => Err(api::Error::Exception(ex)),
                None => Err(api::Error::Host(format!("{what} returned AGNIJS_EXCEPTION without calling agnijs_throw"))),
            }
        }
        AGNIJS_INTERRUPTED => Err(api::Error::Interrupted),
        AGNIJS_OUT_OF_MEMORY => Err(api::Error::OutOfMemory),
        AGNIJS_OUT_OF_FUEL => Err(api::Error::OutOfFuel),
        other => {
            let (_, msg) = last_error();
            Err(api::Error::Host(format!("{what} failed with {} ({msg})", status_name(other))))
        }
    }
}

impl api::Host for CHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        _attributes: &[api::ImportAttribute],
    ) -> Result<String, String> {
        let Some(hook) = self.opts.resolve_module else {
            return Ok(specifier.to_string());
        };
        let r = referrer.map(c_text);
        let s = c_text(specifier);
        let (rp, rl) = match &r {
            Some(r) => (r.as_ptr().cast::<c_char>(), r.len().saturating_sub(1)),
            None => (std::ptr::null(), 0),
        };
        let ud = self.ud();
        // SAFETY: a host-supplied function pointer called with valid,
        // NUL-terminated strings that outlive the call.
        let v = run_sink_hook(SinkKind::Resolve, "resolve_module", |sink| unsafe {
            hook(ud, rp, rl, s.as_ptr().cast(), s.len().saturating_sub(1), sink)
        })?;
        match v {
            SinkValue::Text(t) => Ok(t),
            SinkValue::Module(_) => Err("resolve_module hook answered with a module".into()),
        }
    }

    fn load_module(&self, resolved: &str) -> Result<api::ModuleSource, String> {
        let Some(hook) = self.opts.load_module else {
            return Err(format!("module not found: {resolved} (no load_module hook)"));
        };
        let s = c_text(resolved);
        let ud = self.ud();
        // SAFETY: as in resolve_module.
        let v = run_sink_hook(SinkKind::Load, "load_module", |sink| unsafe {
            hook(ud, s.as_ptr().cast(), s.len().saturating_sub(1), sink)
        })?;
        match v {
            SinkValue::Module(m) => Ok(m),
            SinkValue::Text(_) => Err("load_module hook answered with a string".into()),
        }
    }

    fn import_meta(&self, engine: &mut dyn Engine, realm: api::RealmId, module: &str, meta: &api::Value) -> api::Result<()> {
        let Some(hook) = self.opts.import_meta else { return Ok(()) };
        let e = self.engine;
        let realm_id = with_state(e, true, |st| Ok(st.realm_id(realm))).map_err(|f| api::Error::Host(f.message))?;
        let (guard, ids) = Borrowed::new(e, vec![Some(meta.clone())])?;
        let meta_id = ids.first().copied().unwrap_or(0);
        let m = c_text(module);
        let ud = self.ud();
        // SAFETY: a host-supplied function pointer; the string outlives the call.
        let status = lend(e, engine, || unsafe { hook(ud, e, realm_id, m.as_ptr().cast(), m.len().saturating_sub(1), meta_id) })?;
        drop(guard);
        callback_status(e, status, "import_meta hook")
    }

    fn promise_rejection(&self, engine: &mut dyn Engine, realm: api::RealmId, event: api::RejectionEvent) {
        let Some(hook) = self.opts.promise_rejection else { return };
        let e = self.engine;
        let Ok(realm_id) = with_state(e, true, |st| Ok(st.realm_id(realm))) else { return };
        let (kind, values) = match event {
            api::RejectionEvent::Reject { promise, reason } => (AGNIJS_REJECTION_UNHANDLED, vec![Some(promise), Some(reason)]),
            api::RejectionEvent::Handle { promise } => (AGNIJS_REJECTION_HANDLED, vec![Some(promise), None]),
        };
        let Ok((guard, ids)) = Borrowed::new(e, values) else { return };
        let p = ids.first().copied().unwrap_or(0);
        let r = ids.get(1).copied().unwrap_or(0);
        let ud = self.ud();
        // SAFETY: a host-supplied function pointer.
        let _ = lend(e, engine, || unsafe { hook(ud, e, realm_id, kind, p, r) });
        drop(guard);
    }

    fn can_compile_strings(&self, _realm: api::RealmId) -> bool {
        self.opts.flags & AGNIJS_FLAG_NO_STRING_COMPILATION == 0
    }

    fn console(&self, realm: api::RealmId, level: api::ConsoleLevel, args: Vec<String>) {
        let Some(hook) = self.opts.console else { return };
        let level = match level {
            api::ConsoleLevel::Log => AGNIJS_CONSOLE_LOG,
            api::ConsoleLevel::Info => AGNIJS_CONSOLE_INFO,
            api::ConsoleLevel::Warn => AGNIJS_CONSOLE_WARN,
            api::ConsoleLevel::Error => AGNIJS_CONSOLE_ERROR,
            api::ConsoleLevel::Debug => AGNIJS_CONSOLE_DEBUG,
        };
        let Ok(realm_id) = with_state(self.engine, true, |st| Ok(st.realm_id(realm))) else { return };
        let text = c_text(&args.join(" "));
        let ud = self.ud();
        // SAFETY: a host-supplied function pointer; the text outlives the call.
        let _ = guarded(|| unsafe { hook(ud, realm_id, level, text.as_ptr().cast(), text.len().saturating_sub(1)) });
    }

    fn interrupt(&self) -> bool {
        if self.interrupt_flag.swap(false, Ordering::SeqCst) {
            return true;
        }
        let Some(hook) = self.opts.interrupt else { return false };
        let ud = self.ud();
        // SAFETY: a host-supplied function pointer. A panicking hook
        // interrupts (fail safe).
        guarded(|| unsafe { hook(ud) }).map(|r| r != 0).unwrap_or(true)
    }

    fn now_ms(&self) -> f64 {
        if let Some(hook) = self.opts.now_ms {
            let ud = self.ud();
            // SAFETY: a host-supplied function pointer.
            if let Ok(t) = guarded(|| unsafe { hook(ud) }) {
                return t;
            }
        }
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs_f64() * 1000.0).floor(),
            Err(e) => -((e.duration().as_secs_f64() * 1000.0).floor()),
        }
    }

    fn regex_matcher(&self) -> api::RegexMatcher {
        match self.opts.regex_matcher {
            AGNIJS_REGEX_BACKTRACKING_ONLY => api::RegexMatcher::BacktrackingOnly,
            AGNIJS_REGEX_LINEAR_WHEN_ELIGIBLE => api::RegexMatcher::LinearWhenEligible,
            _ => api::RegexMatcher::Auto,
        }
    }
}

// ---- host functions ------------------------------------------------------------------

/// A host function's C side; releases `user_data` when the function object
/// is collected or the engine is freed.
struct HostFn {
    engine: AgnijsEngine,
    f: unsafe extern "C-unwind" fn(AgnijsEngine, *const AgnijsCallInfo, *mut c_void, *mut AgnijsValue) -> AgnijsStatus,
    user_data: *mut c_void,
    finalize: AgnijsFinalizer,
    /// Set once the engine owns the function (see [`CObject`]).
    armed: Rc<Cell<bool>>,
}

impl Drop for HostFn {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        if let Some(fin) = self.finalize {
            let ud = self.user_data;
            // SAFETY: a host-supplied finalizer, called once with the data it
            // was registered with.
            let _ = guarded(|| unsafe { fin(ud) });
        }
    }
}

/// Wrap a C host function for [`Engine::new_function`]. The finalizer runs
/// only once the returned flag is set (after the engine accepted it).
pub fn host_function(
    engine: AgnijsEngine,
    f: unsafe extern "C-unwind" fn(AgnijsEngine, *const AgnijsCallInfo, *mut c_void, *mut AgnijsValue) -> AgnijsStatus,
    user_data: *mut c_void,
    finalize: AgnijsFinalizer,
) -> (Rc<api::HostFunction>, Rc<Cell<bool>>) {
    let armed = Rc::new(Cell::new(false));
    let data = HostFn { engine, f, user_data, finalize, armed: armed.clone() };
    (Rc::new(move |eng: &mut dyn Engine, cx: api::CallContext| call_host_fn(&data, eng, cx)), armed)
}

fn call_host_fn(data: &HostFn, eng: &mut dyn Engine, cx: api::CallContext) -> api::Result<api::Value> {
    let e = data.engine;
    let realm = with_state(e, true, |st| Ok(st.realm_id(cx.realm))).map_err(|f| api::Error::Host(f.message))?;
    let argc = cx.args.len();
    let mut values = Vec::with_capacity(argc.saturating_add(2));
    values.push(Some(cx.this));
    values.push(cx.new_target);
    values.extend(cx.args.into_iter().map(Some));
    let (guard, ids) = Borrowed::new(e, values)?;
    let this_value = ids.first().copied().unwrap_or(0);
    let new_target = ids.get(1).copied().unwrap_or(0);
    let args: Vec<u64> = ids.iter().skip(2).copied().collect();
    let info = AgnijsCallInfo {
        struct_size: u32::try_from(std::mem::size_of::<AgnijsCallInfo>()).unwrap_or(u32::MAX),
        realm,
        this_value,
        args: if args.is_empty() { std::ptr::null() } else { args.as_ptr() },
        argc: args.len(),
        new_target,
    };
    let mut result: AgnijsValue = 0;
    let (f, ud) = (data.f, data.user_data);
    let result_ptr: *mut AgnijsValue = &mut result;
    // SAFETY: a host-supplied function pointer; `info`, the args array and
    // `result` outlive the call.
    let status = lend(e, eng, || unsafe { f(e, &info, ud, result_ptr) });
    // Take the result before the arguments are freed: a host function may
    // return one of its arguments.
    let taken = match &status {
        Ok(AGNIJS_OK) if result != 0 => Some(
            with_state(e, true, |st| match st.table.remove_kind(result, false, "result")? {
                Entry::Value(v) => Ok(v),
                Entry::Bytes(_) => Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, "result is a buffer")),
            })
            .map_err(|f| api::Error::Host(format!("host function returned an invalid result handle: {}", f.message))),
        ),
        _ => None,
    };
    drop(guard);
    callback_status(e, status?, "host function")?;
    match taken {
        Some(v) => v,
        None => Ok(api::Value::Undefined),
    }
}

// ---- host objects ---------------------------------------------------------------------

/// The data of a host object: a C pointer and its class's finalizer.
pub struct CObject {
    pub class: usize,
    pub data: *mut c_void,
    finalize: AgnijsFinalizer,
    /// Set once the engine owns the object, so data handed back after a
    /// failed creation is never finalized.
    armed: Rc<Cell<bool>>,
}

impl CObject {
    pub fn new(class: usize, data: *mut c_void, finalize: AgnijsFinalizer) -> (CObject, Rc<Cell<bool>>) {
        let armed = Rc::new(Cell::new(false));
        (CObject { class, data, finalize, armed: armed.clone() }, armed)
    }
}

impl api::HostObject for CObject {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Drop for CObject {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        if let Some(fin) = self.finalize {
            let data = self.data;
            // SAFETY: a host-supplied finalizer, called once with the
            // instance data it was given.
            let _ = guarded(|| unsafe { fin(data) });
        }
    }
}
