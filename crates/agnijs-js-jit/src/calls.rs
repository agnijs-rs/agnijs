//! What lets machine code do by itself two things the runtime otherwise
//! does for it (TASK-101.3.3): call a compiled function, and read a global
//! binding.
//!
//! - The [`CalleeTable`] is the runtime's table of functions machine code
//!   may call directly: for a code block, everything needed to push its
//!   frame and enter its machine code. A `Call` in code compiled with a
//!   [`crate::CallLayout`] looks its callee's block up there; a miss makes
//!   the call through the runtime, which may add the callee
//!   ([`CalleeTable::fill`]). The table is shared by every call site, so a
//!   site that calls many functions is served as well as one that calls
//!   one.
//! - A [`GlobalSite`] is one `LdaGlobal`'s cache of where the binding's
//!   value is.
//!
//! Safe code: both are plain words. What makes the words trustworthy is
//! the contract of the functions that fill them, the same kind of contract
//! as [`crate::HeapLayout`]: compiled code reads through them without
//! checking.

use std::any::Any;
use std::cell::Cell;
use std::mem::{offset_of, size_of};
use std::rc::Rc;

use agnijs_js_compiler::bytecode::IcSite;

use crate::{Compiled, JitValue};

/// The table has `2^CALLEE_BITS` buckets of [`CALLEE_WAYS`] entries.
pub(crate) const CALLEE_BITS: u32 = 8;
pub(crate) const CALLEE_WAYS: usize = 4;

/// The multiplier of the table's hash of a code block's word (Fibonacci
/// hashing: the top [`CALLEE_BITS`] bits of the product are the bucket).
pub(crate) const CALLEE_HASH: u64 = 0x9E37_79B9_7F4A_7C15;

/// How often a bucket is filled before it stops taking callees: more hot
/// functions than a bucket has entries would otherwise replace each other
/// at every call, each time through the runtime.
pub const MAX_FILLS: u32 = 64;

/// One callee. Compiled code reads the words at [`CALLEE`].
#[repr(C)]
pub(crate) struct CalleeEntry {
    /// The callee's code block, as the word of a function's reference to
    /// it ([`crate::rc_word`]); 0 for no callee, which no function's word
    /// equals.
    code: usize,
    /// The callee's machine code.
    entry: usize,
    /// The image of the frame to push.
    template: usize,
    /// The callee's inline-cache sites.
    ic: usize,
    /// The callee's code info (`exec::CodeInfo`).
    info: usize,
    /// The cell holding the shape epoch the callee's inline caches are
    /// valid in.
    shape_epoch: usize,
    /// Registers of the callee's window.
    registers: u32,
    /// The first parameter register, and how many there are.
    param_base: u32,
    param_regs: u32,
    /// The realm the callee runs in.
    realm: u32,
}

const EMPTY: CalleeEntry = CalleeEntry {
    code: 0,
    entry: 0,
    template: 0,
    ic: 0,
    info: 0,
    shape_epoch: 0,
    registers: 0,
    param_base: 0,
    param_regs: 0,
    realm: 0,
};

/// Field offsets of a callee entry, and its size, for the code generator.
pub(crate) struct CalleeOffsets {
    pub size: u32,
    pub code: i32,
    pub entry: i32,
    pub template: i32,
    pub ic: i32,
    pub info: i32,
    pub shape_epoch: i32,
    pub registers: i32,
    pub param_base: i32,
    pub param_regs: i32,
    pub realm: i32,
}

pub(crate) const CALLEE: CalleeOffsets = CalleeOffsets {
    size: size_of::<CalleeEntry>() as u32,
    code: offset_of!(CalleeEntry, code) as i32,
    entry: offset_of!(CalleeEntry, entry) as i32,
    template: offset_of!(CalleeEntry, template) as i32,
    ic: offset_of!(CalleeEntry, ic) as i32,
    info: offset_of!(CalleeEntry, info) as i32,
    shape_epoch: offset_of!(CalleeEntry, shape_epoch) as i32,
    registers: offset_of!(CalleeEntry, registers) as i32,
    param_base: offset_of!(CalleeEntry, param_base) as i32,
    param_regs: offset_of!(CalleeEntry, param_regs) as i32,
    realm: offset_of!(CalleeEntry, realm) as i32,
};

// The code generator scales a bucket index by shifting. (Where there is
// none, a 32-bit target among them, the table is never read by machine
// code and its entries may be any size.)
#[cfg(jit_host)]
const _: () = assert!(size_of::<CalleeEntry>().is_power_of_two() && CALLEE_WAYS.is_power_of_two());

/// What an entry keeps alive: the callee's machine code and code info, and
/// the frame image (which holds the code block, and so its feedback).
struct Keep<V: 'static> {
    _compiled: Rc<Compiled<V>>,
    _template: Box<dyn Any>,
}

