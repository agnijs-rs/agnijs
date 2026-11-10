//! The bytecode format.
//!
//! A register machine with an accumulator (`acc`). Registers are frame
//! slots numbered from 0; the compiler fixes their count per function.
//! Most instructions read registers and write `acc`, or read `acc` and
//! write a register or a property, which keeps instructions short.
//!
//! Variable storage is decided at compile time: bindings that no inner
//! function captures live in registers; captured bindings live in heap
//! environments reached through `(depth, slot)` pairs; globals and names
//! under `with` or direct `eval` are looked up dynamically by name.

use crate::serialize::DecodeError;
use agnijs_js_parser::ast::FunctionKind;
use agnijs_js_parser::source::LoneSurrogate;
use agnijs_js_parser::span::Span;
#[cfg(feature = "tier")]
use std::any::Any;
use std::cell::{Cell, OnceCell};
use std::fmt;
use std::rc::Rc;

/// A register index in the current frame.
pub type Reg = u16;
/// An index into [`CodeBlock::constants`].
pub type ConstIdx = u32;
/// An index into [`CodeBlock::names`].
pub type NameIdx = u32;
/// An index into [`CodeBlock::functions`].
pub type FuncIdx = u32;
/// An index into [`CodeBlock::scopes`].
pub type ScopeIdx = u32;
/// A relative jump offset in instructions, from the jump's own index.
pub type Offset = i32;

/// How a generator was resumed (the value of the `kind` register after a
/// `Yield`).
pub const RESUME_NEXT: i32 = 0;
pub const RESUME_THROW: i32 = 1;
pub const RESUME_RETURN: i32 = 2;

/// Error kinds for [`Instr::ThrowError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Type,
    Reference,
    Syntax,
    Range,
}

/// Name prefixes for `SetFunctionName`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamePrefix {
    None,
    Get,
    Set,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateKind {
    Method,
    Getter,
    Setter,
}

/// A binary operator, for the instructions that name their operands
/// (`BinRR` and friends, TASK-100.18) rather than encode the operator in
/// the opcode. The bytecode owns this list: it is not the parser's
/// `BinaryOp`, so the wire encoding does not follow the AST.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
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
    In,
}

