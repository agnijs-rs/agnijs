//! Expressions. Every `expr*` leaves its result in acc.

use super::func::FuncCtx;
use super::*;
use agnijs_js_parser::number::to_js_string;

/// A compiled reference to a member target, for assignment.
pub(crate) enum MemberRef {
    Named { obj: Reg, name: NameIdx },
    Keyed { obj: Reg, key: Reg },
    Super { this: Reg, func: Reg, key: Reg },
    Private { obj: Reg, name: Reg },
}

impl<'a> Compiler<'a> {
    pub(crate) fn expr(&mut self, e: &Expression) -> CResult<()> {
        self.expr_named(e, NameHint::None)
    }

    pub(crate) fn expr_to_reg(&mut self, e: &Expression) -> CResult<Reg> {
        self.expr(e)?;
        let r = self.alloc();
        self.emit(Instr::Star(r));
        Ok(r)
    }

    pub(crate) fn expr_named(&mut self, e: &Expression, hint: NameHint) -> CResult<()> {
        self.set_pos(e.span());
        match e {
            Expression::Ident(id) => {
                self.emit_load_name(&id.name, id.span, false);
            }
            Expression::This(s) => self.emit_load_this(*s),
            Expression::Super(s) => return self.error("unexpected super", *s),
            Expression::Null(_) => {
                self.emit(Instr::LdaNull);
            }
            Expression::Bool(b, _) => {
                self.emit(if *b { Instr::LdaTrue } else { Instr::LdaFalse });
            }
            Expression::Number(n, _) => self.emit_load_number(*n),
            Expression::BigInt(b, _) => {
                let c = self.const_bigint(b);
                self.emit(Instr::LdaConst(c));
            }
            Expression::String(s) => self.emit_load_str(&s.value),
            Expression::RegExp(r) => {
                let p = self.const_str(&r.pattern);
                let f = self.const_str(&r.flags);
                self.emit(Instr::CreateRegExp {
                    pattern: p,
                    flags: f,
                });
            }
            Expression::Template(t) => self.template(t)?,
            Expression::TaggedTemplate(t) => self.tagged_template(t)?,
            Expression::Array(a) => self.array_literal(a)?,
            Expression::Object(o) => self.object_literal(o)?,
            Expression::Function(f) => {
                let name: Option<Rc<str>> = match (&f.id, hint) {
                    (Some(id), _) => Some(id.name.clone()),
                    (None, NameHint::Static(s)) => Some(Rc::from(s)),
                    _ => None,
                };
                let ctx = FuncCtx {
                    self_name: f.id.as_ref().map(|i| i.name.clone()),
                    ..Default::default()
                };
                let idx = self.compile_function_decl(f, name.as_deref(), ctx)?;
                self.set_pos(f.span);
                self.emit(Instr::CreateClosure(idx));
                if f.id.is_none() {
                    if let NameHint::Dynamic(r) = hint {
                        self.emit(Instr::SetFunctionName {
                            key: r,
                            prefix: NamePrefix::None,
                        });
                    }
                }
            }
            Expression::Arrow(a) => {
                let name = match hint {
                    NameHint::Static(s) => Some(s),
                    _ => None,
                };
                self.arrow_closure(a, name)?;
                if let NameHint::Dynamic(r) = hint {
                    self.emit(Instr::SetFunctionName {
                        key: r,
                        prefix: NamePrefix::None,
                    });
                }
            }
            Expression::Class(c) => {
                let hint = if c.id.is_some() { NameHint::None } else { hint };
                self.class(c, hint)?;
            }
            Expression::Unary(u) => self.unary(u)?,
            Expression::Update(u) => self.update(u)?,
            Expression::Binary(b) => self.binary(b)?,
            Expression::Logical(l) => self.logical(l)?,
            Expression::Assignment(a) => self.assignment(a)?,
            Expression::Conditional(c) => {
                self.expr(&c.test)?;
                let jf = self.emit_jump(Instr::JumpIfFalse(0));
                self.expr_named(&c.consequent, hint)?;
                let jend = self.emit_jump(Instr::Jump(0));
                self.patch(jf);
                self.expr_named(&c.alternate, hint)?;
                self.patch(jend);
            }
            Expression::Call(c) => self.call(c)?,
            Expression::New(n) => self.new_expr(n)?,
            Expression::Member(m) => self.member(m)?,
            Expression::OptionalChain(inner, _) => self.optional_chain(inner, false)?,
            Expression::Sequence(s) => {
                let n = s.expressions.len();
                for (i, x) in s.expressions.iter().enumerate() {
                    if i + 1 == n {
                        // A comma expression is not an anonymous function
                        // definition: `(0, function(){})` stays nameless.
                        self.expr(x)?;
                    } else {
                        self.expr(x)?;
                    }
                }
            }
            Expression::Yield(y) => self.yield_expr(y)?,
            Expression::Await(a) => {
                self.expr(&a.argument)?;
                self.set_pos(a.span);
                self.emit(Instr::Await);
            }
            Expression::MetaProperty(m) => match m.kind {
                MetaPropertyKind::NewTarget => self.emit_load_name(NEW_TARGET, m.span, false),
                MetaPropertyKind::ImportMeta => {
                    self.emit(Instr::ImportMeta);
                }
            },
            Expression::Import(i) => {
                let mark = self.mark();
                let spec = self.expr_to_reg(&i.source)?;
                let opts = self.alloc();
                match &i.options {
                    Some(o) => {
                        self.expr(o)?;
                    }
                    None => {
                        self.emit(Instr::LdaUndefined);
                    }
                }
                self.emit(Instr::Star(opts));
                self.set_pos(i.span);
                self.emit(Instr::DynamicImport {
                    spec,
                    options: opts,
                });
                self.free_to(mark);
            }
            Expression::Spread(s) => return self.error("unexpected spread", s.span),
            Expression::PrivateIn(p) => {
                let mark = self.mark();
                let name = self.private_name(&p.left);
                let obj = self.expr_to_reg(&p.right)?;
                self.set_pos(p.span);
                self.emit(Instr::HasPrivate { obj, name });
                self.free_to(mark);
            }
            Expression::Paren(inner, _) => {
                // Parenthesised anonymous functions still get names in
                // `x = (function(){})`.
                self.expr_named(inner, hint)?;
            }
        }
        Ok(())
    }

