//! The recursive-descent parser.
//!
//! The parser owns the lexer and a one-token lookahead, tracks the
//! syntactic context (strictness, `yield`/`await` as keywords, where
//! `return`, `super` and `new.target` are allowed) and the declaration
//! scopes needed for early errors. Cover grammars (parenthesised
//! expressions that become arrow parameters, literals that become
//! destructuring patterns) are parsed as expressions and converted.

mod class;
mod expr;
mod function;
mod module;
mod pattern;
mod scope;
mod stmt;

use crate::ast::*;
use crate::error::{Result, SyntaxError};
use crate::lexer::{Lexer, LexerState, Punct, Token, TokenKind};
use crate::span::Span;
use scope::{PrivateEnv, ScopeStack};
use foldhash::{HashSet, HashSetExt};
use std::rc::Rc;

/// The function-level context. Saved and replaced at every function
/// boundary, restored afterwards.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FnCtx {
    pub strict: bool,
    pub is_async: bool,
    pub is_generator: bool,
    /// `super.x` allowed (methods, class fields, static blocks).
    pub super_property: bool,
    /// `super()` allowed (derived class constructors).
    pub super_call: bool,
    /// `new.target` allowed (inside any non-arrow function or class body).
    pub new_target: bool,
    /// `arguments` forbidden (class field initialisers, static blocks).
    pub no_arguments: bool,
    pub allow_return: bool,
    /// Inside a class static block: `await` is reserved and not an operator.
    pub in_static_block: bool,
    /// At the top level of a module: top-level `await`.
    pub module_top: bool,
    /// Inside a (possibly nested) arrow or function body within a
    /// parameter list of a generator/async function — `yield`/`await`
    /// expressions are not allowed in formal parameters.
    pub in_params: bool,
}

impl FnCtx {
    fn script(strict: bool) -> FnCtx {
        FnCtx {
            strict,
            is_async: false,
            is_generator: false,
            super_property: false,
            super_call: false,
            new_target: false,
            no_arguments: false,
            allow_return: false,
            in_static_block: false,
            module_top: false,
            in_params: false,
        }
    }
}

/// A label in scope: its name and whether it labels an iteration.
struct Label {
    name: Rc<str>,
    is_loop: bool,
}

pub struct Parser<'s> {
    lexer: Lexer<'s>,
    tok: Token<'s>,
    /// End offset of the previous token (for spans).
    prev_end: u32,
    pub(crate) ctx: FnCtx,
    pub(crate) scopes: ScopeStack,
    /// `in` allowed as a relational operator (false in `for (` heads).
    allow_in: bool,
    labels: Vec<Label>,
    loop_depth: u32,
    switch_depth: u32,
    /// Pending cover-grammar error: `{a = 1}` outside a pattern or a
    /// duplicate `__proto__`. Reported once the expression is known not
    /// to be a destructuring target.
    pending_cover: Option<(Span, &'static str)>,
    /// First `yield`/`await` expression seen while parsing a possible
    /// arrow-parameter list.
    pending_yield_await: Option<Span>,
    /// The expression just parsed contained a `yield`/`await` or was
    /// otherwise invalid as parameters: tracked per paren level.
    private_envs: Vec<PrivateEnv>,
    /// Every identifier name seen so far, shared by all its `Ident`s.
    names: HashSet<Rc<str>>,
    /// Set when a direct `eval(...)` call appears in the current function.
    has_direct_eval: bool,
    /// Start offset of the last `await` identifier seen, so that
    /// `async (x = await) =>` can be rejected after the fact.
    last_await: Option<u32>,
    /// The outermost private environment was seeded by `parse_eval`:
    /// unresolved names in it are errors (they cannot bubble further).
    eval_private_env: bool,
}

pub(crate) struct Saved {
    ctx: FnCtx,
    labels: Vec<Label>,
    loop_depth: u32,
    switch_depth: u32,
    has_direct_eval: bool,
}

impl<'s> Parser<'s> {
    pub fn new(source: &'s str, module: bool) -> Result<Parser<'s>> {
        let mut lexer = Lexer::new(source, module);
        let tok = lexer.next_token()?;
        Ok(Parser {
            lexer,
            tok,
            prev_end: 0,
            ctx: FnCtx::script(module),
            scopes: ScopeStack::new(),
            allow_in: true,
            labels: Vec::new(),
            loop_depth: 0,
            switch_depth: 0,
            pending_cover: None,
            pending_yield_await: None,
            private_envs: Vec::new(),
            names: HashSet::new(),
            has_direct_eval: false,
            last_await: None,
            eval_private_env: false,
        })
    }

    pub fn source(&self) -> &'s str {
        self.lexer.source()
    }

