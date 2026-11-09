//! Serialised bytecode: a compiled [`CodeBlock`] tree as bytes, and back.
//!
//! The payload is engine-private (the container header that names the
//! format lives in the engine). Layout, all integers little-endian:
//!
//! - a string table (`Rc<str>`: names, bindings, specifiers' local names),
//!   a UTF-16 table (`Rc<[u16]>`: string constants, template strings,
//!   module specifiers and export names) and a source table (script name,
//!   text and lone-surrogate table, stored once and shared by every function
//!   compiled from it);
//! - the root code block, with nested functions inline, every string an
//!   index into the tables.
//!
//! Within a block the pools an instruction can index (constants, names,
//! scopes, the counts of nested functions and inline caches) come before
//! the instructions, so each instruction is verified as it is decoded, in
//! one pass. Every count is written ahead of its items and the decoder
//! allocates each pool at its final size.
//!
//! Inline caches are runtime state (shape ids of one heap), so only their
//! count is stored and they start empty.
//!
//! The debug section (statement starts and register-resident locals) is
//! prefixed by its byte length. Loading skips it and keeps the bytes in the
//! block's [`DebugSection`], which decodes and verifies them the first time
//! a debugger asks; nothing on the run path needs them.
//!
//! One instruction table ([`instr_table!`]) drives the encoder, the decoder
//! and the verifier, so an instruction cannot be added to one and missed by
//! the others: the encoder's match is exhaustive. Tags are the variants'
//! positions in the table, so reordering it changes the wire format, which
//! the engine's format tag must then name.
//!
//! [`decode`] never panics on bad input. Beyond the wire structure it
//! verifies every operand that indexes a pool of its own block (registers,
//! constants and their kinds, names, nested functions, scopes, inline
//! caches, jump targets, exception handlers, spans and offsets against the
//! source text), so a decoded tree cannot index out of bounds in the
//! interpreter through them. Operands that depend on runtime context
//! (environment depth and slot, module import indices) are taken as
//! produced; the host's bundle format is responsible for integrity
//! (ADR 0010).

use crate::bytecode::*;
use agnijs_js_parser::ast::FunctionKind;
use agnijs_js_parser::source::LoneSurrogate;
use agnijs_js_parser::span::Span;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// Why a payload could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended inside a value.
    Truncated,
    /// Bytes remain after the root code block.
    TrailingBytes,
    /// A value was well-formed on the wire but not a valid tree.
    Invalid(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("truncated bytecode"),
            DecodeError::TrailingBytes => f.write_str("trailing bytes after bytecode"),
            DecodeError::Invalid(m) => write!(f, "invalid bytecode: {m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

type Result<T> = std::result::Result<T, DecodeError>;

fn invalid<T>(m: impl Into<String>) -> Result<T> {
    Err(DecodeError::Invalid(m.into()))
}

/// Nested functions deeper than this are refused (decoding recurses).
pub const MAX_NESTING: usize = 256;

/// `n` items read by `f`, in a vector allocated once at its final size.
/// (`(0..n).map(f).collect::<Result<Vec<_>>>()` cannot reserve: the
/// `Result` adapter reports a lower bound of zero, so the vector grows.)
fn collect_n<T>(n: usize, mut f: impl FnMut() -> Result<T>) -> Result<Vec<T>> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(f()?);
    }
    Ok(out)
}