impl BinOp {
    /// The operators the fused compare-and-branch instructions accept.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::StrictEq
                | BinOp::StrictNe
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Instr {
    // ---- loads and moves -------------------------------------------------
    LdaUndefined,
    LdaNull,
    LdaTrue,
    LdaFalse,
    /// The hole / uninitialised marker.
    LdaEmpty,
    LdaInt(i32),
    LdaConst(ConstIdx),
    /// acc = reg
    Ldar(Reg),
    /// reg = acc
    Star(Reg),
    /// dst = src
    Mov {
        dst: Reg,
        src: Reg,
    },

    // ---- environments ------------------------------------------------------
    /// Push a new declarative environment described by the scope.
    PushEnv(ScopeIdx),
    PopEnv,
    /// Replace the innermost environment with a copy (per-iteration
    /// bindings of `for (let ...)`).
    CopyEnv,
    /// Push an object environment for `with` (object in acc).
    PushWith,
    /// acc = env[depth][slot]
    LdaEnv {
        depth: u16,
        slot: u16,
    },
    /// env[depth][slot] = acc
    StaEnv {
        depth: u16,
        slot: u16,
    },
    /// Throw a ReferenceError if acc is the hole; `name` is the binding.
    CheckTdz(NameIdx),
    /// Throw a TypeError for assignment to the constant `name`.
    ThrowConstAssign(NameIdx),
    /// acc = global `name` (lexical globals first, then the global object);
    /// ReferenceError when absent. The pseudo name `*this*` is the global
    /// `this`.
    LdaGlobal(NameIdx),
    /// Like `LdaGlobal` but undefined when absent (for `typeof`).
    LdaGlobalOrUndefined(NameIdx),
    /// global `name` = acc; strict mode throws when absent.
    StaGlobal {
        name: NameIdx,
        strict: bool,
    },
    /// Initialise the global lexical binding `name` with acc.
    InitGlobalLexical(NameIdx),
    /// Dynamic lookups by name through the runtime environment chain
    /// (`with`, direct eval, and anything inside them).
    LdaDynamic(NameIdx),
    LdaDynamicOrUndefined(NameIdx),
    StaDynamic {
        name: NameIdx,
        strict: bool,
    },
    /// Annex B.3.3.3 in sloppy eval code: store acc into the variable
    /// environment of the eval (the caller's var scope or the global),
    /// bypassing the block scopes in between.
    StaEvalVar(NameIdx),
    /// Initialise a binding found by name (eval-declared lexicals).
    InitDynamic(NameIdx),
    /// `delete name`: acc = result.
    DeleteDynamic(NameIdx),
    /// Resolve `name` through the environment chain now and keep the
    /// reference in `dst` (assignment under `with`/eval evaluates the
    /// reference before the right-hand side).
    /// acc = the `this` value for calling a binding found through the
    /// reference in `reg` (the `with` object, else undefined).
    LdaRefThis(Reg),
    ResolveDynamic {
        name: NameIdx,
        dst: Reg,
    },
    /// acc = GetValue of a reference made by `ResolveDynamic`.
    LdaDynamicRef {
        name: NameIdx,
        reference: Reg,
    },
    /// PutValue(acc) through a reference made by `ResolveDynamic`.
    StaDynamicRef {
        name: NameIdx,
        reference: Reg,
        strict: bool,
    },
    /// For calls `f()` where `f` is resolved dynamically: acc = the `this`
    /// value (`with` object or undefined).
    ThisForDynamicCall(NameIdx),
    /// Run GlobalDeclarationInstantiation / EvalDeclarationInstantiation
    /// for this code block's `declarations`.
    DeclareGlobals,
    /// acc = the imported binding `imports[idx]` (module code).
    LdaImport(u32),

    // ---- property access -------------------------------------------------
    /// acc = obj.name. `ic` indexes the code block's inline caches.
    GetNamed {
        obj: Reg,
        name: NameIdx,
        ic: IcIdx,
    },
    /// obj.name = acc
    SetNamed {
        obj: Reg,
        name: NameIdx,
        strict: bool,
        ic: IcIdx,
    },
    /// acc = obj[key]
    GetKeyed {
        obj: Reg,
        key: Reg,
    },
    /// obj[key] = acc
    SetKeyed {
        obj: Reg,
        key: Reg,
        strict: bool,
    },
    /// acc = delete obj.name
    DeleteNamed {
        obj: Reg,
        name: NameIdx,
        strict: bool,
    },
    DeleteKeyed {
        obj: Reg,
        key: Reg,
        strict: bool,
    },
    /// `super.name` / `super[key]`: `this` is the receiver, `func` the
    /// function whose [[HomeObject]] is used.
    GetSuper {
        this: Reg,
        func: Reg,
        key: Reg,
    },
    SetSuper {
        this: Reg,
        func: Reg,
        key: Reg,
        strict: bool,
    },
    /// CreateDataPropertyOrThrow(obj, key, acc) (literals).
    DefineOwn {
        obj: Reg,
        key: Reg,
    },
    DefineOwnNamed {
        obj: Reg,
        name: NameIdx,
    },
    /// Define a method / accessor from acc on obj under key, setting the
    /// function's name from the key and its [[HomeObject]] to obj.
    DefineMethod {
        obj: Reg,
        key: Reg,
        enumerable: bool,
    },
    DefineGetter {
        obj: Reg,
        key: Reg,
        enumerable: bool,
    },
    DefineSetter {
        obj: Reg,
        key: Reg,
        enumerable: bool,
    },
    /// func.[[HomeObject]] = home
    SetHomeObject {
        func: Reg,
        home: Reg,
    },
    /// Set acc's `name` from key (with prefix).
    SetFunctionName {
        key: Reg,
        prefix: NamePrefix,
    },
    /// `__proto__: v` in a literal: obj.[[Prototype]] = acc if object/null.
    SetPrototypeLiteral {
        obj: Reg,
    },
    /// CopyDataProperties(target, acc, excluded keys in `count` registers
    /// from `excluded`).
    CopyDataProperties {
        target: Reg,
        excluded: Reg,
        count: u16,
    },

    // ---- private names ---------------------------------------------------
    /// acc = a fresh private name with description `#name`.
    NewPrivateName(NameIdx),
    /// acc = obj.#name (name register holds the private name).
    GetPrivate {
        obj: Reg,
        name: Reg,
    },
    SetPrivate {
        obj: Reg,
        name: Reg,
    },
    /// acc = #name in obj
    HasPrivate {
        obj: Reg,
        name: Reg,
    },
    /// Attach acc (a function) to the private name as its method or
    /// accessor half.
    SetPrivateMethod {
        name: Reg,
        kind: PrivateKind,
    },
    /// Install the private method/accessor behind `name` on obj (static
    /// private methods).
    AddPrivateBrand {
        obj: Reg,
        name: Reg,
    },

    // ---- operators (acc = reg OP acc) ------------------------------------
    Add(Reg),
    Sub(Reg),
    Mul(Reg),
    Div(Reg),
    Mod(Reg),
    Exp(Reg),
    Shl(Reg),
    Shr(Reg),
    UShr(Reg),
    BitAnd(Reg),
    BitOr(Reg),
    BitXor(Reg),
    Eq(Reg),
    Ne(Reg),
    StrictEq(Reg),
    StrictNe(Reg),
    Lt(Reg),
    Le(Reg),
    Gt(Reg),
    Ge(Reg),
    InstanceOf(Reg),
    /// acc = reg in acc
    In(Reg),
    // ---- operators with named operands (TASK-100.18) ----------------------
    // The left operand is always the first named; an integer immediate
    // only ever stands on the right, so `7 + x` keeps its string semantics
    // and ToPrimitive order. Each reads its operands, performs every
    // observable effect (ToPrimitive, valueOf) and then writes acc once
    // (ADR 0019 J3).
    /// acc = a OP b
    BinRR {
        op: BinOp,
        a: Reg,
        b: Reg,
    },
    /// acc = a OP imm
    BinRI {
        op: BinOp,
        a: Reg,
        imm: i32,
    },
    /// acc = acc OP b
    BinAR {
        op: BinOp,
        b: Reg,
    },
    /// acc = acc OP imm
    BinAI {
        op: BinOp,
        imm: i32,
    },
    // ---- unary (on acc) --------------------------------------------------
    Neg,
    Pos,
    Not,
    BitNot,
    Typeof,
    ToNumeric,
    Inc,
    Dec,
    ToString,
    ToPropertyKey,
    /// TypeError if acc is null or undefined (destructuring).
    RequireObjectCoercible,

    // ---- control flow -----------------------------------------------------
    Jump(Offset),
    JumpIfTrue(Offset),
    JumpIfFalse(Offset),
    JumpIfNullish(Offset),
    JumpIfNotNullish(Offset),
    JumpIfUndefined(Offset),
    JumpIfNotUndefined(Offset),
    JumpIfEmpty(Offset),
    JumpIfNotEmpty(Offset),
    JumpIfObject(Offset),
    /// Jump if the register holds a value strictly equal to acc.
    JumpIfStrictEq {
        reg: Reg,
        offset: Offset,
    },
    // ---- fused compare-and-branch (TASK-100.18) ---------------------------
    // `op` is a comparison (`BinOp::is_comparison`). The `Not` forms branch
    // when the comparison is false, which is not the opposite comparison:
    // `JumpIfNotCmp Lt` jumps on NaN where `JumpIfCmp Ge` would not. The
    // comparison's effects (ToPrimitive on both sides) all happen before
    // the single branch; acc is left as it was (J3).
    /// Jump if a OP b.
    JumpIfCmpRR {
        op: BinOp,
        a: Reg,
        b: Reg,
        offset: Offset,
    },
    /// Jump unless a OP b.
    JumpIfNotCmpRR {
        op: BinOp,
        a: Reg,
        b: Reg,
        offset: Offset,
    },
    /// Jump if a OP imm.
    JumpIfCmpRI {
        op: BinOp,
        a: Reg,
        imm: i32,
        offset: Offset,
    },
    /// Jump unless a OP imm.
    JumpIfNotCmpRI {
        op: BinOp,
        a: Reg,
        imm: i32,
        offset: Offset,
    },
    /// Jump if a OP acc.
    JumpIfCmpRA {
        op: BinOp,
        a: Reg,
        offset: Offset,
    },
    /// Jump unless a OP acc.
    JumpIfNotCmpRA {
        op: BinOp,
        a: Reg,
        offset: Offset,
    },
    /// Jump if acc OP imm.
    JumpIfCmpAI {
        op: BinOp,
        imm: i32,
        offset: Offset,
    },
    /// Jump unless acc OP imm.
    JumpIfNotCmpAI {
        op: BinOp,
        imm: i32,
        offset: Offset,
    },

    // ---- calls ------------------------------------------------------------
    /// acc = callee.call(this, args[0..argc])
    Call {
        callee: Reg,
        this: Reg,
        args: Reg,
        argc: u16,
    },
    /// acc = callee.apply(this, array in `args`)
    CallSpread {
        callee: Reg,
        this: Reg,
        args: Reg,
    },
    /// Possible direct eval: like `Call` but, when the callee is %eval%,
    /// evaluates the code in the current environment.
    CallEval {
        callee: Reg,
        this: Reg,
        args: Reg,
        argc: u16,
        strict: bool,
    },
    CallEvalSpread {
        callee: Reg,
        this: Reg,
        args: Reg,
        strict: bool,
    },
    /// acc = Construct(callee, args, callee)
    Construct {
        callee: Reg,
        args: Reg,
        argc: u16,
    },
    ConstructSpread {
        callee: Reg,
        args: Reg,
    },
    /// acc = super(...): `func` is the active function (its [[Prototype]]
    /// is the parent constructor).
    SuperCall {
        func: Reg,
        new_target: Reg,
        args: Reg,
        argc: u16,
    },
    /// Like `SuperCall` with the arguments in an array (not iterated).
    SuperCallSpread {
        func: Reg,
        new_target: Reg,
        args: Reg,
    },
    Return,
    /// In a derived constructor before `Return`: acc must be an object or
    /// undefined (then acc = this, which must be initialised).
    CheckDerivedReturn {
        this: Reg,
    },
    Throw,
    /// Throw a new error with a constant message.
    ThrowError {
        kind: ErrorKind,
        message: ConstIdx,
    },
    /// TypeError (constant message) when acc is not an object.
    ThrowIfNotObject(ConstIdx),

    // ---- literals and functions ------------------------------------------
    CreateObject,
    CreateArray,
    /// CreateDataProperty(arr, arr.length, acc)
    ArrayPush {
        arr: Reg,
    },
    /// Append a hole (length += 1).
    ArrayPushHole {
        arr: Reg,
    },
    /// Iterate acc and append every value to arr.
    ArraySpread {
        arr: Reg,
    },
    /// acc = closure over functions[idx] in the current environment.
    CreateClosure(FuncIdx),
    /// Create the class constructor F from functions[idx] with heritage in
    /// `heritage` (Empty when absent) and name in `name` (Empty: use the
    /// code block's name). acc = F; `proto` = F.prototype.
    CreateClass {
        func: FuncIdx,
        heritage: Reg,
        proto: Reg,
        name: Reg,
    },
    /// Record an instance field on F: key (a property key or private
    /// name), initialiser function (or undefined), and whether an
    /// anonymous function value takes the key as its name.
    AddField {
        ctor: Reg,
        key: Reg,
        init: Reg,
        anon: bool,
    },
    /// Define a (static) field with value acc on obj.
    DefineField {
        obj: Reg,
        key: Reg,
        anon: bool,
    },
    /// Record a private method/accessor name on F for instance brands.
    AddInstancePrivateMethod {
        ctor: Reg,
        name: Reg,
    },
    /// Run F's private method brands and field initialisers on `this`.
    InitializeInstanceElements {
        func: Reg,
        this: Reg,
    },
    /// acc = a RegExp from constants (pattern, flags).
    CreateRegExp {
        pattern: ConstIdx,
        flags: ConstIdx,
    },
    /// acc = the (cached per realm and site) template object.
    GetTemplateObject(ConstIdx),
    /// acc = the arguments object.
    CreateArguments {
        mapped: bool,
    },
    /// acc = array of the actual arguments from index `from`.
    CreateRestArray {
        from: u16,
    },
    /// acc = the current frame's callee.
    LdaCallee,
    /// acc = the current frame's new.target (or undefined).
    LdaNewTarget,
    /// acc = the current frame's `this`.
    LdaThis,
    /// acc = import.meta
    ImportMeta,
    /// acc = import(spec, options)
    DynamicImport {
        spec: Reg,
        options: Reg,
    },

    // ---- iteration ----------------------------------------------------------
    /// iter = GetIterator(acc), next = iter.next
    GetIterator {
        iter: Reg,
        next: Reg,
    },
    GetAsyncIterator {
        iter: Reg,
        next: Reg,
    },
    /// acc = next.call(iter); on done jump, else acc = value.
    IteratorStep {
        iter: Reg,
        next: Reg,
        done: Offset,
    },
    /// acc = next.call(iter) (async for-of awaits it before `IteratorResult`).
    IteratorNextRaw {
        iter: Reg,
        next: Reg,
    },
    /// acc must be an object; if acc.done jump, else acc = acc.value.
    IteratorResult {
        done: Offset,
    },
    /// Normal-completion close: call return if present, check the result.
    /// Preserves acc.
    IteratorClose {
        iter: Reg,
    },
    /// Throw-completion close: call return, swallow its errors. Preserves acc.
    IteratorCloseSwallow {
        iter: Reg,
    },
    /// acc = iter.return (undefined when absent), for async closes.
    GetIteratorReturn {
        iter: Reg,
    },
    /// for-in: dst = enumerator over acc.
    ForInPrepare {
        dst: Reg,
    },
    /// acc = next enumerable key, or jump when exhausted.
    ForInNext {
        iter: Reg,
        done: Offset,
    },

    // ---- generators and async ---------------------------------------------
    /// Suspend after declaration instantiation; the call returns the
    /// generator object.
    InitialYield,
    /// Yield acc (wrapped in an iterator result). On resume acc is the
    /// sent value and `kind` the resume mode.
    Yield {
        kind: Reg,
    },
    /// Yield acc as the iterator result object itself (`yield*`).
    YieldRaw {
        kind: Reg,
    },
    /// Await acc. On resume acc is the settled value; a rejection is
    /// thrown at this instruction.
    Await,
    /// After a yield: THROW resumptions throw acc here, RETURN ones jump.
    ResumeDispatch {
        kind: Reg,
        on_return: Offset,
    },

    Debugger,
    Nop,
}

/// A constant pool entry.
#[derive(Clone, Debug)]
pub enum Constant {
    Number(f64),
    String(Rc<[u16]>),
    BigInt(Rc<str>),
    Template(Rc<TemplateConstant>),
}

#[derive(Debug)]
pub struct TemplateConstant {
    pub cooked: Vec<Option<Rc<[u16]>>>,
    pub raw: Vec<Rc<[u16]>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Var,
    Let,
    Const,
    /// A class's inner name binding (immutable).
    Class,
    Function,
    Param,
    CatchParam,
    /// The hidden `this` binding of a function (captured by arrows).
    This,
    /// Internal bindings (`new.target`, the callee, private names).
    Internal,
    /// The name of a named function expression, bound in its own scope:
    /// assignment is ignored in sloppy mode and a TypeError in strict mode.
    FuncName,
}

impl BindingKind {
    pub fn is_lexical(self) -> bool {
        matches!(
            self,
            BindingKind::Let | BindingKind::Const | BindingKind::Class
        )
    }
    pub fn is_const(self) -> bool {
        matches!(self, BindingKind::Const | BindingKind::Class)
    }
    /// Slots of this kind start as the hole (TDZ) rather than undefined.
    pub fn starts_empty(self) -> bool {
        matches!(
            self,
            BindingKind::Let
                | BindingKind::Const
                | BindingKind::Class
                | BindingKind::This
                | BindingKind::Param
                | BindingKind::Internal
        )
    }
}

/// Describes a heap environment: the names of its slots, for dynamic
/// lookup, `eval`, and the debugger.
#[derive(Clone, Debug)]
pub struct ScopeInfo {
    pub kind: ScopeKind,
    /// Slot i holds the binding `bindings[i]`.
    pub bindings: Vec<(Rc<str>, BindingKind)>,
    /// Function-level scopes: where sloppy eval `var`s land.
    pub is_var_scope: bool,
}

impl ScopeInfo {
    pub fn slot_of(&self, name: &str) -> Option<usize> {
        self.bindings.iter().position(|(n, _)| &**n == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Function,
    Block,
    Catch,
    Class,
    Module,
    /// The declarative environment of eval code.
    Eval,
}

/// An exception handler covering `[start, end)`.
#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    /// The environment depth to restore before jumping to the target.
    pub env_depth: u16,
}

/// Declarations of a script, module or eval body, for the declaration
/// instantiation algorithms.
#[derive(Clone, Debug, Default)]
pub struct Declarations {
    pub var_names: Vec<Rc<str>>,
    /// Function declarations in order (the last with a given name wins).
    pub functions: Vec<(Rc<str>, FuncIdx)>,
    pub lexical: Vec<(Rc<str>, BindingKind)>,
    /// Annex B.3.3 function-in-block names hoisted as vars.
    pub annex_b_functions: Vec<Rc<str>>,
}

/// What kind of code a block holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeKind {
    Script,
    Module,
    Eval,
    Function,
    Arrow,
    Method,
    /// A class constructor (base or derived).
    ClassConstructor,
    /// Synthetic class field initialiser or static block.
    ClassInitializer,
}

/// The source text a code block was compiled from, shared by every
/// function in it.
#[derive(Debug)]
pub struct SourceText {
    pub name: Rc<str>,
    pub text: Rc<str>,
    /// The lone surrogates of source that came from a JavaScript string
    /// (eval, `Function`), standing as U+FFFD in `text`
    /// ([`agnijs_js_parser::source`]). Empty for source from a Rust `str`.
    pub lone_surrogates: Rc<[LoneSurrogate]>,
}

impl SourceText {
    /// The code units of `text[start..end]` (byte offsets, clamped to the
    /// text and to character boundaries), lone surrogates included.
    pub fn units(&self, start: usize, end: usize) -> Vec<u16> {
        let text = &*self.text;
        let mut start = start.min(text.len());
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = end.clamp(start, text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        agnijs_js_parser::source::units_of(text, start, end, &self.lone_surrogates)
    }
}

/// Index of an inline cache in `CodeBlock::ic`; `NO_IC` when the function
/// ran out of slots.
pub type IcIdx = u16;
pub const NO_IC: IcIdx = u16::MAX;

/// One recorded case of a named property access site (ADR 0012, ADR 0019
/// J4): plain data at fixed offsets, so a compiled tier can test it
/// inline. The VM owns the meaning of the ids; the compiler only sizes the
/// table.
///
/// - `kind`: what was found (the `KIND_*` constants).
/// - `shape`: the receiver shape the case applies to (object receivers).
/// - `proto`: the receiver's prototype at fill time (prototype kinds), or
///   the realm's `String.prototype` (string kinds).
/// - `holder`: the object holding the property (prototype kinds).
/// - `slot`: the property's slot in the holder (or receiver).
/// - `epoch`: the VM's prototype epoch at fill time (prototype kinds).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcEntry {
    pub kind: u8,
    /// `FLAG_*` bits.
    pub flags: u8,
    pub _reserved: [u8; 2],
    pub shape: u32,
    pub proto: u32,
    pub holder: u32,
    pub slot: u32,
    pub epoch: u32,
}

impl IcEntry {
    /// No case recorded.
    pub const KIND_EMPTY: u8 = 0;
    /// An own data property of the receiver.
    pub const KIND_OWN_DATA: u8 = 1;
    /// A data property of an object on the receiver's prototype chain.
    pub const KIND_PROTO_DATA: u8 = 2;
    /// An own accessor of the receiver.
    pub const KIND_OWN_ACCESSOR: u8 = 3;
    /// An accessor on the receiver's prototype chain.
    pub const KIND_PROTO_ACCESSOR: u8 = 4;
    /// `length` of an array receiver.
    pub const KIND_ARRAY_LENGTH: u8 = 5;
    /// `length` of a string primitive.
    pub const KIND_STRING_LENGTH: u8 = 6;
    /// A data property found from a string primitive through
    /// `String.prototype` (a method, usually).
    pub const KIND_STRING_PROTO_DATA: u8 = 7;
    /// A write that adds an own writable, enumerable, configurable data
    /// property: a receiver of `shape` whose prototype is `proto` (`u32::MAX`
    /// for none) moves to the shape in `holder`. `epoch` guards the
    /// prototype chain, none of which has the key.
    pub const KIND_OWN_ADD: u8 = 8;
    /// The site saw more receivers than it has entries; it stays generic.
    /// Only ever in entry 0.
    pub const KIND_MEGAMORPHIC: u8 = 0xFF;

