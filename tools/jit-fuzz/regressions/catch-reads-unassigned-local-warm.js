// jit-fuzz: node-safe
// TASK-103.34 under AGNIJS_JIT=on: the function is compiled while it does not throw, then the
// first iteration throws.
function f(n, o) {
  var x;
  var y = 0;
  try {
    for (var i = 0; i < n; i++) {
      o.p;
      x = i;
      y = x + 1;
    }
  } catch (e) {
    return typeof x + ":" + x;
  }
  return "no throw " + y;
}
var ok = {};
var last;
for (var k = 0; k < 3000; k++) last = f(50, ok);
console.log(last);
var seen = {};
for (var k = 0; k < 20; k++) {
  var r = f(50, null);
  seen[r] = (seen[r] || 0) + 1;
}
console.log(JSON.stringify(seen));
