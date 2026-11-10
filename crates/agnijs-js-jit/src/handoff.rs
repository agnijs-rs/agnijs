//! The interfaces reserved for deoptimization and GC maps (TASK-101.3
//! hand-off; specified in docs/design/jit-deopt-and-gc-maps.md). Nothing
//! here is implemented, and nothing calls it: the baseline tier needs
//! neither, because it never speculates and never holds a value outside
//! the frame across a call out (see the crate documentation). They become
//! necessary together, the first time compiled code keeps JavaScript
//! state in machine registers or stack slots across a safe point, or
//! assumes something a guard can later find false without a generic path
//! to fall back to: register allocation of JS registers, unboxed numbers
//! across instructions, inline property access against the object arena,
//! inlining, direct calls between compiled functions.
//!
//! The types fix the vocabulary of the specification; their shapes may
//! change when they are implemented, the contracts may not.

/// Where one piece of interpreter state lives at a safe point or side exit
/// of optimised code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Location {
    /// Already in the frame (register file slot or `Frame::acc`): the
    /// baseline tier's only location.
    Frame,
    /// In a machine register, by the code generator's register number.
    MachineReg(u16),
    /// In the compiled frame's spill area, at this byte offset from the
    /// frame's stack pointer.
    Spill(i32),
    /// A number the code holds unboxed (an `f64` in the location given by
    /// the inner index into `FrameState::raw`); boxing it allocates
    /// nothing.
    UnboxedNumber(u16),
    /// A constant the code generator proved (tag, payload words).
    Constant(u64, u64),
}

/// Everything needed to rebuild the interpreter frame at one side exit:
/// the bytecode pc to continue at and where the accumulator and each live
/// register are. Registers not listed are already in the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameState {
    /// The pc the interpreter continues at (an instruction boundary; the
    /// instruction at `pc` has not started, ADR 0019 J3).
    pub pc: u32,
    pub acc: Location,
    /// `(register, location)` for every register whose current value is
    /// not in the register file.
    pub regs: Vec<(u16, Location)>,
    /// Raw machine locations of unboxed numbers (see
    /// [`Location::UnboxedNumber`]).
    pub raw: Vec<Location>,
}

/// A point in compiled code where a collection may run (a call out to the
/// runtime) and the heap values live there outside the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SafePoint {
    /// Byte offset of the call's return address in the function's code.
    pub return_offset: u32,
    /// The bytecode pc of the instruction making the call.
    pub pc: u32,
    /// Locations holding (possibly) heap values that must be traced.
    pub live: Vec<Location>,
}

/// The per-function GC map: one entry per safe point, sorted by
/// `return_offset`.
pub trait GcMap {
    fn safe_points(&self) -> &[SafePoint];
    /// The safe point whose return address is `return_offset`.
    fn at(&self, return_offset: u32) -> Option<&SafePoint> {
        let sp = self.safe_points();
        sp.binary_search_by_key(&return_offset, |s| s.return_offset)
            .ok()
            .map(|i| &sp[i])
    }
}

/// Why optimised code left at a side exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeoptReason {
    /// A type or shape guard failed.
    Guard,
    /// A debugger attached or a breakpoint was set in this function
    /// (ADR 0019 J9).
    Debugger,
    /// The code's assumptions were invalidated from outside (a prototype
    /// epoch moved, a global was redefined).
    Invalidated,
}

/// The side-exit protocol: optimised code calls this with the exit's
/// [`FrameState`] and the machine state it captured; the implementation
/// writes the interpreter frame (boxing unboxed numbers, which allocates
/// nothing) and the caller continues in the interpreter at `state.pc`.
pub trait Deoptimizer<V> {
    /// Rebuild the top frame. `read` gives the 64-bit contents of a
    /// machine location as captured at the exit.
    fn materialize(
        &mut self,
        state: &FrameState,
        reason: DeoptReason,
        read: &dyn Fn(Location) -> u64,
    ) -> Result<(), &'static str>;
    /// Discard all compiled code of a block so the next entry recompiles
    /// or interprets it.
    fn invalidate(&mut self, block_id: usize);
}

#[cfg(test)]
mod tests {
    // The contracts the reserved work must meet, as tests to un-ignore when
    // it lands (docs/design/jit-deopt-and-gc-maps.md lists them in full).

    #[test]
    #[ignore = "deoptimization is reserved (TASK-101.3 hand-off)"]
    fn a_failed_guard_continues_in_the_interpreter_at_the_guarded_instruction() {
        unimplemented!("side exit at a shape guard: frame rebuilt, pc at the instruction")
    }

    #[test]
    #[ignore = "GC maps are reserved (TASK-101.3 hand-off)"]
    fn a_collection_under_optimised_frames_traces_values_in_machine_registers() {
        unimplemented!("a safe point's live locations are traced and survive the collection")
    }

    #[test]
    #[ignore = "GC maps are reserved (TASK-101.3 hand-off)"]
    fn a_nested_run_under_optimised_code_keeps_the_birth_serial_bound() {
        unimplemented!("values born before a native's nested run are retained (TASK-100.16)")
    }

    #[test]
    #[ignore = "deoptimization is reserved (TASK-101.3 hand-off)"]
    fn attaching_a_debugger_deoptimizes_running_frames() {
        unimplemented!("ADR 0019 J9: every frame of the function continues in the interpreter")
    }
}