    // ---- literals --------------------------------------------------------

    fn template(&mut self, t: &TemplateLiteral) -> CResult<()> {
        let first = t.quasis[0]
            .cooked
            .clone()
            .unwrap_or_else(|| Rc::from(&[][..]));
        self.emit_load_str(&first);
        if t.expressions.is_empty() {
            return Ok(());
        }
        let mark = self.mark();
        let acc_reg = self.alloc();
        for (i, e) in t.expressions.iter().enumerate() {
            self.emit(Instr::Star(acc_reg));
            self.expr(e)?;
            self.emit(Instr::ToString);
            self.emit(Instr::Add(acc_reg));
            let q = &t.quasis[i + 1];
            if let Some(c) = &q.cooked {
                if !c.is_empty() {
                    self.emit(Instr::Star(acc_reg));
                    self.emit_load_str(c);
                    self.emit(Instr::Add(acc_reg));
                }
            }
        }
        self.free_to(mark);
        Ok(())
    }

    fn template_object(&mut self, t: &TemplateLiteral) -> ConstIdx {
        let cooked: Vec<Option<Rc<[u16]>>> = t.quasis.iter().map(|q| q.cooked.clone()).collect();
        let raw: Vec<Rc<[u16]>> = t
            .quasis
            .iter()
            .map(|q| Rc::from(q.raw.encode_utf16().collect::<Vec<u16>>()))
            .collect();
        let fs = self.fs();
        let i = fs.constants.len() as ConstIdx;
        fs.constants
            .push(Constant::Template(Rc::new(TemplateConstant {
                cooked,
                raw,
            })));
        i
    }

    fn tagged_template(&mut self, t: &TaggedTemplate) -> CResult<()> {
        let mark = self.mark();
        let (callee, this) = self.callee_and_this(&t.tag)?;
        let argc = (t.quasi.expressions.len() + 1) as u16;
        let args = self.alloc_n(argc);
        let tmpl = self.template_object(&t.quasi);
        self.emit(Instr::GetTemplateObject(tmpl));
        self.emit(Instr::Star(args));
        for (i, e) in t.quasi.expressions.iter().enumerate() {
            self.expr(e)?;
            self.emit(Instr::Star(args + 1 + i as u16));
        }
        self.set_pos(t.span);
        self.emit(Instr::Call {
            callee,
            this,
            args,
            argc,
        });
        self.free_to(mark);
        Ok(())
    }

    fn array_literal(&mut self, a: &ArrayExpression) -> CResult<()> {
        let mark = self.mark();
        let arr = self.alloc();
        self.emit(Instr::CreateArray);
        self.emit(Instr::Star(arr));
        for el in &a.elements {
            match el {
                None => {
                    self.emit(Instr::ArrayPushHole { arr });
                }
                Some(Expression::Spread(s)) => {
                    self.expr(&s.argument)?;
                    self.set_pos(s.span);
                    self.emit(Instr::ArraySpread { arr });
                }
                Some(e) => {
                    self.expr(e)?;
                    self.emit(Instr::ArrayPush { arr });
                }
            }
        }
        self.emit(Instr::Ldar(arr));
        self.free_to(mark);
        Ok(())
    }

    /// Evaluate a property key into a register holding a property key
    /// value. Static keys become string constants.
    pub(crate) fn property_key_reg(&mut self, key: &PropertyKey) -> CResult<Reg> {
        let r = self.alloc();
        match key {
            PropertyKey::Computed(e) => {
                self.expr(e)?;
                self.emit(Instr::ToPropertyKey);
            }
            PropertyKey::Private(p) => {
                let n = private_binding_name(&p.name);
                self.emit_load_name(&n, p.span, false);
            }
            other => {
                let name = static_key_name(other).unwrap();
                self.emit_load_str(&name);
            }
        }
        self.emit(Instr::Star(r));
        Ok(r)
    }

    fn object_literal(&mut self, o: &ObjectExpression) -> CResult<()> {
        let mark = self.mark();
        let obj = self.alloc();
        self.emit(Instr::CreateObject);
        self.emit(Instr::Star(obj));
        for p in &o.properties {
            match p {
                ObjectProperty::Spread(s) => {
                    self.expr(&s.argument)?;
                    self.set_pos(s.span);
                    self.emit(Instr::CopyDataProperties {
                        target: obj,
                        excluded: 0,
                        count: 0,
                    });
                }
                ObjectProperty::Property(p) => {
                    self.set_pos(p.span);
                    if p.is_proto_setter {
                        self.expr(&p.value)?;
                        self.emit(Instr::SetPrototypeLiteral { obj });
                        continue;
                    }
                    let static_name = static_key_name(&p.key);
                    match p.kind {
                        PropertyKind::Init => {
                            if let Some(name) = &static_name {
                                if Self::utf16_is_str(name) {
                                    let s = utf16_to_string(name);
                                    self.expr_named(&p.value, NameHint::Static(&s))?;
                                    let n = self.name(&s);
                                    self.emit(Instr::DefineOwnNamed { obj, name: n });
                                    continue;
                                }
                            }
                            let m2 = self.mark();
                            let key = self.property_key_reg(&p.key)?;
                            self.expr_named(&p.value, NameHint::Dynamic(key))?;
                            self.emit(Instr::DefineOwn { obj, key });
                            self.free_to(m2);
                        }
                        PropertyKind::Method | PropertyKind::Get | PropertyKind::Set => {
                            let m2 = self.mark();
                            let key = self.property_key_reg(&p.key)?;
                            let f = match &p.value {
                                Expression::Function(f) => f,
                                _ => unreachable!("method value is a function"),
                            };
                            self.method_closure(f, None, p.span)?;
                            match p.kind {
                                PropertyKind::Method => self.emit(Instr::DefineMethod {
                                    obj,
                                    key,
                                    enumerable: true,
                                }),
                                PropertyKind::Get => self.emit(Instr::DefineGetter {
                                    obj,
                                    key,
                                    enumerable: true,
                                }),
                                _ => self.emit(Instr::DefineSetter {
                                    obj,
                                    key,
                                    enumerable: true,
                                }),
                            };
                            self.free_to(m2);
                        }
                    }
                }
            }
        }
        self.emit(Instr::Ldar(obj));
        self.free_to(mark);
        Ok(())
    }

