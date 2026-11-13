//! Crafted bytecode that passes the loader runs without a panic
//! (TASK-103.30). The checksum is not a signature (ADR 0010), so a fuzzer
//! here is the attacker who reseals it: it compiles a corpus, alters the
//! code trees (operands whose meaning depends on run-time context, spliced
//! and deleted instructions, handlers, block flags, raw bytes of the tree),
//! puts the result in a fresh container through `encode_bytecode`, which
//! seals it, and runs whatever `eval_bytecode` accepts. Running may throw
//! or fail; it must not panic. With the `jit` feature cases also run with
//! every eligible block compiled on first use (see [`modes`]). A third test
//! does the same to the code inside a realm snapshot.
//!
//! `BYTECODE_FUZZ_ITERS` and `BYTECODE_FUZZ_SEED` widen a run;
//! `BYTECODE_FUZZ_ONLY` reruns one iteration, and `BYTECODE_FUZZ_TRACE`
//! prints each case before it runs.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, Instr};
use agnijs_js_parser::ast::FunctionKind;
use agnijs_js_vm::engine::{encode_bytecode, testing};
use agnijs_js_vm::Vm;
use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

const SCRIPT: &str = r#"
var log = [];
function sloppy(a, b) { arguments[0] = 9; b = 3; return a + arguments[1] + arguments.length; }
function counter() { let n = 0; return { inc: () => ++n, get: function () { return n; } }; }
var fns = [];
for (let i = 0; i < 3; i++) { fns.push(() => i); }
try { with ({ w: 1 }) { log.push(w); } } catch (e) {}
function* gen(x) { try { const y = yield x; yield* [y, 2]; return 3; } finally { log.push('f'); } }
async function af(p) { const v = await p; try { await Promise.reject(1); } catch (e) { return v + e; } }
async function* ag() { yield 1; yield await 2; }
class Base { #p = 1; static s = 2; static { this.t = 3; } get p() { return this.#p; } #m() { return 4; } m() { return this.#m(); } static #sp() { return 5; } static sp() { return Base.#sp(); } }
class Derived extends Base { constructor(...a) { super(...a); this.q = super.m(); } }
function destr({ a, b: [c, ...d] = [1, 2, 3], ...rest }, ...more) { return a + c + d.length + Object.keys(rest).length + more.length; }
function tag(s, ...v) { return s.raw.join('|') + v.join(','); }
function evals(x) { var y = 1; eval('var z = x + y'); return z; }
function disposer() { using r = { [Symbol.dispose]() { log.push('d'); } }; return 1; }
label: for (var k in { a: 1, b: 2 }) { for (const v of [1, 2]) { if (v == 2) continue label; if (k == 'b') break label; } }
try {
  log.push(sloppy(1, 2), counter().inc(), fns.map(f => f()).join());
  var g = gen(1); g.next(); g.next(7); g.return(0);
  var g2 = gen(2); g2.next(); try { g2.throw(new Error('t')); } catch (e) {}
  af(Promise.resolve(1)).then(v => log.push(v));
  (async () => { for await (const v of ag()) log.push(v); })();
  var d = new Derived(); log.push(d.p, d.q, d.m(), Base.sp(), Base.t);
  log.push(destr({ a: 1, x: 2 }, 3, 4), tag`a${1}b${2}`, evals(2), disposer());
  log.push(/a(b)?/.exec('ab')[1], 2n ** 3n, `t${log.length}`, typeof undefinedName);
  var o = { __proto__: null, ['k' + 1]: 1, get g() { return 2; }, set g(v) {}, m() { return super.toString; } };
  log.push(o.k1, o?.g, o.nope?.x, delete o.k1, 'k1' in o);
  var s = 0; for (var j = 0; j < 20; j++) { s += j * 2 > 10 ? j : -j; s |= 0; } log.push(s);
  switch (s) { case 1: log.push(1); break; default: log.push('d'); }
  throw new TypeError('x');
} catch (e) {
  log.push(e.message);
} finally {
  log.push('end');
}
log.join();
"#;

const MODULE: &str = r#"
import { dep } from './dep.js';
import * as lib from './lib.js';
export { dep } from './dep.js';
export let total = 0;
export function add(x) { total += x; return total; }
const later = await Promise.resolve(dep + lib.lib);
add(later);
try { await import('./dep.js'); } catch (e) {}
globalThis.meta = typeof import.meta;
class M { static #x = 1; static get x() { return M.#x; } }
export default M.x + total;
"#;

/// xorshift64*: deterministic, so a failing seed reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn small(&mut self) -> u16 {
        match self.below(4) {
            0 => 0,
            1 => self.below(4) as u16,
            2 => self.below(32) as u16,
            _ => self.next() as u16,
        }
    }
}