// ---- writer and reader --------------------------------------------------------------------------

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    strs: HashMap<Rc<str>, u32>,
    str_list: Vec<Rc<str>>,
    units: HashMap<Rc<[u16]>, u32>,
    unit_list: Vec<Rc<[u16]>>,
    sources: Vec<Rc<SourceText>>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("bytecode pool exceeds u32"));
    }
    /// LEB128: seven bits per byte, low bits first.
    fn var(&mut self, mut v: u32) {
        while v >= 0x80 {
            self.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }
    fn str_index(&mut self, s: &Rc<str>) -> u32 {
        let next = self.str_list.len() as u32;
        let i = *self.strs.entry(s.clone()).or_insert_with(|| next);
        if i == next {
            self.str_list.push(s.clone());
        }
        i
    }
    fn str(&mut self, s: &Rc<str>) {
        let i = self.str_index(s);
        self.u32(i);
    }
    fn strs(&mut self, list: &[Rc<str>]) {
        self.len(list.len());
        for s in list {
            self.str(s);
        }
    }
    fn units(&mut self, s: &Rc<[u16]>) {
        let next = self.unit_list.len() as u32;
        let i = *self.units.entry(s.clone()).or_insert_with(|| next);
        if i == next {
            self.unit_list.push(s.clone());
        }
        self.u32(i);
    }
    fn opt_units(&mut self, s: &Option<Rc<[u16]>>) {
        match s {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.units(s);
            }
        }
    }
    fn source(&mut self, s: &Rc<SourceText>) {
        let i = match self.sources.iter().position(|x| Rc::ptr_eq(x, s)) {
            Some(i) => i,
            None => {
                self.sources.push(s.clone());
                self.sources.len() - 1
            }
        };
        self.len(i);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Shared with every block's packed debug section, whose locals name
    /// strings by index.
    strs: Rc<[Rc<str>]>,
    units: Vec<Rc<[u16]>>,
    sources: Vec<Rc<SourceText>>,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let s = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => invalid(format!("boolean byte {b}")),
        }
    }
    /// A count of items each at least `min_size` bytes long: refused when
    /// the rest of the input cannot hold them, so corrupt counts never
    /// drive a large allocation.
    fn count(&mut self, min_size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_size.max(1)) > self.bytes.len() - self.pos {
            return Err(DecodeError::Truncated);
        }
        Ok(n)
    }
    fn var(&mut self) -> Result<u32> {
        let mut v: u32 = 0;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            if shift == 28 && b > 0x0F {
                return invalid("variable-length integer exceeds 32 bits");
            }
            v |= ((b & 0x7F) as u32) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        unreachable!("the fifth byte either ends the integer or is refused")
    }
    fn str(&mut self) -> Result<Rc<str>> {
        let i = self.u32()? as usize;
        self.str_at(i)
    }
    fn str_at(&mut self, i: usize) -> Result<Rc<str>> {
        match self.strs.get(i) {
            Some(s) => Ok(s.clone()),
            None => invalid(format!("string index {i} out of range")),
        }
    }
    fn strs(&mut self) -> Result<Vec<Rc<str>>> {
        let n = self.count(4)?;
        collect_n(n, || self.str())
    }
    fn units(&mut self) -> Result<Rc<[u16]>> {
        let i = self.u32()? as usize;
        match self.units.get(i) {
            Some(s) => Ok(s.clone()),
            None => invalid(format!("UTF-16 string index {i} out of range")),
        }
    }
    fn opt_units(&mut self) -> Result<Option<Rc<[u16]>>> {
        Ok(if self.bool()? {
            Some(self.units()?)
        } else {
            None
        })
    }
    fn source(&mut self) -> Result<Rc<SourceText>> {
        let i = self.u32()? as usize;
        match self.sources.get(i) {
            Some(s) => Ok(s.clone()),
            None => invalid(format!("source index {i} out of range")),
        }
    }
}

// ---- operand wire types -------------------------------------------------------------------------

trait Wire: Sized + Copy {
    fn put(self, w: &mut Writer);
    fn get(r: &mut Reader) -> Result<Self>;
}

impl Wire for u16 {
    fn put(self, w: &mut Writer) {
        w.u16(self)
    }
    fn get(r: &mut Reader) -> Result<Self> {
        r.u16()
    }
}
impl Wire for u32 {
    fn put(self, w: &mut Writer) {
        w.u32(self)
    }
    fn get(r: &mut Reader) -> Result<Self> {
        r.u32()
    }
}
impl Wire for i32 {
    fn put(self, w: &mut Writer) {
        w.u32(self as u32)
    }
    fn get(r: &mut Reader) -> Result<Self> {
        Ok(r.u32()? as i32)
    }
}
impl Wire for bool {
    fn put(self, w: &mut Writer) {
        w.u8(self as u8)
    }
    fn get(r: &mut Reader) -> Result<Self> {
        r.bool()
    }
}

/// Fieldless enums travel as their position in a fixed list.
macro_rules! wire_enum {
    ($ty:ident { $($v:ident),+ $(,)? }) => {
        impl Wire for $ty {
            fn put(self, w: &mut Writer) {
                let all = [$($ty::$v),+];
                w.u8(all.iter().position(|x| *x == self).unwrap() as u8)
            }
            fn get(r: &mut Reader) -> Result<Self> {
                let all = [$($ty::$v),+];
                let b = r.u8()?;
                match all.get(b as usize) {
                    Some(v) => Ok(*v),
                    None => invalid(format!("{} byte {b}", stringify!($ty))),
                }
            }
        }
    };
}

wire_enum!(ErrorKind {
    Type,
    Reference,
    Syntax,
    Range
});
wire_enum!(NamePrefix { None, Get, Set });
wire_enum!(BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    InstanceOf,
    In
});
wire_enum!(PrivateKind {
    Method,
    Getter,
    Setter
});
wire_enum!(CodeKind {
    Script,
    Module,
    Eval,
    Function,
    Arrow,
    Method,
    ClassConstructor,
    ClassInitializer
});
wire_enum!(FunctionKind {
    Normal,
    Generator,
    Async,
    AsyncGenerator
});
wire_enum!(ScopeKind {
    Function,
    Block,
    Catch,
    Class,
    Module,
    Eval
});
wire_enum!(BindingKind {
    Var,
    Let,
    Const,
    Class,
    Function,
    Param,
    CatchParam,
    This,
    Internal,
    FuncName
});

// ---- the instruction table ----------------------------------------------------------------------

/// Checks operands against the pools of the block being decoded, which are
/// all read before its instructions. Each method is named after an operand
/// kind in the instruction table.
struct Verifier<'a> {
    register_count: Reg,
    constants: &'a [Constant],
    names: usize,
    functions: usize,
    scopes: usize,
    ic: usize,
    /// The block's instruction count (jump targets).
    len: usize,
    pc: usize,
}

