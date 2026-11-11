//! Deoptimization and GC maps (TASK-101.3.1; specified in
//! docs/design/jit-deopt-and-gc-maps.md). The baseline tier needed neither:
//! it never speculated and never held a value outside the frame across a
//! call out. Both became necessary together when compiled code started
//! keeping JavaScript registers in machine state across safe points and
//! holding numbers unboxed on a guess ([`crate::Speculation`]).
//!
//! The vocabulary here is the code generator's description of its own
//! frames, read by the runtime through a [`crate::FrameHandle`]:
//!
//! - A [`Point`] is a place in compiled code where the interpreter's view
//!   of the frame is incomplete and Rust may look at it: a *safe point* (a
//!   call out that may allocate, collect, or run JavaScript), a *side exit*
//!   (a guard that failed, leaving for the interpreter before the guarded
//!   instruction runs) or an *entry* guard (a speculated register found not
//!   to hold a number when compiled code is entered).
//! - Each point lists where every register whose current value is not in
//!   the register file lives ([`Location`]): a spill slot of the compiled
//!   frame, boxed or as an unboxed number. Registers not listed are current
//!   in the file. The code generator fills the spill slots before the call
//!   or the exit and stores the point's index and the spill area's address
//!   in the context, so the runtime can read them while machine code waits.
//! - A [`FrameState`] is what the runtime gets to write into the
//!   interpreter frame: the listed registers' values, and for a side exit
//!   the pc to continue at and the accumulator ([`SideExit`]).
//!
//! A collection under compiled frames traces every frame's spilled heap
//! values through its handle (the collector never scans the native stack,
//! ADR 0019 J6); numbers held unboxed are never heap values, so they are
//! listed for deoptimization only.

/// Where a register's current value lives at a [`Point`], when not in the
/// register file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// A boxed value (the 16 bytes of [`crate::layout`]) in the compiled
    /// frame's spill area, at this byte offset.
    Spill(i32),
    /// An unboxed number (`f64` bits) in the spill area, at this offset.
    /// Boxing it allocates nothing, so materialising a frame never reaches
    /// a safe point with the state half written.
    SpillNumber(i32),
}

/// What kind of point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointKind {
    /// A call out of compiled code that may allocate, collect or run
    /// JavaScript. `next` lists the pcs the runtime may continue compiled
    /// code at; any other pc it reports leaves for the interpreter, with
    /// the frame materialised first.
    SafePoint { next: Vec<u32> },
    /// A guard failed: the instruction at the point's pc has not started
    /// (ADR 0019 J3). `guard` names the register whose speculation failed.
    Exit { guard: Option<u16> },
    /// Compiled code was entered at the point's pc and register `guard`
    /// did not hold the number it was compiled to hold. Nothing is dirty:
    /// the frame is as the interpreter left it.
    Entry { guard: u16 },
}

/// One safe point, side exit or entry guard of a compiled function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// The bytecode pc: of the instruction making the call, or the
    /// instruction to continue at.
    pub pc: u32,
    pub kind: PointKind,
    /// `(register, location)` for every register whose current value is
    /// not in the register file; the rest are current there.
    pub regs: Vec<(u16, Location)>,
}

impl Point {
    /// Whether register `r` is listed (held outside the register file).
    pub fn holds(&self, r: u16) -> bool {
        self.regs.iter().any(|&(x, _)| x == r)
    }
}

/// The per-function table of points: the GC map and the side-exit states
/// in one, indexed by the point id compiled code stores in the context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Points {
    pub points: Vec<Point>,
}

impl Points {
    pub fn at(&self, id: u32) -> Option<&Point> {
        self.points.get(id as usize)
    }
}

/// Why compiled code left at a side exit.
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

/// A side exit's continuation: the interpreter resumes at `pc`, which has
/// not started, with the accumulator `acc`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SideExit<V> {
    pub pc: u32,
    pub acc: V,
    pub reason: DeoptReason,
    /// The register whose speculation failed, when that is the reason.
    pub guard: Option<u16>,
}

/// Everything the runtime writes into the interpreter frame when compiled
/// code is left, or when it must be looked at while blocked in a call out
/// (a debugger attaching): the registers whose values were outside the
/// register file, and for a side exit where to continue.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameState<V> {
    /// The [`Point`] the frame is at.
    pub point: u32,
    /// `(register, value)`; every register not listed is current in the
    /// register file already.
    pub regs: Vec<(u16, V)>,
    /// `None` at a safe point: the runtime's own frame state (pc, acc) is
    /// current, since it wrote it before running the instruction.
    pub exit: Option<SideExit<V>>,
}

