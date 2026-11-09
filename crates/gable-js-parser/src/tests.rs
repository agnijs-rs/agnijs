//! Parser tests: positive programs must parse, negative ones must fail.

use crate::{parse_module, parse_script};

fn script(src: &str) {
    if let Err(e) = parse_script(src) {
        panic!("should parse: {src:?}\n  {e}");
    }
}
fn bad_script(src: &str) {
    if parse_script(src).is_ok() {
        panic!("should not parse: {src:?}");
    }
}
fn module(src: &str) {
    if let Err(e) = parse_module(src) {
        panic!("should parse as module: {src:?}\n  {e}");
    }
}
fn bad_module(src: &str) {
    if parse_module(src).is_ok() {
        panic!("should not parse as module: {src:?}");
    }
}

#[test]
fn statements() {
    script("var a = 1, b; let c = 2; const d = 3;");
    script("if (a) b; else c;");
    script("for (;;) break; for (var i = 0; i < 1; i++) continue;");
    script("for (let x of y) {} for (const k in o) {} for (x of y) {} for (x.y in o) {}");
    script("while (a) {} do ; while (a) x");
    script("l: for (;;) { continue l; } m: { break m; }");
    script("switch (x) { case 1: break; default: }");
    script("try {} catch {} try {} catch (e) {} finally {}");
    script("throw 1; debugger;");
    script("with (o) {}");
    script("a\n++b");
    script("if (a) function f() {}");
    script("for (let in x) {}");
    script("let\nx = 1");
    bad_script("'use strict'; with (o) {}");
    bad_script("break;");
    bad_script("continue;");
    bad_script("l: l: ;");
    bad_script("throw\n1");
    bad_script("switch (x) { default: default: }");
    bad_script("let let = 1");
    bad_script("const x;");
    bad_script("for (let x of y, z) {}");
    bad_script("for (let of x) {}");
    bad_script("for (async of x) {}");
    bad_script("if (a) class C {}");
    bad_script("while (a) function f() {}");
    bad_script("try {}");
    bad_script("try {} catch (e) { let e; }");
    script("try {} catch (e) { var e; }");
    bad_script("try {} catch ([e]) { var e; }");
    script("try {} catch (e) { for (var e of []) ; }");
    bad_script("while (1) l: function f() {}");
    bad_script("if (1) l: m: function f() {}");
    script("l: function f() {}");
    bad_script("() => {} + 1");
    bad_script("() => {}()");
    script("() => {}\n(1)");
    bad_script("class C extends () => {} {}");
    bad_script("async (...a,) => 0");
    bad_script("async (...a = 1) => 0");
    bad_script("async (a = await) => 0");
    bad_script("\\u0061sync () => 0");
    bad_script("({...a,} = o)");
    bad_script("'use strict'; ({eval = 1} = o)");
    bad_script("for (a = 0 in o);");
    script("for ((a) in o);");
    bad_script("{ function* g() {} function g() {} }");
    bad_script("{ async function g() {} async function g() {} }");
    bad_script("return");
}

#[test]
fn declarations() {
    script("function f(a, b = 1, ...c) { return a; }");
    script("function* g() { yield 1; yield* g(); }");
    script("async function h() { await 1; for await (const x of y) {} }");
    script("async function* k() { yield await 1; }");
    script("class C extends B { #x = 1; static y; constructor() { super(); } get a() {} set a(v) {} static m() {} static { this.z = 1; } #m() { this.#x; } static #s() {} }");
    script("let {a, b: [c, d = 1], ...e} = o; let [f, , g = 2, ...h] = arr;");
    bad_script("let a, a;");
    bad_script("let a; var a;");
    bad_script("const a = 1; function a() {}");
    script("var a; var a; function a() {} function a() {}");
    script("{ function f() {} function f() {} }");
    bad_script("'use strict'; { function f() {} function f() {} }");
    bad_script("{ let f; function f() {} }");
    bad_script("function f(a, a) { 'use strict' }");
    script("function f(a, a) {}");
    bad_script("function f(a, a = 1) {}");
    bad_script("function f(a = 1) { 'use strict' }");
    bad_script("class C { constructor() {} constructor() {} }");
    bad_script("class C { #x; #x; }");
    script("class C { get #x() {} set #x(v) {} }");
    bad_script("class C { get #x() {} static set #x(v) {} }");
    bad_script("class C { m() { this.#y; } }");
    script("class C { m() { class D { n() { this.#x; } } } #x; }");
    bad_script("class C { static prototype() {} }");
    bad_script("class C { constructor = 1 }");
    bad_script("class C extends B { x = arguments }");
    script(
        "class C { x = () => this; ['computed'] = 1; 'str' = 2; 3 = 4; static async *gen() {} }",
    );
    bad_script("function f() { super.x }");
    script("({ m() { super.x } })");
    bad_script("({ m() { super() } })");
    bad_script("class C { constructor() { super() } }");
    script("function* g() { function f(a = yield) {} }");
    bad_script("function* g(a = yield) {}");
    bad_script("async function f(a = await 1) {}");
    bad_script("async function f() { var await; }");
    bad_script("function* g() { var yield; }");
    script("var yield, await; yield: ;");
    bad_script("'use strict'; var yield;");
}