impl Verifier<'_> {
    fn raw<T>(&mut self, _: T) -> Result<()> {
        Ok(())
    }
    fn reg(&mut self, r: Reg) -> Result<()> {
        if r < self.register_count {
            Ok(())
        } else {
            invalid(format!(
                "register r{r} at pc {} beyond {} registers",
                self.pc, self.register_count
            ))
        }
    }
    fn constant(&self, c: ConstIdx) -> Result<&Constant> {
        match self.constants.get(c as usize) {
            Some(k) => Ok(k),
            None => invalid(format!("constant {c} at pc {} out of range", self.pc)),
        }
    }
    /// Any constant (messages).
    fn msg(&mut self, c: ConstIdx) -> Result<()> {
        self.constant(c).map(|_| ())
    }
    /// A constant loaded as a value: anything but a template.
    fn konst(&mut self, c: ConstIdx) -> Result<()> {
        match self.constant(c)? {
            Constant::Template(_) => invalid(format!("template constant {c} loaded as a value")),
            _ => Ok(()),
        }
    }
    fn string(&mut self, c: ConstIdx) -> Result<()> {
        match self.constant(c)? {
            Constant::String(_) => Ok(()),
            _ => invalid(format!("constant {c} at pc {} is not a string", self.pc)),
        }
    }
    fn tmpl(&mut self, c: ConstIdx) -> Result<()> {
        match self.constant(c)? {
            Constant::Template(_) => Ok(()),
            _ => invalid(format!("constant {c} at pc {} is not a template", self.pc)),
        }
    }
    fn name(&mut self, n: NameIdx) -> Result<()> {
        if (n as usize) < self.names {
            Ok(())
        } else {
            invalid(format!("name {n} at pc {} out of range", self.pc))
        }
    }
    fn func(&mut self, f: FuncIdx) -> Result<()> {
        if (f as usize) < self.functions {
            Ok(())
        } else {
            invalid(format!("function {f} at pc {} out of range", self.pc))
        }
    }
    fn scope(&mut self, s: ScopeIdx) -> Result<()> {
        if (s as usize) < self.scopes {
            Ok(())
        } else {
            invalid(format!("scope {s} at pc {} out of range", self.pc))
        }
    }
    fn ic(&mut self, i: IcIdx) -> Result<()> {
        if i == NO_IC || (i as usize) < self.ic {
            Ok(())
        } else {
            invalid(format!("inline cache {i} at pc {} out of range", self.pc))
        }
    }
    /// The operator of a fused compare-and-branch: a comparison.
    fn cmp(&mut self, op: BinOp) -> Result<()> {
        if op.is_comparison() {
            Ok(())
        } else {
            invalid(format!("{op:?} at pc {} is not a comparison", self.pc))
        }
    }
    fn off(&mut self, o: Offset) -> Result<()> {
        let target = self.pc as i64 + o as i64;
        if target >= 0 && (target as usize) < self.len {
            Ok(())
        } else {
            invalid(format!(
                "jump at pc {} to {target} leaves the block",
                self.pc
            ))
        }
    }
    /// `count` registers from `base`.
    fn range(&mut self, base: Reg, count: u16) -> Result<()> {
        if base as u32 + count as u32 <= self.register_count as u32 {
            Ok(())
        } else {
            invalid(format!(
                "register range r{base}..+{count} at pc {} beyond {} registers",
                self.pc, self.register_count
            ))
        }
    }
}

macro_rules! instr_table {
    ($( $v:ident $( ( $($p:ident : $pk:ident),+ ) )? $( { $($f:ident : $fk:ident),+ } )? ;)*) => {
        #[allow(dead_code)]
        #[repr(u8)]
        enum Tag { $($v),* }

        #[allow(non_upper_case_globals)]
        mod tag {
            $( pub const $v: u8 = super::Tag::$v as u8; )*
        }

        fn put_instr(w: &mut Writer, i: Instr) {
            match i {
                $( Instr::$v $( ( $($p),+ ) )? $( { $($f),+ } )? => {
                    w.u8(tag::$v);
                    $( $( $p.put(w); )+ )?
                    $( $( $f.put(w); )+ )?
                } )*
            }
        }

        fn get_instr(r: &mut Reader) -> Result<Instr> {
            Ok(match r.u8()? {
                $( tag::$v => Instr::$v
                    $( ( $( { let $p = Wire::get(r)?; $p } ),+ ) )?
                    $( { $( $f: Wire::get(r)? ),+ } )?, )*
                t => return invalid(format!("instruction tag {t}")),
            })
        }

        fn verify_operands(v: &mut Verifier, i: Instr) -> Result<()> {
            match i {
                $( Instr::$v $( ( $($p),+ ) )? $( { $($f),+ } )? => {
                    $( $( v.$pk($p)?; )+ )?
                    $( $( v.$fk($f)?; )+ )?
                } )*
            }
            Ok(())
        }
    };
}

