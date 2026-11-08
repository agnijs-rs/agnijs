//! Destructuring: binding patterns (declarations, parameters) and
//! assignment patterns.

use super::expr::MemberRef;
use super::*;

impl<'a> Compiler<'a> {
    /// Destructure acc into a binding pattern. `init` initialises lexical
    /// bindings; otherwise plain assignment (var declarations).
    pub(crate) fn binding_pattern(&mut self, p: &Pattern, init: bool) -> CResult<()> {
        self.pattern(p, Mode::Binding { init })
    }

    /// Destructure acc into an assignment pattern.
    pub(crate) fn assign_pattern(&mut self, p: &Pattern) -> CResult<()> {
        self.pattern(p, Mode::Assign)
    }

    fn pattern(&mut self, p: &Pattern, mode: Mode) -> CResult<()> {
        match p {
            Pattern::Ident(id) => {
                let init = matches!(mode, Mode::Binding { init: true });
                self.emit_store_name(&id.name, id.span, init);
                Ok(())
            }
            Pattern::Expression(e) => {
                match e.unparen() {
                    Expression::Ident(id) => self.emit_store_name(&id.name, id.span, false),
                    Expression::Member(m) => {
                        // The value is in acc; evaluate the reference, then
                        // store. (Spec order evaluates the reference first
                        // for assignment patterns; see `assign_element`.)
                        let mark = self.mark();
                        let v = self.alloc();
                        self.emit(Instr::Star(v));
                        let r = self.member_ref(m)?;
                        self.emit(Instr::Ldar(v));
                        self.emit_member_set(&r);
                        self.free_to(mark);
                    }
                    Expression::Call(c) => {
                        let mark = self.mark();
                        let v = self.alloc();
                        self.emit(Instr::Star(v));
                        self.call(c)?;
                        self.emit_throw_error(ErrorKind::Reference, "invalid assignment target");
                        self.free_to(mark);
                    }
                    other => return self.error("invalid destructuring target", other.span()),
                }
                Ok(())
            }
            Pattern::Assignment(a) => {
                // Default value when acc is undefined.
                let skip = self.emit_jump(Instr::JumpIfNotUndefined(0));
                let hint = match &a.left {
                    Pattern::Ident(id) => NameHint::Static(&id.name),
                    Pattern::Expression(e) => match e.unparen() {
                        Expression::Ident(id) => NameHint::Static(&id.name),
                        _ => NameHint::None,
                    },
                    _ => NameHint::None,
                };
                self.expr_named(&a.right, hint)?;
                self.patch(skip);
                self.pattern(&a.left, mode)
            }
            Pattern::Object(o) => self.object_pattern(o, mode),
            Pattern::Array(a) => self.array_pattern(a, mode),
            Pattern::Rest(r) => return self.error("unexpected rest element", r.span),
        }
    }

