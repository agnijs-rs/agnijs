//! Classes.

use super::func::FuncCtx;
use super::*;

impl<'a> Compiler<'a> {
    /// ClassDefinitionEvaluation. acc = the constructor.
    pub(crate) fn class(&mut self, c: &Class, hint: NameHint) -> CResult<()> {
        let mark = self.mark();
        self.set_pos(c.span);
        // Class scope: inner name binding and private names.
        self.open_scope(ScopeKind::Class);
        if let Some(id) = &c.id {
            self.declare(&id.name, BindingKind::Class, false);
        }
        let mut private_names: Vec<Rc<str>> = Vec::new();
        for m in &c.body {
            let key = match m {
                ClassMember::Method(m) => &m.key,
                ClassMember::Field(f) => &f.key,
                ClassMember::StaticBlock(_) => continue,
            };
            if let PropertyKey::Private(p) = key {
                let n = private_binding_name(&p.name);
                if !private_names.contains(&n) {
                    private_names.push(n);
                }
            }
        }
        for n in &private_names {
            self.declare(n, BindingKind::Internal, true);
        }
        self.finish_scope();
        for n in &private_names {
            let ni = self.name_rc(n);
            self.emit(Instr::NewPrivateName(ni));
            self.emit_store_name(n, c.span, true);
        }

        // Heritage.
        let heritage = self.alloc();
        match &c.super_class {
            Some(h) => {
                self.expr(h)?;
            }
            None => {
                self.emit(Instr::LdaEmpty);
            }
        }
        self.emit(Instr::Star(heritage));

        // Name.
        let name_reg = self.alloc();
        let static_name: Rc<str> = match (&c.id, hint) {
            (Some(id), _) => id.name.clone(),
            (None, NameHint::Static(s)) => Rc::from(s),
            _ => Rc::from(""),
        };
        match hint {
            NameHint::Dynamic(r) if c.id.is_none() => {
                self.emit(Instr::Ldar(r));
            }
            _ => {
                self.emit(Instr::LdaEmpty);
            }
        }
        self.emit(Instr::Star(name_reg));

        // Constructor.
        let derived = c.super_class.is_some();
        let ctor = c.body.iter().find_map(|m| match m {
            ClassMember::Method(m) if m.kind == MethodKind::Constructor => Some(m),
            _ => None,
        });
        let ctor_idx = match ctor {
            Some(m) => self.compile_function_decl(
                &m.value,
                Some(&static_name),
                FuncCtx {
                    kind: Some(CodeKind::ClassConstructor),
                    is_derived_ctor: derived,
                    is_base_ctor: !derived,
                    self_name: None,
                    source_span: Some(c.span),
                },
            )?,
            None => self.default_constructor(&static_name, derived, c.span),
        };
        let proto = self.alloc();
        let ctor_reg = self.alloc();
        self.set_pos(c.span);
        self.emit(Instr::CreateClass {
            func: ctor_idx,
            heritage,
            proto,
            name: name_reg,
        });
        self.emit(Instr::Star(ctor_reg));

        // Elements, first pass: methods and field keys / initialisers.
        struct StaticElem {
            key: Option<Reg>,
            init: Reg,
            anon: bool,
            is_block: bool,
        }
        let mut statics: Vec<StaticElem> = Vec::new();
        let mut static_private_methods: Vec<(Rc<str>, Reg)> = Vec::new();
        let mut instance_private_methods: Vec<Rc<str>> = Vec::new();
        for m in &c.body {
            match m {
                ClassMember::Method(m) => {
                    if m.kind == MethodKind::Constructor {
                        continue;
                    }
                    self.set_pos(m.span);
                    let home = if m.is_static { ctor_reg } else { proto };
                    let m2 = self.mark();
                    if let PropertyKey::Private(p) = &m.key {
                        let name = self.private_name(p);
                        self.method_closure(&m.value, None, m.span)?;
                        let f = self.alloc();
                        self.emit(Instr::Star(f));
                        self.emit(Instr::SetHomeObject { func: f, home });
                        self.emit(Instr::Ldar(f));
                        let kind = match m.kind {
                            MethodKind::Get => PrivateKind::Getter,
                            MethodKind::Set => PrivateKind::Setter,
                            _ => PrivateKind::Method,
                        };
                        self.emit(Instr::SetPrivateMethod { name, kind });
                        let bn = private_binding_name(&p.name);
                        if m.is_static {
                            if !static_private_methods.iter().any(|(n, _)| *n == bn) {
                                static_private_methods.push((bn.clone(), name));
                            }
                            // Keep the register alive until the brands are
                            // added below.
                            continue;
                        } else if !instance_private_methods.contains(&bn) {
                            instance_private_methods.push(bn);
                            self.emit(Instr::AddInstancePrivateMethod {
                                ctor: ctor_reg,
                                name,
                            });
                        }
                        self.free_to(m2);
                        continue;
                    }
                    let key = self.property_key_reg(&m.key)?;
                    self.method_closure(&m.value, None, m.span)?;
                    match m.kind {
                        MethodKind::Get => self.emit(Instr::DefineGetter {
                            obj: home,
                            key,
                            enumerable: false,
                        }),
                        MethodKind::Set => self.emit(Instr::DefineSetter {
                            obj: home,
                            key,
                            enumerable: false,
                        }),
                        _ => self.emit(Instr::DefineMethod {
                            obj: home,
                            key,
                            enumerable: false,
                        }),
                    };
                    self.free_to(m2);
                }
                ClassMember::Field(f) => {
                    self.set_pos(f.span);
                    let key = self.property_key_reg(&f.key)?;
                    let anon = f.value.as_ref().map(is_anonymous_function).unwrap_or(false);
                    let static_key = static_key_name(&f.key)
                        .filter(|k| Self::utf16_is_str(k))
                        .map(|k| utf16_to_string(&k));
                    let init = self.alloc();
                    match &f.value {
                        Some(v) => {
                            let home = if f.is_static { ctor_reg } else { proto };
                            let idx =
                                self.compile_field_initializer(f, v, static_key.as_deref())?;
                            self.emit(Instr::CreateClosure(idx));
                            self.emit(Instr::Star(init));
                            self.emit(Instr::SetHomeObject { func: init, home });
                        }
                        None => {
                            self.emit(Instr::LdaUndefined);
                            self.emit(Instr::Star(init));
                        }
                    }
                    // Anonymous functions get the key as name only when the
                    // key is computed (static keys are baked in).
                    let anon = anon && static_key.is_none();
                    if f.is_static {
                        statics.push(StaticElem {
                            key: Some(key),
                            init,
                            anon,
                            is_block: false,
                        });
                    } else {
                        self.emit(Instr::AddField {
                            ctor: ctor_reg,
                            key,
                            init,
                            anon,
                        });
                    }
                }
                ClassMember::StaticBlock(b) => {
                    self.set_pos(b.span);
                    let idx = self.compile_static_block(b)?;
                    let init = self.alloc();
                    self.emit(Instr::CreateClosure(idx));
                    self.emit(Instr::Star(init));
                    self.emit(Instr::SetHomeObject {
                        func: init,
                        home: ctor_reg,
                    });
                    statics.push(StaticElem {
                        key: None,
                        init,
                        anon: false,
                        is_block: true,
                    });
                }
            }
        }

        // Inner name binding.
        if let Some(id) = &c.id {
            self.emit(Instr::Ldar(ctor_reg));
            self.emit_store_name(&id.name, id.span, true);
        }
        // Static private methods.
        for (_, name) in static_private_methods {
            self.emit(Instr::AddPrivateBrand {
                obj: ctor_reg,
                name,
            });
        }
        // Static fields and blocks, in order.
        for s in statics {
            if s.is_block {
                self.emit(Instr::Call {
                    callee: s.init,
                    this: ctor_reg,
                    args: 0,
                    argc: 0,
                });
            } else {
                let key = s.key.unwrap();
                let init = s.init;
                self.emit(Instr::Ldar(init));
                let skip = self.emit_jump(Instr::JumpIfUndefined(0));
                self.emit(Instr::Call {
                    callee: init,
                    this: ctor_reg,
                    args: 0,
                    argc: 0,
                });
                self.patch(skip);
                self.emit(Instr::DefineField {
                    obj: ctor_reg,
                    key,
                    anon: s.anon,
                });
            }
        }
        self.emit(Instr::Ldar(ctor_reg));
        self.close_scope();
        self.free_to(mark);
        Ok(())
    }