fn size(c: &CodeBlock) -> usize {
    1 + c.functions.iter().map(|f| size(f)).sum::<usize>()
}

/// The `k`th block in preorder, when this test holds the only reference.
fn nth(c: &mut CodeBlock, mut k: usize) -> Option<&mut CodeBlock> {
    if k == 0 {
        return Some(c);
    }
    k -= 1;
    let mut at = None;
    for (i, f) in c.functions.iter().enumerate() {
        let s = size(f);
        if k < s {
            at = Some(i);
            break;
        }
        k -= s;
    }
    nth(Rc::get_mut(&mut c.functions[at?])?, k)
}

fn palette(c: &CodeBlock, out: &mut Vec<Instr>) {
    out.extend_from_slice(&c.instrs);
    for f in &c.functions {
        palette(f, out);
    }
}

/// Alter one operand of `i`, favouring those whose validity depends on
/// run-time context.
fn tweak(i: &mut Instr, rng: &mut Rng, c: &CodeBlock) {
    let regs = c.register_count.max(1) as usize;
    let reg = |rng: &mut Rng| rng.below(regs) as u16;
    let off = |rng: &mut Rng, len: usize| rng.below(len) as i32 - rng.below(len) as i32;
    let len = c.instrs.len();
    match i {
        Instr::LdaEnv { depth, slot } | Instr::StaEnv { depth, slot } => {
            if rng.below(2) == 0 {
                *depth = rng.small();
            } else {
                *slot = rng.small();
            }
        }
        Instr::LdaImport(n) => *n = rng.small() as u32,
        Instr::PushEnv(s) => *s = rng.below(c.scopes.len().max(1)) as u32,
        Instr::Yield { kind } | Instr::YieldRaw { kind } => *kind = reg(rng),
        Instr::ResumeDispatch { kind, on_return } => {
            if rng.below(2) == 0 {
                *kind = reg(rng)
            } else {
                *on_return = off(rng, len)
            }
        }
        Instr::CreateRestArray { from } => *from = rng.small(),
        Instr::CreateArguments { mapped } => *mapped = !*mapped,
        Instr::Ldar(r) | Instr::Star(r) => *r = reg(rng),
        Instr::Mov { dst, src } => {
            *dst = reg(rng);
            *src = reg(rng)
        }
        Instr::Jump(o)
        | Instr::JumpIfTrue(o)
        | Instr::JumpIfFalse(o)
        | Instr::JumpIfEmpty(o)
        | Instr::JumpIfNotEmpty(o)
        | Instr::IteratorStep { done: o, .. }
        | Instr::ForInNext { done: o, .. }
        | Instr::IteratorResult { done: o } => *o = off(rng, len),
        Instr::Call { callee, this, .. } => {
            *callee = reg(rng);
            *this = reg(rng)
        }
        Instr::GetIterator { iter, next } | Instr::IteratorNextRaw { iter, next } => {
            *iter = reg(rng);
            *next = reg(rng)
        }
        Instr::IteratorClose { iter }
        | Instr::IteratorCloseSwallow { iter }
        | Instr::GetIteratorReturn { iter }
        | Instr::ForInPrepare { dst: iter } => *iter = reg(rng),
        Instr::DisposeAdd { stack, .. }
        | Instr::DisposeStep { stack }
        | Instr::DisposeError { stack }
        | Instr::DisposeEnd { stack } => *stack = reg(rng),
        Instr::DisposeBegin { stack, kind, value } => {
            *stack = reg(rng);
            *kind = reg(rng);
            *value = reg(rng)
        }
        Instr::DefineOwn { obj, key }
        | Instr::DefineMethod { obj, key, .. }
        | Instr::DefineGetter { obj, key, .. }
        | Instr::DefineSetter { obj, key, .. }
        | Instr::DefineField { obj, key, .. } => {
            *obj = reg(rng);
            *key = reg(rng)
        }
        Instr::CreateClass {
            heritage,
            proto,
            name,
            ..
        } => {
            *heritage = reg(rng);
            *proto = reg(rng);
            *name = reg(rng)
        }
        Instr::AddField {
            ctor, key, init, ..
        } => {
            *ctor = reg(rng);
            *key = reg(rng);
            *init = reg(rng)
        }
        Instr::GetPrivate { obj, name }
        | Instr::SetPrivate { obj, name }
        | Instr::HasPrivate { obj, name } => {
            *obj = reg(rng);
            *name = reg(rng)
        }
        Instr::SuperCall {
            func, new_target, ..
        }
        | Instr::SuperCallSpread {
            func, new_target, ..
        } => {
            *func = reg(rng);
            *new_target = reg(rng)
        }
        Instr::CheckDerivedReturn { this } => *this = reg(rng),
        Instr::ArrayPush { arr } | Instr::ArrayPushHole { arr } | Instr::ArraySpread { arr } => {
            *arr = reg(rng)
        }
        _ => {}
    }
}

