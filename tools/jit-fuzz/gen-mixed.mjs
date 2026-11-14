// Random JS program generator for JIT differential testing (AgniJS baseline JIT review, 2026-11).
// Targets: number speculation and deopt, try/catch/finally reading locals, inline caches with
// shape/prototype changes, dense elements, direct calls (this modes, arity, recursion), closures
// and environment slots, global bindings that appear/disappear, getters/setters with side effects.
//
// Generator 1 of docs/jit-review-2026-11.md: a seed gives the program it gave in the review, with
// fixes for programs that were SyntaxErrors then (negative literals and `typeof this` are
// parenthesized, and "use strict" is left out of a function with a default parameter). About a
// quarter of the review's programs were SyntaxErrors and tested nothing; the rest parse to the
// same tree as then. It logs error messages, which differ between engines, so it is not compared
// with Node.
export const nodeSafe = false;
export const hasWarm = false;

export function rng(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const R = (n) => Math.floor(next() * n);
  R.pick = (xs) => xs[R(xs.length)];
  R.chance = (p) => next() < p;
  return R;
}

const HEADER = `
var LOG = [];
var STEPS = 0;
function tick() { if (++STEPS > 2500) throw new RangeError("budget"); }
function fmt(x, d) {
  var t = typeof x;
  if (t === "number") return Object.is(x, -0) ? "-0" : String(x);
  if (t === "string") return x.length > 40 ? '"' + x.slice(0, 40) + '"~' + x.length : JSON.stringify(x);
  if (t === "undefined" || x === null || t === "boolean") return String(x);
  if (t === "bigint") return x + "n";
  if (t === "symbol") return "sym";
  if (t === "function") return "fn";
  d = d | 0;
  if (d > 2) return "obj";
  var s, i, v;
  if (Array.isArray(x)) {
    s = "[";
    for (i = 0; i < x.length && i < 10; i++) s += (i in x ? fmt(x[i], d + 1) : "hole") + ",";
    return s + "]#" + x.length;
  }
  var ks = Object.keys(x);
  s = "{";
  for (i = 0; i < ks.length && i < 10; i++) {
    try { v = fmt(x[ks[i]], d + 1); } catch (e) { v = "throws"; }
    s += ks[i] + ":" + v + ",";
  }
  return s + "}";
}
function log(x) { if (LOG.length < 4000) LOG.push(fmt(x)); }
function err(e) { return e && e.name ? "E:" + e.name + ":" + String(e.message).slice(0, 60) : "E?:" + fmt(e); }
var SIDES = 0;
var P0 = { inh: 7, im: function () { return this.x; }, z: "pz" };
var P1 = { inh: "eight", im: function () { return 8; } };
class C0 {
  constructor(v) { this.v = v; this.w = 1; }
  get g() { side(this.w); return this.v; }
  set s(x) { side(3); this.v = x; }
  m(a) { return this.v + a; }
  static st(a) { return a; }
}
class C1 extends C0 {
  constructor(v) { super(v); this.k = 2; }
  m(a) { return super.m(a) + 1; }
}
function mk() {
  return [
    { x: 1 }, { x: 1.5, y: 2 }, { y: "a", x: 2 }, Object.create(P0), [1, 2, 3], new C0(3), new C1(4),
    [1.5, , "s", null], { x: { x: 9 }, length: 3 }, Object.assign(Object.create(P1), { x: 5 }),
    (function () { return arguments; })(1, 2, 3), new Float64Array([1.5, 2.5, 3.5]), "str", 42, null, undefined,
    { get x() { side(5); return 11; }, set x(v) { side(6); } }, new Proxy({ x: 1 }, {}), function fo(a) { return a; },
  ];
}
var OB = mk();
var G0 = 1, G1 = "s", G2 = { x: 1, y: 2 }, G3 = [1, 2, 3], G4 = 2.5;
let L0 = 10, L1 = { x: "lx" };
function side(k) {
  SIDES++;
  switch ((k >>> 0) % 24) {
    case 0: delete G2.x; break;
    case 1: G2.x = SIDES; break;
    case 2: G2["k" + (SIDES % 5)] = SIDES; break;
    case 3: Object.setPrototypeOf(OB[0], SIDES % 2 ? P0 : P1); break;
    case 4: P0.inh = SIDES; break;
    case 5: delete P0.inh; break;
    case 6: Object.defineProperty(P0, "inh", { get: function () { return "acc" + SIDES; }, configurable: true }); break;
    case 7: G3.length = SIDES % 4; break;
    case 8: G3.push(SIDES, SIDES + 0.5); break;
    case 9: G3[SIDES % 7] = "e" + SIDES; break;
    case 10: globalThis.GX = SIDES; break;
    case 11: delete globalThis.GX; break;
    case 12: globalThis["GY" + (SIDES % 9)] = SIDES; break;
    case 13: OB[1].x = "now a string"; break;
    case 14: OB[4][50] = 1; break;
    case 15: OB = mk(); break;
    case 16: Object.freeze(OB[2]); break;
    case 17: P1.x = SIDES; break;
    case 18: Object.defineProperty(globalThis, "GZ", { get: function () { return SIDES; }, configurable: true }); break;
    case 19: delete globalThis.GZ; break;
    case 20: G0 = SIDES % 3 ? SIDES : "g0s"; break;
    case 21: OB[5].w = SIDES; OB[5].extra = 1; break;
    case 22: G4 = SIDES % 2 ? { valueOf: function () { return SIDES; } } : 2.5; break;
    case 23: Object.defineProperty(OB[0], "x", { value: SIDES, writable: false, configurable: true }); break;
  }
  return SIDES;
}
`;

