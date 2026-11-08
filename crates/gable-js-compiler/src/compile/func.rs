//! Functions: declaration instantiation, parameters, arrows, methods.

use super::stmt::{bound_names, lexical_decls, var_decls};
use super::*;
use crate::analysis::Uses;

/// Extra context for compiling a function body.
#[derive(Clone, Default)]
pub(crate) struct FuncCtx {
    pub kind: Option<CodeKind>,
    /// A named function expression binds its own name.
    pub self_name: Option<Rc<str>>,
    pub is_derived_ctor: bool,
    /// A base class constructor runs instance element initialisation on
    /// entry.
    pub is_base_ctor: bool,
}

impl<'a> Compiler<'a> {
    /// Compile a function and emit `CreateClosure`.
    pub(crate) fn function_closure(&mut self, f: &Function, name: Option<&str>) -> CResult<()> {
        let idx = self.compile_function_decl(f, name, FuncCtx::default())?;
        self.set_pos(f.span);
        self.emit(Instr::CreateClosure(idx));
        Ok(())
    }

    /// Compile a method (object literal or class) and emit `CreateClosure`.
    pub(crate) fn method_closure(&mut self, f: &Function, name: Option<&str>) -> CResult<()> {
        let idx = self.compile_function_decl(
            f,
            name,
            FuncCtx {
                kind: Some(CodeKind::Method),
                ..Default::default()
            },
        )?;
        self.set_pos(f.span);
        self.emit(Instr::CreateClosure(idx));
        Ok(())
    }

    /// Compile a function into the current function's table.
    pub(crate) fn compile_function_decl(
        &mut self,
        f: &Function,
        name: Option<&str>,
        ctx: FuncCtx,
    ) -> CResult<FuncIdx> {
        let cb = self.compile_function(f, name, ctx)?;
        Ok(self.add_function(cb))
    }

    pub(crate) fn compile_function(
        &mut self,
        f: &Function,
        name: Option<&str>,
        ctx: FuncCtx,
    ) -> CResult<Rc<CodeBlock>> {
        let analysis = self.analyses.get(f);
        let name: Rc<str> = Rc::from(name.or(f.id.as_ref().map(|i| &*i.name)).unwrap_or(""));
        let kind = ctx.kind.unwrap_or(CodeKind::Function);
        self.begin_function(name, kind, f.kind, f.strict, analysis.clone(), f.span);
        self.fs().is_derived_ctor = ctx.is_derived_ctor;
        self.function_body(
            &f.params,
            &f.body.body,
            f.simple_params,
            false,
            ctx,
            &analysis,
        )?;
        Ok(self.finish_function())
    }

    pub(crate) fn arrow_closure(&mut self, a: &ArrowFunction, name: Option<&str>) -> CResult<()> {
        let analysis = self.analyses.get(a);
        let name: Rc<str> = Rc::from(name.unwrap_or(""));
        let fk = if a.is_async {
            FunctionKind::Async
        } else {
            FunctionKind::Normal
        };
        self.begin_function(
            name,
            CodeKind::Arrow,
            fk,
            a.strict,
            analysis.clone(),
            a.span,
        );
        match &a.body {
            ArrowBody::Block(b) => {
                self.function_body(
                    &a.params,
                    &b.body,
                    a.simple_params,
                    true,
                    FuncCtx::default(),
                    &analysis,
                )?;
            }
            ArrowBody::Expression(e) => {
                self.function_prologue(
                    &a.params,
                    &[],
                    a.simple_params,
                    true,
                    FuncCtx::default(),
                    &analysis,
                )?;
                self.expr(e)?;
                self.emit_return();
                while self.scopes.len() > self.fs_ref().scope_base {
                    self.close_scope_silent();
                }
            }
        }
        let cb = self.finish_function();
        let idx = self.add_function(cb);
        self.set_pos(a.span);
        self.emit(Instr::CreateClosure(idx));
        Ok(())
    }

    /// Parameters, declaration instantiation, body, implicit return.
    pub(crate) fn function_body(
        &mut self,
        params: &[Pattern],
        body: &[Statement],
        simple: bool,
        is_arrow: bool,
        ctx: FuncCtx,
        analysis: &FnAnalysis,
    ) -> CResult<()> {
        self.function_prologue(params, body, simple, is_arrow, ctx, analysis)?;
        self.stmts(body)?;
        self.emit(Instr::LdaUndefined);
        self.emit_return();
        while self.scopes.len() > self.fs_ref().scope_base {
            self.close_scope_silent();
        }
        Ok(())
    }

