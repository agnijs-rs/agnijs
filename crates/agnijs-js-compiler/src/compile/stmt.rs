//! Statements.

use super::*;

/// Collected declarations of a scope.
#[derive(Default)]
pub(crate) struct ScopeDecls {
    /// let/const/class names with kinds.
    pub lexical: Vec<(Rc<str>, BindingKind)>,
    /// Function declarations directly in this scope (name, node).
    pub functions: Vec<Rc<str>>,
}

/// Lexically scoped declarations directly in a statement list.
pub(crate) fn lexical_decls(body: &[Statement]) -> ScopeDecls {
    let mut d = ScopeDecls::default();
    for s in body {
        add_lexical_decl(s, &mut d);
    }
    d
}

fn add_lexical_decl(s: &Statement, d: &mut ScopeDecls) {
    match s {
        Statement::VariableDeclaration(v) if v.kind != VariableKind::Var => {
            let kind = if v.kind.is_const() {
                BindingKind::Const
            } else {
                BindingKind::Let
            };
            for decl in &v.declarations {
                let mut names = Vec::new();
                bound_names(&decl.id, &mut names);
                for n in names {
                    d.lexical.push((n, kind));
                }
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                d.lexical.push((id.name.clone(), BindingKind::Let));
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                d.functions.push(id.name.clone());
            }
        }
        Statement::Labeled(l) => add_lexical_decl(&l.body, d),
        Statement::ExportNamed(e) => {
            if let Some(decl) = &e.declaration {
                add_lexical_decl(decl, d);
            }
        }
        Statement::ExportDefault(e) => match &e.declaration {
            ExportDefaultKind::Function(f) => {
                d.functions.push(
                    f.id.as_ref()
                        .map(|i| i.name.clone())
                        .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT)),
                );
            }
            ExportDefaultKind::Class(c) => {
                d.lexical.push((
                    c.id.as_ref()
                        .map(|i| i.name.clone())
                        .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT)),
                    BindingKind::Let,
                ));
            }
            ExportDefaultKind::Expression(_) => d
                .lexical
                .push((Rc::from(DEFAULT_EXPORT), BindingKind::Const)),
        },
        _ => {}
    }
}

pub(crate) fn bound_names(p: &Pattern, out: &mut Vec<Rc<str>>) {
    match p {
        Pattern::Ident(i) => out.push(i.name.clone()),
        Pattern::Object(o) => {
            for prop in &o.properties {
                bound_names(&prop.value, out);
            }
            if let Some(r) = &o.rest {
                bound_names(r, out);
            }
        }
        Pattern::Array(a) => {
            for e in a.elements.iter().flatten() {
                bound_names(e, out);
            }
        }
        Pattern::Assignment(a) => bound_names(&a.left, out),
        Pattern::Rest(r) => bound_names(&r.argument, out),
        Pattern::Expression(_) => {}
    }
}

/// Var-scoped declarations of a function body: `var` names (in order of
/// appearance) and Annex B block-level functions that get a var binding.
#[derive(Default)]
pub(crate) struct VarDecls {
    pub vars: Vec<Rc<str>>,
    pub annex_b: Vec<Rc<str>>,
}

pub(crate) fn var_decls(
    body: &[Statement],
    strict: bool,
    top_lexical: &[Rc<str>],
    params: &[Rc<str>],
) -> VarDecls {
    let mut out = VarDecls::default();
    let mut blocked: Vec<Rc<str>> = top_lexical.to_vec();
    for s in body {
        collect_vars(s, &mut out, strict, true, &mut blocked, params);
    }
    out
}