    // ---- operators -------------------------------------------------------

    fn unary(&mut self, u: &UnaryExpression) -> CResult<()> {
        match u.op {
            UnaryOp::Typeof => {
                if let Expression::Ident(id) = u.argument.unparen() {
                    self.emit_load_name(&id.name, id.span, true);
                } else {
                    self.expr(&u.argument)?;
                }
                self.set_pos(u.span);
                self.emit(Instr::Typeof);
            }
            UnaryOp::Delete => self.delete(&u.argument, u.span)?,
            UnaryOp::Void => {
                self.expr(&u.argument)?;
                self.emit(Instr::LdaUndefined);
            }
            UnaryOp::Minus => {
                self.expr(&u.argument)?;
                self.set_pos(u.span);
                self.emit(Instr::Neg);
            }
            UnaryOp::Plus => {
                self.expr(&u.argument)?;
                self.set_pos(u.span);
                self.emit(Instr::Pos);
            }
            UnaryOp::Not => {
                self.expr(&u.argument)?;
                self.emit(Instr::Not);
            }
            UnaryOp::BitNot => {
                self.expr(&u.argument)?;
                self.set_pos(u.span);
                self.emit(Instr::BitNot);
            }
        }
        Ok(())
    }

    fn delete(&mut self, arg: &Expression, span: Span) -> CResult<()> {
        let strict = self.fs_ref().strict;
        match arg {
            Expression::Paren(inner, _) if !matches!(inner.unparen(), Expression::Ident(_)) => {
                return self.delete(inner, span);
            }
            Expression::Ident(id) => {
                // `delete (x)` is also a delete of a reference.
                match self.resolve(&id.name) {
                    Resolved::Reg { .. } | Resolved::Env { .. } | Resolved::Import(_) => {
                        self.emit(Instr::LdaFalse);
                    }
                    Resolved::Global(n) | Resolved::Dynamic(n) => {
                        let n = self.name_rc(&n);
                        self.set_pos(span);
                        self.emit(Instr::DeleteDynamic(n));
                    }
                }
            }
            Expression::Paren(inner, _) => return self.delete(inner, span),
            Expression::Member(m) => {
                let mark = self.mark();
                match &m.property {
                    _ if matches!(m.object.unparen(), Expression::Super(_)) => {
                        // `delete super.x`: evaluate `this`, then throw.
                        self.emit_load_this(m.span);
                        if let MemberProperty::Computed(c) = &m.property {
                            self.expr(c)?;
                        }
                        self.set_pos(span);
                        self.emit_throw_error(
                            ErrorKind::Reference,
                            "cannot delete a super property",
                        );
                    }
                    MemberProperty::Ident(id) => {
                        let obj = self.expr_to_reg(&m.object)?;
                        let n = self.name_rc(&id.name);
                        self.set_pos(span);
                        self.emit(Instr::DeleteNamed {
                            obj,
                            name: n,
                            strict,
                        });
                    }
                    MemberProperty::Computed(c) => {
                        let obj = self.expr_to_reg(&m.object)?;
                        let key = self.expr_to_reg(c)?;
                        self.set_pos(span);
                        self.emit(Instr::DeleteKeyed { obj, key, strict });
                    }
                    MemberProperty::Private(p) => {
                        return self.error("cannot delete a private member", p.span)
                    }
                }
                self.free_to(mark);
            }
            Expression::OptionalChain(inner, _) => {
                // `delete a?.b` is true when the chain short-circuits.
                self.optional_chain_delete(inner, span)?;
            }
            other => {
                self.expr(other)?;
                self.emit(Instr::LdaTrue);
            }
        }
        Ok(())
    }

    fn update(&mut self, u: &UpdateExpression) -> CResult<()> {
        let mark = self.mark();
        let op = if u.increment { Instr::Inc } else { Instr::Dec };
        match u.argument.unparen() {
            Expression::Ident(id) => {
                let r = self.dynamic_ref(&id.name);
                self.emit_load_ref(&id.name, id.span, r);
                self.set_pos(u.span);
                self.emit(Instr::ToNumeric);
                let old = self.alloc();
                if !u.prefix {
                    self.emit(Instr::Star(old));
                }
                self.emit(op);
                self.emit_store_ref(&id.name, id.span, r);
                if !u.prefix {
                    self.emit(Instr::Ldar(old));
                }
            }
            Expression::Member(m) => {
                let r = self.member_ref(m)?;
                self.emit_member_key_once(&r);
                self.emit_member_get(&r);
                self.set_pos(u.span);
                self.emit(Instr::ToNumeric);
                let old = self.alloc();
                if !u.prefix {
                    self.emit(Instr::Star(old));
                }
                self.emit(op);
                self.emit_member_set(&r);
                if !u.prefix {
                    self.emit(Instr::Ldar(old));
                }
            }
            Expression::Call(c) => {
                // Annex B: a runtime ReferenceError after evaluating the call.
                self.call(c)?;
                self.set_pos(u.span);
                self.emit_throw_error(ErrorKind::Reference, "invalid update target");
            }
            _ => return self.error("invalid update target", u.span),
        }
        self.free_to(mark);
        Ok(())
    }

