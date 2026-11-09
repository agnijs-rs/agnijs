//! AST → bytecode.
//!
//! One [`Compiler`] compiles a whole program. It keeps a stack of
//! [`FnState`]s (one per function being compiled, innermost last) and a
//! single lexical chain of [`ScopeFrame`]s spanning those functions, so a
//! reference in an inner function can be resolved against the enclosing
//! functions' scopes to a `(depth, slot)` environment access.

mod class;
mod expr;
mod func;
mod module;
mod pattern;
mod stmt;

use crate::analysis::{private_binding_name, Analyses, FnAnalysis};
use crate::bytecode::*;
use crate::CompileError;
use agnijs_js_parser::ast::*;
use agnijs_js_parser::span::Span;
use foldhash::{HashMap, HashMapExt};
use std::rc::Rc;

pub(crate) const THIS: &str = "*this*";
pub(crate) const NEW_TARGET: &str = "*new.target*";
pub(crate) const FUNC: &str = "*func*";
pub(crate) const ARGUMENTS: &str = "arguments";
pub(crate) const DEFAULT_EXPORT: &str = "*default*";

/// Where a binding lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    Reg(Reg),
    Slot(u16),
    /// A module import (index into `ModuleInfo::imports`).
    Import(u32),
    /// Resolved by name at runtime (script top level, sloppy eval vars).
    Named,
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub kind: BindingKind,
    pub loc: Loc,
    /// The declaration has been evaluated (straight-line TDZ elision).
    pub initialized: bool,
}

pub(crate) struct ScopeFrame {
    pub kind: ScopeKind,
    pub fn_index: usize,
    pub names: Vec<Rc<str>>,
    pub bindings: HashMap<Rc<str>, Binding>,
    /// Slot names in order (for the `ScopeInfo`).
    pub slots: Vec<(Rc<str>, BindingKind)>,
    pub has_env: bool,
    /// A `with` object environment.
    pub is_with: bool,
    /// Bindings in switch scopes are never provably initialised.
    pub always_check_tdz: bool,
    /// Register watermark to restore when the scope is popped.
    pub reg_mark: u16,
    /// Function-level scopes: where eval `var`s land.
    pub is_var_scope: bool,
    pub scope_idx: Option<ScopeIdx>,
    /// Indices into `FnState::locals` of this scope's register bindings.
    pub locals: Vec<usize>,
    /// The environment was created by the caller (module env, eval
    /// scope handled by the VM): no `PushEnv` is emitted.
    pub env_by_host: bool,
}

impl ScopeFrame {
    fn new(kind: ScopeKind, fn_index: usize, reg_mark: u16) -> ScopeFrame {
        ScopeFrame {
            kind,
            fn_index,
            names: Vec::new(),
            bindings: HashMap::new(),
            slots: Vec::new(),
            has_env: false,
            is_with: false,
            always_check_tdz: false,
            reg_mark,
            is_var_scope: false,
            scope_idx: None,
            locals: Vec::new(),
            env_by_host: false,
        }
    }
}

/// How a name resolved.
#[derive(Clone, Debug)]
pub(crate) enum Resolved {
    Reg {
        reg: Reg,
        kind: BindingKind,
        check_tdz: bool,
    },
    Env {
        depth: u16,
        slot: u16,
        kind: BindingKind,
        check_tdz: bool,
    },
    Import(u32),
    Global(Rc<str>),
    Dynamic(Rc<str>),
}

/// A pending exit through a `finally` block.
pub(crate) struct PendingExit {
    pub id: i32,
    pub kind: JumpKind,
    /// Index of the target control entry (for break/continue).
    pub target: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JumpKind {
    Break,
    Continue,
}

/// A for-of loop's iterator state.
#[derive(Clone, Copy)]
pub(crate) struct LoopIter {
    pub iter: Reg,
    /// Set to true before closing on break/return.
    pub closed: Reg,
    pub is_async: bool,
}

pub(crate) enum Ctl {
    /// A materialised environment: `PopEnv` when jumping out.
    Env,
    Loop {
        labels: Vec<Rc<str>>,
        breaks: Vec<usize>,
        continues: Vec<usize>,
        /// A for-of iterator to close when leaving the loop abruptly.
        iterator: Option<LoopIter>,
    },
    /// A labelled non-loop statement.
    Block {
        labels: Vec<Rc<str>>,
        breaks: Vec<usize>,
    },
    Switch {
        breaks: Vec<usize>,
    },
    /// An array destructuring in progress: its iterator is closed when a
    /// generator `return` resumption unwinds through it.
    DestructureIter {
        iter: Reg,
        done: Reg,
    },
    Finally {
        kind_reg: Reg,
        value_reg: Reg,
        entries: Vec<usize>,
        exits: Vec<PendingExit>,
        next_id: i32,
    },
}

pub(crate) struct FnState {
    /// Inline caches allocated so far (GetNamed / SetNamed sites).
    pub ic_count: u16,
    pub kind: CodeKind,
    pub function_kind: FunctionKind,
    pub strict: bool,
    pub is_derived_ctor: bool,
    pub analysis: Rc<FnAnalysis>,
    /// Every binding of this function must live in an environment.
    pub poisoned: bool,
    pub name: Rc<str>,
    pub span: Span,
    pub instrs: Vec<Instr>,
    pub offsets: Vec<u32>,
    pub constants: Vec<Constant>,
    pub number_consts: HashMap<u64, ConstIdx>,
    pub string_consts: HashMap<Rc<[u16]>, ConstIdx>,
    pub names: Vec<Rc<str>>,
    pub name_map: HashMap<Rc<str>, NameIdx>,
    pub functions: Vec<Rc<CodeBlock>>,
    pub scopes: Vec<ScopeInfo>,
    pub handlers: Vec<Handler>,
    pub next_reg: u16,
    pub max_reg: u16,
    pub env_depth: u16,
    pub scope_base: usize,
    pub ctl: Vec<Ctl>,
    pub cur_pos: u32,
    /// The next emitted instruction starts a statement.
    pub stmt_pending: Option<u32>,
    /// Debug info (see `DebugInfo`).
    pub statements: Vec<StatementStart>,
    pub locals: Vec<LocalVar>,
    /// Completion-value register (script and eval code).
    pub completion: Option<Reg>,
    /// Optional-chain short-circuit patch lists (innermost last).
    pub optional_exits: Vec<Vec<usize>>,
    pub param_count: u16,
    pub param_base: Reg,
    pub param_regs: u16,
    pub param_names: Vec<Rc<str>>,
    pub keeps_arguments: bool,
    pub declarations: Option<Declarations>,
    pub module: Option<ModuleInfo>,
    /// Inside a class field initializer: `arguments` is forbidden by the
    /// parser, but `eval` must know.
    pub is_field_initializer: bool,
}

pub struct Compiler<'a> {
    pub(crate) source: Rc<SourceText>,
    pub(crate) analyses: &'a Analyses,
    pub(crate) scopes: Vec<ScopeFrame>,
    pub(crate) fns: Vec<FnState>,
    /// Compiling eval code: unresolved names are looked up dynamically.
    pub(crate) eval_mode: bool,
}