    fn object_pattern(&mut self, o: &ObjectPattern, mode: Mode) -> CResult<()> {
        let mark = self.mark();
        self.set_pos(o.span);
        self.emit(Instr::RequireObjectCoercible);
        let src = self.alloc();
        self.emit(Instr::Star(src));
        // Keys are kept for the rest element's exclusion list.
        let keep_keys = o.rest.is_some();
        let mut key_regs: Vec<Reg> = Vec::new();
        for prop in &o.properties {
            self.set_pos(prop.span);
            // A computed key is evaluated (and converted) first; then, for
            // assignment patterns with member targets, the target reference
            // is evaluated before the property is read.
            let computed = match &prop.key {
                PropertyKey::Computed(_) => Some(self.property_key_reg(&prop.key)?),
                _ => None,
            };
            let target_ref = self.pre_evaluate_target(&prop.value, mode)?;
            match (computed, keep_keys) {
                (Some(key), _) => {
                    key_regs.push(key);
                    self.emit(Instr::GetKeyed { obj: src, key });
                }
                (None, true) => {
                    let key = self.property_key_reg(&prop.key)?;
                    key_regs.push(key);
                    self.emit(Instr::GetKeyed { obj: src, key });
                }
                (None, false) => {
                    let name = static_key_name(&prop.key).unwrap();
                    if Self::utf16_is_str(&name) {
                        let n = self.name(&utf16_to_string(&name));
                        self.emit(Instr::GetNamed { obj: src, name: n });
                    } else {
                        let key = self.property_key_reg(&prop.key)?;
                        self.emit(Instr::GetKeyed { obj: src, key });
                    }
                }
            }
            self.finish_element(&prop.value, mode, target_ref)?;
        }
        if let Some(rest) = &o.rest {
            let target_ref = self.pre_evaluate_target(rest, mode)?;
            // Excluded keys must be in consecutive registers.
            let count = key_regs.len() as u16;
            let base = self.alloc_n(count);
            for (i, k) in key_regs.iter().enumerate() {
                self.emit(Instr::Mov {
                    dst: base + i as u16,
                    src: *k,
                });
            }
            let target = self.alloc();
            self.emit(Instr::CreateObject);
            self.emit(Instr::Star(target));
            self.emit(Instr::Ldar(src));
            self.emit(Instr::CopyDataProperties {
                target,
                excluded: base,
                count,
            });
            self.emit(Instr::Ldar(target));
            self.finish_element(rest, mode, target_ref)?;
        }
        self.free_to(mark);
        Ok(())
    }

    fn array_pattern(&mut self, a: &ArrayPattern, mode: Mode) -> CResult<()> {
        let mark = self.mark();
        let outer_depth = self.env_depth();
        self.set_pos(a.span);
        let iter = self.alloc();
        let next = self.alloc();
        let done = self.alloc();
        self.emit(Instr::GetIterator { iter, next });
        self.emit(Instr::LdaFalse);
        self.emit(Instr::Star(done));
        let body_start = self.pc();
        self.fs().ctl.push(Ctl::DestructureIter { iter, done });
        for el in &a.elements {
            match el {
                None => {
                    // Hole: step the iterator, ignore the value. The done flag is
                    // set before the step so that a throwing next() does
                    // not get the iterator closed.
                    self.emit(Instr::Ldar(done));
                    let skip = self.emit_jump(Instr::JumpIfTrue(0));
                    self.emit(Instr::LdaTrue);
                    self.emit(Instr::Star(done));
                    let d = self.emit_jump(Instr::IteratorStep {
                        iter,
                        next,
                        done: 0,
                    });
                    self.emit(Instr::LdaFalse);
                    self.emit(Instr::Star(done));
                    self.patch(d);
                    self.patch(skip);
                }
                Some(Pattern::Rest(r)) => {
                    let target_ref = self.pre_evaluate_target(&r.argument, mode)?;
                    let arr = self.alloc();
                    self.emit(Instr::CreateArray);
                    self.emit(Instr::Star(arr));
                    self.emit(Instr::Ldar(done));
                    let skip = self.emit_jump(Instr::JumpIfTrue(0));
                    let top = self.pc();
                    self.emit(Instr::LdaTrue);
                    self.emit(Instr::Star(done));
                    let d = self.emit_jump(Instr::IteratorStep {
                        iter,
                        next,
                        done: 0,
                    });
                    self.emit(Instr::ArrayPush { arr });
                    self.emit(Instr::LdaFalse);
                    self.emit(Instr::Star(done));
                    self.emit_jump_to(top);
                    self.patch(d);
                    self.patch(skip);
                    self.emit(Instr::Ldar(arr));
                    self.finish_element(&r.argument, mode, target_ref)?;
                }
                Some(p) => {
                    let target_ref = self.pre_evaluate_target(p, mode)?;
                    self.emit(Instr::Ldar(done));
                    let already = self.emit_jump(Instr::JumpIfTrue(0));
                    self.emit(Instr::LdaTrue);
                    self.emit(Instr::Star(done));
                    let d = self.emit_jump(Instr::IteratorStep {
                        iter,
                        next,
                        done: 0,
                    });
                    let value = self.alloc();
                    self.emit(Instr::Star(value));
                    self.emit(Instr::LdaFalse);
                    self.emit(Instr::Star(done));
                    self.emit(Instr::Ldar(value));
                    let got = self.emit_jump(Instr::Jump(0));
                    self.patch(d);
                    self.patch(already);
                    self.emit(Instr::LdaUndefined);
                    self.patch(got);
                    self.finish_element(p, mode, target_ref)?;
                }
            }
        }
        match self.fs().ctl.pop() {
            Some(Ctl::DestructureIter { .. }) => {}
            _ => unreachable!("destructuring control entry out of balance"),
        }
        let body_end = self.pc();
        // Normal completion: close the iterator if not done.
        self.emit(Instr::Ldar(done));
        let skip_close = self.emit_jump(Instr::JumpIfTrue(0));
        self.emit(Instr::IteratorClose { iter });
        self.patch(skip_close);
        let end = self.emit_jump(Instr::Jump(0));
        // Throw inside the pattern: close (swallowing) unless done, rethrow.
        let handler = self.pc();
        self.add_handler(body_start, body_end, handler, outer_depth);
        let exc = self.alloc();
        self.emit(Instr::Star(exc));
        self.emit(Instr::Ldar(done));
        let skip = self.emit_jump(Instr::JumpIfTrue(0));
        self.emit(Instr::IteratorCloseSwallow { iter });
        self.patch(skip);
        self.emit(Instr::Ldar(exc));
        self.emit(Instr::Throw);
        self.patch(end);
        self.free_to(mark);
        Ok(())
    }