const FUNCTION_KINDS: [FunctionKind; 4] = [
    FunctionKind::Normal,
    FunctionKind::Generator,
    FunctionKind::Async,
    FunctionKind::AsyncGenerator,
];
const CODE_KINDS: [CodeKind; 8] = [
    CodeKind::Script,
    CodeKind::Module,
    CodeKind::Eval,
    CodeKind::Function,
    CodeKind::Arrow,
    CodeKind::Method,
    CodeKind::ClassConstructor,
    CodeKind::ClassInitializer,
];

/// Apply one random edit to `tree`; a description of it for the report.
fn mutate(tree: &mut CodeBlock, all: &[Instr], rng: &mut Rng) -> String {
    let k = rng.below(size(tree));
    let Some(c) = nth(tree, k) else {
        return String::from("none");
    };
    let len = c.instrs.len();
    let pc = rng.below(len);
    match rng.below(10) {
        0 | 1 => {
            let from = c.instrs[rng.below(len)];
            let was = std::mem::replace(&mut c.instrs[pc], from);
            format!("block {k} pc {pc}: {was:?} -> {from:?} (same block)")
        }
        2 | 3 => {
            let from = all[rng.below(all.len())];
            let was = std::mem::replace(&mut c.instrs[pc], from);
            format!("block {k} pc {pc}: {was:?} -> {from:?}")
        }
        4 => {
            let was = std::mem::replace(&mut c.instrs[pc], Instr::Nop);
            format!("block {k} pc {pc}: {was:?} -> Nop")
        }
        5 | 6 => {
            // Prefer an instruction the tweak knows.
            let mut pc = pc;
            for _ in 0..8 {
                let mut probe = c.instrs[pc];
                tweak(&mut probe, rng, c);
                if probe != c.instrs[pc] {
                    break;
                }
                pc = rng.below(len);
            }
            let was = c.instrs[pc];
            let mut now = was;
            tweak(&mut now, rng, c);
            c.instrs[pc] = now;
            format!("block {k} pc {pc}: {was:?} -> {now:?} (tweak)")
        }
        7 => {
            if c.handlers.is_empty() {
                return String::from("none");
            }
            let h = rng.below(c.handlers.len());
            let hd = &mut c.handlers[h];
            match rng.below(4) {
                0 => hd.env_depth = rng.small(),
                1 => hd.target = rng.below(len) as u32,
                2 => hd.start = rng.below(hd.end as usize + 1) as u32,
                _ => hd.end = (hd.start as usize + rng.below(len - hd.start as usize + 1)) as u32,
            }
            format!("block {k} handler {h}: {hd:?}")
        }
        8 => {
            let what = rng.below(7);
            match what {
                0 => c.function_kind = FUNCTION_KINDS[rng.below(4)],
                1 => c.kind = CODE_KINDS[rng.below(8)],
                2 => c.strict = !c.strict,
                3 => c.is_derived_constructor = !c.is_derived_constructor,
                4 => c.keeps_arguments = !c.keeps_arguments,
                5 => c.param_count = rng.small(),
                _ => c.declarations = None,
            }
            format!(
                "block {k} flags {what}: {:?} {:?} strict {} derived {} args {} params {}",
                c.kind,
                c.function_kind,
                c.strict,
                c.is_derived_constructor,
                c.keeps_arguments,
                c.param_count
            )
        }
        _ => {
            let other = rng.below(len);
            c.instrs.swap(pc, other);
            format!("block {k} swap {pc} and {other}")
        }
    }
}

