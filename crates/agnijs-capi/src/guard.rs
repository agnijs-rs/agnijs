//! The wrapper every exported function runs in: panics are caught and
//! turned into [`AGNIJS_ERR_PANIC`], failures into a status plus a
//! thread-local message the host reads with [`crate::agnijs_last_error`].

use crate::types::*;
use std::any::Any;
use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// A failed call: the status to return and the message to record.
#[derive(Debug, Clone)]
pub struct Fail {
    pub status: AgnijsStatus,
    pub message: String,
}

impl Fail {
    pub fn new(status: AgnijsStatus, message: impl Into<String>) -> Fail {
        Fail { status, message: message.into() }
    }
}

pub type Res<T> = Result<T, Fail>;

thread_local! {
    static LAST_ERROR: RefCell<(AgnijsStatus, String)> = const { RefCell::new((AGNIJS_OK, String::new())) };
}

/// Record the last error of this thread. Never panics: a thread-local that
/// is being destroyed or is borrowed is skipped.
pub fn set_last_error(status: AgnijsStatus, message: &str) {
    let _ = LAST_ERROR.try_with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            slot.0 = status;
            slot.1.clear();
            slot.1.push_str(message);
        }
    });
}

/// The last error of this thread.
pub fn last_error() -> (AgnijsStatus, String) {
    LAST_ERROR
        .try_with(|cell| match cell.try_borrow() {
            Ok(slot) => slot.clone(),
            Err(_) => (AGNIJS_OK, String::new()),
        })
        .unwrap_or((AGNIJS_OK, String::new()))
}

/// The text of a panic payload.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Drop a panic payload without letting a panicking `Drop` escape.
pub fn drop_payload(payload: Box<dyn Any + Send>) {
    if let Err(inner) = catch_unwind(AssertUnwindSafe(move || drop(payload))) {
        // A payload whose drop panicked again: leak it rather than unwind.
        std::mem::forget(inner);
    }
}

/// Run the body of an exported function. `name` is the function's C name.
pub fn ffi(name: &'static str, body: impl FnOnce() -> Res<()>) -> AgnijsStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        fault::enter(name);
        let r = body();
        fault::leave();
        r
    }));
    match result {
        Ok(Ok(())) => AGNIJS_OK,
        Ok(Err(fail)) => {
            set_last_error(fail.status, &fail.message);
            fail.status
        }
        Err(payload) => {
            fault::reset();
            let msg = format!("panic caught in {name}: {}", panic_message(&*payload));
            drop_payload(payload);
            set_last_error(AGNIJS_ERR_PANIC, &msg);
            AGNIJS_ERR_PANIC
        }
    }
}

/// Like [`ffi`] for functions that return a value instead of a status.
pub fn ffi_value<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(v) => v,
        Err(payload) => {
            drop_payload(payload);
            fallback
        }
    }
}

/// Forced panics for tests (feature `fault-injection`): the named entry
/// point panics once, inside the engine lease when it takes one, otherwise
/// at the end of its body.
pub mod fault {
    #[cfg(feature = "fault-injection")]
    mod imp {
        use std::cell::RefCell;

        thread_local! {
            static TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
            // Entry points in progress on this thread, innermost last
            // (host callbacks call back in).
            static CURRENT: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        }

        pub fn arm(name: &str) {
            let _ = TARGET.try_with(|t| {
                if let Ok(mut t) = t.try_borrow_mut() {
                    *t = Some(name.to_string());
                }
            });
        }

        pub fn enter(name: &'static str) {
            let _ = CURRENT.try_with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    c.push(name);
                }
            });
        }

        /// Panic if the current entry point is the armed one.
        pub fn fire_here() {
            let current = CURRENT
                .try_with(|c| c.try_borrow().ok().and_then(|c| c.last().copied()))
                .ok()
                .flatten();
            let Some(current) = current else { return };
            let hit = TARGET
                .try_with(|t| match t.try_borrow_mut() {
                    Ok(mut t) if t.as_deref() == Some(current) => {
                        *t = None;
                        true
                    }
                    _ => false,
                })
                .unwrap_or(false);
            if hit {
                // The point of fault injection is a real panic (test-only
                // feature, never in a shipped library).
                #[allow(clippy::panic)]
                std::panic::panic_any(format!("injected fault in {current}"));
            }
        }

        pub fn leave() {
            fire_here();
            reset();
        }

        pub fn reset() {
            let _ = CURRENT.try_with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    c.pop();
                }
            });
        }
    }

    #[cfg(not(feature = "fault-injection"))]
    mod imp {
        #[inline(always)]
        pub fn enter(_name: &'static str) {}
        #[inline(always)]
        pub fn fire_here() {}
        #[inline(always)]
        pub fn leave() {}
        #[inline(always)]
        pub fn reset() {}
    }

    pub use imp::*;
}

/// Arm a forced panic in the entry point named `name` (its C name, for
/// example `"agnijs_eval_script"`); the next call to it panics once.
/// Only built with the `fault-injection` feature, for tests.
///
/// # Safety
/// `name` must be NULL or a NUL-terminated string.
#[cfg(feature = "fault-injection")]
#[no_mangle]
pub unsafe extern "C" fn agnijs_test_inject_panic(name: *const std::ffi::c_char) -> AgnijsStatus {
    if name.is_null() {
        return AGNIJS_ERR_NULL_POINTER;
    }
    // SAFETY: the caller passes a NUL-terminated string (documented above).
    let s = unsafe { std::ffi::CStr::from_ptr(name) };
    match s.to_str() {
        Ok(s) => {
            fault::arm(s);
            AGNIJS_OK
        }
        Err(_) => AGNIJS_ERR_INVALID_UTF8,
    }
}