instr_table! {
    LdaUndefined; LdaNull; LdaTrue; LdaFalse; LdaEmpty;
    LdaInt(a: raw);
    LdaConst(a: konst);
    Ldar(a: reg);
    Star(a: reg);
    Mov { dst: reg, src: reg };
    PushEnv(a: scope);
    PopEnv; CopyEnv; PushWith;
    LdaEnv { depth: raw, slot: raw };
    StaEnv { depth: raw, slot: raw };
    CheckTdz(a: name);
    ThrowConstAssign(a: name);
    LdaGlobal(a: name);
    LdaGlobalOrUndefined(a: name);
    StaGlobal { name: name, strict: raw };
    InitGlobalLexical(a: name);
    LdaDynamic(a: name);
    LdaDynamicOrUndefined(a: name);
    StaDynamic { name: name, strict: raw };
    StaEvalVar(a: name);
    InitDynamic(a: name);
    DeleteDynamic(a: name);
    LdaRefThis(a: reg);
    ResolveDynamic { name: name, dst: reg };
    LdaDynamicRef { name: name, reference: reg };
    StaDynamicRef { name: name, reference: reg, strict: raw };
    ThisForDynamicCall(a: name);
    DeclareGlobals;
    LdaImport(a: raw);
    GetNamed { obj: reg, name: name, ic: ic };
    SetNamed { obj: reg, name: name, strict: raw, ic: ic };
    GetKeyed { obj: reg, key: reg };
    SetKeyed { obj: reg, key: reg, strict: raw };
    DeleteNamed { obj: reg, name: name, strict: raw };
    DeleteKeyed { obj: reg, key: reg, strict: raw };
    GetSuper { this: reg, func: reg, key: reg };
    SetSuper { this: reg, func: reg, key: reg, strict: raw };
    DefineOwn { obj: reg, key: reg };
    DefineOwnNamed { obj: reg, name: name };
    DefineMethod { obj: reg, key: reg, enumerable: raw };
    DefineGetter { obj: reg, key: reg, enumerable: raw };
    DefineSetter { obj: reg, key: reg, enumerable: raw };
    SetHomeObject { func: reg, home: reg };
    SetFunctionName { key: reg, prefix: raw };
    SetPrototypeLiteral { obj: reg };
    CopyDataProperties { target: reg, excluded: raw, count: raw };
    NewPrivateName(a: name);
    GetPrivate { obj: reg, name: reg };
    SetPrivate { obj: reg, name: reg };
    HasPrivate { obj: reg, name: reg };
    SetPrivateMethod { name: reg, kind: raw };
    AddPrivateBrand { obj: reg, name: reg };
    Add(a: reg); Sub(a: reg); Mul(a: reg); Div(a: reg); Mod(a: reg); Exp(a: reg);
    Shl(a: reg); Shr(a: reg); UShr(a: reg); BitAnd(a: reg); BitOr(a: reg); BitXor(a: reg);
    Eq(a: reg); Ne(a: reg); StrictEq(a: reg); StrictNe(a: reg);
    Lt(a: reg); Le(a: reg); Gt(a: reg); Ge(a: reg); InstanceOf(a: reg); In(a: reg);
    BinRR { op: raw, a: reg, b: reg };
    BinRI { op: raw, a: reg, imm: raw };
    BinAR { op: raw, b: reg };
    BinAI { op: raw, imm: raw };
    Neg; Pos; Not; BitNot; Typeof; ToNumeric; Inc; Dec; ToString; ToPropertyKey;
    RequireObjectCoercible;
    Jump(a: off); JumpIfTrue(a: off); JumpIfFalse(a: off);
    JumpIfNullish(a: off); JumpIfNotNullish(a: off);
    JumpIfUndefined(a: off); JumpIfNotUndefined(a: off);
    JumpIfEmpty(a: off); JumpIfNotEmpty(a: off); JumpIfObject(a: off);
    JumpIfStrictEq { reg: reg, offset: off };
    JumpIfCmpRR { op: cmp, a: reg, b: reg, offset: off };
    JumpIfNotCmpRR { op: cmp, a: reg, b: reg, offset: off };
    JumpIfCmpRI { op: cmp, a: reg, imm: raw, offset: off };
    JumpIfNotCmpRI { op: cmp, a: reg, imm: raw, offset: off };
    JumpIfCmpRA { op: cmp, a: reg, offset: off };
    JumpIfNotCmpRA { op: cmp, a: reg, offset: off };
    JumpIfCmpAI { op: cmp, imm: raw, offset: off };
    JumpIfNotCmpAI { op: cmp, imm: raw, offset: off };
    Call { callee: reg, this: reg, args: raw, argc: raw };
    CallSpread { callee: reg, this: reg, args: reg };
    CallEval { callee: reg, this: reg, args: raw, argc: raw, strict: raw };
    CallEvalSpread { callee: reg, this: reg, args: reg, strict: raw };
    Construct { callee: reg, args: raw, argc: raw };
    ConstructSpread { callee: reg, args: reg };
    SuperCall { func: reg, new_target: reg, args: raw, argc: raw };
    SuperCallSpread { func: reg, new_target: reg, args: reg };
    Return;
    CheckDerivedReturn { this: reg };
    Throw;
    ThrowError { kind: raw, message: msg };
    ThrowIfNotObject(a: msg);
    CreateObject; CreateArray;
    ArrayPush { arr: reg };
    ArrayPushHole { arr: reg };
    ArraySpread { arr: reg };
    CreateClosure(a: func);
    CreateClass { func: func, heritage: reg, proto: reg, name: reg };
    AddField { ctor: reg, key: reg, init: reg, anon: raw };
    DefineField { obj: reg, key: reg, anon: raw };
    AddInstancePrivateMethod { ctor: reg, name: reg };
    InitializeInstanceElements { func: reg, this: reg };
    CreateRegExp { pattern: string, flags: string };
    GetTemplateObject(a: tmpl);
    CreateArguments { mapped: raw };
    CreateRestArray { from: raw };
    LdaCallee; LdaNewTarget; LdaThis; ImportMeta;
    DynamicImport { spec: reg, options: reg };
    GetIterator { iter: reg, next: reg };
    GetAsyncIterator { iter: reg, next: reg };
    IteratorStep { iter: reg, next: reg, done: off };
    IteratorNextRaw { iter: reg, next: reg };
    IteratorResult { done: off };
    IteratorClose { iter: reg };
    IteratorCloseSwallow { iter: reg };
    GetIteratorReturn { iter: reg };
    ForInPrepare { dst: reg };
    ForInNext { iter: reg, done: off };
    InitialYield;
    Yield { kind: reg };
    YieldRaw { kind: reg };
    Await;
    ResumeDispatch { kind: reg, on_return: off };
    Debugger; Nop;
}

