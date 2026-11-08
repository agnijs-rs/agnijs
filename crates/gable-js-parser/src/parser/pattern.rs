//! Binding patterns, and the conversion of cover-grammar expressions into
//! assignment patterns and arrow parameters.

use super::Parser;
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};
use std::rc::Rc;

impl<'s> Parser<'s> {
    /// BindingIdentifier | BindingPattern.
    pub(crate) fn parse_binding_target(&mut self) -> Result<Pattern> {
        match &self.tok.kind {
            TokenKind::Punct(Punct::LBracket) => self.parse_array_binding_pattern(),
            TokenKind::Punct(Punct::LBrace) => self.parse_object_binding_pattern(),
            _ => Ok(Pattern::Ident(self.parse_binding_identifier()?)),
        }
    }

    /// BindingElement: a target with an optional initializer.
    pub(crate) fn parse_binding_element(&mut self) -> Result<Pattern> {
        let start = self.tok.span.start;
        let target = self.parse_binding_target()?;
        if self.at(Punct::Eq) {
            self.advance()?;
            let right = self.with_in(true, |p| p.parse_assignment_expression())?;
            return Ok(Pattern::Assignment(Box::new(AssignmentPattern { left: target, right, span: self.span_from(start) })));
        }
        Ok(target)
    }

    fn parse_array_binding_pattern(&mut self) -> Result<Pattern> {
        let start = self.tok.span.start;
        self.expect(Punct::LBracket)?;
        let mut elements = Vec::new();
        while !self.at(Punct::RBracket) {
            if self.at(Punct::Comma) {
                self.advance()?;
                elements.push(None);
                continue;
            }
            if self.at(Punct::Ellipsis) {
                let s = self.tok.span.start;
                self.advance()?;
                let argument = self.parse_binding_target()?;
                elements.push(Some(Pattern::Rest(Box::new(RestElement { argument, span: self.span_from(s) }))));
                if !self.at(Punct::RBracket) {
                    return self.err("rest element must be last", self.tok.span);
                }
                break;
            }
            elements.push(Some(self.parse_binding_element()?));
            if !self.at(Punct::RBracket) {
                self.expect(Punct::Comma)?;
            }
        }
        self.expect(Punct::RBracket)?;
        Ok(Pattern::Array(Box::new(ArrayPattern { elements, span: self.span_from(start) })))
    }

    fn parse_object_binding_pattern(&mut self) -> Result<Pattern> {
        let start = self.tok.span.start;
        self.expect(Punct::LBrace)?;
        let mut properties = Vec::new();
        let mut rest = None;
        while !self.at(Punct::RBrace) {
            if self.at(Punct::Ellipsis) {
                self.advance()?;
                rest = Some(Pattern::Ident(self.parse_binding_identifier()?));
                if !self.at(Punct::RBrace) {
                    return self.err("rest element must be last", self.tok.span);
                }
                break;
            }
            let pstart = self.tok.span.start;
            let key_tok = self.tok.clone();
            let key = self.parse_property_key(false)?;
            if self.eat(Punct::Colon)? {
                let value = self.parse_binding_element()?;
                properties.push(ObjectPatternProperty { key, value, shorthand: false, span: self.span_from(pstart) });
            } else {
                // Shorthand: the key must be a valid binding identifier.
                let (PropertyKey::Ident(id), TokenKind::Ident { escaped, .. }) = (&key, &key_tok.kind) else {
                    return self.unexpected();
                };
                self.check_identifier(&id.name, id.span, *escaped)?;
                self.check_binding_identifier(&id.name, id.span)?;
                let ident = Ident { name: id.name.clone(), span: id.span };
                let mut value = Pattern::Ident(ident);
                if self.at(Punct::Eq) {
                    self.advance()?;
                    let right = self.with_in(true, |p| p.parse_assignment_expression())?;
                    value = Pattern::Assignment(Box::new(AssignmentPattern { left: value, right, span: self.span_from(pstart) }));
                }
                properties.push(ObjectPatternProperty { key, value, shorthand: true, span: self.span_from(pstart) });
            }
            if !self.at(Punct::RBrace) {
                self.expect(Punct::Comma)?;
            }
        }
        self.expect(Punct::RBrace)?;
        Ok(Pattern::Object(Box::new(ObjectPattern { properties, rest, span: self.span_from(start) })))
    }