fn collect_vars(
    s: &Statement,
    out: &mut VarDecls,
    strict: bool,
    top: bool,
    blocked: &mut Vec<Rc<str>>,
    params: &[Rc<str>],
) {
    match s {
        Statement::VariableDeclaration(v) if v.kind == VariableKind::Var => {
            for d in &v.declarations {
                bound_names(&d.id, &mut out.vars);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if !top && !strict && matches!(f.kind, FunctionKind::Normal) {
                if let Some(id) = &f.id {
                    if !blocked.contains(&id.name) && !params.contains(&id.name) {
                        out.annex_b.push(id.name.clone());
                    }
                }
            }
        }
        Statement::Block(b) => collect_vars_block(&b.body, out, strict, blocked, params),
        Statement::If(i) => {
            collect_vars_sub(&i.consequent, out, strict, blocked, params);
            if let Some(a) = &i.alternate {
                collect_vars_sub(a, out, strict, blocked, params);
            }
        }
        Statement::Labeled(l) => collect_vars(&l.body, out, strict, top, blocked, params),
        Statement::With(w) => collect_vars_sub(&w.body, out, strict, blocked, params),
        Statement::Switch(sw) => {
            let mut body: Vec<&Statement> = Vec::new();
            for c in &sw.cases {
                body.extend(c.consequent.iter());
            }
            collect_vars_block_refs(&body, out, strict, blocked, params);
        }
        Statement::Try(t) => {
            collect_vars_block(&t.block.body, out, strict, blocked, params);
            if let Some(h) = &t.handler {
                let mark = blocked.len();
                // A destructured catch parameter blocks Annex B hoisting;
                // a simple one does not (B.3.5).
                if let Some(p) = &h.param {
                    if !matches!(p, Pattern::Ident(_)) {
                        bound_names(p, blocked);
                    }
                }
                collect_vars_block(&h.body.body, out, strict, blocked, params);
                blocked.truncate(mark);
            }
            if let Some(f) = &t.finalizer {
                collect_vars_block(&f.body, out, strict, blocked, params);
            }
        }
        Statement::While(w) => collect_vars_sub(&w.body, out, strict, blocked, params),
        Statement::DoWhile(d) => collect_vars_sub(&d.body, out, strict, blocked, params),
        Statement::For(f) => {
            let mark = blocked.len();
            match &f.init {
                Some(ForInit::VariableDeclaration(v)) if v.kind == VariableKind::Var => {
                    for d in &v.declarations {
                        bound_names(&d.id, &mut out.vars);
                    }
                }
                Some(ForInit::VariableDeclaration(v)) => {
                    for d in &v.declarations {
                        bound_names(&d.id, blocked);
                    }
                }
                _ => {}
            }
            collect_vars_sub(&f.body, out, strict, blocked, params);
            blocked.truncate(mark);
        }
        Statement::ForIn(f) => {
            let mark = blocked.len();
            collect_for_left(&f.left, out, blocked);
            collect_vars_sub(&f.body, out, strict, blocked, params);
            blocked.truncate(mark);
        }
        Statement::ForOf(f) => {
            let mark = blocked.len();
            collect_for_left(&f.left, out, blocked);
            collect_vars_sub(&f.body, out, strict, blocked, params);
            blocked.truncate(mark);
        }
        Statement::ExportNamed(e) => {
            if let Some(d) = &e.declaration {
                collect_vars(d, out, strict, top, blocked, params);
            }
        }
        _ => {}
    }
}

fn collect_for_left(l: &ForLeft, out: &mut VarDecls, blocked: &mut Vec<Rc<str>>) {
    if let ForLeft::VariableDeclaration(v) = l {
        for d in &v.declarations {
            if v.kind == VariableKind::Var {
                bound_names(&d.id, &mut out.vars);
            } else {
                bound_names(&d.id, blocked);
            }
        }
    }
}

/// A sub-statement that is not a block still forms a scope for a lone
/// function declaration (`if (x) function f() {}`).
fn collect_vars_sub(
    s: &Statement,
    out: &mut VarDecls,
    strict: bool,
    blocked: &mut Vec<Rc<str>>,
    params: &[Rc<str>],
) {
    collect_vars(s, out, strict, false, blocked, params);
}

fn collect_vars_block(
    body: &[Statement],
    out: &mut VarDecls,
    strict: bool,
    blocked: &mut Vec<Rc<str>>,
    params: &[Rc<str>],
) {
    let refs: Vec<&Statement> = body.iter().collect();
    collect_vars_block_refs(&refs, out, strict, blocked, params);
}

fn collect_vars_block_refs(
    body: &[&Statement],
    out: &mut VarDecls,
    strict: bool,
    blocked: &mut Vec<Rc<str>>,
    params: &[Rc<str>],
) {
    let mark = blocked.len();
    // Lexical declarations of this block shadow Annex B hoisting of
    // functions declared in inner blocks.
    let mut d = ScopeDecls::default();
    for s in body {
        add_lexical_decl(s, &mut d);
    }
    for (n, _) in &d.lexical {
        blocked.push(n.clone());
    }
    // Functions declared directly in this block: hoisted unless blocked
    // by an enclosing lexical declaration (the block's own function
    // declarations do not block themselves).
    let fn_mark = blocked.len();
    for s in body {
        if let Statement::FunctionDeclaration(_) = s {
            collect_vars(s, out, strict, false, blocked, params);
        }
    }
    // Other function declarations in the same block are lexical from the
    // point of view of inner blocks.
    for n in &d.functions {
        blocked.push(n.clone());
    }
    let _ = fn_mark;
    for s in body {
        if !matches!(s, Statement::FunctionDeclaration(_)) {
            collect_vars(s, out, strict, false, blocked, params);
        }
    }
    blocked.truncate(mark);
}

/// Whether a function declaration gets its Annex B var binding, decided
/// by the same rules as `var_decls`. The compiler records the hoisted
/// names on the function state.
impl<'a> Compiler<'a> {
    /// Declare the lexical bindings and functions of a block-like scope
    /// (the scope must be open) and instantiate the functions after the
    /// scope is finished. Returns the list of function declarations to
    /// instantiate.
    pub(crate) fn declare_block_scope(&mut self, body: &[Statement]) {
        let d = lexical_decls(body);
        for (n, k) in &d.lexical {
            self.declare(n, *k, false);
        }
        for n in &d.functions {
            self.declare(n, BindingKind::Function, false);
        }
    }

    /// Instantiate hoisted function declarations of a statement list in
    /// the current scope.
    pub(crate) fn instantiate_functions(&mut self, body: &[Statement]) -> CResult<()> {
        for s in body {
            self.instantiate_function_stmt(s)?;
        }
        Ok(())
    }

    fn instantiate_function_stmt(&mut self, s: &Statement) -> CResult<()> {
        match s {
            Statement::FunctionDeclaration(f) => {
                let name =
                    f.id.as_ref()
                        .map(|i| i.name.clone())
                        .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT));
                self.function_closure(f, Some(&name))?;
                self.emit_store_name(&name, f.span, true);
            }
            Statement::Labeled(l) => self.instantiate_function_stmt(&l.body)?,
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    self.instantiate_function_stmt(d)?;
                }
            }
            Statement::ExportDefault(e) => {
                if let ExportDefaultKind::Function(f) = &e.declaration {
                    let name =
                        f.id.as_ref()
                            .map(|i| i.name.clone())
                            .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT));
                    let display: Rc<str> = if f.id.is_some() {
                        name.clone()
                    } else {
                        Rc::from("default")
                    };
                    self.function_closure(f, Some(&display))?;
                    self.emit_store_name(&name, f.span, true);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Compile a block body in a fresh scope.
    pub(crate) fn block(&mut self, body: &[Statement]) -> CResult<()> {
        self.open_scope(ScopeKind::Block);
        self.declare_block_scope(body);
        self.finish_scope();
        self.instantiate_functions(body)?;
        self.stmts_disposing(body)?;
        self.close_scope();
        Ok(())
    }

    fn set_completion_undefined(&mut self) {
        if let Some(cv) = self.fs_ref().completion {
            self.emit(Instr::LdaUndefined);
            self.emit(Instr::Star(cv));
        }
    }

    pub(crate) fn stmt(&mut self, s: &Statement) -> CResult<()> {
        if agnijs_js_parser::stack::exhausted() {
            return Err(crate::stack_exhausted());
        }
        self.set_pos(s.span());
        if !matches!(s, Statement::Block(_) | Statement::Empty(_)) {
            self.mark_statement();
        }
        match s {
            Statement::Block(b) => self.block(&b.body),
            Statement::Empty(_) => Ok(()),
            Statement::Debugger(_) => {
                self.emit(Instr::Debugger);
                Ok(())
            }
            Statement::Expression(e) => {
                match self.fs_ref().completion {
                    Some(cv) => {
                        self.expr(&e.expression)?;
                        self.emit(Instr::Star(cv));
                    }
                    None => self.expr_unused(&e.expression)?,
                }
                Ok(())
            }
            Statement::If(i) => {
                self.set_completion_undefined();
                let jf = self.cond_jump(&i.test, false)?;
                self.sub_statement(&i.consequent)?;
                if let Some(alt) = &i.alternate {
                    let jend = self.emit_jump(Instr::Jump(0));
                    self.patch_all(jf);
                    self.sub_statement(alt)?;
                    self.patch(jend);
                } else {
                    self.patch_all(jf);
                }
                Ok(())
            }
            Statement::Labeled(_) => self.labelled(s, Vec::new()),
            Statement::Break(j) => self.jump_stmt(JumpKind::Break, j),
            Statement::Continue(j) => self.jump_stmt(JumpKind::Continue, j),
            Statement::With(w) => {
                self.set_completion_undefined();
                self.expr(&w.object)?;
                self.emit(Instr::PushWith);
                let fn_index = self.fns.len() - 1;
                let mark = self.mark();
                let mut frame = ScopeFrame::new(ScopeKind::Block, fn_index, mark);
                frame.is_with = true;
                frame.has_env = true;
                frame.env_by_host = true; // we emitted PushWith ourselves
                self.scopes.push(frame);
                self.fs().env_depth += 1;
                self.fs().ctl.push(Ctl::Env);
                self.sub_statement(&w.body)?;
                self.scopes.pop();
                self.fs().env_depth -= 1;
                self.fs().ctl.pop();
                self.emit(Instr::PopEnv);
                Ok(())
            }
            Statement::Switch(sw) => self.switch(sw),
            Statement::Return(r) => {
                match &r.argument {
                    Some(a) => {
                        self.expr(a)?;
                        if self.fs_ref().function_kind == FunctionKind::AsyncGenerator {
                            self.emit(Instr::Await);
                        }
                    }
                    None => {
                        self.emit(Instr::LdaUndefined);
                    }
                }
                self.set_pos(r.span);
                self.emit_return();
                Ok(())
            }
            Statement::Throw(t) => {
                self.expr(&t.argument)?;
                self.set_pos(t.span);
                self.emit(Instr::Throw);
                Ok(())
            }
            Statement::Try(t) => self.try_stmt(t),
            Statement::While(_)
            | Statement::DoWhile(_)
            | Statement::For(_)
            | Statement::ForIn(_)
            | Statement::ForOf(_) => self.loop_stmt(s, Vec::new()),
            Statement::VariableDeclaration(d) => self.variable_declaration(d),
            Statement::FunctionDeclaration(f) => {
                // Hoisted; Annex B: copy the block binding to the var one.
                if let Some(id) = &f.id {
                    if self.annex_b_hoisted(&id.name) {
                        self.emit_load_name(&id.name, id.span, false);
                        self.emit_store_var_scope(&id.name, id.span);
                    }
                }
                Ok(())
            }
            Statement::ClassDeclaration(c) => {
                let name = c.id.as_ref().unwrap().name.clone();
                self.class(c, NameHint::Static(&name))?;
                self.emit_store_name(&name, c.span, true);
                Ok(())
            }
            Statement::Import(_) => Ok(()),
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    self.stmt(d)?;
                }
                Ok(())
            }
            Statement::ExportDefault(e) => match &e.declaration {
                ExportDefaultKind::Function(_) => Ok(()),
                ExportDefaultKind::Class(c) => {
                    let name =
                        c.id.as_ref()
                            .map(|i| i.name.clone())
                            .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT));
                    let display: &str = if c.id.is_some() { &name } else { "default" };
                    self.class(c, NameHint::Static(display))?;
                    self.emit_store_name(&name, c.span, true);
                    Ok(())
                }
                ExportDefaultKind::Expression(x) => {
                    self.expr_named(x, NameHint::Static("default"))?;
                    self.emit_store_name(DEFAULT_EXPORT, e.span, true);
                    Ok(())
                }
            },
            Statement::ExportAll(_) => Ok(()),
        }
    }

    /// Whether a block-level function declaration name got an Annex B var
    /// binding in the current function.
    fn annex_b_hoisted(&self, name: &str) -> bool {
        let fs = self.fs_ref();
        if fs.strict {
            return false;
        }
        // Only when the declaration is in a block, not at the top level of
        // the function, script, module or eval code.
        match self.scopes.last() {
            None => return false,
            Some(f) => {
                if f.fn_index != self.fns.len() - 1
                    || f.is_var_scope
                    || matches!(
                        f.kind,
                        ScopeKind::Eval | ScopeKind::Module | ScopeKind::Function
                    )
                {
                    return false;
                }
            }
        }
        if !fs_annex_b(fs, name) {
            return false;
        }
        // A `var` of this name would clash with a lexical binding in an
        // enclosing block (short of the var scope): this declaration is
        // not the one that was hoisted.
        let fs_index = self.fns.len() - 1;
        let base = fs.scope_base;
        for (i, frame) in self.scopes.iter().enumerate().rev().skip(1) {
            if frame.fn_index != fs_index || i < base || frame.is_var_scope {
                break;
            }
            if matches!(
                frame.kind,
                ScopeKind::Eval | ScopeKind::Module | ScopeKind::Function
            ) {
                break;
            }
            if frame.kind != ScopeKind::Catch && frame.bindings.contains_key(name) {
                return false;
            }
        }
        true
    }

    /// Store acc into the var-scope binding `name` of the current
    /// function (Annex B function hoisting).
    fn emit_store_var_scope(&mut self, name: &str, span: Span) {
        self.set_pos(span);
        let fs_index = self.fns.len() - 1;
        let base = self.fs_ref().scope_base;
        // Find the var scope of this function.
        let mut depth = 0u16;
        for (i, frame) in self.scopes.iter().enumerate().rev() {
            if frame.fn_index != fs_index || i < base {
                break;
            }
            if frame.is_var_scope {
                if let Some(b) = frame.bindings.get(name) {
                    match b.loc {
                        Loc::Reg(r) => {
                            self.emit(Instr::Star(r));
                        }
                        Loc::Slot(slot) => {
                            self.emit(Instr::StaEnv { depth, slot });
                        }
                        Loc::Named => {
                            let n = self.name(name);
                            let strict = self.fs_ref().strict;
                            if self.eval_mode {
                                self.emit(Instr::StaEvalVar(n));
                            } else {
                                self.emit(Instr::StaGlobal { name: n, strict });
                            }
                        }
                        Loc::Import(_) => {}
                    }
                }
                return;
            }
            if frame.has_env {
                depth += 1;
            }
        }
        // Script top level: the var is a global. Eval code: the eval's
        // variable environment, found by the VM.
        let n = self.name(name);
        let strict = self.fs_ref().strict;
        if self.eval_mode {
            self.emit(Instr::StaEvalVar(n));
        } else {
            self.emit(Instr::StaGlobal { name: n, strict });
        }
    }

    /// A statement in statement position of `if`/loops/labels: a lone
    /// function declaration there is Annex B sugar for a block.
    pub(crate) fn sub_statement(&mut self, s: &Statement) -> CResult<()> {
        if let Statement::FunctionDeclaration(_) = s {
            return self.block(std::slice::from_ref(s));
        }
        self.stmt(s)
    }

    fn labelled(&mut self, s: &Statement, mut labels: Vec<Rc<str>>) -> CResult<()> {
        let mut cur = s;
        while let Statement::Labeled(l) = cur {
            labels.push(l.label.name.clone());
            cur = &l.body;
        }
        match cur {
            Statement::While(_)
            | Statement::DoWhile(_)
            | Statement::For(_)
            | Statement::ForIn(_)
            | Statement::ForOf(_) => self.loop_stmt(cur, labels),
            _ => {
                self.fs().ctl.push(Ctl::Block {
                    labels,
                    breaks: Vec::new(),
                });
                self.sub_statement(cur)?;
                match self.fs().ctl.pop() {
                    Some(Ctl::Block { breaks, .. }) => self.patch_all(breaks),
                    _ => unreachable!(),
                }
                Ok(())
            }
        }
    }

    fn jump_stmt(&mut self, kind: JumpKind, j: &JumpStatement) -> CResult<()> {
        let label = j.label.as_ref().map(|l| l.name.clone());
        // Find the target entry.
        let ctl = &self.fs_ref().ctl;
        let mut target = None;
        for (i, c) in ctl.iter().enumerate().rev() {
            match c {
                Ctl::Loop { labels, .. } => {
                    let hit = match &label {
                        None => true,
                        Some(l) => labels.contains(l),
                    };
                    if hit {
                        target = Some(i);
                        break;
                    }
                }
                Ctl::Block { labels, .. } => {
                    if let Some(l) = &label {
                        if labels.contains(l) && kind == JumpKind::Break {
                            target = Some(i);
                            break;
                        }
                    }
                }
                Ctl::Switch { .. } => {
                    if label.is_none() && kind == JumpKind::Break {
                        target = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let target = match target {
            Some(t) => t,
            None => return self.error("jump target not found", j.span),
        };
        let from = self.fs_ref().ctl.len();
        if !self.emit_unwind_to(kind, target, from) {
            self.add_jump_patch(kind, target);
        }
        Ok(())
    }

    fn variable_declaration(&mut self, d: &VariableDeclaration) -> CResult<()> {
        for decl in &d.declarations {
            self.set_pos(decl.span);
            match (&decl.id, &decl.init) {
                (Pattern::Ident(id), None) => {
                    if d.kind != VariableKind::Var {
                        // `let x;` initialises to undefined.
                        self.emit(Instr::LdaUndefined);
                        self.emit_store_name(&id.name, id.span, true);
                    }
                }
                (Pattern::Ident(id), Some(init)) => {
                    // A var under with/eval: the reference is resolved
                    // before the initialiser runs.
                    let r = if d.kind == VariableKind::Var {
                        self.dynamic_ref(&id.name)
                    } else {
                        None
                    };
                    self.expr_named(init, NameHint::Static(&id.name))?;
                    self.emit_dispose_add(d.kind, decl.span)?;
                    if r.is_some() {
                        self.emit_store_ref(&id.name, id.span, r);
                    } else {
                        self.emit_store_name(&id.name, id.span, d.kind != VariableKind::Var);
                    }
                }
                (pat, Some(init)) => {
                    self.expr(init)?;
                    self.binding_pattern(pat, d.kind != VariableKind::Var)?;
                }
                (_, None) => unreachable!("destructuring declaration without initializer"),
            }
        }
        Ok(())
    }

    /// AddDisposableResource for the value in acc of a `using` or
    /// `await using` binding (nothing for other declarations).
    fn emit_dispose_add(&mut self, kind: VariableKind, span: Span) -> CResult<()> {
        if kind.is_using() {
            let Some(&stack) = self.fs_ref().dispose.last() else {
                return self.error("using declaration outside a using scope", span);
            };
            self.emit(Instr::DisposeAdd {
                stack,
                is_async: kind == VariableKind::AwaitUsing,
            });
        }
        Ok(())
    }

    fn switch(&mut self, sw: &SwitchStatement) -> CResult<()> {
        self.set_completion_undefined();
        self.expr(&sw.discriminant)?;
        let disc = self.alloc();
        self.emit(Instr::Star(disc));
        // One block scope for all cases.
        self.open_scope(ScopeKind::Block);
        let mut all: Vec<&Statement> = Vec::new();
        for c in &sw.cases {
            all.extend(c.consequent.iter());
        }
        let mut d = ScopeDecls::default();
        for s in &all {
            add_lexical_decl(s, &mut d);
        }
        for (n, k) in &d.lexical {
            self.declare(n, *k, false);
        }
        for n in &d.functions {
            self.declare(n, BindingKind::Function, false);
        }
        self.scopes.last_mut().unwrap().always_check_tdz = true;
        self.finish_scope();
        for s in &all {
            self.instantiate_function_stmt(s)?;
        }
        self.fs().ctl.push(Ctl::Switch { breaks: Vec::new() });
        // Tests.
        let mut case_jumps = Vec::new();
        let mut default_index = None;
        for (i, c) in sw.cases.iter().enumerate() {
            match &c.test {
                Some(t) => {
                    self.expr(t)?;
                    self.emit(Instr::StrictEq(disc));
                    case_jumps.push(Some(self.emit_jump(Instr::JumpIfTrue(0))));
                }
                None => {
                    default_index = Some(i);
                    case_jumps.push(None);
                }
            }
        }
        let jump_default = self.emit_jump(Instr::Jump(0));
        let mut default_pc = None;
        for (i, c) in sw.cases.iter().enumerate() {
            if let Some(j) = case_jumps[i] {
                self.patch(j);
            } else {
                default_pc = Some(self.pc());
            }
            self.stmts(&c.consequent)?;
        }
        let end = self.pc();
        match (default_index, default_pc) {
            (Some(_), Some(pc)) => self.patch_to(jump_default, pc),
            _ => self.patch_to(jump_default, end),
        }
        match self.fs().ctl.pop() {
            Some(Ctl::Switch { breaks }) => self.patch_all(breaks),
            _ => unreachable!(),
        }
        self.close_scope();
        self.free_to(disc);
        Ok(())
    }

    fn try_stmt(&mut self, t: &TryStatement) -> CResult<()> {
        self.set_completion_undefined();
        let outer_depth = self.env_depth();
        let mark = self.mark();
        let finally = t.finalizer.as_ref().map(|_| {
            let kind_reg = self.alloc();
            let value_reg = self.alloc();
            (kind_reg, value_reg)
        });
        if let Some((kind_reg, value_reg)) = finally {
            self.fs().ctl.push(Ctl::Finally {
                kind_reg,
                value_reg,
                entries: Vec::new(),
                exits: Vec::new(),
                next_id: 3,
            });
        }
        // Try block.
        let try_start = self.pc();
        self.block(&t.block.body)?;
        let try_end = self.pc();
        let mut to_end = Vec::new();
        to_end.push(self.emit_jump(Instr::Jump(0)));

        let mut catch_range = None;
        if let Some(h) = &t.handler {
            let catch_pc = self.pc();
            self.add_handler(try_start, try_end, catch_pc, outer_depth);
            let catch_start = self.pc();
            // Catch scope: the parameter and the body in one scope? The
            // body's lexical declarations may not shadow the parameter
            // (early error), so one scope is fine.
            self.open_scope(ScopeKind::Catch);
            let mut names = Vec::new();
            if let Some(p) = &h.param {
                bound_names(p, &mut names);
            }
            for n in &names {
                self.declare(n, BindingKind::CatchParam, false);
            }
            let exc = self.alloc();
            self.emit(Instr::Star(exc));
            self.finish_scope();
            if let Some(p) = &h.param {
                self.emit(Instr::Ldar(exc));
                match p {
                    Pattern::Ident(id) => self.emit_store_name(&id.name, id.span, true),
                    pat => self.binding_pattern(pat, true)?,
                }
            }
            // The catch body is its own block scope: closures in parameter
            // initialisers do not see its lexical declarations.
            self.set_completion_undefined();
            self.block(&h.body.body)?;
            self.close_scope();
            let catch_end = self.pc();
            catch_range = Some((catch_start, catch_end));
            to_end.push(self.emit_jump(Instr::Jump(0)));
        }

        if let Some(fin) = &t.finalizer {
            let (kind_reg, value_reg) = finally.unwrap();
            // Throw path: covers the try block (when there is no catch) or
            // the catch block, landing in finally with kind = throw.
            let throw_pc = self.pc();
            match catch_range {
                Some((cs, ce)) => self.add_handler(cs, ce, throw_pc, outer_depth),
                None => self.add_handler(try_start, try_end, throw_pc, outer_depth),
            }
            self.emit(Instr::Star(value_reg));
            self.emit(Instr::LdaInt(1));
            self.emit(Instr::Star(kind_reg));
            let to_finally = self.emit_jump(Instr::Jump(0));
            // Normal path.
            self.patch_all(to_end);
            self.emit(Instr::LdaInt(0));
            self.emit(Instr::Star(kind_reg));
            // Finally block.
            self.patch(to_finally);
            let (entries, exits) = match self.fs().ctl.pop() {
                Some(Ctl::Finally { entries, exits, .. }) => (entries, exits),
                _ => unreachable!(),
            };
            self.patch_all(entries);
            // The finally block's normal completion is discarded in favour
            // of the try/catch result; an abrupt exit from the try/catch
            // keeps its value too. Save it and start the block from empty.
            let saved_cv = match self.fs_ref().completion {
                Some(cv) => {
                    let s = self.alloc();
                    self.emit(Instr::Ldar(cv));
                    self.emit(Instr::Star(s));
                    self.emit(Instr::LdaUndefined);
                    self.emit(Instr::Star(cv));
                    Some((cv, s))
                }
                None => None,
            };
            self.block(&fin.body)?;
            self.emit_completion_dispatch(kind_reg, value_reg, exits, saved_cv);
        } else {
            self.patch_all(to_end);
        }
        self.free_to(mark);
        Ok(())
    }

    /// After a finally block (or a `using` scope's disposal): continue
    /// with the completion recorded in `kind_reg` and `value_reg`.
    fn emit_completion_dispatch(
        &mut self,
        kind_reg: Reg,
        value_reg: Reg,
        exits: Vec<PendingExit>,
        saved_cv: Option<(Reg, Reg)>,
    ) {
        {
            // Dispatch on the completion kind.
            let mut normal_jumps = Vec::new();
            // Normal: kind == 0.
            self.emit(Instr::LdaInt(0));
            normal_jumps.push(self.emit_jump(Instr::JumpIfStrictEq {
                reg: kind_reg,
                offset: 0,
            }));
            // Throw.
            self.emit(Instr::LdaInt(1));
            let not_throw = self.emit_jump(Instr::JumpIfStrictEq {
                reg: kind_reg,
                offset: 0,
            });
            let after_throw = self.emit_jump(Instr::Jump(0));
            self.patch(not_throw);
            self.emit(Instr::Ldar(value_reg));
            self.emit(Instr::Throw);
            self.patch(after_throw);
            // Return.
            self.emit(Instr::LdaInt(2));
            let is_return = self.emit_jump(Instr::JumpIfStrictEq {
                reg: kind_reg,
                offset: 0,
            });
            let after_return = self.emit_jump(Instr::Jump(0));
            self.patch(is_return);
            self.emit(Instr::Ldar(value_reg));
            let from = self.fs_ref().ctl.len();
            self.emit_return_from(from);
            self.patch(after_return);
            // Pending break/continue exits.
            for exit in exits {
                self.emit(Instr::LdaInt(exit.id));
                let hit = self.emit_jump(Instr::JumpIfStrictEq {
                    reg: kind_reg,
                    offset: 0,
                });
                let skip = self.emit_jump(Instr::Jump(0));
                self.patch(hit);
                if let Some((cv, s)) = saved_cv {
                    self.emit(Instr::Ldar(s));
                    self.emit(Instr::Star(cv));
                }
                let from = self.fs_ref().ctl.len();
                if !self.emit_unwind_to(exit.kind, exit.target, from) {
                    self.add_jump_patch(exit.kind, exit.target);
                }
                self.patch(skip);
            }
            self.patch_all(normal_jumps);
            if let Some((cv, s)) = saved_cv {
                self.emit(Instr::Ldar(s));
                self.emit(Instr::Star(cv));
            }
        }
    }

    /// Run `body` as the scope of `using` declarations: its resources are
    /// disposed however it completes (DisposeResources), with the
    /// completion routed like a finally block's. `is_async` when the scope
    /// has an `await using`.
    pub(crate) fn with_disposal(
        &mut self,
        is_async: bool,
        body: impl FnOnce(&mut Self) -> CResult<()>,
    ) -> CResult<()> {
        let outer_depth = self.env_depth();
        let mark = self.mark();
        let stack = self.alloc();
        let kind_reg = self.alloc();
        let value_reg = self.alloc();
        self.emit(Instr::DisposeStackNew);
        self.emit(Instr::Star(stack));
        self.fs().dispose.push(stack);
        self.fs().ctl.push(Ctl::Finally {
            kind_reg,
            value_reg,
            entries: Vec::new(),
            exits: Vec::new(),
            next_id: 3,
        });
        let start = self.pc();
        // An error abandons the whole compilation, control stack and all.
        body(self)?;
        self.fs().dispose.pop();
        let (entries, exits) = match self.fs().ctl.pop() {
            Some(Ctl::Finally { entries, exits, .. }) => (entries, exits),
            _ => return self.error("control stack out of sync", Span::new(0, 0)),
        };
        let end = self.pc();
        let to_end = self.emit_jump(Instr::Jump(0));
        // Throw path.
        let throw_pc = self.pc();
        self.add_handler(start, end, throw_pc, outer_depth);
        self.emit(Instr::Star(value_reg));
        self.emit(Instr::LdaInt(1));
        self.emit(Instr::Star(kind_reg));
        let to_dispose = self.emit_jump(Instr::Jump(0));
        // Normal path.
        self.patch(to_end);
        self.emit(Instr::LdaInt(0));
        self.emit(Instr::Star(kind_reg));
        self.patch(to_dispose);
        self.patch_all(entries);
        self.emit(Instr::DisposeBegin {
            stack,
            kind: kind_reg,
            value: value_reg,
        });
        let top = self.pc();
        self.emit(Instr::DisposeStep { stack });
        if is_async {
            let done = self.emit_jump(Instr::JumpIfEmpty(0));
            let await_pc = self.pc();
            self.emit(Instr::Await);
            self.emit_jump_to(top);
            // An awaited disposal's rejection joins the completion.
            let reject_pc = self.pc();
            self.add_handler(await_pc, await_pc + 1, reject_pc, outer_depth);
            self.emit(Instr::DisposeError { stack });
            self.emit_jump_to(top);
            self.patch(done);
        }
        self.emit(Instr::DisposeEnd { stack });
        self.emit_completion_dispatch(kind_reg, value_reg, exits, None);
        self.free_to(mark);
        Ok(())
    }

    /// Whether a statement list declares `using` bindings directly, and
    /// whether any of them is `await using`.
    pub(crate) fn using_scope(body: &[Statement]) -> Option<bool> {
        let mut found = None;
        for s in body {
            if let Statement::VariableDeclaration(v) = s {
                if v.kind.is_using() {
                    let a = v.kind == VariableKind::AwaitUsing;
                    found = Some(found.unwrap_or(false) || a);
                }
            }
        }
        found
    }

    /// Compile a statement list, as a `using` scope when it declares any.
    pub(crate) fn stmts_disposing(&mut self, body: &[Statement]) -> CResult<()> {
        match Self::using_scope(body) {
            Some(is_async) => self.with_disposal(is_async, |c| c.stmts(body)),
            None => self.stmts(body),
        }
    }

    // ---- loops -----------------------------------------------------------

    fn loop_stmt(&mut self, s: &Statement, labels: Vec<Rc<str>>) -> CResult<()> {
        self.set_completion_undefined();
        match s {
            Statement::While(w) => {
                let top = self.pc();
                self.fs().ctl.push(Ctl::Loop {
                    labels,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    iterator: None,
                });
                let exit = self.cond_jump(&w.test, false)?;
                self.sub_statement(&w.body)?;
                let (breaks, continues) = self.pop_loop();
                self.patch_all(continues);
                self.emit_jump_to(top);
                self.patch_all(exit);
                self.patch_all(breaks);
            }
            Statement::DoWhile(d) => {
                let top = self.pc();
                self.fs().ctl.push(Ctl::Loop {
                    labels,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    iterator: None,
                });
                self.sub_statement(&d.body)?;
                let (breaks, continues) = self.pop_loop();
                self.patch_all(continues);
                let back = self.cond_jump(&d.test, true)?;
                self.patch_all_to(back, top);
                self.patch_all(breaks);
            }
            Statement::For(f) => self.for_stmt(f, labels)?,
            Statement::ForIn(f) => self.for_in(f, labels)?,
            Statement::ForOf(f) => self.for_of(f, labels)?,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn pop_loop(&mut self) -> (Vec<usize>, Vec<usize>) {
        match self.fs().ctl.pop() {
            Some(Ctl::Loop {
                breaks, continues, ..
            }) => (breaks, continues),
            _ => unreachable!(),
        }
    }

    fn for_stmt(&mut self, f: &ForStatement, labels: Vec<Rc<str>>) -> CResult<()> {
        let mark = self.mark();
        match &f.init {
            Some(ForInit::VariableDeclaration(d)) if d.kind != VariableKind::Var => {
                self.open_scope(ScopeKind::Block);
                let kind = if d.kind.is_const() {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                for decl in &d.declarations {
                    let mut names = Vec::new();
                    bound_names(&decl.id, &mut names);
                    for n in names {
                        self.declare(&n, kind, false);
                    }
                }
                let pushed = self.finish_scope();
                let per_iteration = pushed && d.kind == VariableKind::Let;
                if d.kind.is_using() {
                    // The whole loop is the scope of the using bindings.
                    let is_async = d.kind == VariableKind::AwaitUsing;
                    self.with_disposal(is_async, |c| {
                        c.variable_declaration(d)?;
                        c.for_stmt_loop(f, labels, false)
                    })?;
                } else {
                    self.variable_declaration(d)?;
                    self.for_stmt_loop(f, labels, per_iteration)?;
                }
                self.close_scope();
            }
            Some(ForInit::VariableDeclaration(d)) => {
                self.variable_declaration(d)?;
                self.for_stmt_loop(f, labels, false)?;
            }
            Some(ForInit::Expression(e)) => {
                self.expr_unused(e)?;
                self.for_stmt_loop(f, labels, false)?;
            }
            None => self.for_stmt_loop(f, labels, false)?,
        }
        self.free_to(mark);
        Ok(())
    }

    /// The test, body and update of a classic for loop, after its head.
    fn for_stmt_loop(
        &mut self,
        f: &ForStatement,
        labels: Vec<Rc<str>>,
        per_iteration: bool,
    ) -> CResult<()> {
        if per_iteration {
            self.emit(Instr::CopyEnv);
        }
        let top = self.pc();
        let mut exit = Vec::new();
        if let Some(t) = &f.test {
            exit = self.cond_jump(t, false)?;
        }
        self.fs().ctl.push(Ctl::Loop {
            labels,
            breaks: Vec::new(),
            continues: Vec::new(),
            iterator: None,
        });
        self.sub_statement(&f.body)?;
        let (breaks, continues) = self.pop_loop();
        self.patch_all(continues);
        if per_iteration {
            self.emit(Instr::CopyEnv);
        }
        if let Some(u) = &f.update {
            self.expr_unused(u)?;
        }
        self.emit_jump_to(top);
        self.patch_all(exit);
        self.patch_all(breaks);
        Ok(())
    }

    /// Evaluate the right-hand side of for-in/of with the loop's lexical
    /// names in TDZ when the head is a let/const declaration.
    fn for_head_rhs(&mut self, left: &ForLeft, right: &Expression) -> CResult<()> {
        if let ForLeft::VariableDeclaration(d) = left {
            if d.kind != VariableKind::Var {
                let mut names = Vec::new();
                for decl in &d.declarations {
                    bound_names(&decl.id, &mut names);
                }
                // Only needed if the RHS could reference them.
                let fs = self.fs_ref();
                let referenced = names.iter().any(|n| fs.analysis.all_names.contains(n));
                if referenced {
                    self.open_scope(ScopeKind::Block);
                    for n in &names {
                        self.declare(n, BindingKind::Let, false);
                    }
                    self.finish_scope();
                    self.expr(right)?;
                    self.close_scope();
                    return Ok(());
                }
            }
        }
        self.expr(right)
    }

    /// Bind the iteration value (in acc) to the loop head.
    fn for_bind(&mut self, left: &ForLeft) -> CResult<()> {
        match left {
            ForLeft::VariableDeclaration(d) => {
                let decl = &d.declarations[0];
                let init = d.kind != VariableKind::Var;
                match &decl.id {
                    Pattern::Ident(id) => self.emit_store_name(&id.name, id.span, init),
                    pat => self.binding_pattern(pat, init)?,
                }
            }
            ForLeft::Pattern(p) => self.assign_pattern(p)?,
        }
        Ok(())
    }

    /// Open the per-iteration scope for a let/const head (if any).
    fn for_open_iteration_scope(&mut self, left: &ForLeft) -> bool {
        if let ForLeft::VariableDeclaration(d) = left {
            if d.kind != VariableKind::Var {
                self.open_scope(ScopeKind::Block);
                let kind = if d.kind.is_const() {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                let mut names = Vec::new();
                bound_names(&d.declarations[0].id, &mut names);
                for n in names {
                    self.declare(&n, kind, false);
                }
                self.finish_scope();
                return true;
            }
        }
        false
    }

    fn for_in(&mut self, f: &ForInStatement, labels: Vec<Rc<str>>) -> CResult<()> {
        let mark = self.mark();
        // Annex B: `for (var x = init in o)`.
        if let ForLeft::VariableDeclaration(d) = &f.left {
            if let Some(init) = &d.declarations[0].init {
                if let Pattern::Ident(id) = &d.declarations[0].id {
                    self.expr_named(init, NameHint::Static(&id.name))?;
                    self.emit_store_name(&id.name, id.span, false);
                }
            }
        }
        self.for_head_rhs(&f.left, &f.right)?;
        let iter = self.alloc();
        self.emit(Instr::ForInPrepare { dst: iter });
        let top = self.pc();
        let next = self.emit_jump(Instr::ForInNext { iter, done: 0 });
        self.fs().ctl.push(Ctl::Loop {
            labels,
            breaks: Vec::new(),
            continues: Vec::new(),
            iterator: None,
        });
        // Opening the iteration scope may emit TDZ initialisation that
        // clobbers acc, so the iteration value travels in a temp.
        let value = self.alloc();
        self.emit(Instr::Star(value));
        let scoped = self.for_open_iteration_scope(&f.left);
        self.emit(Instr::Ldar(value));
        self.for_bind(&f.left)?;
        self.sub_statement(&f.body)?;
        if scoped {
            self.close_scope();
        }
        let (breaks, continues) = self.pop_loop();
        self.patch_all(continues);
        self.emit_jump_to(top);
        self.patch(next);
        self.patch_all(breaks);
        self.free_to(mark);
        Ok(())
    }

    fn for_of(&mut self, f: &ForOfStatement, labels: Vec<Rc<str>>) -> CResult<()> {
        let mark = self.mark();
        let outer_depth = self.env_depth();
        self.for_head_rhs(&f.left, &f.right)?;
        let iter = self.alloc();
        let next = self.alloc();
        let closed = self.alloc();
        if f.is_await {
            self.emit(Instr::GetAsyncIterator { iter, next });
        } else {
            self.emit(Instr::GetIterator { iter, next });
        }
        self.emit(Instr::LdaFalse);
        self.emit(Instr::Star(closed));
        let top = self.pc();
        let done_jump;
        if f.is_await {
            self.emit(Instr::IteratorNextRaw { iter, next });
            self.emit(Instr::Await);
            done_jump = self.emit_jump(Instr::IteratorResult { done: 0 });
        } else {
            done_jump = self.emit_jump(Instr::IteratorStep {
                iter,
                next,
                done: 0,
            });
        }
        // Everything from here to the end of the body runs under a handler
        // that closes the iterator on throw.
        let body_start = self.pc();
        self.fs().ctl.push(Ctl::Loop {
            labels,
            breaks: Vec::new(),
            continues: Vec::new(),
            iterator: Some(LoopIter {
                iter,
                closed,
                is_async: f.is_await,
            }),
        });
        // Opening the iteration scope may emit TDZ initialisation that
        // clobbers acc, so the iteration value travels in a temp.
        let value = self.alloc();
        self.emit(Instr::Star(value));
        let scoped = self.for_open_iteration_scope(&f.left);
        match &f.left {
            ForLeft::VariableDeclaration(d) if d.kind.is_using() => {
                // Each iteration is the scope of its using binding.
                let is_async = d.kind == VariableKind::AwaitUsing;
                let kind = d.kind;
                let span = d.span;
                self.with_disposal(is_async, |c| {
                    c.emit(Instr::Ldar(value));
                    c.emit_dispose_add(kind, span)?;
                    c.for_bind(&f.left)?;
                    c.sub_statement(&f.body)
                })?;
            }
            _ => {
                self.emit(Instr::Ldar(value));
                self.for_bind(&f.left)?;
                self.sub_statement(&f.body)?;
            }
        }
        if scoped {
            self.close_scope();
        }
        let (breaks, continues) = self.pop_loop();
        self.patch_all(continues);
        let body_end = self.pc();
        self.emit_jump_to(top);
        // Throw handler: close (swallowing errors) unless a break/return
        // already closed the iterator, then rethrow.
        let handler_pc = self.pc();
        self.add_handler(body_start, body_end, handler_pc, outer_depth);
        let exc = self.alloc();
        self.emit(Instr::Star(exc));
        self.emit(Instr::Ldar(closed));
        let already = self.emit_jump(Instr::JumpIfTrue(0));
        if f.is_await {
            let fr = self.alloc();
            // Errors from getting and calling return() are swallowed: cover
            // the lookup, call and await with a handler that jumps to the
            // rethrow.
            let cs = self.pc();
            self.emit(Instr::GetIteratorReturn { iter });
            let skip = self.emit_jump(Instr::JumpIfUndefined(0));
            self.emit(Instr::Star(fr));
            self.emit(Instr::Call {
                callee: fr,
                this: iter,
                args: 0,
                argc: 0,
            });
            self.emit(Instr::Await);
            let ce = self.pc();
            self.patch(skip);
            let rethrow = self.pc();
            self.add_handler(cs, ce, rethrow, outer_depth);
        } else {
            self.emit(Instr::IteratorCloseSwallow { iter });
        }
        self.patch(already);
        self.emit(Instr::Ldar(exc));
        self.emit(Instr::Throw);
        self.patch(done_jump);
        self.patch_all(breaks);
        self.free_to(mark);
        Ok(())
    }
}

fn fs_annex_b(fs: &FnState, name: &str) -> bool {
    fs.declarations
        .as_ref()
        .map(|d| d.annex_b_functions.iter().any(|n| &**n == name))
        .unwrap_or(false)
}
