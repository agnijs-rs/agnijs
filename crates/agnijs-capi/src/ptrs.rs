//! Reading and writing caller memory. Every pointer the host passes is
//! checked here for NULL, alignment and length overflow before it is
//! dereferenced. What cannot be checked (that a non-NULL pointer really
//! points at `len` readable bytes) is the caller's contract, documented on
//! each function in the header.

use crate::guard::{Fail, Res};
use crate::types::*;
use std::ffi::c_char;

/// The largest length accepted for any input (bytes or code units): Rust
/// slices may not exceed `isize::MAX` bytes.
const MAX_LEN: usize = isize::MAX.unsigned_abs();

fn null(what: &str) -> Fail {
    Fail::new(AGNIJS_ERR_NULL_POINTER, format!("{what} is NULL"))
}

/// Borrow `len` bytes at `ptr`. NULL is allowed when `len` is zero.
///
/// # Safety
/// When `len` is nonzero, `ptr` must point at `len` readable bytes that stay
/// unchanged for the returned lifetime.
pub unsafe fn bytes<'a>(ptr: *const u8, len: usize, what: &str) -> Res<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(null(what));
    }
    if len > MAX_LEN {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what}: length {len} is too large")));
    }
    // SAFETY: non-NULL, length within isize::MAX, and readable per the
    // caller's contract; u8 has no alignment requirement.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Borrow UTF-8 text.
///
/// # Safety
/// As [`bytes`].
pub unsafe fn utf8<'a>(ptr: *const c_char, len: usize, what: &str) -> Res<&'a str> {
    // SAFETY: forwarded contract.
    let b = unsafe { bytes(ptr.cast(), len, what)? };
    std::str::from_utf8(b).map_err(|e| Fail::new(AGNIJS_ERR_INVALID_UTF8, format!("{what} is not UTF-8: {e}")))
}

/// Borrow `len` UTF-16 code units (lone surrogates allowed).
///
/// # Safety
/// When `len` is nonzero, `ptr` must point at `len` readable code units.
pub unsafe fn utf16<'a>(ptr: *const u16, len: usize, what: &str) -> Res<&'a [u16]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(null(what));
    }
    if !ptr.is_aligned() {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what} is not 2-byte aligned")));
    }
    match len.checked_mul(2) {
        Some(b) if b <= MAX_LEN => {}
        _ => return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what}: length {len} is too large"))),
    }
    // SAFETY: non-NULL, aligned, byte length within isize::MAX, readable per
    // the caller's contract.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Borrow `len` handles.
///
/// # Safety
/// When `len` is nonzero, `ptr` must point at `len` readable `u64`s.
pub unsafe fn handles<'a>(ptr: *const u64, len: usize, what: &str) -> Res<&'a [u64]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(null(what));
    }
    if !ptr.is_aligned() {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what} is not aligned")));
    }
    match len.checked_mul(8) {
        Some(b) if b <= MAX_LEN => {}
        _ => return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what}: length {len} is too large"))),
    }
    // SAFETY: as in `utf16`.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// A checked output location, validated before any work is done so a bad
/// pointer cannot leave side effects behind.
pub struct Out<T> {
    ptr: *mut T,
}

impl<T> Out<T> {
    /// A required output.
    pub fn new(ptr: *mut T, what: &str) -> Res<Out<T>> {
        if ptr.is_null() {
            return Err(null(what));
        }
        if !ptr.is_aligned() {
            return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what} is not aligned")));
        }
        Ok(Out { ptr })
    }

    /// An optional output: NULL means the caller does not want it.
    pub fn optional(ptr: *mut T, what: &str) -> Res<Option<Out<T>>> {
        if ptr.is_null() {
            Ok(None)
        } else {
            Out::new(ptr, what).map(Some)
        }
    }

    /// Store the result.
    pub fn write(&self, value: T) {
        // SAFETY: non-NULL and aligned (checked in `new`); writable per the
        // caller's contract. `write` does not read or drop the old contents,
        // which may be uninitialised.
        unsafe { self.ptr.write(value) }
    }
}

