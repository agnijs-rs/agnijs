//! Statements and declarations.

use super::scope::LexKind;
use super::Parser;
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};
use crate::span::Span;

/// IsLabelledFunction: a (possibly nested) labelled statement whose body
/// is a function declaration.
fn labelled_function(s: &Statement) -> Option<Span> {
    match s {
        Statement::Labeled(l) => match &l.body {
            Statement::FunctionDeclaration(f) => Some(f.span),
            other => labelled_function(other),
        },
        _ => None,
    }
}

impl<'s> Parser<'s> {
    /// StatementListItem: a statement or a declaration.
    pub(crate) fn parse_statement_list_item(
        &mut self,
        top_level: bool,
        module: bool,
    ) -> Result<Statement> {
        // Declarations (functions, classes) nest without passing through
        // `parse_statement`, so this entry checks the budget too.
        self.check_stack()?;
        let name = match &self.tok.kind {
            TokenKind::Ident {
                name,
                escaped: false,
            } => Some(name.clone()),
            _ => None,
        };
        match name {
            Some(name) => match &*name {
                "function" => {
                    let f = self.parse_function_declaration(false, false)?;
                    Ok(Statement::FunctionDeclaration(Box::new(f)))
                }
                "async" if self.at_async_function()? => {
                    let f = self.parse_function_declaration(true, false)?;
                    Ok(Statement::FunctionDeclaration(Box::new(f)))
                }
                "class" => Ok(Statement::ClassDeclaration(Box::new(
                    self.parse_class_declaration(false)?,
                ))),
                "const" => {
                    let d = self.parse_variable_declaration(VariableKind::Const, false)?;
                    self.consume_semicolon()?;
                    Ok(Statement::VariableDeclaration(d))
                }
                "let" if self.let_starts_declaration()? => {
                    let d = self.parse_variable_declaration(VariableKind::Let, false)?;
                    self.consume_semicolon()?;
                    Ok(Statement::VariableDeclaration(d))
                }
                "using" | "await" if self.at_using_declaration(false)?.is_some() => {
                    let kind = if &*name == "await" {
                        VariableKind::AwaitUsing
                    } else {
                        VariableKind::Using
                    };
                    if top_level && !module {
                        return self.err(
                            "using declarations are not allowed at the top level of a script",
                            self.tok.span,
                        );
                    }
                    let d = self.parse_variable_declaration(kind, false)?;
                    self.consume_semicolon()?;
                    Ok(Statement::VariableDeclaration(d))
                }
                "import" if module && top_level => {
                    let next = self.peek()?;
                    if matches!(
                        next.kind,
                        TokenKind::Punct(Punct::LParen) | TokenKind::Punct(Punct::Dot)
                    ) {
                        self.parse_statement()
                    } else {
                        self.parse_import_declaration()
                    }
                }
                "export" if module && top_level => self.parse_export_declaration(),
                "import" | "export" if !module || !top_level => {
                    let next = self.peek()?;
                    if &*name == "import"
                        && matches!(
                            next.kind,
                            TokenKind::Punct(Punct::LParen) | TokenKind::Punct(Punct::Dot)
                        )
                    {
                        self.parse_statement()
                    } else {
                        self.err(format!("'{name}' declarations may only appear at the top level of a module"), self.tok.span)
                    }
                }
                _ => self.parse_statement(),
            },
            _ => self.parse_statement(),
        }
    }

    /// `let` begins a lexical declaration when followed by an identifier,
    /// `[`, `{` (or `yield`/`await` that would be identifiers).
    fn let_starts_declaration(&mut self) -> Result<bool> {
        let next = self.peek()?;
        Ok(match &next.kind {
            TokenKind::Punct(Punct::LBracket) | TokenKind::Punct(Punct::LBrace) => true,
            TokenKind::Ident { name, .. } => {
                // `let` newline `x` is still a declaration; `let in x` is not.
                !(matches!(&**name, "in" | "instanceof" | "of") && !next.newline_before)
                    || (self.ctx.strict && &**name == "let")
            }
            _ => false,
        })
    }

