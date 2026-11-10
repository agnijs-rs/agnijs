//! The exported functions. Each one validates its arguments, runs under
//! [`ffi`] (so no panic escapes), and returns an [`AgnijsStatus`].
//!
//! Conventions, also stated in the header:
//!
//! - Text in is UTF-8 as (pointer, length); NULL is allowed when the length
//!   is zero. String values may also come in as UTF-16 code units, lone
//!   surrogates included.
//! - Every `out` pointer is checked before any work is done; on failure
//!   nothing is written to it.
//! - Values and buffers returned through `out` belong to the caller, who
//!   frees them with [`agnijs_value_free`] or [`agnijs_buffer_free`] (or
//!   frees the engine, which frees everything).

use crate::guard::{ffi, ffi_value, last_error, Fail, Res};
use crate::host::{host_function, CHost, CObject};
use crate::ptrs::{self, Out};
use crate::registry::{self, eng, engine_op, with_state, EngineState};
use crate::table::Entry;
use crate::types::*;
use agnijs_engine_api as api;
use std::ffi::{c_char, c_void};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// ---- helpers ---------------------------------------------------------------------------

fn value(e: AgnijsEngine, id: AgnijsValue, what: &str) -> Res<api::Value> {
    with_state(e, false, |st| st.table.value(id, what))
}

fn values(e: AgnijsEngine, ids: &[u64], what: &str) -> Res<Vec<api::Value>> {
    with_state(e, false, |st| ids.iter().map(|id| st.table.value(*id, what)).collect())
}

fn put(e: AgnijsEngine, v: api::Value) -> Res<AgnijsValue> {
    with_state(e, true, |st| st.table.insert(Entry::Value(v)))
}

fn put_bytes(e: AgnijsEngine, b: Vec<u8>) -> Res<AgnijsBuffer> {
    with_state(e, true, |st| st.table.insert(Entry::Bytes(b.into_boxed_slice())))
}

fn realm(e: AgnijsEngine, r: AgnijsRealm) -> Res<api::RealmId> {
    with_state(e, false, |st| st.realm(r))
}

fn object(e: AgnijsEngine, id: AgnijsValue, what: &str) -> Res<api::Value> {
    let v = value(e, id, what)?;
    match v {
        api::Value::Object(_) => Ok(v),
        _ => Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, format!("{what} is not an object"))),
    }
}

fn key_value(e: AgnijsEngine, id: AgnijsValue) -> Res<api::PropertyKey> {
    match value(e, id, "key")? {
        api::Value::String(s) => Ok(api::PropertyKey::String(s)),
        api::Value::Symbol(h) => Ok(api::PropertyKey::Symbol(h)),
        _ => Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "key is neither a string nor a symbol")),
    }
}

/// # Safety
/// As [`ptrs::utf8`].
unsafe fn key_utf8(key: *const c_char, len: usize) -> Res<api::PropertyKey> {
    // SAFETY: forwarded contract.
    Ok(api::PropertyKey::String(api::JsString::from(unsafe { ptrs::utf8(key, len, "key")? })))
}

// ---- version and errors --------------------------------------------------------------

/// `(AGNIJS_ABI_VERSION_MAJOR << 16) | AGNIJS_ABI_VERSION_MINOR` of the
/// library actually loaded. Compare the major part with the header's.
#[no_mangle]
pub extern "C" fn agnijs_abi_version() -> u32 {
    (AGNIJS_ABI_VERSION_MAJOR << 16) | AGNIJS_ABI_VERSION_MINOR
}

static VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// The library version as a static NUL-terminated string (never freed).
#[no_mangle]
pub extern "C" fn agnijs_version_string() -> *const c_char {
    VERSION.as_ptr().cast()
}

/// Copy this thread's last error message (UTF-8, NUL-terminated) into
/// `dst`. `needed` (may be NULL) receives its length without the NUL.
/// Returns `AGNIJS_ERR_BUFFER_TOO_SMALL` when `cap` is not more than that.
/// Does not itself change the last error.
///
/// # Safety
/// `dst` must point at `cap` writable bytes (or be NULL when `cap` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_last_error(dst: *mut c_char, cap: usize, needed: *mut usize) -> AgnijsStatus {
    ffi_value(AGNIJS_ERR_PANIC, || {
        let (_, msg) = last_error();
        let needed = match Out::optional(needed, "needed") {
            Ok(n) => n,
            Err(f) => return f.status,
        };
        // SAFETY: caller contract above.
        match unsafe { ptrs::copy_out(msg.as_bytes(), dst.cast::<u8>(), cap, needed) } {
            Ok(()) => AGNIJS_OK,
            Err(f) => f.status,
        }
    })
}

/// The status of this thread's last failed call.
#[no_mangle]
pub extern "C" fn agnijs_last_status() -> AgnijsStatus {
    ffi_value(AGNIJS_ERR_PANIC, || last_error().0)
}

// ---- engines and realms -----------------------------------------------------------------

