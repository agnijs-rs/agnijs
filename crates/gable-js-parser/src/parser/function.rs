//! Functions: declarations, expressions, methods, arrows and parameters.

use super::{FnCtx, Parser};
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};
use crate::span::Span;
use std::collections::HashSet;

/// What kind of function body is being parsed; selects the context rules.
#[derive(Clone, Copy)]
pub(crate) struct FunctionFlags {
    pub kind: FunctionKind,
    /// Object or class method: `super.x` allowed, no duplicate params.
    pub is_method: bool,
    /// Derived-class constructor: `super()` allowed.
    pub super_call: bool,
    pub is_getter: bool,
    pub is_setter: bool,
    /// Class constructor (field initialisers happen elsewhere).
    /// Class constructors: kept for the compiler (M2) to mark the
    /// function as `[[ConstructorKind]]` base/derived.
    #[allow(dead_code)]
    pub is_class_constructor: bool,
}

impl FunctionFlags {
    pub fn plain(kind: FunctionKind) -> FunctionFlags {
        FunctionFlags { kind, is_method: false, super_call: false, is_getter: false, is_setter: false, is_class_constructor: false }
    }
}

impl<'s> Parser<'s> {
    fn function_kind(is_async: bool, is_generator: bool) -> FunctionKind {
        match (is_async, is_generator) {
            (true, true) => FunctionKind::AsyncGenerator,
            (true, false) => FunctionKind::Async,
            (false, true) => FunctionKind::Generator,
            _ => FunctionKind::Normal,
        }
    }

    /// `function` already current. Parses a function expression (named or
    /// anonymous).
    pub(crate) fn parse_function_expression(&mut self, is_async: bool) -> Result<Expression> {
        let start = self.tok.span.start;
        if is_async {
            self.advance()?; // async
        }
        self.expect_kw("function")?;
        let is_generator = self.eat(Punct::Star)?;
        let kind = Self::function_kind(is_async, is_generator);
        // The name of a function expression is bound inside the function:
        // yield/await rules follow the function's own kind.
        let mut id = None;
        if !self.at(Punct::LParen) {
            let saved = self.ctx;
            self.ctx.is_generator = is_generator;
            self.ctx.is_async = is_async;
            self.ctx.in_params = false;
            self.ctx.in_static_block = false;
            let r = self.parse_binding_identifier();
            self.ctx = saved;
            id = Some(r?);
        }
        let func = self.parse_function_tail(start, id, FunctionFlags::plain(kind), true)?;
        Ok(Expression::Function(Box::new(func)))
    }

    /// `function` (or `async function`) declaration. The current token is
    /// `async` or `function`. `default_export` allows an anonymous function.
    pub(crate) fn parse_function_declaration(&mut self, is_async: bool, default_export: bool) -> Result<Function> {
        let start = self.tok.span.start;
        if is_async {
            self.advance()?;
        }
        self.expect_kw("function")?;
        let is_generator = self.eat(Punct::Star)?;
        let kind = Self::function_kind(is_async, is_generator);
        let id = if self.at(Punct::LParen) && default_export {
            None
        } else {
            // The declared name belongs to the enclosing scope: yield/await
            // rules follow the enclosing context.
            Some(self.parse_binding_identifier()?)
        };
        if let Some(id) = &id {
            self.scopes.declare_function(id.name.clone(), id.span, self.ctx.strict, kind == FunctionKind::Normal)?;
        }
        let func = self.parse_function_tail(start, id, FunctionFlags::plain(kind), false)?;
        Ok(func)
    }

    /// A method: the current token is `(`.
    pub(crate) fn parse_method(
        &mut self,
        kind: FunctionKind,
        super_property: bool,
        super_call: bool,
        is_getter: bool,
        is_setter: bool,
    ) -> Result<Function> {
        let _ = super_property;
        let start = self.tok.span.start;
        let flags = FunctionFlags { kind, is_method: true, super_call, is_getter, is_setter, is_class_constructor: false };
        self.parse_function_tail(start, None, flags, true)
    }

    pub(crate) fn parse_class_method_body(&mut self, kind: FunctionKind, flags: FunctionFlags) -> Result<Function> {
        let _ = kind;
        let start = self.tok.span.start;
        self.parse_function_tail(start, None, flags, true)
    }

