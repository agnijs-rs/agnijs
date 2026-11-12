//! Types and constants of the C ABI. Everything here appears in
//! `include/agnijs.h`. Enumerations are plain integer constants, never Rust
//! enums: a value from C that is out of range is then an error the callee
//! reports, not undefined behaviour.

use std::ffi::{c_char, c_void};

/// ABI major version: changes only when an existing function, constant or
/// struct layout changes incompatibly (see docs/c-api.md).
pub const AGNIJS_ABI_VERSION_MAJOR: u32 = 1;
/// ABI minor version: grows when functions, constants or trailing struct
/// fields are added.
pub const AGNIJS_ABI_VERSION_MINOR: u32 = 2;

/// The result of every function that can fail. Zero is success, positive
/// values are outcomes of running JavaScript, negative values are errors in
/// the call itself. [`crate::agnijs_last_error`] has the message.
pub type AgnijsStatus = i32;

/// Success.
pub const AGNIJS_OK: AgnijsStatus = 0;
/// JavaScript threw; [`crate::agnijs_take_exception`] returns what.
pub const AGNIJS_EXCEPTION: AgnijsStatus = 1;
/// The interrupt hook or [`crate::agnijs_engine_request_interrupt`] stopped execution.
pub const AGNIJS_INTERRUPTED: AgnijsStatus = 2;
/// The engine's memory limit was reached.
pub const AGNIJS_OUT_OF_MEMORY: AgnijsStatus = 3;
/// The fuel budget set with [`crate::agnijs_set_fuel`] is spent (ABI 1.1).
pub const AGNIJS_OUT_OF_FUEL: AgnijsStatus = 4;
/// A required pointer was NULL.
pub const AGNIJS_ERR_NULL_POINTER: AgnijsStatus = -1;
/// The engine id is unknown or the engine was freed.
pub const AGNIJS_ERR_INVALID_ENGINE: AgnijsStatus = -2;
/// The engine belongs to another thread.
pub const AGNIJS_ERR_WRONG_THREAD: AgnijsStatus = -3;
/// A value, buffer, class or sink id is null, freed, stale or not of the
/// expected kind.
pub const AGNIJS_ERR_INVALID_HANDLE: AgnijsStatus = -4;
/// The id belongs to another engine.
pub const AGNIJS_ERR_WRONG_ENGINE: AgnijsStatus = -5;
/// The realm id is unknown to this engine.
pub const AGNIJS_ERR_INVALID_REALM: AgnijsStatus = -6;
/// An argument is out of range: a bad struct size, flag, enumeration value,
/// length or alignment.
pub const AGNIJS_ERR_INVALID_ARGUMENT: AgnijsStatus = -7;
/// Text that must be UTF-8 (or well-formed UTF-16) is not.
pub const AGNIJS_ERR_INVALID_UTF8: AgnijsStatus = -8;
/// The value is not of the type the function needs.
pub const AGNIJS_ERR_TYPE_MISMATCH: AgnijsStatus = -9;
/// The engine is in use by the calling code (a reentrant call that is not
/// allowed, or freeing an engine from inside one of its own callbacks).
pub const AGNIJS_ERR_BUSY: AgnijsStatus = -10;
/// An earlier panic inside this engine poisoned it; only freeing it works.
pub const AGNIJS_ERR_POISONED: AgnijsStatus = -11;
/// A panic was caught inside this call. The engine (if any) is poisoned.
pub const AGNIJS_ERR_PANIC: AgnijsStatus = -12;
/// Bytecode from another engine or format version, or corrupt bytecode.
pub const AGNIJS_ERR_BYTECODE: AgnijsStatus = -13;
/// A host hook failed (module resolution or loading).
pub const AGNIJS_ERR_HOST: AgnijsStatus = -14;
/// The engine reported a failure that is not a JavaScript exception.
pub const AGNIJS_ERR_ENGINE: AgnijsStatus = -15;
/// The engine does not provide this capability.
pub const AGNIJS_ERR_UNSUPPORTED: AgnijsStatus = -16;
/// The output buffer is too small; the needed size was written.
pub const AGNIJS_ERR_BUFFER_TOO_SMALL: AgnijsStatus = -17;
/// A fixed limit was reached (reentrancy depth, handle table size).
pub const AGNIJS_ERR_LIMIT: AgnijsStatus = -18;
/// There is nothing to return (for example no pending exception).
pub const AGNIJS_ERR_NOT_FOUND: AgnijsStatus = -19;

