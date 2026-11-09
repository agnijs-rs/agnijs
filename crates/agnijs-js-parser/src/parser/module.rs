//! Import and export declarations, and module-level early errors.

use super::scope::LexKind;
use super::Parser;
use crate::ast::*;
use crate::error::Result;
use crate::lexer::{Punct, TokenKind};
use foldhash::{HashMap, HashMapExt, HashSet, HashSetExt};
use std::rc::Rc;

impl<'s> Parser<'s> {
    fn parse_module_export_name(&mut self) -> Result<ModuleExportName> {
        match &self.tok.kind {
            TokenKind::String { value, .. } => {
                let value: Rc<[u16]> = Rc::from(&**value);
                let span = self.tok.span;
                // Must be well-formed Unicode.
                if String::from_utf16(&value).is_err() {
                    return self.err("module export name must be a well-formed string", span);
                }
                self.advance()?;
                Ok(ModuleExportName::String(StringLiteral { value, span }))
            }
            TokenKind::Ident { .. } => Ok(ModuleExportName::Ident(self.parse_identifier_name()?)),
            _ => self.unexpected(),
        }
    }

    fn parse_module_source(&mut self) -> Result<StringLiteral> {
        let TokenKind::String { value, .. } = &self.tok.kind else {
            return self.unexpected();
        };
        let value: Rc<[u16]> = Rc::from(&**value);
        let span = self.tok.span;
        self.advance()?;
        Ok(StringLiteral { value, span })
    }

    /// `with { type: "json" }` import attributes.
    fn parse_import_attributes(&mut self) -> Result<Vec<ImportAttribute>> {
        let mut attrs = Vec::new();
        if !self.at_kw("with") {
            return Ok(attrs);
        }
        self.advance()?;
        self.expect(Punct::LBrace)?;
        let mut seen: HashSet<Vec<u16>> = HashSet::new();
        while !self.at(Punct::RBrace) {
            let start = self.tok.span.start;
            let key = self.parse_module_export_name()?;
            if !seen.insert(key.as_utf16()) {
                return self.err("duplicate import attribute", key.span());
            }
            self.expect(Punct::Colon)?;
            let value = self.parse_module_source()?;
            attrs.push(ImportAttribute {
                key,
                value,
                span: self.span_from(start),
            });
            if !self.at(Punct::RBrace) {
                self.expect(Punct::Comma)?;
            }
        }
        self.expect(Punct::RBrace)?;
        Ok(attrs)
    }

    fn declare_import(&mut self, id: &Ident) -> Result<()> {
        self.scopes
            .declare_lexical(id.name.clone(), id.span, LexKind::Lexical, true)
    }

    pub(crate) fn parse_import_declaration(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.expect_kw("import")?;
        let mut specifiers = Vec::new();
        if let TokenKind::String { .. } = &self.tok.kind {
            let source = self.parse_module_source()?;
            let attributes = self.parse_import_attributes()?;
            self.consume_semicolon()?;
            return Ok(Statement::Import(Box::new(ImportDeclaration {
                specifiers,
                source,
                attributes,
                span: self.span_from(start),
            })));
        }
        // Default binding.
        if let TokenKind::Ident { .. } = &self.tok.kind {
            let id = self.parse_binding_identifier()?;
            self.declare_import(&id)?;
            specifiers.push(ImportSpecifier::Default(id));
            if !self.eat(Punct::Comma)? {
                return self.finish_import(start, specifiers);
            }
        }
        if self.eat(Punct::Star)? {
            self.expect_kw("as")?;
            let id = self.parse_binding_identifier()?;
            self.declare_import(&id)?;
            specifiers.push(ImportSpecifier::Namespace(id));
        } else if self.eat(Punct::LBrace)? {
            while !self.at(Punct::RBrace) {
                let sstart = self.tok.span.start;
                let imported_tok = self.tok.clone();
                let imported = self.parse_module_export_name()?;
                let local = if self.eat_kw("as")? {
                    self.parse_binding_identifier()?
                } else {
                    match &imported {
                        ModuleExportName::Ident(i) => {
                            let TokenKind::Ident { escaped, .. } = imported_tok.kind else {
                                unreachable!()
                            };
                            self.check_identifier(&i.name, i.span, escaped)?;
                            self.check_binding_identifier(&i.name, i.span)?;
                            Ident {
                                name: i.name.clone(),
                                span: i.span,
                            }
                        }
                        ModuleExportName::String(s) => {
                            return self.err("string import names require 'as'", s.span);
                        }
                    }
                };
                self.declare_import(&local)?;
                specifiers.push(ImportSpecifier::Named {
                    imported,
                    local,
                    span: self.span_from(sstart),
                });
                if !self.at(Punct::RBrace) {
                    self.expect(Punct::Comma)?;
                }
            }
            self.expect(Punct::RBrace)?;
        } else {
            return self.unexpected();
        }
        self.finish_import(start, specifiers)
    }

