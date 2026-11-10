//! The AgniJS baseline JIT (ADR 0019, ADR 0020, TASK-101.3).
//!
//! A baseline tier in the Sparkplug style: one function's bytecode becomes
//! one machine-code function that keeps the interpreter's frame exactly.
//! The register file, the frame and the accumulator mean what they mean to
//! the interpreter; compiled code only runs the same instructions without
//! the dispatch. What it cannot do inline it hands back, one instruction
//! at a time, to the interpreter's generic semantics through
//! [`Runtime::slow`], with the frame written back first (ADR 0019 J1, J3).
//!
//! Compiled code never holds a JavaScript value anywhere the collector
//! cannot see across a call out of it: the accumulator is flushed to the
//! frame and the register file stays in memory, so collections under
//! compiled code find their roots in the frame stack as they do under the
//! interpreter, and nothing needs a GC map yet. It never speculates: every
//! inline fast path is guarded, and a failed guard runs the instruction's
//! generic semantics instead, so nothing needs deoptimization yet either.
//! Control leaves compiled code only at instruction boundaries where the
//! frame is already the interpreter's ([`Exit`]). Both reserved pieces,
//! deoptimization and GC maps, are specified in [`handoff`].
//!
//! Everything here is safe code except [`exec`]-memory handling, the entry
//! trampoline and the helper trampolines, which live in one small module
//! (ADR 0019 J10).

#![deny(unsafe_code)]

use std::marker::PhantomData;
use std::time::Duration;

use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, IcSite, Instr};
use agnijs_js_parser::ast::FunctionKind;

#[cfg(jit_host)]
mod codegen;
mod exec;
pub mod handoff;

/// The layout of a value as compiled code reads and writes it in the
/// register file and the accumulator: sixteen bytes, a one-byte tag at
/// offset 0 and an eight-byte payload at offset 8 (ADR 0019 J5). The VM's
/// `Value` declares the same constants; [`Compiled::compile`] checks a
/// value type against them before it generates code for it.
pub mod layout {
    pub const SIZE: usize = 16;
    pub const TAG_OFFSET: usize = 0;
    pub const PAYLOAD_OFFSET: usize = 8;
    pub const TAG_UNDEFINED: u8 = 0;
    pub const TAG_NULL: u8 = 1;
    pub const TAG_BOOL: u8 = 2;
    pub const TAG_NUMBER: u8 = 3;
    pub const TAG_STRING: u8 = 4;
    pub const TAG_SYMBOL: u8 = 5;
    pub const TAG_BIGINT: u8 = 6;
    pub const TAG_OBJECT: u8 = 7;
    pub const TAG_EMPTY: u8 = 8;
}

/// Where compiled code finds objects, for the inline-cache hit paths of
/// named and keyed property access (TASK-101.3.2). The runtime computes it
/// once, in safe code, from its own types, and passes `None` to
/// [`Compiled::compile_with`] when any check fails; compiled code then
/// calls out for every property access, as without a layout.
///
/// The layout and the [`HeapView`] are the one contract compiled code
/// trusts without checking: it reads objects at these offsets directly.
/// That is why the runtime derives every field from its types
/// (`offset_of!`, [`vec_layout`], probes of live values) rather than
/// declaring it. Compiled code never changes an object's structure: its
/// only heap writes store a value, of [`layout`], into a data slot or a
/// dense element that its guards have just shown holds one.
///
/// An object is at `objects + id * obj_stride + obj_offset`, where
/// `objects` is the arena base the runtime reports through
/// [`Runtime::heap`] after every call out (the arena may reallocate while
/// the runtime runs, so compiled code never keeps the base, or an address
/// derived from it, across a call out). The other offsets are from the
/// object's start:
///
/// - `shape`: the `u32` shape id; `dict`: a pointer-sized word, non-zero in
///   dictionary mode; `class`: the class tag byte; `proto`: the prototype's
///   `u32` id, `u32::MAX` for none.
/// - `inline`: the first of `inline_slots` contiguous 16-byte property
///   slots, every one of them initialised; `overflow_ptr`, `overflow_len`:
///   the pointer to the slots after them and their number.
/// - A property slot holds a data property exactly as a register holds the
///   value ([`layout`]), so its tag byte is at most [`layout::TAG_EMPTY`];
///   any other tag byte is an accessor.
/// - `array_length`: the `u32` length of an array (class tag `class_array`).
/// - `elements_ptr`, `elements_cap`, `elements_len`: the words of the dense
///   element vector. Elements are dense exactly when the pointer is non-zero
///   and the capacity word, read as signed, is not negative; a dense
///   element is a 16-byte value, [`layout::TAG_EMPTY`] for a hole.
/// - `arguments_mapped`: a pointer-sized word of an arguments object (class
///   tag `class_arguments`), non-zero when the object is mapped.
/// - `not_receiver`, `not_elements`: bit `t` set when objects with class
///   tag `t` never take an inline path as receivers of a named access, or
///   for their elements (an arguments object counts as such only when it is
///   mapped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapLayout {
    pub obj_stride: u32,
    pub obj_offset: u32,
    pub shape: u32,
    pub dict: u32,
    pub class: u32,
    pub proto: u32,
    pub inline: u32,
    pub inline_slots: u32,
    pub overflow_ptr: u32,
    pub overflow_len: u32,
    pub array_length: u32,
    pub elements_ptr: u32,
    pub elements_cap: u32,
    pub elements_len: u32,
    pub arguments_mapped: u32,
    pub class_array: u8,
    pub class_string: u8,
    pub class_arguments: u8,
    pub not_receiver: u64,
    pub not_elements: u64,
}

