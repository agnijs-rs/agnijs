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
use agnijs_js_parser::source::LoneSurrogate;
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
    source_text_with(name, text, &[])
}

fn source_text_with(name: &str, text: &str, lone: &[LoneSurrogate]) -> Rc<SourceText> {
    Rc::new(SourceText {
        name: Rc::from(name),
        text: Rc::from(text),
        lone_surrogates: Rc::from(lone),
    })
}

/// Analyse a program, failing when an installed stack budget
/// (`agnijs_js_parser::stack`) ran out during the walk.
fn analyse(program: &agnijs_js_parser::ast::Program) -> Result<analysis::Analyses> {
    let analyses = analysis::analyse_program(program);
    if analyses.too_deep {
        return Err(stack_exhausted());
    }
    Ok(analyses)
}

pub(crate) fn stack_exhausted() -> CompileError {
    CompileError {
        message: agnijs_js_parser::stack::EXHAUSTED_MESSAGE.into(),
        span: Span::empty(0),
    }
}

/// Parse and compile a script.
pub fn compile_script(source: &str, name: &str) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_script(source)?;
    let analyses = analyse(&program)?;
    let mut c = compile::Compiler::new(source_text(name, source), &analyses, false);
    c.compile_script(&program, name)
}

/// Parse and compile a module.
pub fn compile_module(source: &str, name: &str) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_module(source)?;
    let analyses = analyse(&program)?;
    let mut c = compile::Compiler::new(source_text(name, source), &analyses, false);
    c.compile_module(&program, name)
}

/// Parse and compile eval code. For direct eval the options describe the
/// calling context; indirect eval uses `EvalOptions::default()`.
pub fn compile_eval(source: &str, name: &str, options: &EvalOptions) -> Result<Rc<CodeBlock>> {
    compile_eval_with(source, &[], name, options)
}

/// [`compile_eval`] for eval code given as UTF-16 code units (a JavaScript
/// string), lone surrogates included.
pub fn compile_eval_utf16(
    source: &[u16],
    name: &str,
    options: &EvalOptions,
) -> Result<Rc<CodeBlock>> {
    let (text, lone) = agnijs_js_parser::source::from_utf16(source);
    compile_eval_with(&text, &lone, name, options)
}

fn compile_eval_with(
    source: &str,
    lone: &[LoneSurrogate],
    name: &str,
    options: &EvalOptions,
) -> Result<Rc<CodeBlock>> {
    let program = agnijs_js_parser::parse_eval_with(source, lone, options)?;
    let analyses = analyse(&program)?;
    let mut c = compile::Compiler::new(source_text_with(name, source, lone), &analyses, true);
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
    let cb = compile_dynamic_function_with((params, &[]), (body, &[]), kind)?;
    let text = cb.source.text.to_string();
    // `(` + source + `)`
    Ok((cb, text[1..text.len() - 1].to_string()))
}

/// [`compile_dynamic_function`] for parameters and body given as UTF-16
/// code units (JavaScript strings), lone surrogates included. Returns the
/// code block; its source text is the synthesised function in parentheses.
pub fn compile_dynamic_function_utf16(
    params: &[u16],
    body: &[u16],
    kind: DynamicFunctionKind,
) -> Result<Rc<CodeBlock>> {
    let (params, params_lone) = agnijs_js_parser::source::from_utf16(params);
    let (body, body_lone) = agnijs_js_parser::source::from_utf16(body);
    compile_dynamic_function_with((&params, &params_lone), (&body, &body_lone), kind)
}

/// Source text with its lone surrogates (see `agnijs_js_parser::source`).
type Piece<'a> = (&'a str, &'a [LoneSurrogate]);

fn compile_dynamic_function_with(
    (params, params_lone): Piece,
    (body, body_lone): Piece,
    kind: DynamicFunctionKind,
) -> Result<Rc<CodeBlock>> {
    // The tables of `params` and `body`, moved to where each sits in a
    // synthesised text.
    fn shifted(parts: &[(&[LoneSurrogate], usize)]) -> Vec<LoneSurrogate> {
        let mut out = Vec::new();
        for (lone, at) in parts {
            out.extend(lone.iter().map(|l| LoneSurrogate {
                offset: l.offset + *at as u32,
                unit: l.unit,
            }));
        }
        out
    }
    let prefix = match kind {
        DynamicFunctionKind::Normal => "function",
        DynamicFunctionKind::Generator => "function*",
        DynamicFunctionKind::Async => "async function",
        DynamicFunctionKind::AsyncGenerator => "async function*",
    };
    // Parameters and body must each parse on their own, so that neither
    // can inject syntax into the other.
    let params_at = prefix.len() + " anonymous(".len() + 1;
    let params_only = format!("({prefix} anonymous({params}\n) {{}})");
    agnijs_js_parser::parse_script_with(&params_only, &shifted(&[(params_lone, params_at)]))?;
    let body_only = format!("({prefix} anonymous() {{\n{body}\n}})");
    let body_at = prefix.len() + " anonymous() {\n".len() + 1;
    agnijs_js_parser::parse_script_with(&body_only, &shifted(&[(body_lone, body_at)]))?;
    let wrapped = format!("({prefix} anonymous({params}\n) {{\n{body}\n}})");
    let body_at = params_at + params.len() + "\n) {\n".len();
    let lone = shifted(&[(params_lone, params_at), (body_lone, body_at)]);
    let program = agnijs_js_parser::parse_script_with(&wrapped, &lone)?;
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
    let analyses = analyse(&program)?;
    let source = source_text_with("anonymous", &wrapped, &lone);
    let mut c = compile::Compiler::new(source, &analyses, false);
    c.compile_standalone_function(func, &program)
}

#[cfg(test)]
mod tests;
