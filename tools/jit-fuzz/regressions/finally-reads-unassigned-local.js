// jit-fuzz: node-safe
// TASK-103.34 through finally, a callee that throws, and a stale heap value left in the
// register's slot by an earlier frame.
function thrower(o) { return o.p; }
var seen;
function fin(n, o) {
  var x;
  var y = 0;
  try {
    for (var i = 0; i < n; i++) { thrower(o); x = i; y = x + 1; }
  } finally {
    seen = typeof x + ":" + x;
  }
  return y;
}
try { fin(10, null); } catch (e) {}
console.log(seen);
function viaCall(n, o) {
  var x;
  var y = 0;
  try {
    for (var i = 0; i < n; i++) { thrower(o); x = i; y = x + 1; }
  } catch (e) { return typeof x + ":" + x; }
  return y;
}
console.log(viaCall(10, null));
// A string-building frame first, so the slot's stale payload is a heap id.
function dirty() { var a = "a" + 1, b = { q: 1 }, c = [1, 2], d = a + b; return d.length; }
function after(n, o) {
  var x;
  var y = 0;
  try {
    for (var i = 0; i < n; i++) { o.p; x = i; y = x + 1; }
  } catch (e) { return typeof x; }
  return y;
}
dirty();
console.log(after(10, null));