const NUMS = ["0", "1", "2", "3", "7", "-1", "0.5", "-0", "NaN", "Infinity", "2147483647", "2147483648", "4294967295", "4294967296", "-2147483649", "9007199254740993", "1e21", "1e-7", "255", "31", "32", "33"];
const OTHER = ['"s"', '""', '"12"', "null", "undefined", "true", "false", "1n", "{}", "[]", "[1,2]", "Symbol.iterator"];
const BIN = ["+", "-", "*", "/", "%", "**", "<<", ">>", ">>>", "&", "|", "^", "==", "!=", "===", "!==", "<", "<=", ">", ">="];
const GLOBALS = ["G0", "G1", "G2", "G3", "G4", "L0", "L1", "GX", "GZ", "GY3"];
const PROPS = ["x", "y", "v", "w", "inh", "length", "g", "z", "k"];

function genFunc(R, idx, nfuncs) {
  const locals = ["a", "b", "c", "d"];
  const params = ["p", "q", "r"];
  const loopVars = [];
  let depth = 0;
  let stmts = 0;
  const name = "f" + idx;
  const v = () => R.pick(R.chance(0.6) ? locals : loopVars.length && R.chance(0.6) ? loopVars : params);
  const obj = () => (R.chance(0.5) ? `OB[${R(19)}]` : R.chance(0.5) ? v() : R.pick(["G2", "G3", "L1", "this"]));
  function expr(d) {
    if (d > 3 || R.chance(0.25)) {
      const k = R(10);
      if (k < 4) return v();
      if (k < 7) {
        // Parenthesized when negative: `-1 ** x` and `--1` are SyntaxErrors (the review's
        // version emitted them); elsewhere `(-1)` parses to the same tree as `-1`.
        const lit = R.pick(NUMS);
        return lit[0] === "-" ? `(${lit})` : lit;
      }
      if (k < 8) return R.pick(OTHER);
      if (k < 9) return R.pick(GLOBALS);
      return obj();
    }
    const k = R(26);
    if (k < 9) return `(${expr(d + 1)} ${R.pick(BIN)} ${expr(d + 1)})`;
    if (k < 10) return `(${R.pick(["-", "+", "!", "~", "typeof "])}${expr(d + 1)})`;
    if (k < 13) return `${obj()}.${R.pick(PROPS)}`;
    if (k < 15) return `${obj()}[${R.chance(0.6) ? (loopVars.length ? R.pick(loopVars) : R(5)) : expr(d + 1)}]`;
    if (k < 18 && idx > 0) {
      const j = R(idx);
      const n = R(5);
      const args = Array.from({ length: n }, () => expr(d + 2)).join(", ");
      const form = R(8);
      if (form === 0) return `f${j}.call(${expr(d + 2)}${n ? ", " + args : ""})`;
      if (form === 1) return `f${j}.apply(${obj()}, [${args}])`;
      if (form === 2) return `new f${j}(${args})`;
      return `f${j}(${args})`;
    }
    if (k < 19) return `${name}((p | 0) - 1 - ${R(3)}, ${expr(d + 2)})`;
    if (k < 20) return `${obj()}.m(${expr(d + 1)})`;
    if (k < 21) return `(${expr(d + 1)} ? ${expr(d + 1)} : ${expr(d + 1)})`;
    if (k < 22) return `(() => ${v()} ${R.pick(BIN)} ${expr(d + 2)})()`;
    if (k < 23) return R.pick(["arguments.length", "arguments[0]", "arguments[1]", "this", "(typeof this)", "new C0(" + v() + ")", "new C1(" + v() + ")", "C0.st(" + v() + ")", "side(" + R(24) + ")"]);
    if (k < 24) return `(${v()} = ${expr(d + 1)})`;
    if (k < 25) return `(${v()}${R.pick(["++", "--"])})`;
    return `(${expr(d + 1)} ${R.pick(["&&", "||", "??"])} ${expr(d + 1)})`;
  }
  function block(n) {
    let s = "";
    for (let i = 0; i < n; i++) s += stmt();
    return s;
  }
  function stmt() {
    stmts++;
    const ind = "  ".repeat(depth + 1);
    if (stmts > 40 || depth > 3) return `${ind}${v()} = ${expr(2)};\n`;
    const k = R(32);
    if (k < 8) return `${ind}${v()} = ${expr(0)};\n`;
    if (k < 11) return `${ind}${v()} ${R.pick(["+=", "-=", "*=", "|=", "&=", ">>>="])} ${expr(1)};\n`;
    if (k < 15) {
      const iv = "i" + loopVars.length;
      const n = R.pick([2, 3, 5, 8, 13, 40]);
      loopVars.push(iv);
      depth++;
      const body = block(1 + R(4));
      depth--;
      loopVars.pop();
      const head = R.chance(0.8)
        ? `for (var ${iv} = 0; ${iv} < ${n}; ${iv}++) {`
        : `for (var ${iv} = ${n}; ${iv} > 0; ${iv} -= ${R.pick([1, 2, 0.5])}) {`;
      return `${ind}${head}\n${ind}  tick();\n${body}${ind}}\n`;
    }
    if (k < 18) {
      depth++;
      const a = block(1 + R(3));
      const b = R.chance(0.5) ? block(1 + R(2)) : null;
      depth--;
      return `${ind}if (${expr(1)}) {\n${a}${ind}}${b ? ` else {\n${b}${ind}}` : ""}\n`;
    }
    if (k < 23) {
      depth++;
      const t = block(1 + R(4));
      const c = block(1 + R(2));
      const f = R.chance(0.4) ? block(1 + R(2)) : null;
      depth--;
      const useCatch = !f || R.chance(0.8);
      return (
        `${ind}try {\n${t}${ind}}` +
        (useCatch ? ` catch (e) {\n${ind}  log(err(e)); log([a, b, c, d]);\n${c}${ind}}` : "") +
        (f ? ` finally {\n${ind}  log([a, b, c, d]);\n${f}${ind}}` : "") +
        "\n"
      );
    }
    if (k < 25) return `${ind}${obj()}.${R.pick(PROPS)} = ${expr(1)};\n`;
    if (k < 27) return `${ind}${obj()}[${loopVars.length ? R.pick(loopVars) : R(5)}] = ${expr(1)};\n`;
    if (k < 28) return `${ind}if (${expr(2)}) throw ${R.chance(0.5) ? "new Error(" + expr(2) + ")" : expr(2)};\n`;
    if (k < 29) return loopVars.length ? `${ind}if (${expr(2)}) ${R.pick(["break", "continue"])};\n` : `${ind}log(${v()});\n`;
    if (k < 30) return `${ind}if (${expr(2)}) return ${expr(1)};\n`;
    if (k < 31) return `${ind}${R.pick(GLOBALS.slice(0, 7))} = ${expr(1)};\n`;
    return `${ind}log(${expr(1)});\n`;
  }
  const strict = R.chance(0.3) ? '  "use strict";\n' : "";
  const decl = R.pick([
    "  var a, b, c, d;\n",
    "  var a = 0, b = 1.5, c, d = p;\n",
    "  var a = p, b = q, c = 0, d;\n",
    "  let a = 0, b = 0, c = 0, d = 0;\n",
    "  var a = 0, b; let c = OB[1], d;\n",
  ]);
  const guard = "  if ((p | 0) < -3) return q;\n  tick();\n";
  const body = block(3 + R(6));
  const ret = `  return ${R.chance(0.5) ? "[a, b, c, d]" : expr(1)};\n`;
  const dflt = R.chance(0.15) ? "q = 5" : "q";
  // "use strict" with a default parameter is a SyntaxError, which the review's version emitted in
  // about a quarter of its programs (they tested nothing). Dropped here without changing what the
  // generator draws, so every program that ran then is still the same program.
  const prologue = dflt === "q" ? strict : "";
  return `function ${name}(p, ${dflt}, r) {\n${prologue}${decl}${guard}${body}${ret}}\n`;
}

