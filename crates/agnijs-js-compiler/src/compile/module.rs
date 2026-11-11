//! Scripts, modules and eval code (the top-level code kinds).

use super::stmt::{bound_names, lexical_decls, var_decls};
use super::*;

impl<'a> Compiler<'a> {
    /// Compile the top-level function declarations into the current
    /// function table and build the `Declarations` record.
    fn top_level_declarations(
        &mut self,
        body: &[Statement],
        strict: bool,
    ) -> CResult<Declarations> {
        let lex = lexical_decls(body);
        let top_lexical: Vec<Rc<str>> = lex.lexical.iter().map(|(n, _)| n.clone()).collect();
        let vars = var_decls(body, strict, &top_lexical, &[]);
        let mut functions = Vec::new();
        for s in body {
            self.collect_top_function(s, &mut functions)?;
        }
        Ok(Declarations {
            var_names: vars.vars,
            functions,
            lexical: lex.lexical,
            annex_b_functions: vars.annex_b,
        })
    }

    /// The names collect_top_function would declare, without compiling.
    fn top_function_names(body: &[Statement]) -> Vec<Rc<str>> {
        fn walk(s: &Statement, out: &mut Vec<Rc<str>>) {
            match s {
                Statement::FunctionDeclaration(f) => out.push(f.id.as_ref().unwrap().name.clone()),
                Statement::Labeled(l) => walk(&l.body, out),
                Statement::ExportNamed(e) => {
                    if let Some(d) = &e.declaration {
                        walk(d, out);
                    }
                }
                Statement::ExportDefault(e) => {
                    if let ExportDefaultKind::Function(f) = &e.declaration {
                        out.push(
                            f.id.as_ref()
                                .map(|i| i.name.clone())
                                .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT)),
                        );
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for s in body {
            walk(s, &mut out);
        }
        out
    }

    fn collect_top_function(
        &mut self,
        s: &Statement,
        out: &mut Vec<(Rc<str>, FuncIdx)>,
    ) -> CResult<()> {
        match s {
            Statement::FunctionDeclaration(f) => {
                let name = f.id.as_ref().unwrap().name.clone();
                let idx = self.compile_function_decl(f, Some(&name), Default::default())?;
                out.push((name, idx));
            }
            Statement::Labeled(l) => self.collect_top_function(&l.body, out)?,
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    self.collect_top_function(d, out)?;
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
                    let idx = self.compile_function_decl(f, Some(&display), Default::default())?;
                    out.push((name, idx));
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn compile_script(&mut self, program: &Program, name: &str) -> CResult<Rc<CodeBlock>> {
        let analysis = self.analyses.get(program);
        self.begin_function(
            Rc::from(name),
            CodeKind::Script,
            FunctionKind::Normal,
            program.strict,
            analysis,
            program.span,
        );
        let cv = self.alloc();
        self.fs().completion = Some(cv);
        self.emit(Instr::LdaUndefined);
        self.emit(Instr::Star(cv));
        let decls = self.top_level_declarations(&program.body, program.strict)?;
        self.fs().declarations = Some(decls);
        self.emit(Instr::DeclareGlobals);
        self.stmts(&program.body)?;
        self.emit(Instr::Ldar(cv));
        self.emit(Instr::Return);
        Ok(self.finish_function())
    }

    pub fn compile_eval(&mut self, program: &Program, name: &str) -> CResult<Rc<CodeBlock>> {
        let analysis = self.analyses.get(program);
        let strict = program.strict;
        self.begin_function(
            Rc::from(name),
            CodeKind::Eval,
            FunctionKind::Normal,
            strict,
            analysis,
            program.span,
        );
        let cv = self.alloc();
        self.fs().completion = Some(cv);
        self.emit(Instr::LdaUndefined);
        self.emit(Instr::Star(cv));
        let lex = lexical_decls(&program.body);
        let top_lexical: Vec<Rc<str>> = lex.lexical.iter().map(|(n, _)| n.clone()).collect();
        let vars = var_decls(&program.body, strict, &top_lexical, &[]);
        let fn_names = Self::top_function_names(&program.body);
        // The eval scope: lexical declarations always; vars and functions
        // too in strict code (they are local to the eval).
        self.open_scope(ScopeKind::Eval);
        self.scopes.last_mut().unwrap().is_var_scope = strict;
        if strict {
            for n in &fn_names {
                self.declare(n, BindingKind::Function, true);
            }
            for n in &vars.vars {
                self.declare(n, BindingKind::Var, true);
            }
        } else {
            for n in &fn_names {
                self.declare_named(n, BindingKind::Function);
            }
            for n in &vars.vars {
                self.declare_named(n, BindingKind::Var);
            }
            for n in &vars.annex_b {
                self.declare_named(n, BindingKind::Var);
            }
        }
        for (n, k) in &lex.lexical {
            self.declare(n, *k, true);
        }
        self.finish_scope();
        // Functions are compiled now so that they see the eval scope.
        let decls = self.top_level_declarations(&program.body, strict)?;
        self.fs().declarations = Some(decls);
        self.emit(Instr::DeclareGlobals);
        self.stmts(&program.body)?;
        self.emit(Instr::Ldar(cv));
        self.emit(Instr::Return);
        self.close_scope_silent();
        Ok(self.finish_function())
    }

    pub fn compile_module(&mut self, program: &Program, name: &str) -> CResult<Rc<CodeBlock>> {
        let analysis = self.analyses.get(program);
        let fk = if analysis.has_tla {
            FunctionKind::Async
        } else {
            FunctionKind::Normal
        };
        let uses_this = analysis.uses.this;
        self.begin_function(
            Rc::from(name),
            CodeKind::Module,
            fk,
            true,
            analysis,
            program.span,
        );
        let lex = lexical_decls(&program.body);
        let top_lexical: Vec<Rc<str>> = lex.lexical.iter().map(|(n, _)| n.clone()).collect();
        let vars = var_decls(&program.body, true, &top_lexical, &[]);
        let fn_names = Self::top_function_names(&program.body);
        let info = self.module_info(program)?;

        self.open_scope(ScopeKind::Module);
        {
            let frame = self.scopes.last_mut().unwrap();
            frame.is_var_scope = true;
            frame.env_by_host = true;
            frame.has_env = true;
        }
        // Imports first (they are indirections, not slots).
        for (i, imp) in info.imports.iter().enumerate() {
            if imp.import_name.is_none() {
                // Namespace imports are ordinary (host-initialised) slots.
                self.declare(&imp.local_name, BindingKind::Const, true);
            } else {
                self.declare_import(&imp.local_name, i as u32);
            }
        }
        if uses_this {
            self.declare(&Rc::from(THIS), BindingKind::This, true);
        }
        for n in &fn_names {
            self.declare(n, BindingKind::Function, true);
        }
        for n in &vars.vars {
            self.declare(n, BindingKind::Var, true);
        }
        for (n, k) in &lex.lexical {
            self.declare(n, *k, true);
        }
        self.finish_scope();
        // Functions are compiled now so that they see the module scope.
        let decls = self.top_level_declarations(&program.body, true)?;
        // Namespace import slots count as initialised.
        for imp in &info.imports {
            if imp.import_name.is_none() {
                self.mark_initialized(&imp.local_name);
            }
        }
        if uses_this {
            self.emit(Instr::LdaUndefined);
            self.emit_store_name(THIS, program.span, true);
        }
        self.fs().declarations = Some(decls);
        self.fs().module = Some(info);
        self.stmts_disposing(&program.body)?;
        self.emit(Instr::LdaUndefined);
        self.emit(Instr::Return);
        self.close_scope_silent();
        Ok(self.finish_function())
    }

    fn module_info(&mut self, program: &Program) -> CResult<ModuleInfo> {
        let mut info = ModuleInfo::default();
        fn request(
            info: &mut ModuleInfo,
            source: &StringLiteral,
            attrs: &[ImportAttribute],
        ) -> usize {
            let spec: Rc<[u16]> = source.value.clone();
            let mut attributes: Vec<(Rc<[u16]>, Rc<[u16]>)> = attrs
                .iter()
                .map(|a| (Rc::from(a.key.as_utf16()), a.value.value.clone()))
                .collect();
            attributes.sort();
            if let Some(i) = info
                .requested
                .iter()
                .position(|r| r.specifier == spec && r.attributes == attributes)
            {
                return i;
            }
            info.requested.push(ModuleRequest {
                specifier: spec,
                attributes,
            });
            info.requested.len() - 1
        }
        for s in &program.body {
            match s {
                Statement::Import(d) => {
                    let r = request(&mut info, &d.source, &d.attributes);
                    for sp in &d.specifiers {
                        match sp {
                            ImportSpecifier::Default(l) => info.imports.push(ImportEntry {
                                request: r,
                                import_name: Some(Rc::from(
                                    "default".encode_utf16().collect::<Vec<u16>>(),
                                )),
                                local_name: l.name.clone(),
                            }),
                            ImportSpecifier::Namespace(l) => info.imports.push(ImportEntry {
                                request: r,
                                import_name: None,
                                local_name: l.name.clone(),
                            }),
                            ImportSpecifier::Named {
                                imported, local, ..
                            } => info.imports.push(ImportEntry {
                                request: r,
                                import_name: Some(Rc::from(imported.as_utf16())),
                                local_name: local.name.clone(),
                            }),
                        }
                    }
                }
                Statement::ExportNamed(e) => {
                    if let Some(decl) = &e.declaration {
                        let mut names = Vec::new();
                        match decl {
                            Statement::VariableDeclaration(v) => {
                                for d in &v.declarations {
                                    bound_names(&d.id, &mut names);
                                }
                            }
                            Statement::FunctionDeclaration(f) => {
                                names.push(f.id.as_ref().unwrap().name.clone())
                            }
                            Statement::ClassDeclaration(c) => {
                                names.push(c.id.as_ref().unwrap().name.clone())
                            }
                            _ => {}
                        }
                        for n in names {
                            info.local_exports.push(ExportEntry {
                                export_name: Some(Rc::from(n.encode_utf16().collect::<Vec<u16>>())),
                                request: None,
                                import_name: None,
                                local_name: Some(n),
                            });
                        }
                    } else if let Some(src) = &e.source {
                        let r = request(&mut info, src, &e.attributes);
                        for sp in &e.specifiers {
                            info.indirect_exports.push(ExportEntry {
                                export_name: Some(Rc::from(sp.exported.as_utf16())),
                                request: Some(r),
                                import_name: Some(Rc::from(sp.local.as_utf16())),
                                local_name: None,
                            });
                        }
                    } else {
                        for sp in &e.specifiers {
                            let local = match &sp.local {
                                ModuleExportName::Ident(i) => i.name.clone(),
                                ModuleExportName::String(s) => {
                                    Rc::from(String::from_utf16_lossy(&s.value))
                                }
                            };
                            info.local_exports.push(ExportEntry {
                                export_name: Some(Rc::from(sp.exported.as_utf16())),
                                request: None,
                                import_name: None,
                                local_name: Some(local),
                            });
                        }
                    }
                }
                Statement::ExportDefault(e) => {
                    let local: Rc<str> = match &e.declaration {
                        ExportDefaultKind::Function(f) => {
                            f.id.as_ref()
                                .map(|i| i.name.clone())
                                .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT))
                        }
                        ExportDefaultKind::Class(c) => {
                            c.id.as_ref()
                                .map(|i| i.name.clone())
                                .unwrap_or_else(|| Rc::from(DEFAULT_EXPORT))
                        }
                        ExportDefaultKind::Expression(_) => Rc::from(DEFAULT_EXPORT),
                    };
                    info.local_exports.push(ExportEntry {
                        export_name: Some(Rc::from("default".encode_utf16().collect::<Vec<u16>>())),
                        request: None,
                        import_name: None,
                        local_name: Some(local),
                    });
                }
                Statement::ExportAll(e) => {
                    let r = request(&mut info, &e.source, &e.attributes);
                    match &e.exported {
                        Some(name) => info.indirect_exports.push(ExportEntry {
                            export_name: Some(Rc::from(name.as_utf16())),
                            request: Some(r),
                            import_name: None,
                            local_name: None,
                        }),
                        None => info.star_exports.push(r),
                    }
                }
                _ => {}
            }
        }
        // Local exports that re-export an import are indirect exports.
        let mut i = 0;
        while i < info.local_exports.len() {
            let local = info.local_exports[i].local_name.clone().unwrap();
            if let Some(imp) = info.imports.iter().find(|imp| imp.local_name == local) {
                if let Some(import_name) = &imp.import_name {
                    let e = info.local_exports.remove(i);
                    info.indirect_exports.push(ExportEntry {
                        export_name: e.export_name,
                        request: Some(imp.request),
                        import_name: Some(import_name.clone()),
                        local_name: None,
                    });
                    continue;
                }
            }
            i += 1;
        }
        info.has_tla = self.fs_ref().function_kind.is_async();
        Ok(info)
    }
}

impl<'a> Compiler<'a> {
    /// Compile a function expression as a standalone code block (the
    /// `Function` constructor): a script-level function state is opened so
    /// that free names resolve as globals.
    pub fn compile_standalone_function(
        &mut self,
        f: &Function,
        program: &Program,
    ) -> CResult<Rc<CodeBlock>> {
        let analysis = self.analyses.get(program);
        self.begin_function(
            Rc::from("anonymous"),
            CodeKind::Script,
            FunctionKind::Normal,
            false,
            analysis,
            program.span,
        );
        let cb = self.compile_function(f, Some("anonymous"), Default::default())?;
        self.finish_function();
        Ok(cb)
    }
}
