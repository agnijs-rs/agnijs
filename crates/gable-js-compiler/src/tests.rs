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
        .any(|i| matches!(i, Instr::StaDynamic { .. })));
}

#[test]
fn dynamic_function_checks_parts_separately() {
    assert!(compile_dynamic_function("a, b", "return a + b", DynamicFunctionKind::Normal).is_ok());
    assert!(compile_dynamic_function("a) { evil(", "", DynamicFunctionKind::Normal).is_err());
    assert!(compile_dynamic_function("", "}; evil(); {", DynamicFunctionKind::Normal).is_err());
}