    fn binary(&mut self, b: &BinaryExpression) -> CResult<()> {
        let mark = self.mark();
        let l = self.expr_to_reg(&b.left)?;
        self.expr(&b.right)?;
        self.set_pos(b.span);
        self.emit(binary_instr(b.op, l));
        self.free_to(mark);
        Ok(())
    }

    fn logical(&mut self, l: &LogicalExpression) -> CResult<()> {
        self.expr(&l.left)?;
        let j = match l.op {
            LogicalOp::Or => self.emit_jump(Instr::JumpIfTrue(0)),
            LogicalOp::And => self.emit_jump(Instr::JumpIfFalse(0)),
            LogicalOp::Nullish => self.emit_jump(Instr::JumpIfNotNullish(0)),
        };
        self.expr(&l.right)?;
        self.patch(j);
        Ok(())
    }

    // ---- member references -------------------------------------------

    /// Evaluate the object (and key) of a member expression into registers
    /// without reading the property.
    pub(crate) fn member_ref(&mut self, m: &MemberExpression) -> CResult<MemberRef> {
        if matches!(m.object.unparen(), Expression::Super(_)) {
            let this = self.alloc();
            self.emit_load_this(m.span);
            self.emit(Instr::Star(this));
            let func = self.alloc();
            self.emit_load_name(FUNC, m.span, false);
            self.emit(Instr::Star(func));
            let key = self.alloc();
            match &m.property {
                MemberProperty::Ident(id) => {
                    let s: Vec<u16> = id.name.encode_utf16().collect();
                    self.emit_load_str(&s);
                }
                MemberProperty::Computed(c) => {
                    // ToPropertyKey happens in GetSuper/SetSuper, after the
                    // right-hand side of an assignment.
                    self.expr(c)?;
                }
                MemberProperty::Private(p) => return self.error("unexpected private name", p.span),
            }
            self.emit(Instr::Star(key));
            return Ok(MemberRef::Super { this, func, key });
        }
        let obj = self.expr_to_reg(&m.object)?;
        if m.optional {
            self.emit(Instr::Ldar(obj));
            self.optional_exit();
        }
        Ok(match &m.property {
            MemberProperty::Ident(id) => {
                let name = self.name_rc(&id.name);
                MemberRef::Named { obj, name }
            }
            MemberProperty::Computed(c) => {
                let key = self.expr_to_reg(c)?;
                MemberRef::Keyed { obj, key }
            }
            MemberProperty::Private(p) => {
                let name = self.private_name(p);
                MemberRef::Private { obj, name }
            }
        })
    }

    /// For compound and logical assignment: convert a computed key once so
    /// the get and the set see the same property key.
    pub(crate) fn emit_member_key_once(&mut self, r: &MemberRef) {
        match *r {
            MemberRef::Keyed { obj, key } => {
                self.emit(Instr::Ldar(obj));
                self.emit(Instr::RequireObjectCoercible);
                self.emit(Instr::Ldar(key));
                self.emit(Instr::ToPropertyKey);
                self.emit(Instr::Star(key));
            }
            // GetSuper/SetSuper capture the base before converting the key.
            _ => {}
        }
    }

    /// A name resolved through the runtime environment chain: the
    /// reference is taken before the right-hand side is evaluated.
    pub(crate) fn dynamic_ref(&mut self, name: &str) -> Option<(NameIdx, Reg)> {
        match self.resolve(name) {
            Resolved::Dynamic(n) => {
                let n = self.name_rc(&n);
                let dst = self.alloc();
                self.emit(Instr::ResolveDynamic { name: n, dst });
                Some((n, dst))
            }
            _ => None,
        }
    }

    /// The reference for a plain assignment target. Besides dynamic names,
    /// strict code resolves a global name before the right-hand side runs,
    /// so a binding the right-hand side creates does not rescue an
    /// unresolvable reference (PutValue throws a ReferenceError).
    pub(crate) fn assign_ref(&mut self, name: &str) -> Option<(NameIdx, Reg)> {
        if let Some(r) = self.dynamic_ref(name) {
            return Some(r);
        }
        if self.fs_ref().strict {
            if let Resolved::Global(n) = self.resolve(name) {
                let n = self.name_rc(&n);
                let dst = self.alloc();
                self.emit(Instr::ResolveDynamic { name: n, dst });
                return Some((n, dst));
            }
        }
        None
    }

    fn emit_load_ref(&mut self, name: &str, span: Span, r: Option<(NameIdx, Reg)>) {
        match r {
            Some((n, reference)) => {
                self.set_pos(span);
                self.emit(Instr::LdaDynamicRef { name: n, reference });
            }
            None => self.emit_load_name(name, span, false),
        }
    }

    pub(crate) fn emit_store_ref(&mut self, name: &str, span: Span, r: Option<(NameIdx, Reg)>) {
        match r {
            Some((n, reference)) => {
                let strict = self.fs_ref().strict;
                self.set_pos(span);
                self.emit(Instr::StaDynamicRef {
                    name: n,
                    reference,
                    strict,
                });
            }
            None => self.emit_store_name(name, span, false),
        }
    }

    pub(crate) fn emit_member_get(&mut self, r: &MemberRef) {
        match *r {
            MemberRef::Named { obj, name } => {
                self.emit_get_named(obj, name);
                return;
            }
            MemberRef::Keyed { obj, key } => self.emit(Instr::GetKeyed { obj, key }),
            MemberRef::Super { this, func, key } => self.emit(Instr::GetSuper { this, func, key }),
            MemberRef::Private { obj, name } => self.emit(Instr::GetPrivate { obj, name }),
        };
    }

