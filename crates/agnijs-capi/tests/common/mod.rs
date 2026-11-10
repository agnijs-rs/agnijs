//! Helpers shared by the C API integration tests. They call the exported
//! functions exactly as a C caller would, through raw pointers.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::field_reassign_with_default)]

use agnijs_capi::*;
use std::ptr;

pub fn options() -> AgnijsEngineOptions {
    let mut o = AgnijsEngineOptions::default();
    o.struct_size = std::mem::size_of::<AgnijsEngineOptions>() as u32;
    o
}

pub fn engine_with(opts: &AgnijsEngineOptions) -> AgnijsEngine {
    let mut e: AgnijsEngine = 0;
    let st = unsafe { agnijs_engine_new(opts, &mut e) };
    assert_eq!(st, AGNIJS_OK, "engine_new: {}", last_error());
    e
}

pub fn engine() -> AgnijsEngine {
    engine_with(&options())
}

pub fn realm(e: AgnijsEngine) -> AgnijsRealm {
    let mut r: AgnijsRealm = 0;
    assert_eq!(unsafe { agnijs_engine_main_realm(e, &mut r) }, AGNIJS_OK);
    r
}

pub fn last_error() -> String {
    let mut needed = 0usize;
    unsafe { agnijs_last_error(ptr::null_mut(), 0, &mut needed) };
    let mut buf = vec![0u8; needed + 1];
    unsafe { agnijs_last_error(buf.as_mut_ptr().cast(), buf.len(), &mut needed) };
    buf.truncate(needed);
    String::from_utf8(buf).unwrap()
}

/// Evaluate `src` and return (status, value handle or 0).
pub fn eval_status(e: AgnijsEngine, src: &str) -> (AgnijsStatus, AgnijsValue) {
    let r = realm(e);
    let mut v: AgnijsValue = 0;
    let name = "test.js";
    let st = unsafe {
        agnijs_eval_script(e, r, name.as_ptr().cast(), name.len(), src.as_ptr().cast(), src.len(), &mut v)
    };
    (st, v)
}

pub fn eval(e: AgnijsEngine, src: &str) -> AgnijsValue {
    let (st, v) = eval_status(e, src);
    assert_eq!(st, AGNIJS_OK, "eval {src:?}: {}", last_error());
    v
}

pub fn number(e: AgnijsEngine, v: AgnijsValue) -> f64 {
    let mut n = 0.0;
    assert_eq!(unsafe { agnijs_value_get_number(e, v, &mut n) }, AGNIJS_OK, "{}", last_error());
    n
}

/// The UTF-8 text of a string value (lossy for lone surrogates).
pub fn utf8(e: AgnijsEngine, v: AgnijsValue) -> String {
    let mut needed = 0usize;
    let st = unsafe { agnijs_string_to_utf8(e, v, ptr::null_mut(), 0, &mut needed) };
    assert!(st == AGNIJS_OK || st == AGNIJS_ERR_BUFFER_TOO_SMALL, "{}", last_error());
    let mut buf = vec![0u8; needed + 1];
    assert_eq!(
        unsafe { agnijs_string_to_utf8(e, v, buf.as_mut_ptr().cast(), buf.len(), &mut needed) },
        AGNIJS_OK
    );
    buf.truncate(needed);
    String::from_utf8(buf).unwrap()
}

/// `String(v)` through the engine.
pub fn display(e: AgnijsEngine, v: AgnijsValue) -> String {
    let mut s: AgnijsValue = 0;
    assert_eq!(unsafe { agnijs_to_string(e, realm(e), v, &mut s) }, AGNIJS_OK, "{}", last_error());
    let out = utf8(e, s);
    free(e, s);
    out
}

pub fn eval_display(e: AgnijsEngine, src: &str) -> String {
    let v = eval(e, src);
    let s = display(e, v);
    free(e, v);
    s
}

pub fn free(e: AgnijsEngine, v: AgnijsValue) {
    assert_eq!(agnijs_value_free(e, v), AGNIJS_OK, "{}", last_error());
}

pub fn free_engine(e: AgnijsEngine) {
    assert_eq!(agnijs_engine_free(e), AGNIJS_OK, "{}", last_error());
}

pub fn new_string(e: AgnijsEngine, s: &str) -> AgnijsValue {
    let mut v = 0;
    assert_eq!(unsafe { agnijs_new_string_utf8(e, s.as_ptr().cast(), s.len(), &mut v) }, AGNIJS_OK);
    v
}

pub fn global(e: AgnijsEngine) -> AgnijsValue {
    let mut g = 0;
    assert_eq!(unsafe { agnijs_global_object(e, realm(e), &mut g) }, AGNIJS_OK);
    g
}

pub fn get(e: AgnijsEngine, obj: AgnijsValue, key: &str) -> AgnijsValue {
    let mut v = 0;
    let st = unsafe { agnijs_get(e, realm(e), obj, key.as_ptr().cast(), key.len(), &mut v) };
    assert_eq!(st, AGNIJS_OK, "get {key}: {}", last_error());
    v
}

pub fn set(e: AgnijsEngine, obj: AgnijsValue, key: &str, val: AgnijsValue) {
    let st = unsafe { agnijs_set(e, realm(e), obj, key.as_ptr().cast(), key.len(), val) };
    assert_eq!(st, AGNIJS_OK, "set {key}: {}", last_error());
}

pub fn set_global(e: AgnijsEngine, key: &str, val: AgnijsValue) {
    let g = global(e);
    set(e, g, key, val);
    free(e, g);
}

pub fn take_exception(e: AgnijsEngine) -> (String, String, String) {
    let mut ex = AgnijsException::default();
    ex.struct_size = std::mem::size_of::<AgnijsException>() as u32;
    assert_eq!(unsafe { agnijs_take_exception(e, &mut ex) }, AGNIJS_OK, "{}", last_error());
    let read = |b: AgnijsBuffer| {
        let mut p: *const u8 = ptr::null();
        let mut n = 0usize;
        assert_eq!(unsafe { agnijs_buffer_data(e, b, &mut p, &mut n) }, AGNIJS_OK);
        let s = String::from_utf8(unsafe { std::slice::from_raw_parts(p, n) }.to_vec()).unwrap();
        assert_eq!(agnijs_buffer_free(e, b), AGNIJS_OK);
        s
    };
    let out = (read(ex.name), read(ex.message), read(ex.stack));
    free(e, ex.value);
    out
}