/// An engine: one heap, its realms and its values. Zero is never valid.
pub type AgnijsEngine = u64;
/// A realm of an engine. Zero is never valid.
pub type AgnijsRealm = u64;
/// A JavaScript value held by the host. Zero is never valid; where a
/// function takes an optional value, zero means "none".
pub type AgnijsValue = u64;
/// Bytes owned by the engine (bytecode, exception text). Zero is never valid.
pub type AgnijsBuffer = u64;
/// A host class registered with an engine. Zero is never valid.
pub type AgnijsClass = u64;
/// Where a module hook writes its answer. Valid only during the hook call.
pub type AgnijsSink = u64;

/// `typeof` results from [`crate::agnijs_value_type`].
pub const AGNIJS_TYPE_UNDEFINED: i32 = 0;
pub const AGNIJS_TYPE_NULL: i32 = 1;
pub const AGNIJS_TYPE_BOOLEAN: i32 = 2;
pub const AGNIJS_TYPE_NUMBER: i32 = 3;
pub const AGNIJS_TYPE_STRING: i32 = 4;
pub const AGNIJS_TYPE_BIGINT: i32 = 5;
pub const AGNIJS_TYPE_SYMBOL: i32 = 6;
pub const AGNIJS_TYPE_OBJECT: i32 = 7;
pub const AGNIJS_TYPE_FUNCTION: i32 = 8;

/// Error kinds for [`crate::agnijs_new_error`].
pub const AGNIJS_ERROR: i32 = 0;
pub const AGNIJS_TYPE_ERROR: i32 = 1;
pub const AGNIJS_RANGE_ERROR: i32 = 2;
pub const AGNIJS_SYNTAX_ERROR: i32 = 3;
pub const AGNIJS_REFERENCE_ERROR: i32 = 4;
pub const AGNIJS_EVAL_ERROR: i32 = 5;
pub const AGNIJS_URI_ERROR: i32 = 6;

/// Promise states from [`crate::agnijs_promise_state`].
pub const AGNIJS_PROMISE_PENDING: i32 = 0;
pub const AGNIJS_PROMISE_FULFILLED: i32 = 1;
pub const AGNIJS_PROMISE_REJECTED: i32 = 2;

/// Module kinds for [`crate::agnijs_sink_set_module`].
pub const AGNIJS_MODULE_SOURCE: i32 = 0;
pub const AGNIJS_MODULE_JSON: i32 = 1;
pub const AGNIJS_MODULE_BYTECODE: i32 = 2;

/// Console levels passed to the console hook.
pub const AGNIJS_CONSOLE_LOG: i32 = 0;
pub const AGNIJS_CONSOLE_INFO: i32 = 1;
pub const AGNIJS_CONSOLE_WARN: i32 = 2;
pub const AGNIJS_CONSOLE_ERROR: i32 = 3;
pub const AGNIJS_CONSOLE_DEBUG: i32 = 4;

/// Rejection events passed to the rejection hook.
pub const AGNIJS_REJECTION_UNHANDLED: i32 = 0;
pub const AGNIJS_REJECTION_HANDLED: i32 = 1;

/// Regular-expression matcher policies ([`AgnijsEngineOptions::regex_matcher`]).
pub const AGNIJS_REGEX_AUTO: i32 = 0;
pub const AGNIJS_REGEX_BACKTRACKING_ONLY: i32 = 1;
pub const AGNIJS_REGEX_LINEAR_WHEN_ELIGIBLE: i32 = 2;

/// [`AgnijsEngineOptions::flags`]: refuse `eval` and `new Function`.
pub const AGNIJS_FLAG_NO_STRING_COMPILATION: u32 = 1;

/// The default and the largest nesting of host callbacks that call back
/// into the engine ([`AgnijsEngineOptions::max_reentrancy`]).
pub const AGNIJS_DEFAULT_MAX_REENTRANCY: u32 = 32;
pub const AGNIJS_MAX_REENTRANCY_LIMIT: u32 = 256;

