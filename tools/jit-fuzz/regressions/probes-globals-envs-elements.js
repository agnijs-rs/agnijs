// The review's hand-written probes of global sites, environment slots (with, eval, per-iteration
// bindings, TDZ), dense and odd elements, stack text, and number speculation at the edges.
var out = [];
function log(x) { out.push(String(x)); }

// 1. a compiled loop reads a global while a callee grows and shrinks the global object
var G = 5;
function grow(i) { globalThis["dyn" + i] = i; if (i % 3 == 0) delete globalThis["dyn" + (i - 3)]; return 0; }
function readG(n) { var s = 0; for (var i = 0; i < n; i++) { s += G; grow(i); s += G; } return s; }
log(readG(300)); log(readG(300));
function rw(n) { var s = 0; for (var i = 0; i < n; i++) { G = i; grow(1000 + i); s += G; } return s; }
log(rw(200));
// accessor installed over a cached global, then removed
globalThis.GG = 6;
function rg() { return GG; }
for (var i = 0; i < 4; i++) log(rg());
Object.defineProperty(globalThis, "GG", { get: function () { return "acc"; }, configurable: true });
for (var i = 0; i < 2; i++) log(rg());
delete globalThis.GG;
for (var i = 0; i < 2; i++) { try { log(rg()); } catch (e) { log(e.name); } }
globalThis.GG = "back"; for (var i = 0; i < 2; i++) log(rg());
function ty() { return typeof NOPE + typeof G; }
for (var i = 0; i < 3; i++) log(ty());
// lexical in its dead zone, read by a function compiled earlier
function rl() { return LX; }
for (var i = 0; i < 3; i++) { try { log(rl()); } catch (e) { log(e.name); } }
let LX = 9;
for (var i = 0; i < 3; i++) log(rl());
LX = "nine"; log(rl());

// 2. environment slots: per-iteration bindings, captured and written from callees
function envs() {
  var fs = [], t = 0;
  for (let i = 0; i < 6; i++) { fs.push(function () { return i++; }); t += i; }
  var r = [];
  for (var k = 0; k < fs.length; k++) { r.push(fs[k]()); r.push(fs[k]()); }
  return r.join(",") + ":" + t;
}
for (var i = 0; i < 3; i++) log(envs());
function counter() { var c = 0; function inc() { c++; return c; } function get() { return c; } return [inc, get]; }
function useCounter() { var p = counter(), s = 0; for (var i = 0; i < 10; i++) { p[0](); s += p[1](); } return s; }
for (var i = 0; i < 3; i++) log(useCounter());
function tdz() { var f = function () { return z; }; try { f(); } catch (e) { log(e.name); } let z = 4; return f(); }
for (var i = 0; i < 3; i++) log(tdz());
function withEnv(o) { var q = 1; with (o) { var f = function () { return q; }; } return f(); }
for (var i = 0; i < 3; i++) log(withEnv(i % 2 ? { q: "with" } : {}));
function evalEnv(s) { var v = 1; var f = function () { return v; }; eval(s); return f() + ":" + v; }
for (var i = 0; i < 3; i++) log(evalEnv(i % 2 ? "var v = 2" : ""));