    /// The key is `length`: the entry never applies to receivers whose
    /// `length` is exotic (arrays, String objects).
    pub const FLAG_LENGTH: u8 = 1;

    pub const EMPTY: IcEntry = IcEntry {
        kind: Self::KIND_EMPTY,
        flags: 0,
        _reserved: [0; 2],
        shape: u32::MAX,
        proto: u32::MAX,
        holder: u32::MAX,
        slot: u32::MAX,
        epoch: 0,
    };
}

/// Entries per inline-cache site: a site with more distinct receivers than
/// this goes megamorphic.
pub const IC_ENTRIES: usize = 4;

/// An inline-cache site: up to [`IC_ENTRIES`] cases, filled from the front.
/// `repr(C)` (with `Cell` transparent): a compiled tier reads the entries
/// in place, `size_of::<IcEntry>()` apart.
#[repr(C)]
#[derive(Debug)]
pub struct IcSite {
    pub entries: [Cell<IcEntry>; IC_ENTRIES],
}

/// The state of an [`IcSite`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcState {
    Empty,
    Monomorphic,
    Polymorphic,
    Megamorphic,
}

impl IcSite {
    pub fn new() -> IcSite {
        IcSite {
            entries: std::array::from_fn(|_| Cell::new(IcEntry::EMPTY)),
        }
    }