/// Read a versioned input struct whose first field is `u32 struct_size`.
/// Older (smaller) structs are refused, since only version 1 exists; a
/// newer (larger) struct is accepted when the fields this library does not
/// know are all zero, so a caller built against a newer header still works
/// when it leaves the new fields at their defaults.
///
/// # Safety
/// `ptr` must be NULL or point at a readable struct of at least
/// `struct_size` bytes whose first four bytes are `struct_size`, and every
/// field of `T` must accept any bit pattern a C caller can store (integers,
/// raw pointers and optional function pointers only).
pub unsafe fn versioned<T: Copy>(ptr: *const T, what: &str) -> Res<T> {
    if ptr.is_null() {
        return Err(null(what));
    }
    if !ptr.is_aligned() {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what} is not aligned")));
    }
    // SAFETY: non-NULL and aligned for T, whose first field is a u32 (so the
    // pointer is aligned for u32 too).
    let size = unsafe { ptr.cast::<u32>().read() };
    let ours = std::mem::size_of::<T>();
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size < ours {
        return Err(Fail::new(
            AGNIJS_ERR_INVALID_ARGUMENT,
            format!("{what}.struct_size is {size}, expected at least {ours}"),
        ));
    }
    if size > 4096 {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, format!("{what}.struct_size {size} is implausible")));
    }
    let base = ptr.cast::<u8>();
    for i in ours..size {
        // SAFETY: i < struct_size, and the caller's struct is that large.
        let b = unsafe { base.wrapping_add(i).read() };
        if b != 0 {
            return Err(Fail::new(
                AGNIJS_ERR_INVALID_ARGUMENT,
                format!("{what} has fields this library does not know (byte {i} is nonzero); upgrade the library"),
            ));
        }
    }
    // SAFETY: readable for size_of::<T>() bytes (struct_size >= that) and
    // aligned; every field accepts any bit pattern (the caller's contract).
    Ok(unsafe { ptr.read() })
}

/// Check the `struct_size` of an output struct the caller owns.
///
/// # Safety
/// `ptr` must be NULL or point at a readable, writable struct whose first
/// four bytes are its `struct_size`.
pub unsafe fn versioned_out<T>(ptr: *mut T, what: &str) -> Res<Out<T>> {
    let out = Out::new(ptr, what)?;
    // SAFETY: non-NULL and aligned (checked by `Out::new`); the first field
    // is a u32.
    let size = unsafe { ptr.cast::<u32>().read() };
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size < std::mem::size_of::<T>() {
        return Err(Fail::new(
            AGNIJS_ERR_INVALID_ARGUMENT,
            format!("{what}.struct_size is {size}, expected at least {}", std::mem::size_of::<T>()),
        ));
    }
    Ok(out)
}

/// Copy `src` plus a terminating zero into a caller buffer of `cap`
/// elements, reporting the length without the terminator in `needed`.
///
/// # Safety
/// When `cap` is nonzero, `dst` must point at `cap` writable elements.
pub unsafe fn copy_out<T: Copy + Default>(src: &[T], dst: *mut T, cap: usize, needed: Option<Out<usize>>) -> Res<()> {
    if let Some(n) = &needed {
        n.write(src.len());
    }
    let fits = cap > src.len();
    if !fits {
        return Err(Fail::new(
            AGNIJS_ERR_BUFFER_TOO_SMALL,
            format!("buffer holds {cap}, needs {} plus the terminator", src.len()),
        ));
    }
    if dst.is_null() {
        return Err(null("dst"));
    }
    if !dst.is_aligned() {
        return Err(Fail::new(AGNIJS_ERR_INVALID_ARGUMENT, "dst is not aligned"));
    }
    // SAFETY: dst is non-NULL, aligned and holds cap > src.len() elements
    // (caller's contract), so src.len() + 1 writes stay in bounds; the
    // regions cannot overlap because src is engine-owned memory.
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
        dst.wrapping_add(src.len()).write(T::default());
    }
    Ok(())
}
