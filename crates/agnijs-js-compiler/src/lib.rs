//! agnijs-js-compiler: ECMAScript AST → bytecode for the AgniJS VM.
//!
//! Entry points parse and compile scripts, modules, eval code and
//! dynamically created functions (`Function(...)`). The output is a tree
//! of [`CodeBlock`]s; the VM owns execution.

// ADR 0019 J10 (TASK-100.32): engine crates stay free of `unsafe`.
#![forbid(unsafe_code)]

pub mod analysis;
pub mod bytecode;
mod compile;
pub mod serialize;

pub use bytecode::*;
pub use agnijs_js_parser::EvalOptions;

use agnijs_js_parser::ast::{Expression, Statement};
use agnijs_js_parser::span::Span;
use agnijs_js_parser::SyntaxError;
use std::fmt;
use std::rc::Rc;

/// A compile-time error: a `SyntaxError` for the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SyntaxError: {}", self.message)
    }
}

impl std::error::Error for CompileError {}

impl From<SyntaxError> for CompileError {
    fn from(e: SyntaxError) -> CompileError {
        CompileError {
            message: e.message,
            span: e.span,
        }
    }
}

pub type Result<T> = std::result::Result<T, CompileError>;

fn source_text(name: &str, text: &str) -> Rc<SourceText> {
    Rc::new(SourceText {
        name: Rc::from(name),
        text: Rc::from(text),
    })
}

/// Parse and compile a script.
pub fn compile_script(source: &str, name: &str) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_script(source)?;
    let analyses = analysis::analyse_program(&program);
    let mut c = compile::Compiler::new(source_text(name, source), &analyses, false);
    c.compile_script(&program, name)
}

/// Parse and compile a module.
pub fn compile_module(source: &str, name: &str) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_module(source)?;
    let analyses = analysis::analyse_program(&program);
    let mut c = compile::Compiler::new(source_text(name, source), &analyses, false);
    c.compile_module(&program, name)
}

/// Parse and compile eval code. For direct eval the options describe the
/// calling context; indirect eval uses `EvalOptions::default()`.
pub fn compile_eval(source: &str, name: &str, options: &EvalOptions) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_eval(source, options)?;
    let analyses = analysis::analyse_program(&program);
    let direct = options.in_function
        || options.in_method
        || options.strict
        || !options.private_names.is_empty()
        || options.in_class_field_initializer;
    let _ = direct;
    let mut c = compile::Compiler::new(source_text(name, source), &analyses, true);
    c.compile_eval(&program, name)
}

/// The kind of function `CreateDynamicFunction` builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicFunctionKind {
    Normal,
    Generator,
    Async,
    AsyncGenerator,
}

/// Compile a function created by the `Function` constructor family. The
/// parameters and body are checked separately, as the spec requires, and
/// the returned code block is the function itself (to be closed over the
/// global environment). Also returns the synthesised source text.
pub fn compile_dynamic_function(
    params: &str,
    body: &str,
    kind: DynamicFunctionKind,
) -> Result<(Rc<CodeBlock>, String)> {
    let prefix = match kind {
        DynamicFunctionKind::Normal => "function",
        DynamicFunctionKind::Generator => "function*",
        DynamicFunctionKind::Async => "async function",
        DynamicFunctionKind::AsyncGenerator => "async function*",
    };
    let source = format!("{prefix} anonymous({params}\n) {{\n{body}\n}}");
    // Parameters and body must each parse on their own, so that neither
    // can inject syntax into the other.
    let params_only = format!("({prefix} anonymous({params}\n) {{}})");
    agnijs_js_parser::parse_script(&params_only)?;
    let body_only = format!("({prefix} anonymous() {{\n{body}\n}})");
    agnijs_js_parser::parse_script(&body_only)?;
    let wrapped = format!("({source})");
    let program = agnijs_js_parser::parse_script(&wrapped)?;
    let func = match program.body.as_slice() {
        [Statement::Expression(e)] => match e.expression.unparen() {
            Expression::Function(f) => f,
            _ => {
                return Err(CompileError {
                    message: "invalid function source".into(),
                    span: Span::empty(0),
                })
            }
        },
        _ => {
            return Err(CompileError {
                message: "invalid function source".into(),
                span: Span::empty(0),
            })
        }
    };
    let analyses = analysis::analyse_program(&program);
    let mut c = compile::Compiler::new(source_text("anonymous", &wrapped), &analyses, false);
    let cb = c.compile_standalone_function(func, &program)?;
    Ok((cb, source))
}

#[cfg(test)]
mod tests;