    pub fn state(&self) -> IcState {
        let first = self.entries[0].get().kind;
        if first == IcEntry::KIND_MEGAMORPHIC {
            return IcState::Megamorphic;
        }
        match self
            .entries
            .iter()
            .filter(|e| e.get().kind != IcEntry::KIND_EMPTY)
            .count()
        {
            0 => IcState::Empty,
            1 => IcState::Monomorphic,
            _ => IcState::Polymorphic,
        }
    }
}

impl Default for IcSite {
    fn default() -> Self {
        IcSite::new()
    }
}

/// A string constant as the VM last materialised it: the string's id and
/// the collection count it is valid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstString {
    pub id: u32,
    pub epoch: u64,
}

impl ConstString {
    pub const EMPTY: ConstString = ConstString {
        id: u32::MAX,
        epoch: u64::MAX,
    };
}

/// A block's run-time feedback (ADR 0019 J4): inline caches, the property
/// keys of its names and its materialised string constants. Allocated by
/// the first VM that runs the block, which owns it (`owner`); another VM
/// running the same block uses no feedback. Never serialised: a loaded
/// block starts with none.
#[derive(Debug)]
pub struct Feedback {
    pub owner: u32,
    /// One per GetNamed / SetNamed site, indexed by [`IcIdx`].
    pub ic: Box<[IcSite]>,
    /// The VM's encoding of each name's property key (0: not yet
    /// computed), indexed by [`NameIdx`].
    pub name_keys: Box<[Cell<u64>]>,
    /// String constants, indexed by constant index.
    pub strings: Box<[Cell<ConstString>]>,
    /// The string value of the block's `name` (a function's initial
    /// `name` property), like a string constant.
    pub name: Cell<ConstString>,
    /// The owning VM's shape epoch the `ic` entries are valid in: a VM
    /// that frees shapes (and may reuse their ids) moves its epoch, and
    /// clears entries recorded in an earlier one before using them.
    pub shape_epoch: Cell<u64>,
    /// The owning VM's collection that last traced this feedback (its
    /// collector's bookkeeping).
    pub gc_mark: Cell<u64>,
    /// The block's tiering state (ADR 0019, TASK-101.3). Only with the
    /// `tier` feature, so that builds without a JIT keep the table as it was.
    #[cfg(feature = "tier")]
    pub tier: Tier,
}