// 3. elements
function sumArr(a, n) { var s = 0; for (var i = 0; i < n; i++) { var x = a[i]; s += x === undefined ? 1000 : x; } return s; }
log(sumArr([1, 2, 3, 4], 6));
log(sumArr([1, , 3, 4], 4));
var proto = [10, 20, 30, 40, 50, 60]; var holey = [1, , 3]; Object.setPrototypeOf(holey, proto);
log(sumArr(holey, 5));
var sparse = [1, 2, 3]; sparse[5000] = 7; log(sumArr(sparse, 5));
log(sumArr(new Float64Array([0.5, 1.5]), 3));
log(sumArr(new Uint8Array([250, 5]), 3));
log(sumArr("123", 2) );
log(sumArr((function () { return arguments; })(4, 5, 6), 4));
function shrinker(a) { var s = 0; for (var i = 0; i < 8; i++) { if (i == 3) a.length = 2; if (i == 5) a.push(7, 8, 9, 10, 11); s += a[i] === undefined ? 100 : a[i]; } return s; }
for (var i = 0; i < 3; i++) log(shrinker([1, 2, 3, 4, 5, 6, 7, 8]));
function wrIdx(a, k, v) { a[k] = v; return a.length + ":" + a[k]; }
var w = [1, 2, 3];
log(wrIdx(w, 3, "app")); log(wrIdx(w, 10, "gap")); log(wrIdx(w, -1, "neg")); log(wrIdx(w, 1.5, "frac")); log(wrIdx(w, 4294967294, "big")); log(wrIdx(w, 4294967295, "max")); log(wrIdx(w, "2", "str")); log(wrIdx(w, -0, "nz"));
var ta = new Int8Array(4); log(wrIdx(ta, 1, 300)); log(wrIdx(ta, 9, 1));
function idx(a, k) { return a[k]; }
log(idx([1, 2, 3], -0)); log(idx([1, 2, 3], NaN)); log(idx([1, 2, 3], 2.0000000001)); log(idx([1, 2, 3], 4294967296)); log(idx([1, 2, 3], Infinity));
var getterArr = [1, 2, 3]; Object.defineProperty(getterArr, 1, { get: function () { return "g"; } });
for (var i = 0; i < 3; i++) log(idx(getterArr, 1) + idx(getterArr, 0));
var len = [1, 2, 3];
function lens(a) { return a.length; }
log(lens(len)); len.length = 1; log(lens(len)); log(lens("four")); log(lens({ length: "obj" })); log(lens(new Uint8Array(7))); log(lens((function () { return arguments; })(1, 2)));

// 4. stacks: the same text in every tier
function thrower(n) { if (n == 0) throw new Error("deep"); return thrower(n - 1) + 1; }
function stackOf(n) { try { thrower(n); } catch (e) { return e.stack.split("\n").length + "|" + e.stack.split("\n").slice(0, 3).join(";"); } }
for (var i = 0; i < 4; i++) log(stackOf(i));
function nullProp(o) { return o.a.b; }
for (var i = 0; i < 3; i++) { try { nullProp(i == 2 ? {} : { a: { b: i } }); log("ok"); } catch (e) { log(e.message + "|" + e.stack.split("\n").slice(0, 2).join(";")); } }

// 5. speculation: values at the edges, a local that changes type and changes back
function edges(a, b) { var r = []; for (var i = 0; i < 2; i++) { r.push(a + b, a - b, a * b, a / b, a % b, a ** b, a << b, a >> b, a >>> b, a & b, a | b, a ^ b, a < b, a <= b, a == b, a === b, a != b, -a, ~a); } return r.join(" "); }
var E = [0, -0, 1, -1, 0.5, 2147483647, 2147483648, -2147483648, -2147483649, 4294967295, 4294967296, 9007199254740992, 1e300, -1e300, Infinity, -Infinity, NaN, 31, 32, 33, 63, 64, 1e-320];
var h = 0;
for (var i = 0; i < E.length; i++) for (var j = 0; j < E.length; j++) { var s = edges(E[i], E[j]); for (var k = 0; k < s.length; k++) h = (h * 31 + s.charCodeAt(k)) | 0; }
log("edges " + h);
function flip(n) { var x = 0, s = ""; for (var i = 0; i < n; i++) { x = x + 1; if (i % 7 == 3) x = "s" + i; if (i % 7 == 5) x = i; if (i % 11 == 0) x = { valueOf: function () { return 4; } }; s += typeof x === "object" ? "o" : x; s += ","; } return s; }
log(flip(40)); log(flip(40));
function negz(n) { var z = 0; for (var i = 0; i < n; i++) { z = z * -1; } return 1 / z; }
log(negz(3)); log(negz(4));
function nanl(n) { var c = 0, x = NaN; for (var i = 0; i < n; i++) { if (x == x) c++; if (!(x < i)) c += 10; if (x != x) c += 100; x = i % 2 ? NaN : i; } return c; }
log(nanl(9));

console.log(out.join("\n"));
