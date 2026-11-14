// The review's hand-written probes of direct calls and inline caches: class constructors without
// new, this modes, closures, arity, recursion through try/catch, callee replacement, deep
// recursion, getters that reshape, frozen and sealed arrays and objects, prototype edits.
var out = [];
function log(x) { out.push(String(x)); }

// 1. class constructor called without new after having been constructed from compiled code
class K { constructor() { this.a = 1; } }
function mkK(i) { return i % 2 ? new K() : 0; }
function callK() { return K(); }
for (var i = 0; i < 6; i++) mkK(i);
for (var i = 0; i < 4; i++) { try { callK(); log("no throw"); } catch (e) { log(e.name); } }

// 2. this modes through direct calls
function sloppy() { return typeof this + (this === globalThis); }
function strict() { "use strict"; return typeof this + String(this); }
var arrow = () => typeof this;
function callAll(t) { return [sloppy.call(t), strict.call(t), sloppy(), strict(), arrow()].join(","); }
var o = { sloppy: sloppy, strict: strict, m() { return [this.sloppy(), this.strict()].join(","); } };
for (var i = 0; i < 5; i++) { log(callAll(i % 2 ? 5 : undefined)); log(o.m()); }
function viaProp(x) { return x.sloppy() + x.strict(); }
for (var i = 0; i < 5; i++) { log(viaProp(o)); }
var prim = 7;
Number.prototype.sloppy = sloppy; Number.prototype.strict = strict;
function onPrim(x) { return x.sloppy() + "|" + x.strict(); }
for (var i = 0; i < 5; i++) log(onPrim(prim));
for (var i = 0; i < 5; i++) log(onPrim("s".length));

// 3. closures of one code block with different environments, called directly
function mkAdder(n) { return function (x) { return x + n; }; }
var add1 = mkAdder(1), add10 = mkAdder(10);
function useAdders(i) { return add1(i) * 100 + add10(i); }
for (var i = 0; i < 6; i++) log(useAdders(i));

// 4. arity: fewer and more arguments than parameters, defaults
function three(a, b, c) { return [a, b, c].join("/"); }
function dflt(a, b = a + 1) { return a + ":" + b; }
function arity(i) { return three(i) + " " + three(i, 2, 3, 4, 5) + " " + dflt(i) + " " + dflt(i, undefined) + " " + dflt(i, null); }
for (var i = 0; i < 5; i++) log(arity(i));

// 5. recursion through direct calls with a try/catch in the middle
function rec(n) { if (n == 0) throw new Error("bottom"); try { return rec(n - 1) + 1; } catch (e) { if (n % 3) throw e; return n; } }
for (var i = 0; i < 12; i++) { try { log(rec(i)); } catch (e) { log("E" + i); } }

// 6. a function replaced by another with the same shape of code
var fnv = function (x) { return x + 1; };
function callFnv(i) { return fnv(i); }
for (var i = 0; i < 5; i++) log(callFnv(i));
fnv = function (x) { return x + 2; };
for (var i = 0; i < 5; i++) log(callFnv(i));
fnv = Math.abs; for (var i = 0; i < 3; i++) log(callFnv(-i));
fnv = add10.bind(null); for (var i = 0; i < 3; i++) log(callFnv(i));
fnv = new Proxy(add1, {}); for (var i = 0; i < 3; i++) log(callFnv(i));
fnv = 5; for (var i = 0; i < 2; i++) { try { callFnv(i); } catch (e) { log(e.name); } }

// 7. deep recursion: overflow is reported alike
function deep(n) { return n == 0 ? 0 : 1 + deep(n - 1); }
log(deep(5000));
try { deep(1e6); log("no overflow"); } catch (e) { log(e.name); }
log(deep(100));

// 8. getters with side effects on the receiver's shape and on arrays being indexed
var arr = [1, 2, 3, 4, 5, 6, 7, 8];
var sneaky = { get v() { arr.length = 2; delete this.w; this.z = 1; return 1; }, w: 5 };
function useSneaky(i) { var a = arr; var s = sneaky; var t = 0; t += a[5] | 0; t += s.v; t += (a[5] === undefined) ? 100 : 0; t += s.w === undefined ? 1000 : 0; return t; }
for (var i = 0; i < 4; i++) { log(useSneaky(i)); arr = [1, 2, 3, 4, 5, 6, 7, 8]; sneaky.w = 5; }

// 9. frozen and sealed arrays, non-writable elements
var fr = Object.freeze([1, 2, 3]);
function wr(a, i, v) { a[i] = v; return a[i]; }
for (var i = 0; i < 5; i++) log(wr(fr, 1, 99));
var nw = [1, 2, 3]; Object.defineProperty(nw, 1, { value: 7, writable: false });
for (var i = 0; i < 5; i++) log(wr(nw, 1, 99));
var se = Object.seal([1, 2, 3]);
for (var i = 0; i < 3; i++) log(wr(se, 1, 50 + i));
function wrs(a, i, v) { "use strict"; a[i] = v; return a[i]; }
for (var i = 0; i < 3; i++) { try { log(wrs(fr, 1, 99)); } catch (e) { log(e.name); } }

// 10. frozen objects and non-writable named properties
var fo = Object.freeze({ x: 1 });
function wn(o, v) { o.x = v; return o.x; }
for (var i = 0; i < 5; i++) log(wn({ x: 0 }, i));
for (var i = 0; i < 5; i++) log(wn(fo, i));
var nwo = { x: 1 }; for (var i = 0; i < 3; i++) log(wn(nwo, i));
Object.defineProperty(nwo, "x", { writable: false }); for (var i = 0; i < 3; i++) log(wn(nwo, i + 10));

// 11. prototype chain edits between hits
var gp = { q: "gp" }, par = Object.create(gp), kid = Object.create(par);
function rq(o) { return o.q; }
for (var i = 0; i < 4; i++) log(rq(kid));
par.q = "par"; for (var i = 0; i < 2; i++) log(rq(kid));
delete par.q; for (var i = 0; i < 2; i++) log(rq(kid));
gp.q = "gp2"; for (var i = 0; i < 2; i++) log(rq(kid));
Object.setPrototypeOf(par, { q: "other" }); for (var i = 0; i < 2; i++) log(rq(kid));
Object.defineProperty(gp, "q", { get() { return "getter"; } }); Object.setPrototypeOf(par, gp); for (var i = 0; i < 2; i++) log(rq(kid));
kid.q = "own"; for (var i = 0; i < 2; i++) log(rq(kid));

console.log(out.join("\n"));
