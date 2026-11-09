//! Compiler smoke tests: programs compile and the code looks sane.

use crate::*;

fn script(src: &str) -> Rc<CodeBlock> {
    match compile_script(src, "test") {
        Ok(cb) => cb,
        Err(e) => panic!("compile failed for {src:?}: {e}"),
    }
}

#[test]
fn compiles_basic_programs() {
    let cb = script("var a = 1; let b = a + 2; function f(x) { return x * b; } f(a);");
    assert_eq!(cb.kind, CodeKind::Script);
    assert_eq!(cb.functions.len(), 1);
    assert!(cb.instrs.iter().any(|i| matches!(i, Instr::DeclareGlobals)));
    let f = &cb.functions[0];
    assert_eq!(&*f.name, "f");
    assert_eq!(f.param_count, 1);
}

#[test]
fn captured_bindings_go_to_environments() {
    let cb = script("function outer() { let x = 1; return () => x; }");
    let outer = &cb.functions[0];
    // x is captured by the arrow: outer pushes an environment.
    assert!(outer.instrs.iter().any(|i| matches!(i, Instr::PushEnv(_))));
    let arrow = &outer.functions[0];
    assert!(arrow
        .instrs
        .iter()
        .any(|i| matches!(i, Instr::LdaEnv { depth: 0, slot: 0 })));
}

#[test]
fn uncaptured_bindings_stay_in_registers() {
    let cb = script("function f() { let x = 1; let y = x + 1; return y; }");
    let f = &cb.functions[0];
    assert!(!f.instrs.iter().any(|i| matches!(i, Instr::PushEnv(_))));
}

#[test]
fn module_block_scoped_bindings_stay_in_registers() {
    // A module's top-level bindings live in the module environment (the host
    // creates it), but a loop variable nobody captures is a register, so the
    // loop neither pushes nor copies an environment per iteration.
    let cb = compile_module(
        "let s = 0; for (let i = 0; i < 10; i++) s += i; export { s };",
        "m",
    )
    .unwrap();
    assert!(!cb.instrs.iter().any(|i| matches!(i, Instr::PushEnv(_))));
    assert!(!cb.instrs.iter().any(|i| matches!(i, Instr::CopyEnv)));
    // Captured by a closure, it is back in an environment copied per iteration.
    let cb = compile_module(
        "export const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i);",
        "m",
    )
    .unwrap();
    assert!(cb.instrs.iter().any(|i| matches!(i, Instr::CopyEnv)));
}

#[test]
fn compiles_control_flow() {
    script("for (let i = 0; i < 3; i++) { if (i == 1) continue; if (i == 2) break; }");
    script("l: for (const k in o) { for (const v of a) { if (v) continue l; else break l; } }");
    script("try { throw 1 } catch (e) { e } finally { 2 }");
    script("function f() { for (const x of y) { try { return x } finally { g() } } }");
    script("switch (x) { case 1: let a = 1; break; default: a; }");
    script("a: { b: { break a; } }");
    script("with (o) { x = y }");
}

#[test]
fn compiles_functions_and_classes() {
    script("class A { #x = 1; static y = 2; constructor() {} get x() { return this.#x } static { this.z = 3 } m() { return super.toString() } }");
    script(
        "class B extends A { constructor(...a) { super(...a); } static #p() {} q = () => this; }",
    );
    script("function* g() { const x = yield 1; yield* [x]; }");
    script("async function h() { for await (const x of y) { await x } }");
    script("async function* k() { yield await 1; yield* g(); }");
    script("const {a, b: [c = 1, ...d], ...e} = o; [a.b, c[d]] = arr; ({x: o.y = 2} = p);");
    script("function f(a, b = a, {c}, ...d) { arguments; eval('x'); }");
    script("var o = { a, b() {}, get c() {}, set c(v) {}, [k]: 1, ...s, __proto__: null, async *d() {} };");
    script("x = a?.b?.[c]?.(d); delete a?.b; `t${1}` ; tag`x${y}`;");
}

#[test]
fn compiles_modules() {
    let cb = compile_module("import a, {b as c} from 'm'; export const d = a + c; export default function () {} export * from 'n'; export {d as e};", "m").unwrap();
    let info = cb.module.as_ref().unwrap();
    assert_eq!(info.requested.len(), 2);
    assert_eq!(info.imports.len(), 2);
    assert_eq!(info.local_exports.len(), 3);
    assert_eq!(info.star_exports.len(), 1);
    assert!(cb.instrs.iter().any(|i| matches!(i, Instr::LdaImport(_))));
}

#[test]
fn compiles_eval_code() {
    let cb = compile_eval(
        "var x = 1; let y = 2; x + y",
        "eval",
        &EvalOptions::default(),
    )
    .unwrap();
    assert_eq!(cb.kind, CodeKind::Eval);
    assert!(cb.instrs.iter().any(|i| matches!(i, Instr::DeclareGlobals)));
    assert!(cb
        .instrs
        .iter()
        .any(|i| matches!(i, Instr::StaDynamic { .. } | Instr::StaDynamicRef { .. })));
}