/// Checks that need more than one operand.
fn verify_ranges(v: &mut Verifier, i: Instr) -> Result<()> {
    match i {
        Instr::CopyDataProperties {
            excluded, count, ..
        } => v.range(excluded, count),
        Instr::Call { args, argc, .. }
        | Instr::CallEval { args, argc, .. }
        | Instr::Construct { args, argc, .. }
        | Instr::SuperCall { args, argc, .. } => v.range(args, argc),
        _ => Ok(()),
    }
}

// ---- code blocks --------------------------------------------------------------------------------

fn put_constant(w: &mut Writer, c: &Constant) {
    match c {
        Constant::Number(n) => {
            w.u8(0);
            w.out.extend_from_slice(&n.to_bits().to_le_bytes());
        }
        Constant::String(s) => {
            w.u8(1);
            w.units(s);
        }
        Constant::BigInt(s) => {
            w.u8(2);
            w.str(s);
        }
        Constant::Template(t) => {
            w.u8(3);
            w.len(t.cooked.len());
            for c in &t.cooked {
                w.opt_units(c);
            }
            w.len(t.raw.len());
            for s in &t.raw {
                w.units(s);
            }
        }
    }
}

fn get_constant(r: &mut Reader) -> Result<Constant> {
    Ok(match r.u8()? {
        0 => Constant::Number(f64::from_bits(u64::from_le_bytes(
            r.take(8)?.try_into().unwrap(),
        ))),
        1 => Constant::String(r.units()?),
        2 => {
            let s = r.str()?;
            if !is_bigint_literal(&s) {
                return invalid(format!("BigInt constant {s:?}"));
            }
            Constant::BigInt(s)
        }
        3 => {
            let n = r.count(1)?;
            let cooked = collect_n(n, || r.opt_units())?;
            let n = r.count(4)?;
            let raw = collect_n(n, || r.units())?;
            Constant::Template(Rc::new(TemplateConstant { cooked, raw }))
        }
        t => return invalid(format!("constant tag {t}")),
    })
}

/// The digits a BigInt constant holds: decimal, or `0x`/`0o`/`0b` prefixed.
fn is_bigint_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let (radix, digits) = match b {
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        [b'0', b'o' | b'O', rest @ ..] => (8, rest),
        [b'0', b'b' | b'B', rest @ ..] => (2, rest),
        _ => (10, b),
    };
    !digits.is_empty() && digits.iter().all(|d| (*d as char).is_digit(radix))
}

fn put_module(w: &mut Writer, m: &ModuleInfo) {
    w.len(m.requested.len());
    for q in &m.requested {
        w.units(&q.specifier);
        w.len(q.attributes.len());
        for (k, v) in &q.attributes {
            w.units(k);
            w.units(v);
        }
    }
    w.len(m.imports.len());
    for i in &m.imports {
        w.len(i.request);
        w.opt_units(&i.import_name);
        w.str(&i.local_name);
    }
    for list in [&m.local_exports, &m.indirect_exports] {
        w.len(list.len());
        for e in list {
            w.opt_units(&e.export_name);
            match e.request {
                None => w.u8(0),
                Some(r) => {
                    w.u8(1);
                    w.len(r);
                }
            }
            w.opt_units(&e.import_name);
            match &e.local_name {
                None => w.u8(0),
                Some(s) => {
                    w.u8(1);
                    w.str(s);
                }
            }
        }
    }
    w.len(m.star_exports.len());
    for s in &m.star_exports {
        w.len(*s);
    }
    w.u8(m.has_tla as u8);
}

