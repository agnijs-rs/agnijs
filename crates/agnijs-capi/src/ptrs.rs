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

/// Run under Miri in CI (`cargo miri test -p agnijs-capi --lib miri_`):
/// every raw-pointer path here, with the hostile inputs the checks exist
/// for, so Miri sees no out-of-bounds, misaligned or uninitialised access.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn status<T>(r: Res<T>) -> AgnijsStatus {
        match r {
            Ok(_) => AGNIJS_OK,
            Err(f) => f.status,
        }
    }

    #[test]
    fn miri_inputs_check_null_alignment_length_and_encoding() {
        // SAFETY: every pointer is NULL, points at the local arrays with a
        // length they cover, or is refused (misaligned, overflowing) before
        // it is read.
        unsafe {
            assert_eq!(bytes(std::ptr::null(), 0, "p").unwrap(), b"");
            assert_eq!(status(bytes(std::ptr::null(), 1, "p")), AGNIJS_ERR_NULL_POINTER);
            let data = [1u8, 2, 3];
            assert_eq!(bytes(data.as_ptr(), 3, "p").unwrap(), &data);
            assert_eq!(status(bytes(data.as_ptr(), usize::MAX, "p")), AGNIJS_ERR_INVALID_ARGUMENT);
            assert_eq!(utf8(b"h\xc3\xa9".as_ptr().cast(), 3, "s").unwrap(), "h\u{e9}");
            assert_eq!(status(utf8(b"\xff".as_ptr().cast(), 1, "s")), AGNIJS_ERR_INVALID_UTF8);
            assert_eq!(status(utf8(b"\xc3".as_ptr().cast(), 1, "s")), AGNIJS_ERR_INVALID_UTF8);

            let units = [0xD800u16, 0x41, 0x42];
            assert_eq!(utf16(units.as_ptr(), 3, "u").unwrap(), &units);
            let odd = units.as_ptr().cast::<u8>().wrapping_add(1).cast::<u16>();
            assert_eq!(status(utf16(odd, 1, "u")), AGNIJS_ERR_INVALID_ARGUMENT);
            assert_eq!(status(utf16(units.as_ptr(), usize::MAX / 2 + 1, "u")), AGNIJS_ERR_INVALID_ARGUMENT);
            assert_eq!(status(utf16(std::ptr::null(), 1, "u")), AGNIJS_ERR_NULL_POINTER);

            let ids = [7u64, 8];
            assert_eq!(handles(ids.as_ptr(), 2, "h").unwrap(), &ids);
            let odd = ids.as_ptr().cast::<u8>().wrapping_add(4).cast::<u64>();
            assert_eq!(status(handles(odd, 1, "h")), AGNIJS_ERR_INVALID_ARGUMENT);
            assert_eq!(status(handles(ids.as_ptr(), usize::MAX / 8 + 1, "h")), AGNIJS_ERR_INVALID_ARGUMENT);
        }
    }

    #[test]
    fn miri_outputs_write_only_into_checked_memory() {
        let mut slot = std::mem::MaybeUninit::<u64>::uninit();
        Out::new(slot.as_mut_ptr(), "out").unwrap().write(5);
        // SAFETY: `write` just initialised the slot.
        assert_eq!(unsafe { slot.assume_init() }, 5);
        assert_eq!(status(Out::<u64>::new(std::ptr::null_mut(), "out")), AGNIJS_ERR_NULL_POINTER);
        assert!(Out::<u64>::optional(std::ptr::null_mut(), "out").unwrap().is_none());
        let mut words = [0u64; 2];
        let odd = words.as_mut_ptr().cast::<u8>().wrapping_add(1).cast::<u64>();
        assert_eq!(status(Out::new(odd, "out")), AGNIJS_ERR_INVALID_ARGUMENT);

        let src = *b"abc";
        let mut dst = [0xAAu8; 4];
        let mut needed = 0usize;
        // SAFETY: `dst` holds 4 bytes and every `cap` passed with it is at
        // most 4; NULL is passed only where it must be refused.
        unsafe {
            let n = Out::optional(&mut needed, "needed").unwrap();
            assert_eq!(status(copy_out(&src, dst.as_mut_ptr(), 3, n)), AGNIJS_ERR_BUFFER_TOO_SMALL);
            assert_eq!(needed, 3);
            assert_eq!(dst, [0xAA; 4], "a short buffer is left untouched");
            assert_eq!(status(copy_out(&src, std::ptr::null_mut(), 0, None)), AGNIJS_ERR_BUFFER_TOO_SMALL);
            assert_eq!(status(copy_out(&src, std::ptr::null_mut(), 9, None)), AGNIJS_ERR_NULL_POINTER);
            copy_out(&src, dst.as_mut_ptr(), 4, None).unwrap();
        }
        assert_eq!(dst, *b"abc\0");
    }

    #[test]
    fn miri_versioned_structs_check_size_and_unknown_fields() {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct V1 {
            struct_size: u32,
            a: u32,
        }
        // A caller built against a newer header: V1 plus one more field.
        #[repr(C)]
        #[allow(dead_code)] // `b` is read only through raw bytes
        struct V2 {
            v1: V1,
            b: u64,
        }
        let size = |n: usize| u32::try_from(n).unwrap();
        // SAFETY: each pointer is NULL or points at a local struct at least
        // as large as its `struct_size` claims (or claims more and is refused
        // before the extra bytes are read: 4096 is the cap, u32::MAX fails it).
        unsafe {
            let ok = V1 { struct_size: size(8), a: 3 };
            assert_eq!(versioned(&ok, "o").unwrap().a, 3);
            let small = V1 { struct_size: 4, a: 3 };
            assert_eq!(status(versioned(&small, "o")), AGNIJS_ERR_INVALID_ARGUMENT);
            let huge = V1 { struct_size: u32::MAX, a: 3 };
            assert_eq!(status(versioned(&huge, "o")), AGNIJS_ERR_INVALID_ARGUMENT);
            assert_eq!(status(versioned::<V1>(std::ptr::null(), "o")), AGNIJS_ERR_NULL_POINTER);

            let newer = V2 { v1: V1 { struct_size: size(std::mem::size_of::<V2>()), a: 4 }, b: 0 };
            assert_eq!(versioned(std::ptr::addr_of!(newer).cast::<V1>(), "o").unwrap().a, 4);
            let newer_used = V2 { v1: V1 { struct_size: size(std::mem::size_of::<V2>()), a: 4 }, b: 1 };
            assert_eq!(status(versioned(std::ptr::addr_of!(newer_used).cast::<V1>(), "o")), AGNIJS_ERR_INVALID_ARGUMENT);

            let mut out = V1 { struct_size: 4, a: 0 };
            assert_eq!(status(versioned_out(&mut out, "o")), AGNIJS_ERR_INVALID_ARGUMENT);
            out.struct_size = size(8);
            assert!(versioned_out(&mut out, "o").is_ok());
        }
    }
}