    /// In assignment patterns, member targets are evaluated before the
    /// value is produced. Returns the evaluated reference, if any.
    fn pre_evaluate_target(&mut self, p: &Pattern, mode: Mode) -> CResult<Option<TargetRef>> {
        let target = match p {
            Pattern::Assignment(a) => &a.left,
            other => other,
        };
        // Identifier references in with/eval scopes are resolved before the
        // value is fetched (ResolveBinding precedes GetV in the spec).
        let ident = match target {
            Pattern::Ident(id) => Some(id),
            Pattern::Expression(e) => match e.unparen() {
                Expression::Ident(id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        if let Some(id) = ident {
            if let Some(r) = self.dynamic_ref(&id.name) {
                return Ok(Some(TargetRef::Dynamic(id.name.clone(), id.span, r)));
            }
            return Ok(None);
        }
        if mode != Mode::Assign {
            return Ok(None);
        }
        if let Pattern::Expression(e) = target {
            if let Expression::Member(m) = e.unparen() {
                return Ok(Some(TargetRef::Member(self.member_ref(m)?)));
            }
        }
        Ok(None)
    }

    /// Bind the element value in acc, using a pre-evaluated target when
    /// one exists.
    fn finish_element(
        &mut self,
        p: &Pattern,
        mode: Mode,
        target_ref: Option<TargetRef>,
    ) -> CResult<()> {
        match target_ref {
            None => self.pattern(p, mode),
            Some(TargetRef::Member(r)) => {
                if let Pattern::Assignment(a) = p {
                    let skip = self.emit_jump(Instr::JumpIfNotUndefined(0));
                    self.expr(&a.right)?;
                    self.patch(skip);
                }
                self.emit_member_set(&r);
                Ok(())
            }
            Some(TargetRef::Dynamic(name, span, r)) => {
                if let Pattern::Assignment(a) = p {
                    let skip = self.emit_jump(Instr::JumpIfNotUndefined(0));
                    self.expr_named(&a.right, NameHint::Static(&name))?;
                    self.patch(skip);
                }
                self.emit_store_ref(&name, span, Some(r));
                Ok(())
            }
        }
    }
}

/// A destructuring target evaluated before its value.
enum TargetRef {
    Member(MemberRef),
    /// A dynamically resolved identifier: name, span, resolved reference.
    Dynamic(Rc<str>, Span, (NameIdx, Reg)),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Binding { init: bool },
    Assign,
}