// Where and why the last panic on this thread happened, so a failure
// report names the VM invariant instead of just "panicked"; set while a
// case runs.
thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
    static CAPTURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn quiet_panics() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !CAPTURING.with(|c| c.get()) {
            return prev(info);
        }
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        LAST_PANIC.with(|p| *p.borrow_mut() = Some(format!("{at}: {msg}")));
        if std::env::var_os("BYTECODE_FUZZ_VERBOSE").is_some() {
            prev(info);
        }
    }));
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Interpreter,
    #[cfg(feature = "jit")]
    JitForce,
}

/// The modes iteration `iter` runs in. Compiling every block is slow in a
/// debug build, so there only one iteration in `BYTECODE_FUZZ_JIT_EVERY`
/// (default 50) also runs compiled; in a release build every one does.
#[cfg(feature = "jit")]
fn modes(iter: usize) -> Vec<Mode> {
    let every: usize = std::env::var("BYTECODE_FUZZ_JIT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 50 } else { 1 });
    if iter % every.max(1) == 0 {
        vec![Mode::Interpreter, Mode::JitForce]
    } else {
        vec![Mode::Interpreter]
    }
}

#[cfg(not(feature = "jit"))]
fn modes(_iter: usize) -> Vec<Mode> {
    vec![Mode::Interpreter]
}

/// Load and run `bytes` in a fresh engine; whether the loader took them.
fn run(bytes: &[u8], module: bool, mode: Mode) -> bool {
    let host = TestHost::new();
    host.add_module("dep.js", "export const dep = 20;");
    host.add_module("lib.js", "export const lib = 1;");
    let mut vm = Vm::new(host.clone());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    Engine::set_memory_limit(&mut vm, Some(64 << 20));
    bound(&mut vm, &host, mode);
    let _ = module;
    let Ok(v) = vm.eval_bytecode(realm, bytes) else {
        return false;
    };
    let _ = vm.run_jobs(realm);
    if let Value::Object(_) = v {
        let _ = vm.promise_state(realm, &v);
    }
    vm.collect_garbage();
    // A snapshot writes the loaded code trees again.
    let _ = Engine::snapshot(&mut vm, realm);
    // The engine stays usable for the next call.
    let _ = vm.eval_script(realm, "after.js", "typeof log");
    vm.collect_garbage();
    true
}

/// Bound a run that an edit made endless.
fn bound(vm: &mut Vm, host: &TestHost, mode: Mode) {
    let _ = host;
    match mode {
        Mode::Interpreter => Engine::set_fuel(vm, Some(100_000)).unwrap(),
        // Compiled code is never entered under a fuel budget, so the
        // compiled runs are bounded by the host's interrupt instead.
        #[cfg(feature = "jit")]
        Mode::JitForce => {
            vm.set_jit_mode(agnijs_js_vm::interp::jit::JitMode::Force);
            host.interrupt_after.set(Some(32));
        }
    }
}

fn compile(src: &str, module: bool) -> Rc<CodeBlock> {
    if module {
        agnijs_js_compiler::compile_module(src, "main.js").unwrap()
    } else {
        agnijs_js_compiler::compile_script(src, "fuzz.js").unwrap()
    }
}

fn fuzz(module: bool, default_iters: usize) {
    quiet_panics();
    let src = if module { MODULE } else { SCRIPT };
    let mut all = Vec::new();
    palette(&compile(src, module), &mut all);
    palette(
        &compile(if module { SCRIPT } else { MODULE }, !module),
        &mut all,
    );
    let iters = iterations(default_iters);
    let seed: u64 = std::env::var("BYTECODE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5eed_0103_0030_0001 ^ module as u64);
    let mut rng = Rng(seed | 1);
    // Rerun one iteration of a seed (the edits of the others are still drawn).
    let only: Option<usize> = std::env::var("BYTECODE_FUZZ_ONLY")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut failures: Vec<String> = Vec::new();
    let (mut loaded, mut tried) = (0usize, 0usize);
    for iter in 0..iters {
        let mut tree = Rc::try_unwrap(compile(src, module)).expect("sole owner");
        let mut what = Vec::new();
        let bytes = if rng.below(5) == 0 {
            // Raw edits of the serialised tree, in place, resealed.
            let mut raw = encode_bytecode(module, &tree);
            let span = testing::tree_span(&raw).expect("tree span");
            for _ in 0..1 + rng.below(3) {
                let at = span.start + rng.below(span.len());
                raw[at] = match rng.below(3) {
                    0 => raw[at] ^ (1 << rng.below(8)),
                    1 => raw[at].wrapping_add(1 + rng.below(3) as u8),
                    _ => rng.next() as u8,
                };
                what.push(format!("byte {} = {}", at - span.start, raw[at]));
            }
            testing::reseal(&mut raw);
            raw
        } else {
            for _ in 0..1 + rng.below(3) {
                what.push(mutate(&mut tree, &all, &mut rng));
            }
            encode_bytecode(module, &tree)
        };
        if only.is_some_and(|o| o != iter) {
            continue;
        }
        for mode in modes(iter) {
            tried += 1;
            if std::env::var_os("BYTECODE_FUZZ_TRACE").is_some() {
                eprintln!("iter {iter} {mode:?}: {}", what.join("; "));
            }
            match caught(|| run(&bytes, module, mode)) {
                Ok(true) => loaded += 1,
                Ok(false) => {}
                Err(why) => {
                    failures.push(format!(
                        "seed {seed:#x} iter {iter} {mode:?}: {why}\n    edits: {}",
                        what.join("; ")
                    ));
                }
            }
        }
    }
    eprintln!(
        "bytecode fuzz ({}): {loaded} of {tried} runs loaded",
        if module { "module" } else { "script" }
    );
    report(&failures, tried);
}

/// `BYTECODE_FUZZ_ITERS`, or `default`: a twentieth of it under GC stress,
/// where every safe point collects, and two cases under Miri.
fn iterations(default: usize) -> usize {
    let stress = std::env::var_os("AGNIJS_GC_STRESS").is_some();
    std::env::var("BYTECODE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(miri) {
            2
        } else if stress {
            default / 20
        } else {
            default
        })
}

/// Run `f`, turning a panic into the message the hook recorded.
fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    LAST_PANIC.with(|p| p.borrow_mut().take());
    CAPTURING.with(|c| c.set(true));
    let outcome = catch_unwind(AssertUnwindSafe(f));
    CAPTURING.with(|c| c.set(false));
    outcome.map_err(|_| {
        LAST_PANIC
            .with(|p| p.borrow_mut().take())
            .unwrap_or_default()
    })
}