    /// Parameters and body. The current token is `(`.
    pub(crate) fn parse_function_tail(
        &mut self,
        start: u32,
        id: Option<Ident>,
        flags: FunctionFlags,
        is_expression: bool,
    ) -> Result<Function> {
        let kind = flags.kind;
        let new_ctx = FnCtx {
            strict: self.ctx.strict,
            is_async: kind.is_async(),
            is_generator: kind.is_generator(),
            super_property: flags.is_method,
            super_call: flags.super_call,
            new_target: true,
            no_arguments: false,
            allow_return: true,
            in_static_block: false,
            module_top: false,
            in_params: true,
        };
        let saved = self.save_fn_ctx(new_ctx);
        let saved_cover = self.pending_cover.take();
        let saved_ya = self.pending_yield_await.take();
        let saved_privates_len = self.private_envs.len();
        self.scopes.push_function_top(false);

        let result = (|| -> Result<Function> {
            let (params, simple_params, params_span) = self.parse_formal_parameters()?;
            if flags.is_getter && !params.is_empty() {
                return self.err("getter must not have any formal parameters", params_span);
            }
            if flags.is_setter && (params.len() != 1 || matches!(params[0], Pattern::Rest(_))) {
                return self.err("setter must have exactly one formal parameter", params_span);
            }
            self.ctx.in_params = false;
            let body_start = self.tok.span.start;
            self.expect(Punct::LBrace)?;
            let body = self.parse_body_with_prologue(false, false)?;
            self.expect(Punct::RBrace)?;
            let body_span = self.span_from(body_start);
            let has_use_strict = body.iter().take_while(|s| matches!(s, Statement::Expression(e) if e.directive.is_some())).any(
                |s| matches!(s, Statement::Expression(e) if e.directive.as_deref() == Some("use strict")),
            );
            if has_use_strict && !simple_params {
                return self.err("'use strict' not allowed in function with non-simple parameters", body_span);
            }
            let strict = self.ctx.strict;
            self.validate_params(&params, strict, simple_params, flags.is_method, kind)?;
            if strict {
                if let Some(id) = &id {
                    self.check_binding_identifier(&id.name, id.span)?;
                    if Self::is_strict_reserved(&id.name) {
                        return self.err(format!("unexpected strict mode reserved word '{}'", id.name), id.span);
                    }
                }
            }
            Ok(Function {
                id,
                params,
                body: FunctionBody { body, span: body_span },
                kind,
                strict,
                simple_params,
                has_direct_eval: self.has_direct_eval,
                is_expression,
                span: self.span_from(start),
            })
        })();

        self.scopes.pop();
        self.private_envs.truncate(saved_privates_len);
        self.pending_cover = saved_cover;
        self.pending_yield_await = saved_ya;
        self.restore_fn_ctx(saved);
        result
    }

    /// `(` FormalParameters `)`. Returns the patterns, whether the list is
    /// simple, and the span.
    pub(crate) fn parse_formal_parameters(&mut self) -> Result<(Vec<Pattern>, bool, Span)> {
        let start = self.tok.span.start;
        self.expect(Punct::LParen)?;
        let mut params = Vec::new();
        let mut simple = true;
        while !self.at(Punct::RParen) {
            if self.at(Punct::Ellipsis) {
                let s = self.tok.span.start;
                self.advance()?;
                let argument = self.parse_binding_target()?;
                if self.at(Punct::Eq) {
                    return self.err("rest parameter may not have a default initializer", self.tok.span);
                }
                params.push(Pattern::Rest(Box::new(RestElement { argument, span: self.span_from(s) })));
                simple = false;
                if !self.at(Punct::RParen) {
                    return self.err("rest parameter must be last formal parameter", self.tok.span);
                }
                break;
            }
            let p = self.parse_binding_element()?;
            if !matches!(p, Pattern::Ident(_)) {
                simple = false;
            }
            params.push(p);
            if !self.at(Punct::RParen) {
                self.expect(Punct::Comma)?;
            }
        }
        self.expect(Punct::RParen)?;
        for p in &params {
            let mut names = Vec::new();
            Self::bound_names(p, &mut names);
            for n in names {
                self.scopes.add_param(n.name.clone());
            }
        }
        Ok((params, simple, self.span_from(start)))
    }

