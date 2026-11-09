//! agnijs-js-parser: the ECMAScript parser of the AgniJS engine.
//!
//! A handwritten lexer and recursive-descent parser producing an ESTree-
//! shaped AST with source spans on every node. Early errors (duplicate
//! declarations, strict-mode restrictions, invalid assignment targets,
//! private-name resolution, module export checks) are reported as
//! `SyntaxError`s with locations. Covers ES2025 plus Annex B, minus
//! `IsHTMLDDA` (which is a runtime concept anyway).

// ADR 0019 J10 (TASK-100.32): engine crates stay free of `unsafe`.
#![forbid(unsafe_code)]

pub mod ast;
pub mod error;
pub mod lexer;
pub mod number;
pub mod parser;
pub mod span;

pub use error::SyntaxError;
pub use parser::{parse_eval, parse_module, parse_script, EvalOptions};
pub use span::{LineCol, LineTable, Span};

#[cfg(test)]
mod tests;
