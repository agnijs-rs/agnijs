//! The C API under hostile use: NULL and garbage arguments, stale, foreign
//! and misused handles, double frees, use after free, huge lengths,
//! misalignment, the wrong thread, forbidden reentrancy and host code that
//! panics. Every call must return an error status; none may panic, abort
//! or corrupt the engine.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;
use agnijs_capi::*;
use common::*;
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::ptr;

#[test]
fn null_pointers_are_refused_before_any_work() {
    let e = engine();
    let r = realm(e);
    assert_eq!(unsafe { agnijs_engine_new(ptr::null(), ptr::null_mut()) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_engine_main_realm(e, ptr::null_mut()) }, AGNIJS_ERR_NULL_POINTER);
    let src = "globalThis.ran = 1";
    let st = unsafe { agnijs_eval_script(e, r, ptr::null(), 0, src.as_ptr().cast(), src.len(), ptr::null_mut()) };
    assert_eq!(st, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(eval_display(e, "typeof ran"), "undefined", "nothing ran");
    let mut v = 0;
    let st = unsafe { agnijs_eval_script(e, r, ptr::null(), 0, ptr::null(), 5, &mut v) };
    assert_eq!(st, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_new_string_utf8(e, ptr::null(), 0, &mut v) }, AGNIJS_OK, "empty text may be NULL");
    assert_eq!(unsafe { agnijs_call(e, r, v, 0, ptr::null(), 2, &mut v) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_buffer_data(e, 1, ptr::null_mut(), ptr::null_mut()) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_take_exception(e, ptr::null_mut()) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_memory_usage(e, ptr::null_mut()) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(unsafe { agnijs_register_class(e, ptr::null(), &mut v) }, AGNIJS_ERR_NULL_POINTER);
    assert_eq!(
        unsafe { agnijs_new_function(e, r, ptr::null(), 0, None, ptr::null_mut(), None, &mut v) },
        AGNIJS_ERR_NULL_POINTER
    );
    assert_eq!(unsafe { agnijs_sink_set_string(1, ptr::null(), 3) }, AGNIJS_ERR_NULL_POINTER);
    assert!(!last_error().is_empty());
    free_engine(e);
}

#[test]
fn garbage_and_stale_ids_are_refused() {
    let e = engine();
    let r = realm(e);
    let mut out = 0;
    for bad in [0u64, 1, 2, u64::MAX, 0xDEAD_BEEF, r, 1 << 48] {
        assert!(agnijs_value_free(e, bad) < 0, "{bad:#x}");
        assert!(unsafe { agnijs_value_dup(e, bad, &mut out) } < 0, "{bad:#x}");
        assert!(agnijs_buffer_free(e, bad) < 0, "{bad:#x}");
        assert!(agnijs_engine_free(bad.wrapping_add(1 << 40)) < 0, "{bad:#x}");
        assert!(unsafe { agnijs_global_object(e, bad.wrapping_add(7), &mut out) } < 0, "{bad:#x}");
    }
    assert_eq!(unsafe { agnijs_global_object(e, 0, &mut out) }, AGNIJS_ERR_INVALID_REALM);
    assert_eq!(unsafe { agnijs_global_object(e, r + 1000, &mut out) }, AGNIJS_ERR_INVALID_REALM);
    let v = eval(e, "({})");
    free(e, v);
    assert_eq!(agnijs_value_free(e, v), AGNIJS_ERR_INVALID_HANDLE, "double free");
    let mut n = 0.0;
    assert_eq!(unsafe { agnijs_value_get_number(e, v, &mut n) }, AGNIJS_ERR_INVALID_HANDLE, "use after free");
    let w = eval(e, "1");
    assert_ne!(v, w);
    assert_eq!(agnijs_value_free(e, v), AGNIJS_ERR_INVALID_HANDLE, "a stale id never aliases a new value");
    assert_eq!(number(e, w), 1.0);
    // Kinds are not interchangeable.
    let mut b = 0;
    let s = "1";
    unsafe { agnijs_compile_script(e, r, ptr::null(), 0, s.as_ptr().cast(), 1, &mut b) };
    assert_eq!(agnijs_value_free(e, b), AGNIJS_ERR_INVALID_HANDLE);
    assert_eq!(agnijs_buffer_free(e, w), AGNIJS_ERR_INVALID_HANDLE);
    assert_eq!(unsafe { agnijs_value_get_number(e, b, &mut n) }, AGNIJS_ERR_INVALID_HANDLE);
    let mut data = ptr::null_mut();
    assert_eq!(unsafe { agnijs_host_object_data(e, w, w, &mut data) }, AGNIJS_ERR_INVALID_HANDLE);
    free_engine(e);
}

#[test]
fn ids_of_another_engine_are_refused() {
    let a = engine();
    let b = engine();
    let va = eval(a, "({ x: 1 })");
    let mut n = 0.0;
    assert_eq!(unsafe { agnijs_value_get_number(b, va, &mut n) }, AGNIJS_ERR_WRONG_ENGINE);
    assert_eq!(agnijs_value_free(b, va), AGNIJS_ERR_WRONG_ENGINE);
    let mut g = 0;
    assert_eq!(unsafe { agnijs_global_object(b, realm(a), &mut g) }, AGNIJS_ERR_WRONG_ENGINE);
    let gb = global(b);
    let st = unsafe { agnijs_set(b, realm(b), gb, "x".as_ptr().cast(), 1, va) };
    assert_eq!(st, AGNIJS_ERR_WRONG_ENGINE);
    free_engine(a);
    free_engine(b);
}

#[test]
fn a_freed_engine_is_gone_with_everything_it_handed_out() {
    let e = engine();
    let v = eval(e, "'x'");
    free_engine(e);
    assert_eq!(agnijs_engine_free(e), AGNIJS_ERR_INVALID_ENGINE, "double free");
    assert_eq!(agnijs_value_free(e, v), AGNIJS_ERR_INVALID_ENGINE);
    let mut r = 0;
    assert_eq!(unsafe { agnijs_engine_main_realm(e, &mut r) }, AGNIJS_ERR_INVALID_ENGINE);
    // Ids are never reused, even for a new engine.
    let f = engine();
    assert_ne!(e, f);
    assert_eq!(agnijs_value_free(f, v), AGNIJS_ERR_WRONG_ENGINE);
    free_engine(f);
}

#[test]
fn bad_text_lengths_and_alignment_are_refused() {
    let e = engine();
    let r = realm(e);
    let mut v = 0;
    let bad = [0x66u8, 0xFF, 0x6F];
    assert_eq!(unsafe { agnijs_new_string_utf8(e, bad.as_ptr().cast(), 3, &mut v) }, AGNIJS_ERR_INVALID_UTF8);
    let st = unsafe { agnijs_eval_script(e, r, ptr::null(), 0, bad.as_ptr().cast(), 3, &mut v) };
    assert_eq!(st, AGNIJS_ERR_INVALID_UTF8);
    let x = 0u8;
    assert_eq!(unsafe { agnijs_new_string_utf8(e, (&x as *const u8).cast(), usize::MAX, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let units = [0u16; 4];
    assert_eq!(unsafe { agnijs_new_string_utf16(e, units.as_ptr(), usize::MAX / 2 + 1, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let odd = unsafe { units.as_ptr().cast::<u8>().add(1) }.cast::<u16>();
    assert_eq!(unsafe { agnijs_new_string_utf16(e, odd, 1, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let ids = [0u64; 2];
    let odd_ids = unsafe { ids.as_ptr().cast::<u8>().add(4) }.cast::<u64>();
    assert_eq!(unsafe { agnijs_new_array(e, r, odd_ids, 1, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    assert_eq!(unsafe { agnijs_new_array(e, r, ids.as_ptr(), usize::MAX, &mut v) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let mut out = [0u64; 2];
    let odd_out = unsafe { out.as_mut_ptr().cast::<u8>().add(1) }.cast::<u64>();
    assert_eq!(unsafe { agnijs_new_null(e, odd_out) }, AGNIJS_ERR_INVALID_ARGUMENT);
    assert_eq!(unsafe { agnijs_new_array(e, r, ids.as_ptr(), 2, &mut v) }, AGNIJS_ERR_INVALID_HANDLE, "ids of 0");
    free_engine(e);
}

#[test]
fn versioned_structs_are_checked() {
    let mut e = 0;
    let mut o = options();
    o.struct_size = 8;
    assert_eq!(unsafe { agnijs_engine_new(&o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    o.struct_size = u32::MAX;
    assert_eq!(unsafe { agnijs_engine_new(&o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    // A newer, larger struct whose extra fields are zero is accepted...
    #[repr(C)]
    struct Newer {
        o: AgnijsEngineOptions,
        extra: [u64; 4],
    }
    let mut n = Newer { o: options(), extra: [0; 4] };
    n.o.struct_size = std::mem::size_of::<Newer>() as u32;
    assert_eq!(unsafe { agnijs_engine_new(&n.o, &mut e) }, AGNIJS_OK);
    free_engine(e);
    // ...and refused when they are not.
    n.extra[2] = 1;
    assert_eq!(unsafe { agnijs_engine_new(&n.o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let mut o = options();
    o.flags = 0x80;
    assert_eq!(unsafe { agnijs_engine_new(&o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let mut o = options();
    o.regex_matcher = 9;
    assert_eq!(unsafe { agnijs_engine_new(&o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let mut o = options();
    o.max_reentrancy = 100_000;
    assert_eq!(unsafe { agnijs_engine_new(&o, &mut e) }, AGNIJS_ERR_INVALID_ARGUMENT);
    let e = engine();
    let mut ex = AgnijsException::default();
    assert_eq!(unsafe { agnijs_take_exception(e, &mut ex) }, AGNIJS_ERR_INVALID_ARGUMENT, "struct_size 0");
    free_engine(e);
}

#[test]
fn another_thread_gets_wrong_thread_errors() {
    let e = engine();
    let v = eval(e, "7");
    let st = std::thread::spawn(move || {
        let mut n = 0.0;
        let a = unsafe { agnijs_value_get_number(e, v, &mut n) };
        let b = agnijs_engine_free(e);
        let c = agnijs_value_free(e, v);
        (a, b, c)
    })
    .join()
    .unwrap();
    assert_eq!(st, (AGNIJS_ERR_WRONG_THREAD, AGNIJS_ERR_WRONG_THREAD, AGNIJS_ERR_WRONG_THREAD));
    assert_eq!(number(e, v), 7.0, "the owner is unaffected");
    free_engine(e);
}

#[test]
fn the_last_error_is_per_thread_and_survives_reading() {
    let e = engine();
    let status = agnijs_value_free(e, 12345);
    assert!(status < 0);
    let first = last_error();
    assert!(!first.is_empty());
    assert_eq!(last_error(), first, "reading it does not change it");
    assert_eq!(agnijs_last_status(), status);
    let mut small = [0 as c_char; 2];
    let mut needed = 0;
    assert_eq!(unsafe { agnijs_last_error(small.as_mut_ptr(), 2, &mut needed) }, AGNIJS_ERR_BUFFER_TOO_SMALL);
    assert_eq!(needed, first.len());
    assert_eq!(last_error(), first);
    free_engine(e);
}

// ---- callbacks -------------------------------------------------------------------------

thread_local! {
    static SEEN: Cell<AgnijsStatus> = const { Cell::new(0) };
}

unsafe extern "C-unwind" fn free_own_engine(e: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    SEEN.with(|s| s.set(agnijs_engine_free(e)));
    AGNIJS_OK
}

#[test]
fn an_engine_cannot_be_freed_from_its_own_callback() {
    let e = engine();
    let mut f = 0;
    unsafe { agnijs_new_function(e, realm(e), ptr::null(), 0, Some(free_own_engine), ptr::null_mut(), None, &mut f) };
    set_global(e, "f", f);
    eval(e, "f()");
    SEEN.with(|s| assert_eq!(s.get(), AGNIJS_ERR_BUSY));
    free_engine(e);
}

unsafe extern "C-unwind" fn panics(_: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    panic!("host function bug");
}

unsafe extern "C-unwind" fn bad_result(_: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, result: *mut AgnijsValue) -> AgnijsStatus {
    unsafe { *result = 0xBAD_BAD };
    AGNIJS_OK
}

unsafe extern "C-unwind" fn odd_status(_: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    -12345
}

unsafe extern "C-unwind" fn throws_nothing(_: AgnijsEngine, _: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    AGNIJS_EXCEPTION
}

unsafe extern "C-unwind" fn keeps_args(e: AgnijsEngine, call: *const AgnijsCallInfo, _: *mut c_void, _: *mut AgnijsValue) -> AgnijsStatus {
    let a = unsafe { *(*call).args };
    SEEN.with(|s| s.set(i32::try_from(a & 0xFFFF_FFFF).unwrap_or(0)));
    KEPT.with(|k| k.set(a));
    let _ = e;
    AGNIJS_OK
}

thread_local! {
    static KEPT: Cell<u64> = const { Cell::new(0) };
}

fn install(e: AgnijsEngine, name: &str, f: AgnijsHostFunction) {
    let mut v = 0;
    let st = unsafe { agnijs_new_function(e, realm(e), name.as_ptr().cast(), name.len(), f, ptr::null_mut(), None, &mut v) };
    assert_eq!(st, AGNIJS_OK);
    set_global(e, name, v);
    free(e, v);
}

#[test]
fn misbehaving_host_functions_become_errors() {
    let e = engine();
    install(e, "panics", Some(panics));
    install(e, "badResult", Some(bad_result));
    install(e, "oddStatus", Some(odd_status));
    install(e, "throwsNothing", Some(throws_nothing));
    install(e, "keepsArgs", Some(keeps_args));
    let catch = |f: &str| eval_display(e, &format!("try {{ {f}(); 'no error' }} catch (x) {{ String(x) }}"));
    assert!(catch("panics").contains("host function bug"), "{}", catch("panics"));
    assert!(catch("badResult").contains("invalid result handle"));
    assert!(catch("oddStatus").contains("unknown status"));
    assert!(catch("throwsNothing").contains("without calling agnijs_throw"));
    // A panic in a host function is caught at the callback; the engine is
    // not poisoned and keeps working.
    assert_eq!(eval_display(e, "1 + 1"), "2");
    // Argument handles die when the call returns.
    eval(e, "keepsArgs({})");
    let mut t = 0;
    assert_eq!(unsafe { agnijs_value_type(e, KEPT.with(|k| k.get()), &mut t) }, AGNIJS_ERR_INVALID_HANDLE);
    free_engine(e);
}

unsafe extern "C-unwind" fn panicking_hook(_: *mut c_void, _: *const c_char, _: usize, _: *const c_char, _: usize, _: AgnijsSink) -> AgnijsStatus {
    panic!("resolver bug");
}

unsafe extern "C-unwind" fn silent_loader(_: *mut c_void, _: *const c_char, _: usize, sink: AgnijsSink) -> AgnijsStatus {
    SINK.with(|s| s.set(sink));
    AGNIJS_OK
}

thread_local! {
    static SINK: Cell<u64> = const { Cell::new(0) };
}

unsafe extern "C-unwind" fn panicking_interrupt(_: *mut c_void) -> i32 {
    panic!("interrupt bug");
}

unsafe extern "C-unwind" fn panicking_console(_: *mut c_void, _: AgnijsRealm, _: i32, _: *const c_char, _: usize) {
    panic!("console bug");
}

unsafe extern "C-unwind" fn panicking_finalizer(_: *mut c_void) {
    panic!("finalizer bug");
}

fn load_status(e: AgnijsEngine, spec: &str) -> (AgnijsStatus, String) {
    let r = realm(e);
    let mut p = 0;
    let st = unsafe { agnijs_load_module(e, r, spec.as_ptr().cast(), spec.len(), &mut p) };
    if st != AGNIJS_OK {
        return (st, last_error());
    }
    unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
    let (mut state, mut reason) = (0, 0);
    unsafe { agnijs_promise_state(e, r, p, &mut state, &mut reason) };
    (state, display(e, reason))
}

#[test]
fn misbehaving_hooks_become_errors() {
    let mut o = options();
    o.resolve_module = Some(panicking_hook);
    o.console = Some(panicking_console);
    let e = engine_with(&o);
    let (_, msg) = load_status(e, "x");
    assert!(msg.contains("resolver bug"), "{msg}");
    eval(e, "console.log('ignored')");
    free_engine(e);

    let mut o = options();
    o.load_module = Some(silent_loader);
    let e = engine_with(&o);
    let (_, msg) = load_status(e, "y");
    assert!(msg.contains("without an answer"), "{msg}");
    // The sink died with the hook.
    assert_eq!(unsafe { agnijs_sink_set_string(SINK.with(|s| s.get()), "z".as_ptr().cast(), 1) }, AGNIJS_ERR_INVALID_HANDLE);
    free_engine(e);

    let mut o = options();
    o.interrupt = Some(panicking_interrupt);
    let e = engine_with(&o);
    let (st, _) = eval_status(e, "for (;;) {}");
    assert_eq!(st, AGNIJS_INTERRUPTED, "a panicking interrupt hook interrupts");
    free_engine(e);

    let e = engine();
    let def = AgnijsClassDef {
        struct_size: std::mem::size_of::<AgnijsClassDef>() as u32,
        finalize: Some(panicking_finalizer),
        ..Default::default()
    };
    let mut class = 0;
    assert_eq!(unsafe { agnijs_register_class(e, &def, &mut class) }, AGNIJS_OK);
    let mut obj = 0;
    unsafe { agnijs_new_host_object(e, realm(e), class, ptr::null_mut(), 0, &mut obj) };
    free_engine(e);
}

unsafe extern "C-unwind" fn reenter_from_resolver(ud: *mut c_void, _: *const c_char, _: usize, _: *const c_char, _: usize, sink: AgnijsSink) -> AgnijsStatus {
    let e = unsafe { &*(ud as *const Cell<AgnijsEngine>) }.get();
    let mut v = 0;
    SEEN.with(|s| s.set(unsafe { agnijs_new_object(e, 0, &mut v) }));
    let mut r = 0;
    let st = unsafe { agnijs_engine_main_realm(e, &mut r) };
    KEPT.with(|k| k.set(u64::from(st.unsigned_abs())));
    unsafe { agnijs_sink_set_string(sink, "q".as_ptr().cast(), 1) }
}

#[test]
fn hooks_without_an_engine_cannot_reenter() {
    let id = Cell::new(0u64);
    let mut o = options();
    o.resolve_module = Some(reenter_from_resolver);
    o.user_data = (&id as *const Cell<u64>).cast_mut().cast();
    let e = engine_with(&o);
    id.set(e);
    let _ = load_status(e, "q");
    assert_eq!(KEPT.with(|k| k.get()), u64::from(AGNIJS_ERR_BUSY.unsigned_abs()));
    free_engine(e);
}

#[test]
fn many_engines_and_handles_do_not_leak_or_collide() {
    for _ in 0..20 {
        let e = engine();
        let mut ids = Vec::new();
        for i in 0..200 {
            let mut v = 0;
            unsafe { agnijs_new_number(e, f64::from(i), &mut v) };
            ids.push(v);
        }
        for (i, v) in ids.iter().enumerate() {
            if i % 2 == 0 {
                free(e, *v);
            }
        }
        let mut m = AgnijsMemoryUsage { struct_size: std::mem::size_of::<AgnijsMemoryUsage>() as u32, ..Default::default() };
        unsafe { agnijs_memory_usage(e, &mut m) };
        assert_eq!(m.host_handles, 100);
        free_engine(e);
    }
}