/// What compiled code reads of the heap besides objects, refreshed by the
/// runtime after every call out ([`Runtime::heap`]).
#[derive(Clone, Copy, Debug)]
pub struct HeapView {
    /// The object arena's base ([`HeapLayout`]), null if there is none.
    pub objects: *mut u8,
    /// The runtime's prototype epoch: a prototype inline-cache entry hits
    /// only in the epoch it was recorded in (ADR 0012).
    pub proto_epoch: u32,
}

impl HeapView {
    pub const NONE: HeapView = HeapView {
        objects: std::ptr::null_mut(),
        proto_epoch: 0,
    };
}

/// Where a vector keeps its buffer pointer, capacity and length, as byte
/// offsets in the vector itself (see [`vec_layout`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VecLayout {
    pub ptr: u32,
    pub cap: u32,
    pub len: u32,
}

/// The layout of `Vec<T>` on this build, found by inspecting a vector of
/// three copies of `sample`; `None` if it is not three distinct words.
pub fn vec_layout<T: Copy>(sample: T) -> Option<VecLayout> {
    exec::vec_layout(sample)
}

/// A value type compiled code can work on. The constructors give the
/// values compiled code writes itself; [`Compiled::compile`] checks their
/// bytes against [`layout`] and refuses a type that does not match, so an
/// implementation cannot make compiled code write an invalid value.
pub trait JitValue: Copy + 'static {
    fn undefined() -> Self;
    fn null() -> Self;
    fn boolean(b: bool) -> Self;
    fn number(n: f64) -> Self;
    fn empty() -> Self;
}

/// Why compiled code returned to its caller. Apart from [`Exit::Return`],
/// the frame stack is exactly as the interpreter would leave it at an
/// instruction boundary: the caller reloads the top frame and goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The function executed `Return`: [`Outcome::acc`] is the
    /// accumulator and [`Outcome::pc`] the pc after the instruction; the
    /// frame has not been written back or popped.
    Return,
    /// An instruction run by [`Runtime::slow`] pushed or popped frames (a
    /// call, a getter): the top frame is no longer this function's, or is
    /// it at a pc compiled code was not told about.
    Frame,
    /// [`Runtime::slow`] or [`Runtime::poll`] raised a fault, which the
    /// runtime keeps.
    Fault,
    /// [`Runtime::slow`] finished the run (an outcome the runtime keeps).
    Done,
    /// Continue in the interpreter at the frame's pc: the runtime asked
    /// for it (a debugger attached, the attention word is set), or the
    /// entry pc is not one compiled code can start at.
    Interp,
}

/// What a runtime call returns to compiled code.
pub enum Resume<V> {
    /// The instruction completed normally: continue at `pc` with `acc`.
    Next { pc: u32, acc: V },
    /// Leave compiled code; the frame has been written back.
    Exit(Exit),
}

