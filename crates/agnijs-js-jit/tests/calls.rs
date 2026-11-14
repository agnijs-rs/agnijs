//! Calls made in machine code and global sites (TASK-101.3.3), against a
//! toy runtime that describes a frame stack, function objects and a callee
//! table of its own, as the VM describes its real ones (`CallLayout`,
//! `HeapLayout`). The VM's tests cover the real runtime
//! (crates/agnijs-js-vm/tests/jit_calls.rs); what only a toy can do is
//! misbehave: answer a call out with a pc the VM never answers
//! (TASK-103.36), or panic in a fast path (TASK-103.35).

use std::cell::Cell;
use std::mem::{offset_of, size_of};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use agnijs_js_compiler::bytecode::Instr;
use agnijs_js_compiler::{compile_script, CodeBlock};
use agnijs_js_jit::{
    rc_word, vec_layout, vec_layout_of, Arith, CallLayout, CallView, Callee, CalleeTable, Compiled,
    Exit, GlobalSite, HeapLayout, HeapView, JitValue, Resume, Runtime, Speculation,
};

#[repr(C, u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)]
enum Val {
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

/// The class tag of a function object.
const CLASS_FUNCTION: u8 = 1;

/// An object of the toy heap: all the call thunk reads of one is its class
/// and, for a function, the word addressing its data.
#[repr(C)]
struct Obj {
    class: u8,
    data: Box<FnData>,
}

/// A function's data, with the fields a call copies into the frame.
#[repr(C)]
struct FnData {
    code: Rc<CodeBlock>,
    env: u64,
    module: u64,
    realm: u32,
    /// 0: `this` as passed (1 would be "the global object for none").
    this_mode: u8,
}

/// A frame of the toy's frame stack. Machine code pushes one by copying
/// the callee's image and setting the fields `CallLayout` names.
#[repr(C)]
struct Frame {
    code: Rc<CodeBlock>,
    pc: usize,
    base: usize,
    argc: usize,
    acc: Val,
    this: Val,
    env: u64,
    base_env: u64,
    module: u64,
    callee: u32,
    realm: u32,
}

impl Frame {
    fn of(code: &Rc<CodeBlock>) -> Frame {
        Frame {
            code: code.clone(),
            pc: 0,
            base: 0,
            argc: 0,
            acc: Val::Undefined,
            this: Val::Undefined,
            env: 0,
            base_env: 0,
            module: 0,
            callee: 0,
            realm: 0,
        }
    }
}

/// The toy's objects as compiled code finds them. Only the class is real:
/// no function compiled here has a property access, so the fields the
/// inline-cache paths read are never used, and every class is one those
/// paths refuse.
fn heap_layout() -> HeapLayout {
    HeapLayout {
        obj_stride: size_of::<Obj>() as u32,
        obj_offset: 0,
        shape: 0,
        dict: 0,
        class: offset_of!(Obj, class) as u32,
        proto: 0,
        inline: 0,
        inline_slots: 0,
        overflow_ptr: 0,
        overflow_len: 0,
        array_length: 0,
        elements_ptr: 0,
        elements_cap: 0,
        elements_len: 0,
        arguments_mapped: 0,
        class_array: 0xfe,
        class_string: 0xfd,
        class_arguments: 0xfc,
        not_receiver: u64::MAX,
        not_elements: u64::MAX,
    }
}

fn call_layout(globals: bool) -> CallLayout {
    let u = |n: usize| n as u32;
    assert_eq!(size_of::<Frame>() % 8, 0);
    assert_eq!(size_of::<Box<FnData>>(), 8);
    CallLayout {
        frames: vec_layout_of::<Frame>().expect("a vector is three words"),
        registers: vec_layout(Val::Undefined).expect("a vector is three words"),
        frame_size: u(size_of::<Frame>()),
        max_frames: 64,
        frame_code: u(offset_of!(Frame, code)),
        frame_pc: u(offset_of!(Frame, pc)),
        frame_base: u(offset_of!(Frame, base)),
        frame_acc: u(offset_of!(Frame, acc)),
        frame_this: u(offset_of!(Frame, this)),
        frame_argc: u(offset_of!(Frame, argc)),
        frame_callee: u(offset_of!(Frame, callee)),
        frame_env: u(offset_of!(Frame, env)),
        frame_base_env: u(offset_of!(Frame, base_env)),
        env_size: 8,
        frame_module: u(offset_of!(Frame, module)),
        module_size: 8,
        frame_realm: u(offset_of!(Frame, realm)),
        class_function: CLASS_FUNCTION,
        function_data: u(offset_of!(Obj, data)),
        fn_code: u(offset_of!(FnData, code)),
        fn_env: u(offset_of!(FnData, env)),
        fn_module: u(offset_of!(FnData, module)),
        fn_realm: u(offset_of!(FnData, realm)),
        fn_this_mode: u(offset_of!(FnData, this_mode)),
        this_mode_global: 1,
        direct: true,
        env: None,
        globals,
    }
}

/// A runtime with one frame stack and one register file, like the VM's.
/// Its generic path answers every instruction with a fixed value, except
/// where a test tells it to misbehave.
struct Rt {
    frames: Vec<Frame>,
    regs: Vec<Val>,
    realm: u32,
    objects: Vec<Obj>,
    table: CalleeTable,
    /// The epoch every block's inline caches are of, and the cell the
    /// callee table reads it from.
    epoch: Box<Cell<u64>>,
    stamp: u64,
    /// Stands for an environment arena: no function here reads one.
    envs: u64,
    counter: u32,
    /// `(frame depth, pc)` of every instruction run generically.
    slow_calls: Vec<(usize, u32)>,
    /// Calls that came through `Runtime::call` and `Runtime::returned`,
    /// and the calls machine code made itself.
    runtime_calls: u32,
    returns: u32,
    direct: u64,
    global_calls: u32,
    panic_in_global: bool,
    /// The pc the generic path says a function called directly (a frame
    /// above the first) continues at, whatever the instruction.
    callee_continues_at: Option<u32>,
}

impl Rt {
    /// A runtime about to run `code` in its first frame.
    fn new(code: &Rc<CodeBlock>) -> Rt {
        let mut frames = Vec::with_capacity(8);
        frames.push(Frame::of(code));
        let mut regs = Vec::with_capacity(256);
        regs.resize(code.register_count as usize, Val::Undefined);
        Rt {
            frames,
            regs,
            realm: 0,
            objects: Vec::new(),
            table: CalleeTable::default(),
            epoch: Box::new(Cell::new(0)),
            stamp: 0,
            envs: 0,
            counter: 0,
            slow_calls: Vec::new(),
            runtime_calls: 0,
            returns: 0,
            direct: 0,
            global_calls: 0,
            panic_in_global: false,
            callee_continues_at: None,
        }
    }

