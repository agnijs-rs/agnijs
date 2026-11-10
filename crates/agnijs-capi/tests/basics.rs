//! The C API used as intended: engines, realms, evaluation, values in and
//! out, properties, calls, host functions and classes, exceptions,
//! promises, modules, bytecode, limits and interrupts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;
use agnijs_capi::*;
use common::*;
use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::ptr;

#[test]
fn evaluates_a_script_and_reads_a_number() {
    let e = engine();
    let v = eval(e, "1 + 2");
    assert_eq!(number(e, v), 3.0);
    free(e, v);
    free_engine(e);
}

#[test]
fn reports_the_abi_and_library_version() {
    assert_eq!(agnijs_abi_version() >> 16, AGNIJS_ABI_VERSION_MAJOR);
    let s = unsafe { std::ffi::CStr::from_ptr(agnijs_version_string()) };
    assert!(!s.to_str().unwrap().is_empty());
}

#[test]
fn null_options_mean_the_defaults() {
    let mut e = 0;
    assert_eq!(unsafe { agnijs_engine_new(ptr::null(), &mut e) }, AGNIJS_OK);
    let v = eval(e, "'ok'");
    assert_eq!(utf8(e, v), "ok");
    free_engine(e);
}

#[test]
fn values_round_trip_with_their_types() {
    let e = engine();
    let r = realm(e);
    let ty = |v| {
        let mut t = -1;
        assert_eq!(unsafe { agnijs_value_type(e, v, &mut t) }, AGNIJS_OK);
        t
    };
    let mut v = 0;
    unsafe { agnijs_new_undefined(e, &mut v) };
    assert_eq!(ty(v), AGNIJS_TYPE_UNDEFINED);
    unsafe { agnijs_new_null(e, &mut v) };
    assert_eq!(ty(v), AGNIJS_TYPE_NULL);
    unsafe { agnijs_new_bool(e, 7, &mut v) };
    let mut b = 0;
    assert_eq!(unsafe { agnijs_value_get_bool(e, v, &mut b) }, AGNIJS_OK);
    assert_eq!(b, 1);
    unsafe { agnijs_new_number(e, 2.5, &mut v) };
    assert_eq!(number(e, v), 2.5);
    let big = "-123456789012345678901234567890";
    assert_eq!(unsafe { agnijs_new_bigint(e, big.as_ptr().cast(), big.len(), &mut v) }, AGNIJS_OK);
    assert_eq!(ty(v), AGNIJS_TYPE_BIGINT);
    assert_eq!(utf8(e, v), big);
    assert_eq!(unsafe { agnijs_new_bigint(e, "12a".as_ptr().cast(), 3, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let d = "tag";
    assert_eq!(unsafe { agnijs_new_symbol(e, r, d.as_ptr().cast(), d.len(), &mut v) }, AGNIJS_OK);
    assert_eq!(ty(v), AGNIJS_TYPE_SYMBOL);
    let f = eval(e, "(function () {})");
    assert_eq!(ty(f), AGNIJS_TYPE_FUNCTION);
    let s = new_string(e, "héllo");
    assert_eq!(ty(s), AGNIJS_TYPE_STRING);
    assert_eq!(utf8(e, s), "héllo");
    let mut not_number = 0.0;
    assert_eq!(unsafe { agnijs_value_get_number(e, s, &mut not_number) }, AGNIJS_ERR_TYPE_MISMATCH);
    let mut n = 0.0;
    assert_eq!(unsafe { agnijs_to_number(e, r, eval(e, "'42'"), &mut n) }, AGNIJS_OK);
    assert_eq!(n, 42.0);
    free_engine(e);
}

#[test]
fn utf16_strings_keep_lone_surrogates() {
    let e = engine();
    let units = [0x61u16, 0xD800, 0x62];
    let mut s = 0;
    assert_eq!(unsafe { agnijs_new_string_utf16(e, units.as_ptr(), units.len(), &mut s) }, AGNIJS_OK);
    set_global(e, "s", s);
    let len = eval(e, "s.length === 3 && s.charCodeAt(1) === 0xD800");
    let mut b = 0;
    unsafe { agnijs_value_get_bool(e, len, &mut b) };
    assert_eq!(b, 1);
    let back = eval(e, "s + '\\uDC00'");
    let mut needed = 0;
    let mut buf = [0u16; 8];
    assert_eq!(unsafe { agnijs_string_to_utf16(e, back, buf.as_mut_ptr(), buf.len(), &mut needed) }, AGNIJS_OK);
    assert_eq!(&buf[..needed + 1], &[0x61, 0xD800, 0x62, 0xDC00, 0]);
    // The UTF-8 view is lossy, never invalid.
    assert_eq!(utf8(e, s), "a\u{FFFD}b");
    // String() of a lone surrogate keeps it.
    let mut t = 0;
    assert_eq!(unsafe { agnijs_to_string(e, realm(e), s, &mut t) }, AGNIJS_OK);
    let mut buf2 = [0u16; 4];
    assert_eq!(unsafe { agnijs_string_to_utf16(e, t, buf2.as_mut_ptr(), 4, &mut needed) }, AGNIJS_OK);
    assert_eq!(&buf2, &[0x61, 0xD800, 0x62, 0]);
    free_engine(e);
}

#[test]
fn output_buffers_report_the_size_they_need() {
    let e = engine();
    let s = new_string(e, "abcdef");
    let mut needed = 0;
    let mut small = [0 as c_char; 6];
    assert_eq!(
        unsafe { agnijs_string_to_utf8(e, s, small.as_mut_ptr(), small.len(), &mut needed) },
        AGNIJS_ERR_BUFFER_TOO_SMALL
    );
    assert_eq!(needed, 6);
    let mut fits = [0 as c_char; 7];
    assert_eq!(unsafe { agnijs_string_to_utf8(e, s, fits.as_mut_ptr(), fits.len(), &mut needed) }, AGNIJS_OK);
    assert_eq!(fits[6], 0);
    free_engine(e);
}

#[test]
fn objects_arrays_and_properties() {
    let e = engine();
    let r = realm(e);
    let mut o = 0;
    assert_eq!(unsafe { agnijs_new_object(e, r, &mut o) }, AGNIJS_OK);
    let one = eval(e, "1");
    set(e, o, "a", one);
    let mut has = 0;
    assert_eq!(unsafe { agnijs_has(e, r, o, "a".as_ptr().cast(), 1, &mut has) }, AGNIJS_OK);
    assert_eq!(has, 1);
    let a = get(e, o, "a");
    assert_eq!(number(e, a), 1.0);
    let keys = {
        let mut k = 0;
        assert_eq!(unsafe { agnijs_own_keys(e, r, o, &mut k) }, AGNIJS_OK);
        k
    };
    let mut is = 0;
    unsafe { agnijs_is_array(e, keys, &mut is) };
    assert_eq!(is, 1);
    set_global(e, "keys", keys);
    assert_eq!(eval_display(e, "keys.join()"), "a");
    let mut deleted = 0;
    assert_eq!(unsafe { agnijs_delete(e, r, o, "a".as_ptr().cast(), 1, &mut deleted) }, AGNIJS_OK);
    assert_eq!(deleted, 1);
    // Keys as values, symbols included.
    let sym = {
        let mut s = 0;
        unsafe { agnijs_new_symbol(e, r, "k".as_ptr().cast(), 1, &mut s) };
        s
    };
    assert_eq!(agnijs_set_by_value(e, r, o, sym, one), AGNIJS_OK);
    let mut got = 0;
    assert_eq!(unsafe { agnijs_get_by_value(e, r, o, sym, &mut got) }, AGNIJS_OK);
    assert_eq!(number(e, got), 1.0);
    assert_eq!(unsafe { agnijs_get_by_value(e, r, o, one, &mut got) }, AGNIJS_ERR_TYPE_MISMATCH);
    let items = [one, sym];
    let mut arr = 0;
    assert_eq!(unsafe { agnijs_new_array(e, r, items.as_ptr(), 2, &mut arr) }, AGNIJS_OK);
    set_global(e, "arr", arr);
    assert_eq!(eval_display(e, "arr.length + typeof arr[1]"), "2symbol");
    free_engine(e);
}

#[test]
fn calls_and_constructs_functions() {
    let e = engine();
    let r = realm(e);
    let f = eval(e, "(function (a, b) { return this.base + a * b; })");
    let this = eval(e, "({ base: 100 })");
    let args = [eval(e, "6"), eval(e, "7")];
    let mut out = 0;
    assert_eq!(unsafe { agnijs_call(e, r, f, this, args.as_ptr(), 2, &mut out) }, AGNIJS_OK);
    assert_eq!(number(e, out), 142.0);
    let c = eval(e, "(class P { constructor(x) { this.x = x; } })");
    assert_eq!(unsafe { agnijs_construct(e, r, c, args.as_ptr(), 1, &mut out) }, AGNIJS_OK);
    let x = get(e, out, "x");
    assert_eq!(number(e, x), 6.0);
    let mut eq = 0;
    assert_eq!(unsafe { agnijs_strict_equals(e, x, args[0], &mut eq) }, AGNIJS_OK);
    assert_eq!(eq, 1);
    // Calling a non-function is a TypeError exception.
    assert_eq!(unsafe { agnijs_call(e, r, x, 0, ptr::null(), 0, &mut out) }, AGNIJS_EXCEPTION);
    let (name, _, _) = take_exception(e);
    assert_eq!(name, "TypeError");
    free_engine(e);
}

#[test]
fn exceptions_carry_name_message_and_stack() {
    let e = engine();
    let (st, _) = eval_status(e, "function inner() { throw new RangeError('too far'); }\nfunction outer() { inner(); }\nouter();");
    assert_eq!(st, AGNIJS_EXCEPTION);
    let (name, message, stack) = take_exception(e);
    assert_eq!(name, "RangeError");
    assert_eq!(message, "too far");
    assert!(stack.contains("inner (test.js:1"), "{stack}");
    assert!(stack.contains("outer (test.js:2"), "{stack}");
    // Taken once.
    let mut ex = AgnijsException { struct_size: std::mem::size_of::<AgnijsException>() as u32, ..Default::default() };
    assert_eq!(unsafe { agnijs_take_exception(e, &mut ex) }, AGNIJS_ERR_NOT_FOUND);
    // A syntax error is an exception too.
    let (st, _) = eval_status(e, "let let = 1;");
    assert_eq!(st, AGNIJS_EXCEPTION);
    assert_eq!(take_exception(e).0, "SyntaxError");
    // A thrown primitive.
    let (st, _) = eval_status(e, "throw 42");
    assert_eq!(st, AGNIJS_EXCEPTION);
    assert_eq!(take_exception(e).1, "42");
    let mut err = 0;
    let r = realm(e);
    assert_eq!(unsafe { agnijs_new_error(e, r, AGNIJS_TYPE_ERROR, "m".as_ptr().cast(), 1, &mut err) }, AGNIJS_OK);
    set_global(e, "err", err);
    assert_eq!(eval_display(e, "err instanceof TypeError && err.message"), "m");
    assert_eq!(unsafe { agnijs_new_error(e, r, 99, ptr::null(), 0, &mut err) }, AGNIJS_ERR_INVALID_ARGUMENT);
    free_engine(e);
}

// ---- host functions -------------------------------------------------------------------

unsafe extern "C-unwind" fn add(e: AgnijsEngine, call: *const AgnijsCallInfo, ud: *mut c_void, result: *mut AgnijsValue) -> AgnijsStatus {
    let call = unsafe { &*call };
    let args = unsafe { std::slice::from_raw_parts(call.args, call.argc) };
    let calls = unsafe { &*(ud as *const RefCell<u32>) };
    *calls.borrow_mut() += 1;
    let mut sum = 0.0;
    for a in args {
        let mut n = 0.0;
        if unsafe { agnijs_value_get_number(e, *a, &mut n) } != AGNIJS_OK {
            let msg = "not a number";
            let mut err = 0;
            unsafe { agnijs_new_error(e, call.realm, AGNIJS_TYPE_ERROR, msg.as_ptr().cast(), msg.len(), &mut err) };
            agnijs_throw(e, err);
            agnijs_value_free(e, err);
            return AGNIJS_EXCEPTION;
        }
        sum += n;
    }
    unsafe { agnijs_new_number(e, sum, result) }
}

unsafe extern "C-unwind" fn count_finalize(ud: *mut c_void) {
    let calls = unsafe { &*(ud as *const RefCell<u32>) };
    *calls.borrow_mut() += 1000;
}

fn host_add(e: AgnijsEngine, counter: &RefCell<u32>) -> AgnijsValue {
    let mut f = 0;
    let st = unsafe {
        agnijs_new_function(
            e,
            realm(e),
            "add".as_ptr().cast(),
            3,
            Some(add),
            counter as *const RefCell<u32> as *mut c_void,
            Some(count_finalize),
            &mut f,
        )
    };
    assert_eq!(st, AGNIJS_OK, "{}", last_error());
    f
}

#[test]
fn host_functions_are_called_and_can_throw() {
    let counter = RefCell::new(0u32);
    let e = engine();
    let f = host_add(e, &counter);
    set_global(e, "add", f);
    free(e, f);
    assert_eq!(eval_display(e, "add(1, 2, 3)"), "6");
    assert_eq!(eval_display(e, "add.name"), "add");
    assert_eq!(eval_display(e, "try { add(1, 'x') } catch (e) { e instanceof TypeError && e.message }"), "not a number");
    let (st, _) = eval_status(e, "function f() { add('y'); }\nf();");
    assert_eq!(st, AGNIJS_EXCEPTION);
    let (name, message, stack) = take_exception(e);
    assert_eq!((name.as_str(), message.as_str()), ("TypeError", "not a number"), "{stack}");
    assert_eq!(*counter.borrow(), 3);
    free_engine(e);
    assert_eq!(*counter.borrow(), 1003, "the finalizer ran once at teardown");
}

unsafe extern "C-unwind" fn recurse(e: AgnijsEngine, call: *const AgnijsCallInfo, _: *mut c_void, result: *mut AgnijsValue) -> AgnijsStatus {
    // Call back into JavaScript from the host function: depth - 1.
    let call = unsafe { &*call };
    let src = "typeof depth === 'number' && depth > 0 ? (depth--, again()) : 'bottom'";
    unsafe {
        agnijs_eval_script(e, call.realm, "r.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), result)
    }
}

#[test]
fn host_functions_may_call_back_into_the_engine_up_to_the_limit() {
    let mut o = options();
    o.max_reentrancy = 4;
    let e = engine_with(&o);
    let mut f = 0;
    unsafe { agnijs_new_function(e, realm(e), "again".as_ptr().cast(), 5, Some(recurse), ptr::null_mut(), None, &mut f) };
    set_global(e, "again", f);
    assert_eq!(eval_display(e, "var depth = 3; again()"), "bottom");
    let (st, _) = eval_status(e, "depth = 10; again()");
    assert_eq!(st, AGNIJS_EXCEPTION, "{}", last_error());
    let (_, message, _) = take_exception(e);
    assert!(message.contains("max_reentrancy"), "{message}");
    // The engine still works.
    assert_eq!(eval_display(e, "depth = 2; again()"), "bottom");
    free_engine(e);
}

// ---- host classes -------------------------------------------------------------------

thread_local! {
    static FINALIZED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C-unwind" fn record_finalize(data: *mut c_void) {
    FINALIZED.with(|f| f.borrow_mut().push(data as usize));
}

#[test]
fn host_objects_carry_data_and_are_finalized() {
    FINALIZED.with(|f| f.borrow_mut().clear());
    let e = engine();
    let r = realm(e);
    let name = "Point";
    let def = AgnijsClassDef {
        struct_size: std::mem::size_of::<AgnijsClassDef>() as u32,
        name: name.as_ptr().cast(),
        name_len: name.len(),
        finalize: Some(record_finalize),
    };
    let mut class = 0;
    assert_eq!(unsafe { agnijs_register_class(e, &def, &mut class) }, AGNIJS_OK);
    let mut other = 0;
    assert_eq!(unsafe { agnijs_register_class(e, &def, &mut other) }, AGNIJS_OK);
    let proto = eval(e, "({ hello() { return 'hi'; } })");
    let mut obj = 0;
    assert_eq!(unsafe { agnijs_new_host_object(e, r, class, 0x1234 as *mut c_void, proto, &mut obj) }, AGNIJS_OK);
    let mut data = ptr::null_mut();
    assert_eq!(unsafe { agnijs_host_object_data(e, obj, class, &mut data) }, AGNIJS_OK);
    assert_eq!(data as usize, 0x1234);
    assert_eq!(unsafe { agnijs_host_object_data(e, obj, other, &mut data) }, AGNIJS_ERR_TYPE_MISMATCH);
    assert_eq!(unsafe { agnijs_host_object_data(e, proto, class, &mut data) }, AGNIJS_ERR_TYPE_MISMATCH);
    set_global(e, "p", obj);
    assert_eq!(eval_display(e, "p.hello()"), "hi");
    // A collected instance is finalized.
    let mut tmp = 0;
    unsafe { agnijs_new_host_object(e, r, class, 0x99 as *mut c_void, 0, &mut tmp) };
    free(e, tmp);
    assert_eq!(agnijs_collect_garbage(e), AGNIJS_OK);
    // Finalizers of collected objects run on the next job drain.
    assert_eq!(unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) }, AGNIJS_OK);
    FINALIZED.with(|f| assert_eq!(*f.borrow(), vec![0x99]));
    free_engine(e);
    FINALIZED.with(|f| assert_eq!(*f.borrow(), vec![0x99, 0x1234]));
}

// ---- promises and jobs ----------------------------------------------------------------

#[test]
fn promises_settle_when_jobs_run() {
    let e = engine();
    let r = realm(e);
    let p = eval(e, "Promise.resolve(20).then(x => x + 1)");
    let mut is = 0;
    unsafe { agnijs_is_promise(e, p, &mut is) };
    assert_eq!(is, 1);
    let mut state = -1;
    let mut result = 0;
    assert_eq!(unsafe { agnijs_promise_state(e, r, p, &mut state, &mut result) }, AGNIJS_OK);
    assert_eq!((state, result), (AGNIJS_PROMISE_PENDING, 0));
    let mut pending = 0;
    unsafe { agnijs_has_pending_jobs(e, &mut pending) };
    assert_eq!(pending, 1);
    let mut ran = 0;
    assert_eq!(unsafe { agnijs_run_jobs(e, r, &mut ran) }, AGNIJS_OK);
    assert!(ran >= 1);
    assert_eq!(unsafe { agnijs_promise_state(e, r, p, &mut state, &mut result) }, AGNIJS_OK);
    assert_eq!(state, AGNIJS_PROMISE_FULFILLED);
    assert_eq!(number(e, result), 21.0);
    let q = eval(e, "Promise.reject(new Error('no'))");
    assert_eq!(unsafe { agnijs_promise_state(e, r, q, &mut state, &mut result) }, AGNIJS_OK);
    assert_eq!(state, AGNIJS_PROMISE_REJECTED);
    assert_eq!(unsafe { agnijs_promise_state(e, r, result, &mut state, ptr::null_mut()) }, AGNIJS_ERR_TYPE_MISMATCH);
    free_engine(e);
}

thread_local! {
    static REJECTIONS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    static LOGS: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C-unwind" fn on_rejection(_: *mut c_void, e: AgnijsEngine, _: AgnijsRealm, event: i32, promise: AgnijsValue, _: AgnijsValue) {
    let mut is = 0;
    unsafe { agnijs_is_promise(e, promise, &mut is) };
    assert_eq!(is, 1);
    REJECTIONS.with(|r| r.borrow_mut().push(event));
}

unsafe extern "C-unwind" fn on_console(_: *mut c_void, _: AgnijsRealm, level: i32, text: *const c_char, len: usize) {
    let s = std::str::from_utf8(unsafe { std::slice::from_raw_parts(text.cast::<u8>(), len) }).unwrap().to_string();
    LOGS.with(|l| l.borrow_mut().push((level, s)));
}

unsafe extern "C-unwind" fn fixed_clock(_: *mut c_void) -> f64 {
    1_000_000.0
}

#[test]
fn hooks_report_rejections_console_and_time() {
    let mut o = options();
    o.promise_rejection = Some(on_rejection);
    o.console = Some(on_console);
    o.now_ms = Some(fixed_clock);
    let e = engine_with(&o);
    let r = realm(e);
    let v = eval(e, "var p = Promise.reject(1); console.warn('a', 2); Date.now()");
    assert_eq!(number(e, v), 1_000_000.0);
    unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
    eval(e, "p.catch(() => {})");
    unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
    REJECTIONS.with(|x| assert_eq!(*x.borrow(), vec![AGNIJS_REJECTION_UNHANDLED, AGNIJS_REJECTION_HANDLED]));
    LOGS.with(|l| assert_eq!(*l.borrow(), vec![(AGNIJS_CONSOLE_WARN, "a 2".to_string())]));
    free_engine(e);
}

// ---- modules --------------------------------------------------------------------------

unsafe extern "C-unwind" fn resolve(_: *mut c_void, _: *const c_char, _: usize, spec: *const c_char, len: usize, sink: AgnijsSink) -> AgnijsStatus {
    let s = std::str::from_utf8(unsafe { std::slice::from_raw_parts(spec.cast::<u8>(), len) }).unwrap();
    let full = format!("mem:/{}", s.trim_start_matches("./"));
    unsafe { agnijs_sink_set_string(sink, full.as_ptr().cast(), full.len()) }
}

unsafe extern "C-unwind" fn load(_: *mut c_void, name: *const c_char, len: usize, sink: AgnijsSink) -> AgnijsStatus {
    let s = std::str::from_utf8(unsafe { std::slice::from_raw_parts(name.cast::<u8>(), len) }).unwrap();
    let (kind, text) = match s {
        "mem:/main.js" => (AGNIJS_MODULE_SOURCE, "import { two } from './dep.js'; import data from './d.json' with { type: 'json' }; export const answer = two * 21 + data.x; export const url = import.meta.url;"),
        "mem:/dep.js" => (AGNIJS_MODULE_SOURCE, "export const two = 2;"),
        "mem:/d.json" => (AGNIJS_MODULE_JSON, "{\"x\": 0}"),
        _ => {
            let m = format!("no module {s}");
            unsafe { agnijs_sink_set_error(sink, m.as_ptr().cast(), m.len()) };
            return AGNIJS_ERR_NOT_FOUND;
        }
    };
    unsafe { agnijs_sink_set_module(sink, kind, text.as_ptr(), text.len()) }
}

unsafe extern "C-unwind" fn meta(_: *mut c_void, e: AgnijsEngine, r: AgnijsRealm, module: *const c_char, len: usize, meta: AgnijsValue) -> AgnijsStatus {
    let mut url = 0;
    unsafe { agnijs_new_string_utf8(e, module, len, &mut url) };
    let st = unsafe { agnijs_set(e, r, meta, "url".as_ptr().cast(), 3, url) };
    agnijs_value_free(e, url);
    st
}

fn module_engine() -> AgnijsEngine {
    let mut o = options();
    o.resolve_module = Some(resolve);
    o.load_module = Some(load);
    o.import_meta = Some(meta);
    engine_with(&o)
}

#[test]
fn modules_load_through_the_host_hooks() {
    let e = module_engine();
    let r = realm(e);
    let mut p = 0;
    assert_eq!(unsafe { agnijs_load_module(e, r, "./main.js".as_ptr().cast(), 9, &mut p) }, AGNIJS_OK, "{}", last_error());
    unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
    let (mut state, mut ns) = (0, 0);
    unsafe { agnijs_promise_state(e, r, p, &mut state, &mut ns) };
    assert_eq!(state, AGNIJS_PROMISE_FULFILLED);
    let answer = get(e, ns, "answer");
    assert_eq!(number(e, answer), 42.0);
    let url = get(e, ns, "url");
    assert_eq!(utf8(e, url), "mem:/main.js");
    // A module the hook cannot find rejects (or fails) with its message.
    let mut q = 0;
    let st = unsafe { agnijs_load_module(e, r, "./missing.js".as_ptr().cast(), 12, &mut q) };
    if st == AGNIJS_OK {
        unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
        let mut reason = 0;
        unsafe { agnijs_promise_state(e, r, q, &mut state, &mut reason) };
        assert_eq!(state, AGNIJS_PROMISE_REJECTED);
        assert!(display(e, reason).contains("no module mem:/missing.js"));
    } else {
        assert!(last_error().contains("no module mem:/missing.js"), "{}", last_error());
    }
    let mut released = 0;
    let spec = "mem:/main.js";
    assert_eq!(unsafe { agnijs_release_module(e, r, spec.as_ptr().cast(), spec.len(), &mut released) }, AGNIJS_OK);
    assert_eq!(released, 1);
    free_engine(e);
}

// ---- bytecode -------------------------------------------------------------------------

#[test]
fn bytecode_saves_and_loads() {
    let e = engine();
    let r = realm(e);
    let src = "var compiled = 'yes'; 6 * 7";
    let mut b = 0;
    assert_eq!(unsafe { agnijs_compile_script(e, r, "c.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), &mut b) }, AGNIJS_OK);
    let (mut p, mut n) = (ptr::null(), 0usize);
    assert_eq!(unsafe { agnijs_buffer_data(e, b, &mut p, &mut n) }, AGNIJS_OK);
    let bytes = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
    assert_eq!(agnijs_buffer_free(e, b), AGNIJS_OK);
    assert_eq!(agnijs_buffer_free(e, b), AGNIJS_ERR_INVALID_HANDLE);
    // Loaded into a fresh engine.
    let e2 = engine();
    let r2 = realm(e2);
    let mut v = 0;
    assert_eq!(unsafe { agnijs_eval_bytecode(e2, r2, bytes.as_ptr(), bytes.len(), &mut v) }, AGNIJS_OK, "{}", last_error());
    assert_eq!(number(e2, v), 42.0);
    // Corrupt bytecode is refused cleanly.
    let mut bad = bytes.clone();
    bad.truncate(bad.len() / 2);
    assert_eq!(unsafe { agnijs_eval_bytecode(e2, r2, bad.as_ptr(), bad.len(), &mut v) }, AGNIJS_ERR_BYTECODE);
    assert_eq!(unsafe { agnijs_eval_bytecode(e2, r2, b"junk".as_ptr(), 4, &mut v) }, AGNIJS_ERR_BYTECODE);
    // A module compiled to bytecode can answer a load hook.
    let ms = "export default 1;";
    assert_eq!(unsafe { agnijs_compile_module(e2, r2, "m.js".as_ptr().cast(), 4, ms.as_ptr().cast(), ms.len(), &mut b) }, AGNIJS_OK);
    free_engine(e);
    free_engine(e2);
}

// ---- realms, limits and interrupts ------------------------------------------------------

#[test]
fn realms_have_their_own_globals() {
    let e = engine();
    let mut r2 = 0;
    assert_eq!(unsafe { agnijs_realm_new(e, &mut r2) }, AGNIJS_OK);
    eval(e, "var here = 1");
    let src = "typeof here";
    let mut v = 0;
    assert_eq!(unsafe { agnijs_eval_script(e, r2, "x".as_ptr().cast(), 1, src.as_ptr().cast(), src.len(), &mut v) }, AGNIJS_OK);
    assert_eq!(utf8(e, v), "undefined");
    free_engine(e);
}

#[test]
fn the_memory_limit_stops_runaway_allocation() {
    let mut o = options();
    o.memory_limit = 32 << 20;
    let e = engine_with(&o);
    let (st, _) = eval_status(e, "var a = []; for (;;) a.push({ x: [1, 2, 3, 4] });");
    assert_eq!(st, AGNIJS_OUT_OF_MEMORY, "{}", last_error());
    // Lift the limit; the engine is usable again.
    eval(e, "a = null");
    assert_eq!(agnijs_set_memory_limit(e, 0), AGNIJS_OK);
    assert_eq!(eval_display(e, "1 + 1"), "2");
    let mut m = AgnijsMemoryUsage { struct_size: std::mem::size_of::<AgnijsMemoryUsage>() as u32, ..Default::default() };
    assert_eq!(unsafe { agnijs_memory_usage(e, &mut m) }, AGNIJS_OK);
    assert!(m.heap_bytes > 0);
    free_engine(e);
}

#[test]
fn a_fuel_budget_stops_an_infinite_loop_at_the_same_instruction_every_time() {
    let run = || {
        let e = engine();
        eval(e, "var n = 0");
        let mut left = 0u64;
        // No budget yet.
        assert_eq!(unsafe { agnijs_get_fuel(e, &mut left) }, AGNIJS_ERR_NOT_FOUND);
        assert_eq!(agnijs_set_fuel(e, 100_000), AGNIJS_OK);
        let (st, _) = eval_status(e, "for (;;) n++;");
        assert_eq!(st, AGNIJS_OUT_OF_FUEL, "{}", last_error());
        assert_eq!(unsafe { agnijs_get_fuel(e, &mut left) }, AGNIJS_OK);
        assert_eq!(left, 0);
        // Spent: later calls fail too, until the budget is cleared.
        assert_eq!(eval_status(e, "1").0, AGNIJS_OUT_OF_FUEL);
        assert_eq!(agnijs_clear_fuel(e), AGNIJS_OK);
        let n = eval_display(e, "n");
        free_engine(e);
        n
    };
    let first = run();
    assert_eq!(run(), first);
    assert_eq!(first, "12499");
}

#[test]
fn a_random_seed_makes_math_random_repeat() {
    let draw = |seed: u64| {
        let e = engine();
        assert_eq!(agnijs_set_random_seed(e, seed), AGNIJS_OK);
        let first = eval_display(e, "[Math.random(), Math.random()].join()");
        // Reseeding mid-run restarts the sequence.
        assert_eq!(agnijs_set_random_seed(e, seed), AGNIJS_OK);
        assert_eq!(eval_display(e, "[Math.random(), Math.random()].join()"), first);
        free_engine(e);
        first
    };
    assert_eq!(draw(42), draw(42));
    assert_ne!(draw(42), draw(43));
}

#[test]
fn a_host_function_returning_out_of_fuel_passes_it_on() {
    unsafe extern "C-unwind" fn spent(_e: AgnijsEngine, _call: *const AgnijsCallInfo, _ud: *mut c_void, _result: *mut AgnijsValue) -> AgnijsStatus {
        AGNIJS_OUT_OF_FUEL
    }
    let e = engine();
    let r = realm(e);
    let mut f = 0;
    assert_eq!(
        unsafe { agnijs_new_function(e, r, "spent".as_ptr().cast(), 5, Some(spent), ptr::null_mut(), None, &mut f) },
        AGNIJS_OK,
        "{}",
        last_error()
    );
    set_global(e, "spent", f);
    let (st, _) = eval_status(e, "try { spent() } catch (x) {}");
    assert_eq!(st, AGNIJS_OUT_OF_FUEL, "{}", last_error());
    free_engine(e);
}

unsafe extern "C-unwind" fn interrupt_after(ud: *mut c_void) -> i32 {
    let n = unsafe { &*(ud as *const RefCell<u32>) };
    *n.borrow_mut() += 1;
    i32::from(*n.borrow() > 3)
}

#[test]
fn the_interrupt_hook_stops_an_infinite_loop() {
    let polls = RefCell::new(0u32);
    let mut o = options();
    o.interrupt = Some(interrupt_after);
    o.user_data = &polls as *const RefCell<u32> as *mut c_void;
    let e = engine_with(&o);
    let (st, _) = eval_status(e, "for (;;) {}");
    assert_eq!(st, AGNIJS_INTERRUPTED);
    free_engine(e);
}

#[test]
fn another_thread_can_request_an_interrupt() {
    let e = engine();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        agnijs_engine_request_interrupt(e)
    });
    let (st, _) = eval_status(e, "for (;;) {}");
    assert_eq!(st, AGNIJS_INTERRUPTED);
    assert_eq!(t.join().unwrap(), AGNIJS_OK);
    assert_eq!(eval_display(e, "'after'"), "after");
    free_engine(e);
}

#[test]
fn string_compilation_can_be_refused() {
    let mut o = options();
    o.flags = AGNIJS_FLAG_NO_STRING_COMPILATION;
    let e = engine_with(&o);
    let (st, _) = eval_status(e, "eval('1')");
    assert_eq!(st, AGNIJS_EXCEPTION);
    take_exception(e);
    free_engine(e);
}
