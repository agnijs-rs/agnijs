//! The crate's own seam: compile a function and run it against a toy
//! runtime and value type. The VM's differential tests cover the real
//! runtime (crates/agnijs-js-vm/tests/jit_differential.rs).

use std::rc::Rc;

use agnijs_js_compiler::{compile_script, CodeBlock};
use agnijs_js_jit::{
    available, exec_memory_kind, Arith, Compiled, ExecMemoryKind, Exit, Ineligible, JitValue,
    Resume, Runtime,
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

/// A runtime whose generic path answers every instruction with a fixed
/// value and records the pcs it was asked about.
struct Toy {
    regs: Vec<Val>,
    slow_pcs: Vec<u32>,
    slow_result: Val,
    counter: u32,
    polls: u32,
}

impl Toy {
    fn new(registers: usize) -> Toy {
        Toy {
            regs: vec![Val::Undefined; registers],
            slow_pcs: vec![],
            slow_result: Val::Str(99),
            counter: 0,
            polls: 0,
        }
    }
}

impl Runtime<Val> for Toy {
    fn slow(&mut self, pc: u32, _acc: Val) -> Resume<Val> {
        self.slow_pcs.push(pc);
        Resume::Next {
            pc: pc + 1,
            acc: self.slow_result,
        }
    }
    fn poll(&mut self, _pc: u32, target: u32, acc: Val) -> Resume<Val> {
        self.polls += 1;
        Resume::Next { pc: target, acc }
    }
    fn registers(&mut self) -> &mut [Val] {
        &mut self.regs
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
}

/// The first function declared in `src`.
fn function(src: &str) -> Rc<CodeBlock> {
    let script = compile_script(src, "test.js").expect("compiles");
    script.functions[0].clone()
}

fn run(code: &CodeBlock, args: &[Val]) -> (Toy, Val) {
    let compiled = Compiled::<Val>::compile(code).expect("compiles to machine code");
    let mut rt = Toy::new(code.register_count as usize);
    for (i, a) in args.iter().enumerate() {
        rt.regs[code.param_base as usize + i] = *a;
    }
    let out = compiled.enter(&mut rt, 0, Val::Undefined);
    assert_eq!(out.exit, Exit::Return);
    (rt, out.acc)
}

/// What the host says to a `MAP_JIT` mapping, for a failure message.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(unsafe_code)]
fn map_jit_diagnostic() -> String {
    extern "C" {
        fn pthread_jit_write_protect_supported_np() -> std::ffi::c_int;
    }
    // SAFETY: a query, and a fresh anonymous mapping unmapped at once.
    unsafe {
        let supported = pthread_jit_write_protect_supported_np();
        let p = libc::mmap(
            std::ptr::null_mut(),
            16384,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANON | 0x0800,
            -1,
            0,
        );
        let err = std::io::Error::last_os_error();
        if p != libc::MAP_FAILED {
            libc::munmap(p, 16384);
        }
        format!("write protect supported: {supported}; MAP_JIT mmap: {}", if p == libc::MAP_FAILED { err.to_string() } else { "ok".into() })
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn map_jit_diagnostic() -> String {
    String::new()
}

#[test]
fn the_probe_runs_machine_code_on_this_host() {
    assert!(available());
    let kind = exec_memory_kind().expect("the probe mapped executable memory");
    // Apple silicon uses MAP_JIT with per-thread write protection, which
    // also runs under the hardened runtime with the allow-jit entitlement.
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        assert_eq!(kind, ExecMemoryKind::MapJit, "{}", map_jit_diagnostic());
    } else {
        assert_eq!(kind, ExecMemoryKind::Protect);
    }
}

#[test]
fn arithmetic_on_numbers_runs_without_calling_out() {
    let f = function("function f(a, b) { return (a + b) * 2 - b / 4; }");
    let (rt, v) = run(&f, &[Val::Number(3.0), Val::Number(8.0)]);
    assert_eq!(v, Val::Number(20.0));
    assert!(rt.slow_pcs.is_empty(), "called out at {:?}", rt.slow_pcs);
}

#[test]
fn a_loop_runs_to_completion_and_polls_at_its_back_edge() {
    let f = function("function f(n) { var s = 0; for (var i = 0; i < n; i++) s += i; return s; }");
    let (rt, v) = run(&f, &[Val::Number(10_000.0)]);
    assert_eq!(v, Val::Number(49_995_000.0));
    assert!(rt.slow_pcs.is_empty(), "called out at {:?}", rt.slow_pcs);
    // Every 0x1000th back-edge asks the runtime.
    assert_eq!(rt.polls, 10_000 / 0x1000);
    assert_eq!(rt.counter, 10_000);
}

#[test]
fn operands_that_are_not_numbers_take_the_generic_path() {
    let f = function("function f(a, b) { return a + b; }");
    let (rt, v) = run(&f, &[Val::Str(1), Val::Number(2.0)]);
    assert_eq!(rt.slow_pcs.len(), 1);
    assert_eq!(v, Val::Str(99));
}

#[test]
fn bitwise_operators_follow_to_int32() {
    let f = function("function f(a, b) { return ((a | 0) + (a >>> b) + (a << b) + (a >> b) + (a ^ b) + (a & b)); }");
    let js = |a: f64, b: f64| -> f64 {
        let ai = (a as i64) as i32;
        let bi = (b as i64) as u32 & 31;
        (ai as f64)
            + (((ai as u32) >> bi) as f64)
            + (ai.wrapping_shl(bi) as f64)
            + ((ai >> bi) as f64)
            + ((ai ^ (b as i64) as i32) as f64)
            + ((ai & (b as i64) as i32) as f64)
    };
    for (a, b) in [(5.0, 1.0), (-7.5, 3.0), (4_294_967_301.0, 33.0), (-0.0, 0.0), (2_147_483_648.0, 1.0)] {
        let (rt, v) = run(&f, &[Val::Number(a), Val::Number(b)]);
        assert!(rt.slow_pcs.is_empty());
        assert_eq!(v, Val::Number(js(a, b)), "a={a} b={b}");
    }
    // NaN and infinities are left to the generic path.
    let (rt, _) = run(&f, &[Val::Number(f64::INFINITY), Val::Number(1.0)]);
    assert!(!rt.slow_pcs.is_empty());
}

#[test]
fn comparisons_treat_nan_as_unordered() {
    let f = function("function f(a, b) { if (a < b) return 1; if (!(a >= b)) return 2; return 3; }");
    assert_eq!(run(&f, &[Val::Number(1.0), Val::Number(2.0)]).1, Val::Number(1.0));
    assert_eq!(run(&f, &[Val::Number(f64::NAN), Val::Number(2.0)]).1, Val::Number(2.0));
    assert_eq!(run(&f, &[Val::Number(3.0), Val::Number(2.0)]).1, Val::Number(3.0));
}

#[test]
fn truthiness_is_decided_inline_for_non_heap_values() {
    let f = function("function f(a) { if (a) return 1; return 0; }");
    for (a, want) in [
        (Val::Number(0.0), 0.0),
        (Val::Number(f64::NAN), 0.0),
        (Val::Number(-3.0), 1.0),
        (Val::Bool(true), 1.0),
        (Val::Bool(false), 0.0),
        (Val::Null, 0.0),
        (Val::Undefined, 0.0),
        (Val::Obj(4), 1.0),
    ] {
        let (rt, v) = run(&f, &[a]);
        assert!(rt.slow_pcs.is_empty(), "{a:?}");
        assert_eq!(v, Val::Number(want), "{a:?}");
    }
}

#[test]
fn generators_are_not_eligible() {
    let f = function("function* g() { yield 1; }");
    assert_eq!(Compiled::<Val>::compile(&f).err(), Some(Ineligible::Kind));
}

#[test]
fn entering_at_a_pc_that_is_not_an_entry_returns_to_the_interpreter() {
    let f = function("function f(a, b) { return a + b; }");
    let compiled = Compiled::<Val>::compile(&f).unwrap();
    let pc = (0..f.instrs.len() as u32).find(|&pc| !compiled.is_entry(pc as usize));
    if let Some(pc) = pc {
        let mut rt = Toy::new(f.register_count as usize);
        assert_eq!(compiled.enter(&mut rt, pc, Val::Undefined).exit, Exit::Interp);
    }
}
