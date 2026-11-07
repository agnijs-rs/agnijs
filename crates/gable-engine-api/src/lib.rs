//! The Gable engine interface: the boundary the Host runtime codes against,
//! satisfied by the Gable engine and by the interim QuickJS-NG adapter.
//!
//! See `docs/js-engine-spec.md` section 7 and ADR 0005 for the design.
//!
//! - [`Engine`]: evaluation, modules, jobs, bytecode, limits, values, host
//!   functions and classes. Every call takes `&mut self`.
//! - [`Host`]: hooks the engine calls out through (module loading,
//!   `import.meta`, rejection tracking, console, interrupt).
//! - [`Value`] and [`Handle`]: primitives are copied; heap values are
//!   persistent, reference-counted roots with no lifetime parameter.
//! - [`conformance`]: the kit every implementation must pass.

pub mod conformance;
pub mod engine;
pub mod error;
pub mod host;
pub mod value;

pub use engine::{Engine, EngineInfo, ErrorKind, MemoryUsage};
pub use error::{Error, Exception, Result, StackFrame};
pub use host::{
    CallContext, ConsoleLevel, Host, HostClassDef, HostClassId, HostFunction, HostObject,
    ImportAttribute, ModuleSource, RejectionEvent, Tracer,
};
pub use value::{
    next_engine_id, EngineId, Handle, HandleRegistry, HeapKind, JsBigInt, JsString, PromiseState,
    PropertyKey, RealmId, TypeOf, Value,
};

/// Bytecode container header shared by every engine: 4-byte magic, then a
/// NUL-terminated format tag (engine name and format version), then the
/// engine's payload. Engines reject a mismatch before touching the payload.
pub mod bytecode {
    use crate::error::{Error, Result};

    pub const MAGIC: &[u8; 4] = b"GBJS";

    pub fn wrap(format: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + format.len() + 1 + payload.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(format.as_bytes());
        out.push(0);
        out.extend_from_slice(payload);
        out
    }

    /// Check the header against `format` and return the payload.
    pub fn unwrap<'a>(format: &str, bytes: &'a [u8]) -> Result<&'a [u8]> {
        if bytes.len() < 5 || &bytes[..4] != MAGIC {
            return Err(Error::BytecodeMismatch {
                expected: format!("{} {format}", String::from_utf8_lossy(MAGIC)),
                found: "not Gable bytecode".into(),
            });
        }
        let rest = &bytes[4..];
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| Error::BytecodeCorrupt("unterminated format tag".into()))?;
        let found = String::from_utf8_lossy(&rest[..nul]).into_owned();
        if found != format {
            return Err(Error::BytecodeMismatch { expected: format.to_string(), found });
        }
        Ok(&rest[nul + 1..])
    }
}