pub(crate) type CResult<T> = Result<T, CompileError>;

impl<'a> Compiler<'a> {
    pub fn new(source: Rc<SourceText>, analyses: &'a Analyses, eval_mode: bool) -> Compiler<'a> {
        Compiler {
            source,
            analyses,
            scopes: Vec::new(),
            fns: Vec::new(),
            eval_mode,
        }
    }

    pub(crate) fn fs(&mut self) -> &mut FnState {
        self.fns.last_mut().unwrap()
    }
    pub(crate) fn fs_ref(&self) -> &FnState {
        self.fns.last().unwrap()
    }

    pub(crate) fn error<T>(&self, msg: impl Into<String>, span: Span) -> CResult<T> {
        Err(CompileError {
            message: msg.into(),
            span,
        })
    }

    // ---- emission ---------------------------------------------------------

    pub(crate) fn set_pos(&mut self, span: Span) {
        self.fs().cur_pos = span.start;
    }

    pub(crate) fn emit(&mut self, i: Instr) -> usize {
        let fs = self.fs();
        let pc = fs.instrs.len();
        fs.instrs.push(i);
        fs.offsets.push(fs.cur_pos);
        if let Some(offset) = fs.stmt_pending.take() {
            if fs.statements.last().map(|s| s.pc) != Some(pc as u32) {
                fs.statements.push(StatementStart {
                    pc: pc as u32,
                    offset,
                });
            }
        }
        pc
    }

    /// Mark the next emitted instruction as a statement start.
    pub(crate) fn mark_statement(&mut self) {
        let fs = self.fs();
        if fs.stmt_pending.is_none() {
            fs.stmt_pending = Some(fs.cur_pos);
        }
    }

    pub(crate) fn pc(&self) -> usize {
        self.fs_ref().instrs.len()
    }

    /// Emit a forward jump with a placeholder offset.
    pub(crate) fn emit_jump(&mut self, i: Instr) -> usize {
        self.emit(i)
    }

    /// Point jump `at` to the current pc.
    pub(crate) fn patch(&mut self, at: usize) {
        let target = self.pc();
        self.patch_to(at, target);
    }

    pub(crate) fn patch_to(&mut self, at: usize, target: usize) {
        let off = target as i64 - at as i64;
        let off = off as Offset;
        let fs = self.fs();
        fs.instrs[at] = with_offset(fs.instrs[at], off);
    }

    pub(crate) fn patch_all(&mut self, list: Vec<usize>) {
        for at in list {
            self.patch(at);
        }
    }

    pub(crate) fn patch_all_to(&mut self, list: Vec<usize>, target: usize) {
        for at in list {
            self.patch_to(at, target);
        }
    }

    pub(crate) fn emit_jump_to(&mut self, target: usize) {
        let at = self.pc();
        let off = target as i64 - at as i64;
        self.emit(Instr::Jump(off as Offset));
    }

    // ---- registers ------------------------------------------------------

    pub(crate) fn alloc(&mut self) -> Reg {
        self.alloc_n(1)
    }

    pub(crate) fn alloc_n(&mut self, n: u16) -> Reg {
        let fs = self.fs();
        let r = fs.next_reg;
        fs.next_reg += n;
        if fs.next_reg > fs.max_reg {
            fs.max_reg = fs.next_reg;
        }
        r
    }

    pub(crate) fn mark(&self) -> u16 {
        self.fs_ref().next_reg
    }

    pub(crate) fn free_to(&mut self, mark: u16) {
        self.fs().next_reg = mark;
    }

    // ---- constants and names -----------------------------------------

    pub(crate) fn const_number(&mut self, n: f64) -> ConstIdx {
        let fs = self.fs();
        let bits = if n == 0.0 && n.is_sign_negative() {
            u64::MAX - 1
        } else if n.is_nan() {
            u64::MAX
        } else {
            n.to_bits()
        };
        if let Some(&i) = fs.number_consts.get(&bits) {
            return i;
        }
        let i = fs.constants.len() as ConstIdx;
        fs.constants.push(Constant::Number(n));
        fs.number_consts.insert(bits, i);
        i
    }

    pub(crate) fn const_string(&mut self, s: &[u16]) -> ConstIdx {
        let fs = self.fs();
        if let Some(&i) = fs.string_consts.get(s) {
            return i;
        }
        let rc: Rc<[u16]> = Rc::from(s);
        let i = fs.constants.len() as ConstIdx;
        fs.constants.push(Constant::String(rc.clone()));
        fs.string_consts.insert(rc, i);
        i
    }

    pub(crate) fn const_str(&mut self, s: &str) -> ConstIdx {
        let v: Vec<u16> = s.encode_utf16().collect();
        self.const_string(&v)
    }

    pub(crate) fn const_bigint(&mut self, s: &str) -> ConstIdx {
        let fs = self.fs();
        let i = fs.constants.len() as ConstIdx;
        fs.constants.push(Constant::BigInt(Rc::from(s)));
        i
    }

    pub(crate) fn name(&mut self, s: &str) -> NameIdx {
        let fs = self.fs();
        if let Some(&i) = fs.name_map.get(s) {
            return i;
        }
        let rc: Rc<str> = Rc::from(s);
        let i = fs.names.len() as NameIdx;
        fs.names.push(rc.clone());
        fs.name_map.insert(rc, i);
        i
    }

    pub(crate) fn name_rc(&mut self, s: &Rc<str>) -> NameIdx {
        let fs = self.fs();
        if let Some(&i) = fs.name_map.get(s) {
            return i;
        }
        let i = fs.names.len() as NameIdx;
        fs.names.push(s.clone());
        fs.name_map.insert(s.clone(), i);
        i
    }

    /// Whether a UTF-16 key is representable as a `str` name.
    pub(crate) fn utf16_is_str(s: &[u16]) -> bool {
        String::from_utf16(s).is_ok()
    }

    pub(crate) fn emit_load_str(&mut self, s: &[u16]) {
        let c = self.const_string(s);
        self.emit(Instr::LdaConst(c));
    }

    pub(crate) fn emit_load_number(&mut self, n: f64) {
        match expr::int_immediate(n) {
            Some(i) => {
                self.emit(Instr::LdaInt(i));
            }
            None => {
                let c = self.const_number(n);
                self.emit(Instr::LdaConst(c));
            }
        }
    }

    pub(crate) fn emit_throw_error(&mut self, kind: ErrorKind, msg: &str) {
        let c = self.const_str(msg);
        self.emit(Instr::ThrowError { kind, message: c });
    }

    // ---- scopes -------------------------------------------------------------

    pub(crate) fn is_captured(&self, name: &str) -> bool {
        let fs = self.fs_ref();
        if fs.poisoned {
            return true;
        }
        fs.analysis.captured_names.contains(name) || fs.analysis.with_names.contains(name)
    }

    /// Open a scope. Bindings are added with `declare`; `finish_scope`
    /// emits the `PushEnv` once the slot list is known.
    pub(crate) fn open_scope(&mut self, kind: ScopeKind) {
        let fn_index = self.fns.len() - 1;
        let mark = self.mark();
        self.scopes.push(ScopeFrame::new(kind, fn_index, mark));
    }

    /// Declare a binding in the innermost open scope. Returns false if the
    /// name already exists there (the caller decides whether that is OK).
    pub(crate) fn declare(&mut self, name: &Rc<str>, kind: BindingKind, force_slot: bool) -> bool {
        let captured = force_slot || self.is_captured(name);
        let frame = self.scopes.last_mut().unwrap();
        if frame.bindings.contains_key(name) {
            return false;
        }
        let loc = if captured {
            let slot = frame.slots.len() as u16;
            frame.slots.push((name.clone(), kind));
            frame.has_env = true;
            Loc::Slot(slot)
        } else {
            Loc::Reg(0) // patched in finish_scope
        };
        frame.names.push(name.clone());
        frame.bindings.insert(
            name.clone(),
            Binding {
                kind,
                loc,
                initialized: false,
            },
        );
        true
    }

    /// Declare a binding that is resolved by name at runtime.
    pub(crate) fn declare_named(&mut self, name: &Rc<str>, kind: BindingKind) {
        let frame = self.scopes.last_mut().unwrap();
        if frame.bindings.contains_key(name) {
            return;
        }
        frame.names.push(name.clone());
        frame.bindings.insert(
            name.clone(),
            Binding {
                kind,
                loc: Loc::Named,
                initialized: true,
            },
        );
    }

    pub(crate) fn declare_import(&mut self, name: &Rc<str>, idx: u32) {
        let frame = self.scopes.last_mut().unwrap();
        frame.names.push(name.clone());
        frame.bindings.insert(
            name.clone(),
            Binding {
                kind: BindingKind::Const,
                loc: Loc::Import(idx),
                initialized: false,
            },
        );
    }

    /// Allocate registers for register bindings, emit `PushEnv` when the
    /// scope has slots, and reset lexical register bindings to the hole.
    /// Returns whether an environment was pushed.
    pub(crate) fn finish_scope(&mut self) -> bool {
        let idx = self.scopes.len() - 1;
        // Allocate registers.
        let names: Vec<Rc<str>> = self.scopes[idx].names.clone();
        let mut lexical_regs = Vec::new();
        for n in &names {
            let b = self.scopes[idx].bindings.get(n).unwrap().clone();
            if b.loc == Loc::Reg(0) {
                let r = self.alloc();
                self.scopes[idx].bindings.get_mut(n).unwrap().loc = Loc::Reg(r);
                let start = self.pc() as u32;
                let fs = self.fs();
                fs.locals.push(LocalVar {
                    name: n.clone(),
                    reg: r,
                    start,
                    end: u32::MAX,
                });
                let li = fs.locals.len() - 1;
                self.scopes[idx].locals.push(li);
                if b.kind.is_lexical() || b.kind == BindingKind::This {
                    lexical_regs.push(r);
                }
            }
        }
        let pushed = self.scopes[idx].has_env && !self.scopes[idx].env_by_host;
        if self.scopes[idx].has_env {
            let frame = &self.scopes[idx];
            let info = ScopeInfo {
                kind: frame.kind,
                bindings: frame.slots.clone(),
                is_var_scope: frame.is_var_scope,
            };
            let si = self.fs().scopes.len() as ScopeIdx;
            self.fs().scopes.push(info);
            self.scopes[idx].scope_idx = Some(si);
            if pushed {
                self.emit(Instr::PushEnv(si));
                self.fs().env_depth += 1;
                self.fs().ctl.push(Ctl::Env);
            }
        }
        if !lexical_regs.is_empty() {
            self.emit(Instr::LdaEmpty);
            for r in lexical_regs {
                self.emit(Instr::Star(r));
            }
        }
        pushed
    }

    fn end_locals(&mut self, frame: &ScopeFrame) {
        let end = self.pc() as u32;
        let fs = self.fs();
        for &li in &frame.locals {
            fs.locals[li].end = end;
        }
    }

    pub(crate) fn close_scope(&mut self) {
        let frame = self.scopes.pop().unwrap();
        self.end_locals(&frame);
        if frame.has_env && !frame.env_by_host {
            self.emit(Instr::PopEnv);
            self.fs().env_depth -= 1;
            match self.fs().ctl.pop() {
                Some(Ctl::Env) => {}
                _ => panic!("control stack out of sync"),
            }
        }
        self.free_to(frame.reg_mark);
    }

    /// Pop the scope without emitting `PopEnv` (the code path is dead or
    /// the pop was emitted by hand).
    pub(crate) fn close_scope_silent(&mut self) {
        let frame = self.scopes.pop().unwrap();
        self.end_locals(&frame);
        if frame.has_env && !frame.env_by_host {
            self.fs().env_depth -= 1;
            match self.fs().ctl.pop() {
                Some(Ctl::Env) => {}
                _ => panic!("control stack out of sync"),
            }
        }
        self.free_to(frame.reg_mark);
    }

    /// Resolve a name from the current position.
    pub(crate) fn resolve(&self, name: &str) -> Resolved {
        let cur_fn = self.fns.len() - 1;
        let mut depth: u16 = 0;
        let mut dynamic = false;
        let mut iter_fn = cur_fn;
        for frame in self.scopes.iter().rev() {
            let shadowable = !name.starts_with('*') && !name.starts_with('#');
            if frame.fn_index != iter_fn {
                // Leaving function `iter_fn` for its parent.
                if self.fns[iter_fn].analysis.own_eval && shadowable {
                    dynamic = true;
                }
                iter_fn = frame.fn_index;
            }
            if frame.is_with {
                if shadowable {
                    dynamic = true;
                }
                depth += 1;
                continue;
            }
            if let Some(b) = frame.bindings.get(name) {
                if dynamic {
                    return Resolved::Dynamic(Rc::from(name));
                }
                let same_fn = frame.fn_index == cur_fn;
                let check_tdz = (b.kind.is_lexical()
                    || b.kind == BindingKind::This
                    || b.kind == BindingKind::Param)
                    && (!same_fn || !b.initialized || frame.always_check_tdz);
                match b.loc {
                    Loc::Reg(reg) => {
                        if !same_fn {
                            // Analysis missed a capture; fall back to a
                            // dynamic lookup, which still works because the
                            // register is not visible... it is not. This is
                            // a compiler bug, so fail loudly.
                            panic!(
                                "binding {name} in a register referenced from an inner function"
                            );
                        }
                        return Resolved::Reg {
                            reg,
                            kind: b.kind,
                            check_tdz,
                        };
                    }
                    Loc::Slot(slot) => {
                        return Resolved::Env {
                            depth,
                            slot,
                            kind: b.kind,
                            check_tdz,
                        }
                    }
                    Loc::Import(i) => return Resolved::Import(i),
                    Loc::Named => {
                        return if self.eval_mode {
                            Resolved::Dynamic(Rc::from(name))
                        } else {
                            Resolved::Global(Rc::from(name))
                        }
                    }
                }
            }
            if frame.has_env {
                depth += 1;
            }
        }
        if self.fns[iter_fn].analysis.own_eval {
            dynamic = true;
        }
        if dynamic || self.eval_mode {
            Resolved::Dynamic(Rc::from(name))
        } else {
            Resolved::Global(Rc::from(name))
        }
    }

    /// Mark a binding in the current function as initialised.
    pub(crate) fn mark_initialized(&mut self, name: &str) {
        let cur_fn = self.fns.len() - 1;
        for frame in self.scopes.iter_mut().rev() {
            if frame.fn_index != cur_fn {
                return;
            }
            if let Some(b) = frame.bindings.get_mut(name) {
                b.initialized = true;
                return;
            }
        }
    }

    /// Load a binding into acc.
    pub(crate) fn emit_load_name(&mut self, name: &str, span: Span, for_typeof: bool) {
        self.set_pos(span);
        match self.resolve(name) {
            Resolved::Reg { reg, check_tdz, .. } => {
                self.emit(Instr::Ldar(reg));
                if check_tdz {
                    let n = self.name(name);
                    self.emit(Instr::CheckTdz(n));
                }
            }
            Resolved::Env {
                depth,
                slot,
                check_tdz,
                ..
            } => {
                self.emit(Instr::LdaEnv { depth, slot });
                if check_tdz {
                    let n = self.name(name);
                    self.emit(Instr::CheckTdz(n));
                }
            }
            Resolved::Import(i) => {
                self.emit(Instr::LdaImport(i));
                let n = self.name(name);
                self.emit(Instr::CheckTdz(n));
            }
            Resolved::Global(n) => {
                let n = self.name_rc(&n);
                self.emit(if for_typeof {
                    Instr::LdaGlobalOrUndefined(n)
                } else {
                    Instr::LdaGlobal(n)
                });
            }
            Resolved::Dynamic(n) => {
                let n = self.name_rc(&n);
                self.emit(if for_typeof {
                    Instr::LdaDynamicOrUndefined(n)
                } else {
                    Instr::LdaDynamic(n)
                });
            }
        }
    }

    /// Store acc into a binding. `init` initialises a lexical binding
    /// (no TDZ or const checks); otherwise this is an assignment.
    pub(crate) fn emit_store_name(&mut self, name: &str, span: Span, init: bool) {
        self.set_pos(span);
        let strict = self.fs_ref().strict;
        match self.resolve(name) {
            Resolved::Reg {
                reg,
                kind,
                check_tdz,
            } => {
                if init {
                    self.emit(Instr::Star(reg));
                    self.mark_initialized(name);
                    return;
                }
                if kind == BindingKind::FuncName {
                    if strict {
                        let n = self.name(name);
                        self.emit(Instr::ThrowConstAssign(n));
                    }
                    return;
                }
                if kind.is_const() {
                    if check_tdz {
                        let tmp = self.alloc();
                        self.emit(Instr::Star(tmp));
                        self.emit(Instr::Ldar(reg));
                        let n = self.name(name);
                        self.emit(Instr::CheckTdz(n));
                        self.emit(Instr::Ldar(tmp));
                        self.free_to(tmp);
                    }
                    let n = self.name(name);
                    if strict || kind == BindingKind::Class || kind == BindingKind::Const {
                        self.emit(Instr::ThrowConstAssign(n));
                    }
                    return;
                }
                if check_tdz {
                    let tmp = self.alloc();
                    self.emit(Instr::Star(tmp));
                    self.emit(Instr::Ldar(reg));
                    let n = self.name(name);
                    self.emit(Instr::CheckTdz(n));
                    self.emit(Instr::Ldar(tmp));
                    self.free_to(tmp);
                }
                self.emit(Instr::Star(reg));
            }
            Resolved::Env {
                depth,
                slot,
                kind,
                check_tdz,
            } => {
                if init {
                    self.emit(Instr::StaEnv { depth, slot });
                    self.mark_initialized(name);
                    return;
                }
                if kind == BindingKind::FuncName {
                    if strict {
                        let n = self.name(name);
                        self.emit(Instr::ThrowConstAssign(n));
                    }
                    return;
                }
                if kind.is_const() || kind == BindingKind::Internal {
                    if check_tdz {
                        let tmp = self.alloc();
                        self.emit(Instr::Star(tmp));
                        self.emit(Instr::LdaEnv { depth, slot });
                        let n = self.name(name);
                        self.emit(Instr::CheckTdz(n));
                        self.emit(Instr::Ldar(tmp));
                        self.free_to(tmp);
                    }
                    let n = self.name(name);
                    self.emit(Instr::ThrowConstAssign(n));
                    return;
                }
                if check_tdz {
                    let tmp = self.alloc();
                    self.emit(Instr::Star(tmp));
                    self.emit(Instr::LdaEnv { depth, slot });
                    let n = self.name(name);
                    self.emit(Instr::CheckTdz(n));
                    self.emit(Instr::Ldar(tmp));
                    self.free_to(tmp);
                }
                self.emit(Instr::StaEnv { depth, slot });
            }
            Resolved::Import(_) => {
                let n = self.name(name);
                self.emit(Instr::ThrowConstAssign(n));
            }
            Resolved::Global(n) => {
                let n = self.name_rc(&n);
                if init {
                    self.emit(Instr::InitGlobalLexical(n));
                } else {
                    self.emit(Instr::StaGlobal { name: n, strict });
                }
            }
            Resolved::Dynamic(n) => {
                let n = self.name_rc(&n);
                if init {
                    self.emit(Instr::InitDynamic(n));
                } else {
                    self.emit(Instr::StaDynamic { name: n, strict });
                }
            }
        }
    }

    pub(crate) fn private_name(&mut self, p: &PrivateName) -> Reg {
        let n = private_binding_name(&p.name);
        let r = self.alloc();
        self.emit_load_name(&n, p.span, false);
        self.emit(Instr::Star(r));
        r
    }

    /// Load `this` into acc.
    pub(crate) fn emit_load_this(&mut self, span: Span) {
        self.emit_load_name(THIS, span, false);
    }

    // ---- control flow -----------------------------------------------------

    /// Emit the code that leaves control entries from the top of the
    /// control stack down to (excluding) `target`, closing iterators,
    /// popping environments and routing through `finally` blocks. Returns
    /// true when the jump was diverted into a finally block (the caller
    /// must not emit its own jump).
    pub(crate) fn emit_unwind_to(&mut self, kind: JumpKind, target: usize, from: usize) -> bool {
        // Entries strictly above the target: the target itself is handled
        // by the caller (or the break case below).
        let mut i = from;
        while i > target + 1 {
            i -= 1;
            match &self.fs_ref().ctl[i] {
                Ctl::Env => {
                    self.emit(Instr::PopEnv);
                }
                Ctl::Loop { iterator, .. } => {
                    if let Some(li) = *iterator {
                        // Leaving this loop (break or a jump past it) closes
                        // its iterator; a `continue` of this very loop does
                        // not reach here because `target == i`.
                        self.emit_loop_iterator_close(li);
                    }
                }
                Ctl::Block { .. } | Ctl::Switch { .. } => {}
                Ctl::DestructureIter { iter, done } => {
                    let (iter, done) = (*iter, *done);
                    self.emit_destructure_iterator_close(iter, done);
                }
                Ctl::Finally { .. } => {
                    let (id, kind_reg) = match &mut self.fs().ctl[i] {
                        Ctl::Finally {
                            next_id,
                            exits,
                            kind_reg,
                            ..
                        } => {
                            let id = *next_id;
                            *next_id += 1;
                            exits.push(PendingExit { id, kind, target });
                            (id, *kind_reg)
                        }
                        _ => unreachable!(),
                    };
                    self.emit(Instr::LdaInt(id));
                    self.emit(Instr::Star(kind_reg));
                    let j = self.emit_jump(Instr::Jump(0));
                    if let Ctl::Finally { entries, .. } = &mut self.fs().ctl[i] {
                        entries.push(j);
                    }
                    return true;
                }
            }
        }
        // The target loop's iterator is closed by a break, not a continue.
        if kind == JumpKind::Break {
            if let Ctl::Loop {
                iterator: Some(li), ..
            } = &self.fs_ref().ctl[target]
            {
                let li = *li;
                self.emit_loop_iterator_close(li);
            }
        }
        false
    }

    /// Record a break/continue jump at the target entry.
    pub(crate) fn add_jump_patch(&mut self, kind: JumpKind, target: usize) {
        let j = self.emit_jump(Instr::Jump(0));
        match &mut self.fs().ctl[target] {
            Ctl::Loop {
                breaks, continues, ..
            } => {
                if kind == JumpKind::Break {
                    breaks.push(j)
                } else {
                    continues.push(j)
                }
            }
            Ctl::Block { breaks, .. } | Ctl::Switch { breaks } => breaks.push(j),
            _ => unreachable!(),
        }
    }

    /// Emit a `return` of acc from the current position, running finally
    /// blocks and closing iterators on the way out.
    pub(crate) fn emit_return(&mut self) {
        let from = self.fs_ref().ctl.len();
        self.emit_return_from(from);
    }

    pub(crate) fn emit_return_from(&mut self, from: usize) {
        let mut i = from;
        // The value being returned travels in a temp while we close
        // iterators (which clobber acc).
        let needs_tmp = (0..from).any(|k| {
            matches!(
                self.fs_ref().ctl[k],
                Ctl::Loop {
                    iterator: Some(_),
                    ..
                } | Ctl::DestructureIter { .. }
            )
        });
        let tmp = if needs_tmp {
            let t = self.alloc();
            self.emit(Instr::Star(t));
            Some(t)
        } else {
            None
        };
        // Environments only need popping when the return is diverted into
        // a finally block (which runs at its own depth); a real return
        // discards the frame, and the derived-constructor check below
        // resolves `this` at the current compile-time depth.
        let has_finally = (0..from).any(|k| matches!(self.fs_ref().ctl[k], Ctl::Finally { .. }));
        while i > 0 {
            i -= 1;
            match &self.fs_ref().ctl[i] {
                Ctl::Env => {
                    if has_finally {
                        self.emit(Instr::PopEnv);
                    }
                }
                Ctl::Loop {
                    iterator: Some(li), ..
                } => {
                    let li = *li;
                    self.emit_loop_iterator_close(li);
                }
                Ctl::Loop { .. } | Ctl::Block { .. } | Ctl::Switch { .. } => {}
                Ctl::DestructureIter { iter, done } => {
                    let (iter, done) = (*iter, *done);
                    self.emit_destructure_iterator_close(iter, done);
                }
                Ctl::Finally {
                    kind_reg,
                    value_reg,
                    ..
                } => {
                    let (kr, vr) = (*kind_reg, *value_reg);
                    if let Some(t) = tmp {
                        self.emit(Instr::Ldar(t));
                        self.free_to(t);
                    }
                    self.emit(Instr::Star(vr));
                    self.emit(Instr::LdaInt(2));
                    self.emit(Instr::Star(kr));
                    let j = self.emit_jump(Instr::Jump(0));
                    if let Ctl::Finally { entries, .. } = &mut self.fs().ctl[i] {
                        entries.push(j);
                    }
                    return;
                }
            }
        }
        if let Some(t) = tmp {
            self.emit(Instr::Ldar(t));
            self.free_to(t);
        }
        self.emit_final_return();
    }

    /// The actual `Return`, with constructor result checks.
    pub(crate) fn emit_final_return(&mut self) {
        if self.fs_ref().is_derived_ctor {
            let t = self.alloc();
            self.emit(Instr::Star(t));
            // Load `this` without the TDZ check; CheckDerivedReturn does it.
            match self.resolve(THIS) {
                Resolved::Reg { reg, .. } => self.emit(Instr::Ldar(reg)),
                Resolved::Env { depth, slot, .. } => self.emit(Instr::LdaEnv { depth, slot }),
                _ => self.emit(Instr::LdaUndefined),
            };
            let th = self.alloc();
            self.emit(Instr::Star(th));
            self.emit(Instr::Ldar(t));
            self.emit(Instr::CheckDerivedReturn { this: th });
            self.free_to(t);
        }
        self.emit(Instr::Return);
    }

    /// Close a for-of loop's iterator on a break/return exit, marking the
    /// loop's `closed` flag first so the loop's throw handler does not
    /// close it again if `return()` throws. Preserves acc.
    pub(crate) fn emit_loop_iterator_close(&mut self, li: LoopIter) {
        let saved = self.alloc();
        self.emit(Instr::Star(saved));
        self.emit(Instr::LdaTrue);
        self.emit(Instr::Star(li.closed));
        self.emit(Instr::Ldar(saved));
        self.free_to(saved);
        self.emit_iterator_close(li.iter, li.is_async);
    }

    /// Close a destructuring iterator that is not yet done. Preserves acc.
    pub(crate) fn emit_destructure_iterator_close(&mut self, iter: Reg, done: Reg) {
        let saved = self.alloc();
        self.emit(Instr::Star(saved));
        self.emit(Instr::Ldar(done));
        let skip = self.emit_jump(Instr::JumpIfTrue(0));
        // Mark done first: if return() throws, the pattern's handler must
        // not close the iterator a second time.
        self.emit(Instr::LdaTrue);
        self.emit(Instr::Star(done));
        self.emit(Instr::IteratorClose { iter });
        self.patch(skip);
        self.emit(Instr::Ldar(saved));
        self.free_to(saved);
    }

    /// Close an iterator on a normal (break/return) exit. Preserves acc.
    pub(crate) fn emit_iterator_close(&mut self, iter: Reg, is_async: bool) {
        if !is_async {
            self.emit(Instr::IteratorClose { iter });
            return;
        }
        let mark = self.mark();
        let saved = self.alloc();
        self.emit(Instr::Star(saved));
        let f = self.alloc();
        self.emit(Instr::GetIteratorReturn { iter });
        let skip = self.emit_jump(Instr::JumpIfUndefined(0));
        self.emit(Instr::Star(f));
        self.emit(Instr::Call {
            callee: f,
            this: iter,
            args: 0,
            argc: 0,
        });
        self.emit(Instr::Await);
        let c = self.const_str("iterator return() result is not an object");
        self.emit(Instr::ThrowIfNotObject(c));
        self.patch(skip);
        self.emit(Instr::Ldar(saved));
        self.free_to(mark);
    }

    /// Compile a statement list.
    pub(crate) fn stmts(&mut self, body: &[Statement]) -> CResult<()> {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// Create a new function state. The caller pushes scopes and compiles
    /// the body, then calls `finish_function`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_function(
        &mut self,
        name: Rc<str>,
        kind: CodeKind,
        function_kind: FunctionKind,
        strict: bool,
        analysis: Rc<FnAnalysis>,
        span: Span,
    ) {
        // A module's top-level bindings are forced into the module
        // environment by `compile_module` itself (they are exports and live
        // imports); its nested block scopes follow the capture analysis like
        // any function's, so `for (let i ...)` at module level does not copy
        // an environment per iteration (TASK-100.17).
        let poisoned = analysis.has_eval_inside || kind == CodeKind::Eval;
        self.fns.push(FnState {
            ic_count: 0,
            kind,
            function_kind,
            strict,
            is_derived_ctor: false,
            analysis,
            poisoned,
            name,
            span,
            instrs: Vec::new(),
            offsets: Vec::new(),
            constants: Vec::new(),
            number_consts: HashMap::new(),
            string_consts: HashMap::new(),
            names: Vec::new(),
            name_map: HashMap::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            handlers: Vec::new(),
            next_reg: 0,
            max_reg: 0,
            env_depth: 0,
            scope_base: self.scopes.len(),
            ctl: Vec::new(),
            cur_pos: span.start,
            stmt_pending: None,
            statements: Vec::new(),
            locals: Vec::new(),
            completion: None,
            optional_exits: Vec::new(),
            param_count: 0,
            param_base: 0,
            param_regs: 0,
            param_names: Vec::new(),
            keeps_arguments: false,
            declarations: None,
            module: None,
            is_field_initializer: false,
        });
    }

    pub(crate) fn finish_function(&mut self) -> Rc<CodeBlock> {
        let mut fs = self.fns.pop().unwrap();
        debug_assert!(fs.ctl.is_empty(), "control stack not empty");
        let len = fs.instrs.len() as u32;
        for l in &mut fs.locals {
            l.end = l.end.min(len);
        }
        debug_assert_eq!(self.scopes.len(), fs.scope_base, "scopes not closed");
        Rc::new(CodeBlock {
            name: fs.name,
            kind: fs.kind,
            function_kind: fs.function_kind,
            strict: fs.strict,
            is_derived_constructor: fs.is_derived_ctor,
            param_count: fs.param_count,
            register_count: fs.max_reg,
            param_base: fs.param_base,
            param_regs: fs.param_regs,
            param_names: fs.param_names,
            keeps_arguments: fs.keeps_arguments,
            instrs: fs.instrs,
            offsets: fs.offsets,
            constants: fs.constants,
            names: fs.names,
            functions: fs.functions,
            scopes: fs.scopes,
            handlers: fs.handlers,
            declarations: fs.declarations,
            source: self.source.clone(),
            span: fs.span,
            module: fs.module.map(Rc::new),
            ic_count: fs.ic_count,
            feedback: std::cell::OnceCell::new(),
            globals: std::cell::OnceCell::new(),
            debug: DebugInfo {
                statements: fs.statements,
                locals: fs.locals,
            }
            .into(),
        })
    }

    /// Allocate an inline-cache slot for a named property access.
    pub(crate) fn ic(&mut self) -> IcIdx {
        let fs = self.fs();
        if fs.ic_count == NO_IC {
            return NO_IC;
        }
        let i = fs.ic_count;
        fs.ic_count += 1;
        i
    }

    pub(crate) fn emit_get_named(&mut self, obj: Reg, name: NameIdx) {
        let ic = self.ic();
        self.emit(Instr::GetNamed { obj, name, ic });
    }

    pub(crate) fn emit_set_named(&mut self, obj: Reg, name: NameIdx, strict: bool) {
        let ic = self.ic();
        self.emit(Instr::SetNamed {
            obj,
            name,
            strict,
            ic,
        });
    }

    pub(crate) fn add_function(&mut self, cb: Rc<CodeBlock>) -> FuncIdx {
        let fs = self.fs();
        let i = fs.functions.len() as FuncIdx;
        fs.functions.push(cb);
        i
    }

    pub(crate) fn add_handler(&mut self, start: usize, end: usize, target: usize, env_depth: u16) {
        if start == end {
            return;
        }
        self.fs().handlers.push(Handler {
            start: start as u32,
            end: end as u32,
            target: target as u32,
            env_depth,
        });
    }

    /// Current environment depth (number of environments this function
    /// has pushed).
    pub(crate) fn env_depth(&self) -> u16 {
        self.fs_ref().env_depth
    }
}

/// Replace the jump offset of a jump instruction.
pub(crate) fn with_offset(i: Instr, off: Offset) -> Instr {
    match i {
        Instr::Jump(_) => Instr::Jump(off),
        Instr::JumpIfTrue(_) => Instr::JumpIfTrue(off),
        Instr::JumpIfFalse(_) => Instr::JumpIfFalse(off),
        Instr::JumpIfNullish(_) => Instr::JumpIfNullish(off),
        Instr::JumpIfNotNullish(_) => Instr::JumpIfNotNullish(off),
        Instr::JumpIfUndefined(_) => Instr::JumpIfUndefined(off),
        Instr::JumpIfNotUndefined(_) => Instr::JumpIfNotUndefined(off),
        Instr::JumpIfEmpty(_) => Instr::JumpIfEmpty(off),
        Instr::JumpIfNotEmpty(_) => Instr::JumpIfNotEmpty(off),
        Instr::JumpIfObject(_) => Instr::JumpIfObject(off),
        Instr::JumpIfStrictEq { reg, .. } => Instr::JumpIfStrictEq { reg, offset: off },
        Instr::JumpIfCmpRR { op, a, b, .. } => Instr::JumpIfCmpRR { op, a, b, offset: off },
        Instr::JumpIfNotCmpRR { op, a, b, .. } => Instr::JumpIfNotCmpRR { op, a, b, offset: off },
        Instr::JumpIfCmpRI { op, a, imm, .. } => Instr::JumpIfCmpRI { op, a, imm, offset: off },
        Instr::JumpIfNotCmpRI { op, a, imm, .. } => Instr::JumpIfNotCmpRI { op, a, imm, offset: off },
        Instr::JumpIfCmpRA { op, a, .. } => Instr::JumpIfCmpRA { op, a, offset: off },
        Instr::JumpIfNotCmpRA { op, a, .. } => Instr::JumpIfNotCmpRA { op, a, offset: off },
        Instr::JumpIfCmpAI { op, imm, .. } => Instr::JumpIfCmpAI { op, imm, offset: off },
        Instr::JumpIfNotCmpAI { op, imm, .. } => Instr::JumpIfNotCmpAI { op, imm, offset: off },
        Instr::IteratorStep { iter, next, .. } => Instr::IteratorStep {
            iter,
            next,
            done: off,
        },
        Instr::IteratorResult { .. } => Instr::IteratorResult { done: off },
        Instr::ForInNext { iter, .. } => Instr::ForInNext { iter, done: off },
        Instr::ResumeDispatch { kind, .. } => Instr::ResumeDispatch {
            kind,
            on_return: off,
        },
        other => panic!("not a jump: {other:?}"),
    }
}

/// The static string name of a property key, when it has one.
pub(crate) fn static_key_name(key: &PropertyKey) -> Option<Vec<u16>> {
    key.static_name()
}

/// Whether an expression is an anonymous function definition (for
/// NamedEvaluation).
pub(crate) fn is_anonymous_function(e: &Expression) -> bool {
    match e.unparen() {
        Expression::Function(f) => f.id.is_none(),
        Expression::Arrow(_) => true,
        Expression::Class(c) => c.id.is_none(),
        _ => false,
    }
}

/// How an anonymous function gets its name.
#[derive(Clone, Copy)]
pub(crate) enum NameHint<'n> {
    None,
    Static(&'n str),
    /// A register holding the (property-key) name.
    Dynamic(Reg),
}

pub(crate) fn utf16_to_string(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}