    fn default_constructor(&mut self, name: &str, derived: bool, span: Span) -> FuncIdx {
        let analysis = Rc::new(FnAnalysis::default());
        self.begin_function(
            Rc::from(name),
            CodeKind::ClassConstructor,
            FunctionKind::Normal,
            true,
            analysis,
            span,
        );
        self.fs().is_derived_ctor = derived;
        self.open_scope(ScopeKind::Function);
        self.scopes.last_mut().unwrap().is_var_scope = true;
        self.declare(&Rc::from(THIS), BindingKind::This, false);
        self.declare(&Rc::from(FUNC), BindingKind::Internal, false);
        if derived {
            self.declare(&Rc::from(NEW_TARGET), BindingKind::Internal, false);
        }
        self.finish_scope();
        self.emit(Instr::LdaCallee);
        self.emit_store_name(FUNC, span, true);
        if derived {
            self.fs().keeps_arguments = true;
            self.emit(Instr::LdaNewTarget);
            self.emit_store_name(NEW_TARGET, span, true);
            let func = self.alloc();
            self.emit_load_name(FUNC, span, false);
            self.emit(Instr::Star(func));
            let nt = self.alloc();
            self.emit_load_name(NEW_TARGET, span, false);
            self.emit(Instr::Star(nt));
            let args = self.alloc();
            self.emit(Instr::CreateRestArray { from: 0 });
            self.emit(Instr::Star(args));
            self.emit(Instr::SuperCallSpread {
                func,
                new_target: nt,
                args,
            });
            let result = self.alloc();
            self.emit(Instr::Star(result));
            self.emit_store_name(THIS, span, true);
            self.emit(Instr::InitializeInstanceElements { func, this: result });
            self.emit(Instr::Ldar(result));
            self.emit_return();
        } else {
            self.emit(Instr::LdaThis);
            self.emit_store_name(THIS, span, true);
            let f = self.alloc();
            self.emit_load_name(FUNC, span, false);
            self.emit(Instr::Star(f));
            let t = self.alloc();
            self.emit_load_this(span);
            self.emit(Instr::Star(t));
            self.emit(Instr::InitializeInstanceElements { func: f, this: t });
            self.emit(Instr::LdaUndefined);
            self.emit_return();
        }
        self.close_scope_silent();
        let cb = self.finish_function();
        self.add_function(cb)
    }

