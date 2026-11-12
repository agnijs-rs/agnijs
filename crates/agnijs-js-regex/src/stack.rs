//! An optional native stack budget for the recursive parts of the engine.
//!
//! The parser, the compiler and the VM's native reentry recurse on the
//! native stack, so deeply nested input (`[[[[...` 100000 deep) can exhaust
//! a small host thread stack. An embedder that cannot choose its stack size
//! (the C API) installs a budget with [`StackBudget::enter`]; the recursive
//! paths then call [`exhausted`] and fail with an error instead of
//! overflowing. Without a budget [`exhausted`] is always false.
//!
//! The VM installs a budget around every call into it, [`DEFAULT_BUDGET`]
//! unless the host sets another (`Engine::set_stack_budget`), so engines
//! made through the Rust API are bounded by default too.
//!
//! The stack pointer is approximated by the address of a local, which is
//! safe code; stacks grow downwards on every supported target. Under
//! AddressSanitizer, run with `ASAN_OPTIONS=detect_stack_use_after_return=0`:
//! its fake stacks (on by default on Linux) put locals in heap memory,
//! where their addresses say nothing about the stack, and the budget then
//! runs out at random depths.

use std::cell::Cell;

thread_local! {
    /// The lowest stack address the recursive paths may reach; 0 for none.
    static LIMIT: Cell<usize> = const { Cell::new(0) };
}

/// The message used when the budget runs out.
pub const EXHAUSTED_MESSAGE: &str = "Maximum call stack size exceeded";

/// The budget an engine installs unless its host sets another: 512 KiB.
/// With [`HEADROOM`] it needs 768 KiB of free stack at the call into the
/// engine, so a 2 MiB thread (the Rust default) and a 1 MiB one (the
/// Windows main thread) both have room for it beside their own frames.
pub const DEFAULT_BUDGET: usize = 512 * 1024;

/// Stack the engine may use past the budget before it notices (between
/// checks, and to build the error). Keep this much free beyond the budget.
pub const HEADROOM: usize = 256 * 1024;

#[inline(never)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::hint::black_box(std::ptr::addr_of!(marker)) as usize
}

/// Whether the current thread has used up the installed stack budget.
#[inline]
pub fn exhausted() -> bool {
    let limit = LIMIT.with(Cell::get);
    limit != 0 && stack_position() < limit
}

/// The lowest stack address the installed budget allows, or 0 without one:
/// for code that bounds its own use of the stack by an address (the JIT's
/// machine frames), where the recursive paths ask [`exhausted`].
#[inline]
pub fn limit() -> usize {
    LIMIT.with(Cell::get)
}

/// An installed budget; dropping it restores the previous one.
pub struct StackBudget {
    previous: usize,
}

impl StackBudget {
    /// Allows the recursive paths `bytes` of stack below the current
    /// position. A budget already installed further out is kept when it is
    /// tighter, so a nested entry (a host callback calling back in) never
    /// extends the outer budget.
    pub fn enter(bytes: usize) -> StackBudget {
        let limit = stack_position().saturating_sub(bytes).max(1);
        let previous = LIMIT.with(|l| {
            let previous = l.get();
            l.set(previous.max(limit));
            previous
        });
        StackBudget { previous }
    }
}

impl Drop for StackBudget {
    fn drop(&mut self) {
        LIMIT.with(|l| l.set(self.previous));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_budget_is_never_exhausted() {
        assert!(!exhausted());
    }

    #[test]
    fn a_zero_budget_is_exhausted_and_restored_on_drop() {
        {
            let _b = StackBudget::enter(0);
            fn deeper() -> bool {
                let pad = std::hint::black_box([0u8; 256]);
                exhausted() && pad[0] == 0
            }
            assert!(deeper());
        }
        assert!(!exhausted());
    }
}