/// The interpreter's side of compiled code: the generic semantics of one
/// instruction, the interrupt poll, and the register window. Implemented
/// by the VM in safe code; called only from compiled code, between
/// instructions, with nothing of the function's state outside the frame
/// but the accumulator passed in.
pub trait Runtime<V: JitValue> {
    /// Run the instruction at `pc` by its generic semantics with
    /// accumulator `acc`: write the frame back (`pc + 1`, `acc`), step,
    /// and report where execution continues. Every instruction's effects
    /// complete before it returns (ADR 0019 J3).
    fn slow(&mut self, pc: u32, acc: V) -> Resume<V>;

    /// [`Runtime::slow`] for a `Call` instruction, through its own entry
    /// so that the runtime can take the call without decoding a general
    /// instruction (and run a compiled callee from here).
    fn call(&mut self, pc: u32, acc: V) -> Resume<V> {
        self.slow(pc, acc)
    }

    /// [`Runtime::slow`] for a global or environment load or store
    /// (`LdaGlobal`, `LdaEnv`, `StaEnv`), through its own
    /// entry: the common case is a cache hit or a slot read, which a small
    /// function serves at a fraction of the general path's cost.
    ///
    /// `Some` is the new accumulator, the instruction done, and promises
    /// that nothing compiled code caches changed: no allocation, no
    /// interrupt-counter change, no safe point due, the register window
    /// and heap view as they were. `None` runs the instruction by
    /// [`Runtime::slow`] instead.
    fn fast(&mut self, pc: u32, acc: V) -> Option<V> {
        let _ = (pc, acc);
        None
    }
    /// The running frame's `this`, which compiled code loads itself for
    /// `LdaThis` (it is fixed for a frame's life). Asked when code that
    /// has an `LdaThis` is entered; `None` leaves such code unentered.
    fn this_value(&mut self) -> Option<V> {
        None
    }
    /// A loop back-edge from the jump at `pc` to `target` whose interrupt
    /// poll is due (the counter, already advanced, is a multiple of
    /// 0x1000: ask the host), or taken while [`Runtime::safe_point_due`]
    /// said yes (service it). `Next` continues at `target`.
    fn poll(&mut self, pc: u32, target: u32, acc: V) -> Resume<V>;
    /// Whether the runtime wants every back-edge to be a safe point that
    /// calls [`Runtime::poll`], as under GC stress, where the interpreter
    /// collects at every loop head. Asked on entry and after every call out
    /// that continues compiled code.
    fn safe_point_due(&self) -> bool {
        false
    }
    /// The running function's register window (from its base). Compiled
    /// code addresses registers through it until the next runtime call:
    /// the register file may reallocate when frames are pushed.
    fn registers(&mut self) -> &mut [V];
    /// The VM's interrupt-poll counter (ADR 0019 J7); compiled code
    /// advances a copy at back-edges and hands it back at every call out.
    fn interrupt_counter(&self) -> u32;
    fn set_interrupt_counter(&mut self, counter: u32);
    /// `a OP b` on two numbers for the operators compiled code does not
    /// expand itself. Pure: it sees no runtime state, must not panic, and
    /// is called without the frame being written back.
    fn arith(op: Arith, a: f64, b: f64) -> f64;
    /// The object arena and the prototype epoch, for code compiled with a
    /// [`HeapLayout`]. Called when compiled code is entered and after every
    /// call out that continues it, never while it runs. The default reports
    /// no heap, and code compiled with a layout is then not entered.
    fn heap(&mut self) -> HeapView {
        HeapView::NONE
    }
    /// The running block's inline-cache sites (its `Feedback::ic`), for code
    /// compiled with a [`HeapLayout`]; empty if this runtime keeps no
    /// feedback for the block, and code that has sites is then not entered.
    /// Called when compiled code is entered. Compiled code reads the entries
    /// in place for the rest of the run (they are `Cell`s, which the runtime
    /// may update meanwhile), so the table must stay where it is while the
    /// block runs, as a block's feedback does.
    fn ic_table(&mut self) -> &[IcSite] {
        &[]
    }
}

/// The number operators compiled code calls [`Runtime::arith`] for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Arith {
    /// `%` (ECMAScript `Number::remainder`).
    Mod = 0,
    /// `**` (`Number::exponentiate`).
    Exp = 1,
}

