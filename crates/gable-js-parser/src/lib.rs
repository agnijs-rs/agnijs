//! gable-js-parser: the ECMAScript parser of the Gable engine.
//!
//! A handwritten lexer and recursive-descent parser producing an ESTree-
//! shaped AST with source spans on every node. Early errors (duplicate
//! declarations, strict-mode restrictions, invalid assignment targets,
//! private-name resolution, module export checks) are reported as
//! `SyntaxError`s with locations. Covers ES2025 plus Annex B, minus
//! `IsHTMLDDA` (which is a runtime concept anyway).

pub mod ast;
pub mod error;
pub mod lexer;
pub mod number;
pub mod parser;
pub mod span;

pub use error::SyntaxError;
pub use parser::{parse_module, parse_script};
pub use span::{LineCol, LineTable, Span};

#[cfg(test)]
mod tests;
