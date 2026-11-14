// Second generator for the JIT review: mostly non-throwing numeric code with typed local pools,
// so that loops get compiled with number speculation and direct calls, plus the events that can
// break a guess between guard and use: a throw caught in the same function (locals read in the
// handler), a local that changes type, a first assignment that is skipped, getters and callees
// with side effects, and finally blocks. With `warm` every function is first run hot with benign
// arguments so that AGNIJS_JIT=on compiles it.
//
// Generator 2 of docs/jit-review-2026-11.md, kept byte-for-byte: a seed gives the program it gave
// in the review (`GEN_WARM=1` there is `{ warm: true }` here). Its programs log error names only,
// so Node can serve as an outside oracle.
import { rng } from "./gen-mixed.mjs";

export const nodeSafe = true;
export const hasWarm = true;

let WARM = false;

const header = () => `
var H = 0, LOG = [], QUIET = false, STEPS = 0;
function hs(s) { var h = 7; for (var i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0; return h; }
function fmt(x, d) {
  var t = typeof x;
  if (t === "number") return Object.is(x, -0) ? "-0" : String(x);
  if (t === "string") return '"' + x + '"';
  if (t === "undefined" || x === null || t === "boolean") return String(x);
  if (t === "function") return "fn";
  if (t !== "object") return t;
  d = d | 0;
  if (d > 1) return "obj";
  var s, i;
  if (Array.isArray(x)) { s = "["; for (i = 0; i < x.length && i < 8; i++) s += fmt(x[i], d + 1) + ","; return s + "]#" + x.length; }
  var ks = Object.keys(x); s = "{";
  for (i = 0; i < ks.length && i < 6; i++) s += ks[i] + ":" + fmt(x[ks[i]], d + 1) + ",";
  return s + "}";
}
function log(x) { if (QUIET) { H = (H + 1) | 0; return; } var s = fmt(x); H = (H * 33 + hs(s)) | 0; if (!QUIET && LOG.length < 1500) LOG.push(s); }
function err(e) { return e && e.name ? "E:" + e.name : "E?"; }
var CNT = 0, BUDGET = ${WARM ? 1200 : 2500};
var OK = { x: 1, y: 2.5, n: 3, f: function (a) { return a + 1; }, arr: [1, 2, 3, 4, 5, 6, 7, 8] };
var OK2 = { y: 1, x: "sx", n: 4, f: function (a) { return a * 2; }, arr: [0.5, 1.5, 2.5] };
var PR = Object.create(OK); PR.own = 1;
var GET = { get x() { CNT++; if (CNT % 7 == 0) throw new Error("getter"); return CNT; }, y: 1, n: 2, f: function (a) { return a; }, arr: [9] };
var G_NUM = 3, G_ANY = 1;
let L_NUM = 4;
function reset() { OK.x = 1; OK.y = 2.5; OK.n = 3; OK2.y = 1; OK2.x = "sx"; OK2.n = 4; G_ANY = 1; G_NUM = 3; L_NUM = 4; OK.arr.length = 8; }
function ident(a) { return a; }
function inc(a) { return a + 1; }
function maybeThrow(a) { if (a === 3 || a === null) throw new RangeError("mt"); return a; }
function catching(a) { var r = 0; try { r = maybeThrow(a) + 1; } catch (e) { r = -5; } return r; }
function getx(o) { return o.x; }
function sum3(a, b, c) { return a + b + c; }
function viaThis() { return this === undefined ? -1 : this.n; }
function swap() { var t = OK.x; OK.x = OK2.x; OK2.x = t; G_ANY = typeof G_ANY === "number" ? "s" : 1; return 0; }
function recur(n, a) { if (n <= 0) return a; try { return recur(n - 1, a + n) + (n === 2 ? maybeThrow(a) : 0); } catch (e) { return -n; } }
`;

const HELPERS = ["ident", "inc", "catching", "sum3", "recur"];