/// Tiering state of one block for its owning VM: how hot the block is and
/// its compiled code, if any. The VM owns the meaning; the compiled code
/// is opaque here (the JIT is an optional crate the compiler does not
/// depend on).
#[cfg(feature = "tier")]
#[derive(Default)]
pub struct Tier {
    /// Function entries and loop back-edges counted so far.
    pub hotness: Cell<u32>,
    /// [`Tier::COLD`], [`Tier::COMPILED`] or [`Tier::NEVER`].
    pub state: Cell<u8>,
    pub code: OnceCell<Rc<dyn Any>>,
}

#[cfg(feature = "tier")]
impl Tier {
    pub const COLD: u8 = 0;
    pub const COMPILED: u8 = 1;
    /// Not eligible, or compilation failed: stays in the interpreter.
    pub const NEVER: u8 = 2;
}

#[cfg(feature = "tier")]
impl fmt::Debug for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tier")
            .field("hotness", &self.hotness.get())
            .field("state", &self.state.get())
            .finish()
    }
}

/// A cached global binding for one name of a block ([`CodeBlock::globals`],
/// indexed by [`NameIdx`]): where the VM last resolved the name. The VM
/// owns the meaning of the fields; the compiler only sizes the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalEntry {
    /// `NONE`, `LEXICAL` or `PROPERTY`.
    pub kind: u32,
    /// The realm the entry was recorded in.
    pub realm: u32,
    /// Position of the binding in that realm's table of global lexicals,
    /// or of the property in the global object's dictionary.
    pub index: u32,
    /// The realm's global epoch when a `PROPERTY` entry was recorded.
    pub epoch: u32,
}