    // ---- token helpers ------------------------------------------------

    pub(crate) fn intern(&mut self, name: &str) -> Rc<str> {
        if let Some(n) = self.names.get(name) {
            return n.clone();
        }
        let rc: Rc<str> = Rc::from(name);
        self.names.insert(rc.clone());
        rc
    }

    pub(crate) fn err<T>(&self, message: impl Into<String>, span: Span) -> Result<T> {
        Err(SyntaxError::new(message, span))
    }

    pub(crate) fn unexpected<T>(&self) -> Result<T> {
        let msg = match &self.tok.kind {
            TokenKind::Eof => "unexpected end of input".to_string(),
            TokenKind::Ident { name, .. } => format!("unexpected identifier '{name}'"),
            TokenKind::String { .. } => "unexpected string".to_string(),
            TokenKind::Number { .. } | TokenKind::BigInt(_) => "unexpected number".to_string(),
            TokenKind::Template { .. } => "unexpected template".to_string(),
            TokenKind::RegExp { .. } => "unexpected regular expression".to_string(),
            TokenKind::PrivateName(n) => format!("unexpected private name #{n}"),
            TokenKind::Punct(p) => format!("unexpected token {p:?}"),
        };
        self.err(msg, self.tok.span)
    }

    pub(crate) fn advance(&mut self) -> Result<Token<'s>> {
        let next = self.lexer.next_token()?;
        self.prev_end = self.tok.span.end;
        Ok(std::mem::replace(&mut self.tok, next))
    }

    pub(crate) fn at(&self, p: Punct) -> bool {
        self.tok.kind == TokenKind::Punct(p)
    }

    pub(crate) fn eat(&mut self, p: Punct) -> Result<bool> {
        if self.at(p) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn expect(&mut self, p: Punct) -> Result<Span> {
        if self.at(p) {
            Ok(self.advance()?.span)
        } else {
            self.unexpected()
        }
    }

    /// Current token is the identifier/keyword `name`, unescaped.
    pub(crate) fn at_kw(&self, name: &str) -> bool {
        matches!(&self.tok.kind, TokenKind::Ident { name: n, escaped: false } if &**n == name)
    }

    /// Current token is `name` whether escaped or not.
    pub(crate) fn at_name(&self, name: &str) -> bool {
        matches!(&self.tok.kind, TokenKind::Ident { name: n, .. } if &**n == name)
    }

    pub(crate) fn eat_kw(&mut self, name: &str) -> Result<bool> {
        if self.at_kw(name) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn expect_kw(&mut self, name: &str) -> Result<Span> {
        if self.at_kw(name) {
            Ok(self.advance()?.span)
        } else if self.at_name(name) {
            self.err(
                format!("keyword '{name}' must not contain escape sequences"),
                self.tok.span,
            )
        } else {
            self.unexpected()
        }
    }

    pub(crate) fn span_from(&self, start: u32) -> Span {
        Span::new(start, self.prev_end)
    }

    /// Look at the token after the current one without consuming anything.
    pub(crate) fn peek(&mut self) -> Result<Token<'s>> {
        let state = self.lexer.state();
        let t = self.lexer.next_token();
        self.lexer.restore(state);
        t
    }

    pub(crate) fn snapshot(&self) -> (LexerState, Token<'s>, u32) {
        (self.lexer.state(), self.tok.clone(), self.prev_end)
    }

    pub(crate) fn rewind(&mut self, s: (LexerState, Token<'s>, u32)) {
        self.lexer.restore(s.0);
        self.tok = s.1;
        self.prev_end = s.2;
    }

    /// Automatic semicolon insertion.
    pub(crate) fn consume_semicolon(&mut self) -> Result<()> {
        if self.eat(Punct::Semicolon)? {
            return Ok(());
        }
        if self.at(Punct::RBrace) || self.tok.kind == TokenKind::Eof || self.tok.newline_before {
            return Ok(());
        }
        self.unexpected()
    }

    // ---- identifiers and reserved words --------------------------------

    /// Words that are always reserved (in this context).
    pub(crate) fn is_reserved_word(&self, name: &str) -> bool {
        matches!(
            name,
            "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "import"
                | "in"
                | "instanceof"
                | "new"
                | "null"
                | "return"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        ) && !(name == "await" && self.await_is_identifier())
            && !(name == "yield" && self.yield_is_identifier())
    }

    pub(crate) fn is_strict_reserved(name: &str) -> bool {
        matches!(
            name,
            "implements"
                | "interface"
                | "let"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "static"
                | "yield"
        )
    }

    pub(crate) fn yield_is_identifier(&self) -> bool {
        !self.ctx.is_generator && !self.ctx.strict
    }

    pub(crate) fn await_is_identifier(&self) -> bool {
        !self.ctx.is_async && !self.lexer.module && !self.ctx.in_static_block
    }

    /// Validate a name used as an IdentifierReference, BindingIdentifier or
    /// LabelIdentifier in the current context.
    pub(crate) fn check_identifier(&mut self, name: &str, span: Span, escaped: bool) -> Result<()> {
        if name == "await" {
            self.last_await = Some(span.start);
        }
        if self.is_reserved_word(name) {
            if escaped {
                return self.err(
                    format!("keyword '{name}' must not contain escape sequences"),
                    span,
                );
            }
            return self.err(format!("unexpected reserved word '{name}'"), span);
        }
        if name == "yield" {
            if self.ctx.in_params && self.ctx.is_generator {
                return self.err("'yield' is not allowed in generator parameters", span);
            }
        }
        if name == "await" {
            if self.ctx.in_params && self.ctx.is_async {
                return self.err("'await' is not allowed in async function parameters", span);
            }
        }
        if self.ctx.strict && Self::is_strict_reserved(name) {
            return self.err(
                format!("unexpected strict mode reserved word '{name}'"),
                span,
            );
        }
        Ok(())
    }

    /// Additional checks for a name being bound (not just referenced).
    pub(crate) fn check_binding_identifier(&self, name: &str, span: Span) -> Result<()> {
        if self.ctx.strict && (name == "eval" || name == "arguments") {
            return self.err(format!("'{name}' cannot be bound in strict mode"), span);
        }
        Ok(())
    }

    /// Parse a BindingIdentifier.
    pub(crate) fn parse_binding_identifier(&mut self) -> Result<Ident> {
        let TokenKind::Ident { name, escaped } = &self.tok.kind else {
            return self.unexpected();
        };
        let (name, escaped) = (name.clone(), *escaped);
        let span = self.tok.span;
        self.check_identifier(&name, span, escaped)?;
        self.check_binding_identifier(&name, span)?;
        self.advance()?;
        let name = self.intern(&name);
        Ok(Ident { name, span })
    }

    /// Parse an IdentifierName (any name, reserved words included).
    pub(crate) fn parse_identifier_name(&mut self) -> Result<Ident> {
        let TokenKind::Ident { name, .. } = &self.tok.kind else {
            return self.unexpected();
        };
        let name = name.clone();
        let span = self.tok.span;
        self.advance()?;
        let name = self.intern(&name);
        Ok(Ident { name, span })
    }

    pub(crate) fn save_fn_ctx(&mut self, new_ctx: FnCtx) -> Saved {
        let saved = Saved {
            ctx: self.ctx,
            labels: std::mem::take(&mut self.labels),
            loop_depth: self.loop_depth,
            switch_depth: self.switch_depth,
            has_direct_eval: self.has_direct_eval,
        };
        self.ctx = new_ctx;
        self.loop_depth = 0;
        self.switch_depth = 0;
        self.has_direct_eval = false;
        saved
    }

    pub(crate) fn restore_fn_ctx(&mut self, saved: Saved) {
        self.ctx = saved.ctx;
        self.labels = saved.labels;
        self.loop_depth = saved.loop_depth;
        self.switch_depth = saved.switch_depth;
        self.has_direct_eval = saved.has_direct_eval;
    }

    // ---- entry points --------------------------------------------------

    pub fn parse_program(mut self) -> Result<Program> {
        let module = self.lexer.module;
        let start = 0;
        self.scopes.push_function_top(module);
        if module {
            self.ctx.module_top = true;
            self.ctx.strict = true;
            self.ctx.is_async = true; // top-level await
        }
        let body = self.parse_body_with_prologue(true, module)?;
        if self.tok.kind != TokenKind::Eof {
            return self.unexpected();
        }
        if module {
            self.check_module_exports(&body)?;
        }
        if self.eval_private_env {
            let env = self.private_envs.pop().unwrap();
            for (name, span) in env.referenced.iter() {
                if !env.is_declared(name) {
                    return self.err(format!("private name #{name} is not defined"), *span);
                }
            }
        }
        self.scopes.pop();
        Ok(Program {
            source_type: if module {
                SourceType::Module
            } else {
                SourceType::Script
            },
            body,
            strict: self.ctx.strict,
            span: Span::new(start, self.lexer.source().len() as u32),
        })
    }

    /// Parse statements with a directive prologue; sets strict mode when
    /// `"use strict"` appears. Stops at `}` or EOF.
    pub(crate) fn parse_body_with_prologue(
        &mut self,
        top_level: bool,
        module: bool,
    ) -> Result<Vec<Statement>> {
        let mut body = Vec::new();
        let mut in_prologue = true;
        // Legacy octal escapes in directives before "use strict" become
        // errors once strictness is known.
        let mut octal_directive: Option<Span> = None;
        loop {
            if self.at(Punct::RBrace) || self.tok.kind == TokenKind::Eof {
                break;
            }
            if in_prologue {
                if let TokenKind::String {
                    value,
                    legacy_octal,
                } = &self.tok.kind
                {
                    let is_use_strict =
                        &**value == "use strict".encode_utf16().collect::<Vec<_>>().as_slice();
                    let legacy_octal = *legacy_octal;
                    let span = self.tok.span;
                    // A directive is a string literal statement on its own.
                    let next = self.peek()?;
                    let ends = matches!(
                        next.kind,
                        TokenKind::Punct(Punct::Semicolon) | TokenKind::Punct(Punct::RBrace)
                    ) || next.kind == TokenKind::Eof
                        || next.newline_before;
                    if ends {
                        if legacy_octal && octal_directive.is_none() {
                            octal_directive = Some(span);
                        }
                        let raw =
                            &self.lexer.source()[span.start as usize + 1..span.end as usize - 1];
                        let raw_is_use_strict = raw == "use strict";
                        if is_use_strict && raw_is_use_strict {
                            if !self.ctx.strict {
                                self.ctx.strict = true;
                            }
                            if let Some(o) = octal_directive {
                                return self.err(
                                    "octal escape sequences are not allowed in strict mode",
                                    o,
                                );
                            }
                        }
                        let stmt = self.parse_statement_list_item(top_level, module)?;
                        let stmt = match stmt {
                            Statement::Expression(mut e) => {
                                e.directive = Some(raw.into());
                                Statement::Expression(e)
                            }
                            s => s,
                        };
                        body.push(stmt);
                        continue;
                    }
                }
                in_prologue = false;
            }
            body.push(self.parse_statement_list_item(top_level, module)?);
        }
        if self.ctx.strict {
            if let Some(o) = octal_directive {
                return self.err("octal escape sequences are not allowed in strict mode", o);
            }
        }
        Ok(body)
    }
}

/// Parse a script.
pub fn parse_script(source: &str) -> Result<Program> {
    Parser::new(source, false)?.parse_program()
}

/// Parse a module.
pub fn parse_module(source: &str) -> Result<Program> {
    Parser::new(source, true)?.parse_program()
}

/// The syntactic context of a direct `eval` call (PerformEval's
/// in-function, in-method, in-derived-constructor and in-class-field
/// initializer flags, plus strictness and the visible private names).
#[derive(Clone, Debug, Default)]
pub struct EvalOptions {
    pub strict: bool,
    /// Inside a non-arrow function: `new.target` allowed.
    pub in_function: bool,
    /// Inside a method: `super.x` allowed.
    pub in_method: bool,
    /// Inside a derived constructor: `super()` allowed.
    pub in_derived_constructor: bool,
    /// Inside a class field initializer or static block: `arguments`
    /// forbidden.
    pub in_class_field_initializer: bool,
    /// Private names visible at the call site.
    pub private_names: Vec<Rc<str>>,
}

/// Parse eval code. The result is a script whose `strict` flag reflects
/// both the options and a "use strict" directive.
pub fn parse_eval(source: &str, options: &EvalOptions) -> Result<Program> {
    let mut p = Parser::new(source, false)?;
    p.ctx.strict = options.strict;
    p.ctx.new_target = options.in_function;
    p.ctx.super_property = options.in_method;
    p.ctx.super_call = options.in_derived_constructor;
    p.ctx.no_arguments = options.in_class_field_initializer;
    if !options.private_names.is_empty() {
        let mut env = PrivateEnv::default();
        for name in &options.private_names {
            // Visible names are declared with a private kind that cannot
            // clash; eval code cannot declare into this environment.
            env.declare(
                name.clone(),
                Span::empty(0),
                scope::PrivateKind::Other,
                false,
            )?;
        }
        p.private_envs.push(env);
        p.eval_private_env = true;
    }
    p.parse_program()
}