    pub(crate) fn emit_member_set(&mut self, r: &MemberRef) {
        let strict = self.fs_ref().strict;
        match *r {
            MemberRef::Named { obj, name } => {
                self.emit_set_named(obj, name, strict);
                return;
            }
            MemberRef::Keyed { obj, key } => self.emit(Instr::SetKeyed { obj, key, strict }),
            MemberRef::Super { this, func, key } => self.emit(Instr::SetSuper {
                this,
                func,
                key,
                strict,
            }),
            MemberRef::Private { obj, name } => self.emit(Instr::SetPrivate { obj, name }),
        };
    }

    fn member(&mut self, m: &MemberExpression) -> CResult<()> {
        let mark = self.mark();
        let r = self.member_ref(m)?;
        self.set_pos(m.span);
        self.emit_member_get(&r);
        self.free_to(mark);
        Ok(())
    }

    // ---- optional chains ---------------------------------------------

    /// Emit a short-circuit jump for the innermost optional chain (acc is
    /// the value to test).
    pub(crate) fn optional_exit(&mut self) {
        let j = self.emit_jump(Instr::JumpIfNullish(0));
        match self.fs().optional_exits.last_mut() {
            Some(v) => v.push(j),
            None => panic!("optional link outside a chain"),
        }
    }

    fn optional_chain(&mut self, inner: &Expression, _for_delete: bool) -> CResult<()> {
        self.fs().optional_exits.push(Vec::new());
        self.expr(inner)?;
        let exits = self.fs().optional_exits.pop().unwrap();
        if !exits.is_empty() {
            let jend = self.emit_jump(Instr::Jump(0));
            self.patch_all(exits);
            self.emit(Instr::LdaUndefined);
            self.patch(jend);
        }
        Ok(())
    }

    fn optional_chain_delete(&mut self, inner: &Expression, span: Span) -> CResult<()> {
        self.fs().optional_exits.push(Vec::new());
        self.delete(inner, span)?;
        let exits = self.fs().optional_exits.pop().unwrap();
        if !exits.is_empty() {
            let jend = self.emit_jump(Instr::Jump(0));
            self.patch_all(exits);
            self.emit(Instr::LdaTrue);
            self.patch(jend);
        }
        Ok(())
    }

    // ---- calls -----------------------------------------------------------

    /// Evaluate a callee into (callee, this) registers.
    fn callee_and_this(&mut self, callee: &Expression) -> CResult<(Reg, Reg)> {
        match callee.unparen() {
            Expression::Member(m) if !matches!(callee, Expression::Paren(..)) || true => {
                let r = self.member_ref(m)?;
                let this = match &r {
                    MemberRef::Named { obj, .. }
                    | MemberRef::Keyed { obj, .. }
                    | MemberRef::Private { obj, .. } => *obj,
                    MemberRef::Super { this, .. } => *this,
                };
                self.set_pos(m.span);
                self.emit_member_get(&r);
                let f = self.alloc();
                self.emit(Instr::Star(f));
                Ok((f, this))
            }
            Expression::OptionalChain(inner, _)
                if matches!(inner.unparen(), Expression::Member(_)) =>
            {
                // `(a?.b)()`: the chain short-circuits to undefined (and
                // the call then throws), otherwise `this` is the base.
                let Expression::Member(m) = inner.unparen() else {
                    unreachable!()
                };
                self.fs().optional_exits.push(Vec::new());
                let r = self.member_ref(m)?;
                let this = match &r {
                    MemberRef::Named { obj, .. }
                    | MemberRef::Keyed { obj, .. }
                    | MemberRef::Private { obj, .. } => *obj,
                    MemberRef::Super { this, .. } => *this,
                };
                self.set_pos(m.span);
                self.emit_member_get(&r);
                let exits = self.fs().optional_exits.pop().unwrap();
                if !exits.is_empty() {
                    let jend = self.emit_jump(Instr::Jump(0));
                    self.patch_all(exits);
                    self.emit(Instr::LdaUndefined);
                    self.patch(jend);
                }
                let f = self.alloc();
                self.emit(Instr::Star(f));
                Ok((f, this))
            }
            Expression::Ident(id) => {
                let f = self.alloc();
                let this = self.alloc();
                match self.resolve(&id.name) {
                    Resolved::Dynamic(n) => {
                        let n = self.name_rc(&n);
                        self.emit(Instr::ResolveDynamic { name: n, dst: this });
                        self.set_pos(id.span);
                        self.emit(Instr::LdaDynamicRef {
                            name: n,
                            reference: this,
                        });
                        self.emit(Instr::Star(f));
                        self.emit(Instr::LdaRefThis(this));
                        self.emit(Instr::Star(this));
                    }
                    _ => {
                        self.emit_load_name(&id.name, id.span, false);
                        self.emit(Instr::Star(f));
                        self.emit(Instr::LdaUndefined);
                        self.emit(Instr::Star(this));
                    }
                }
                Ok((f, this))
            }
            other => {
                let f = self.expr_to_reg(other)?;
                let this = self.alloc();
                self.emit(Instr::LdaUndefined);
                self.emit(Instr::Star(this));
                Ok((f, this))
            }
        }
    }

    /// Evaluate arguments. Returns `Ok((base, argc))` for positional
    /// arguments or `Err(array_reg)` when a spread forced an array.
    fn arguments(&mut self, args: &[Expression]) -> CResult<Result<(Reg, u16), Reg>> {
        let has_spread = args.iter().any(|a| matches!(a, Expression::Spread(_)));
        if !has_spread {
            let argc = args.len() as u16;
            let base = self.alloc_n(argc);
            for (i, a) in args.iter().enumerate() {
                self.expr(a)?;
                self.emit(Instr::Star(base + i as u16));
            }
            return Ok(Ok((base, argc)));
        }
        let arr = self.alloc();
        self.emit(Instr::CreateArray);
        self.emit(Instr::Star(arr));
        for a in args {
            match a {
                Expression::Spread(s) => {
                    self.expr(&s.argument)?;
                    self.set_pos(s.span);
                    self.emit(Instr::ArraySpread { arr });
                }
                e => {
                    self.expr(e)?;
                    self.emit(Instr::ArrayPush { arr });
                }
            }
        }
        Ok(Err(arr))
    }