impl GlobalEntry {
    pub const NONE: u32 = 0;
    pub const LEXICAL: u32 = 1;
    pub const PROPERTY: u32 = 2;
    pub const EMPTY: GlobalEntry = GlobalEntry {
        kind: Self::NONE,
        realm: u32::MAX,
        index: u32::MAX,
        epoch: 0,
    };
}

/// A compiled function (or script, module, eval body).
#[derive(Debug)]
pub struct CodeBlock {
    pub name: Rc<str>,
    pub kind: CodeKind,
    pub function_kind: FunctionKind,
    pub strict: bool,
    pub is_derived_constructor: bool,
    /// `function.length`.
    pub param_count: u16,
    pub register_count: u16,
    /// Parameter i is passed in register `param_base + i`.
    pub param_base: Reg,
    /// Number of positional parameter registers (one per formal parameter,
    /// including patterns and a rest parameter).
    pub param_regs: u16,
    /// Bound names of the parameters, in order (mapped arguments).
    pub param_names: Vec<Rc<str>>,
    /// The frame must keep the actual arguments (rest / `arguments`).
    pub keeps_arguments: bool,
    pub instrs: Vec<Instr>,
    /// Source offset of each instruction (parallel to `instrs`).
    pub offsets: Vec<u32>,
    pub constants: Vec<Constant>,
    pub names: Vec<Rc<str>>,
    pub functions: Vec<Rc<CodeBlock>>,
    pub scopes: Vec<ScopeInfo>,
    pub handlers: Vec<Handler>,
    pub declarations: Option<Declarations>,
    pub source: Rc<SourceText>,
    pub span: Span,
    /// Module metadata (module code only).
    pub module: Option<Rc<ModuleInfo>>,
    /// Number of inline-cache sites (GetNamed / SetNamed).
    pub ic_count: u16,
    /// Run-time feedback, allocated on first use (see [`Self::feedback`]).
    pub feedback: OnceCell<Feedback>,
    /// Global-binding cache, one entry per name; filled by the VM and
    /// allocated on the block's first global access (see [`Self::globals`]).
    pub globals: OnceCell<Box<[Cell<GlobalEntry>]>>,
    /// Statement starts and register-resident bindings, for the debugger.
    /// A loaded block decodes them on first use.
    pub debug: DebugSection,
}