function genFunc(R, idx) {
  const N = ["n0", "n1", "n2", "n3", "n4"];
  const O = ["o0", "o1"];
  const loopVars = [];
  let depth = 0, stmts = 0;
  const name = "t" + idx;
  const num = (d = 0) => {
    const k = R(d > 2 ? 6 : 16);
    if (k < 3) return R.pick(N);
    if (k < 5) return R.pick(["0", "1", "2", "3", "0.5", "-1", "7", "100", "1e9", "-0.25"]);
    if (k < 6) return loopVars.length ? R.pick(loopVars) : "p";
    if (k < 10) return `(${num(d + 1)} ${R.pick(["+", "-", "*", "+", "-", "%", "|", "&", "^", "<<", ">>", ">>>", "/"])} ${num(d + 1)})`;
    if (k < 11) return `${R.pick(O)}.${R.pick(["x", "y", "n"])}`;
    if (k < 12) return `${R.pick(O)}.arr[${loopVars.length ? R.pick(loopVars) : R(4)}]`;
    if (k < 13) {
      const h = R.pick(HELPERS);
      if (h === "sum3") return `sum3(${num(d + 1)}, ${num(d + 1)}, ${num(d + 1)})`;
      if (h === "recur") return `recur(${R(5)}, ${num(d + 1)})`;
      return `${h}(${num(d + 1)})`;
    }
    if (k < 14) return R.pick(["G_NUM", "L_NUM", "G_ANY", "p", "q"]);
    if (k < 15 && idx > 0) return `t${R(idx)}(${num(d + 1)}, ${R.pick(O)}, ${R.pick(["0", "1", "q"])})`;
    return R.pick([`${R.pick(O)}.f(${num(d + 1)})`, `getx(${R.pick(O)})`, `viaThis.call(${R.pick(O)})`, `maybeThrow(${num(d + 1)})`, `(() => ${R.pick(N)} + 1)()`, "swap()"]);
  };
  const cond = () => `${num(1)} ${R.pick(["<", "<=", ">", ">=", "==", "!=", "===", "!=="])} ${num(1)}`;
  function block(n) { let s = ""; for (let i = 0; i < n; i++) s += stmt(); return s; }
  function stmt() {
    stmts++;
    const ind = "  ".repeat(depth + 1);
    if (stmts > 34 || depth > 3) return `${ind}${R.pick(N)} = ${num(1)};\n`;
    const k = R(30);
    if (k < 9) return `${ind}${R.pick(N)} = ${num()};\n`;
    if (k < 11) return `${ind}${R.pick(N)} ${R.pick(["+=", "-=", "*=", "|="])} ${num(1)};\n`;
    if (k < 16) {
      const iv = "i" + loopVars.length;
      const n = WARM ? R.pick([3, 8, 30]) : R.pick([2, 3, 6, 12]);
      loopVars.push(iv); depth++;
      const body = block(1 + R(4));
      depth--; loopVars.pop();
      return `${ind}for (var ${iv} = 0; ${iv} < ${n}; ${iv}++) {\n${ind}  if (++STEPS > BUDGET) break;\n${body}${ind}}\n`;
    }
    if (k < 18) {
      depth++;
      const a = block(1 + R(2));
      const b = R.chance(0.4) ? block(1 + R(2)) : null;
      depth--;
      return `${ind}if (${cond()}) {\n${a}${ind}}${b ? ` else {\n${b}${ind}}` : ""}\n`;
    }
    if (k < 23) {
      depth++;
      const t = block(1 + R(4));
      const c = R.chance(0.5) ? block(1) : "";
      const f = R.chance(0.35) ? block(1) : null;
      depth--;
      const useCatch = !f || R.chance(0.8);
      return `${ind}try {\n${t}${ind}}` +
        (useCatch ? ` catch (e) {\n${ind}  log(err(e)); log([n0, n1, n2, n3, n4]);\n${c}${ind}}` : "") +
        (f ? ` finally {\n${ind}  log([n0, n1, n2, n3, n4]);\n${f}${ind}}` : "") + "\n";
    }
    if (k < 24) return `${ind}${R.pick(N)} = ${R.pick(['"s"', "undefined", "null", "o0", "true", "[1]", "{}", "o1.x", "undefined", "null", "true", "o1"])};\n`;
    if (k < 25) return `${ind}${R.pick(O)}.${R.pick(["x", "y", "n", "z"])} = ${num(1)};\n`;
    if (k < 26) return `${ind}${R.pick(O)}.arr[${loopVars.length ? R.pick(loopVars) : R(4)}] = ${num(1)};\n`;
    if (k < 27) return `${ind}if (${cond()}) throw new Error("t");\n`;
    if (k < 28) return loopVars.length ? `${ind}if (${cond()}) ${R.pick(["break", "continue"])};\n` : `${ind}log(${R.pick(N)});\n`;
    if (k < 29) return `${ind}if (${cond()}) return ${num(1)};\n`;
    return `${ind}${R.pick(O)} = ${R.pick(["o", "OK", "OK2", "PR", "GET", "o"])};\n`;
  }
  const decl = "  var " + N.map((n) => (R.chance(0.45) ? n : `${n} = ${R.pick(["0", "1", "p", "0.5", "q"])}`)).join(", ") + ";\n" +
    `  var o0 = o, o1 = ${R.pick(["OK", "OK2", "o", "PR"])};\n`;
  const strict = R.chance(0.3) ? '  "use strict";\n' : "";
  return `function ${name}(p, o, q) {\n${strict}  if (++STEPS > BUDGET) return -1;\n${decl}${block(3 + R(5))}  return [n0, n1, n2, n3, n4];\n}\n`;
}

export function generate(seed, opts = {}) {
  WARM = !!opts.warm;
  const R = rng(seed);
  const n = 2 + R(4);
  let src = header();
  for (let i = 0; i < n; i++) src += genFunc(R, i);
  const SAFE_O = ["OK", "OK2", "PR"];
  const ANY_O = ["OK", "OK2", "PR", "GET", "null", "undefined", "5", '"s"', "{}"];
  const P = ["0", "1", "2", "3", "7", "0.5", '"s"', "undefined", "null"];
  const call = (i, o, p) => `STEPS = 0; reset(); try { log(t${i}(${p}, ${o}, ${R.pick(["0", "1", "2"])})); } catch (e) { log(err(e)); }`;
  if (WARM) {
    src += "QUIET = true;\n";
    for (let i = 0; i < n; i++) {
      src += `for (var w = 0; w < 1300; w++) { ${call(i, R.pick(SAFE_O), R.pick(["1", "2", "7", "w"]))} }\n`;
    }
    src += "QUIET = false;\n";
  }
  src += `for (var round = 0; round < ${WARM ? 3 : 4}; round++) {\n`;
  for (let i = 0; i < n; i++) {
    const calls = 1 + R(3);
    for (let c = 0; c < calls; c++) src += `  ${call(i, R.pick(R.chance(0.5) ? SAFE_O : ANY_O), R.pick(P))}\n`;
  }
  src += "}\n";
  src += 'log([OK, OK2, G_NUM, G_ANY, L_NUM, CNT]);\nconsole.log(LOG.join("\\n"));\nconsole.log("H=" + H);\n';
  return src;
}