/// Create an engine on the calling thread, which owns it from then on.
/// `options` may be NULL for the defaults.
///
/// # Safety
/// `options` must be NULL or point at an `AgnijsEngineOptions` whose
/// `struct_size` is set; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_engine_new(options: *const AgnijsEngineOptions, out: *mut AgnijsEngine) -> AgnijsStatus {
    ffi("agnijs_engine_new", || {
        let out = Out::new(out, "out")?;
        let opts = if options.is_null() {
            AgnijsEngineOptions {
                struct_size: u32::try_from(std::mem::size_of::<AgnijsEngineOptions>()).unwrap_or(u32::MAX),
                ..AgnijsEngineOptions::default()
            }
        } else {
            // SAFETY: caller contract above; every field accepts any bit
            // pattern (integers, a raw pointer, optional fn pointers).
            unsafe { ptrs::versioned(options, "options")? }
        };
        if opts.flags & !AGNIJS_FLAG_NO_STRING_COMPILATION != 0 {
            return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("unknown option flags {:#x}", opts.flags)));
        }
        if !matches!(opts.regex_matcher, AGNIJS_REGEX_AUTO | AGNIJS_REGEX_BACKTRACKING_ONLY | AGNIJS_REGEX_LINEAR_WHEN_ELIGIBLE) {
            return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("unknown regex_matcher {}", opts.regex_matcher)));
        }
        let max_reentrancy = match opts.max_reentrancy {
            0 => AGNIJS_DEFAULT_MAX_REENTRANCY,
            n if n <= AGNIJS_MAX_REENTRANCY_LIMIT => n,
            n => {
                return Err(Fail::new(
                    AGNIJS_ERR_INVALID_ARGUMENT,
                    format!("max_reentrancy {n} exceeds {AGNIJS_MAX_REENTRANCY_LIMIT}"),
                ))
            }
        };
        let (id, tag) = registry::next_engine_id();
        let flag = Arc::new(AtomicBool::new(false));
        let host = Rc::new(CHost { engine: id, opts, interrupt_flag: flag.clone() });
        let mut engine = agnijs_js_vm::make_engine(host);
        if opts.memory_limit != 0 {
            engine.set_memory_limit(Some(opts.memory_limit));
        }
        registry::register(EngineState::new(id, tag, engine, max_reentrancy), flag)?;
        out.write(id);
        Ok(())
    })
}

/// Free an engine, every value and buffer it handed out, and its host
/// objects (their finalizers run). Refused with `AGNIJS_ERR_BUSY` from
/// inside the engine's own callbacks. A poisoned engine can be freed.
#[no_mangle]
pub extern "C" fn agnijs_engine_free(engine: AgnijsEngine) -> AgnijsStatus {
    ffi("agnijs_engine_free", || registry::unregister(engine))
}

/// Ask the engine to stop running JavaScript as soon as it next polls
/// (the running call returns `AGNIJS_INTERRUPTED`). The only function that
/// may be called from any thread.
#[no_mangle]
pub extern "C" fn agnijs_engine_request_interrupt(engine: AgnijsEngine) -> AgnijsStatus {
    ffi("agnijs_engine_request_interrupt", || registry::request_interrupt(engine))
}

/// The realm created with the engine.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_engine_main_realm(engine: AgnijsEngine, out: *mut AgnijsRealm) -> AgnijsStatus {
    ffi("agnijs_engine_main_realm", || {
        let out = Out::new(out, "out")?;
        let r = engine_op(engine, |eng| Ok(eng.main_realm()))?;
        out.write(with_state(engine, false, |st| Ok(st.realm_id(r)))?);
        Ok(())
    })
}

/// Create a new realm with its own global object and built-ins.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_realm_new(engine: AgnijsEngine, out: *mut AgnijsRealm) -> AgnijsStatus {
    ffi("agnijs_realm_new", || {
        let out = Out::new(out, "out")?;
        let r = engine_op(engine, |eng| eng_res(engine, eng.create_realm()))?;
        out.write(with_state(engine, false, |st| Ok(st.realm_id(r)))?);
        Ok(())
    })
}

fn eng_res<T>(e: AgnijsEngine, r: api::Result<T>) -> Res<T> {
    eng(e, r)
}