    /// Convert an expression parsed under the cover grammar into a pattern.
    /// `binding` is true for arrow parameters (identifiers only as leaves),
    /// false for destructuring assignment (any simple target as a leaf).
    pub(crate) fn expr_to_pattern(&mut self, e: Expression, binding: bool) -> Result<Pattern> {
        match e {
            Expression::Ident(id) => {
                self.check_binding_identifier(&id.name, id.span)?;
                if binding && !self.ctx.strict && Self::is_strict_reserved(&id.name) && &*id.name != "yield" && &*id.name != "let" {
                    // fine in sloppy code
                }
                Ok(Pattern::Ident(id))
            }
            Expression::Member(_) | Expression::Call(_) if !binding => {
                self.expr_to_simple_target(e, false)
            }
            Expression::Paren(inner, span) if !binding => {
                // `(a) = 1` and `(a.b) = 1` are valid; `([a]) = 1` is not.
                match inner.unparen() {
                    Expression::Ident(_) | Expression::Member(_) | Expression::Call(_) => {
                        self.expr_to_simple_target(Expression::Paren(inner, span), false)
                    }
                    _ => self.err("invalid destructuring assignment target", span),
                }
            }
            Expression::Array(a) => {
                let a = *a;
                if a.trailing_comma_after_spread {
                    return self.err("rest element may not have a trailing comma", a.span);
                }
                let n = a.elements.len();
                let mut elements = Vec::with_capacity(n);
                for (i, el) in a.elements.into_iter().enumerate() {
                    match el {
                        None => elements.push(None),
                        Some(Expression::Spread(s)) => {
                            if i + 1 != n {
                                return self.err("rest element must be last", s.span);
                            }
                            let argument = self.expr_to_pattern(s.argument, binding)?;
                            if matches!(argument, Pattern::Assignment(_)) {
                                return self.err("rest element may not have an initializer", s.span);
                            }
                            elements.push(Some(Pattern::Rest(Box::new(RestElement { argument, span: s.span }))));
                        }
                        Some(e) => elements.push(Some(self.expr_to_pattern(e, binding)?)),
                    }
                }
                Ok(Pattern::Array(Box::new(ArrayPattern { elements, span: a.span })))
            }
            Expression::Object(o) => {
                let o = *o;
                if o.trailing_comma_after_spread {
                    return self.err("rest element may not have a trailing comma", o.span);
                }
                let n = o.properties.len();
                let mut properties = Vec::new();
                let mut rest = None;
                for (i, prop) in o.properties.into_iter().enumerate() {
                    match prop {
                        ObjectProperty::Spread(s) => {
                            if i + 1 != n {
                                return self.err("rest element must be last", s.span);
                            }
                            let target = match s.argument.unparen() {
                                Expression::Ident(_) => self.expr_to_pattern(s.argument, binding)?,
                                Expression::Member(_) if !binding => self.expr_to_pattern(s.argument, binding)?,
                                _ => return self.err("invalid rest element", s.span),
                            };
                            rest = Some(target);
                        }
                        ObjectProperty::Property(p) => {
                            if p.kind != PropertyKind::Init {
                                return self.err("invalid destructuring assignment target", p.span);
                            }
                            let value = self.expr_to_pattern(p.value, binding)?;
                            properties.push(ObjectPatternProperty { key: p.key, value, shorthand: p.shorthand, span: p.span });
                        }
                    }
                }
                Ok(Pattern::Object(Box::new(ObjectPattern { properties, rest, span: o.span })))
            }
            Expression::Assignment(a) => {
                let a = *a;
                if a.op != AssignOp::Assign {
                    return self.err("invalid destructuring assignment target", a.span);
                }
                // The left side was already converted when `=` was parsed;
                // for binding patterns it must only contain identifiers.
                if let Pattern::Ident(id) = &a.left {
                    self.check_binding_identifier(&id.name, id.span)?;
                }
                let left = if binding { self.repattern_as_binding(a.left)? } else { a.left };
                Ok(Pattern::Assignment(Box::new(AssignmentPattern { left, right: a.right, span: a.span })))
            }
            other => self.err("invalid destructuring assignment target", other.span()),
        }
    }