impl CodeBlock {
    /// The global-binding cache, indexed by [`NameIdx`].
    #[inline]
    pub fn globals(&self) -> &[Cell<GlobalEntry>] {
        self.globals.get_or_init(|| {
            self.names
                .iter()
                .map(|_| Cell::new(GlobalEntry::EMPTY))
                .collect()
        })
    }

    /// The block's feedback when it belongs to VM `owner`, allocating it
    /// for `owner` if no VM has run the block yet.
    #[inline]
    pub fn feedback(&self, owner: u32) -> Option<&Feedback> {
        let f = self.feedback.get_or_init(|| Feedback {
            owner,
            ic: (0..self.ic_count).map(|_| IcSite::new()).collect(),
            name_keys: self.names.iter().map(|_| Cell::new(0)).collect(),
            strings: self
                .constants
                .iter()
                .map(|_| Cell::new(ConstString::EMPTY))
                .collect(),
            name: Cell::new(ConstString::EMPTY),
            shape_epoch: Cell::new(0),
            gc_mark: Cell::new(0),
            #[cfg(feature = "tier")]
            tier: Tier::default(),
        });
        if f.owner == owner {
            Some(f)
        } else {
            None
        }
    }

    /// The feedback if it exists and belongs to `owner` (never allocates).
    #[inline]
    pub fn feedback_if(&self, owner: u32) -> Option<&Feedback> {
        match self.feedback.get() {
            Some(f) if f.owner == owner => Some(f),
            _ => None,
        }
    }
}

/// A block's [`DebugInfo`]: the compiler's own, or the packed section of a
/// loaded payload, decoded and verified the first time a debugger asks
/// (stepping, locals, breakpoint resolution, pause positions). Nothing on
/// the run path reads it: stack traces use `offsets`. The verdict of the
/// decode is kept, so a malformed section costs one failed decode and then
/// reads as no debug info.
pub struct DebugSection {
    info: OnceCell<Result<DebugInfo, DecodeError>>,
    packed: Cell<Option<PackedDebugInfo>>,
}

/// The undecoded debug section of a loaded block, with what its verifier
/// needs about the block.
pub struct PackedDebugInfo {
    pub(crate) bytes: Box<[u8]>,
    pub(crate) strs: Rc<[Rc<str>]>,
    pub(crate) instr_count: u32,
    pub(crate) register_count: u16,
    pub(crate) text_len: u32,
}