/// A callee for [`CalleeTable::fill`].
pub struct Callee<'a, V: 'static> {
    /// The callee's compiled code.
    pub compiled: &'a Rc<Compiled<V>>,
    /// The word of the function's reference to its code block
    /// ([`crate::rc_word`]).
    pub code: *const usize,
    /// The frame to push, as the runtime's own frame type: what its frame
    /// stack holds, built in safe code. Compiled code pushes a copy of its
    /// bytes and then sets the fields [`crate::CallLayout`] names.
    pub template: Box<dyn Any>,
    /// The callee block's inline-cache sites ([`crate::Runtime::ic_table`]).
    pub ic: &'a [IcSite],
    /// The cell holding the shape epoch those sites are valid in; compiled
    /// code calls directly only while it equals
    /// [`crate::Runtime::shape_epoch`].
    pub shape_epoch: &'a Cell<u64>,
    /// The callee block's register count.
    pub registers: u32,
    /// The callee's first parameter register, and how many parameters it
    /// takes in registers (a call's further arguments are dropped).
    pub param_base: u32,
    pub param_regs: u32,
    /// The callee's realm.
    pub realm: u32,
}

/// The functions machine code may call directly, by code block: a small
/// set-associative cache the runtime owns and fills, and compiled code
/// reads ([`crate::CallView::callees`]).
pub struct CalleeTable {
    entries: Box<[CalleeEntry]>,
    /// What keeps each entry's words valid.
    keep: Vec<Option<Box<dyn Any>>>,
    /// Per bucket: fills so far, and the way the next fill replaces.
    fills: Vec<u32>,
}

impl Default for CalleeTable {
    /// A table with no entries yet ([`CalleeTable::base`] makes them).
    fn default() -> Self {
        CalleeTable {
            entries: Box::new([]),
            keep: Vec::new(),
            fills: Vec::new(),
        }
    }
}

impl CalleeTable {
    /// The table's first entry, for [`crate::CallView::callees`]. Compiled
    /// code reads the table through it between runtime calls only. The
    /// entries are made the first time this is asked (a runtime that never
    /// runs compiled code pays nothing for them) and stay where they are.
    pub fn base(&mut self) -> *const u8 {
        if self.entries.is_empty() {
            let n = (1usize << CALLEE_BITS) * CALLEE_WAYS;
            self.entries = (0..n).map(|_| EMPTY).collect();
            self.keep = (0..n).map(|_| None).collect();
            self.fills = vec![0; 1 << CALLEE_BITS];
        }
        self.entries.as_ptr() as *const u8
    }

    /// Whether [`CalleeTable::fill`] would take a callee with this code
    /// block: asked before a frame image is built for it.
    pub fn takes(&self, code: *const usize) -> bool {
        !code.is_null() && !self.entries.is_empty() && self.fills[Self::bucket(code)] < MAX_FILLS
    }

    fn bucket(code: *const usize) -> usize {
        ((code as usize as u64).wrapping_mul(CALLEE_HASH) >> (64 - CALLEE_BITS)) as usize
    }

    /// Whether the table has the function whose code block `code` is the
    /// word of ([`crate::rc_word`]) for realm `realm`. Compiled code calls
    /// through the runtime a function the table has when something else
    /// stops it (a poll due, the nesting bound, no room in a stack): there
    /// is nothing to fill then.
    pub fn has(&self, code: *const usize, realm: u32) -> bool {
        if code.is_null() || self.entries.is_empty() {
            return false;
        }
        let b = Self::bucket(code) * CALLEE_WAYS;
        self.entries[b..b + CALLEE_WAYS]
            .iter()
            .any(|e| e.code == code as usize && e.realm == realm)
    }

    /// Add `callee`: compiled code then calls it directly. Returns what
    /// the entry it replaces kept alive. The caller keeps that until no
    /// compiled code is running: a frame of the old callee may still be on
    /// the native stack, below this call.
    ///
    /// Nothing is added (and `callee.template` is dropped) when the callee
    /// cannot be called directly, its table has fewer inline-cache sites
    /// than its code reads, or its bucket has been filled [`MAX_FILLS`]
    /// times.
    ///
    /// The contract compiled code trusts without checking, which the
    /// runtime meets by construction:
    ///
    /// - `template` is a frame of the runtime's frame stack for a call of a
    ///   function whose code block is the one `code` is the word of, laid
    ///   out as the [`crate::CallLayout`] compiled code was compiled with
    ///   says, and holding its own reference to that block;
    /// - `ic` and `shape_epoch` belong to that block, so they live as long
    ///   as the template does;
    /// - `registers`, `param_base`, `param_regs` and `realm` are that
    ///   block's and that function's, and the parameter registers are
    ///   inside the block's window.
    pub fn fill<V: JitValue>(&mut self, callee: Callee<'_, V>) -> Option<Box<dyn Any>> {
        let info = callee.compiled.info();
        let in_window = callee
            .param_base
            .checked_add(callee.param_regs)
            .is_some_and(|end| end <= callee.registers);
        if !info.direct
            || callee.ic.len() < info.ic
            || callee.code.is_null()
            || !in_window
            || self.entries.is_empty()
        {
            return None;
        }
        let bucket = Self::bucket(callee.code);
        let fills = self.fills[bucket];
        if fills >= MAX_FILLS {
            return None;
        }
        self.fills[bucket] = fills + 1;
        let first = bucket * CALLEE_WAYS;
        // The callee's own entry (another realm's function of the block
        // replaces it), else an empty one, else the ways in turn.
        let ways = first..first + CALLEE_WAYS;
        let at = ways
            .clone()
            .find(|&i| self.entries[i].code == callee.code as usize)
            .or_else(|| ways.clone().find(|&i| self.entries[i].code == 0))
            .unwrap_or(first + fills as usize % CALLEE_WAYS);
        self.entries[at] = CalleeEntry {
            code: callee.code as usize,
            entry: callee.compiled.entry_address(),
            template: &*callee.template as *const dyn Any as *const u8 as usize,
            ic: callee.ic.as_ptr() as usize,
            info: Rc::as_ptr(info) as usize,
            shape_epoch: callee.shape_epoch.as_ptr() as usize,
            registers: callee.registers,
            param_base: callee.param_base,
            param_regs: callee.param_regs,
            realm: callee.realm,
        };
        let keep: Box<dyn Any> = Box::new(Keep {
            _compiled: callee.compiled.clone(),
            _template: callee.template,
        });
        self.keep[at].replace(keep)
    }
}