/// The default [`AgnijsEngineOptions::stack_budget`]: 512 KiB.
pub const AGNIJS_DEFAULT_STACK_BUDGET: usize = 512 * 1024;
/// Stack the library may use past the budget before it notices (between
/// checks, and to build the error). Keep this much free beyond the budget.
pub const AGNIJS_STACK_HEADROOM: usize = 256 * 1024;
// The C API uses the Rust engine's defaults.
const _: () = assert!(AGNIJS_DEFAULT_STACK_BUDGET == agnijs_js_parser::stack::DEFAULT_BUDGET);
const _: () = assert!(AGNIJS_STACK_HEADROOM == agnijs_js_parser::stack::HEADROOM);
/// The smallest accepted nonzero [`AgnijsEngineOptions::stack_budget`].
pub const AGNIJS_MIN_STACK_BUDGET: usize = 64 * 1024;

/// Polled while JavaScript runs; return nonzero to interrupt.
pub type AgnijsInterruptHook = Option<unsafe extern "C-unwind" fn(user_data: *mut c_void) -> i32>;

/// A `console` call; `text` is the arguments joined by spaces, UTF-8,
/// NUL-terminated, valid only during the call.
pub type AgnijsConsoleHook = Option<
    unsafe extern "C-unwind" fn(user_data: *mut c_void, realm: AgnijsRealm, level: i32, text: *const c_char, len: usize),
>;

/// Resolve `specifier` imported from `referrer` (NULL for a root load) to a
/// canonical specifier, written with [`crate::agnijs_sink_set_string`].
/// Strings are UTF-8 and NUL-terminated. Without this hook the specifier
/// is used as it is.
pub type AgnijsResolveHook = Option<
    unsafe extern "C-unwind" fn(
        user_data: *mut c_void,
        referrer: *const c_char,
        referrer_len: usize,
        specifier: *const c_char,
        specifier_len: usize,
        sink: AgnijsSink,
    ) -> AgnijsStatus,
>;

/// Load a resolved module, written with [`crate::agnijs_sink_set_module`].
pub type AgnijsLoadHook = Option<
    unsafe extern "C-unwind" fn(
        user_data: *mut c_void,
        resolved: *const c_char,
        resolved_len: usize,
        sink: AgnijsSink,
    ) -> AgnijsStatus,
>;

/// Populate `import.meta` of `module`. `meta` is valid only during the call.
pub type AgnijsImportMetaHook = Option<
    unsafe extern "C-unwind" fn(
        user_data: *mut c_void,
        engine: AgnijsEngine,
        realm: AgnijsRealm,
        module: *const c_char,
        module_len: usize,
        meta: AgnijsValue,
    ) -> AgnijsStatus,
>;

/// Promise-rejection tracking. `reason` is zero for
/// [`AGNIJS_REJECTION_HANDLED`]. Both values are valid only during the call.
pub type AgnijsRejectionHook = Option<
    unsafe extern "C-unwind" fn(
        user_data: *mut c_void,
        engine: AgnijsEngine,
        realm: AgnijsRealm,
        event: i32,
        promise: AgnijsValue,
        reason: AgnijsValue,
    ),
>;

/// The clock behind `Date.now()`: milliseconds since the Unix epoch.
pub type AgnijsClockHook = Option<unsafe extern "C-unwind" fn(user_data: *mut c_void) -> f64>;

/// Releases host data (a host function's `user_data`, a host object's data).
pub type AgnijsFinalizer = Option<unsafe extern "C-unwind" fn(data: *mut c_void)>;

/// A host function. Write the result to `*result` (the engine takes that
/// handle over; leave it zero for `undefined`) and return [`AGNIJS_OK`];
/// return [`AGNIJS_EXCEPTION`] after [`crate::agnijs_throw`] to throw, or
/// return the `AGNIJS_EXCEPTION` a nested call gave you (without taking its
/// exception) to pass that exception on; [`AGNIJS_INTERRUPTED`] and
/// [`AGNIJS_OUT_OF_MEMORY`] pass on as such; any other status throws an
/// `Error` naming it.
pub type AgnijsHostFunction = Option<
    unsafe extern "C-unwind" fn(
        engine: AgnijsEngine,
        call: *const AgnijsCallInfo,
        user_data: *mut c_void,
        result: *mut AgnijsValue,
    ) -> AgnijsStatus,
>;