export function generate(seed) {
  const R = rng(seed);
  const n = 3 + R(5);
  let src = HEADER;
  for (let i = 0; i < n; i++) src += genFunc(R, i, n);
  const ARGS = ["1", "2", "0.5", '"s"', "null", "undefined", "OB[0]", "OB[4]", "OB[5]", "-0", "1n", "true", "G3", "3", "7", "NaN"];
  src += "var RES = [];\n";
  src += `for (var round = 0; round < ${2 + R(4)}; round++) {\n`;
  for (let i = 0; i < n; i++) {
    const calls = 1 + R(3);
    for (let c = 0; c < calls; c++) {
      const na = R(5);
      const args = Array.from({ length: na }, () => R.pick(ARGS)).join(", ");
      const how = R(6);
      const call = how === 0 ? `f${i}.call(${R.pick(ARGS)}${na ? ", " + args : ""})` : how === 1 ? `new f${i}(${args})` : `f${i}(${args})`;
      src += `  STEPS = 0; try { log(${call}); } catch (e) { log(err(e)); }\n`;
    }
    if (R.chance(0.3)) src += `  side(${R(24)});\n`;
  }
  src += "}\n";
  src += 'try { log([G0, G1, G2, G3, G4, L0, L1, SIDES]); } catch (e) { log(err(e)); }\n';
  src += 'console.log(LOG.join("\\n"));\n';
  return src;
}