/// The global object of a realm.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_global_object(engine: AgnijsEngine, realm_id: AgnijsRealm, out: *mut AgnijsValue) -> AgnijsStatus {
    ffi("agnijs_global_object", || {
        let out = Out::new(out, "out")?;
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.global_object(r)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

// ---- evaluation ------------------------------------------------------------------------

/// Evaluate a classic script. `name` names it in stack traces. On
/// `AGNIJS_EXCEPTION`, `agnijs_take_exception` returns what was thrown.
///
/// # Safety
/// `name` and `source` must point at their lengths in readable bytes;
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_eval_script(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    name: *const c_char,
    name_len: usize,
    source: *const c_char,
    source_len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_eval_script", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let name = unsafe { ptrs::utf8(name, name_len, "name")? };
        // SAFETY: caller contract above.
        let source = unsafe { ptrs::utf8(source, source_len, "source")? };
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.eval_script(r, name, source)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// Load and evaluate a module through the resolve and load hooks. `out`
/// receives a promise for the module namespace, settled by running jobs.
///
/// # Safety
/// `specifier` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_load_module(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    specifier: *const c_char,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_load_module", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let spec = unsafe { ptrs::utf8(specifier, len, "specifier")? };
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.load_module(r, spec)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// Forget the module registered under a resolved specifier so the next
/// import loads it again. `released` (may be NULL) receives 1 if a record
/// was dropped.
///
/// # Safety
/// `specifier` must point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn agnijs_release_module(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    specifier: *const c_char,
    len: usize,
    released: *mut i32,
) -> AgnijsStatus {
    ffi("agnijs_release_module", || {
        let out = Out::optional(released, "released")?;
        // SAFETY: caller contract above.
        let spec = unsafe { ptrs::utf8(specifier, len, "specifier")? };
        let r = realm(engine, realm_id)?;
        let done = engine_op(engine, |eng| eng_res(engine, eng.release_module(r, spec)))?;
        if let Some(o) = out {
            o.write(i32::from(done));
        }
        Ok(())
    })
}

/// Run every pending job (promise reactions). `ran` (may be NULL) receives
/// the number run. An exception escaping a job stops the drain.
///
/// # Safety
/// `ran` must be NULL or writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_run_jobs(engine: AgnijsEngine, realm_id: AgnijsRealm, ran: *mut usize) -> AgnijsStatus {
    ffi("agnijs_run_jobs", || {
        let out = Out::optional(ran, "ran")?;
        let r = realm(engine, realm_id)?;
        let n = engine_op(engine, |eng| eng_res(engine, eng.run_jobs(r)))?;
        if let Some(o) = out {
            o.write(n);
        }
        Ok(())
    })
}

/// Whether jobs are waiting (1) or not (0).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_has_pending_jobs(engine: AgnijsEngine, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_has_pending_jobs", || {
        let out = Out::new(out, "out")?;
        let b = engine_op(engine, |eng| Ok(eng.has_pending_jobs()))?;
        out.write(i32::from(b));
        Ok(())
    })
}

// ---- bytecode ---------------------------------------------------------------------------

/// Compile a classic script to bytecode for `agnijs_eval_bytecode`.
///
/// # Safety
/// As `agnijs_eval_script`.
#[no_mangle]
pub unsafe extern "C" fn agnijs_compile_script(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    name: *const c_char,
    name_len: usize,
    source: *const c_char,
    source_len: usize,
    out: *mut AgnijsBuffer,
) -> AgnijsStatus {
    ffi("agnijs_compile_script", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let name = unsafe { ptrs::utf8(name, name_len, "name")? };
        // SAFETY: caller contract above.
        let source = unsafe { ptrs::utf8(source, source_len, "source")? };
        let r = realm(engine, realm_id)?;
        let b = engine_op(engine, |eng| eng_res(engine, eng.compile_script(r, name, source)))?;
        out.write(put_bytes(engine, b)?);
        Ok(())
    })
}

/// Compile a module to bytecode, for a load hook to answer with
/// `AGNIJS_MODULE_BYTECODE`.
///
/// # Safety
/// As `agnijs_eval_script`.
#[no_mangle]
pub unsafe extern "C" fn agnijs_compile_module(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    specifier: *const c_char,
    specifier_len: usize,
    source: *const c_char,
    source_len: usize,
    out: *mut AgnijsBuffer,
) -> AgnijsStatus {
    ffi("agnijs_compile_module", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let spec = unsafe { ptrs::utf8(specifier, specifier_len, "specifier")? };
        // SAFETY: caller contract above.
        let source = unsafe { ptrs::utf8(source, source_len, "source")? };
        let r = realm(engine, realm_id)?;
        let b = engine_op(engine, |eng| eng_res(engine, eng.compile_module(r, spec, source)))?;
        out.write(put_bytes(engine, b)?);
        Ok(())
    })
}

/// Evaluate bytecode. Bytecode from another engine or format version, or
/// corrupt bytecode, returns `AGNIJS_ERR_BYTECODE`.
///
/// # Safety
/// `data` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_eval_bytecode(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    data: *const u8,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_eval_bytecode", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let bytes = unsafe { ptrs::bytes(data, len, "data")? };
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.eval_bytecode(r, bytes)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// Borrow a buffer's bytes. The pointer stays valid until the buffer or
/// the engine is freed; do not write through it.
///
/// # Safety
/// `data` and `len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_buffer_data(
    engine: AgnijsEngine,
    buffer: AgnijsBuffer,
    data: *mut *const u8,
    len: *mut usize,
) -> AgnijsStatus {
    ffi("agnijs_buffer_data", || {
        let d = Out::new(data, "data")?;
        let l = Out::new(len, "len")?;
        let (p, n) = with_state(engine, true, |st| {
            let b = st.table.bytes(buffer, "buffer")?;
            Ok((b.as_ptr(), b.len()))
        })?;
        d.write(p);
        l.write(n);
        Ok(())
    })
}

/// Free a buffer. Freeing it twice returns `AGNIJS_ERR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn agnijs_buffer_free(engine: AgnijsEngine, buffer: AgnijsBuffer) -> AgnijsStatus {
    ffi("agnijs_buffer_free", || {
        let entry = with_state(engine, true, |st| st.table.remove_kind(buffer, true, "buffer"))?;
        drop(entry);
        Ok(())
    })
}

// ---- limits -------------------------------------------------------------------------------

/// Cap the heap in bytes (0 removes the cap). Allocation beyond it fails
/// the running call with `AGNIJS_OUT_OF_MEMORY`.
#[no_mangle]
pub extern "C" fn agnijs_set_memory_limit(engine: AgnijsEngine, bytes: usize) -> AgnijsStatus {
    ffi("agnijs_set_memory_limit", || {
        engine_op(engine, |eng| {
            eng.set_memory_limit(if bytes == 0 { None } else { Some(bytes) });
            Ok(())
        })
    })
}