#[cfg(test)]
mod tests {
    //! The contracts of the hand-off (docs/design/jit-deopt-and-gc-maps.md,
    //! section 7), met against a mock runtime: a register file, one frame
    //! (pc, acc), a heap of numbered objects with a birth serial and the
    //! retention bound of TASK-100.16, and the list of compiled frames the
    //! collector traces through their handles.

    use std::cell::RefCell;
    use std::rc::Rc;

    use agnijs_js_compiler::bytecode::Instr;
    use agnijs_js_compiler::{compile_script, CodeBlock};

    use super::*;
    use crate::{Arith, Compiled, Exit, FrameHandle, JitValue, Resume, Runtime, Speculation};

    #[repr(C, u8)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    #[allow(dead_code)]
    enum Val {
        #[default]
        Undefined = 0,
        Null = 1,
        Bool(bool) = 2,
        Number(f64) = 3,
        Str(u32) = 4,
        Sym(u32) = 5,
        Big(u32) = 6,
        Obj(u32) = 7,
        Empty = 8,
    }

    impl JitValue for Val {
        fn undefined() -> Self {
            Val::Undefined
        }
        fn null() -> Self {
            Val::Null
        }
        fn boolean(b: bool) -> Self {
            Val::Bool(b)
        }
        fn number(n: f64) -> Self {
            Val::Number(n)
        }
        fn empty() -> Self {
            Val::Empty
        }
    }

    /// Numbered objects with a birth serial; a collection frees every
    /// object not reached from the roots, except those born before the
    /// retention bound of a nested run (TASK-100.16).
    #[derive(Default)]
    struct Heap {
        live: Vec<bool>,
        born: Vec<u64>,
        serial: u64,
        retain_from: u64,
        collections: u32,
    }

    impl Heap {
        fn alloc(&mut self) -> Val {
            self.serial += 1;
            self.live.push(true);
            self.born.push(self.serial);
            Val::Obj((self.live.len() - 1) as u32)
        }
    }

