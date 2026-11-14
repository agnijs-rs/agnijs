// jit-fuzz: node-safe
// TASK-103.34: the handlers generator's seed 17, minimized. Before the fix, AGNIJS_JIT=force
// printed a number for a local that was never assigned when a throw before its first assignment
// reached a handler that read it. The full program: jit-fuzz.mjs --print handlers:17
var log = [];
function thrower(i, t) { if (i === t) throw new RangeError('T' + i); return i + 1; }
function show(v) { var t = typeof v; return t === 'object' ? (v === null ? 'null' : 'obj') : t === 'function' ? 'fn' : t + ':' + String(v); }
function f1(n, o, a, t, t2) {
  var v0, v1 = 0, v2, v3 = 10;
  try {
    v2 = thrower(0, t);
    for (var i = 0; i < n; i++) {
      try { v1 = v2 + v1; if (i === t2) continue; } catch (e2) { log.push('c2:' + e2.name + ':' + [show(v0), show(v1), show(v2), show(v3)].join(',')); v0 = v3 * 2 + i; }
    }
  } finally { log.push('fin1:' + [show(v0), show(v1), show(v2), show(v3)].join(',')); }
}
var objs = [null, {}, { p: { q: 1 } }, { p: null }, { p: { q: 2 }, q: { q: 3 } }, undefined];
var arrs = [[1, 2, 3, 4, 5, 6, 7, 8], [1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5], [1, 's', 3, null, 5, {}, 7, 8], [{ z: 1 }, { z: 2 }, { z: 3 }, { z: 4 }, { z: 5 }, { z: 6 }, { z: 7 }, { z: 8 }], [{}, {}, null, {}, {}, {}, {}, {}]];
log.push('f1#5 ' + (function () { try { return f1(4, objs[5], arrs[0], 2, 8); } catch (x) { return 'threw ' + x.name; } })());
log.push('f1#8 ' + (function () { try { return f1(3, objs[4], arrs[3], 0, 2); } catch (x) { return 'threw ' + x.name; } })());
console.log(log.join('\n'));
