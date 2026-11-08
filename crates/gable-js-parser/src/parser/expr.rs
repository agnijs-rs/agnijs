//! Expressions.

use super::Parser;
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};
use crate::span::Span;
use std::rc::Rc;

// Binary precedence levels (higher binds tighter).
const PREC_NULLISH: u8 = 1;
const PREC_OR: u8 = 2;
const PREC_AND: u8 = 3;
const PREC_BIT_OR: u8 = 4;
const PREC_BIT_XOR: u8 = 5;
const PREC_BIT_AND: u8 = 6;
const PREC_EQUALITY: u8 = 7;
const PREC_RELATIONAL: u8 = 8;
const PREC_SHIFT: u8 = 9;
const PREC_ADDITIVE: u8 = 10;
const PREC_MULTIPLICATIVE: u8 = 11;
const PREC_EXP: u8 = 12;

enum BinOrLogical {
    Binary(BinaryOp),
    Logical(LogicalOp),
}

impl<'s> Parser<'s> {
    /// Expression (comma-separated).
    pub(crate) fn parse_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let first = self.parse_assignment_expression()?;
        if !self.at(Punct::Comma) {
            return Ok(first);
        }
        let mut exprs = vec![first];
        while self.eat(Punct::Comma)? {
            exprs.push(self.parse_assignment_expression()?);
        }
        Ok(Expression::Sequence(Box::new(SequenceExpression {
            expressions: exprs,
            span: self.span_from(start),
        })))
    }

    pub(crate) fn with_in<T>(
        &mut self,
        allow_in: bool,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let old = std::mem::replace(&mut self.allow_in, allow_in);
        let r = f(self);
        self.allow_in = old;
        r
    }

    /// Report a pending cover-grammar error if the expression just parsed
    /// cannot become a pattern any more.
    fn settle_cover(
        &mut self,
        expr: &Expression,
        saved: Option<(Span, &'static str)>,
    ) -> Result<()> {
        if let Some((span, msg)) = self.pending_cover {
            let still_pattern = matches!(expr, Expression::Object(_) | Expression::Array(_));
            if !still_pattern {
                return self.err(msg, span);
            }
            if saved.is_some() {
                self.pending_cover = saved;
            }
        } else {
            self.pending_cover = saved;
        }
        Ok(())
    }

    pub(crate) fn parse_assignment_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;

        // yield
        if self.at_kw("yield") && self.ctx.is_generator {
            return self.parse_yield();
        }

        // Single-identifier arrow: `x => ...`, `async x => ...`
        if let TokenKind::Ident { name, escaped } = &self.tok.kind {
            let name = name.clone();
            let escaped = *escaped;
            let next = self.peek()?;
            if next.kind == TokenKind::Punct(Punct::Arrow) && !next.newline_before {
                self.check_identifier(&name, self.tok.span, escaped)?;
                let span = self.tok.span;
                self.advance()?;
                let param = Ident {
                    name: self.intern(&name),
                    span,
                };
                return self.parse_arrow_from_params(
                    start,
                    vec![Pattern::Ident(param)],
                    false,
                    true,
                );
            }
            if &*name == "async" && !escaped && !next.newline_before {
                if let TokenKind::Ident {
                    name: pname,
                    escaped: pescaped,
                } = &next.kind
                {
                    // `async x =>`
                    let snap = self.snapshot();
                    self.advance()?;
                    let ptok = self.advance()?;
                    if self.at(Punct::Arrow) && !self.tok.newline_before {
                        let saved_ctx = self.ctx;
                        // Parameter names of an async arrow: `await` is reserved.
                        self.ctx.is_async = true;
                        let r = self.check_identifier(pname, ptok.span, *pescaped);
                        self.ctx = saved_ctx;
                        r?;
                        self.check_binding_identifier(pname, ptok.span)?;
                        let param = Ident {
                            name: self.intern(pname),
                            span: ptok.span,
                        };
                        return self.parse_arrow_from_params(
                            start,
                            vec![Pattern::Ident(param)],
                            true,
                            true,
                        );
                    }
                    self.rewind(snap);
                }
            }
        }

        let saved_cover = self.pending_cover.take();
        let lhs = self.parse_conditional_expression()?;

        // Parenthesised arrow heads are handled inside parse_primary;
        // `async (...) =>` arrives here as a call expression.
        if self.at(Punct::Arrow) {
            if self.tok.newline_before {
                return self.unexpected();
            }
            if let Expression::Call(call) = &lhs {
                if matches!(&call.callee, Expression::Ident(i) if &*i.name == "async")
                    && self.async_arrow_ok(call)
                {
                    let Expression::Call(call) = lhs else {
                        unreachable!()
                    };
                    let call = *call;
                    if let Some(pos) = self.last_await {
                        if pos >= call.span.start && pos < call.span.end {
                            return self.err(
                                "'await' is not allowed in async arrow parameters",
                                Span::new(pos, pos + 5),
                            );
                        }
                    }
                    if let Some(Expression::Spread(s)) = call.arguments.last() {
                        let after = &self.source()[s.span.end as usize..call.span.end as usize];
                        if after.contains(',') {
                            return self
                                .err("rest parameter may not have a trailing comma", s.span);
                        }
                    }
                    let params = self.arguments_to_params(call.arguments)?;
                    self.pending_cover = saved_cover;
                    return self.parse_arrow_from_params(start, params, true, false);
                }
            }
            return self.unexpected();
        }

        let op = match &self.tok.kind {
            TokenKind::Punct(p) => match p {
                Punct::Eq => Some(AssignOp::Assign),
                Punct::PlusEq => Some(AssignOp::Add),
                Punct::MinusEq => Some(AssignOp::Sub),
                Punct::StarEq => Some(AssignOp::Mul),
                Punct::SlashEq => Some(AssignOp::Div),
                Punct::PercentEq => Some(AssignOp::Mod),
                Punct::StarStarEq => Some(AssignOp::Exp),
                Punct::ShlEq => Some(AssignOp::Shl),
                Punct::ShrEq => Some(AssignOp::Shr),
                Punct::UShrEq => Some(AssignOp::UShr),
                Punct::AmpEq => Some(AssignOp::BitAnd),
                Punct::PipeEq => Some(AssignOp::BitOr),
                Punct::CaretEq => Some(AssignOp::BitXor),
                Punct::AmpAmpEq => Some(AssignOp::And),
                Punct::PipePipeEq => Some(AssignOp::Or),
                Punct::QuestionQuestionEq => Some(AssignOp::Nullish),
                _ => None,
            },
            _ => None,
        };
        let Some(op) = op else {
            self.settle_cover(&lhs, saved_cover)?;
            return Ok(lhs);
        };

        let left = if op == AssignOp::Assign {
            let p = self.expr_to_pattern(lhs, false)?;
            self.pending_cover = saved_cover;
            p
        } else {
            self.settle_cover(&lhs, saved_cover)?;
            let logical = matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Nullish);
            self.expr_to_simple_target(lhs, logical)?
        };
        self.advance()?;
        let right = self.parse_assignment_expression()?;
        Ok(Expression::Assignment(Box::new(AssignmentExpression {
            op,
            left,
            right,
            span: self.span_from(start),
        })))
    }

    /// `async (...)` parsed as a call can be an arrow head only when there
    /// was no line terminator between `async` and `(`.
    fn async_arrow_ok(&self, call: &CallExpression) -> bool {
        if call.optional {
            return false;
        }
        let Expression::Ident(id) = &call.callee else {
            return false;
        };
        // An escaped `\u0061sync` is an ordinary identifier, not the keyword.
        if &self.source()[id.span.start as usize..id.span.end as usize] != "async" {
            return false;
        }
        let between = &self.source()[id.span.end as usize..];
        // Arguments start at the first `(`; anything before it is trivia.
        let paren = between.find('(').unwrap_or(0);
        !between[..paren]
            .chars()
            .any(crate::lexer::is_line_terminator)
    }

    fn parse_yield(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let span = self.tok.span;
        if self.ctx.in_params {
            return self.err("yield expression not allowed in formal parameters", span);
        }
        self.advance()?;
        if self.pending_yield_await.is_none() {
            self.pending_yield_await = Some(span);
        }
        let mut delegate = false;
        let mut argument = None;
        if !self.tok.newline_before {
            if self.eat(Punct::Star)? {
                delegate = true;
                argument = Some(self.parse_assignment_expression()?);
            } else if self.can_start_expression() {
                argument = Some(self.parse_assignment_expression()?);
            }
        }
        Ok(Expression::Yield(Box::new(YieldExpression {
            argument,
            delegate,
            span: self.span_from(start),
        })))
    }

    fn can_start_expression(&self) -> bool {
        match &self.tok.kind {
            TokenKind::Eof => false,
            TokenKind::Punct(p) => matches!(
                p,
                Punct::LBrace
                    | Punct::LParen
                    | Punct::LBracket
                    | Punct::Plus
                    | Punct::Minus
                    | Punct::Bang
                    | Punct::Tilde
                    | Punct::PlusPlus
                    | Punct::MinusMinus
                    | Punct::Slash
                    | Punct::SlashEq
                    | Punct::Hash
                    | Punct::At
            ),
            TokenKind::Ident { name, .. } => {
                !(&**name == "in" || &**name == "of" || &**name == "instanceof")
            }
            TokenKind::PrivateName(_) => true,
            _ => true,
        }
    }

    fn parse_conditional_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let test = self.parse_binary_expression(0)?;
        if !self.at(Punct::Question) {
            return Ok(test);
        }
        if self.arrow_stops(&test)? {
            return Ok(test);
        }
        self.advance()?;
        let consequent = self.with_in(true, |p| p.parse_assignment_expression())?;
        self.expect(Punct::Colon)?;
        let alternate = self.parse_assignment_expression()?;
        Ok(Expression::Conditional(Box::new(ConditionalExpression {
            test,
            consequent,
            alternate,
            span: self.span_from(start),
        })))
    }

    fn binary_op(&self) -> Option<(BinOrLogical, u8)> {
        use BinOrLogical::*;
        let r = match &self.tok.kind {
            TokenKind::Punct(p) => match p {
                Punct::QuestionQuestion => (Logical(LogicalOp::Nullish), PREC_NULLISH),
                Punct::PipePipe => (Logical(LogicalOp::Or), PREC_OR),
                Punct::AmpAmp => (Logical(LogicalOp::And), PREC_AND),
                Punct::Pipe => (Binary(BinaryOp::BitOr), PREC_BIT_OR),
                Punct::Caret => (Binary(BinaryOp::BitXor), PREC_BIT_XOR),
                Punct::Amp => (Binary(BinaryOp::BitAnd), PREC_BIT_AND),
                Punct::EqEq => (Binary(BinaryOp::EqEq), PREC_EQUALITY),
                Punct::NotEq => (Binary(BinaryOp::NotEq), PREC_EQUALITY),
                Punct::EqEqEq => (Binary(BinaryOp::EqEqEq), PREC_EQUALITY),
                Punct::NotEqEq => (Binary(BinaryOp::NotEqEq), PREC_EQUALITY),
                Punct::Lt => (Binary(BinaryOp::Lt), PREC_RELATIONAL),
                Punct::Gt => (Binary(BinaryOp::Gt), PREC_RELATIONAL),
                Punct::LtEq => (Binary(BinaryOp::LtEq), PREC_RELATIONAL),
                Punct::GtEq => (Binary(BinaryOp::GtEq), PREC_RELATIONAL),
                Punct::Shl => (Binary(BinaryOp::Shl), PREC_SHIFT),
                Punct::Shr => (Binary(BinaryOp::Shr), PREC_SHIFT),
                Punct::UShr => (Binary(BinaryOp::UShr), PREC_SHIFT),
                Punct::Plus => (Binary(BinaryOp::Add), PREC_ADDITIVE),
                Punct::Minus => (Binary(BinaryOp::Sub), PREC_ADDITIVE),
                Punct::Star => (Binary(BinaryOp::Mul), PREC_MULTIPLICATIVE),
                Punct::Slash => (Binary(BinaryOp::Div), PREC_MULTIPLICATIVE),
                Punct::Percent => (Binary(BinaryOp::Mod), PREC_MULTIPLICATIVE),
                Punct::StarStar => (Binary(BinaryOp::Exp), PREC_EXP),
                _ => return None,
            },
            TokenKind::Ident {
                name,
                escaped: false,
            } => match &**name {
                "in" if self.allow_in => (Binary(BinaryOp::In), PREC_RELATIONAL),
                "instanceof" => (Binary(BinaryOp::Instanceof), PREC_RELATIONAL),
                _ => return None,
            },
            _ => return None,
        };
        Some(r)
    }

    fn parse_binary_expression(&mut self, min_prec: u8) -> Result<Expression> {
        let start = self.tok.span.start;
        let mut left = if let TokenKind::PrivateName(name) = &self.tok.kind {
            // `#x in obj`
            let name = name.clone();
            let span = self.tok.span;
            if min_prec > PREC_RELATIONAL || !self.allow_in {
                return self.unexpected();
            }
            self.advance()?;
            if !self.at_kw("in") {
                return self.unexpected();
            }
            self.advance()?;
            let left = self.intern_private(&name, span)?;
            let right = self.parse_binary_expression(PREC_RELATIONAL + 1)?;
            self.no_arrow(&right)?;
            Expression::PrivateIn(Box::new(PrivateInExpression {
                left,
                right,
                span: self.span_from(start),
            }))
        } else {
            self.parse_unary_expression()?
        };
        loop {
            let Some((op, prec)) = self.binary_op() else {
                break;
            };
            if prec < min_prec {
                break;
            }
            if self.arrow_stops(&left)? {
                break;
            }
            if prec == PREC_EXP {
                // UnaryExpression ** x is a syntax error (needs parens).
                if matches!(left, Expression::Unary(_) | Expression::Await(_)) {
                    return self.err("unary operator before ** needs parentheses", left.span());
                }
            }
            self.advance()?;
            // ** is right associative; everything else left associative.
            let next_min = if prec == PREC_EXP { prec } else { prec + 1 };
            let right = self.parse_binary_expression(next_min)?;
            self.no_arrow(&right)?;
            let span = self.span_from(start);
            left = match op {
                BinOrLogical::Binary(op) => Expression::Binary(Box::new(BinaryExpression {
                    op,
                    left,
                    right,
                    span,
                })),
                BinOrLogical::Logical(op) => {
                    // `??` cannot mix with `||`/`&&` without parentheses.
                    let is_mixed = |e: &Expression| match e {
                        Expression::Logical(l) => {
                            (l.op == LogicalOp::Nullish) != (op == LogicalOp::Nullish)
                        }
                        _ => false,
                    };
                    if is_mixed(&left) || is_mixed(&right) {
                        return self.err("cannot mix ?? with || or && without parentheses", span);
                    }
                    Expression::Logical(Box::new(LogicalExpression {
                        op,
                        left,
                        right,
                        span,
                    }))
                }
            };
        }
        Ok(left)
    }

    fn parse_unary_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let op = match &self.tok.kind {
            TokenKind::Punct(Punct::Minus) => Some(UnaryOp::Minus),
            TokenKind::Punct(Punct::Plus) => Some(UnaryOp::Plus),
            TokenKind::Punct(Punct::Bang) => Some(UnaryOp::Not),
            TokenKind::Punct(Punct::Tilde) => Some(UnaryOp::BitNot),
            TokenKind::Ident {
                name,
                escaped: false,
            } => match &**name {
                "typeof" => Some(UnaryOp::Typeof),
                "void" => Some(UnaryOp::Void),
                "delete" => Some(UnaryOp::Delete),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            self.advance()?;
            let argument = self.parse_unary_expression()?;
            self.no_arrow(&argument)?;
            if op == UnaryOp::Delete {
                let inner = argument.unparen();
                if self.ctx.strict && matches!(inner, Expression::Ident(_)) {
                    return self.err(
                        "delete of an unqualified identifier in strict mode",
                        argument.span(),
                    );
                }
                if let Expression::Member(m) = inner {
                    if matches!(m.property, MemberProperty::Private(_)) {
                        return self.err("private fields cannot be deleted", argument.span());
                    }
                }
                if let Expression::OptionalChain(chain, _) = inner {
                    if let Expression::Member(m) = chain.unparen() {
                        if matches!(m.property, MemberProperty::Private(_)) {
                            return self.err("private fields cannot be deleted", argument.span());
                        }
                    }
                }
            }
            return Ok(Expression::Unary(Box::new(UnaryExpression {
                op,
                argument,
                span: self.span_from(start),
            })));
        }
        if self.at(Punct::PlusPlus) || self.at(Punct::MinusMinus) {
            let increment = self.at(Punct::PlusPlus);
            self.advance()?;
            let argument = self.parse_unary_expression()?;
            let argument = self.check_update_target(argument)?;
            return Ok(Expression::Update(Box::new(UpdateExpression {
                increment,
                prefix: true,
                argument,
                span: self.span_from(start),
            })));
        }
        if self.at_kw("await") && self.await_is_keyword() {
            return self.parse_await();
        }
        self.parse_postfix_expression()
    }

    pub(crate) fn await_is_keyword(&self) -> bool {
        self.ctx.is_async || self.ctx.module_top
    }

    fn parse_await(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let span = self.tok.span;
        if self.ctx.in_params {
            return self.err("await expression not allowed in formal parameters", span);
        }
        if self.ctx.in_static_block {
            return self.err("await is not allowed in class static blocks", span);
        }
        self.advance()?;
        if self.pending_yield_await.is_none() {
            self.pending_yield_await = Some(span);
        }
        let argument = self.parse_unary_expression()?;
        Ok(Expression::Await(Box::new(AwaitExpression {
            argument,
            span: self.span_from(start),
        })))
    }

    fn check_update_target(&mut self, e: Expression) -> Result<Expression> {
        match e.unparen() {
            Expression::Ident(id) => {
                if self.ctx.strict && (&*id.name == "eval" || &*id.name == "arguments") {
                    return self.err("invalid increment/decrement operand", e.span());
                }
                Ok(e)
            }
            Expression::Member(_) => Ok(e),
            // Annex B: call expressions are runtime ReferenceErrors in sloppy code.
            Expression::Call(c) if !self.ctx.strict && !c.optional => Ok(e),
            _ => self.err("invalid increment/decrement operand", e.span()),
        }
    }

    /// An arrow function is an AssignmentExpression and cannot be an
    /// operand. When the token that would continue the expression is on
    /// the same line that is an error; after a line terminator the
    /// expression simply ends (ASI) and the caller stops.
    fn arrow_stops(&self, e: &Expression) -> Result<bool> {
        if !matches!(e, Expression::Arrow(_)) {
            return Ok(false);
        }
        if self.tok.newline_before {
            return Ok(true);
        }
        self.err(
            "arrow function must be parenthesised to be used as an operand",
            self.tok.span,
        )
    }

    /// Rejects an unparenthesised arrow function used as an operand.
    pub(crate) fn no_arrow(&self, e: &Expression) -> Result<()> {
        if matches!(e, Expression::Arrow(_)) {
            return self.err(
                "arrow function must be parenthesised to be used as an operand",
                e.span(),
            );
        }
        Ok(())
    }

    fn parse_postfix_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let expr = self.parse_lhs_expression()?;
        if (self.at(Punct::PlusPlus) || self.at(Punct::MinusMinus)) && !self.tok.newline_before {
            self.no_arrow(&expr)?;
            let increment = self.at(Punct::PlusPlus);
            let argument = self.check_update_target(expr)?;
            self.advance()?;
            return Ok(Expression::Update(Box::new(UpdateExpression {
                increment,
                prefix: false,
                argument,
                span: self.span_from(start),
            })));
        }
        Ok(expr)
    }

    /// LeftHandSideExpression: member accesses, calls, `new`, optional
    /// chains and tagged templates.
    pub(crate) fn parse_lhs_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let mut expr = if self.at_kw("new") {
            self.parse_new_expression()?
        } else if self.at_kw("super") {
            self.parse_super()?
        } else if self.at_kw("import") {
            self.parse_import_expression()?
        } else {
            self.parse_primary_expression()?
        };
        let mut in_chain = false;
        loop {
            if matches!(expr, Expression::Arrow(_))
                && matches!(
                    self.tok.kind,
                    TokenKind::Punct(
                        Punct::Dot | Punct::LParen | Punct::LBracket | Punct::QuestionDot
                    ) | TokenKind::Template { .. }
                )
                && self.arrow_stops(&expr)?
            {
                break;
            }
            match &self.tok.kind {
                TokenKind::Punct(Punct::Dot) => {
                    self.advance()?;
                    let property = self.parse_member_name()?;
                    expr = Expression::Member(Box::new(MemberExpression {
                        object: expr,
                        property,
                        optional: false,
                        span: self.span_from(start),
                    }));
                }
                TokenKind::Punct(Punct::QuestionDot) => {
                    self.advance()?;
                    in_chain = true;
                    if self.at(Punct::LParen) {
                        let arguments = self.parse_arguments()?;
                        expr = Expression::Call(Box::new(CallExpression {
                            callee: expr,
                            arguments,
                            optional: true,
                            span: self.span_from(start),
                        }));
                    } else if self.at(Punct::LBracket) {
                        self.advance()?;
                        let prop = self.with_in(true, |p| p.parse_expression())?;
                        self.expect(Punct::RBracket)?;
                        expr = Expression::Member(Box::new(MemberExpression {
                            object: expr,
                            property: MemberProperty::Computed(prop),
                            optional: true,
                            span: self.span_from(start),
                        }));
                    } else if matches!(self.tok.kind, TokenKind::Template { .. }) {
                        return self.err(
                            "tagged template cannot be used in an optional chain",
                            self.tok.span,
                        );
                    } else {
                        let property = self.parse_member_name()?;
                        expr = Expression::Member(Box::new(MemberExpression {
                            object: expr,
                            property,
                            optional: true,
                            span: self.span_from(start),
                        }));
                    }
                }
                TokenKind::Punct(Punct::LBracket) => {
                    self.advance()?;
                    let prop = self.with_in(true, |p| p.parse_expression())?;
                    self.expect(Punct::RBracket)?;
                    expr = Expression::Member(Box::new(MemberExpression {
                        object: expr,
                        property: MemberProperty::Computed(prop),
                        optional: false,
                        span: self.span_from(start),
                    }));
                }
                TokenKind::Punct(Punct::LParen) => {
                    let arguments = self.parse_arguments()?;
                    if let Expression::Ident(id) = &expr {
                        if &*id.name == "eval" {
                            self.has_direct_eval = true;
                        }
                    }
                    expr = Expression::Call(Box::new(CallExpression {
                        callee: expr,
                        arguments,
                        optional: false,
                        span: self.span_from(start),
                    }));
                }
                TokenKind::Template { .. } => {
                    if in_chain {
                        return self.err(
                            "tagged template cannot be used in an optional chain",
                            self.tok.span,
                        );
                    }
                    let quasi = self.parse_template_literal(true)?;
                    expr = Expression::TaggedTemplate(Box::new(TaggedTemplate {
                        tag: expr,
                        quasi,
                        span: self.span_from(start),
                    }));
                }
                _ => break,
            }
        }
        if in_chain {
            let span = self.span_from(start);
            expr = Expression::OptionalChain(Box::new(expr), span);
        }
        Ok(expr)
    }

    fn parse_member_name(&mut self) -> Result<MemberProperty> {
        if let TokenKind::PrivateName(name) = &self.tok.kind {
            let name = name.clone();
            let span = self.tok.span;
            self.advance()?;
            return Ok(MemberProperty::Private(self.intern_private(&name, span)?));
        }
        Ok(MemberProperty::Ident(self.parse_identifier_name()?))
    }

    /// Record a private-name reference for resolution at the end of the
    /// enclosing class body.
    pub(crate) fn intern_private(&mut self, name: &str, span: Span) -> Result<PrivateName> {
        if self.private_envs.is_empty() {
            return self.err(format!("private name #{name} used outside a class"), span);
        }
        let name = self.intern(name);
        self.private_envs
            .last_mut()
            .unwrap()
            .referenced
            .push((name.clone(), span));
        Ok(PrivateName { name, span })
    }

    /// `new` MemberExpression Arguments?, `new.target`.
    fn parse_new_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        self.advance()?; // new
        if self.at(Punct::Dot) {
            self.advance()?;
            if !self.at_kw("target") {
                return self.unexpected();
            }
            if !self.ctx.new_target {
                return self.err(
                    "new.target is not allowed here",
                    Span::new(start, self.tok.span.end),
                );
            }
            self.advance()?;
            return Ok(Expression::MetaProperty(MetaProperty {
                kind: MetaPropertyKind::NewTarget,
                span: self.span_from(start),
            }));
        }
        if self.at_kw("import") && matches!(self.peek()?.kind, TokenKind::Punct(Punct::LParen)) {
            return self.err("cannot use new with import", self.tok.span);
        }
        // The callee is a MemberExpression (no calls, no optional chains).
        let callee_start = self.tok.span.start;
        let mut callee = if self.at_kw("new") {
            self.parse_new_expression()?
        } else if self.at_kw("super") {
            self.parse_super()?
        } else if self.at_kw("import") {
            self.parse_import_expression()?
        } else {
            self.parse_primary_expression()?
        };
        self.no_arrow(&callee)?;
        loop {
            match &self.tok.kind {
                TokenKind::Punct(Punct::Dot) => {
                    self.advance()?;
                    let property = self.parse_member_name()?;
                    callee = Expression::Member(Box::new(MemberExpression {
                        object: callee,
                        property,
                        optional: false,
                        span: self.span_from(callee_start),
                    }));
                }
                TokenKind::Punct(Punct::LBracket) => {
                    self.advance()?;
                    let prop = self.with_in(true, |p| p.parse_expression())?;
                    self.expect(Punct::RBracket)?;
                    callee = Expression::Member(Box::new(MemberExpression {
                        object: callee,
                        property: MemberProperty::Computed(prop),
                        optional: false,
                        span: self.span_from(callee_start),
                    }));
                }
                TokenKind::Template { .. } => {
                    let quasi = self.parse_template_literal(true)?;
                    callee = Expression::TaggedTemplate(Box::new(TaggedTemplate {
                        tag: callee,
                        quasi,
                        span: self.span_from(callee_start),
                    }));
                }
                TokenKind::Punct(Punct::QuestionDot) => {
                    return self.err("optional chain cannot be the callee of new", self.tok.span);
                }
                _ => break,
            }
        }
        let arguments = if self.at(Punct::LParen) {
            self.parse_arguments()?
        } else {
            Vec::new()
        };
        Ok(Expression::New(Box::new(NewExpression {
            callee,
            arguments,
            span: self.span_from(start),
        })))
    }

    fn parse_super(&mut self) -> Result<Expression> {
        let span = self.tok.span;
        self.advance()?;
        if self.at(Punct::LParen) {
            if !self.ctx.super_call {
                return self.err(
                    "'super' call is only allowed in derived class constructors",
                    span,
                );
            }
        } else if self.at(Punct::Dot) || self.at(Punct::LBracket) {
            if !self.ctx.super_property {
                return self.err("'super' property access is only allowed in methods", span);
            }
            if self.at(Punct::Dot) {
                let next = self.peek()?;
                if matches!(next.kind, TokenKind::PrivateName(_)) {
                    return self.err("private names are not allowed on super", next.span);
                }
            }
        } else {
            return self.err(
                "'super' must be followed by an argument list or property access",
                span,
            );
        }
        Ok(Expression::Super(span))
    }

    fn parse_import_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let span = self.tok.span;
        self.advance()?;
        if self.at(Punct::Dot) {
            self.advance()?;
            if !self.at_kw("meta") {
                return self.unexpected();
            }
            if !self.lexer.module {
                return self.err(
                    "import.meta is only valid in module code",
                    Span::new(start, self.tok.span.end),
                );
            }
            self.advance()?;
            return Ok(Expression::MetaProperty(MetaProperty {
                kind: MetaPropertyKind::ImportMeta,
                span: self.span_from(start),
            }));
        }
        if !self.at(Punct::LParen) {
            return self.err("unexpected 'import'", span);
        }
        self.advance()?;
        let source = self.with_in(true, |p| p.parse_assignment_expression())?;
        let mut options = None;
        if self.eat(Punct::Comma)? && !self.at(Punct::RParen) {
            options = Some(self.with_in(true, |p| p.parse_assignment_expression())?);
            self.eat(Punct::Comma)?;
        }
        self.expect(Punct::RParen)?;
        Ok(Expression::Import(Box::new(ImportCall {
            source,
            options,
            span: self.span_from(start),
        })))
    }

    pub(crate) fn parse_arguments(&mut self) -> Result<Vec<Expression>> {
        self.expect(Punct::LParen)?;
        let mut args = Vec::new();
        self.with_in(true, |p| {
            while !p.at(Punct::RParen) {
                if p.at(Punct::Ellipsis) {
                    let start = p.tok.span.start;
                    p.advance()?;
                    let argument = p.parse_assignment_expression()?;
                    args.push(Expression::Spread(Box::new(SpreadElement {
                        argument,
                        span: p.span_from(start),
                    })));
                } else {
                    args.push(p.parse_assignment_expression()?);
                }
                if !p.at(Punct::RParen) {
                    p.expect(Punct::Comma)?;
                }
            }
            Ok(())
        })?;
        self.expect(Punct::RParen)?;
        Ok(args)
    }

    pub(crate) fn parse_primary_expression(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        let span = self.tok.span;
        match &self.tok.kind {
            TokenKind::Ident { name, escaped } => {
                let name = name.clone();
                let escaped = *escaped;
                if !escaped {
                    match &*name {
                        "this" => {
                            self.advance()?;
                            return Ok(Expression::This(span));
                        }
                        "null" => {
                            self.advance()?;
                            return Ok(Expression::Null(span));
                        }
                        "true" => {
                            self.advance()?;
                            return Ok(Expression::Bool(true, span));
                        }
                        "false" => {
                            self.advance()?;
                            return Ok(Expression::Bool(false, span));
                        }
                        "function" => return self.parse_function_expression(false),
                        "class" => return self.parse_class_expression(),
                        "async" => {
                            let next = self.peek()?;
                            if matches!(&next.kind, TokenKind::Ident { name, escaped: false } if &**name == "function")
                                && !next.newline_before
                            {
                                return self.parse_function_expression(true);
                            }
                        }
                        "let" if self.ctx.strict => {
                            return self.err("unexpected strict mode reserved word 'let'", span);
                        }
                        _ => {}
                    }
                }
                self.check_identifier(&name, span, escaped)?;
                if self.ctx.no_arguments && &*name == "arguments" {
                    return self.err(
                        "'arguments' is not allowed in class field initializers or static blocks",
                        span,
                    );
                }
                self.advance()?;
                let name = self.intern(&name);
                Ok(Expression::Ident(Ident { name, span }))
            }
            TokenKind::Number { value, kind } => {
                let value = *value;
                if self.ctx.strict
                    && matches!(
                        kind,
                        crate::lexer::NumberKind::LegacyOctal
                            | crate::lexer::NumberKind::LegacyDecimal
                    )
                {
                    return self.err("legacy octal literals are not allowed in strict mode", span);
                }
                self.advance()?;
                Ok(Expression::Number(value, span))
            }
            TokenKind::BigInt(digits) => {
                let digits = digits.clone();
                self.advance()?;
                Ok(Expression::BigInt(digits, span))
            }
            TokenKind::String {
                value,
                legacy_octal,
            } => {
                let value: Rc<[u16]> = Rc::from(&**value);
                if self.ctx.strict && *legacy_octal {
                    return self.err(
                        "octal escape sequences are not allowed in strict mode",
                        span,
                    );
                }
                self.advance()?;
                Ok(Expression::String(StringLiteral { value, span }))
            }
            TokenKind::Template { .. } => Ok(Expression::Template(Box::new(
                self.parse_template_literal(false)?,
            ))),
            TokenKind::RegExp { .. } => unreachable!("regexp tokens are produced by rescan only"),
            TokenKind::Punct(Punct::Slash) | TokenKind::Punct(Punct::SlashEq) => {
                let tok = self.lexer.rescan_regexp(&self.tok)?;
                self.tok = tok;
                let TokenKind::RegExp { pattern, flags } = &self.tok.kind else {
                    unreachable!()
                };
                let (pattern, flags) = (pattern.clone(), flags.clone());
                let span = self.tok.span;
                if let Err(msg) = gable_js_regex::syntax::validate(&pattern, &flags) {
                    return self.err(format!("invalid regular expression: {msg}"), span);
                }
                self.advance()?;
                Ok(Expression::RegExp(Box::new(RegExpLiteral {
                    pattern,
                    flags,
                    span,
                })))
            }
            TokenKind::Punct(Punct::LBracket) => self.parse_array_literal(),
            TokenKind::Punct(Punct::LBrace) => self.parse_object_literal(),
            TokenKind::Punct(Punct::LParen) => self.parse_paren_or_arrow(start),
            TokenKind::Punct(Punct::Hash) | TokenKind::Punct(Punct::At) => self.unexpected(),
            _ => self.unexpected(),
        }
    }

    pub(crate) fn parse_template_literal(&mut self, tagged: bool) -> Result<TemplateLiteral> {
        let start = self.tok.span.start;
        let mut quasis = Vec::new();
        let mut expressions = Vec::new();
        loop {
            let TokenKind::Template { cooked, raw, tail } = &self.tok.kind else {
                return self.unexpected();
            };
            let tail = *tail;
            if cooked.is_none() && !tagged {
                return self.err("invalid escape sequence in template literal", self.tok.span);
            }
            quasis.push(TemplateElement {
                cooked: cooked.as_ref().map(|c| Rc::from(&**c)),
                raw: Rc::from(&**raw),
                span: self.tok.span,
            });
            if tail {
                self.advance()?;
                break;
            }
            self.advance()?;
            expressions.push(self.with_in(true, |p| p.parse_expression())?);
            if !self.at(Punct::RBrace) {
                return self.unexpected();
            }
            // Re-lex from the `}` as a template continuation.
            self.tok = self.lexer.rescan_template_continuation(&self.tok)?;
        }
        Ok(TemplateLiteral {
            quasis,
            expressions,
            span: self.span_from(start),
        })
    }

    fn parse_array_literal(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        self.expect(Punct::LBracket)?;
        let mut elements = Vec::new();
        let mut trailing_comma_after_spread = false;
        self.with_in(true, |p| {
            while !p.at(Punct::RBracket) {
                if p.at(Punct::Comma) {
                    p.advance()?;
                    elements.push(None);
                    continue;
                }
                let is_spread = p.at(Punct::Ellipsis);
                let el = if is_spread {
                    let s = p.tok.span.start;
                    p.advance()?;
                    let argument = p.parse_assignment_expression()?;
                    Expression::Spread(Box::new(SpreadElement {
                        argument,
                        span: p.span_from(s),
                    }))
                } else {
                    p.parse_assignment_expression()?
                };
                elements.push(Some(el));
                if !p.at(Punct::RBracket) {
                    p.expect(Punct::Comma)?;
                    if is_spread && p.at(Punct::RBracket) {
                        trailing_comma_after_spread = true;
                    }
                }
            }
            Ok(())
        })?;
        self.expect(Punct::RBracket)?;
        Ok(Expression::Array(Box::new(ArrayExpression {
            elements,
            trailing_comma_after_spread,
            span: self.span_from(start),
        })))
    }

    /// Parse a property name: identifier, string, number, bigint, computed
    /// or private (when `allow_private`).
    pub(crate) fn parse_property_key(&mut self, allow_private: bool) -> Result<PropertyKey> {
        let span = self.tok.span;
        match &self.tok.kind {
            TokenKind::Ident { .. } => Ok(PropertyKey::Ident(self.parse_identifier_name()?)),
            TokenKind::String {
                value,
                legacy_octal,
            } => {
                if self.ctx.strict && *legacy_octal {
                    return self.err(
                        "octal escape sequences are not allowed in strict mode",
                        span,
                    );
                }
                let value: Rc<[u16]> = Rc::from(&**value);
                self.advance()?;
                Ok(PropertyKey::String(StringLiteral { value, span }))
            }
            TokenKind::Number { value, kind } => {
                if self.ctx.strict
                    && matches!(
                        kind,
                        crate::lexer::NumberKind::LegacyOctal
                            | crate::lexer::NumberKind::LegacyDecimal
                    )
                {
                    return self.err("legacy octal literals are not allowed in strict mode", span);
                }
                let v = *value;
                self.advance()?;
                Ok(PropertyKey::Number(v, span))
            }
            TokenKind::BigInt(d) => {
                let d = d.clone();
                self.advance()?;
                Ok(PropertyKey::BigInt(d, span))
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.advance()?;
                let e = self.with_in(true, |p| p.parse_assignment_expression())?;
                self.expect(Punct::RBracket)?;
                Ok(PropertyKey::Computed(Box::new(e)))
            }
            TokenKind::PrivateName(name) if allow_private => {
                let name = name.clone();
                self.advance()?;
                if &*name == "constructor" {
                    return self.err(
                        "classes cannot have a private member named #constructor",
                        span,
                    );
                }
                Ok(PropertyKey::Private(PrivateName {
                    name: self.intern(&name),
                    span,
                }))
            }
            _ => self.unexpected(),
        }
    }

    fn parse_object_literal(&mut self) -> Result<Expression> {
        let start = self.tok.span.start;
        self.expect(Punct::LBrace)?;
        let mut properties = Vec::new();
        let mut seen_proto: Option<Span> = None;
        let mut trailing_comma_after_spread = false;
        self.with_in(true, |p| {
            while !p.at(Punct::RBrace) {
                if p.at(Punct::Ellipsis) {
                    let s = p.tok.span.start;
                    p.advance()?;
                    let argument = p.parse_assignment_expression()?;
                    properties.push(ObjectProperty::Spread(SpreadElement {
                        argument,
                        span: p.span_from(s),
                    }));
                    trailing_comma_after_spread = p.at(Punct::Comma);
                } else {
                    trailing_comma_after_spread = false;
                    let prop = p.parse_object_property()?;
                    if prop.is_proto_setter {
                        if seen_proto.is_some() {
                            // Deferred: a duplicate __proto__ is fine in a
                            // destructuring pattern.
                            if p.pending_cover.is_none() {
                                p.pending_cover = Some((
                                    prop.span,
                                    "duplicate __proto__ fields are not allowed in object literals",
                                ));
                            }
                        }
                        seen_proto = Some(prop.span);
                    }
                    properties.push(ObjectProperty::Property(prop));
                }
                if !p.at(Punct::RBrace) {
                    p.expect(Punct::Comma)?;
                }
            }
            Ok(())
        })?;
        self.expect(Punct::RBrace)?;
        Ok(Expression::Object(Box::new(ObjectExpression {
            properties,
            trailing_comma_after_spread,
            span: self.span_from(start),
        })))
    }

    fn parse_object_property(&mut self) -> Result<Property> {
        let start = self.tok.span.start;
        // Modifiers: get / set / async / *
        let mut is_async = false;
        let mut is_generator = false;
        let mut accessor: Option<PropertyKind> = None;

        if let TokenKind::Ident {
            name,
            escaped: false,
        } = &self.tok.kind
        {
            let name = name.clone();
            let next = self.peek()?;
            let next_is_key_start = !matches!(
                next.kind,
                TokenKind::Punct(Punct::Comma)
                    | TokenKind::Punct(Punct::RBrace)
                    | TokenKind::Punct(Punct::Colon)
                    | TokenKind::Punct(Punct::LParen)
                    | TokenKind::Punct(Punct::Eq)
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
                    "get" => {
                        accessor = Some(PropertyKind::Get);
                        self.advance()?;
                    }
                    "set" => {
                        accessor = Some(PropertyKind::Set);
                        self.advance()?;
                    }
                    _ => {}
                }
            }
        }
        if !is_async && accessor.is_none() && self.eat(Punct::Star)? {
            is_generator = true;
        }

        let key_tok = self.tok.clone();
        let key = self.parse_property_key(false)?;
        let is_ident_key = matches!(key_tok.kind, TokenKind::Ident { .. });

        if let Some(kind) = accessor {
            let func = self.parse_method(
                FunctionKind::Normal,
                true,
                false,
                kind == PropertyKind::Get,
                kind == PropertyKind::Set,
            )?;
            return Ok(Property {
                key,
                value: Expression::Function(Box::new(func)),
                kind,
                shorthand: false,
                is_proto_setter: false,
                span: self.span_from(start),
            });
        }
        if is_async || is_generator || self.at(Punct::LParen) {
            let kind = match (is_async, is_generator) {
                (true, true) => FunctionKind::AsyncGenerator,
                (true, false) => FunctionKind::Async,
                (false, true) => FunctionKind::Generator,
                _ => FunctionKind::Normal,
            };
            let func = self.parse_method(kind, true, false, false, false)?;
            return Ok(Property {
                key,
                value: Expression::Function(Box::new(func)),
                kind: PropertyKind::Method,
                shorthand: false,
                is_proto_setter: false,
                span: self.span_from(start),
            });
        }
        if self.eat(Punct::Colon)? {
            let value = self.parse_assignment_expression()?;
            let is_proto_setter = match &key {
                PropertyKey::Ident(i) => &*i.name == "__proto__",
                PropertyKey::String(s) => {
                    &*s.value == "__proto__".encode_utf16().collect::<Vec<_>>().as_slice()
                }
                _ => false,
            };
            return Ok(Property {
                key,
                value,
                kind: PropertyKind::Init,
                shorthand: false,
                is_proto_setter,
                span: self.span_from(start),
            });
        }
        // Shorthand `a` or cover `a = 1`.
        if !is_ident_key {
            return self.unexpected();
        }
        let PropertyKey::Ident(id) = &key else {
            unreachable!()
        };
        let TokenKind::Ident { escaped, .. } = key_tok.kind else {
            unreachable!()
        };
        self.check_identifier(&id.name, id.span, escaped)?;
        if self.ctx.no_arguments && &*id.name == "arguments" {
            return self.err(
                "'arguments' is not allowed in class field initializers or static blocks",
                id.span,
            );
        }
        let ident = Ident {
            name: id.name.clone(),
            span: id.span,
        };
        if self.at(Punct::Eq) {
            // CoverInitializedName: only valid as a destructuring pattern.
            let eq_span = self.tok.span;
            self.advance()?;
            let right = self.parse_assignment_expression()?;
            if self.pending_cover.is_none() {
                self.pending_cover = Some((eq_span, "invalid shorthand property initializer"));
            }
            let span = self.span_from(start);
            let value = Expression::Assignment(Box::new(AssignmentExpression {
                op: AssignOp::Assign,
                left: Pattern::Ident(ident),
                right,
                span,
            }));
            return Ok(Property {
                key,
                value,
                kind: PropertyKind::Init,
                shorthand: true,
                is_proto_setter: false,
                span,
            });
        }
        Ok(Property {
            key,
            value: Expression::Ident(ident),
            kind: PropertyKind::Init,
            shorthand: true,
            is_proto_setter: false,
            span: self.span_from(start),
        })
    }

    /// `(` ... `)`: a parenthesised expression or an arrow head.
    fn parse_paren_or_arrow(&mut self, start: u32) -> Result<Expression> {
        self.expect(Punct::LParen)?;
        let saved_cover = self.pending_cover.take();
        let saved_ya = self.pending_yield_await.take();

        let mut items: Vec<Expression> = Vec::new();
        let mut rest: Option<Pattern> = None;
        let mut trailing_comma = false;
        self.with_in(true, |p| {
            while !p.at(Punct::RParen) {
                if p.at(Punct::Ellipsis) {
                    let s = p.tok.span.start;
                    p.advance()?;
                    let target = p.parse_binding_target()?;
                    rest = Some(Pattern::Rest(Box::new(RestElement {
                        argument: target,
                        span: p.span_from(s),
                    })));
                    if !p.at(Punct::RParen) {
                        return p.err("rest parameter must be last", p.tok.span);
                    }
                    break;
                }
                items.push(p.parse_assignment_expression()?);
                if !p.at(Punct::RParen) {
                    p.expect(Punct::Comma)?;
                    if p.at(Punct::RParen) {
                        trailing_comma = true;
                    }
                }
            }
            Ok(())
        })?;
        let close = self.expect(Punct::RParen)?;

        if self.at(Punct::Arrow) && !self.tok.newline_before {
            if let Some(span) = self.pending_yield_await {
                return self.err(
                    "yield/await expressions are not allowed in arrow parameters",
                    span,
                );
            }
            self.pending_yield_await = saved_ya;
            let mut params = Vec::new();
            for item in items {
                params.push(self.expr_to_pattern(item, true)?);
            }
            if let Some(r) = rest {
                params.push(r);
            }
            self.pending_cover = saved_cover;
            return self.parse_arrow_from_params(start, params, false, false);
        }

        // Not an arrow: it was a parenthesised expression after all.
        if let Some((span, msg)) = self.pending_cover.take() {
            return self.err(msg, span);
        }
        self.pending_cover = saved_cover;
        if self.pending_yield_await.is_none() {
            self.pending_yield_await = saved_ya;
        } else if saved_ya.is_some() {
            self.pending_yield_await = saved_ya;
        }
        if rest.is_some() || trailing_comma || items.is_empty() {
            return self.err("unexpected token )", close);
        }
        let span = Span::new(start, close.end);
        let inner = if items.len() == 1 {
            items.pop().unwrap()
        } else {
            let s = Span::new(items[0].span().start, items.last().unwrap().span().end);
            Expression::Sequence(Box::new(SequenceExpression {
                expressions: items,
                span: s,
            }))
        };
        Ok(Expression::Paren(Box::new(inner), span))
    }

    /// Convert the arguments of `async(...)` into arrow parameters.
    fn arguments_to_params(&mut self, args: Vec<Expression>) -> Result<Vec<Pattern>> {
        let mut params = Vec::new();
        let n = args.len();
        for (i, a) in args.into_iter().enumerate() {
            match a {
                Expression::Spread(s) => {
                    if i + 1 != n {
                        return self.err("rest parameter must be last", s.span);
                    }
                    let argument = self.expr_to_pattern(s.argument, true)?;
                    if matches!(argument, Pattern::Assignment(_)) {
                        return self.err("rest parameter may not have an initializer", s.span);
                    }
                    params.push(Pattern::Rest(Box::new(RestElement {
                        argument,
                        span: s.span,
                    })));
                }
                e => params.push(self.expr_to_pattern(e, true)?),
            }
        }
        Ok(params)
    }
}