#[test]
fn expressions() {
    script("a = b ? c : d; a += b; a ||= b; a ??= b; a ** b; (-a) ** b;");
    bad_script("-a ** b");
    bad_script("a ?? b || c");
    script("(a ?? b) || c");
    script("x = a?.b?.[c]?.(d); new a.b(); new new a()(); function f() { new.target }");
    bad_script("new a?.b()");
    bad_script("a?.b = 1");
    bad_script("a?.`t`");
    bad_script("new.target");
    script("`a${b}c${d}e`; tag`x`; tag`\\unicode`");
    bad_script("`\\unicode`");
    script("/re/gimsuy; /[/]/; a / b / c");
    bad_script("/(/");
    bad_script("/a/gg");
    script("[a, , b = 1, ...c] = d; ({a, b: c, ...d} = e)");
    bad_script("[...a, b] = c");
    bad_script("({a = 1})");
    script("({a = 1} = o)");
    script("({a = 1}) => 0");
    bad_script("({__proto__: 1, __proto__: 2})");
    script("({__proto__: a, __proto__: b} = o)");
    script("({__proto__: 1, ['__proto__']: 2, __proto__})");
    script("x => x; (a, b) => a; (a = 1, {b}, [c], ...d) => 0; async x => x; async (a) => a; async () => {}");
    bad_script("(a, a) => 0");
    bad_script("(...a, b) => 0");
    bad_script("x\n=> x");
    bad_script("(a = 1) => { 'use strict' }");
    script("async\nx => x");
    bad_script("async x\n=> x");
    script("delete a.b; delete a; typeof void 0;");
    bad_script("'use strict'; delete a;");
    bad_script("class C { #x; m() { delete this.#x } }");
    script("a++; ++a; a.b--;");
    bad_script("1++");
    bad_script("++(a = 1)");
    script("import('x'); import('x', {with: {}})");
    bad_script("import.meta");
    script("async function f() { for await (x of y); }");
    script("yield => 1; async function f() { yield => 1 }");
    script("class C { #x; static has(o) { return #x in o; } }");
    bad_script("class C { #x; static has(o) { return 1 + #x in o; } }");
    script("x = {get a() {}, set a(v) {}, async b() {}, *c() {}, async *d() {}, get: 1, set: 2, async: 3, await: 4}");
    script("0b101 + 0o17 + 0xff + 1_000 + .5e3 + 5n + 017 + 09");
    bad_script("'use strict'; 017");
    bad_script("'use strict'; '\\07'");
    bad_script("'use strict'; '\\8'");
    script("'\\8'");
    bad_script("1__0");
    bad_script("0_1");
    bad_script("08n");
    script("a\n(b)");
    script("if (a) {} else {}\n/re/.test(x)");
    script("x = y / z / w");
    script("<!-- html comment\n--> also a comment\nx");
    bad_module("<!-- not in modules");
}

#[test]
fn modules() {
    module("import a, {b as c, 'd' as e} from 'm'; import * as ns from 'n'; import 'side'; import x from 'y' with { type: 'json' };");
    module("export var a; export let b; export const c = 1; export function f() {} export class C {} export default 1;");
    module("let x; export {x as y, x as 'z'}; export * from 'm'; export * as w from 'm'; export {q} from 'm';");
    module("export default function () {} ; export { a as b } from 'm'");
    module("await 1; for await (const x of y);");
    module("export default class {}");
    bad_module("export {x}");
    bad_module("let a; export {a}; export {a}");
    bad_module("export default 1; export default 2;");
    bad_module("import {a} from 'm'; let a;");
    bad_module("import 'x' with { type: 'json', type: 'json' };");
    bad_module("var await;");
    bad_module("import.meta = 1");
    module("import.meta.url; new import.meta()");
    script("class C { static { (function await(await) {}); (function* await() {}); } }");
    bad_script("class C { static { (async function await() {}); } }");
    bad_script("(function* yield() {})");
    module("export { 'string name' } from 'm'");
    bad_module("export { 'string name' }");
    bad_module("with (o) {}");
    bad_module("import {default} from 'm'");
    module("import {default as d} from 'm'");
    bad_script("import a from 'm'");
    bad_script("export var a");
    script("import('m')");
}

#[test]
fn strictness() {
    script("'use strict'; var eval2;");
    bad_script("'use strict'; var eval;");
    bad_script("'use strict'; arguments = 1");
    bad_script("'use strict'; function static() {}");
    script("function static() {}");
    bad_script("function f() { 'use strict'; var implements; }");
    script("function f() { 'not strict'; var implements; }");
    bad_script("'use strict'; 'use strict'; 010");
    bad_script("'\\010'; 'use strict';");
    script("'use strict'; ({ eval: 1, arguments: 2 })");
    bad_script("'use strict'; ({ eval } = o)");
    bad_script("'use strict'; eval => 1");
    bad_script("class C { m(eval) {} }");
    bad_script("class C { m() { with (o); } }");
}