    /// A pattern built for assignment may contain member expressions; as a
    /// binding pattern (arrow parameters) it may not.
    fn repattern_as_binding(&mut self, p: Pattern) -> Result<Pattern> {
        match p {
            Pattern::Ident(id) => {
                self.check_binding_identifier(&id.name, id.span)?;
                Ok(Pattern::Ident(id))
            }
            Pattern::Expression(e) => self.err("invalid destructuring assignment target", e.span()),
            Pattern::Object(o) => {
                let mut o = *o;
                for prop in o.properties.iter_mut() {
                    let v = std::mem::replace(&mut prop.value, Pattern::Ident(Ident { name: Rc::from(""), span: prop.span }));
                    prop.value = self.repattern_as_binding(v)?;
                }
                if let Some(r) = o.rest.take() {
                    o.rest = Some(self.repattern_as_binding(r)?);
                }
                Ok(Pattern::Object(Box::new(o)))
            }
            Pattern::Array(a) => {
                let mut a = *a;
                for el in a.elements.iter_mut() {
                    if let Some(v) = el.take() {
                        *el = Some(self.repattern_as_binding(v)?);
                    }
                }
                Ok(Pattern::Array(Box::new(a)))
            }
            Pattern::Assignment(a) => {
                let mut a = *a;
                a.left = self.repattern_as_binding(a.left)?;
                Ok(Pattern::Assignment(Box::new(a)))
            }
            Pattern::Rest(r) => {
                let mut r = *r;
                r.argument = self.repattern_as_binding(r.argument)?;
                Ok(Pattern::Rest(Box::new(r)))
            }
        }
    }

    /// A simple assignment target: identifier or member expression (call
    /// expressions too in sloppy code, Annex B, as a runtime error).
    /// `logical` is set for `&&=`, `||=`, `??=`, which reject calls.
    pub(crate) fn expr_to_simple_target(&mut self, e: Expression, logical: bool) -> Result<Pattern> {
        match e.unparen() {
            Expression::Ident(id) => {
                if self.ctx.strict && (&*id.name == "eval" || &*id.name == "arguments") {
                    return self.err(format!("cannot assign to '{}' in strict mode", id.name), id.span);
                }
                match e {
                    Expression::Ident(id) => Ok(Pattern::Ident(id)),
                    other => Ok(Pattern::Expression(Box::new(other))),
                }
            }
            Expression::Member(_) => Ok(Pattern::Expression(Box::new(e))),
            Expression::Call(c) if !self.ctx.strict && !logical && !c.optional => Ok(Pattern::Expression(Box::new(e))),
            _ => self.err("invalid assignment target", e.span()),
        }
    }

    /// Collect the identifiers bound by a pattern.
    pub(crate) fn bound_names<'a>(p: &'a Pattern, out: &mut Vec<&'a Ident>) {
        match p {
            Pattern::Ident(i) => out.push(i),
            Pattern::Object(o) => {
                for prop in &o.properties {
                    Self::bound_names(&prop.value, out);
                }
                if let Some(r) = &o.rest {
                    Self::bound_names(r, out);
                }
            }
            Pattern::Array(a) => {
                for el in a.elements.iter().flatten() {
                    Self::bound_names(el, out);
                }
            }
            Pattern::Assignment(a) => Self::bound_names(&a.left, out),
            Pattern::Rest(r) => Self::bound_names(&r.argument, out),
            Pattern::Expression(_) => {}
        }
    }
}