/// The arguments of a host function call. Every handle in it is owned by
/// the engine and freed when the host function returns; duplicate one with
/// [`crate::agnijs_value_dup`] to keep it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AgnijsCallInfo {
    /// `sizeof(AgnijsCallInfo)` as built into the library.
    pub struct_size: u32,
    pub realm: AgnijsRealm,
    pub this_value: AgnijsValue,
    pub args: *const AgnijsValue,
    pub argc: usize,
    /// Zero unless called with `new`. Host functions are not constructors
    /// in ABI 1.0 (`new f()` throws a TypeError before calling), so this is
    /// zero for now; wrap the function in a JavaScript class or function to
    /// construct (see the C++ example).
    pub new_target: AgnijsValue,
}

/// Engine creation options. Set `struct_size` to `sizeof(AgnijsEngineOptions)`
/// and zero every field you do not use: zero always means the default.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AgnijsEngineOptions {
    pub struct_size: u32,
    /// `AGNIJS_FLAG_*` bits; unknown bits are an error.
    pub flags: u32,
    /// Heap cap in bytes; zero for none.
    pub memory_limit: usize,
    /// `AGNIJS_REGEX_*`.
    pub regex_matcher: i32,
    /// Nesting limit for host callbacks that call back into the engine;
    /// zero for [`AGNIJS_DEFAULT_MAX_REENTRANCY`].
    pub max_reentrancy: u32,
    /// Passed to every hook below.
    pub user_data: *mut c_void,
    pub interrupt: AgnijsInterruptHook,
    pub console: AgnijsConsoleHook,
    pub resolve_module: AgnijsResolveHook,
    pub load_module: AgnijsLoadHook,
    pub import_meta: AgnijsImportMetaHook,
    pub promise_rejection: AgnijsRejectionHook,
    pub now_ms: AgnijsClockHook,
    /// Native stack, in bytes, one call into the library may use below the
    /// point where it was entered; deeper recursion (deeply nested source,
    /// deep native reentry) fails with a JavaScript error instead of
    /// overflowing the thread's stack. Zero for
    /// [`AGNIJS_DEFAULT_STACK_BUDGET`]. The calling thread needs this much
    /// free stack plus [`AGNIJS_STACK_HEADROOM`].
    pub stack_budget: usize,
}

impl Default for AgnijsEngineOptions {
    fn default() -> Self {
        AgnijsEngineOptions {
            struct_size: 0,
            flags: 0,
            memory_limit: 0,
            regex_matcher: 0,
            max_reentrancy: 0,
            user_data: std::ptr::null_mut(),
            interrupt: None,
            console: None,
            resolve_module: None,
            load_module: None,
            import_meta: None,
            promise_rejection: None,
            now_ms: None,
            stack_budget: 0,
        }
    }
}

/// A host class definition for [`crate::agnijs_register_class`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AgnijsClassDef {
    pub struct_size: u32,
    /// UTF-8, `name_len` bytes.
    pub name: *const c_char,
    pub name_len: usize,
    /// Called with an instance's data when the instance is collected or the
    /// engine is freed. May be NULL.
    pub finalize: AgnijsFinalizer,
}

impl Default for AgnijsClassDef {
    fn default() -> Self {
        AgnijsClassDef { struct_size: 0, name: std::ptr::null(), name_len: 0, finalize: None }
    }
}

/// A JavaScript exception taken with [`crate::agnijs_take_exception`]. The
/// caller owns every handle in it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AgnijsException {
    pub struct_size: u32,
    /// Number of stack frames (innermost first) in `stack`.
    pub frame_count: u32,
    /// The thrown value.
    pub value: AgnijsValue,
    /// The `name` of an Error object (empty for a thrown primitive), UTF-8.
    pub name: AgnijsBuffer,
    /// The `message`, or the thrown primitive as text, UTF-8.
    pub message: AgnijsBuffer,
    /// One line per frame, `function (script:line:column)`, UTF-8.
    pub stack: AgnijsBuffer,
}

/// Memory and handle statistics from [`crate::agnijs_memory_usage`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AgnijsMemoryUsage {
    pub struct_size: u32,
    pub heap_bytes: usize,
    pub object_count: usize,
    /// Values and buffers the host holds and has not freed.
    pub host_handles: usize,
    /// Persistent roots inside the engine (should track `host_handles`).
    pub engine_handles: usize,
}