    /// The interpreter frame the compiled code shadows.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct Frame {
        pc: u32,
        acc: Val,
    }

    type Native = Box<dyn FnMut(&mut Mock, Val) -> Val>;

    struct Mock {
        regs: Vec<Val>,
        /// Register 0 of the running frame's window in `regs`.
        base: usize,
        frame: Frame,
        heap: Heap,
        /// Compiled frames blocked in call outs, innermost last, with the
        /// base of each one's window.
        frames: Vec<FrameHandle<Val>>,
        bases: Vec<usize>,
        /// Every call out collects (GC stress).
        stress: bool,
        counter: u32,
        /// What `Call` does: the native, given the first argument.
        native: Option<Native>,
        /// Materialised frame states, in order, with the frame's depth.
        materialized: Vec<(usize, FrameState<Val>)>,
        /// Attach a debugger at this poll (counting from 1): every compiled
        /// frame is materialised and left for the interpreter.
        attach_at_poll: u32,
        polls: u32,
        debugger: bool,
        slow_pcs: Vec<u32>,
        /// The next call out faults (the runtime keeps the fault).
        fault: bool,
    }

    impl Mock {
        fn new(code: &CodeBlock) -> Mock {
            Mock {
                regs: vec![Val::Undefined; code.register_count as usize],
                base: 0,
                frame: Frame::default(),
                heap: Heap::default(),
                frames: Vec::new(),
                bases: Vec::new(),
                stress: false,
                counter: 0,
                native: None,
                materialized: Vec::new(),
                attach_at_poll: 0,
                polls: 0,
                debugger: false,
                slow_pcs: Vec::new(),
                fault: false,
            }
        }

        /// The roots: the register file, the frame's accumulator, every
        /// compiled frame's spilled heap values, and what was born before
        /// the retention bound.
        fn collect(&mut self) {
            let mut marked = vec![false; self.heap.live.len()];
            let mut mark = |v: Val| {
                if let Val::Obj(o) = v {
                    marked[o as usize] = true;
                }
            };
            for &v in &self.regs {
                mark(v);
            }
            mark(self.frame.acc);
            for h in &self.frames {
                h.live(&mut mark);
            }
            for (i, &born) in self.heap.born.iter().enumerate() {
                if self.heap.retain_from != 0 && born < self.heap.retain_from {
                    marked[i] = true;
                }
            }
            for (i, m) in marked.into_iter().enumerate() {
                if !m {
                    self.heap.live[i] = false;
                }
            }
            self.heap.collections += 1;
        }

        fn materialize_all(&mut self) {
            let states: Vec<FrameState<Val>> = self.frames.iter().map(|h| h.state()).collect();
            for (depth, st) in states.into_iter().enumerate() {
                self.write_frame(depth, st);
            }
        }

        fn write_frame(&mut self, depth: usize, st: FrameState<Val>) {
            let base = self.bases[depth];
            for &(r, v) in &st.regs {
                self.regs[base + r as usize] = v;
            }
            if let Some(e) = st.exit {
                self.frame.pc = e.pc;
                self.frame.acc = e.acc;
            }
            self.materialized.push((depth, st));
        }

        /// Run `code` (compiled as `compiled`) as a nested run from a
        /// native: a window above the caller's, the caller's frame saved,
        /// and the retention bound of TASK-100.16 over objects born before.
        fn nested(&mut self, code: &CodeBlock, compiled: &Compiled<Val>, args: &[Val]) -> Val {
            let outer_base = self.base;
            let outer_frame = self.frame;
            let outer_bound = self.heap.retain_from;
            self.base = self.regs.len();
            self.regs
                .resize(self.base + code.register_count as usize, Val::Undefined);
            for (i, &a) in args.iter().enumerate() {
                self.regs[self.base + code.param_base as usize + i] = a;
            }
            self.frame = Frame::default();
            self.heap.retain_from = self.heap.serial + 1;
            let out = compiled.enter(self, 0, Val::Undefined);
            self.heap.retain_from = outer_bound;
            self.regs.truncate(self.base);
            self.base = outer_base;
            self.frame = outer_frame;
            match out.exit {
                Exit::Return => out.acc,
                other => panic!("the nested run ended with {other:?}"),
            }
        }

        /// What every call out answers once a debugger is attached (the
        /// interpreter takes over at the frame's pc) or a fault is pending.
        fn attached(&mut self) -> Option<Resume<Val>> {
            if self.fault {
                return Some(Resume::Exit(Exit::Fault));
            }
            self.debugger.then_some(Resume::Exit(Exit::Interp))
        }
    }

    impl Runtime<Val> for Mock {
        fn slow(&mut self, pc: u32, acc: Val) -> Resume<Val> {
            self.slow_pcs.push(pc);
            self.frame.pc = pc + 1;
            self.frame.acc = acc;
            if self.stress {
                self.collect();
            }
            if let Some(exit) = self.attached() {
                return exit;
            }
            Resume::Next {
                pc: pc + 1,
                acc: Val::Str(99),
            }
        }

        fn call(&mut self, pc: u32, acc: Val) -> Resume<Val> {
            self.frame.pc = pc + 1;
            self.frame.acc = acc;
            let Some(mut native) = self.native.take() else {
                return self.slow(pc, acc);
            };
            let arg = Val::Undefined;
            let v = native(self, arg);
            self.native = Some(native);
            // The result is in the frame before the safe point, as in the
            // VM.
            self.frame.acc = v;
            if self.stress {
                self.collect();
            }
            if let Some(exit) = self.attached() {
                return exit;
            }
            Resume::Next { pc: pc + 1, acc: v }
        }

        fn poll(&mut self, pc: u32, target: u32, acc: Val) -> Resume<Val> {
            self.polls += 1;
            self.frame.pc = pc + 1;
            self.frame.acc = acc;
            if self.stress {
                self.collect();
            }
            if self.polls == self.attach_at_poll {
                self.debugger = true;
                self.materialize_all();
            }
            self.frame.pc = target;
            if let Some(exit) = self.attached() {
                return exit;
            }
            Resume::Next { pc: target, acc }
        }

        fn safe_point_due(&self) -> bool {
            self.stress
        }

        fn registers(&mut self) -> &mut [Val] {
            &mut self.regs[self.base..]
        }

        fn interrupt_counter(&self) -> u32 {
            self.counter
        }

        fn set_interrupt_counter(&mut self, counter: u32) {
            self.counter = counter;
        }

        fn arith(op: Arith, a: f64, b: f64) -> f64 {
            match op {
                Arith::Mod => a % b,
                Arith::Exp => a.powf(b),
            }
        }

        fn frame_entered(&mut self, frame: FrameHandle<Val>) {
            self.frames.push(frame);
            self.bases.push(self.base);
        }

        fn frame_left(&mut self) {
            self.frames.pop();
            self.bases.pop();
        }

        fn materialize(&mut self, state: FrameState<Val>) {
            let depth = self.frames.len() - 1;
            self.write_frame(depth, state);
        }
    }

    /// The first function declared in `src`.
    fn function(src: &str) -> Rc<CodeBlock> {
        compile_script(src, "test.js").expect("compiles").functions[0].clone()
    }

    /// The register of the first local initialised with an integer
    /// (`var s = 0`): the `Star` after the first `LdaInt`. The parameters'
    /// registers come first and are copied in before it.
    fn first_initialised(code: &CodeBlock) -> u16 {
        code.instrs
            .windows(2)
            .find_map(|w| match (w[0], w[1]) {
                (Instr::LdaInt(_), Instr::Star(r)) => Some(r),
                _ => None,
            })
            .expect("a local initialised with an integer")
    }

    fn compile(code: &CodeBlock, numbers: u64) -> Compiled<Val> {
        Compiled::<Val>::compile_speculating(code, None, &Speculation { numbers }).expect("compiles")
    }

    #[test]
    fn a_failed_guard_continues_in_the_interpreter_at_the_guarded_instruction() {
        // `s` is speculated to hold a number: the loop keeps it unboxed in
        // machine state, and the store of `b` into it is the guard that
        // fails. The exit is before that store, with the frame rebuilt.
        let code = function("function f(a, b) { var s = 0; for (var i = 0; i < 3; i++) { s = s + a; } s = b; return s; }");
        let s = first_initialised(&code);
        let compiled = compile(&code, 1 << s);
        let mut rt = Mock::new(&code);
        let pb = code.param_base as usize;
        rt.regs[pb] = Val::Number(5.0);
        rt.regs[pb + 1] = Val::Str(7);
        let out = compiled.enter(&mut rt, 0, Val::Undefined);
        assert_eq!(out.exit, Exit::Interp);
        assert!(rt.slow_pcs.is_empty(), "nothing called out: {:?}", rt.slow_pcs);
        // At the guarded instruction, which has not run: `s` still holds
        // the loop's sum, and the accumulator the value to store.
        let pc = rt.frame.pc as usize;
        assert!(
            matches!(code.instrs[pc], Instr::Star(r) | Instr::Mov { dst: r, .. } if r == s),
            "pc {pc}: {:?}",
            code.instrs[pc]
        );
        assert_eq!(out.pc, pc as u32);
        assert_eq!(rt.frame.acc, Val::Str(7));
        assert_eq!(rt.regs[s as usize], Val::Number(15.0));
        let (_, st) = rt.materialized.last().expect("materialised once");
        let exit = st.exit.expect("a side exit");
        assert_eq!(exit.reason, DeoptReason::Guard);
        assert_eq!(exit.guard, Some(s));
        assert!(st.regs.iter().any(|&(r, v)| r == s && v == Val::Number(15.0)), "{st:?}");
        // The points describe the exit: `s` was outside the register file.
        let point = compiled.points().at(st.point).unwrap();
        assert_eq!(point.kind, PointKind::Exit { guard: Some(s) });
        assert_eq!(point.pc, pc as u32);
        assert!(point.holds(s));
    }

    /// What a native saw of the compiled frame below it at each call: the
    /// registers the frame's point listed, and the register file.
    type Seen = Rc<RefCell<Vec<(Vec<(u16, Val)>, Vec<Val>)>>>;

    /// A native that allocates an object (and one more, unreachable) and
    /// returns it, recording what the compiled frame below holds.
    fn allocating_native(seen: Seen) -> Native {
        Box::new(move |m, _| {
            let v = m.heap.alloc();
            m.heap.alloc();
            let st = m.frames.last().expect("a compiled frame below the native").state();
            seen.borrow_mut().push((st.regs, m.regs.clone()));
            v
        })
    }

    #[test]
    fn a_collection_under_optimised_frames_traces_values_in_machine_registers() {
        // `t` is written in the loop, so compiled code keeps it in machine
        // state; the later calls of `g` do not read it, so it is in the
        // frame's spill area, not the register file, while they run, and
        // every one collects.
        let code = function(
            "function f(g, n) { var t = null; for (var i = 0; i < n; i++) { if (i == 0) { t = g(); } else { g(); } } return t; }",
        );
        let compiled = compile(&code, 0);
        let mut rt = Mock::new(&code);
        rt.stress = true;
        let seen: Seen = Rc::default();
        rt.native = Some(allocating_native(seen.clone()));
        let pb = code.param_base as usize;
        rt.regs[pb] = Val::Str(1);
        rt.regs[pb + 1] = Val::Number(4.0);
        let out = compiled.enter(&mut rt, 0, Val::Undefined);
        assert_eq!(out.exit, Exit::Return);
        let Val::Obj(first) = out.acc else {
            panic!("returned {:?}", out.acc)
        };
        assert!(rt.heap.live[first as usize], "the object held only by compiled code survived");
        assert!(!rt.heap.live[first as usize + 1], "the unreachable one did not");
        assert!(rt.heap.collections >= 4, "{} collections", rt.heap.collections);
        let seen = seen.borrow();
        assert_eq!(seen.len(), 4);
        for (regs, file) in &seen[1..] {
            assert!(
                regs.iter().any(|&(_, v)| v == Val::Obj(first)),
                "the point lists the object: {regs:?}"
            );
            assert!(!file.contains(&Val::Obj(first)), "the register file does not hold it: {file:?}");
        }
        let (regs, _) = &seen[0];
        assert!(!regs.iter().any(|&(_, v)| v == Val::Obj(first)), "not before it was stored");
    }

    #[test]
    fn a_nested_run_under_optimised_code_keeps_the_birth_serial_bound() {
        // The native runs compiled code of its own (a nested run) under
        // the outer compiled frame, which holds an object only in its
        // spill area. Collections in the nested run retain everything
        // born before it (TASK-100.16); the one after it, when the bound
        // is lifted, must find the object through the outer frame's map.
        let outer = function(
            "function f(g, n) { var t = null; for (var i = 0; i < n; i++) { if (i == 0) { t = g(); } else { g(); } } return t; }",
        );
        let inner = function("function h(n) { var k = 0; for (var i = 0; i < n; i++) { k = k + i; p(); } return k; }");
        let outer_c = Rc::new(compile(&outer, 0));
        let inner_c = Rc::new(compile(&inner, 0));
        let mut rt = Mock::new(&outer);
        rt.stress = true;
        let depths: Rc<RefCell<Vec<usize>>> = Rc::default();
        let freed_in_nested: Rc<RefCell<Vec<bool>>> = Rc::default();
        let d = depths.clone();
        let fr = freed_in_nested.clone();
        let (ic, inr) = (inner_c.clone(), inner.clone());
        rt.native = Some(Box::new(move |m, _| {
            let v = m.heap.alloc();
            let garbage = m.heap.alloc();
            let Val::Obj(g) = garbage else { unreachable!() };
            // The nested run: `p()` has no native (it is taken), so it is
            // the generic path, which collects under stress.
            let k = m.nested(&inr, &ic, &[Val::Number(3.0)]);
            assert_eq!(k, Val::Number(3.0));
            d.borrow_mut().push(m.frames.len());
            fr.borrow_mut().push(!m.heap.live[g as usize]);
            v
        }));
        let pb = outer.param_base as usize;
        rt.regs[pb] = Val::Str(1);
        rt.regs[pb + 1] = Val::Number(3.0);
        let out = outer_c.enter(&mut rt, 0, Val::Undefined);
        assert_eq!(out.exit, Exit::Return);
        let Val::Obj(first) = out.acc else {
            panic!("returned {:?}", out.acc)
        };
        assert!(rt.heap.live[first as usize], "the object survived the collections after the nested runs");
        assert_eq!(*depths.borrow(), vec![1, 1, 1], "the nested frames were popped");
        assert_eq!(
            *freed_in_nested.borrow(),
            vec![false, false, false],
            "born before the nested run: retained by the bound, not freed"
        );
        assert!(!rt.heap.live[first as usize + 1], "freed once the bound was lifted");
        assert_eq!(rt.frames.len(), 0);
    }

    #[test]
    fn attaching_a_debugger_deoptimizes_running_frames() {
        // Two compiled frames, the inner one run by a native under the
        // outer: a debugger attaching at the inner loop's second poll
        // finds both frames complete in the interpreter's view (ADR 0019
        // J9), and both runs leave for the interpreter.
        let outer = function("function f(g, n) { var t = 10; for (var i = 0; i < n; i++) { t = t + 1; g(); } return t; }");
        let inner = function("function h(n) { var k = 100; for (var i = 0; i < n; i++) { k = k + 1; } return k; }");
        let outer_c = Rc::new(compile(&outer, 0));
        let inner_c = Rc::new(compile(&inner, 0));
        let mut rt = Mock::new(&outer);
        // Every back-edge polls (as under GC stress); the second poll of
        // the run is the inner loop's second.
        rt.stress = true;
        rt.attach_at_poll = 2;
        let (ic, inr) = (inner_c.clone(), inner.clone());
        rt.native = Some(Box::new(move |m, _| {
            let outer_base = m.base;
            let outer_frame = m.frame;
            m.base = m.regs.len();
            m.regs.resize(m.base + inr.register_count as usize, Val::Undefined);
            m.regs[m.base + inr.param_base as usize] = Val::Number(5.0);
            m.frame = Frame::default();
            let out = ic.enter(m, 0, Val::Undefined);
            assert_eq!(out.exit, Exit::Interp, "the inner run left for the interpreter");
            // Its frame is complete: `k` after two iterations, `i` at 2.
            let k = first_initialised(&inr) as usize;
            assert_eq!(m.regs[m.base + k], Val::Number(102.0));
            assert_eq!(m.regs[m.base + k + 1], Val::Number(2.0));
            m.regs.truncate(m.base);
            m.base = outer_base;
            m.frame = outer_frame;
            Val::Undefined
        }));
        let pb = outer.param_base as usize;
        rt.regs[pb] = Val::Str(1);
        rt.regs[pb + 1] = Val::Number(3.0);
        let out = outer_c.enter(&mut rt, 0, Val::Undefined);
        assert_eq!(out.exit, Exit::Interp);
        assert!(rt.debugger);
        // The outer frame too, at its call: `t` after one increment.
        let t = first_initialised(&outer) as usize;
        assert_eq!(rt.regs[t], Val::Number(11.0));
        assert_eq!(rt.regs[t + 1], Val::Number(0.0));
        let depths: Vec<usize> = rt.materialized.iter().map(|(d, _)| *d).collect();
        assert_eq!(depths[..2], [0, 1], "both frames, outermost first, at the attach: {depths:?}");
        assert!(rt.materialized.iter().all(|(_, st)| st.exit.is_none()), "no side exit: safe points");
        assert_eq!(rt.frames.len(), 0);
    }

    // Faults: a call out that leaves compiled code any other way, or panics
    // under it, with registers held in machine state.

    #[test]
    fn a_fault_in_a_call_out_leaves_the_frame_complete() {
        let code = function("function f(g, n) { var t = 10; for (var i = 0; i < n; i++) { t = t + 1; g(); } return t; }");
        let compiled = compile(&code, 0);
        let mut rt = Mock::new(&code);
        rt.native = Some(Box::new(|m, _| {
            m.fault = true;
            Val::Undefined
        }));
        let pb = code.param_base as usize;
        rt.regs[pb] = Val::Str(1);
        rt.regs[pb + 1] = Val::Number(3.0);
        let out = compiled.enter(&mut rt, 0, Val::Undefined);
        assert_eq!(out.exit, Exit::Fault);
        // The frame the fault handler finds has the loop's state.
        let t = first_initialised(&code) as usize;
        assert_eq!(rt.regs[t], Val::Number(11.0));
        assert_eq!(rt.regs[t + 1], Val::Number(0.0));
        assert_eq!(rt.frames.len(), 0);
    }

    #[test]
    fn a_panic_in_a_call_out_unwinds_through_compiled_code_and_drops_the_frame() {
        let code = function("function f(g, n) { var t = 10; for (var i = 0; i < n; i++) { t = t + 1; g(); } return t; }");
        let compiled = compile(&code, 0);
        let mut rt = Mock::new(&code);
        rt.native = Some(Box::new(|_, _| panic!("a bug in the native")));
        let pb = code.param_base as usize;
        rt.regs[pb] = Val::Str(1);
        rt.regs[pb + 1] = Val::Number(3.0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compiled.enter(&mut rt, 0, Val::Undefined)
        }));
        let Err(payload) = r else {
            panic!("the panic did not reach the caller of `enter`")
        };
        assert_eq!(payload.downcast_ref::<&str>().copied(), Some("a bug in the native"));
        // The machine frame is gone with its handle: nothing dangles on
        // the runtime's list.
        assert_eq!(rt.frames.len(), 0);
        assert_eq!(rt.bases.len(), 0);
    }
}