    /// Whether a `using` or `await using` declaration starts here:
    /// `using` [no LineTerminator] BindingIdentifier, or `await`
    /// [no LineTerminator] `using` [no LineTerminator] BindingIdentifier
    /// where `await` is a keyword. Some(true) for `await using`. In a
    /// `for` head, `using of` is the expression `using` followed by `of`.
    pub(crate) fn at_using_declaration(&mut self, in_for: bool) -> Result<Option<bool>> {
        let binding_ok = |p: &Self, t: &crate::lexer::Token<'_>, allow_of: bool| match &t.kind {
            TokenKind::Ident { name, .. } => {
                !t.newline_before
                    && !p.is_reserved_word(name)
                    && (allow_of || &**name != "of")
            }
            _ => false,
        };
        if self.at_kw("using") {
            let next = self.peek()?;
            if in_for && binding_ok(self, &next, true) && !binding_ok(self, &next, false) {
                // `for (using of = x;;)` declares `of`; `for (using of of
                // xs)` iterates into `using`.
                let snap = self.snapshot();
                self.advance()?;
                let after = self.peek();
                self.rewind(snap);
                let after = after?;
                let decl = matches!(after.kind, TokenKind::Punct(Punct::Eq));
                return Ok(decl.then_some(false));
            }
            return Ok(binding_ok(self, &next, !in_for).then_some(false));
        }
        if !self.at_kw("await") || !self.await_is_keyword() {
            return Ok(None);
        }
        let next = self.peek()?;
        let is_using = matches!(
            &next.kind,
            TokenKind::Ident { name, escaped: false } if &**name == "using"
        ) && !next.newline_before;
        if !is_using {
            return Ok(None);
        }
        let snap = self.snapshot();
        self.advance()?;
        let after = self.peek();
        self.rewind(snap);
        let after = after?;
        Ok(binding_ok(self, &after, true).then_some(true))
    }

    /// Statement (not a declaration).
    pub(crate) fn parse_statement(&mut self) -> Result<Statement> {
        self.check_stack()?;
        let start = self.tok.span.start;
        match &self.tok.kind {
            TokenKind::Punct(Punct::LBrace) => Ok(Statement::Block(self.parse_block()?)),
            TokenKind::Punct(Punct::Semicolon) => {
                let span = self.tok.span;
                self.advance()?;
                Ok(Statement::Empty(span))
            }
            TokenKind::Ident {
                name,
                escaped: false,
            } => {
                let name = name.clone();
                match &*name {
                    "var" => {
                        let d = self.parse_variable_declaration(VariableKind::Var, false)?;
                        self.consume_semicolon()?;
                        Ok(Statement::VariableDeclaration(d))
                    }
                    "if" => self.parse_if(),
                    "for" => self.parse_for(),
                    "while" => self.parse_while(),
                    "do" => self.parse_do_while(),
                    "continue" => self.parse_jump(true),
                    "break" => self.parse_jump(false),
                    "return" => self.parse_return(),
                    "with" => self.parse_with(),
                    "switch" => self.parse_switch(),
                    "throw" => self.parse_throw(),
                    "try" => self.parse_try(),
                    "debugger" => {
                        self.advance()?;
                        self.consume_semicolon()?;
                        Ok(Statement::Debugger(self.span_from(start)))
                    }
                    "function" => self.err(
                        "function declarations are not allowed in this position",
                        self.tok.span,
                    ),
                    "class" => self.err(
                        "class declarations are not allowed in this position",
                        self.tok.span,
                    ),
                    "async" if self.at_async_function()? => self.err(
                        "async function declarations are not allowed in this position",
                        self.tok.span,
                    ),
                    "let" => {
                        let next = self.peek()?;
                        if matches!(next.kind, TokenKind::Punct(Punct::LBracket)) {
                            return self.err(
                                "lexical declaration cannot appear in a single-statement context",
                                self.tok.span,
                            );
                        }
                        if self.ctx.strict {
                            return self
                                .err("unexpected strict mode reserved word 'let'", self.tok.span);
                        }
                        self.parse_expression_or_labelled()
                    }
                    _ => self.parse_expression_or_labelled(),
                }
            }
            _ => self.parse_expression_or_labelled(),
        }
    }

    fn parse_expression_or_labelled(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        // Labelled statement: Identifier `:`
        if let TokenKind::Ident { name, escaped } = self.tok.kind.clone() {
            let next = self.peek()?;
            if next.kind == TokenKind::Punct(Punct::Colon) {
                let span = self.tok.span;
                self.check_identifier(&name, span, escaped)?;
                self.advance()?;
                self.advance()?; // :
                let name = self.intern(&name);
                if self.labels.iter().any(|l| l.name == name) {
                    return self.err(format!("label '{name}' has already been declared"), span);
                }
                let is_loop = self.at_kw("for") || self.at_kw("while") || self.at_kw("do");
                self.labels.push(super::Label {
                    name: name.clone(),
                    is_loop,
                });
                let body = if self.at_kw("function") {
                    // Annex B: labelled function declarations in sloppy code.
                    if self.ctx.strict {
                        self.labels.pop();
                        return self.err(
                            "labelled function declarations are not allowed in strict mode",
                            self.tok.span,
                        );
                    }
                    let f = self.parse_function_declaration(false, false);
                    match f {
                        Ok(f) => {
                            if f.kind != FunctionKind::Normal {
                                self.labels.pop();
                                return self.err("labelled generator or async function declarations are not allowed", f.span);
                            }
                            Ok(Statement::FunctionDeclaration(Box::new(f)))
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    self.parse_statement()
                };
                self.labels.pop();
                let body = body?;
                return Ok(Statement::Labeled(Box::new(LabeledStatement {
                    label: Ident { name, span },
                    body,
                    span: self.span_from(start),
                })));
            }
        }
        let expression = self.with_in(true, |p| p.parse_expression())?;
        self.consume_semicolon()?;
        Ok(Statement::Expression(ExpressionStatement {
            expression,
            directive: None,
            span: self.span_from(start),
        }))
    }

    pub(crate) fn parse_block(&mut self) -> Result<Block> {
        let start = self.tok.span.start;
        self.expect(Punct::LBrace)?;
        self.scopes.push_block();
        let body = self.parse_block_body();
        self.scopes.pop();
        let body = body?;
        self.expect(Punct::RBrace)?;
        Ok(Block {
            body,
            span: self.span_from(start),
        })
    }

    fn parse_block_body(&mut self) -> Result<Vec<Statement>> {
        let mut body = Vec::new();
        while !self.at(Punct::RBrace) {
            if self.tok.kind == TokenKind::Eof {
                return self.unexpected();
            }
            body.push(self.parse_statement_list_item(false, false)?);
        }
        Ok(body)
    }

    /// The body of `if`, loops and `with`: a Statement, with the Annex B
    /// allowance of a plain function declaration directly under `if`.
    fn parse_substatement(&mut self, allow_function: bool) -> Result<Statement> {
        if allow_function && !self.ctx.strict && self.at_kw("function") {
            // Treated as if wrapped in a block.
            let start = self.tok.span.start;
            self.scopes.push_block();
            let f = self.parse_function_declaration(false, false);
            self.scopes.pop();
            let f = f?;
            if f.kind != FunctionKind::Normal {
                return self.err(
                    "generator or async function declarations are not allowed here",
                    f.span,
                );
            }
            let span = self.span_from(start);
            return Ok(Statement::Block(Block {
                body: vec![Statement::FunctionDeclaration(Box::new(f))],
                span,
            }));
        }
        let stmt = self.parse_statement()?;
        if let Some(span) = labelled_function(&stmt) {
            return self.err(
                "labelled function declarations are not allowed in this position",
                span,
            );
        }
        Ok(stmt)
    }

    fn parse_if(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        self.expect(Punct::LParen)?;
        let test = self.with_in(true, |p| p.parse_expression())?;
        self.expect(Punct::RParen)?;
        let consequent = self.parse_substatement(true)?;
        let alternate = if self.eat_kw("else")? {
            Some(self.parse_substatement(true)?)
        } else {
            None
        };
        Ok(Statement::If(Box::new(IfStatement {
            test,
            consequent,
            alternate,
            span: self.span_from(start),
        })))
    }

    fn parse_loop_body(&mut self) -> Result<Statement> {
        self.loop_depth += 1;
        let r = self.parse_substatement(false);
        self.loop_depth -= 1;
        r
    }

    fn parse_while(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        self.expect(Punct::LParen)?;
        let test = self.with_in(true, |p| p.parse_expression())?;
        self.expect(Punct::RParen)?;
        let body = self.parse_loop_body()?;
        Ok(Statement::While(Box::new(WhileStatement {
            test,
            body,
            span: self.span_from(start),
        })))
    }

    fn parse_do_while(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        let body = self.parse_loop_body()?;
        self.expect_kw("while")?;
        self.expect(Punct::LParen)?;
        let test = self.with_in(true, |p| p.parse_expression())?;
        self.expect(Punct::RParen)?;
        // A semicolon after do-while is optional even without a newline.
        self.eat(Punct::Semicolon)?;
        Ok(Statement::DoWhile(Box::new(DoWhileStatement {
            body,
            test,
            span: self.span_from(start),
        })))
    }

    fn parse_jump(&mut self, is_continue: bool) -> Result<Statement> {
        let start = self.tok.span.start;
        let kw_span = self.tok.span;
        self.advance()?;
        let mut label = None;
        if let TokenKind::Ident { name, escaped } = &self.tok.kind {
            if !self.tok.newline_before && !self.at(Punct::Semicolon) {
                let name = name.clone();
                let escaped = *escaped;
                let span = self.tok.span;
                self.check_identifier(&name, span, escaped)?;
                let found = self.labels.iter().find(|l| &*l.name == &*name);
                match found {
                    None => return self.err(format!("undefined label '{name}'"), span),
                    Some(l) => {
                        if is_continue && !l.is_loop {
                            return self.err(
                                format!("label '{name}' does not denote an iteration statement"),
                                span,
                            );
                        }
                    }
                }
                self.advance()?;
                label = Some(Ident {
                    name: self.intern(&name),
                    span,
                });
            }
        }
        if label.is_none() {
            if is_continue && self.loop_depth == 0 {
                return self.err("continue must be inside a loop", kw_span);
            }
            if !is_continue && self.loop_depth == 0 && self.switch_depth == 0 {
                return self.err("break must be inside a loop or switch", kw_span);
            }
        }
        self.consume_semicolon()?;
        let stmt = JumpStatement {
            label,
            span: self.span_from(start),
        };
        Ok(if is_continue {
            Statement::Continue(stmt)
        } else {
            Statement::Break(stmt)
        })
    }

    fn parse_return(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        if !self.ctx.allow_return {
            return self.err("return is not allowed outside a function", self.tok.span);
        }
        self.advance()?;
        let mut argument = None;
        if !self.at(Punct::Semicolon)
            && !self.at(Punct::RBrace)
            && self.tok.kind != TokenKind::Eof
            && !self.tok.newline_before
        {
            argument = Some(self.with_in(true, |p| p.parse_expression())?);
        }
        self.consume_semicolon()?;
        Ok(Statement::Return(ReturnStatement {
            argument,
            span: self.span_from(start),
        }))
    }

    fn parse_with(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        if self.ctx.strict {
            return self.err(
                "with statements are not allowed in strict mode",
                self.tok.span,
            );
        }
        self.advance()?;
        self.expect(Punct::LParen)?;
        let object = self.with_in(true, |p| p.parse_expression())?;
        self.expect(Punct::RParen)?;
        let body = self.parse_substatement(false)?;
        Ok(Statement::With(Box::new(WithStatement {
            object,
            body,
            span: self.span_from(start),
        })))
    }

    fn parse_switch(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        self.expect(Punct::LParen)?;
        let discriminant = self.with_in(true, |p| p.parse_expression())?;
        self.expect(Punct::RParen)?;
        self.expect(Punct::LBrace)?;
        self.scopes.push_block();
        self.switch_depth += 1;
        let cases = (|| {
            let mut cases = Vec::new();
            let mut seen_default = false;
            while !self.at(Punct::RBrace) {
                let cstart = self.tok.span.start;
                let test = if self.eat_kw("case")? {
                    Some(self.with_in(true, |p| p.parse_expression())?)
                } else if self.at_kw("default") {
                    if seen_default {
                        return self.err(
                            "more than one default clause in switch statement",
                            self.tok.span,
                        );
                    }
                    seen_default = true;
                    self.advance()?;
                    None
                } else {
                    return self.unexpected();
                };
                self.expect(Punct::Colon)?;
                let mut consequent = Vec::new();
                while !self.at_kw("case") && !self.at_kw("default") && !self.at(Punct::RBrace) {
                    if self.tok.kind == TokenKind::Eof {
                        return self.unexpected();
                    }
                    if self.at_using_declaration(false)?.is_some() {
                        return self.err(
                            "using declarations are not allowed directly in a switch case",
                            self.tok.span,
                        );
                    }
                    consequent.push(self.parse_statement_list_item(false, false)?);
                }
                cases.push(SwitchCase {
                    test,
                    consequent,
                    span: self.span_from(cstart),
                });
            }
            Ok(cases)
        })();
        self.switch_depth -= 1;
        self.scopes.pop();
        let cases = cases?;
        self.expect(Punct::RBrace)?;
        Ok(Statement::Switch(Box::new(SwitchStatement {
            discriminant,
            cases,
            span: self.span_from(start),
        })))
    }

    fn parse_throw(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        if self.tok.newline_before {
            return self.err("illegal newline after throw", self.tok.span);
        }
        let argument = self.with_in(true, |p| p.parse_expression())?;
        self.consume_semicolon()?;
        Ok(Statement::Throw(ThrowStatement {
            argument,
            span: self.span_from(start),
        }))
    }

    fn parse_try(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?;
        let block = self.parse_block()?;
        let mut handler = None;
        let mut finalizer = None;
        if self.at_kw("catch") {
            let cstart = self.tok.span.start;
            self.advance()?;
            self.scopes.push_catch();
            let r = (|| {
                let mut param = None;
                if self.eat(Punct::LParen)? {
                    let p = self.parse_binding_target()?;
                    let simple = matches!(p, Pattern::Ident(_));
                    let mut names = Vec::new();
                    Self::bound_names(&p, &mut names);
                    for n in names {
                        self.scopes
                            .declare_catch_param(n.name.clone(), n.span, simple)?;
                    }
                    self.expect(Punct::RParen)?;
                    param = Some(p);
                }
                let bstart = self.tok.span.start;
                self.expect(Punct::LBrace)?;
                // The catch block shares the parameter's scope so that
                // `catch (e) { let e; }` is an error.
                let body = self.parse_block_body()?;
                self.expect(Punct::RBrace)?;
                Ok((
                    param,
                    Block {
                        body,
                        span: self.span_from(bstart),
                    },
                ))
            })();
            self.scopes.pop();
            let (param, body) = r?;
            handler = Some(CatchClause {
                param,
                body,
                span: self.span_from(cstart),
            });
        }
        if self.eat_kw("finally")? {
            finalizer = Some(self.parse_block()?);
        }
        if handler.is_none() && finalizer.is_none() {
            return self.err("missing catch or finally after try", self.tok.span);
        }
        Ok(Statement::Try(Box::new(TryStatement {
            block,
            handler,
            finalizer,
            span: self.span_from(start),
        })))
    }

    /// `var`/`let`/`const` declarations. `in_for` leaves out the
    /// initializer requirement and declaration checks that the `for` head
    /// handles itself.
    pub(crate) fn parse_variable_declaration(
        &mut self,
        kind: VariableKind,
        in_for: bool,
    ) -> Result<VariableDeclaration> {
        let start = self.tok.span.start;
        self.advance()?; // var / let / const / using / await
        if kind == VariableKind::AwaitUsing {
            self.advance()?; // using
        }
        let mut declarations = Vec::new();
        loop {
            let dstart = self.tok.span.start;
            if kind.is_using() {
                match &self.tok.kind {
                    TokenKind::Ident { name, .. } if &**name == "let" => {
                        return self.err("'let' is not a valid using binding name", self.tok.span);
                    }
                    TokenKind::Ident { .. } => {}
                    _ => {
                        return self.err(
                            "using declarations may not have binding patterns",
                            self.tok.span,
                        )
                    }
                }
            }
            let id = self.parse_binding_target()?;
            let mut names = Vec::new();
            Self::bound_names(&id, &mut names);
            for n in names {
                match kind {
                    VariableKind::Var => self.scopes.declare_var(n.name.clone(), n.span)?,
                    _ => self.scopes.declare_lexical(
                        n.name.clone(),
                        n.span,
                        LexKind::Lexical,
                        self.ctx.strict,
                    )?,
                }
            }
            let mut init = None;
            if self.eat(Punct::Eq)? {
                init = Some(self.parse_assignment_expression()?);
            } else if !in_for {
                if kind.is_const() {
                    let what = if kind == VariableKind::Const { "const" } else { "using" };
                    return self.err(
                        format!("missing initializer in {what} declaration"),
                        self.span_from(dstart),
                    );
                }
                if !matches!(id, Pattern::Ident(_)) {
                    return self.err(
                        "missing initializer in destructuring declaration",
                        self.span_from(dstart),
                    );
                }
            }
            declarations.push(VariableDeclarator {
                id,
                init,
                span: self.span_from(dstart),
            });
            if !self.eat(Punct::Comma)? {
                break;
            }
        }
        Ok(VariableDeclaration {
            kind,
            declarations,
            span: self.span_from(start),
        })
    }

    fn parse_for(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.advance()?; // for
        let mut is_await = false;
        if self.at_kw("await") {
            if !self.await_is_keyword() {
                return self.unexpected();
            }
            if self.ctx.in_static_block {
                return self.err(
                    "for await is not allowed in class static blocks",
                    self.tok.span,
                );
            }
            is_await = true;
            self.advance()?;
        }
        self.expect(Punct::LParen)?;
        // A lexical declaration in the head gets its own scope.
        self.scopes.push_block();
        let r = self.parse_for_rest(start, is_await);
        self.scopes.pop();
        r
    }

    fn parse_for_rest(&mut self, start: u32, is_await: bool) -> Result<Statement> {
        // Empty init: `for (;;)`
        if self.at(Punct::Semicolon) {
            if is_await {
                return self.unexpected();
            }
            return self.parse_for_classic(start, None);
        }

        // Declarations.
        let decl_kind = if self.at_kw("var") {
            Some(VariableKind::Var)
        } else if self.at_kw("const") {
            Some(VariableKind::Const)
        } else if self.at_kw("let") && self.let_starts_declaration()? {
            Some(VariableKind::Let)
        } else {
            match self.at_using_declaration(true)? {
                Some(true) => Some(VariableKind::AwaitUsing),
                Some(false) => Some(VariableKind::Using),
                None => None,
            }
        };
        if let Some(kind) = decl_kind {
            let decl_start = self.tok.span.start;
            let decl = self.with_in(false, |p| p.parse_variable_declaration(kind, true))?;
            let is_of = self.at_kw("of");
            let is_in = self.at_kw("in");
            if is_in && kind.is_using() {
                return self.err("for-in loops may not use a using declaration", decl.span);
            }
            if (is_of || is_in) && decl.declarations.len() == 1 {
                let d = &decl.declarations[0];
                if d.init.is_some() {
                    // Annex B: `for (var x = 1 in o)` in sloppy code with a
                    // simple binding.
                    let annex_b = is_in
                        && !self.ctx.strict
                        && kind == VariableKind::Var
                        && matches!(d.id, Pattern::Ident(_));
                    if !annex_b {
                        return self.err(
                            "for-in/of loop variable declaration may not have an initializer",
                            d.span,
                        );
                    }
                }
                let left = ForLeft::VariableDeclaration(decl);
                return self.parse_for_in_of_rest(start, left, is_of, is_await);
            }
            if is_await {
                return self.unexpected();
            }
            // Classic for: initializers are required for const/destructuring.
            for d in &decl.declarations {
                if d.init.is_none() {
                    if kind.is_const() {
                        let what = if kind == VariableKind::Const { "const" } else { "using" };
                        return self.err(format!("missing initializer in {what} declaration"), d.span);
                    }
                    if !matches!(d.id, Pattern::Ident(_)) {
                        return self
                            .err("missing initializer in destructuring declaration", d.span);
                    }
                }
            }
            let _ = decl_start;
            return self.parse_for_classic(start, Some(ForInit::VariableDeclaration(decl)));
        }

        // Expression or assignment pattern.
        let init_start = self.tok.span.start;
        let starts_with_let = self.at_kw("let");
        let starts_with_async = self.at_kw("async");
        let saved_cover = self.pending_cover.take();
        let init = self.with_in(false, |p| p.parse_expression())?;
        let is_of = self.at_kw("of");
        let is_in = self.at_kw("in");
        if is_of || is_in {
            if is_of && starts_with_let {
                return self.err(
                    "the left-hand side of a for-of loop may not start with 'let'",
                    Span::new(init_start, init.span().end),
                );
            }
            if is_of && !is_await && starts_with_async && matches!(init, Expression::Ident(_)) {
                return self.err(
                    "the left-hand side of a for-of loop may not be 'async'",
                    init.span(),
                );
            }
            if !matches!(
                init,
                Expression::Ident(_)
                    | Expression::Member(_)
                    | Expression::Call(_)
                    | Expression::Object(_)
                    | Expression::Array(_)
                    | Expression::Paren(..)
            ) {
                return self.err("invalid left-hand side in for-in/of loop", init.span());
            }
            let left = self.expr_to_pattern(init, false)?;
            self.pending_cover = saved_cover;
            // Only simple targets and patterns: `for ((a) of b)` is fine,
            // `for (a + b of c)` is not (expr_to_pattern rejects it).
            return self.parse_for_in_of_rest(start, ForLeft::Pattern(left), is_of, is_await);
        }
        self.settle_cover_for(&init, saved_cover)?;
        if is_await {
            return self.unexpected();
        }
        self.parse_for_classic(start, Some(ForInit::Expression(init)))
    }

    fn settle_cover_for(
        &mut self,
        expr: &Expression,
        saved: Option<(Span, &'static str)>,
    ) -> Result<()> {
        if let Some((span, msg)) = self.pending_cover.take() {
            let _ = expr;
            return self.err(msg, span);
        }
        self.pending_cover = saved;
        Ok(())
    }

    fn parse_for_classic(&mut self, start: u32, init: Option<ForInit>) -> Result<Statement> {
        self.expect(Punct::Semicolon)?;
        let test = if self.at(Punct::Semicolon) {
            None
        } else {
            Some(self.with_in(true, |p| p.parse_expression())?)
        };
        self.expect(Punct::Semicolon)?;
        let update = if self.at(Punct::RParen) {
            None
        } else {
            Some(self.with_in(true, |p| p.parse_expression())?)
        };
        self.expect(Punct::RParen)?;
        let body = self.parse_loop_body()?;
        Ok(Statement::For(Box::new(ForStatement {
            init,
            test,
            update,
            body,
            span: self.span_from(start),
        })))
    }

    fn parse_for_in_of_rest(
        &mut self,
        start: u32,
        left: ForLeft,
        is_of: bool,
        is_await: bool,
    ) -> Result<Statement> {
        if is_await && !is_of {
            return self.unexpected();
        }
        self.advance()?; // in / of
        let right = if is_of {
            self.with_in(true, |p| p.parse_assignment_expression())?
        } else {
            self.with_in(true, |p| p.parse_expression())?
        };
        self.expect(Punct::RParen)?;
        let body = self.parse_loop_body()?;
        let span = self.span_from(start);
        if is_of {
            Ok(Statement::ForOf(Box::new(ForOfStatement {
                left,
                right,
                body,
                is_await,
                span,
            })))
        } else {
            Ok(Statement::ForIn(Box::new(ForInStatement {
                left,
                right,
                body,
                span,
            })))
        }
    }
}
