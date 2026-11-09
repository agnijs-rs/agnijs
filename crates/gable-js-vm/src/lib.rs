//! gable-js-vm: the Gable JavaScript engine's bytecode interpreter, heap,
//! garbage collector and standard library.
//!
//! The crate is pure Rust with no platform-specific code. `unsafe` is not
//! used: heap values are arena indices (`value::ObjId` and friends) and the
//! collector is a precise mark-sweep over those arenas (see `heap.rs`).

pub mod bigint;
pub mod builtins;
pub mod convert;
pub mod debug;
pub mod engine;
pub mod env;
pub mod gc;
pub mod heap;
pub mod interp;
pub mod modules;
pub mod object;
pub mod ops;
pub mod realm;
pub mod string;
pub mod symbol;
pub mod value;
pub mod vm;

pub use debug::make_debuggee;
pub use engine::make_engine;
pub use vm::Vm;

#[cfg(test)]
mod conformance {
    gable_engine_api::conformance_tests!(|host| crate::make_engine(host));
}