    pub(crate) fn call(&mut self, c: &CallExpression) -> CResult<()> {
        let mark = self.mark();
        // super(...)
        if let Expression::Super(s) = &c.callee {
            let func = self.alloc();
            self.emit_load_name(FUNC, *s, false);
            self.emit(Instr::Star(func));
            let nt = self.alloc();
            self.emit_load_name(NEW_TARGET, *s, false);
            self.emit(Instr::Star(nt));
            let args = self.arguments(&c.arguments)?;
            self.set_pos(c.span);
            match args {
                Ok((base, argc)) => self.emit(Instr::SuperCall {
                    func,
                    new_target: nt,
                    args: base,
                    argc,
                }),
                Err(arr) => self.emit(Instr::SuperCallSpread {
                    func,
                    new_target: nt,
                    args: arr,
                }),
            };
            // Bind `this`: a second super() call is a ReferenceError.
            let result = self.alloc();
            self.emit(Instr::Star(result));
            match self.resolve(THIS) {
                Resolved::Reg { reg, .. } => self.emit(Instr::Ldar(reg)),
                Resolved::Env { depth, slot, .. } => self.emit(Instr::LdaEnv { depth, slot }),
                Resolved::Dynamic(_) => {
                    let n = self.name(THIS);
                    self.emit(Instr::LdaDynamic(n))
                }
                _ => self.emit(Instr::LdaUndefined),
            };
            let ok = self.emit_jump(Instr::JumpIfEmpty(0));
            self.emit_throw_error(ErrorKind::Reference, "super() has already been called");
            self.patch(ok);
            self.emit(Instr::Ldar(result));
            self.emit_store_name(THIS, c.span, true);
            self.emit(Instr::InitializeInstanceElements { func, this: result });
            self.emit(Instr::Ldar(result));
            self.free_to(mark);
            return Ok(());
        }
        let is_direct_eval =
            !c.optional && matches!(&c.callee, Expression::Ident(id) if &*id.name == "eval");
        let (callee, this) = self.callee_and_this(&c.callee)?;
        if c.optional {
            self.emit(Instr::Ldar(callee));
            self.optional_exit();
        }
        let args = self.arguments(&c.arguments)?;
        self.set_pos(c.span);
        let strict = self.fs_ref().strict;
        match (args, is_direct_eval) {
            (Ok((base, argc)), false) => self.emit(Instr::Call {
                callee,
                this,
                args: base,
                argc,
            }),
            (Err(arr), false) => self.emit(Instr::CallSpread {
                callee,
                this,
                args: arr,
            }),
            (Ok((base, argc)), true) => self.emit(Instr::CallEval {
                callee,
                this,
                args: base,
                argc,
                strict,
            }),
            (Err(arr), true) => self.emit(Instr::CallEvalSpread {
                callee,
                this,
                args: arr,
                strict,
            }),
        };
        self.free_to(mark);
        Ok(())
    }

    fn new_expr(&mut self, n: &NewExpression) -> CResult<()> {
        let mark = self.mark();
        let callee = self.expr_to_reg(&n.callee)?;
        let args = self.arguments(&n.arguments)?;
        self.set_pos(n.span);
        match args {
            Ok((base, argc)) => self.emit(Instr::Construct {
                callee,
                args: base,
                argc,
            }),
            Err(arr) => self.emit(Instr::ConstructSpread { callee, args: arr }),
        };
        self.free_to(mark);
        Ok(())
    }

    // ---- assignment ----------------------------------------------------

