//! agnijs-js-vm: the AgniJS JavaScript engine's bytecode interpreter, heap,
//! garbage collector and standard library.
//!
//! The crate is pure Rust with no platform-specific code. `unsafe` is not
//! used: heap values are arena indices (`value::ObjId` and friends) and the
//! collector is a precise mark-sweep over those arenas (see `heap.rs`).

// ADR 0019 J10 (TASK-100.32): engine crates stay free of `unsafe`.
#![forbid(unsafe_code)]

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
pub mod snapshot;
pub mod stack_trace;
pub mod string;
pub mod symbol;
pub mod value;
pub mod vm;

pub use debug::make_debuggee;
pub use engine::{make_engine, make_engine_from_snapshot};
pub use vm::Vm;

#[cfg(test)]
mod conformance {
    agnijs_engine_api::conformance_tests!(|host| crate::make_engine(host));
}

/// The conformance kit again, every engine restored from a snapshot of a
/// fresh realm (TASK-101.2).
#[cfg(test)]
mod conformance_from_snapshot {
    fn fresh_snapshot() -> Vec<u8> {
        thread_local! {
            static BYTES: Vec<u8> = {
                let mut vm = crate::Vm::new(agnijs_engine_api::conformance::TestHost::new());
                vm.write_snapshot(agnijs_engine_api::RealmId(0)).expect("snapshot")
            };
        }
        BYTES.with(|b| b.clone())
    }
    agnijs_engine_api::conformance_tests!(|host| crate::make_engine_from_snapshot(host, &fresh_snapshot()).expect("restore"));
}