    /// Everything up to the first body statement.
    pub(crate) fn function_prologue(
        &mut self,
        params: &[Pattern],
        body: &[Statement],
        simple: bool,
        is_arrow: bool,
        ctx: FuncCtx,
        analysis: &FnAnalysis,
    ) -> CResult<()> {
        let strict = self.fs_ref().strict;
        let poisoned = self.fs_ref().poisoned;
        let kind = self.fs_ref().kind;
        let fk = self.fs_ref().function_kind;

        // Parameter registers: one per formal, filled by the caller.
        let nparams = params.len() as u16;
        let param_base = self.alloc_n(nparams);
        self.fs().param_base = param_base;
        self.fs().param_regs = nparams;
        let mut length = 0u16;
        for p in params {
            match p {
                Pattern::Assignment(_) | Pattern::Rest(_) => break,
                _ => length += 1,
            }
        }
        self.fs().param_count = length;
        let mut param_names: Vec<Rc<str>> = Vec::new();
        for p in params {
            bound_names(p, &mut param_names);
        }
        let has_rest = matches!(params.last(), Some(Pattern::Rest(_)));

        let lex = lexical_decls(body);
        let top_lexical: Vec<Rc<str>> = lex.lexical.iter().map(|(n, _)| n.clone()).collect();
        // "arguments" joins parameterNames when an arguments object exists
        // (FunctionDeclarationInstantiation step 22.f), which blocks Annex B
        // hoisting of a block function of that name.
        let mut annex_blocked = param_names.clone();
        if !annex_blocked.iter().any(|n| &**n == "arguments") {
            annex_blocked.push(Rc::from("arguments"));
        }
        let vars = var_decls(body, strict, &top_lexical, &annex_blocked);

        // Does the function need an arguments object?
        let needs_arguments = !is_arrow
            && (analysis.uses.arguments || poisoned)
            && !param_names.iter().any(|n| &**n == ARGUMENTS)
            // With parameter expressions the body's declarations live in a
            // separate scope and do not suppress the arguments object.
            && (!simple
                || (!lex.functions.iter().any(|n| &**n == ARGUMENTS)
                    && !top_lexical.iter().any(|n| &**n == ARGUMENTS)))
            && kind != CodeKind::ClassInitializer;
        let mapped = needs_arguments && !strict && simple;
        self.fs().keeps_arguments = needs_arguments || has_rest;

        // Pseudo-bindings.
        let uses: Uses = analysis.uses.clone();
        let cap = &analysis.captured_uses;

        // ---- parameter scope ----
        self.open_scope(ScopeKind::Function);
        let separate_body_scope = !simple
            && (!vars.vars.is_empty()
                || !lex.functions.is_empty()
                || !top_lexical.is_empty()
                || analysis.has_eval_inside);
        if !separate_body_scope {
            self.scopes.last_mut().unwrap().is_var_scope = true;
        }
        if !is_arrow {
            let need_this = uses.this
                || poisoned
                || ctx.is_derived_ctor
                || ctx.is_base_ctor
                || kind == CodeKind::ClassInitializer;
            if need_this {
                self.declare(&Rc::from(THIS), BindingKind::This, cap.this || poisoned);
            }
            if uses.new_target || poisoned || ctx.is_derived_ctor {
                self.declare(
                    &Rc::from(NEW_TARGET),
                    BindingKind::Internal,
                    cap.new_target || poisoned,
                );
            }
            if uses.super_
                || poisoned
                || ctx.is_derived_ctor
                || ctx.is_base_ctor
                || kind == CodeKind::ClassInitializer
                || kind == CodeKind::Method
            {
                self.declare(
                    &Rc::from(FUNC),
                    BindingKind::Internal,
                    cap.super_ || poisoned,
                );
            }
        }
        for n in &param_names {
            // Mapped arguments alias parameters, which must then be slots.
            self.declare(n, BindingKind::Param, mapped);
        }
        if needs_arguments {
            self.declare(&Rc::from(ARGUMENTS), BindingKind::Var, false);
        }
        if !separate_body_scope {
            for n in &lex.functions {
                self.declare(n, BindingKind::Function, false);
            }
            for n in &vars.vars {
                self.declare(n, BindingKind::Var, false);
            }
            for n in &vars.annex_b {
                self.declare(n, BindingKind::Var, false);
            }
            for (n, k) in &lex.lexical {
                self.declare(n, *k, false);
            }
        }
        // The function expression's own name, unless a parameter or (in
        // the shared scope) a body declaration shadows it.
        let self_name: Option<Rc<str>> = match &ctx.self_name {
            Some(n) if !self.scope_has(n) => {
                self.declare(n, BindingKind::FuncName, false);
                Some(n.clone())
            }
            _ => None,
        };
        self.finish_scope();
        self.fs().param_names = param_names.clone();
        self.fs().declarations = Some(Declarations {
            var_names: Vec::new(),
            functions: Vec::new(),
            lexical: Vec::new(),
            annex_b_functions: vars.annex_b.clone(),
        });

        // ---- entry stores ----
        if !is_arrow {
            if self.scope_has(THIS) {
                if ctx.is_derived_ctor {
                    self.emit(Instr::LdaEmpty);
                    // Store without marking initialised: TDZ applies.
                    match self.resolve(THIS) {
                        Resolved::Reg { reg, .. } => {
                            self.emit(Instr::Star(reg));
                        }
                        Resolved::Env { depth, slot, .. } => {
                            self.emit(Instr::StaEnv { depth, slot });
                        }
                        _ => {}
                    }
                } else {
                    self.emit(Instr::LdaThis);
                    self.emit_store_name(THIS, self.fs_ref().span, true);
                }
            }
            if self.scope_has(NEW_TARGET) {
                self.emit(Instr::LdaNewTarget);
                self.emit_store_name(NEW_TARGET, self.fs_ref().span, true);
            }
            if self.scope_has(FUNC) {
                self.emit(Instr::LdaCallee);
                self.emit_store_name(FUNC, self.fs_ref().span, true);
            }
            if let Some(n) = &self_name {
                self.emit(Instr::LdaCallee);
                self.emit_store_name(n, self.fs_ref().span, true);
            }
        }
        if simple {
            // Copy parameters into slots where captured; registers already
            // hold them. Duplicate names: the last one wins.
            for (i, n) in param_names.iter().enumerate() {
                let last = param_names.iter().rposition(|m| m == n).unwrap();
                if last != i {
                    continue;
                }
                match self.resolve(n) {
                    Resolved::Env { depth, slot, .. } => {
                        self.emit(Instr::Ldar(param_base + i as u16));
                        self.emit(Instr::StaEnv { depth, slot });
                    }
                    Resolved::Reg { reg, .. } if reg != param_base + i as u16 => {
                        self.emit(Instr::Ldar(param_base + i as u16));
                        self.emit(Instr::Star(reg));
                    }
                    _ => {}
                }
                self.mark_initialized(n);
            }
        }
        if needs_arguments {
            self.emit(Instr::CreateArguments { mapped });
            self.emit_store_name(ARGUMENTS, self.fs_ref().span, true);
        }
        if !simple {
            // Parameters are in TDZ until initialised in order.
            let mut first = true;
            for n in &param_names {
                if let Resolved::Reg { reg, .. } = self.resolve(n) {
                    if first {
                        self.emit(Instr::LdaEmpty);
                        first = false;
                    }
                    self.emit(Instr::Star(reg));
                }
            }
            for (i, p) in params.iter().enumerate() {
                self.set_pos(p.span());
                match p {
                    Pattern::Rest(r) => {
                        self.emit(Instr::CreateRestArray { from: i as u16 });
                        self.binding_pattern(&r.argument, true)?;
                    }
                    p => {
                        self.emit(Instr::Ldar(param_base + i as u16));
                        self.binding_pattern(p, true)?;
                    }
                }
            }
        }

        // ---- body var scope ----
        if separate_body_scope {
            self.open_scope(ScopeKind::Function);
            self.scopes.last_mut().unwrap().is_var_scope = true;
            for n in &lex.functions {
                self.declare(n, BindingKind::Function, false);
            }
            for n in &vars.vars {
                self.declare(n, BindingKind::Var, false);
            }
            for n in &vars.annex_b {
                self.declare(n, BindingKind::Var, false);
            }
            for (n, k) in &lex.lexical {
                self.declare(n, *k, false);
            }
            self.finish_scope();
            // Vars with the same name as a parameter start with its value.
            let mut seen: Vec<&Rc<str>> = Vec::new();
            for n in vars.vars.iter().chain(vars.annex_b.iter()) {
                if seen.contains(&n) || !param_names.contains(n) || lex.functions.contains(n) {
                    continue;
                }
                seen.push(n);
                // Load from the parameter scope (one frame out).
                let outer = self.scopes.len() - 2;
                let b = self.scopes[outer].bindings.get(n).cloned().unwrap();
                match b.loc {
                    Loc::Reg(r) => {
                        self.emit(Instr::Ldar(r));
                    }
                    Loc::Slot(s) => {
                        let depth = if self.scopes.last().unwrap().has_env {
                            1
                        } else {
                            0
                        };
                        self.emit(Instr::LdaEnv { depth, slot: s });
                    }
                    _ => continue,
                }
                self.emit_store_name(n, self.fs_ref().span, false);
            }
        }

        // Hoisted functions.
        self.instantiate_functions(body)?;

        if ctx.is_base_ctor {
            let mark = self.mark();
            let f = self.alloc();
            self.emit_load_name(FUNC, self.fs_ref().span, false);
            self.emit(Instr::Star(f));
            let t = self.alloc();
            self.emit_load_this(self.fs_ref().span);
            self.emit(Instr::Star(t));
            self.emit(Instr::InitializeInstanceElements { func: f, this: t });
            self.free_to(mark);
        }
        if fk.is_generator() {
            self.emit(Instr::InitialYield);
        }
        Ok(())
    }

    /// Whether the current function's scopes declare `name`.
    fn scope_has(&self, name: &str) -> bool {
        let base = self.fs_ref().scope_base;
        self.scopes[base..]
            .iter()
            .any(|f| f.bindings.contains_key(name))
    }
}