    fn assignment(&mut self, a: &AssignmentExpression) -> CResult<()> {
        let mark = self.mark();
        match a.op {
            AssignOp::Assign => match &a.left {
                Pattern::Ident(id) => {
                    let r = self.assign_ref(&id.name);
                    self.expr_named(&a.right, NameHint::Static(&id.name))?;
                    self.emit_store_ref(&id.name, id.span, r);
                }
                Pattern::Expression(e) => match e.unparen() {
                    Expression::Member(m) => {
                        let r = self.member_ref(m)?;
                        self.expr(&a.right)?;
                        self.set_pos(a.span);
                        self.emit_member_set(&r);
                    }
                    Expression::Ident(id) => {
                        // `(x) = function(){}` is not an IdentifierRef:
                        // no name inference through parentheses.
                        let hint = if matches!(&**e, Expression::Paren(..)) {
                            NameHint::None
                        } else {
                            NameHint::Static(&id.name)
                        };
                        let r = self.assign_ref(&id.name);
                        self.expr_named(&a.right, hint)?;
                        self.emit_store_ref(&id.name, id.span, r);
                    }
                    Expression::Call(c) => {
                        // Annex B: a runtime ReferenceError after evaluating
                        // the call.
                        self.call(c)?;
                        self.set_pos(a.span);
                        self.emit_throw_error(ErrorKind::Reference, "invalid assignment target");
                    }
                    _ => return self.error("invalid assignment target", e.span()),
                },
                pat => {
                    self.expr(&a.right)?;
                    let v = self.alloc();
                    self.emit(Instr::Star(v));
                    self.assign_pattern(pat)?;
                    self.emit(Instr::Ldar(v));
                }
            },
            AssignOp::Or | AssignOp::And | AssignOp::Nullish => {
                let jump = |c: &mut Self| match a.op {
                    AssignOp::Or => c.emit_jump(Instr::JumpIfTrue(0)),
                    AssignOp::And => c.emit_jump(Instr::JumpIfFalse(0)),
                    _ => c.emit_jump(Instr::JumpIfNotNullish(0)),
                };
                match &a.left {
                    Pattern::Ident(id) => {
                        let r = self.dynamic_ref(&id.name);
                        self.emit_load_ref(&id.name, id.span, r);
                        let j = jump(self);
                        self.expr_named(&a.right, NameHint::Static(&id.name))?;
                        self.emit_store_ref(&id.name, id.span, r);
                        self.patch(j);
                    }
                    Pattern::Expression(e) => match e.unparen() {
                        Expression::Member(m) => {
                            let r = self.member_ref(m)?;
                            self.emit_member_key_once(&r);
                            self.emit_member_get(&r);
                            let j = jump(self);
                            self.expr(&a.right)?;
                            self.set_pos(a.span);
                            self.emit_member_set(&r);
                            self.patch(j);
                        }
                        Expression::Ident(id) => {
                            let hint = if matches!(&**e, Expression::Paren(..)) {
                                NameHint::None
                            } else {
                                NameHint::Static(&id.name)
                            };
                            let r = self.dynamic_ref(&id.name);
                            self.emit_load_ref(&id.name, id.span, r);
                            let j = jump(self);
                            self.expr_named(&a.right, hint)?;
                            self.emit_store_ref(&id.name, id.span, r);
                            self.patch(j);
                        }
                        _ => return self.error("invalid assignment target", e.span()),
                    },
                    _ => return self.error("invalid assignment target", a.span),
                }
            }
            op => {
                let bin = compound_op(op);
                match &a.left {
                    Pattern::Ident(id) => {
                        let r = self.dynamic_ref(&id.name);
                        self.emit_load_ref(&id.name, id.span, r);
                        let tmp = self.alloc();
                        self.emit(Instr::Star(tmp));
                        self.expr(&a.right)?;
                        self.set_pos(a.span);
                        self.emit(binary_instr(bin, tmp));
                        self.emit_store_ref(&id.name, id.span, r);
                    }
                    Pattern::Expression(e) => match e.unparen() {
                        Expression::Member(m) => {
                            let r = self.member_ref(m)?;
                            self.emit_member_key_once(&r);
                            self.emit_member_get(&r);
                            let tmp = self.alloc();
                            self.emit(Instr::Star(tmp));
                            self.expr(&a.right)?;
                            self.set_pos(a.span);
                            self.emit(binary_instr(bin, tmp));
                            self.emit_member_set(&r);
                        }
                        Expression::Call(c) => {
                            // Annex B: evaluate the call, then throw before
                            // touching the right-hand side.
                            self.call(c)?;
                            self.set_pos(a.span);
                            self.emit_throw_error(
                                ErrorKind::Reference,
                                "invalid assignment target",
                            );
                        }
                        Expression::Ident(id) => {
                            let r = self.dynamic_ref(&id.name);
                            self.emit_load_ref(&id.name, id.span, r);
                            let tmp = self.alloc();
                            self.emit(Instr::Star(tmp));
                            self.expr(&a.right)?;
                            self.set_pos(a.span);
                            self.emit(binary_instr(bin, tmp));
                            self.emit_store_ref(&id.name, id.span, r);
                        }
                        _ => return self.error("invalid assignment target", e.span()),
                    },
                    _ => return self.error("invalid assignment target", a.span),
                }
            }
        }
        self.free_to(mark);
        Ok(())
    }

    // ---- generators --------------------------------------------------------

    fn yield_expr(&mut self, y: &YieldExpression) -> CResult<()> {
        let is_async = self.fs_ref().function_kind.is_async();
        if y.delegate {
            return self.yield_delegate(y.argument.as_ref().unwrap(), is_async, y.span);
        }
        match &y.argument {
            Some(a) => self.expr(a)?,
            None => {
                self.emit(Instr::LdaUndefined);
            }
        }
        self.set_pos(y.span);
        if is_async {
            self.emit(Instr::Await);
        }
        let mark = self.mark();
        let kind = self.alloc();
        self.emit(Instr::Yield { kind });
        self.emit_resume_dispatch(kind, is_async);
        self.free_to(mark);
        Ok(())
    }

    /// After a `Yield`: handle a return resumption (throw resumptions are
    /// thrown by the VM itself).
    fn emit_resume_dispatch(&mut self, kind: Reg, is_async: bool) {
        let d = self.emit_jump(Instr::ResumeDispatch { kind, on_return: 0 });
        let cont = self.emit_jump(Instr::Jump(0));
        self.patch(d);
        if is_async {
            self.emit(Instr::Await);
        }
        self.emit_return();
        self.patch(cont);
    }