    fn finish_import(&mut self, start: u32, specifiers: Vec<ImportSpecifier>) -> Result<Statement> {
        self.expect_kw("from")?;
        let source = self.parse_module_source()?;
        let attributes = self.parse_import_attributes()?;
        self.consume_semicolon()?;
        Ok(Statement::Import(Box::new(ImportDeclaration {
            specifiers,
            source,
            attributes,
            span: self.span_from(start),
        })))
    }

    pub(crate) fn parse_export_declaration(&mut self) -> Result<Statement> {
        let start = self.tok.span.start;
        self.expect_kw("export")?;

        // export default ...
        if self.at_kw("default") {
            self.advance()?;
            let declaration = if self.at_kw("function") || self.at_async_function()? {
                let is_async = self.at_kw("async");
                let f = self.parse_function_declaration(is_async, true)?;
                if f.id.is_none() {
                    self.declare_default_binding(f.span)?;
                }
                ExportDefaultKind::Function(Box::new(f))
            } else if self.at_kw("class") {
                let c = self.parse_class_declaration(true)?;
                if c.id.is_none() {
                    self.declare_default_binding(c.span)?;
                }
                ExportDefaultKind::Class(Box::new(c))
            } else {
                let e = self.with_in(true, |p| p.parse_assignment_expression())?;
                self.consume_semicolon()?;
                self.declare_default_binding(e.span())?;
                ExportDefaultKind::Expression(e)
            };
            return Ok(Statement::ExportDefault(Box::new(
                ExportDefaultDeclaration {
                    declaration,
                    span: self.span_from(start),
                },
            )));
        }

        // export * from / export * as x from
        if self.eat(Punct::Star)? {
            let exported = if self.eat_kw("as")? {
                Some(self.parse_module_export_name()?)
            } else {
                None
            };
            self.expect_kw("from")?;
            let source = self.parse_module_source()?;
            let attributes = self.parse_import_attributes()?;
            self.consume_semicolon()?;
            return Ok(Statement::ExportAll(Box::new(ExportAllDeclaration {
                exported,
                source,
                attributes,
                span: self.span_from(start),
            })));
        }

        // export { a, b as c } [from "x"]
        if self.eat(Punct::LBrace)? {
            let mut specifiers = Vec::new();
            let mut local_idents_unescaped: Vec<bool> = Vec::new();
            while !self.at(Punct::RBrace) {
                let sstart = self.tok.span.start;
                let tok_escaped = matches!(self.tok.kind, TokenKind::Ident { escaped: true, .. });
                let local = self.parse_module_export_name()?;
                let exported = if self.eat_kw("as")? {
                    self.parse_module_export_name()?
                } else {
                    local.clone()
                };
                specifiers.push(ExportSpecifier {
                    local,
                    exported,
                    span: self.span_from(sstart),
                });
                local_idents_unescaped.push(!tok_escaped);
                if !self.at(Punct::RBrace) {
                    self.expect(Punct::Comma)?;
                }
            }
            self.expect(Punct::RBrace)?;
            let mut source = None;
            let mut attributes = Vec::new();
            if self.eat_kw("from")? {
                source = Some(self.parse_module_source()?);
                attributes = self.parse_import_attributes()?;
            } else {
                // Without `from`, local names are references: they must be
                // identifiers (not strings, not reserved words).
                for s in &specifiers {
                    match &s.local {
                        ModuleExportName::String(st) => {
                            return self.err("export of a string name requires 'from'", st.span);
                        }
                        ModuleExportName::Ident(i) => {
                            if self.is_reserved_word(&i.name) || Self::is_strict_reserved(&i.name) {
                                return self
                                    .err(format!("unexpected reserved word '{}'", i.name), i.span);
                            }
                        }
                    }
                }
            }
            self.consume_semicolon()?;
            return Ok(Statement::ExportNamed(Box::new(ExportNamedDeclaration {
                declaration: None,
                specifiers,
                source,
                attributes,
                span: self.span_from(start),
            })));
        }

        // export var/let/const/function/class
        let name = match &self.tok.kind {
            TokenKind::Ident {
                name,
                escaped: false,
            } => Some(name.clone()),
            _ => None,
        };
        let declaration = match name {
            Some(name) => match &*name {
                "var" | "let" | "const" => {
                    let kind = match &*name {
                        "var" => VariableKind::Var,
                        "let" => VariableKind::Let,
                        _ => VariableKind::Const,
                    };
                    let d = self.parse_variable_declaration(kind, false)?;
                    self.consume_semicolon()?;
                    Statement::VariableDeclaration(d)
                }
                "function" => Statement::FunctionDeclaration(Box::new(
                    self.parse_function_declaration(false, false)?,
                )),
                "async" if self.at_async_function()? => Statement::FunctionDeclaration(Box::new(
                    self.parse_function_declaration(true, false)?,
                )),
                "class" => {
                    Statement::ClassDeclaration(Box::new(self.parse_class_declaration(false)?))
                }
                _ => return self.unexpected(),
            },
            _ => return self.unexpected(),
        };
        Ok(Statement::ExportNamed(Box::new(ExportNamedDeclaration {
            declaration: Some(declaration),
            specifiers: Vec::new(),
            source: None,
            attributes: Vec::new(),
            span: self.span_from(start),
        })))
    }