    /// A field initialiser: a method-like function returning the value.
    fn compile_field_initializer(
        &mut self,
        f: &ClassField,
        value: &Expression,
        name: Option<&str>,
    ) -> CResult<FuncIdx> {
        let analysis = self.analyses.get(f);
        self.begin_function(
            Rc::from(""),
            CodeKind::ClassInitializer,
            FunctionKind::Normal,
            true,
            analysis.clone(),
            f.span,
        );
        self.fs().is_field_initializer = true;
        self.function_prologue(
            &[],
            &[],
            true,
            false,
            FuncCtx {
                kind: Some(CodeKind::ClassInitializer),
                ..Default::default()
            },
            &analysis,
        )?;
        match name {
            Some(n) => self.expr_named(value, NameHint::Static(n))?,
            None => self.expr(value)?,
        }
        self.emit_return();
        while self.scopes.len() > self.fs_ref().scope_base {
            self.close_scope_silent();
        }
        let cb = self.finish_function();
        Ok(self.add_function(cb))
    }

    fn compile_static_block(&mut self, b: &StaticBlock) -> CResult<FuncIdx> {
        let analysis = self.analyses.get(b);
        self.begin_function(
            Rc::from(""),
            CodeKind::ClassInitializer,
            FunctionKind::Normal,
            true,
            analysis.clone(),
            b.span,
        );
        self.fs().is_field_initializer = true;
        self.function_body(
            &[],
            &b.body,
            true,
            false,
            FuncCtx {
                kind: Some(CodeKind::ClassInitializer),
                ..Default::default()
            },
            &analysis,
        )?;
        let cb = self.finish_function();
        Ok(self.add_function(cb))
    }
}