/// How often a global site is filled before it stops caching: a program
/// that keeps adding and removing global bindings (or properties of any
/// object in dictionary mode, which move the same stamp) would otherwise
/// refill every site at every stamp.
pub const MAX_GLOBAL_FILLS: u32 = 64;

/// One `LdaGlobal` instruction's cache, in code compiled with
/// [`crate::CallLayout::globals`]: where the binding's value was when the
/// runtime last resolved it, the realm it was resolved in, and the
/// runtime's stamp then ([`crate::Runtime::global_stamp`]). Compiled code
/// reads the value there itself while the current realm and the stamp are
/// the same; the runtime fills the site ([`GlobalSite::fill`]).
#[repr(C)]
pub struct GlobalSite {
    stamp: Cell<u64>,
    addr: Cell<usize>,
    realm: Cell<u32>,
    fills: Cell<u32>,
}

/// Field offsets of [`GlobalSite`] for the code generator.
pub(crate) struct GlobalOffsets {
    pub stamp: i32,
    pub addr: i32,
    pub realm: i32,
}

pub(crate) const GLOBAL: GlobalOffsets = GlobalOffsets {
    stamp: offset_of!(GlobalSite, stamp) as i32,
    addr: offset_of!(GlobalSite, addr) as i32,
    realm: offset_of!(GlobalSite, realm) as i32,
};

impl Default for GlobalSite {
    fn default() -> Self {
        GlobalSite {
            // No stamp the runtime gives: it counts up from zero.
            stamp: Cell::new(u64::MAX),
            addr: Cell::new(0),
            realm: Cell::new(u32::MAX),
            fills: Cell::new(0),
        }
    }
}

impl GlobalSite {
    /// Cache a binding: in realm `realm`, while the stamp is `stamp`, the
    /// binding's value is the value at `value`. Nothing is cached once the
    /// site has been filled [`MAX_GLOBAL_FILLS`] times.
    ///
    /// The contract compiled code trusts without checking: until the
    /// runtime's stamp differs from `stamp`, `value` is the address of a
    /// live slot holding either a value of [`crate::layout`] (which
    /// compiled code takes as the binding's value unless its tag is
    /// [`crate::layout::TAG_EMPTY`]) or something whose first byte is above
    /// that tag (which compiled code leaves to the runtime, as it does the
    /// empty value).
    pub fn fill<T>(&self, stamp: u64, realm: u32, value: &T) {
        let fills = self.fills.get();
        if fills >= MAX_GLOBAL_FILLS || stamp == u64::MAX {
            return;
        }
        self.fills.set(fills + 1);
        self.stamp.set(u64::MAX);
        self.addr.set(value as *const T as usize);
        self.realm.set(realm);
        self.stamp.set(stamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_table_has_no_function() {
        let mut t = CalleeTable::default();
        let word = std::ptr::without_provenance::<usize>(0x1000);
        assert!(!t.has(word, 0));
        assert!(!t.takes(word), "no entries yet");
        let base = t.base();
        assert!(!base.is_null());
        assert_eq!(t.base(), base, "the entries stay where they are");
        assert!(!t.has(word, 0));
        assert!(t.takes(word));
        assert!(!t.has(std::ptr::null(), 0));
        assert!(!t.takes(std::ptr::null()));
    }

    #[test]
    fn a_global_site_takes_a_binding_a_bounded_number_of_times() {
        let s = GlobalSite::default();
        let v = 7u64;
        for i in 0..MAX_GLOBAL_FILLS + 5 {
            s.fill(u64::from(i), 1, &v);
        }
        assert_eq!(s.stamp.get(), u64::from(MAX_GLOBAL_FILLS - 1));
        assert_eq!(s.addr.get(), &v as *const u64 as usize);
        assert_eq!(s.realm.get(), 1);
    }
}
