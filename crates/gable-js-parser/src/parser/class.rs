//! Classes.

use super::function::FunctionFlags;
use super::scope::{LexKind, PrivateEnv, PrivateKind};
use super::{FnCtx, Parser};
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};

impl<'s> Parser<'s> {
    pub(crate) fn parse_class_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        self.expect_kw("class")?;
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true;
        let id = if self.at(Punct::LBrace) || self.at_kw("extends") {
            None
        } else {
            let r = self.parse_binding_identifier();
            if r.is_err() {
                self.ctx.strict = saved_strict;
            }
            Some(r?)
        };
        let class = self.parse_class_tail(start, id);
        self.ctx.strict = saved_strict;
        Ok(Expression::Class(Box::new(class?)))
    }

    pub(crate) fn parse_class_declaration(&mut self, default_export: bool) -> Result<Class> {
        let start = self.tok.span.start;
        self.expect_kw("class")?;
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true;
        let id = if (self.at(Punct::LBrace) || self.at_kw("extends")) && default_export {
            None
        } else {
            let r = self.parse_binding_identifier();
            if r.is_err() {
                self.ctx.strict = saved_strict;
            }
            Some(r?)
        };
        if let Some(id) = &id {
            if let Err(e) = self.scopes.declare_lexical(id.name.clone(), id.span, LexKind::Lexical, true) {
                self.ctx.strict = saved_strict;
                return Err(e);
            }
        }
        let class = self.parse_class_tail(start, id);
        self.ctx.strict = saved_strict;
        class
    }

    /// `extends`? `{` ClassBody `}`. Strict mode is already on.
    fn parse_class_tail(&mut self, start: u32, id: Option<Ident>) -> Result<Class> {
        let mut super_class = None;
        if self.eat_kw("extends")? {
            let heritage = self.parse_lhs_expression()?;
            self.no_arrow(&heritage)?;
            super_class = Some(heritage);
        }
        let has_super = super_class.is_some();
        self.expect(Punct::LBrace)?;
        self.private_envs.push(PrivateEnv::default());
        let body = self.parse_class_body(has_super);
        let env = self.private_envs.pop().unwrap();
        let body = body?;
        // Resolve private-name references: unknown names bubble to the
        // enclosing class, or are errors at the outermost one.
        for (name, span) in env.referenced.iter().cloned() {
            if env_declares(&env, &name) {
                continue;
            }
            match self.private_envs.last_mut() {
                Some(outer) => outer.referenced.push((name, span)),
                None => return self.err(format!("reference to undeclared private name #{name}"), span),
            }
        }
        self.expect(Punct::RBrace)?;
        Ok(Class { id, super_class, body, span: self.span_from(start) })
    }

    fn parse_class_body(&mut self, has_super: bool) -> Result<Vec<ClassMember>> {
        let mut members = Vec::new();
        let mut seen_constructor = false;
        while !self.at(Punct::RBrace) {
            if self.eat(Punct::Semicolon)? {
                continue;
            }
            let member = self.parse_class_member(has_super)?;
            if let ClassMember::Method(m) = &member {
                if m.kind == MethodKind::Constructor {
                    if seen_constructor {
                        return self.err("a class may only have one constructor", m.span);
                    }
                    seen_constructor = true;
                }
            }
            members.push(member);
        }
        Ok(members)
    }

    fn parse_class_member(&mut self, has_super: bool) -> Result<ClassMember> {
        let start = self.tok.span.start;
        let mut is_static = false;
        let mut is_async = false;
        let mut is_generator = false;
        let mut accessor: Option<MethodKind> = None;

        // `static` modifier (unless it is the member name itself).
        if self.at_kw("static") {
            let next = self.peek()?;
            let is_name = matches!(
                next.kind,
                TokenKind::Punct(Punct::LParen) | TokenKind::Punct(Punct::Eq) | TokenKind::Punct(Punct::Semicolon) | TokenKind::Punct(Punct::RBrace)
            ) || (next.newline_before && !matches!(next.kind, TokenKind::Punct(Punct::LBrace)) && !can_follow_static_as_modifier(&next.kind));
            if !is_name {
                self.advance()?;
                is_static = true;
                if self.at(Punct::LBrace) {
                    return self.parse_static_block(start);
                }
            }
        }

        if let TokenKind::Ident { name, escaped: false } = &self.tok.kind {
            let name = name.clone();
            let next = self.peek()?;
            let next_is_key_start = !matches!(
                next.kind,
                TokenKind::Punct(Punct::LParen)
                    | TokenKind::Punct(Punct::Eq)
                    | TokenKind::Punct(Punct::Semicolon)
                    | TokenKind::Punct(Punct::RBrace)
            );
            if next_is_key_start {
                match &*name {
                    "async" if !next.newline_before => {
                        is_async = true;
                        self.advance()?;
                        if self.eat(Punct::Star)? {
                            is_generator = true;
                        }
                    }
                    "get" if !next.newline_before || !matches!(next.kind, TokenKind::Punct(Punct::Star)) => {
                        // `get` followed by a newline and then a name is
                        // still a getter (no restriction), but a field
                        // named `get` followed by `*gen(){}` is two members.
                        if !(next.newline_before && matches!(next.kind, TokenKind::Punct(Punct::Star))) {
                            accessor = Some(MethodKind::Get);
                            self.advance()?;
                        }
                    }
                    "set" => {
                        if !(next.newline_before && matches!(next.kind, TokenKind::Punct(Punct::Star))) {
                            accessor = Some(MethodKind::Set);
                            self.advance()?;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !is_async && accessor.is_none() && self.eat(Punct::Star)? {
            is_generator = true;
        }

        let key_span = self.tok.span;
        let key = self.parse_property_key(true)?;
        let key_name = key.static_name();
        let is_name = |s: &str| key_name.as_deref() == Some(s.encode_utf16().collect::<Vec<_>>().as_slice());
        let is_private = matches!(key, PropertyKey::Private(_));

        let is_method = self.at(Punct::LParen);
        if is_method {
            let is_constructor = !is_static && !is_private && is_name("constructor") && !matches!(key, PropertyKey::Computed(_));
            if is_constructor && (is_async || is_generator || accessor.is_some()) {
                return self.err("class constructor may not be an accessor, generator or async", key_span);
            }
            if is_static && is_name("prototype") && !matches!(key, PropertyKey::Computed(_)) {
                return self.err("classes may not have a static property named 'prototype'", key_span);
            }
            let kind = match (is_async, is_generator) {
                (true, true) => FunctionKind::AsyncGenerator,
                (true, false) => FunctionKind::Async,
                (false, true) => FunctionKind::Generator,
                _ => FunctionKind::Normal,
            };
            let method_kind = if is_constructor {
                MethodKind::Constructor
            } else {
                accessor.unwrap_or(MethodKind::Method)
            };
            if let PropertyKey::Private(p) = &key {
                let pk = match method_kind {
                    MethodKind::Get => PrivateKind::Getter,
                    MethodKind::Set => PrivateKind::Setter,
                    _ => PrivateKind::Other,
                };
                self.private_envs.last_mut().unwrap().declare(p.name.clone(), p.span, pk, is_static)?;
            }
            let flags = FunctionFlags {
                kind,
                is_method: true,
                super_call: is_constructor && has_super,
                is_getter: method_kind == MethodKind::Get,
                is_setter: method_kind == MethodKind::Set,
                is_class_constructor: is_constructor,
            };
            let value = self.parse_class_method_body(kind, flags)?;
            return Ok(ClassMember::Method(ClassMethod { key, value, kind: method_kind, is_static, span: self.span_from(start) }));
        }

        // Field.
        if is_async || is_generator || accessor.is_some() {
            return self.unexpected();
        }
        if !matches!(key, PropertyKey::Computed(_)) && !is_private {
            if is_name("constructor") {
                return self.err("classes may not have a field named 'constructor'", key_span);
            }
            if is_static && is_name("prototype") {
                return self.err("classes may not have a static field named 'prototype'", key_span);
            }
        }
        if let PropertyKey::Private(p) = &key {
            self.private_envs.last_mut().unwrap().declare(p.name.clone(), p.span, PrivateKind::Other, is_static)?;
        }
        let mut value = None;
        if self.eat(Punct::Eq)? {
            // Initialisers are like methods: `super.x` and `new.target`
            // allowed, `arguments` and `super()` not, not generators/async.
            let new_ctx = FnCtx {
                strict: true,
                is_async: false,
                is_generator: false,
                super_property: true,
                super_call: false,
                new_target: true,
                no_arguments: true,
                allow_return: false,
                in_static_block: false,
                module_top: false,
                in_params: false,
            };
            let saved = self.save_fn_ctx(new_ctx);
            self.scopes.push_function_top(false);
            let r = self.with_in(true, |p| p.parse_assignment_expression());
            self.scopes.pop();
            self.restore_fn_ctx(saved);
            value = Some(r?);
        }
        self.consume_semicolon()?;
        Ok(ClassMember::Field(ClassField { key, value, is_static, span: self.span_from(start) }))
    }

    fn parse_static_block(&mut self, start: u32) -> Result<ClassMember> {
        self.expect(Punct::LBrace)?;
        let new_ctx = FnCtx {
            strict: true,
            is_async: false,
            is_generator: false,
            super_property: true,
            super_call: false,
            new_target: true,
            no_arguments: true,
            allow_return: false,
            in_static_block: true,
            module_top: false,
            in_params: false,
        };
        let saved = self.save_fn_ctx(new_ctx);
        self.scopes.push_function_top(false);
        let body = (|| {
            let mut body = Vec::new();
            while !self.at(Punct::RBrace) {
                if self.tok.kind == TokenKind::Eof {
                    return self.unexpected();
                }
                body.push(self.parse_statement_list_item(false, false)?);
            }
            Ok(body)
        })();
        self.scopes.pop();
        self.restore_fn_ctx(saved);
        let body = body?;
        self.expect(Punct::RBrace)?;
        Ok(ClassMember::StaticBlock(StaticBlock { body, span: self.span_from(start) }))
    }
}

fn env_declares(env: &PrivateEnv, name: &str) -> bool {
    env.is_declared(name)
}

/// After `static` and a line terminator, these tokens still make `static`
/// a modifier (the next member name) rather than a field named `static`.
fn can_follow_static_as_modifier(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident { .. }
            | TokenKind::String { .. }
            | TokenKind::Number { .. }
            | TokenKind::BigInt(_)
            | TokenKind::PrivateName(_)
            | TokenKind::Punct(Punct::LBracket)
            | TokenKind::Punct(Punct::Star)
    )
}