/// Where a run of compiled code ended.
pub struct Outcome<V> {
    pub exit: Exit,
    /// For [`Exit::Return`]: the pc after the `Return`.
    pub pc: u32,
    /// For [`Exit::Return`]: the accumulator.
    pub acc: V,
}

/// Why a code block has no compiled code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ineligible {
    /// No executable memory or no code generator on this platform, or the
    /// start-up probe failed (ADR 0020).
    Unavailable,
    /// The value type does not have the [`layout`] compiled code assumes.
    Layout,
    /// Generators, async functions, class constructors, modules and eval
    /// code stay in the interpreter.
    Kind,
    /// An instruction the baseline tier does not handle, at this pc.
    Instruction(u32),
    TooLarge,
    /// The code generator refused the function (a bug).
    Compile(String),
}

/// The largest block compiled (instructions). Larger blocks are rarely
/// hot and take long to compile.
pub const MAX_INSTRS: usize = 20_000;

/// Whether `code` can be compiled: a check on the block alone, before any
/// code is generated. Generators and async functions suspend their frames
/// and class constructors bind `this` late; eval and module code run once;
/// direct eval and `super()` inspect the frame in ways compiled code does
/// not model; `debugger` belongs to the interpreter (ADR 0019 J9).
pub fn eligible(code: &CodeBlock) -> Result<(), Ineligible> {
    if code.function_kind != FunctionKind::Normal
        || !matches!(
            code.kind,
            CodeKind::Script | CodeKind::Function | CodeKind::Arrow | CodeKind::Method
        )
        || code.is_derived_constructor
    {
        return Err(Ineligible::Kind);
    }
    if code.instrs.is_empty() || code.instrs.len() > MAX_INSTRS {
        return Err(Ineligible::TooLarge);
    }
    for (pc, instr) in code.instrs.iter().enumerate() {
        if matches!(
            instr,
            Instr::InitialYield
                | Instr::Yield { .. }
                | Instr::YieldRaw { .. }
                | Instr::Await
                | Instr::ResumeDispatch { .. }
                | Instr::Debugger
                | Instr::CallEval { .. }
                | Instr::CallEvalSpread { .. }
                | Instr::SuperCall { .. }
                | Instr::SuperCallSpread { .. }
        ) {
            return Err(Ineligible::Instruction(pc as u32));
        }
    }
    Ok(())
}

/// Whether compiled code can be expected to beat the interpreter on
/// `code`. Every instruction compiled code does not expand leaves machine
/// code for the runtime ([`Runtime::slow`]), which costs more than the
/// interpreter's own dispatch of the same instruction: a call out, the
/// frame handed over, a second decode. Calls cost more still, since they
/// leave compiled code altogether and re-enter it when the callee returns.
/// So a block is worth compiling only when most of its instructions run
/// inline: at most one in [`CALL_OUT_SHARE`] calls out, counting a call
/// as two. A tiering policy applies this; forced compilation (for tests)
/// does not.
pub fn profitable(code: &CodeBlock) -> bool {
    profitable_with(code, false)
}

/// [`profitable`], for code compiled with a [`HeapLayout`] when `heap`:
/// named and keyed property accesses then run inline on a cache hit.
///
/// A block without a loop is not worth it either: it is entered once per
/// call, and entering compiled code (and leaving it for every call the
/// block makes) costs more than a short block's dispatch saves.
pub fn profitable_with(code: &CodeBlock, heap: bool) -> bool {
    let has_loop = code
        .instrs
        .iter()
        .any(|i| codegen_branch_offset(i).is_some_and(|o| o <= 0));
    if !has_loop {
        return false;
    }
    let mut cost = 0usize;
    for instr in code.instrs.iter() {
        cost += match instr {
            i if expands_inline(code, i) => 0,
            Instr::GetNamed { ic, .. } | Instr::SetNamed { ic, .. } if heap => named_site_cost(code, *ic),
            Instr::GetKeyed { .. } | Instr::SetKeyed { .. } if heap => 0,
            Instr::Call { .. }
            | Instr::CallSpread { .. }
            | Instr::Construct { .. }
            | Instr::ConstructSpread { .. } => 2,
            _ => 1,
        };
    }
    cost * CALL_OUT_SHARE <= code.instrs.len()
}

/// See [`profitable`].
pub const CALL_OUT_SHARE: usize = 6;