fn get_module(r: &mut Reader) -> Result<ModuleInfo> {
    let n = r.count(8)?;
    let mut requested = Vec::with_capacity(n);
    for _ in 0..n {
        let specifier = r.units()?;
        let k = r.count(8)?;
        let attributes = collect_n(k, || Ok((r.units()?, r.units()?)))?;
        requested.push(ModuleRequest {
            specifier,
            attributes,
        });
    }
    let in_requests = |i: usize| -> Result<usize> {
        if i < n {
            Ok(i)
        } else {
            invalid(format!("module request {i} out of range"))
        }
    };
    let k = r.count(9)?;
    let mut imports = Vec::with_capacity(k);
    for _ in 0..k {
        let request = in_requests(r.u32()? as usize)?;
        imports.push(ImportEntry {
            request,
            import_name: r.opt_units()?,
            local_name: r.str()?,
        });
    }
    let mut exports = [Vec::new(), Vec::new()];
    for list in exports.iter_mut() {
        let k = r.count(4)?;
        *list = collect_n(k, || {
            let export_name = r.opt_units()?;
            let request = if r.bool()? {
                Some(in_requests(r.u32()? as usize)?)
            } else {
                None
            };
            let import_name = r.opt_units()?;
            let local_name = if r.bool()? { Some(r.str()?) } else { None };
            Ok(ExportEntry {
                export_name,
                request,
                import_name,
                local_name,
            })
        })?;
    }
    let [local_exports, indirect_exports] = exports;
    let k = r.count(4)?;
    let star_exports = collect_n(k, || in_requests(r.u32()? as usize))?;
    Ok(ModuleInfo {
        requested,
        imports,
        local_exports,
        indirect_exports,
        star_exports,
        has_tla: r.bool()?,
    })
}

/// Block layout: the scalars, then every pool an instruction indexes
/// (constants, names, scopes), the declarations, source, span and module
/// record, the inline-cache and nested-function counts, then the
/// instructions and their offsets, the handlers, the debug section, and
/// last the nested blocks.
fn put_block(w: &mut Writer, c: &CodeBlock) {
    w.str(&c.name);
    c.kind.put(w);
    c.function_kind.put(w);
    w.u8(c.strict as u8);
    w.u8(c.is_derived_constructor as u8);
    w.u16(c.param_count);
    w.u16(c.register_count);
    w.u16(c.param_base);
    w.u16(c.param_regs);
    w.strs(&c.param_names);
    w.u8(c.keeps_arguments as u8);
    w.len(c.constants.len());
    for k in &c.constants {
        put_constant(w, k);
    }
    w.strs(&c.names);
    w.len(c.scopes.len());
    for s in &c.scopes {
        s.kind.put(w);
        w.len(s.bindings.len());
        for (n, k) in &s.bindings {
            w.str(n);
            k.put(w);
        }
        w.u8(s.is_var_scope as u8);
    }
    match &c.declarations {
        None => w.u8(0),
        Some(d) => {
            w.u8(1);
            w.strs(&d.var_names);
            w.len(d.functions.len());
            for (n, f) in &d.functions {
                w.str(n);
                w.u32(*f);
            }
            w.len(d.lexical.len());
            for (n, k) in &d.lexical {
                w.str(n);
                k.put(w);
            }
            w.strs(&d.annex_b_functions);
        }
    }
    w.source(&c.source);
    w.u32(c.span.start);
    w.u32(c.span.end);
    match &c.module {
        None => w.u8(0),
        Some(m) => {
            w.u8(1);
            put_module(w, m);
        }
    }
    w.len(c.ic_count as usize);
    w.len(c.functions.len());
    w.len(c.instrs.len());
    for i in &c.instrs {
        put_instr(w, *i);
    }
    w.len(c.offsets.len());
    for o in &c.offsets {
        w.u32(*o);
    }
    w.len(c.handlers.len());
    for h in &c.handlers {
        w.u32(h.start);
        w.u32(h.end);
        w.u32(h.target);
        w.u16(h.env_depth);
    }
    put_debug_section(w, &c.debug);
    for f in &c.functions {
        put_block(w, f);
    }
}

/// Debug info is most of a block's metadata by count, so it is packed:
/// statement starts as deltas from the previous one (the instruction index
/// as an unsigned delta, the source offset zigzag-encoded, since a
/// statement can start before the one emitted ahead of it) and locals as
/// (name index, register, start, length), all LEB128. The section is
/// prefixed by its byte length so a loader can skip it.
fn put_debug_section(w: &mut Writer, d: &DebugSection) {
    let at = w.out.len();
    w.u32(0);
    let d = d.get().expect("encoding a block whose debug section was refused");
    w.len(d.statements.len());
    let (mut pc, mut offset) = (0u32, 0u32);
    for s in &d.statements {
        w.var(s.pc.wrapping_sub(pc));
        let delta = s.offset.wrapping_sub(offset) as i32;
        w.var(((delta << 1) ^ (delta >> 31)) as u32);
        (pc, offset) = (s.pc, s.offset);
    }
    w.len(d.locals.len());
    for l in &d.locals {
        let name = w.str_index(&l.name);
        w.var(name);
        w.var(l.reg as u32);
        w.var(l.start);
        w.var(l.end.saturating_sub(l.start));
    }
    let len = u32::try_from(w.out.len() - at - 4).expect("debug section exceeds u32");
    w.out[at..at + 4].copy_from_slice(&len.to_le_bytes());
}