    /// `export default <anonymous>` binds the hidden `*default*` name.
    fn declare_default_binding(&mut self, span: crate::span::Span) -> Result<()> {
        let name = self.intern("*default*");
        self.scopes
            .declare_lexical(name, span, LexKind::Lexical, true)
    }

    /// Module-level checks after the whole body is parsed: exported names
    /// are unique, and every `export { x }` without `from` names a
    /// module-level declaration.
    pub(crate) fn check_module_exports(&mut self, body: &[Statement]) -> Result<()> {
        let mut declared: HashSet<Vec<u16>> = HashSet::new();
        let mut exported: HashMap<Vec<u16>, crate::span::Span> = HashMap::new();
        let mut local_refs: Vec<(Vec<u16>, crate::span::Span)> = Vec::new();

        fn add_pattern(p: &Pattern, declared: &mut HashSet<Vec<u16>>) {
            let mut names = Vec::new();
            Parser::bound_names(p, &mut names);
            for n in names {
                declared.insert(n.name.encode_utf16().collect());
            }
        }
        fn add_decl(s: &Statement, declared: &mut HashSet<Vec<u16>>) {
            match s {
                Statement::VariableDeclaration(d) => {
                    for decl in &d.declarations {
                        add_pattern(&decl.id, declared);
                    }
                }
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        declared.insert(id.name.encode_utf16().collect());
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        declared.insert(id.name.encode_utf16().collect());
                    }
                }
                _ => {}
            }
        }
        let utf16 = |s: &str| s.encode_utf16().collect::<Vec<u16>>();

        for s in body {
            match s {
                Statement::Import(d) => {
                    for sp in &d.specifiers {
                        let local = match sp {
                            ImportSpecifier::Default(i) | ImportSpecifier::Namespace(i) => i,
                            ImportSpecifier::Named { local, .. } => local,
                        };
                        declared.insert(utf16(&local.name));
                    }
                }
                Statement::ExportNamed(e) => {
                    if let Some(d) = &e.declaration {
                        add_decl(d, &mut declared);
                        let mut names = Vec::new();
                        match d {
                            Statement::VariableDeclaration(v) => {
                                for decl in &v.declarations {
                                    let mut ids = Vec::new();
                                    Parser::bound_names(&decl.id, &mut ids);
                                    for i in ids {
                                        names.push((utf16(&i.name), i.span));
                                    }
                                }
                            }
                            Statement::FunctionDeclaration(f) => {
                                if let Some(id) = &f.id {
                                    names.push((utf16(&id.name), id.span));
                                }
                            }
                            Statement::ClassDeclaration(c) => {
                                if let Some(id) = &c.id {
                                    names.push((utf16(&id.name), id.span));
                                }
                            }
                            _ => {}
                        }
                        for (n, span) in names {
                            if exported.insert(n, span).is_some() {
                                return self.err("duplicate export name", span);
                            }
                        }
                    }
                    for sp in &e.specifiers {
                        let name = sp.exported.as_utf16();
                        if exported.insert(name, sp.exported.span()).is_some() {
                            return self.err("duplicate export name", sp.exported.span());
                        }
                        if e.source.is_none() {
                            local_refs.push((sp.local.as_utf16(), sp.local.span()));
                        }
                    }
                }
                Statement::ExportDefault(e) => {
                    if exported.insert(utf16("default"), e.span).is_some() {
                        return self.err("duplicate export name 'default'", e.span);
                    }
                    match &e.declaration {
                        ExportDefaultKind::Function(f) => {
                            if let Some(id) = &f.id {
                                declared.insert(utf16(&id.name));
                            }
                        }
                        ExportDefaultKind::Class(c) => {
                            if let Some(id) = &c.id {
                                declared.insert(utf16(&id.name));
                            }
                        }
                        _ => {}
                    }
                }
                Statement::ExportAll(e) => {
                    if let Some(name) = &e.exported {
                        if exported.insert(name.as_utf16(), name.span()).is_some() {
                            return self.err("duplicate export name", name.span());
                        }
                    }
                }
                other => add_decl(other, &mut declared),
            }
        }
        for (name, span) in local_refs {
            if !declared.contains(&name) {
                let shown = String::from_utf16_lossy(&name);
                return self.err(format!("export '{shown}' is not defined in module"), span);
            }
        }
        Ok(())
    }
}