    /// Checks that depend on the function's final strictness.
    pub(crate) fn validate_params(
        &self,
        params: &[Pattern],
        strict: bool,
        simple: bool,
        no_duplicates: bool,
        kind: FunctionKind,
    ) -> Result<()> {
        let mut names = Vec::new();
        for p in params {
            Self::bound_names(p, &mut names);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let dups_forbidden = strict || !simple || no_duplicates;
        for id in names {
            let name = &*id.name;
            if strict {
                if name == "eval" || name == "arguments" {
                    return self.err(format!("'{name}' cannot be a parameter name in strict mode"), id.span);
                }
                if Self::is_strict_reserved(name) {
                    return self.err(format!("unexpected strict mode reserved word '{name}'"), id.span);
                }
            }
            if kind.is_generator() && name == "yield" {
                return self.err("'yield' cannot be a parameter name in a generator", id.span);
            }
            if kind.is_async() && name == "await" {
                return self.err("'await' cannot be a parameter name in an async function", id.span);
            }
            if !seen.insert(name) && dups_forbidden {
                return self.err(format!("duplicate parameter name '{name}'"), id.span);
            }
        }
        Ok(())
    }

    /// The `=>` is current. Parses the arrow body with `params`.
    pub(crate) fn parse_arrow_from_params(
        &mut self,
        start: u32,
        params: Vec<Pattern>,
        is_async: bool,
        single_ident: bool,
    ) -> Result<Expression> {
        let _ = single_ident;
        self.expect(Punct::Arrow)?;
        let simple_params = params.iter().all(|p| matches!(p, Pattern::Ident(_)));
        let new_ctx = FnCtx {
            strict: self.ctx.strict,
            is_async,
            is_generator: false,
            super_property: self.ctx.super_property,
            super_call: self.ctx.super_call,
            new_target: self.ctx.new_target,
            no_arguments: self.ctx.no_arguments,
            allow_return: true,
            in_static_block: false,
            module_top: false,
            in_params: false,
        };
        let saved = self.save_fn_ctx(new_ctx);
        let saved_cover = self.pending_cover.take();
        let saved_ya = self.pending_yield_await.take();
        let saved_privates_len = self.private_envs.len();
        self.scopes.push_function_top(false);
        for p in &params {
            let mut names = Vec::new();
            Self::bound_names(p, &mut names);
            for n in names {
                self.scopes.add_param(n.name.clone());
            }
        }

        let result = (|| -> Result<Expression> {
            let body;
            if self.at(Punct::LBrace) {
                let body_start = self.tok.span.start;
                self.advance()?;
                let stmts = self.parse_body_with_prologue(false, false)?;
                self.expect(Punct::RBrace)?;
                let body_span = self.span_from(body_start);
                let has_use_strict = stmts.iter().take_while(|s| matches!(s, Statement::Expression(e) if e.directive.is_some())).any(
                    |s| matches!(s, Statement::Expression(e) if e.directive.as_deref() == Some("use strict")),
                );
                if has_use_strict && !simple_params {
                    return self.err("'use strict' not allowed in function with non-simple parameters", body_span);
                }
                body = ArrowBody::Block(FunctionBody { body: stmts, span: body_span });
            } else {
                let e = self.parse_assignment_expression()?;
                body = ArrowBody::Expression(e);
            }
            let strict = self.ctx.strict;
            let kind = if is_async { FunctionKind::Async } else { FunctionKind::Normal };
            self.validate_params(&params, strict, simple_params, true, kind)?;
            Ok(Expression::Arrow(Box::new(ArrowFunction {
                params,
                body,
                is_async,
                strict,
                simple_params,
                has_direct_eval: self.has_direct_eval,
                span: self.span_from(start),
            })))
        })();

        self.scopes.pop();
        self.private_envs.truncate(saved_privates_len);
        self.pending_cover = saved_cover;
        self.pending_yield_await = saved_ya;
        self.restore_fn_ctx(saved);
        result
    }

    /// Whether the current token begins `async function` (no newline).
    pub(crate) fn at_async_function(&mut self) -> Result<bool> {
        if !self.at_kw("async") {
            return Ok(false);
        }
        let next = self.peek()?;
        Ok(matches!(&next.kind, TokenKind::Ident { name, escaped: false } if &**name == "function") && !next.newline_before)
    }
}