/// Heap and handle statistics.
///
/// # Safety
/// `out` must point at an `AgnijsMemoryUsage` whose `struct_size` is set.
#[no_mangle]
pub unsafe extern "C" fn agnijs_memory_usage(engine: AgnijsEngine, out: *mut AgnijsMemoryUsage) -> AgnijsStatus {
    ffi("agnijs_memory_usage", || {
        // SAFETY: caller contract above.
        let out = unsafe { ptrs::versioned_out(out, "out")? };
        let (m, live) = engine_op(engine, |eng| {
            eng.reclaim_handles();
            Ok((eng.memory_usage(), eng.live_handles()))
        })?;
        let host_handles = with_state(engine, false, |st| Ok(st.table.live()))?;
        out.write(AgnijsMemoryUsage {
            struct_size: u32::try_from(std::mem::size_of::<AgnijsMemoryUsage>()).unwrap_or(u32::MAX),
            heap_bytes: m.heap_bytes,
            object_count: m.object_count,
            host_handles,
            engine_handles: live,
        });
        Ok(())
    })
}

/// Run a full garbage collection now (deferred to the next safe point when
/// called from inside a host function).
#[no_mangle]
pub extern "C" fn agnijs_collect_garbage(engine: AgnijsEngine) -> AgnijsStatus {
    ffi("agnijs_collect_garbage", || {
        engine_op(engine, |eng| {
            eng.reclaim_handles();
            eng.collect_garbage();
            Ok(())
        })
    })
}

// ---- exceptions ---------------------------------------------------------------------------

/// Take the exception the last `AGNIJS_EXCEPTION` result left behind.
/// Returns `AGNIJS_ERR_NOT_FOUND` when there is none.
///
/// # Safety
/// `out` must point at an `AgnijsException` whose `struct_size` is set.
#[no_mangle]
pub unsafe extern "C" fn agnijs_take_exception(engine: AgnijsEngine, out: *mut AgnijsException) -> AgnijsStatus {
    ffi("agnijs_take_exception", || {
        // SAFETY: caller contract above.
        let out = unsafe { ptrs::versioned_out(out, "out")? };
        let ex = with_state(engine, true, |st| {
            st.exception.take().ok_or_else(|| Fail::new(AGNIJS_ERR_NOT_FOUND, "no pending exception"))
        })?;
        let mut stack = String::new();
        for (i, f) in ex.stack.iter().enumerate() {
            if i > 0 {
                stack.push('\n');
            }
            let function = if f.function.is_empty() { "<anonymous>" } else { f.function.as_str() };
            stack.push_str(&format!("{function} ({}:{}:{})", f.script, f.line, f.column));
        }
        let frame_count = u32::try_from(ex.stack.len()).unwrap_or(u32::MAX);
        let ids = with_state(engine, true, |st| {
            let value = st.table.insert(Entry::Value(ex.value))?;
            let name = st.table.insert(Entry::Bytes(ex.name.into_bytes().into_boxed_slice()))?;
            let message = st.table.insert(Entry::Bytes(ex.message.into_bytes().into_boxed_slice()))?;
            let stack = st.table.insert(Entry::Bytes(stack.into_bytes().into_boxed_slice()))?;
            Ok((value, name, message, stack))
        })?;
        out.write(AgnijsException {
            struct_size: u32::try_from(std::mem::size_of::<AgnijsException>()).unwrap_or(u32::MAX),
            frame_count,
            value: ids.0,
            name: ids.1,
            message: ids.2,
            stack: ids.3,
        });
        Ok(())
    })
}

/// From inside a host function: throw `value` once it returns
/// `AGNIJS_EXCEPTION`. The handle stays the caller's.
#[no_mangle]
pub extern "C" fn agnijs_throw(engine: AgnijsEngine, value_id: AgnijsValue) -> AgnijsStatus {
    ffi("agnijs_throw", || {
        with_state(engine, false, |st| {
            let v = st.table.value(value_id, "value")?;
            st.pending_throw = Some(v);
            Ok(())
        })
    })
}

// ---- values in ------------------------------------------------------------------------------

macro_rules! constructor {
    ($(#[$doc:meta])* $name:ident, $v:expr $(, $arg:ident : $ty:ty)*) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `out` must be writable.
        #[no_mangle]
        pub unsafe extern "C" fn $name(engine: AgnijsEngine $(, $arg: $ty)*, out: *mut AgnijsValue) -> AgnijsStatus {
            ffi(stringify!($name), || {
                let out = Out::new(out, "out")?;
                out.write(with_state(engine, false, |st| st.table.insert(Entry::Value($v)))?);
                Ok(())
            })
        }
    };
}

constructor!(
    /// `undefined`.
    agnijs_new_undefined, api::Value::Undefined
);
constructor!(
    /// `null`.
    agnijs_new_null, api::Value::Null
);
constructor!(
    /// A boolean (any nonzero is true).
    agnijs_new_bool, api::Value::Bool(b != 0), b: i32
);
constructor!(
    /// A number.
    agnijs_new_number, api::Value::Number(n), n: f64
);

/// A string from UTF-8.
///
/// # Safety
/// `text` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_string_utf8(
    engine: AgnijsEngine,
    text: *const c_char,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_string_utf8", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let s = unsafe { ptrs::utf8(text, len, "text")? };
        out.write(put_checked(engine, api::Value::String(api::JsString::from(s)))?);
        Ok(())
    })
}