// ---- accumulator traffic (TASK-100.18) ----------------------------------------

#[test]
fn instructions_stay_twelve_bytes() {
    // ADR 0019 J2: `Instr` is fixed-size and `pc` an index. The fused
    // compare-and-branch forms (operator, register, immediate, offset) are
    // the widest.
    assert_eq!(std::mem::size_of::<Instr>(), 12);
}

#[test]
fn an_unused_postfix_update_does_not_convert_the_old_value() {
    // `i++` as a statement never observes the old value, so the ToNumeric
    // that produces it (and the register holding it) is dropped. Inc/Dec
    // convert their operand themselves.
    let cb = script("function f() { var i = 0; i++; for (;;) { i--; } return i; }");
    let f = &cb.functions[0];
    assert!(!f.instrs.iter().any(|i| matches!(i, Instr::ToNumeric)), "{:#?}", f.instrs);
    // When the old value is used, it is still converted first.
    let cb = script("function f() { var i = 0; var j = i++; return j; }");
    let f = &cb.functions[0];
    assert!(f.instrs.iter().any(|i| matches!(i, Instr::ToNumeric)), "{:#?}", f.instrs);
}

#[test]
fn a_loop_test_on_register_variables_is_one_fused_compare_and_branch() {
    let cb = script(
        "function f(n) { var s = 0; for (var i = 0; i < n; i++) { s = (s + i * 7) % 1000003; } return s; }",
    );
    let f = &cb.functions[0];
    let fused = f
        .instrs
        .iter()
        .filter(|i| matches!(i, Instr::JumpIfNotCmpRR { op: BinOp::Lt, .. }))
        .count();
    assert_eq!(fused, 1, "{:#?}", f.instrs);
    assert!(!f.instrs.iter().any(|i| matches!(i, Instr::Lt(_) | Instr::JumpIfFalse(_))), "{:#?}", f.instrs);
    // A comparison against an integer literal takes it as an immediate.
    let cb = script("function g(n) { while (n > 2) { n = n - 1; } return n; }");
    let g = &cb.functions[0];
    assert!(g.instrs.iter().any(|i| matches!(i, Instr::JumpIfNotCmpRI { op: BinOp::Gt, imm: 2, .. })), "{:#?}", g.instrs);
    assert!(g.instrs.iter().any(|i| matches!(i, Instr::BinRI { op: BinOp::Sub, imm: 1, .. })), "{:#?}", g.instrs);
}

#[test]
fn binary_operators_on_register_variables_take_register_and_immediate_operands() {
    let cb = script("function f(a, b) { var c = a * b; return (c + a) % 1000003; }");
    let f = &cb.functions[0];
    // a * b: both registers, no load into the accumulator first.
    assert!(f.instrs.iter().any(|i| matches!(i, Instr::BinRR { op: BinOp::Mul, .. })), "{:#?}", f.instrs);
    // (c + a) % 1000003: the result of the addition is in the accumulator,
    // the literal is an immediate.
    assert!(f.instrs.iter().any(|i| matches!(i, Instr::BinAI { op: BinOp::Mod, imm: 1000003 })), "{:#?}", f.instrs);
    // After the parameter prologue (arguments copied into their binding
    // registers), nothing goes through the accumulator but the results.
    let first = f.instrs.iter().position(|i| matches!(i, Instr::BinRR { .. })).unwrap();
    assert!(!f.instrs[first..].iter().any(|i| matches!(i, Instr::Ldar(_))), "{:#?}", f.instrs);
    // A literal on the left stays a load: `7 + x` is string concatenation
    // when x is a string, and the order of ToPrimitive calls is observable.
    let cb = script("function g(x) { return 7 + x; }");
    let g = &cb.functions[0];
    assert!(!g.instrs.iter().any(|i| matches!(i, Instr::BinRI { .. } | Instr::BinAI { .. })), "{:#?}", g.instrs);
    // An operand that assigns to a register variable forces the other
    // operand to be read first into a temporary.
    let cb = script("function h(x) { return x + (x = 5); }");
    let h = &cb.functions[0];
    assert!(!h.instrs.iter().any(|i| matches!(i, Instr::BinRR { .. } | Instr::BinAR { .. })), "{:#?}", h.instrs);
}

#[test]
fn dynamic_function_checks_parts_separately() {
    assert!(compile_dynamic_function("a, b", "return a + b", DynamicFunctionKind::Normal).is_ok());
    assert!(compile_dynamic_function("a) { evil(", "", DynamicFunctionKind::Normal).is_err());
    assert!(compile_dynamic_function("", "}; evil(); {", DynamicFunctionKind::Normal).is_err());
}
