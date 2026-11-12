//! String storage: one-byte (Latin-1) or two-byte (UTF-16) flat strings and
//! ropes that flatten on first access. JS strings are sequences of UTF-16
//! code units and may contain lone surrogates, so no representation here
//! is UTF-8.

use crate::value::StrId;
use crate::vm::{Fault, JsResult};
use agnijs_engine_api::Host;
use std::cell::Cell;
use std::rc::Rc;

/// The interrupt check for a string search (TASK-103.28). A naive search
/// costs up to haystack times needle units, so it asks the host whether to
/// stop once per [`SearchPoll::SLICE`] units compared, counted across every
/// search that shares the poll.
pub struct SearchPoll {
    work: usize,
    host: Rc<dyn Host>,
}

impl SearchPoll {
    /// Units compared between polls.
    pub const SLICE: usize = 1 << 20;

    pub fn new(host: &Rc<dyn Host>) -> SearchPoll {
        SearchPoll {
            work: 0,
            host: host.clone(),
        }
    }

    /// Count `n` units of work; past a slice, ask the host.
    #[inline]
    pub fn charge(&mut self, n: usize) -> JsResult<()> {
        self.work += n;
        if self.work >= Self::SLICE {
            return self.ask();
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn ask(&mut self) -> JsResult<()> {
        self.work = 0;
        if self.host.interrupt() {
            return Err(Fault::Interrupt);
        }
        Ok(())
    }
}

/// The longest string the engine makes, in UTF-16 code units: 2^30
/// (TASK-103.26). Every operation that would make a longer one throws
/// `RangeError: Invalid string length`, as V8 (2^29 - 24) and SpiderMonkey
/// (2^30 - 2) do past theirs. 2^30 is the cap `repeat` and `padEnd` always
/// had; twice it still fits the `u32` a rope keeps its length in, so a
/// concatenation is checked before its length could wrap; and a flat
/// string of it is 1 GiB one-byte or 2 GiB two-byte.
pub const MAX_STRING_LENGTH: usize = 1 << 30;

pub enum Repr {
    One(Box<[u8]>),
    Two(Box<[u16]>),
    Rope {
        left: StrId,
        right: StrId,
        len: u32,
        one_byte: bool,
    },
}

pub struct StringData {
    pub repr: Repr,
    /// Cached content hash; 0 means not computed (a real hash of 0 is
    /// remapped to 1).
    hash: Cell<u64>,
    /// Interned: content-unique, usable as a property key by identity.
    pub atom: bool,
}

impl StringData {
    pub fn new(repr: Repr) -> StringData {
        StringData {
            repr,
            hash: Cell::new(0),
            atom: false,
        }
    }

    pub fn len(&self) -> usize {
        match &self.repr {
            Repr::One(b) => b.len(),
            Repr::Two(u) => u.len(),
            Repr::Rope { len, .. } => *len as usize,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_one_byte(&self) -> bool {
        match &self.repr {
            Repr::One(_) => true,
            Repr::Two(_) => false,
            Repr::Rope { one_byte, .. } => *one_byte,
        }
    }

    pub fn is_rope(&self) -> bool {
        matches!(self.repr, Repr::Rope { .. })
    }

    pub fn byte_len(&self) -> usize {
        match &self.repr {
            Repr::One(b) => b.len(),
            Repr::Two(u) => u.len() * 2,
            Repr::Rope { .. } => 16,
        }
    }

    /// The code units of a flat string. Panics on a rope: callers flatten
    /// through the heap first.
    #[inline]
    pub fn units(&self) -> Units<'_> {
        match &self.repr {
            Repr::One(b) => Units::One(b),
            Repr::Two(u) => Units::Two(u),
            Repr::Rope { .. } => panic!("units() on an unflattened rope"),
        }
    }

    pub fn cached_hash(&self) -> Option<u64> {
        let h = self.hash.get();
        if h == 0 {
            None
        } else {
            Some(h)
        }
    }

    pub fn set_hash(&self, h: u64) {
        self.hash.set(if h == 0 { 1 } else { h });
    }
}

/// A view of a flat string's code units.
#[derive(Clone, Copy)]
pub enum Units<'a> {
    One(&'a [u8]),
    Two(&'a [u16]),
}

impl<'a> Units<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Units::One(b) => b.len(),
            Units::Two(u) => u.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn is_one_byte(&self) -> bool {
        matches!(self, Units::One(_))
    }

    pub fn as_one_byte(&self) -> Option<&'a [u8]> {
        match self {
            Units::One(b) => Some(b),
            _ => None,
        }
    }

    /// Code unit at `i` (panics when out of range).
    #[inline]
    pub fn at(&self, i: usize) -> u16 {
        match self {
            Units::One(b) => b[i] as u16,
            Units::Two(u) => u[i],
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> Option<u16> {
        if i < self.len() {
            Some(self.at(i))
        } else {
            None
        }
    }

    pub fn iter(&self) -> UnitsIter<'a> {
        UnitsIter {
            units: *self,
            pos: 0,
            end: self.len(),
        }
    }

    pub fn slice(&self, start: usize, end: usize) -> Units<'a> {
        match self {
            Units::One(b) => Units::One(&b[start..end]),
            Units::Two(u) => Units::Two(&u[start..end]),
        }
    }

    pub fn to_vec(&self) -> Vec<u16> {
        match self {
            Units::One(b) => b.iter().map(|&c| c as u16).collect(),
            Units::Two(u) => u.to_vec(),
        }
    }

    /// Append these units to `out`, widening one-byte storage.
    pub fn extend_into(&self, out: &mut Vec<u16>) {
        match self {
            Units::One(b) => out.extend(b.iter().map(|&c| c as u16)),
            Units::Two(u) => out.extend_from_slice(u),
        }
    }

    /// Whether every unit fits in one byte (Latin-1), whatever the storage.
    pub fn fits_one_byte(&self) -> bool {
        match self {
            Units::One(_) => true,
            Units::Two(u) => u.iter().all(|&c| c <= 0xFF),
        }
    }

    pub fn eq(&self, other: &Units<'_>) -> bool {
        match (self, other) {
            (Units::One(a), Units::One(b)) => a == b,
            (Units::Two(a), Units::Two(b)) => a == b,
            _ => self.len() == other.len() && self.iter().eq(other.iter()),
        }
    }

    pub fn eq_str(&self, s: &str) -> bool {
        match self {
            Units::One(b) if s.is_ascii() => *b == s.as_bytes(),
            _ => self.iter().eq(s.encode_utf16()),
        }
    }

    pub fn eq_units(&self, s: &[u16]) -> bool {
        self.iter().eq(s.iter().copied())
    }

    pub fn starts_with_str(&self, s: &str) -> bool {
        let mut it = self.iter();
        for c in s.encode_utf16() {
            if it.next() != Some(c) {
                return false;
            }
        }
        true
    }

    /// Lexicographic comparison by code unit.
    pub fn cmp(&self, other: &Units<'_>) -> std::cmp::Ordering {
        self.iter().cmp(other.iter())
    }

    pub fn to_string_lossy(&self) -> String {
        match self {
            Units::One(b) => b.iter().map(|&c| c as char).collect(),
            Units::Two(u) => String::from_utf16_lossy(u),
        }
    }

    /// Exact conversion; `None` when the string has a lone surrogate.
    pub fn to_rust_string(&self) -> Option<String> {
        match self {
            Units::One(b) => Some(b.iter().map(|&c| c as char).collect()),
            Units::Two(u) => String::from_utf16(u).ok(),
        }
    }

    /// Content hash (FNV-1a over code units, widened).
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for u in self.iter() {
            h ^= u as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= self.len() as u64;
        h.wrapping_mul(0x100000001b3)
    }

    pub fn hash_str(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut n = 0;
        for u in s.encode_utf16() {
            h ^= u as u64;
            h = h.wrapping_mul(0x100000001b3);
            n += 1;
        }
        h ^= n as u64;
        h.wrapping_mul(0x100000001b3)
    }

    /// Find `needle` starting at `from`, charging the comparisons to
    /// `poll`.
    pub fn index_of(
        &self,
        needle: &Units<'_>,
        from: usize,
        poll: &mut SearchPoll,
    ) -> JsResult<Option<usize>> {
        let n = needle.len();
        let len = self.len();
        if n == 0 {
            return Ok(if from <= len { Some(from) } else { None });
        }
        if n > len {
            return Ok(None);
        }
        match (self, needle) {
            (Units::One(h), Units::One(nd)) => return index_of_slice(h, nd, from, poll),
            (Units::Two(h), Units::Two(nd)) => return index_of_slice(h, nd, from, poll),
            // A needle with a unit above 0xFF never occurs in one-byte text.
            (Units::One(_), Units::Two(nd)) if nd.iter().any(|&c| c > 0xFF) => return Ok(None),
            _ => {}
        }
        let mut i = from;
        while i + n <= len {
            let mut ok = true;
            let mut j = 0;
            while j < n {
                if self.at(i + j) != needle.at(j) {
                    ok = false;
                    break;
                }
                j += 1;
            }
            if ok {
                return Ok(Some(i));
            }
            poll.charge(j + 1)?;
            i += 1;
        }
        Ok(None)
    }

    /// Find the last occurrence of `needle` starting at or before `from`,
    /// charging the comparisons to `poll`.
    pub fn last_index_of(
        &self,
        needle: &Units<'_>,
        from: usize,
        poll: &mut SearchPoll,
    ) -> JsResult<Option<usize>> {
        let n = needle.len();
        let len = self.len();
        if n > len {
            return Ok(None);
        }
        let mut i = from.min(len - n);
        loop {
            let mut ok = true;
            let mut j = 0;
            while j < n {
                if self.at(i + j) != needle.at(j) {
                    ok = false;
                    break;
                }
                j += 1;
            }
            if ok {
                return Ok(Some(i));
            }
            if i == 0 {
                return Ok(None);
            }
            poll.charge(j + 1)?;
            i -= 1;
        }
    }

    /// The code point at `i` and its length in units (1 or 2).
    pub fn code_point_at(&self, i: usize) -> (u32, usize) {
        let first = self.at(i);
        if (0xD800..0xDC00).contains(&first) && i + 1 < self.len() {
            let second = self.at(i + 1);
            if (0xDC00..0xE000).contains(&second) {
                let cp = 0x10000 + (((first as u32) - 0xD800) << 10) + ((second as u32) - 0xDC00);
                return (cp, 2);
            }
        }
        (first as u32, 1)
    }

    /// Whether the string is well formed (no lone surrogates).
    pub fn is_well_formed(&self) -> bool {
        let mut i = 0;
        let len = self.len();
        while i < len {
            let u = self.at(i);
            if (0xD800..0xDC00).contains(&u) {
                if i + 1 < len && (0xDC00..0xE000).contains(&self.at(i + 1)) {
                    i += 2;
                    continue;
                }
                return false;
            }
            if (0xDC00..0xE000).contains(&u) {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// The first index at or after `from` where `needle` (not empty) occurs.
/// Each candidate is charged the needle's length, the most its comparison
/// can cost.
fn index_of_slice<T: Copy + PartialEq>(
    hay: &[T],
    needle: &[T],
    from: usize,
    poll: &mut SearchPoll,
) -> JsResult<Option<usize>> {
    let n = needle.len();
    if from + n > hay.len() {
        return Ok(None);
    }
    let first = needle[0];
    let rest = &needle[1..];
    let last_start = hay.len() - n;
    let mut i = from;
    while i <= last_start {
        let Some(k) = hay[i..=last_start].iter().position(|&c| c == first) else {
            return Ok(None);
        };
        i += k;
        if &hay[i + 1..i + n] == rest {
            return Ok(Some(i));
        }
        poll.charge(n)?;
        i += 1;
    }
    Ok(None)
}

pub struct UnitsIter<'a> {
    units: Units<'a>,
    pos: usize,
    end: usize,
}

impl<'a> Iterator for UnitsIter<'a> {
    type Item = u16;
    #[inline]
    fn next(&mut self) -> Option<u16> {
        if self.pos < self.end {
            let u = self.units.at(self.pos);
            self.pos += 1;
            Some(u)
        } else {
            None
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.pos;
        (n, Some(n))
    }
}

impl<'a> DoubleEndedIterator for UnitsIter<'a> {
    fn next_back(&mut self) -> Option<u16> {
        if self.pos < self.end {
            self.end -= 1;
            Some(self.units.at(self.end))
        } else {
            None
        }
    }
}

impl<'a> ExactSizeIterator for UnitsIter<'a> {}

/// Builds a string incrementally. It stores one byte per unit until the
/// first unit above 0xFF arrives and widens once then, so a Latin-1 result
/// becomes a heap string without a conversion copy.
pub enum StrBuilder {
    One(Vec<u8>),
    Two(Vec<u16>),
}

impl Default for StrBuilder {
    fn default() -> StrBuilder {
        StrBuilder::new()
    }
}

impl StrBuilder {
    pub fn new() -> StrBuilder {
        StrBuilder::One(Vec::new())
    }
    pub fn with_capacity(n: usize) -> StrBuilder {
        StrBuilder::One(Vec::with_capacity(n))
    }
    /// Switch to two-byte storage (keeping the content).
    #[cold]
    fn widen(&mut self) -> &mut Vec<u16> {
        if let StrBuilder::One(b) = self {
            let mut w = Vec::with_capacity(b.capacity().max(b.len() + 8));
            w.extend(b.iter().map(|&c| c as u16));
            *self = StrBuilder::Two(w);
        }
        match self {
            StrBuilder::Two(w) => w,
            StrBuilder::One(_) => unreachable!(),
        }
    }
    #[inline]
    pub fn push(&mut self, u: u16) {
        match self {
            StrBuilder::One(b) if u <= 0xFF => b.push(u as u8),
            StrBuilder::Two(w) => w.push(u),
            StrBuilder::One(_) => self.widen().push(u),
        }
    }
    /// Append ASCII or Latin-1 bytes.
    #[inline]
    pub fn push_bytes(&mut self, s: &[u8]) {
        match self {
            StrBuilder::One(b) => b.extend_from_slice(s),
            StrBuilder::Two(w) => w.extend(s.iter().map(|&c| c as u16)),
        }
    }
    pub fn push_str(&mut self, s: &str) {
        if s.is_ascii() {
            self.push_bytes(s.as_bytes());
        } else {
            for u in s.encode_utf16() {
                self.push(u);
            }
        }
    }
    pub fn push_units(&mut self, u: &Units<'_>) {
        match u {
            Units::One(b) => self.push_bytes(b),
            Units::Two(w) => self.push_slice(w),
        }
    }
    pub fn push_slice(&mut self, u: &[u16]) {
        match self {
            StrBuilder::One(b) => {
                if u.iter().all(|&c| c <= 0xFF) {
                    b.extend(u.iter().map(|&c| c as u8));
                } else {
                    self.widen().extend_from_slice(u);
                }
            }
            StrBuilder::Two(w) => w.extend_from_slice(u),
        }
    }
    pub fn push_code_point(&mut self, cp: u32) {
        if cp < 0x10000 {
            self.push(cp as u16);
        } else {
            let c = cp - 0x10000;
            let w = self.widen();
            w.push(0xD800 + (c >> 10) as u16);
            w.push(0xDC00 + (c & 0x3FF) as u16);
        }
    }
    pub fn len(&self) -> usize {
        match self {
            StrBuilder::One(b) => b.len(),
            StrBuilder::Two(w) => w.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Drop everything after the first `n` units.
    pub fn truncate(&mut self, n: usize) {
        match self {
            StrBuilder::One(b) => b.truncate(n),
            StrBuilder::Two(w) => w.truncate(n),
        }
    }
    /// The content as UTF-16 code units.
    pub fn finish(self) -> Vec<u16> {
        match self {
            StrBuilder::One(b) => b.iter().map(|&c| c as u16).collect(),
            StrBuilder::Two(w) => w,
        }
    }
    pub fn as_units(&self) -> Units<'_> {
        match self {
            StrBuilder::One(b) => Units::One(b),
            StrBuilder::Two(w) => Units::Two(w),
        }
    }
}

/// Is this code unit a JS `WhiteSpace` or `LineTerminator`?
pub fn is_js_whitespace(u: u16) -> bool {
    matches!(
        u,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

/// Trim JS whitespace from both ends; returns the unit range kept.
pub fn trim_range(u: &Units<'_>, start: bool, end: bool) -> (usize, usize) {
    let mut s = 0;
    let mut e = u.len();
    if start {
        while s < e && is_js_whitespace(u.at(s)) {
            s += 1;
        }
    }
    if end {
        while e > s && is_js_whitespace(u.at(e - 1)) {
            e -= 1;
        }
    }
    (s, e)
}