/// A string from UTF-16 code units, kept exactly (lone surrogates too).
///
/// # Safety
/// `units` must point at `len` readable, 2-byte-aligned code units; `out`
/// must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_string_utf16(
    engine: AgnijsEngine,
    units: *const u16,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_string_utf16", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let u = unsafe { ptrs::utf16(units, len, "units")? };
        out.write(put_checked(engine, api::Value::String(api::JsString::from_utf16(u.to_vec())))?);
        Ok(())
    })
}

/// A BigInt from its decimal text (`-?[0-9]+`).
///
/// # Safety
/// `text` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_bigint(
    engine: AgnijsEngine,
    text: *const c_char,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_bigint", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let s = unsafe { ptrs::utf8(text, len, "text")? };
        let digits = s.strip_prefix('-').unwrap_or(s);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, "a BigInt is written -?[0-9]+"));
        }
        out.write(put_checked(engine, api::Value::BigInt(api::JsBigInt(s.to_string())))?);
        Ok(())
    })
}

fn put_checked(e: AgnijsEngine, v: api::Value) -> Res<AgnijsValue> {
    with_state(e, false, |st| st.table.insert(Entry::Value(v)))
}

/// A new plain object.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_object(engine: AgnijsEngine, realm_id: AgnijsRealm, out: *mut AgnijsValue) -> AgnijsStatus {
    ffi("agnijs_new_object", || {
        let out = Out::new(out, "out")?;
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.new_object(r)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// A new array of `count` values.
///
/// # Safety
/// `items` must point at `count` readable value ids; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_array(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    items: *const AgnijsValue,
    count: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_array", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let ids = unsafe { ptrs::handles(items, count, "items")? };
        let vals = values(engine, ids, "items[i]")?;
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.new_array(r, &vals)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// A new Error object of `kind` (`AGNIJS_ERROR`, `AGNIJS_TYPE_ERROR`, ...).
///
/// # Safety
/// `message` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_error(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    kind: i32,
    message: *const c_char,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_error", || {
        let out = Out::new(out, "out")?;
        let kind = match kind {
            AGNIJS_ERROR => api::ErrorKind::Error,
            AGNIJS_TYPE_ERROR => api::ErrorKind::TypeError,
            AGNIJS_RANGE_ERROR => api::ErrorKind::RangeError,
            AGNIJS_SYNTAX_ERROR => api::ErrorKind::SyntaxError,
            AGNIJS_REFERENCE_ERROR => api::ErrorKind::ReferenceError,
            AGNIJS_EVAL_ERROR => api::ErrorKind::EvalError,
            AGNIJS_URI_ERROR => api::ErrorKind::UriError,
            other => return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("unknown error kind {other}"))),
        };
        // SAFETY: caller contract above.
        let msg = unsafe { ptrs::utf8(message, len, "message")? };
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.new_error(r, kind, msg)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// A new symbol with a description.
///
/// # Safety
/// `description` must point at `len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_symbol(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    description: *const c_char,
    len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_symbol", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let d = unsafe { ptrs::utf8(description, len, "description")? };
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.new_symbol(r, d)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

// ---- values out ---------------------------------------------------------------------------

/// Free a value. Freeing it twice returns `AGNIJS_ERR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn agnijs_value_free(engine: AgnijsEngine, value_id: AgnijsValue) -> AgnijsStatus {
    ffi("agnijs_value_free", || {
        let entry = with_state(engine, true, |st| st.table.remove_kind(value_id, false, "value"))?;
        drop(entry);
        Ok(())
    })
}

/// A second, independent handle to the same value.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_value_dup(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut AgnijsValue) -> AgnijsStatus {
    ffi("agnijs_value_dup", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        out.write(put_checked(engine, v)?);
        Ok(())
    })
}

/// `typeof`, as an `AGNIJS_TYPE_*` constant (`null` is `AGNIJS_TYPE_NULL`).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_value_type(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_value_type", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        let t = engine_op(engine, |eng| Ok(eng.type_of(&v)))?;
        out.write(match t {
            api::TypeOf::Undefined => AGNIJS_TYPE_UNDEFINED,
            api::TypeOf::Null => AGNIJS_TYPE_NULL,
            api::TypeOf::Boolean => AGNIJS_TYPE_BOOLEAN,
            api::TypeOf::Number => AGNIJS_TYPE_NUMBER,
            api::TypeOf::String => AGNIJS_TYPE_STRING,
            api::TypeOf::BigInt => AGNIJS_TYPE_BIGINT,
            api::TypeOf::Symbol => AGNIJS_TYPE_SYMBOL,
            api::TypeOf::Object => AGNIJS_TYPE_OBJECT,
            api::TypeOf::Function => AGNIJS_TYPE_FUNCTION,
        });
        Ok(())
    })
}

/// The number in a number value (`AGNIJS_ERR_TYPE_MISMATCH` otherwise).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_value_get_number(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut f64) -> AgnijsStatus {
    ffi("agnijs_value_get_number", || {
        let out = Out::new(out, "out")?;
        match value(engine, value_id, "value")? {
            api::Value::Number(n) => {
                out.write(n);
                Ok(())
            }
            _ => Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not a number")),
        }
    })
}