/// Fail with one example per distinct panic site.
fn report(failures: &[String], tried: usize) {
    if !failures.is_empty() {
        let mut distinct: Vec<&String> = Vec::new();
        for f in failures {
            let key = f.split(": ").nth(1).unwrap_or("");
            if !distinct
                .iter()
                .any(|d| d.split(": ").nth(1).unwrap_or("") == key)
            {
                distinct.push(f);
            }
        }
        panic!(
            "{} of {tried} runs panicked ({} distinct sites):\n{}",
            failures.len(),
            distinct.len(),
            distinct
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn crafted_script_bytecode_runs_without_panicking() {
    fuzz(false, 1500);
}

#[test]
fn crafted_module_bytecode_runs_without_panicking() {
    fuzz(true, 600);
}

/// Calls into every function the corpus script defines, each on its own,
/// so that one throw does not hide the rest.
const PROBE: &[&str] = &[
    "sloppy(1, 2)",
    "counter().inc()",
    "fns.map(f => f()).join()",
    "var g = gen(1); g.next(); g.next(7); g.return(0)",
    "af(Promise.resolve(1)).then(v => log.push(v))",
    "(async () => { for await (const v of ag()) log.push(v); })()",
    "var d = new Derived(); d.p + d.q + d.m() + Base.sp() + Base.t",
    "destr({ a: 1, x: 2 }, 3, 4)",
    "tag`a${1}b${2}`",
    "evals(2)",
    "disposer()",
    "o.m(); o.g",
    "log.join()",
];

/// A realm snapshot whose code trees were altered and resealed: restore it
/// and run the probe. The snapshot loader decodes code with the bytecode
/// verifier, so whatever it accepts must run (or throw) without a panic.
fn run_snapshot(bytes: &[u8], mode: Mode) -> bool {
    let host = TestHost::new();
    let Ok(mut vm) = agnijs_js_vm::snapshot::restore(host.clone(), bytes) else {
        return false;
    };
    let realm = vm.main_realm();
    Engine::set_memory_limit(&mut vm, Some(64 << 20));
    bound(&mut vm, &host, mode);
    for p in PROBE {
        let _ = vm.eval_script(realm, "probe.js", &format!("try {{ {p} }} catch (e) {{}}"));
    }
    let _ = vm.run_jobs(realm);
    vm.collect_garbage();
    let _ = Engine::snapshot(&mut vm, realm);
    true
}

#[test]
// Function addresses are not stable under Miri, so the native registry
// cannot name the built-ins and no realm can be snapshotted there.
#[cfg_attr(miri, ignore)]
fn crafted_snapshot_code_runs_without_panicking() {
    use agnijs_js_vm::snapshot::testing::{code_span, reseal};
    quiet_panics();
    let base = {
        let mut vm = Vm::new(TestHost::new());
        Vm::create_realm(&mut vm);
        let realm = vm.main_realm();
        vm.eval_script(realm, "corpus.js", SCRIPT)
            .expect("corpus runs");
        let _ = vm.run_jobs(realm);
        // Generator objects cannot be snapshotted (ADR 0022).
        vm.eval_script(realm, "clear.js", "g = g2 = undefined;")
            .expect("clear");
        Engine::snapshot(&mut vm, realm).expect("corpus snapshots")
    };
    let span = code_span(&base).expect("code span");
    let iters = iterations(400);
    let seed: u64 = std::env::var("BYTECODE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5eed_0103_0030_0002);
    let mut rng = Rng(seed | 1);
    let mut failures = Vec::new();
    let (mut loaded, mut tried) = (0usize, 0usize);
    for iter in 0..iters {
        let mut bytes = base.clone();
        let mut what = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let at = span.start + rng.below(span.len());
            bytes[at] = match rng.below(3) {
                0 => bytes[at] ^ (1 << rng.below(8)),
                1 => bytes[at].wrapping_add(1 + rng.below(3) as u8),
                _ => rng.next() as u8,
            };
            what.push(format!("byte {} = {}", at - span.start, bytes[at]));
        }
        reseal(&mut bytes);
        for mode in modes(iter) {
            tried += 1;
            match caught(|| run_snapshot(&bytes, mode)) {
                Ok(true) => loaded += 1,
                Ok(false) => {}
                Err(why) => {
                    failures.push(format!(
                        "seed {seed:#x} iter {iter} {mode:?}: {why}\n    edits: {}",
                        what.join("; ")
                    ));
                }
            }
        }
    }
    eprintln!("snapshot code fuzz: {loaded} of {tried} runs loaded");
    report(&failures, tried);
}