/// Decode and verify a block's packed debug section ([`DebugSection::get`]
/// calls this on first use). Statement starts must be ascending, inside
/// the block and inside the source text; locals must name a register the
/// block has and an instruction range inside it.
pub(crate) fn decode_debug_info(p: &PackedDebugInfo) -> Result<DebugInfo> {
    let mut r = Reader {
        bytes: &p.bytes,
        pos: 0,
        strs: p.strs.clone(),
        units: Vec::new(),
        sources: Vec::new(),
    };
    let len = p.instr_count as usize;
    let n = r.count(2)?;
    let mut statements: Vec<StatementStart> = Vec::with_capacity(n);
    let (mut pc, mut offset) = (0u32, 0u32);
    for i in 0..n {
        pc = pc.wrapping_add(r.var()?);
        let z = r.var()?;
        let delta = ((z >> 1) as i32) ^ -((z & 1) as i32);
        offset = offset.wrapping_add(delta as u32);
        if pc as usize >= len
            || offset > p.text_len
            || (i > 0 && statements[i - 1].pc >= pc)
        {
            return invalid("statement starts out of order or outside the block");
        }
        statements.push(StatementStart { pc, offset });
    }
    let n = r.count(4)?;
    let locals = collect_n(n, || {
        let name = r.var()? as usize;
        let name = r.str_at(name)?;
        let reg = u16::try_from(r.var()?)
            .map_err(|_| DecodeError::Invalid("local register exceeds 16 bits".into()))?;
        let start = r.var()?;
        let end = start
            .checked_add(r.var()?)
            .ok_or_else(|| DecodeError::Invalid("local range overflows".into()))?;
        if reg >= p.register_count || end as usize > len {
            return invalid(format!("local {name:?} outside the block"));
        }
        Ok(LocalVar { name, reg, start, end })
    })?;
    if r.pos != p.bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(DebugInfo { statements, locals })
}

fn get_block(r: &mut Reader, depth: usize) -> Result<Rc<CodeBlock>> {
    if depth > MAX_NESTING {
        return invalid(format!("functions nested deeper than {MAX_NESTING}"));
    }
    let name = r.str()?;
    let kind = CodeKind::get(r)?;
    let function_kind = FunctionKind::get(r)?;
    let strict = r.bool()?;
    let is_derived_constructor = r.bool()?;
    let param_count = r.u16()?;
    let register_count = r.u16()?;
    let param_base = r.u16()?;
    let param_regs = r.u16()?;
    if param_base as u32 + param_regs as u32 > register_count as u32 {
        return invalid("parameter registers beyond the register count");
    }
    let param_names = r.strs()?;
    let keeps_arguments = r.bool()?;
    let n = r.count(1)?;
    let constants = collect_n(n, || get_constant(r))?;
    let names = r.strs()?;
    let n = r.count(6)?;
    let scopes = collect_n(n, || {
        let kind = ScopeKind::get(r)?;
        let k = r.count(5)?;
        let bindings = collect_n(k, || Ok((r.str()?, BindingKind::get(r)?)))?;
        Ok(ScopeInfo {
            kind,
            bindings,
            is_var_scope: r.bool()?,
        })
    })?;
    let declarations = if r.bool()? {
        let var_names = r.strs()?;
        let k = r.count(8)?;
        let functions = collect_n(k, || Ok((r.str()?, r.u32()?)))?;
        let k = r.count(5)?;
        let lexical = collect_n(k, || Ok((r.str()?, BindingKind::get(r)?)))?;
        Some(Declarations {
            var_names,
            functions,
            lexical,
            annex_b_functions: r.strs()?,
        })
    } else {
        None
    };
    let source = r.source()?;
    let text = &*source.text;
    let span = Span {
        start: r.u32()?,
        end: r.u32()?,
    };
    let on_boundary = |o: u32| text.is_char_boundary(o as usize);
    if span.start > span.end || !on_boundary(span.start) || !on_boundary(span.end) {
        return invalid(format!("span {span:?} outside the source text"));
    }
    let module = if r.bool()? {
        Some(Rc::new(get_module(r)?))
    } else {
        None
    };
    let ic_len = r.count(1)?;
    // A nested block is far larger than this; the bound only guards the
    // allocation.
    let function_count = r.count(16)?;
    if let Some(d) = &declarations {
        if let Some((_, f)) = d
            .functions
            .iter()
            .find(|(_, f)| *f as usize >= function_count)
        {
            return invalid(format!("declared function {f} out of range"));
        }
    }
    let len = r.count(1)?;
    if len == 0 {
        return invalid("empty code block");
    }
    if ic_len > len || ic_len >= NO_IC as usize {
        return invalid(format!("{ic_len} inline caches for {len} instructions"));
    }
    let mut v = Verifier {
        register_count,
        constants: &constants,
        names: names.len(),
        functions: function_count,
        scopes: scopes.len(),
        ic: ic_len,
        len,
        pc: 0,
    };
    let instrs = collect_n(len, || {
        let i = get_instr(r)?;
        verify_operands(&mut v, i)?;
        verify_ranges(&mut v, i)?;
        v.pc += 1;
        Ok(i)
    })?;
    if !matches!(
        instrs[len - 1],
        Instr::Return | Instr::Throw | Instr::Jump(_) | Instr::ThrowError { .. }
    ) {
        return invalid("code block can run past its last instruction");
    }
    let n = r.count(4)?;
    if n != len {
        return invalid(format!("{n} offsets for {len} instructions"));
    }
    let offsets = collect_n(n, || {
        let o = r.u32()?;
        if o as usize > text.len() {
            return invalid(format!("source offset {o} outside the source text"));
        }
        Ok(o)
    })?;
    let n = r.count(14)?;
    let handlers = collect_n(n, || {
        let h = Handler {
            start: r.u32()?,
            end: r.u32()?,
            target: r.u32()?,
            env_depth: r.u16()?,
        };
        if h.start > h.end || h.end as usize > len || h.target as usize >= len {
            return invalid(format!("exception handler {h:?} leaves the block"));
        }
        Ok(h)
    })?;
    let debug_len = r.count(1)?;
    let debug = DebugSection::packed(PackedDebugInfo {
        bytes: r.take(debug_len)?.into(),
        strs: r.strs.clone(),
        instr_count: len as u32,
        register_count,
        text_len: text.len() as u32,
    });
    let functions = collect_n(function_count, || get_block(r, depth + 1))?;
    Ok(Rc::new(CodeBlock {
        name,
        kind,
        function_kind,
        strict,
        is_derived_constructor,
        param_count,
        register_count,
        param_base,
        param_regs,
        param_names,
        keeps_arguments,
        instrs,
        offsets,
        constants,
        names,
        functions,
        scopes,
        handlers,
        declarations,
        source,
        span,
        module,
        ic_count: ic_len as u16,
        feedback: std::cell::OnceCell::new(),
        globals: std::cell::OnceCell::new(),
        debug,
    }))
}