/// The boolean in a boolean value, as 0 or 1.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_value_get_bool(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_value_get_bool", || {
        let out = Out::new(out, "out")?;
        match value(engine, value_id, "value")? {
            api::Value::Bool(b) => {
                out.write(i32::from(b));
                Ok(())
            }
            _ => Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not a boolean")),
        }
    })
}

/// Copy a string value (or a BigInt's decimal text) as UTF-8 plus a NUL
/// into `dst`; lone surrogates become U+FFFD. `needed` (may be NULL)
/// receives the length without the NUL; `AGNIJS_ERR_BUFFER_TOO_SMALL` when
/// `cap` is not more than that. Runs no JavaScript.
///
/// # Safety
/// `dst` must point at `cap` writable bytes (or be NULL when `cap` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_string_to_utf8(
    engine: AgnijsEngine,
    value_id: AgnijsValue,
    dst: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> AgnijsStatus {
    ffi("agnijs_string_to_utf8", || {
        let needed = Out::optional(needed, "needed")?;
        let text = match value(engine, value_id, "value")? {
            api::Value::String(s) => s.to_string_lossy(),
            api::Value::BigInt(b) => b.0,
            _ => return Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not a string; use agnijs_to_string")),
        };
        // SAFETY: caller contract above.
        unsafe { ptrs::copy_out(text.as_bytes(), dst.cast::<u8>(), cap, needed) }
    })
}

/// Copy a string value's exact UTF-16 code units plus a 0 unit into `dst`.
/// Sizes are in code units.
///
/// # Safety
/// `dst` must point at `cap` writable, aligned code units (or be NULL when
/// `cap` is 0).
#[no_mangle]
pub unsafe extern "C" fn agnijs_string_to_utf16(
    engine: AgnijsEngine,
    value_id: AgnijsValue,
    dst: *mut u16,
    cap: usize,
    needed: *mut usize,
) -> AgnijsStatus {
    ffi("agnijs_string_to_utf16", || {
        let needed = Out::optional(needed, "needed")?;
        let s = match value(engine, value_id, "value")? {
            api::Value::String(s) => s,
            _ => return Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not a string")),
        };
        // SAFETY: caller contract above.
        unsafe { ptrs::copy_out(s.units(), dst, cap, needed) }
    })
}

/// `String(value)` as a new string value (may run JavaScript).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_to_string(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    value_id: AgnijsValue,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_to_string", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        let r = realm(engine, realm_id)?;
        let s = match v {
            api::Value::String(_) => v,
            // Engine::to_string is lossy, so String() goes through the
            // engine's own String function, which keeps lone surrogates.
            other => engine_op(engine, |eng| {
                let g = eng_res(engine, eng.global_object(r))?;
                let f = eng_res(engine, eng.get(r, &g, &api::PropertyKey::from("String")))?;
                eng_res(engine, eng.call(r, &f, &api::Value::Undefined, &[other]))
            })?,
        };
        out.write(put(engine, s)?);
        Ok(())
    })
}

/// `Number(value)` (may run JavaScript).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_to_number(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    value_id: AgnijsValue,
    out: *mut f64,
) -> AgnijsStatus {
    ffi("agnijs_to_number", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        let r = realm(engine, realm_id)?;
        out.write(engine_op(engine, |eng| eng_res(engine, eng.to_number(r, &v)))?);
        Ok(())
    })
}

/// `a === b`, as 0 or 1.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_strict_equals(engine: AgnijsEngine, a: AgnijsValue, b: AgnijsValue, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_strict_equals", || {
        let out = Out::new(out, "out")?;
        let x = value(engine, a, "a")?;
        let y = value(engine, b, "b")?;
        out.write(i32::from(engine_op(engine, |eng| Ok(eng.strict_equals(&x, &y)))?));
        Ok(())
    })
}

/// `Array.isArray(value)`, as 0 or 1.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_is_array(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_is_array", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        out.write(i32::from(engine_op(engine, |eng| Ok(eng.is_array(&v)))?));
        Ok(())
    })
}

/// Whether `value` is a promise, as 0 or 1.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_is_promise(engine: AgnijsEngine, value_id: AgnijsValue, out: *mut i32) -> AgnijsStatus {
    ffi("agnijs_is_promise", || {
        let out = Out::new(out, "out")?;
        let v = value(engine, value_id, "value")?;
        out.write(i32::from(engine_op(engine, |eng| Ok(eng.is_promise(&v)))?));
        Ok(())
    })
}

/// A promise's state (`AGNIJS_PROMISE_*`) and, once settled, its result
/// (`result` may be NULL; it receives 0 while pending).
///
/// # Safety
/// `state` must be writable; `result` NULL or writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_promise_state(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    promise: AgnijsValue,
    state: *mut i32,
    result: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_promise_state", || {
        let state = Out::new(state, "state")?;
        let result = Out::optional(result, "result")?;
        let p = object(engine, promise, "promise")?;
        let r = realm(engine, realm_id)?;
        let st = engine_op(engine, |eng| {
            if !eng.is_promise(&p) {
                return Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not a promise"));
            }
            eng_res(engine, eng.promise_state(r, &p))
        })?;
        let (code, v) = match st {
            api::PromiseState::Pending => (AGNIJS_PROMISE_PENDING, None),
            api::PromiseState::Fulfilled(v) => (AGNIJS_PROMISE_FULFILLED, Some(v)),
            api::PromiseState::Rejected(v) => (AGNIJS_PROMISE_REJECTED, Some(v)),
        };
        if let Some(out) = result {
            out.write(match v {
                Some(v) => put(engine, v)?,
                None => 0,
            });
        }
        state.write(code);
        Ok(())
    })
}

