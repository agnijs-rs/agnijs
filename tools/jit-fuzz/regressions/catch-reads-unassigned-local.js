// jit-fuzz: node-safe
// TASK-103.34, the review's reproduction: a register guessed to be a number, not yet assigned,
// read in a catch handler after a throw from an instruction with known register access.
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
console.log(f(10, null));
console.log(f(10, {}));
console.log(f(10, null));