// ---- entry points -------------------------------------------------------------------------------

/// Serialise a compiled code tree.
pub fn encode(code: &CodeBlock) -> Vec<u8> {
    let mut w = Writer::default();
    put_block(&mut w, code);
    let body = std::mem::take(&mut w.out);
    // Sources name strings, so they join the string table before it is
    // written.
    let sources = std::mem::take(&mut w.sources);
    let mut source_refs = Vec::with_capacity(sources.len());
    for s in &sources {
        let at = w.out.len();
        w.str(&s.name);
        w.str(&s.text);
        w.len(s.lone_surrogates.len());
        for l in s.lone_surrogates.iter() {
            w.u32(l.offset);
            w.u16(l.unit);
        }
        source_refs.extend_from_slice(&w.out[at..]);
    }
    w.out.clear();
    let mut out = Vec::with_capacity(body.len() + 64);
    out.extend_from_slice(&(w.str_list.len() as u32).to_le_bytes());
    for s in &w.str_list {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out.extend_from_slice(&(w.unit_list.len() as u32).to_le_bytes());
    for s in &w.unit_list {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        for u in s.iter() {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out.extend_from_slice(&(sources.len() as u32).to_le_bytes());
    out.extend_from_slice(&source_refs);
    out.extend_from_slice(&body);
    out
}

/// Deserialise and verify a code tree produced by [`encode`].
pub fn decode(bytes: &[u8]) -> Result<Rc<CodeBlock>> {
    let mut r = Reader {
        bytes,
        pos: 0,
        strs: Rc::from(Vec::new()),
        units: Vec::new(),
        sources: Vec::new(),
    };
    let n = r.count(4)?;
    let strs = collect_n(n, || {
        let len = r.u32()? as usize;
        std::str::from_utf8(r.take(len)?)
            .map(Rc::from)
            .map_err(|_| DecodeError::Invalid("string table entry is not UTF-8".into()))
    })?;
    r.strs = Rc::from(strs);
    let n = r.count(4)?;
    r.units = collect_n(n, || {
        let len = r.u32()? as usize;
        let raw = r.take(len.checked_mul(2).ok_or(DecodeError::Truncated)?)?;
        Ok(raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect::<Rc<[u16]>>())
    })?;
    let n = r.count(8)?;
    r.sources = collect_n(n, || {
        let name = r.str()?;
        let text = r.str()?;
        let n = r.count(6)?;
        let mut lone = Vec::with_capacity(n);
        for _ in 0..n {
            let offset = r.u32()?;
            let unit = r.u16()?;
            let at = offset as usize;
            // In order, a surrogate, and standing on a U+FFFD of the text.
            let in_order = lone
                .last()
                .is_none_or(|l: &LoneSurrogate| l.offset < offset);
            let on_placeholder = text.get(at..at.saturating_add(3)) == Some("\u{FFFD}");
            if !in_order || !(0xD800..0xE000).contains(&unit) || !on_placeholder {
                return invalid(format!(
                    "lone surrogate {unit:#x} at {offset} does not fit the source text"
                ));
            }
            lone.push(LoneSurrogate { offset, unit });
        }
        Ok(Rc::new(SourceText {
            name,
            text,
            lone_surrogates: Rc::from(lone),
        }))
    })?;
    let code = get_block(&mut r, 0)?;
    if r.pos != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(code)
}