// ---- properties and calls ---------------------------------------------------------------

/// `object[key]` with a UTF-8 key.
///
/// # Safety
/// `key` must point at `key_len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_get(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: *const c_char,
    key_len: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_get", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let k = unsafe { key_utf8(key, key_len)? };
        get_impl(engine, realm_id, object_id, k, out)
    })
}

/// `object[key]` with a string or symbol value as the key.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_get_by_value(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: AgnijsValue,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_get_by_value", || {
        let out = Out::new(out, "out")?;
        let k = key_value(engine, key)?;
        get_impl(engine, realm_id, object_id, k, out)
    })
}

fn get_impl(e: AgnijsEngine, realm_id: AgnijsRealm, object_id: AgnijsValue, k: api::PropertyKey, out: Out<AgnijsValue>) -> Res<()> {
    let o = object(e, object_id, "object")?;
    let r = realm(e, realm_id)?;
    let v = engine_op(e, |eng| eng_res(e, eng.get(r, &o, &k)))?;
    out.write(put(e, v)?);
    Ok(())
}

/// `object[key] = value` with a UTF-8 key (throws in strict-mode fashion
/// when the property cannot be set).
///
/// # Safety
/// `key` must point at `key_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn agnijs_set(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: *const c_char,
    key_len: usize,
    value_id: AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_set", || {
        // SAFETY: caller contract above.
        let k = unsafe { key_utf8(key, key_len)? };
        set_impl(engine, realm_id, object_id, k, value_id)
    })
}

/// `object[key] = value` with a string or symbol value as the key.
#[no_mangle]
pub extern "C" fn agnijs_set_by_value(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: AgnijsValue,
    value_id: AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_set_by_value", || {
        let k = key_value(engine, key)?;
        set_impl(engine, realm_id, object_id, k, value_id)
    })
}

fn set_impl(e: AgnijsEngine, realm_id: AgnijsRealm, object_id: AgnijsValue, k: api::PropertyKey, value_id: AgnijsValue) -> Res<()> {
    let o = object(e, object_id, "object")?;
    let v = value(e, value_id, "value")?;
    let r = realm(e, realm_id)?;
    engine_op(e, |eng| eng_res(e, eng.set(r, &o, &k, &v)))
}

/// `key in object` with a UTF-8 key, as 0 or 1.
///
/// # Safety
/// `key` must point at `key_len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_has(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: *const c_char,
    key_len: usize,
    out: *mut i32,
) -> AgnijsStatus {
    ffi("agnijs_has", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let k = unsafe { key_utf8(key, key_len)? };
        let o = object(engine, object_id, "object")?;
        let r = realm(engine, realm_id)?;
        out.write(i32::from(engine_op(engine, |eng| eng_res(engine, eng.has(r, &o, &k)))?));
        Ok(())
    })
}

/// `delete object[key]` with a UTF-8 key; `out` (may be NULL) receives the
/// result as 0 or 1.
///
/// # Safety
/// `key` must point at `key_len` readable bytes; `out` NULL or writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_delete(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    key: *const c_char,
    key_len: usize,
    out: *mut i32,
) -> AgnijsStatus {
    ffi("agnijs_delete", || {
        let out = Out::optional(out, "out")?;
        // SAFETY: caller contract above.
        let k = unsafe { key_utf8(key, key_len)? };
        let o = object(engine, object_id, "object")?;
        let r = realm(engine, realm_id)?;
        let done = engine_op(engine, |eng| eng_res(engine, eng.delete(r, &o, &k)))?;
        if let Some(out) = out {
            out.write(i32::from(done));
        }
        Ok(())
    })
}

/// The object's own enumerable string keys, as a new array of strings.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_own_keys(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    object_id: AgnijsValue,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_own_keys", || {
        let out = Out::new(out, "out")?;
        let o = object(engine, object_id, "object")?;
        let r = realm(engine, realm_id)?;
        let arr = engine_op(engine, |eng| {
            let keys = eng_res(engine, eng.own_keys(r, &o))?;
            let items: Vec<api::Value> = keys
                .into_iter()
                .map(|k| match k {
                    api::PropertyKey::String(s) => api::Value::String(s),
                    api::PropertyKey::Symbol(h) => api::Value::Symbol(h),
                })
                .collect();
            eng_res(engine, eng.new_array(r, &items))
        })?;
        out.write(put(engine, arr)?);
        Ok(())
    })
}

