//! The AgniJS C API: a stable C ABI over the engine interface
//! (`agnijs-engine-api`) for C, C++ and every language that can call C.
//!
//! The header is `include/agnijs.h`, generated from this crate by cbindgen
//! and checked in; `docs/c-api.md` is the guide and the ABI and versioning
//! policy, ADR 0021 the design record.
//!
//! The rules this crate keeps (TASK-103.3):
//!
//! - **No panic reaches the host.** Every exported function runs its body
//!   under `catch_unwind` ([`guard::ffi`]); a caught panic becomes
//!   [`AGNIJS_ERR_PANIC`] with a message, poisons the engine it happened in,
//!   and later calls on that engine return [`AGNIJS_ERR_POISONED`] while
//!   freeing it stays safe. Clippy denies `unwrap`, `expect`, `panic!`,
//!   indexing, slicing and unchecked arithmetic here, and the crate refuses
//!   to build with `panic=abort`.
//! - **Nothing the host holds is a Rust pointer.** Engines, realms, values,
//!   buffers, classes and hook sinks are integer ids checked on every use,
//!   so a freed, stale, foreign or garbage id is an error, not undefined
//!   behaviour.
//! - **No allocation crosses the boundary to be freed on the other side.**
//!   Output text is copied into memory the caller owns; output bytes live in
//!   buffers the engine owns and frees by id.
//! - **One thread per engine.** Engines live in a thread-local registry; a
//!   call from another thread returns [`AGNIJS_ERR_WRONG_THREAD`]. The only
//!   function callable from any thread is [`agnijs_engine_request_interrupt`].
//! - **Host callbacks are untrusted.** They run under `catch_unwind` too,
//!   their results are validated, and calls back into the engine from them
//!   follow the reentrancy rules in [`registry`].
//!
//! `unsafe` is confined to this crate: reading caller memory ([`ptrs`]),
//! calling host function pointers, and the engine lease of [`registry`].
//! Every block states its invariant.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::undocumented_unsafe_blocks,
    clippy::missing_safety_doc
)]
// The exported functions take raw pointers by design (they are the C ABI).
#![allow(clippy::not_unsafe_ptr_arg_deref)]

// A panic=abort build would turn every caught panic into a host abort.
#[cfg(panic = "abort")]
compile_error!("agnijs-capi must be built with panic=unwind: the C API catches panics so the host survives them");

mod api;
mod guard;
mod host;
mod ptrs;
mod registry;
mod table;
mod types;

pub use api::*;
pub use host::{agnijs_sink_set_error, agnijs_sink_set_module, agnijs_sink_set_source_map, agnijs_sink_set_string};
pub use types::*;

#[cfg(feature = "fault-injection")]
pub use guard::agnijs_test_inject_panic;
