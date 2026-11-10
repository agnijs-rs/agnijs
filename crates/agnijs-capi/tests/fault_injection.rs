//! Fault injection (feature `fault-injection`): a forced panic inside every
//! entry point is caught, returned as `AGNIJS_ERR_PANIC`, and leaves the
//! process and every other engine intact. An engine that was running when
//! the panic hit is poisoned: it refuses further work with
//! `AGNIJS_ERR_POISONED` and can still be freed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;
use agnijs_capi::*;
use common::*;
use std::ffi::{c_char, c_void, CString};
use std::ptr;

struct Cx {
    e: AgnijsEngine,
    r: AgnijsRealm,
    num: AgnijsValue,
    obj: AgnijsValue,
    func: AgnijsValue,
    text: AgnijsValue,
    promise: AgnijsValue,
    buffer: AgnijsBuffer,
    class: AgnijsClass,
    host: AgnijsValue,
}

unsafe extern "C-unwind" fn noop(_: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    AGNIJS_OK
}

fn setup() -> Cx {
    let e = engine();
    let r = realm(e);
    let s = "1";
    let mut buffer = 0;
    assert_eq!(unsafe { agnijs_compile_script(e, r, ptr::null(), 0, s.as_ptr().cast(), 1, &mut buffer) }, AGNIJS_OK);
    let def = AgnijsClassDef { struct_size: std::mem::size_of::<AgnijsClassDef>() as u32, ..Default::default() };
    let mut class = 0;
    assert_eq!(unsafe { agnijs_register_class(e, &def, &mut class) }, AGNIJS_OK);
    let mut host = 0;
    assert_eq!(unsafe { agnijs_new_host_object(e, r, class, ptr::null_mut(), 0, &mut host) }, AGNIJS_OK);
    Cx {
        e,
        r,
        num: eval(e, "5"),
        obj: eval(e, "({ a: 1 })"),
        func: eval(e, "(function (x) { return x; })"),
        text: eval(e, "'text'"),
        promise: eval(e, "Promise.resolve(1)"),
        buffer,
        class,
        host,
    }
}

type Call = fn(&Cx) -> AgnijsStatus;

fn k(s: &str) -> (*const c_char, usize) {
    (s.as_ptr().cast(), s.len())
}