/// Call `function` with `this` (0 for `undefined`) and `argc` arguments.
///
/// # Safety
/// `args` must point at `argc` readable value ids; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_call(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    function: AgnijsValue,
    this_value: AgnijsValue,
    args: *const AgnijsValue,
    argc: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_call", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let ids = unsafe { ptrs::handles(args, argc, "args")? };
        let f = value(engine, function, "function")?;
        let t = if this_value == 0 { api::Value::Undefined } else { value(engine, this_value, "this")? };
        let a = values(engine, ids, "args[i]")?;
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.call(r, &f, &t, &a)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// `new constructor(...args)`.
///
/// # Safety
/// `args` must point at `argc` readable value ids; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_construct(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    constructor: AgnijsValue,
    args: *const AgnijsValue,
    argc: usize,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_construct", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let ids = unsafe { ptrs::handles(args, argc, "args")? };
        let c = value(engine, constructor, "constructor")?;
        let a = values(engine, ids, "args[i]")?;
        let r = realm(engine, realm_id)?;
        let v = engine_op(engine, |eng| eng_res(engine, eng.construct(r, &c, &a)))?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

// ---- host functions and classes -----------------------------------------------------------

/// A JavaScript function that calls `function`. `user_data` is passed to
/// every call and to `finalize` (may be NULL) once the function object is
/// collected or the engine freed. On failure `finalize` is not called.
///
/// # Safety
/// `name` must point at `name_len` readable bytes; `out` must be writable;
/// `function` and `finalize` must be valid to call for the engine's
/// lifetime.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_function(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    name: *const c_char,
    name_len: usize,
    function: AgnijsHostFunction,
    user_data: *mut c_void,
    finalize: AgnijsFinalizer,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_function", || {
        let out = Out::new(out, "out")?;
        let Some(f) = function else { return Err(Fail::new(AGNIJS_ERR_NULL_POINTER, "function is NULL")) };
        // SAFETY: caller contract above.
        let name = unsafe { ptrs::utf8(name, name_len, "name")? };
        let r = realm(engine, realm_id)?;
        // Disarmed until the engine owns it, so a failed creation never
        // finalizes the caller's data.
        let (hf, armed) = host_function(engine, f, user_data, finalize);
        let v = engine_op(engine, |eng| {
            let v = eng_res(engine, eng.new_function(r, name, hf))?;
            armed.set(true);
            Ok(v)
        })?;
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// Register a host class. Instances made with `agnijs_new_host_object`
/// carry a `void*` the class's `finalize` receives when they are collected
/// or the engine is freed.
///
/// # Safety
/// `def` must point at an `AgnijsClassDef` whose `struct_size` is set and
/// whose `name` points at `name_len` readable bytes; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_register_class(engine: AgnijsEngine, def: *const AgnijsClassDef, out: *mut AgnijsClass) -> AgnijsStatus {
    ffi("agnijs_register_class", || {
        let out = Out::new(out, "out")?;
        // SAFETY: caller contract above.
        let d = unsafe { ptrs::versioned(def, "def")? };
        // SAFETY: caller contract above.
        let name = unsafe { ptrs::utf8(d.name, d.name_len, "def.name")? }.to_string();
        let id = engine_op(engine, |eng| {
            Ok(eng.register_host_class(api::HostClassDef { name, finalize: Some(Box::new(drop)) }))
        })?;
        let handle = with_state(engine, false, |st| {
            st.classes.push((id, d.finalize));
            Ok(st.class_id(st.classes.len().saturating_sub(1)))
        })?;
        out.write(handle);
        Ok(())
    })
}

/// A new instance of a host class holding `data`, with `prototype` (0 for
/// `Object.prototype`). On failure the data is not finalized.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_new_host_object(
    engine: AgnijsEngine,
    realm_id: AgnijsRealm,
    class: AgnijsClass,
    data: *mut c_void,
    prototype: AgnijsValue,
    out: *mut AgnijsValue,
) -> AgnijsStatus {
    ffi("agnijs_new_host_object", || {
        let out = Out::new(out, "out")?;
        let (index, class_id, finalize) = with_state(engine, false, |st| {
            let i = st.class(class)?;
            let (id, fin) = st.classes.get(i).copied().ok_or_else(|| Fail::new(AGNIJS_ERR_INVALID_HANDLE, "unknown class"))?;
            Ok((i, id, fin))
        })?;
        let proto = if prototype == 0 { None } else { Some(object(engine, prototype, "prototype")?) };
        let r = realm(engine, realm_id)?;
        let (obj, armed) = CObject::new(index, data, finalize);
        let v = engine_op(engine, |eng| eng_res(engine, eng.new_host_object(r, class_id, Box::new(obj), proto.as_ref())))?;
        armed.set(true);
        out.write(put(engine, v)?);
        Ok(())
    })
}

/// The `data` of a host object of `class` (`AGNIJS_ERR_TYPE_MISMATCH` for
/// any other value).
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn agnijs_host_object_data(
    engine: AgnijsEngine,
    value_id: AgnijsValue,
    class: AgnijsClass,
    out: *mut *mut c_void,
) -> AgnijsStatus {
    ffi("agnijs_host_object_data", || {
        let out = Out::new(out, "out")?;
        let index = with_state(engine, false, |st| st.class(class))?;
        let v = value(engine, value_id, "value")?;
        let data = engine_op(engine, |eng| {
            let obj = eng.host_object_data(&v);
            match obj.and_then(|o| o.as_any().downcast_ref::<CObject>()) {
                Some(c) if c.class == index => Ok(c.data),
                _ => Err(Fail::new(AGNIJS_ERR_TYPE_MISMATCH, "value is not an instance of this class")),
            }
        })?;
        out.write(data);
        Ok(())
    })
}