    /// A function object for `code`, which machine code may call directly
    /// from now on: its compiled code goes into the callee table with the
    /// image of its frame. Returns the object.
    fn add_function(&mut self, code: &Rc<CodeBlock>, compiled: &Rc<Compiled<Val>>) -> Val {
        assert!(
            compiled.direct(),
            "the callee keeps nothing in machine state"
        );
        assert_eq!(code.ic_count, 0, "the callee has no inline-cache site");
        self.table.base();
        let word = rc_word(code).expect("an Rc is the address of its count");
        let old = self.table.fill(Callee {
            compiled,
            code: word,
            template: Box::new(Frame::of(code)),
            ic: &[],
            shape_epoch: &self.epoch,
            registers: u32::from(code.register_count),
            param_base: u32::from(code.param_base),
            param_regs: u32::from(code.param_regs),
            realm: 0,
        });
        assert!(old.is_none());
        assert!(self.table.has(word, 0), "the table took the callee");
        self.objects.push(Obj {
            class: CLASS_FUNCTION,
            data: Box::new(FnData {
                code: code.clone(),
                env: 0,
                module: 0,
                realm: 0,
                this_mode: 0,
            }),
        });
        Val::Obj((self.objects.len() - 1) as u32)
    }
}

impl Runtime<Val> for Rt {
    fn slow(&mut self, pc: u32, acc: Val) -> Resume<Val> {
        let depth = self.frames.len() - 1;
        self.slow_calls.push((depth, pc));
        let top = self.frames.last_mut().expect("a running frame");
        top.acc = acc;
        if let (true, Some(to)) = (depth > 0, self.callee_continues_at) {
            top.pc = to as usize;
            return Resume::Next {
                pc: to,
                acc: Val::Str(7),
            };
        }
        top.pc = pc as usize + 1;
        Resume::Next {
            pc: pc + 1,
            acc: Val::Str(99),
        }
    }