    fn yield_delegate(&mut self, arg: &Expression, is_async: bool, span: Span) -> CResult<()> {
        let mark = self.mark();
        self.expr(arg)?;
        self.set_pos(span);
        let iter = self.alloc();
        let next = self.alloc();
        if is_async {
            self.emit(Instr::GetAsyncIterator { iter, next });
        } else {
            self.emit(Instr::GetIterator { iter, next });
        }
        let received = self.alloc();
        let mode = self.alloc();
        let method = self.alloc();
        let result = self.alloc();
        self.emit(Instr::LdaUndefined);
        self.emit(Instr::Star(received));
        self.emit(Instr::LdaInt(RESUME_NEXT));
        self.emit(Instr::Star(mode));
        let top = self.pc();
        if is_async {
            // AsyncGeneratorYield: a return resumption awaits its value
            // before yield* sees it; a rejection becomes a throw resumption.
            self.emit(Instr::LdaInt(RESUME_RETURN));
            let do_await = self.emit_jump(Instr::JumpIfStrictEq {
                reg: mode,
                offset: 0,
            });
            let skip = self.emit_jump(Instr::Jump(0));
            self.patch(do_await);
            let cs = self.pc();
            self.emit(Instr::Ldar(received));
            self.emit(Instr::Await);
            let ce = self.pc();
            self.emit(Instr::Star(received));
            let after = self.emit_jump(Instr::Jump(0));
            let handler = self.pc();
            let depth = self.env_depth();
            self.add_handler(cs, ce, handler, depth);
            self.emit(Instr::Star(received));
            self.emit(Instr::LdaInt(RESUME_THROW));
            self.emit(Instr::Star(mode));
            self.patch(after);
            self.patch(skip);
        }
        // Dispatch on the resumption mode.
        self.emit(Instr::LdaInt(RESUME_NEXT));
        let to_next = self.emit_jump(Instr::JumpIfStrictEq {
            reg: mode,
            offset: 0,
        });
        self.emit(Instr::LdaInt(RESUME_THROW));
        let to_throw = self.emit_jump(Instr::JumpIfStrictEq {
            reg: mode,
            offset: 0,
        });
        // Return.
        let n_return = self.name("return");
        self.emit_get_named(iter, n_return);
        let no_return = self.emit_jump(Instr::JumpIfNullish(0));
        self.emit(Instr::Star(method));
        self.emit(Instr::Call {
            callee: method,
            this: iter,
            args: received,
            argc: 1,
        });
        let to_result_1 = self.emit_jump(Instr::Jump(0));
        self.patch(no_return);
        self.emit(Instr::Ldar(received));
        if is_async {
            self.emit(Instr::Await);
        }
        self.emit_return();
        // Throw.
        self.patch(to_throw);
        let n_throw = self.name("throw");
        self.emit_get_named(iter, n_throw);
        let no_throw = self.emit_jump(Instr::JumpIfNullish(0));
        self.emit(Instr::Star(method));
        self.emit(Instr::Call {
            callee: method,
            this: iter,
            args: received,
            argc: 1,
        });
        let to_result_2 = self.emit_jump(Instr::Jump(0));
        self.patch(no_throw);
        self.emit_iterator_close(iter, is_async);
        self.emit_throw_error(
            ErrorKind::Type,
            "the iterator does not provide a 'throw' method",
        );
        // Next.
        self.patch(to_next);
        self.emit(Instr::Call {
            callee: next,
            this: iter,
            args: received,
            argc: 1,
        });
        self.patch(to_result_1);
        self.patch(to_result_2);
        if is_async {
            self.emit(Instr::Await);
        }
        let c = self.const_str("iterator result is not an object");
        self.emit(Instr::ThrowIfNotObject(c));
        self.emit(Instr::Star(result));
        let n_done = self.name("done");
        self.emit_get_named(result, n_done);
        let done = self.emit_jump(Instr::JumpIfTrue(0));
        if is_async {
            let n_value = self.name("value");
            self.emit_get_named(result, n_value);
            self.emit(Instr::Yield { kind: mode });
        } else {
            self.emit(Instr::Ldar(result));
            self.emit(Instr::YieldRaw { kind: mode });
        }
        self.emit(Instr::Star(received));
        self.emit_jump_to(top);
        // Done.
        self.patch(done);
        let n_value = self.name("value");
        self.emit(Instr::LdaInt(RESUME_RETURN));
        let is_return = self.emit_jump(Instr::JumpIfStrictEq {
            reg: mode,
            offset: 0,
        });
        self.emit_get_named(result, n_value);
        let end = self.emit_jump(Instr::Jump(0));
        self.patch(is_return);
        self.emit_get_named(result, n_value);
        if is_async {
            self.emit(Instr::Await);
        }
        self.emit_return();
        self.patch(end);
        self.free_to(mark);
        Ok(())
    }
}

pub(crate) fn binary_instr(op: BinaryOp, l: Reg) -> Instr {
    match op {
        BinaryOp::EqEq => Instr::Eq(l),
        BinaryOp::NotEq => Instr::Ne(l),
        BinaryOp::EqEqEq => Instr::StrictEq(l),
        BinaryOp::NotEqEq => Instr::StrictNe(l),
        BinaryOp::Lt => Instr::Lt(l),
        BinaryOp::LtEq => Instr::Le(l),
        BinaryOp::Gt => Instr::Gt(l),
        BinaryOp::GtEq => Instr::Ge(l),
        BinaryOp::Shl => Instr::Shl(l),
        BinaryOp::Shr => Instr::Shr(l),
        BinaryOp::UShr => Instr::UShr(l),
        BinaryOp::Add => Instr::Add(l),
        BinaryOp::Sub => Instr::Sub(l),
        BinaryOp::Mul => Instr::Mul(l),
        BinaryOp::Div => Instr::Div(l),
        BinaryOp::Mod => Instr::Mod(l),
        BinaryOp::Exp => Instr::Exp(l),
        BinaryOp::BitOr => Instr::BitOr(l),
        BinaryOp::BitXor => Instr::BitXor(l),
        BinaryOp::BitAnd => Instr::BitAnd(l),
        BinaryOp::In => Instr::In(l),
        BinaryOp::Instanceof => Instr::InstanceOf(l),
    }
}

fn compound_op(op: AssignOp) -> BinaryOp {
    match op {
        AssignOp::Add => BinaryOp::Add,
        AssignOp::Sub => BinaryOp::Sub,
        AssignOp::Mul => BinaryOp::Mul,
        AssignOp::Div => BinaryOp::Div,
        AssignOp::Mod => BinaryOp::Mod,
        AssignOp::Exp => BinaryOp::Exp,
        AssignOp::Shl => BinaryOp::Shl,
        AssignOp::Shr => BinaryOp::Shr,
        AssignOp::UShr => BinaryOp::UShr,
        AssignOp::BitOr => BinaryOp::BitOr,
        AssignOp::BitXor => BinaryOp::BitXor,
        AssignOp::BitAnd => BinaryOp::BitAnd,
        _ => unreachable!(),
    }
}

#[allow(dead_code)]
pub(crate) fn number_key(n: f64) -> String {
    to_js_string(n)
}