impl DebugSection {
    /// A section still to be decoded.
    pub(crate) fn packed(p: PackedDebugInfo) -> DebugSection {
        DebugSection {
            info: OnceCell::new(),
            packed: Cell::new(Some(p)),
        }
    }

    /// The decoded info, or why the section was refused. Decodes on the
    /// first call; later calls return the same verdict.
    pub fn get(&self) -> Result<&DebugInfo, DecodeError> {
        self.info
            .get_or_init(|| match self.packed.take() {
                Some(p) => crate::serialize::decode_debug_info(&p),
                None => Ok(DebugInfo::default()),
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Whether `get` has run (or the info was the compiler's).
    pub fn is_decoded(&self) -> bool {
        self.info.get().is_some()
    }

    /// Statement starts, by ascending instruction index; empty when the
    /// section was refused.
    pub fn statements(&self) -> &[StatementStart] {
        self.get().map(|d| &d.statements[..]).unwrap_or(&[])
    }

    /// Register bindings in declaration order; empty when the section was
    /// refused.
    pub fn locals(&self) -> &[LocalVar] {
        self.get().map(|d| &d.locals[..]).unwrap_or(&[])
    }

    /// Whether instruction `pc` starts a statement.
    pub fn is_statement(&self, pc: usize) -> bool {
        self.get().map(|d| d.is_statement(pc)).unwrap_or(false)
    }

    /// The info, decoding it if needed, by value.
    pub fn into_info(self) -> Result<DebugInfo, DecodeError> {
        self.get()?;
        self.info.into_inner().expect("decoded above")
    }
}

impl From<DebugInfo> for DebugSection {
    fn from(info: DebugInfo) -> DebugSection {
        DebugSection {
            info: OnceCell::from(Ok(info)),
            packed: Cell::new(None),
        }
    }
}

impl Default for DebugSection {
    fn default() -> DebugSection {
        DebugInfo::default().into()
    }
}

/// Prints the decoded info (decoding it), so a loaded tree prints like the
/// compiled one.
impl fmt::Debug for DebugSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Ok(d) => d.fmt(f),
            Err(e) => f.debug_struct("DebugInfo").field("error", &e).finish(),
        }
    }
}

/// What a debugger needs beyond `offsets`: where statements start (the
/// places a breakpoint or a step can stop) and which registers hold which
/// named bindings over which instruction ranges. Captured bindings are
/// already named by [`ScopeInfo`].
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    /// Statement starts, by ascending and distinct instruction index.
    pub statements: Vec<StatementStart>,
    /// Register bindings, in declaration order.
    pub locals: Vec<LocalVar>,
}

/// A statement's first instruction and the source offset of the
/// statement (which can precede the instruction's own offset, e.g. the
// `const` of a declaration whose first instruction evaluates its
/// initialiser).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatementStart {
    pub pc: u32,
    pub offset: u32,
}

/// A binding kept in a register: `name` is in register `reg` while
/// `start <= pc < end`.
#[derive(Clone, Debug)]
pub struct LocalVar {
    pub name: Rc<str>,
    pub reg: Reg,
    pub start: u32,
    pub end: u32,
}

impl DebugInfo {
    /// Whether instruction `pc` starts a statement.
    pub fn is_statement(&self, pc: usize) -> bool {
        self.statements
            .binary_search_by_key(&(pc as u32), |s| s.pc)
            .is_ok()
    }
}

/// Static module structure (ES Module Record fields).
#[derive(Debug, Default)]
pub struct ModuleInfo {
    pub requested: Vec<ModuleRequest>,
    pub imports: Vec<ImportEntry>,
    pub local_exports: Vec<ExportEntry>,
    pub indirect_exports: Vec<ExportEntry>,
    /// Indices into `requested`.
    pub star_exports: Vec<usize>,
    pub has_tla: bool,
}

#[derive(Debug, Clone)]
pub struct ModuleRequest {
    pub specifier: Rc<[u16]>,
    pub attributes: Vec<(Rc<[u16]>, Rc<[u16]>)>,
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    /// Index into `requested`.
    pub request: usize,
    /// `None` for namespace imports.
    pub import_name: Option<Rc<[u16]>>,
    pub local_name: Rc<str>,
}

#[derive(Debug, Clone)]
pub struct ExportEntry {
    pub export_name: Option<Rc<[u16]>>,
    /// Index into `requested` for indirect exports.
    pub request: Option<usize>,
    /// `None` with a request: `export * as ns from`.
    pub import_name: Option<Rc<[u16]>>,
    pub local_name: Option<Rc<str>>,
}

impl CodeBlock {
    /// The source offset of instruction `pc`.
    pub fn offset_at(&self, pc: usize) -> u32 {
        self.offsets.get(pc).copied().unwrap_or(self.span.start)
    }

    /// The handler for a throw at `pc`, innermost first.
    pub fn handler_for(&self, pc: usize) -> Option<&Handler> {
        let pc = pc as u32;
        self.handlers.iter().find(|h| h.start <= pc && pc < h.end)
    }
}
