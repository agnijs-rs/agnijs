//! Errors crossing the engine interface.

use crate::value::Value;
use std::fmt;

/// One frame of a JavaScript stack trace.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StackFrame {
    pub function: String,
    /// The script name given at evaluation (a URL in the bundle).
    pub script: String,
    /// 1-based line, 0 when unknown.
    pub line: u32,
    /// 1-based column, 0 when unknown.
    pub column: u32,
}

/// A JavaScript exception as seen by the host.
#[derive(Clone, Debug)]
pub struct Exception {
    /// The thrown value.
    pub value: Value,
    /// The `name` of an Error object, or the `typeof` of a primitive.
    pub name: String,
    /// The `message` of an Error object, or the stringified primitive.
    pub message: String,
    /// Stack frames, innermost first. May be empty.
    pub stack: Vec<StackFrame>,
}

impl Exception {
    pub fn is(&self, name: &str) -> bool {
        self.name == name
    }
}

/// Every failure an engine call can report.
#[derive(Clone, Debug)]
pub enum Error {
    /// JavaScript code threw.
    Exception(Exception),
    /// The host's interrupt hook asked execution to stop.
    Interrupted,
    /// The memory limit was hit.
    OutOfMemory,
    /// The fuel budget set with [`crate::Engine::set_fuel`] was spent. Like
    /// an interrupt, JavaScript cannot catch it.
    OutOfFuel,
    /// A handle from another engine, or an already-released slot.
    ForeignHandle,
    /// The realm id is unknown.
    UnknownRealm,
    /// Bytecode produced by a different engine or format version.
    BytecodeMismatch { expected: String, found: String },
    /// Bytecode bytes that did not decode.
    BytecodeCorrupt(String),
    /// The engine does not provide this optional capability.
    Unsupported(&'static str),
    /// A realm holds state a snapshot cannot carry ([`crate::Engine::snapshot`]):
    /// host objects and functions, pending jobs, generators, modules and the
    /// other cases the message names.
    SnapshotUnsupported(String),
    /// The host hook failed (module resolution or loading).
    Host(String),
    /// An engine-internal failure that is not a JS exception.
    Internal(String),
}

impl Error {
    pub fn exception(&self) -> Option<&Exception> {
        match self {
            Error::Exception(e) => Some(e),
            _ => None,
        }
    }
    pub fn is_exception_named(&self, name: &str) -> bool {
        self.exception().map(|e| e.is(name)).unwrap_or(false)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Exception(e) => {
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
            Error::Interrupted => f.write_str("execution interrupted by host"),
            Error::OutOfMemory => f.write_str("memory limit exceeded"),
            Error::OutOfFuel => f.write_str("fuel budget exhausted"),
            Error::ForeignHandle => f.write_str("handle does not belong to this engine"),
            Error::UnknownRealm => f.write_str("unknown realm"),
            Error::BytecodeMismatch { expected, found } => {
                write!(
                    f,
                    "bytecode version mismatch: expected {expected}, found {found}"
                )
            }
            Error::BytecodeCorrupt(m) => write!(f, "bytecode corrupt: {m}"),
            Error::Unsupported(what) => write!(f, "unsupported by this engine: {what}"),
            Error::SnapshotUnsupported(m) => write!(f, "cannot snapshot the realm: {m}"),
            Error::Host(m) => write!(f, "host error: {m}"),
            Error::Internal(m) => write!(f, "engine internal error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
