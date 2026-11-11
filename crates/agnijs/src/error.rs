//! Typed errors for the embedding API.

use agnijs_engine_api as api;
use std::fmt;

/// An error a Rust function raises into JavaScript: it is thrown as an
/// `Error` object of this kind. Any `std::error::Error` converts into one
/// (kind `Error`, the error's `Display` text as the message), so `?` works
/// in host functions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsError {
    pub kind: api::ErrorKind,
    pub message: String,
}

impl JsError {
    pub fn new(kind: api::ErrorKind, message: impl Into<String>) -> Self {
        JsError {
            kind,
            message: message.into(),
        }
    }
    /// A plain `Error`.
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(api::ErrorKind::Error, message)
    }
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new(api::ErrorKind::TypeError, message)
    }
    pub fn range_error(message: impl Into<String>) -> Self {
        Self::new(api::ErrorKind::RangeError, message)
    }
}

impl<E: std::error::Error> From<E> for JsError {
    fn from(e: E) -> Self {
        JsError::error(e.to_string())
    }
}

/// A JavaScript exception that reached Rust.
#[derive(Clone, Debug)]
pub struct JsException {
    /// The thrown value's `name` (`TypeError`, `AbortError`, ...).
    pub name: String,
    pub message: String,
    pub stack: Vec<api::StackFrame>,
    /// The thrown value itself, for code that needs more than the text.
    pub value: api::Value,
}

/// What an embedding call can fail with.
#[derive(Debug)]
pub enum Error {
    /// JavaScript threw (or a promise rejected).
    Js(JsException),
    /// A value could not cross the boundary: a function, symbol or BigInt
    /// where data was expected, a cycle, or a serde type mismatch.
    Convert(String),
    /// `block_on` ran out of work: the promise is still pending and no Rust
    /// future is left that could settle it.
    Deadlock,
    /// `block_on` hit its time limit.
    TimedOut,
    /// Any other engine failure (interrupt, out of memory, bytecode, ...).
    Engine(api::Error),
}

impl Error {
    /// The JavaScript exception, when this is one.
    pub fn exception(&self) -> Option<&JsException> {
        match self {
            Error::Js(e) => Some(e),
            _ => None,
        }
    }
    /// True for a JavaScript exception whose `name` matches.
    pub fn is_js(&self, name: &str) -> bool {
        self.exception().is_some_and(|e| e.name == name)
    }
}

impl From<api::Error> for Error {
    fn from(e: api::Error) -> Self {
        match e {
            api::Error::Exception(x) => Error::Js(JsException {
                name: x.name,
                message: x.message,
                stack: x.stack,
                value: x.value,
            }),
            other => Error::Engine(other),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Js(e) => {
                write!(f, "{}: {}", e.name, e.message)?;
                for fr in &e.stack {
                    write!(
                        f,
                        "\n    at {} ({}:{}:{})",
                        fr.function, fr.script, fr.line, fr.column
                    )?;
                }
                Ok(())
            }
            Error::Convert(m) => write!(f, "conversion failed: {m}"),
            Error::Deadlock => f.write_str("promise is pending and nothing can settle it"),
            Error::TimedOut => f.write_str("timed out waiting for a promise"),
            Error::Engine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