/// The [`profitable`] cost of named access site `ic`, judged by the
/// feedback the block has so far: 0 when compiled code can run its hits
/// itself, 1 when it calls out (megamorphic, or a kind compiled code does
/// not expand), 2 when it has seen an accessor (a call).
fn named_site_cost(code: &CodeBlock, ic: u16) -> usize {
    use agnijs_js_compiler::bytecode::IcEntry;
    if ic as usize >= code.ic_count as usize {
        return 1;
    }
    let Some(site) = code.feedback.get().and_then(|f| f.ic.get(ic as usize)) else {
        return 0;
    };
    let mut cost = 0;
    for e in site.entries.iter() {
        cost = cost.max(match e.get().kind {
            IcEntry::KIND_EMPTY | IcEntry::KIND_OWN_DATA | IcEntry::KIND_PROTO_DATA | IcEntry::KIND_ARRAY_LENGTH => 0,
            IcEntry::KIND_OWN_ACCESSOR | IcEntry::KIND_PROTO_ACCESSOR => 2,
            _ => 1,
        });
    }
    cost
}

/// See [`codegen`]'s branch offsets; `None` on hosts without one.
fn codegen_branch_offset(instr: &Instr) -> Option<i32> {
    #[cfg(jit_host)]
    return codegen::branch_offset(instr);
    #[cfg(not(jit_host))]
    {
        let _ = instr;
        None
    }
}

/// Whether compiled code expands `instr` itself, at least for numbers
/// (the code generator's inline cases; others call out).
fn expands_inline(code: &CodeBlock, instr: &Instr) -> bool {
    match *instr {
        Instr::LdaConst(c) => matches!(
            code.constants.get(c as usize),
            Some(agnijs_js_compiler::bytecode::Constant::Number(_))
        ),
        Instr::BinRR { op, .. } | Instr::BinRI { op, .. } | Instr::BinAR { op, .. } | Instr::BinAI { op, .. } => {
            !matches!(
                op,
                agnijs_js_compiler::bytecode::BinOp::InstanceOf | agnijs_js_compiler::bytecode::BinOp::In
            )
        }
        Instr::JumpIfCmpRR { op, .. }
        | Instr::JumpIfNotCmpRR { op, .. }
        | Instr::JumpIfCmpRI { op, .. }
        | Instr::JumpIfNotCmpRI { op, .. }
        | Instr::JumpIfCmpRA { op, .. }
        | Instr::JumpIfNotCmpRA { op, .. }
        | Instr::JumpIfCmpAI { op, .. }
        | Instr::JumpIfNotCmpAI { op, .. } => op.is_comparison(),
        Instr::LdaUndefined
        | Instr::LdaNull
        | Instr::LdaTrue
        | Instr::LdaFalse
        | Instr::LdaEmpty
        | Instr::LdaInt(_)
        | Instr::Ldar(_)
        | Instr::Star(_)
        | Instr::Mov { .. }
        | Instr::Nop
        | Instr::Add(_)
        | Instr::Sub(_)
        | Instr::Mul(_)
        | Instr::Div(_)
        | Instr::Mod(_)
        | Instr::Exp(_)
        | Instr::Shl(_)
        | Instr::Shr(_)
        | Instr::UShr(_)
        | Instr::BitAnd(_)
        | Instr::BitOr(_)
        | Instr::BitXor(_)
        | Instr::Eq(_)
        | Instr::Ne(_)
        | Instr::StrictEq(_)
        | Instr::StrictNe(_)
        | Instr::Lt(_)
        | Instr::Le(_)
        | Instr::Gt(_)
        | Instr::Ge(_)
        | Instr::Inc
        | Instr::Dec
        | Instr::ToNumeric
        | Instr::CheckTdz(_)
        | Instr::Jump(_)
        | Instr::JumpIfTrue(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfNullish(_)
        | Instr::JumpIfNotNullish(_)
        | Instr::JumpIfUndefined(_)
        | Instr::JumpIfNotUndefined(_)
        | Instr::JumpIfEmpty(_)
        | Instr::JumpIfNotEmpty(_)
        | Instr::JumpIfObject(_)
        | Instr::JumpIfStrictEq { .. }
        | Instr::Return => true,
        _ => false,
    }
}

/// Whether this process can run compiled code: a supported target and a
/// start-up probe that compiled and ran a function (cached).
pub fn available() -> bool {
    exec::available()
}

/// How executable memory is obtained on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecMemoryKind {
    /// Pages mapped read-write, filled, then made read-execute.
    Protect,
    /// Apple silicon: a `MAP_JIT` mapping, written with this thread's JIT
    /// write protection lifted (`pthread_jit_write_protect_np`), which
    /// hardened-runtime processes need (the allow-jit entitlement).
    MapJit,
}