fn entry_points() -> Vec<(&'static str, Call)> {
    let mut v: Vec<(&'static str, Call)> = Vec::new();
    v.push(("agnijs_engine_new", |_| {
        let mut e = 0;
        unsafe { agnijs_engine_new(ptr::null(), &mut e) }
    }));
    v.push(("agnijs_engine_request_interrupt", |c| agnijs_engine_request_interrupt(c.e)));
    v.push(("agnijs_engine_main_realm", |c| {
        let mut r = 0;
        unsafe { agnijs_engine_main_realm(c.e, &mut r) }
    }));
    v.push(("agnijs_realm_new", |c| {
        let mut r = 0;
        unsafe { agnijs_realm_new(c.e, &mut r) }
    }));
    v.push(("agnijs_global_object", |c| {
        let mut g = 0;
        unsafe { agnijs_global_object(c.e, c.r, &mut g) }
    }));
    v.push(("agnijs_eval_script", |c| {
        let (s, n) = k("1 + 1");
        let mut out = 0;
        unsafe { agnijs_eval_script(c.e, c.r, ptr::null(), 0, s, n, &mut out) }
    }));
    v.push(("agnijs_load_module", |c| {
        let (s, n) = k("m");
        let mut out = 0;
        unsafe { agnijs_load_module(c.e, c.r, s, n, &mut out) }
    }));
    v.push(("agnijs_release_module", |c| {
        let (s, n) = k("m");
        unsafe { agnijs_release_module(c.e, c.r, s, n, ptr::null_mut()) }
    }));
    v.push(("agnijs_run_jobs", |c| unsafe { agnijs_run_jobs(c.e, c.r, ptr::null_mut()) }));
    v.push(("agnijs_has_pending_jobs", |c| {
        let mut b = 0;
        unsafe { agnijs_has_pending_jobs(c.e, &mut b) }
    }));
    v.push(("agnijs_compile_script", |c| {
        let (s, n) = k("1");
        let mut b = 0;
        unsafe { agnijs_compile_script(c.e, c.r, ptr::null(), 0, s, n, &mut b) }
    }));
    v.push(("agnijs_compile_module", |c| {
        let (s, n) = k("export default 1");
        let mut b = 0;
        unsafe { agnijs_compile_module(c.e, c.r, ptr::null(), 0, s, n, &mut b) }
    }));
    v.push(("agnijs_eval_bytecode", |c| {
        let mut out = 0;
        unsafe { agnijs_eval_bytecode(c.e, c.r, b"x".as_ptr(), 1, &mut out) }
    }));
    v.push(("agnijs_buffer_data", |c| {
        let (mut p, mut n) = (ptr::null(), 0);
        unsafe { agnijs_buffer_data(c.e, c.buffer, &mut p, &mut n) }
    }));
    v.push(("agnijs_buffer_free", |c| agnijs_buffer_free(c.e, c.buffer)));
    v.push(("agnijs_set_memory_limit", |c| agnijs_set_memory_limit(c.e, 0)));
    v.push(("agnijs_memory_usage", |c| {
        let mut m = AgnijsMemoryUsage { struct_size: std::mem::size_of::<AgnijsMemoryUsage>() as u32, ..Default::default() };
        unsafe { agnijs_memory_usage(c.e, &mut m) }
    }));
    v.push(("agnijs_collect_garbage", |c| agnijs_collect_garbage(c.e)));
    v.push(("agnijs_take_exception", |c| {
        let mut x = AgnijsException { struct_size: std::mem::size_of::<AgnijsException>() as u32, ..Default::default() };
        unsafe { agnijs_take_exception(c.e, &mut x) }
    }));
    v.push(("agnijs_throw", |c| agnijs_throw(c.e, c.num)));
    v.push(("agnijs_new_undefined", |c| {
        let mut o = 0;
        unsafe { agnijs_new_undefined(c.e, &mut o) }
    }));
    v.push(("agnijs_new_null", |c| {
        let mut o = 0;
        unsafe { agnijs_new_null(c.e, &mut o) }
    }));
    v.push(("agnijs_new_bool", |c| {
        let mut o = 0;
        unsafe { agnijs_new_bool(c.e, 1, &mut o) }
    }));
    v.push(("agnijs_new_number", |c| {
        let mut o = 0;
        unsafe { agnijs_new_number(c.e, 1.0, &mut o) }
    }));
    v.push(("agnijs_new_string_utf8", |c| {
        let (s, n) = k("s");
        let mut o = 0;
        unsafe { agnijs_new_string_utf8(c.e, s, n, &mut o) }
    }));
    v.push(("agnijs_new_string_utf16", |c| {
        let u = [0x61u16];
        let mut o = 0;
        unsafe { agnijs_new_string_utf16(c.e, u.as_ptr(), 1, &mut o) }
    }));
    v.push(("agnijs_new_bigint", |c| {
        let (s, n) = k("12");
        let mut o = 0;
        unsafe { agnijs_new_bigint(c.e, s, n, &mut o) }
    }));
    v.push(("agnijs_new_object", |c| {
        let mut o = 0;
        unsafe { agnijs_new_object(c.e, c.r, &mut o) }
    }));
    v.push(("agnijs_new_array", |c| {
        let mut o = 0;
        unsafe { agnijs_new_array(c.e, c.r, &c.num, 1, &mut o) }
    }));
    v.push(("agnijs_new_error", |c| {
        let mut o = 0;
        unsafe { agnijs_new_error(c.e, c.r, AGNIJS_ERROR, ptr::null(), 0, &mut o) }
    }));
    v.push(("agnijs_new_symbol", |c| {
        let mut o = 0;
        unsafe { agnijs_new_symbol(c.e, c.r, ptr::null(), 0, &mut o) }
    }));
    v.push(("agnijs_value_free", |c| agnijs_value_free(c.e, c.num)));
    v.push(("agnijs_value_dup", |c| {
        let mut o = 0;
        unsafe { agnijs_value_dup(c.e, c.num, &mut o) }
    }));
    v.push(("agnijs_value_type", |c| {
        let mut t = 0;
        unsafe { agnijs_value_type(c.e, c.num, &mut t) }
    }));
    v.push(("agnijs_value_get_number", |c| {
        let mut n = 0.0;
        unsafe { agnijs_value_get_number(c.e, c.num, &mut n) }
    }));
    v.push(("agnijs_value_get_bool", |c| {
        let mut n = 0;
        unsafe { agnijs_value_get_bool(c.e, c.num, &mut n) }
    }));
    v.push(("agnijs_string_to_utf8", |c| {
        let mut b = [0 as c_char; 16];
        unsafe { agnijs_string_to_utf8(c.e, c.text, b.as_mut_ptr(), 16, ptr::null_mut()) }
    }));
    v.push(("agnijs_string_to_utf16", |c| {
        let mut b = [0u16; 16];
        unsafe { agnijs_string_to_utf16(c.e, c.text, b.as_mut_ptr(), 16, ptr::null_mut()) }
    }));
    v.push(("agnijs_to_string", |c| {
        let mut o = 0;
        unsafe { agnijs_to_string(c.e, c.r, c.num, &mut o) }
    }));
    v.push(("agnijs_to_number", |c| {
        let mut n = 0.0;
        unsafe { agnijs_to_number(c.e, c.r, c.text, &mut n) }
    }));
    v.push(("agnijs_strict_equals", |c| {
        let mut b = 0;
        unsafe { agnijs_strict_equals(c.e, c.num, c.num, &mut b) }
    }));
    v.push(("agnijs_is_array", |c| {
        let mut b = 0;
        unsafe { agnijs_is_array(c.e, c.obj, &mut b) }
    }));
    v.push(("agnijs_is_promise", |c| {
        let mut b = 0;
        unsafe { agnijs_is_promise(c.e, c.promise, &mut b) }
    }));
    v.push(("agnijs_promise_state", |c| {
        let mut s = 0;
        unsafe { agnijs_promise_state(c.e, c.r, c.promise, &mut s, ptr::null_mut()) }
    }));
    v.push(("agnijs_get", |c| {
        let (s, n) = k("a");
        let mut o = 0;
        unsafe { agnijs_get(c.e, c.r, c.obj, s, n, &mut o) }
    }));
    v.push(("agnijs_get_by_value", |c| {
        let mut o = 0;
        unsafe { agnijs_get_by_value(c.e, c.r, c.obj, c.text, &mut o) }
    }));
    v.push(("agnijs_set", |c| {
        let (s, n) = k("a");
        unsafe { agnijs_set(c.e, c.r, c.obj, s, n, c.num) }
    }));
    v.push(("agnijs_set_by_value", |c| agnijs_set_by_value(c.e, c.r, c.obj, c.text, c.num)));
    v.push(("agnijs_has", |c| {
        let (s, n) = k("a");
        let mut b = 0;
        unsafe { agnijs_has(c.e, c.r, c.obj, s, n, &mut b) }
    }));
    v.push(("agnijs_delete", |c| {
        let (s, n) = k("a");
        unsafe { agnijs_delete(c.e, c.r, c.obj, s, n, ptr::null_mut()) }
    }));
    v.push(("agnijs_own_keys", |c| {
        let mut o = 0;
        unsafe { agnijs_own_keys(c.e, c.r, c.obj, &mut o) }
    }));
    v.push(("agnijs_call", |c| {
        let mut o = 0;
        unsafe { agnijs_call(c.e, c.r, c.func, 0, &c.num, 1, &mut o) }
    }));
    v.push(("agnijs_construct", |c| {
        let mut o = 0;
        unsafe { agnijs_construct(c.e, c.r, c.func, ptr::null(), 0, &mut o) }
    }));
    v.push(("agnijs_new_function", |c| {
        let mut o = 0;
        unsafe { agnijs_new_function(c.e, c.r, ptr::null(), 0, Some(noop), ptr::null_mut(), None, &mut o) }
    }));
    v.push(("agnijs_register_class", |c| {
        let def = AgnijsClassDef { struct_size: std::mem::size_of::<AgnijsClassDef>() as u32, ..Default::default() };
        let mut o = 0;
        unsafe { agnijs_register_class(c.e, &def, &mut o) }
    }));
    v.push(("agnijs_new_host_object", |c| {
        let mut o = 0;
        unsafe { agnijs_new_host_object(c.e, c.r, c.class, ptr::null_mut(), 0, &mut o) }
    }));
    v.push(("agnijs_host_object_data", |c| {
        let mut d = ptr::null_mut();
        unsafe { agnijs_host_object_data(c.e, c.host, c.class, &mut d) }
    }));
    v.push(("agnijs_sink_set_string", |_| unsafe { agnijs_sink_set_string(1, ptr::null(), 0) }));
    v.push(("agnijs_sink_set_module", |_| unsafe { agnijs_sink_set_module(1, AGNIJS_MODULE_SOURCE, ptr::null(), 0) }));
    v.push(("agnijs_sink_set_error", |_| unsafe { agnijs_sink_set_error(1, ptr::null(), 0) }));
    v.push(("agnijs_engine_free", |c| agnijs_engine_free(c.e)));
    v
}

fn arm(name: &str) {
    let c = CString::new(name).unwrap();
    assert_eq!(unsafe { agnijs_test_inject_panic(c.as_ptr()) }, AGNIJS_OK);
}

#[test]
fn a_panic_in_any_entry_point_is_caught() {
    let bystander = engine();
    let witness = eval(bystander, "'still here'");
    for (name, call) in entry_points() {
        let cx = setup();
        arm(name);
        let st = call(&cx);
        assert_eq!(st, AGNIJS_ERR_PANIC, "{name}: {}", last_error());
        let msg = last_error();
        assert!(msg.contains("injected fault") && msg.contains(name), "{name}: {msg}");
        // Whatever state the engine is in, freeing it works (or it was the
        // engine_free call itself, which may have completed first).
        let freed = agnijs_engine_free(cx.e);
        assert!(freed == AGNIJS_OK || freed == AGNIJS_ERR_INVALID_ENGINE, "{name}: free gave {freed}: {}", last_error());
        // The fault fired once and is spent.
        let again = setup();
        assert_ne!(call(&again), AGNIJS_ERR_PANIC, "{name}");
        let _ = agnijs_engine_free(again.e);
    }
    assert_eq!(utf8(bystander, witness), "still here", "other engines are untouched");
    free_engine(bystander);
}

#[test]
fn a_panic_inside_the_engine_poisons_only_that_engine() {
    let cx = setup();
    arm("agnijs_eval_script");
    let (st, _) = eval_status(cx.e, "1");
    assert_eq!(st, AGNIJS_ERR_PANIC);
    let (st, _) = eval_status_raw(cx.e, cx.r, "1");
    assert_eq!(st, AGNIJS_ERR_POISONED);
    let mut n = 0.0;
    assert_eq!(unsafe { agnijs_value_get_number(cx.e, cx.num, &mut n) }, AGNIJS_ERR_POISONED);
    // Freeing values and the engine still works.
    assert_eq!(agnijs_value_free(cx.e, cx.num), AGNIJS_OK);
    assert_eq!(agnijs_engine_free(cx.e), AGNIJS_OK);
}

fn eval_status_raw(e: AgnijsEngine, r: AgnijsRealm, src: &str) -> (AgnijsStatus, AgnijsValue) {
    let mut v = 0;
    let st = unsafe { agnijs_eval_script(e, r, ptr::null(), 0, src.as_ptr().cast(), src.len(), &mut v) };
    (st, v)
}

unsafe extern "C-unwind" fn nested(e: AgnijsEngine, call: *const AgnijsCallInfo, _: *mut c_void, result: *mut AgnijsValue) -> AgnijsStatus {
    let realm = unsafe { (*call).realm };
    let mut o = 0;
    let st = unsafe { agnijs_new_object(e, realm, &mut o) };
    assert_eq!(st, AGNIJS_ERR_PANIC);
    unsafe { *result = 0 };
    st
}

#[test]
fn a_panic_in_a_nested_call_from_a_host_function_fails_the_outer_call_safely() {
    let cx = setup();
    let mut f = 0;
    unsafe { agnijs_new_function(cx.e, cx.r, ptr::null(), 0, Some(nested), ptr::null_mut(), None, &mut f) };
    set_global(cx.e, "nested", f);
    arm("agnijs_new_object");
    let (st, _) = eval_status_raw(cx.e, cx.r, "nested()");
    assert!(st == AGNIJS_ERR_ENGINE || st == AGNIJS_ERR_POISONED || st == AGNIJS_EXCEPTION, "{st}: {}", last_error());
    let (st, _) = eval_status_raw(cx.e, cx.r, "1");
    assert_eq!(st, AGNIJS_ERR_POISONED);
    assert_eq!(agnijs_engine_free(cx.e), AGNIJS_OK);
}
