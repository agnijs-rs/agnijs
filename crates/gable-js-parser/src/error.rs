//! Syntax errors. Every early error in ECMAScript is a `SyntaxError`, so the
//! parser has a single error type with a message and a location.

use crate::span::Span;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub span: Span,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, span: Span) -> SyntaxError {
        SyntaxError { message: message.into(), span }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SyntaxError: {} (at {}..{})", self.message, self.span.start, self.span.end)
    }
}

impl std::error::Error for SyntaxError {}

pub type Result<T> = std::result::Result<T, SyntaxError>;