/// The kind of executable memory the most recent mapping used (`None`
/// before any, or where the JIT is unavailable). The probe behind
/// [`available`] makes one.
pub fn exec_memory_kind() -> Option<ExecMemoryKind> {
    exec::memory_kind()
}

/// One code block's machine code.
pub struct Compiled<V> {
    #[cfg_attr(not(jit_host), allow(dead_code))]
    mem: exec::ExecMemory,
    /// `entry[pc]`: compiled code can start at `pc`.
    entry: Box<[bool]>,
    registers: usize,
    /// Compiled with a [`HeapLayout`]: entered only with a heap view, and
    /// (`needs_ic`) an inline-cache table.
    needs_heap: bool,
    needs_ic: usize,
    /// Has an `LdaThis`: entered only with [`Runtime::this_value`].
    needs_this: bool,
    /// Size of the machine code in bytes.
    pub code_size: usize,
    /// Time spent generating it.
    pub compile_time: Duration,
    _value: PhantomData<fn(V) -> V>,
}

impl<V: JitValue> Compiled<V> {
    /// Compile `code`, which must be [`eligible`].
    pub fn compile(code: &CodeBlock) -> Result<Self, Ineligible> {
        Self::compile_with(code, None)
    }

    /// Compile `code`, with the inline-cache hit paths when the runtime
    /// describes its heap (`heap`); see [`HeapLayout`].
    pub fn compile_with(code: &CodeBlock, heap: Option<&HeapLayout>) -> Result<Self, Ineligible> {
        eligible(code)?;
        if !available() {
            return Err(Ineligible::Unavailable);
        }
        if !exec::layout_matches::<V>() {
            return Err(Ineligible::Layout);
        }
        Self::generate(code, heap)
    }

    #[cfg(jit_host)]
    fn generate(code: &CodeBlock, heap: Option<&HeapLayout>) -> Result<Self, Ineligible> {
        let start = std::time::Instant::now();
        let lowered = codegen::compile(code, &exec::offsets::<V>(), heap).map_err(Ineligible::Compile)?;
        let mem = exec::ExecMemory::new(&lowered.bytes).ok_or(Ineligible::Unavailable)?;
        let mut entry = vec![false; code.instrs.len()].into_boxed_slice();
        for &pc in &lowered.entries {
            entry[pc as usize] = true;
        }
        Ok(Compiled {
            mem,
            entry,
            registers: code.register_count as usize,
            needs_heap: heap.is_some(),
            needs_ic: if heap.is_some() { code.ic_count as usize } else { 0 },
            needs_this: code.instrs.iter().any(|i| matches!(i, Instr::LdaThis)),
            code_size: lowered.bytes.len(),
            compile_time: start.elapsed(),
            _value: PhantomData,
        })
    }

    #[cfg(not(jit_host))]
    fn generate(_code: &CodeBlock, _heap: Option<&HeapLayout>) -> Result<Self, Ineligible> {
        Err(Ineligible::Unavailable)
    }

    /// Whether compiled code can start at `pc`: the function's start, a
    /// loop header, or the instruction after one that may call out.
    #[inline]
    pub fn is_entry(&self, pc: usize) -> bool {
        self.entry.get(pc).copied().unwrap_or(false)
    }

    /// Run from `pc` (an entry, else [`Exit::Interp`] at once) with the
    /// accumulator `acc`, the top frame being this function's.
    #[inline]
    pub fn enter<R: Runtime<V>>(&self, rt: &mut R, pc: u32, acc: V) -> Outcome<V> {
        if !self.is_entry(pc as usize) {
            return Outcome {
                exit: Exit::Interp,
                pc,
                acc,
            };
        }
        exec::enter(
            &self.mem,
            exec::Needs {
                registers: self.registers,
                heap: self.needs_heap,
                ic: self.needs_ic,
                this: self.needs_this,
            },
            rt,
            pc,
            acc,
        )
    }
}