    /// No test here expects a call through the runtime: it is counted and
    /// the run left.
    fn call(&mut self, pc: u32, acc: Val) -> Resume<Val> {
        self.runtime_calls += 1;
        let top = self.frames.last_mut().expect("a running frame");
        top.pc = pc as usize + 1;
        top.acc = acc;
        Resume::Exit(Exit::Interp)
    }

    fn returned(&mut self, pc: u32, acc: Val, popped: bool) -> Resume<Val> {
        self.returns += 1;
        if !popped {
            let callee = self.frames.pop().expect("the callee's frame");
            self.regs.truncate(callee.base);
            self.realm = self.frames.last().expect("the caller's frame").realm;
        }
        self.frames.last_mut().expect("the caller's frame").acc = acc;
        Resume::Next { pc: pc + 1, acc }
    }

    fn call_view(&mut self) -> CallView {
        CallView {
            frames: &mut self.frames as *mut Vec<Frame> as *mut u8,
            registers: &mut self.regs as *mut Vec<Val> as *mut u8,
            realm: &mut self.realm as *mut u32 as *mut u8,
            callees: self.table.base(),
        }
    }

    fn shape_epoch(&self) -> u64 {
        self.epoch.get()
    }

    fn stack_floor(&self) -> usize {
        // Any depth the test's two frames reach is above it.
        1
    }

    fn direct_calls(&mut self, n: u64) {
        self.direct += n;
    }

    fn global(&mut self, _pc: u32, _acc: Val, _site: Option<&GlobalSite>) -> Option<Val> {
        self.global_calls += 1;
        if self.panic_in_global {
            panic!("a bug in the global path");
        }
        None
    }

    fn global_stamp(&mut self) -> *const u64 {
        &self.stamp
    }

    fn this_value(&mut self) -> Option<Val> {
        self.frames.last().map(|f| f.this)
    }

    fn poll(&mut self, _pc: u32, target: u32, acc: Val) -> Resume<Val> {
        Resume::Next { pc: target, acc }
    }

    fn registers(&mut self) -> &mut [Val] {
        let base = self.frames.last().map_or(0, |f| f.base);
        &mut self.regs[base..]
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

    fn heap(&mut self) -> HeapView {
        HeapView {
            objects: self.objects.as_mut_ptr() as *mut u8,
            proto_epoch: 0,
            envs: &mut self.envs as *mut u64 as *mut u8,
        }
    }
}

fn compile(code: &CodeBlock, globals: bool) -> Rc<Compiled<Val>> {
    let c = Compiled::<Val>::compile_calling(
        code,
        Some(&heap_layout()),
        Some(&call_layout(globals)),
        &Speculation::NONE,
    )
    .expect("compiles to machine code");
    Rc::new(c)
}

/// The first function of `src` about to call the second, which is in the
/// callee table: the runtime, the caller's compiled code and blocks of
/// both, and the pc of the caller's `Call`. The callee's function object
/// is the caller's first argument.
struct Pair {
    rt: Rt,
    caller: Rc<Compiled<Val>>,
    callee_code: Rc<CodeBlock>,
    call_pc: u32,
}

fn pair(src: &str) -> Pair {
    let script = compile_script(src, "test.js").expect("compiles");
    let (f, g) = (script.functions[0].clone(), script.functions[1].clone());
    let caller = compile(&f, false);
    let callee = compile(&g, false);
    let mut rt = Rt::new(&f);
    let function = rt.add_function(&g, &callee);
    rt.regs[f.param_base as usize] = function;
    let call_pc = f
        .instrs
        .iter()
        .position(|i| matches!(i, Instr::Call { .. }))
        .expect("the caller has a call") as u32;
    Pair {
        rt,
        caller,
        callee_code: g,
        call_pc,
    }
}

#[test]
fn machine_code_calls_a_function_in_the_callee_table_itself() {
    let mut p = pair("function f(g) { return g(41); } function g(a) { return a + 1; }");
    let held = Rc::strong_count(&p.callee_code);
    let out = p.caller.enter(&mut p.rt, 0, Val::Undefined);
    assert_eq!(out.exit, Exit::Return);
    assert_eq!(
        Rc::strong_count(&p.callee_code),
        held,
        "the frame's reference to the callee's block was given back"
    );
    assert_eq!(out.acc, Val::Number(42.0));
    assert_eq!(p.rt.direct, 1, "the call was made in machine code");
    assert_eq!(p.rt.runtime_calls, 0);
    assert!(p.rt.slow_calls.is_empty(), "{:?}", p.rt.slow_calls);
    assert_eq!(p.rt.frames.len(), 1, "the callee's frame was popped");
    assert_eq!(
        p.rt.frames[0].pc,
        p.call_pc as usize + 1,
        "the caller was written back for the call"
    );
}

// TASK-103.36: machine code answers its caller with one word, an exit code
// or a pc "the runtime continued this frame at, which compiled code has no
// block for". A function called directly that left with such a pc handed
// it, through the call thunk, to the caller's call site, which compared it
// with its own successor: a callee leaving at the caller's `pc + 1` made
// the caller go on as if the call had returned, the callee's frame still
// on the stack.
#[test]
fn a_callee_that_leaves_at_a_pc_of_its_own_makes_the_caller_leave_for_the_interpreter() {
    let mut p =
        pair("function f(g) { var r = g(1); return r; } function g(a) { return someGlobal; }");
    let load = p
        .callee_code
        .instrs
        .iter()
        .position(|i| matches!(i, Instr::LdaGlobal(_)))
        .expect("the callee loads a global") as u32;
    // The worst pc the callee could leave at: the caller's continuation.
    let leftover = p.call_pc + 1;
    assert_ne!(
        leftover,
        load + 1,
        "not a successor of the callee's own instruction"
    );
    p.rt.callee_continues_at = Some(leftover);
    let out = p.caller.enter(&mut p.rt, 0, Val::Undefined);
    assert_eq!(
        out.exit,
        Exit::Interp,
        "the caller took the callee's leftover pc for its own continuation"
    );
    // Both frames are as the runtime left them, for the interpreter to go
    // on from: the callee's on top, at the pc the runtime gave it; the
    // caller's under it, written back at the instruction after the call.
    assert_eq!(p.rt.frames.len(), 2);
    assert_eq!(p.rt.frames[1].pc, leftover as usize);
    assert!(Rc::ptr_eq(&p.rt.frames[1].code, &p.callee_code));
    assert_eq!(
        p.rt.frames[1].base,
        p.rt.frames[0].code.register_count as usize
    );
    assert_eq!(p.rt.frames[0].pc, p.call_pc as usize + 1);
    assert_eq!(
        p.rt.regs.len(),
        p.rt.frames[1].base + p.callee_code.register_count as usize,
        "the callee's registers are still there"
    );
    // Nothing of the caller ran after the call.
    assert_eq!(p.rt.slow_calls, vec![(1, load)]);
    assert_eq!(p.rt.direct, 1);
    assert_eq!((p.rt.runtime_calls, p.rt.returns), (0, 0));
}

/// A run of a function that loads a global through a global site
/// (`CallLayout::globals`), the site not filled: how it ended, or the
/// panic's message.
fn load_global(panic_in_global: bool) -> (Rt, Result<Exit, String>) {
    let script =
        compile_script("function f() { return someGlobal; }", "test.js").expect("compiles");
    let f = script.functions[0].clone();
    let compiled = compile(&f, true);
    let mut rt = Rt::new(&f);
    rt.panic_in_global = panic_in_global;
    let r = catch_unwind(AssertUnwindSafe(|| {
        compiled.enter(&mut rt, 0, Val::Undefined)
    }));
    let r = match r {
        Ok(out) => Ok(out.exit),
        Err(payload) => Err(payload
            .downcast_ref::<&str>()
            .copied()
            .unwrap_or("?")
            .to_string()),
    };
    (rt, r)
}

#[test]
fn a_global_site_that_does_not_serve_asks_the_runtime() {
    let (rt, r) = load_global(false);
    assert_eq!(r, Ok(Exit::Return));
    assert_eq!(rt.global_calls, 1);
    assert_eq!(
        rt.slow_calls.len(),
        1,
        "a miss runs the instruction generically"
    );
}

// TASK-103.35, the global site's trampoline.
#[test]
fn a_panic_in_global_leaves_enter_as_a_panic() {
    let (rt, r) = load_global(true);
    assert_eq!(
        r,
        Err("a bug in the global path".to_string()),
        "the panic in Runtime::global was swallowed"
    );
    assert_eq!(rt.global_calls, 1);
    assert!(
        rt.slow_calls.is_empty(),
        "the instruction was run again after the panic: {:?}",
        rt.slow_calls
    );
}
