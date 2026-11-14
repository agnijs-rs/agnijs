// Loops whose locals are guessed to be numbers, inside try/catch/finally, with throws before,
// between and after the assignments, locals that change type, nested handlers, and handlers that
// log every local: what TASK-103.34 was about. With `warm` every function is first run 1300 times
// on arguments that mostly do not throw, so that AGNIJS_JIT=on compiles it from inside its loop.
//
// The generator written for the TASK-103.34 fix (docs/jit-review-2026-11.md, "After the fixes"),
// kept byte-for-byte: a seed gives the program it gave then. Seeds 17, 1006 and 1019 are the three
// programs the pre-fix binary got wrong in mode force (regressions/handlers-*.js). It logs error
// names only, so Node can serve as an outside oracle.
export const nodeSafe = true;
export const hasWarm = true;

function rng(seed) {
  let s = BigInt(seed) * 0x9e3779b97f4a7c15n + 0x1234567n;
  s &= (1n << 64n) - 1n;
  if (s === 0n) s = 1n;
  const next = () => {
    s ^= s >> 12n;
    s ^= (s << 25n) & ((1n << 64n) - 1n);
    s ^= s >> 27n;
    return Number((s * 0x2545f4914f6cdd1dn) & 0xffffffffn);
  };
  const below = (n) => next() % n;
  return { below, pick: (xs) => xs[below(xs.length)], chance: (p) => below(1000) < p * 1000 };
}

export function generate(seed, opts = {}) {
  const warm = !!opts.warm;
  const r = rng(seed);
  const out = [];
  out.push("var log = [];");
  out.push("function thrower(i, t) { if (i === t) throw new RangeError('T' + i); return i + 1; }");
  out.push("function num(x, i) { return x * 2 + i; }");
  out.push("function odd(i) { return i % 3 === 0 ? 's' + i : i; }");
  out.push("function show(v) { var t = typeof v; return t === 'object' ? (v === null ? 'null' : 'obj') : t === 'function' ? 'fn' : t + ':' + String(v); }");
  const nf = 2 + r.below(3);
  for (let k = 0; k < nf; k++) {
    const nl = 2 + r.below(5);
    const locals = Array.from({ length: nl }, (_, i) => `v${i}`);
    const L = () => r.pick(locals);
    const dump = `[${locals.map((l) => `show(${l})`).join(", ")}].join(',')`;
    const throws = () =>
      r.pick([
        "o.p;",
        "o.p.q;",
        "if (i === t) o.p;",
        "thrower(i, t);",
        `${L()} = thrower(i, t);`,
        "if (i === t) throw new Error('E' + i);",
        "a[i].z;",
        "if (i === t) { o.p; }",
        "o[i % 2 === 0 ? 'p' : 'q'].q;",
      ]);
    const arith = () =>
      r.pick([
        `${L()} = i;`,
        `${L()} = ${L()} + 1;`,
        `${L()} = ${L()} * 2 + i;`,
        `${L()} = a[i];`,
        `${L()} = ${L()} + ${L()};`,
        `${L()} = num(${L()}, i);`,
        `${L()} = i - ${L()};`,
        `${L()} = (${L()} | 0) + (i & 3);`,
        `if (${L()} < i) ${L()} = i;`,
        `${L()}++;`,
        `${L()} = i % 7;`,
      ]);
    const retype = () =>
      r.pick([
        `if (i === t2) ${L()} = 's';`,
        `if (i === t2) ${L()} = undefined;`,
        `if (i === t2) ${L()} = o;`,
        `${L()} = odd(i);`,
        `if (i === t2) ${L()} = null;`,
        `if (i === t2) ${L()} = true;`,
      ]);
    const stmt = (depth) => {
      const p = r.below(100);
      if (p < 45) return arith();
      if (p < 65) return throws();
      if (p < 73) return retype();
      if (p < 80 && depth < 2) return `try { ${stmt(depth + 1)} ${stmt(depth + 1)} } catch (e2) { log.push('c2:' + e2.name + ':' + ${dump}); ${arith()} }`;
      if (p < 86 && depth < 2) return `for (var j = 0; j < 3; j++) { ${stmt(depth + 1)} }`;
      if (p < 90) return "if (i === t2) continue;";
      if (p < 93) return "if (i === t + 2) break;";
      if (p < 97 && depth < 2) return `try { ${stmt(depth + 1)} } finally { log.push('f2:' + ${dump}); }`;
      return arith();
    };
    const block = (n) => Array.from({ length: n }, () => stmt(0)).join("\n      ");
    const decl = locals.map((l) => (r.chance(0.45) ? `${l} = ${r.pick(["0", "1", "0.5", "-1", "10"])}` : l)).join(", ");
    const pre = r.chance(0.3) ? arith().replace(/\bi\b/g, "1").replace(/a\[1\]/g, "a[0]") : "";
    const inTry = r.chance(0.3) ? throws().replace(/\bi\b/g, "0") : "";
    const fin = r.chance(0.35) ? ` finally { log.push('fin${k}:' + ${dump}); }` : "";
    const loop = r.chance(0.8) ? `for (var i = 0; i < n; i++) {\n      ${block(2 + r.below(6))}\n    }` : `var i = -1; while (++i < n) {\n      ${block(2 + r.below(4))}\n    }`;
    out.push(`function f${k}(n, o, a, t, t2) {
  var ${decl};
  ${pre}
  try {
    ${inTry}
    ${loop}
    ${r.chance(0.3) ? throws().replace(/\bi\b/g, "n") : ""}
  } catch (e) {
    return 'caught ' + e.name + ' ' + ${dump};
  }${fin}
  return 'done ' + ${dump};
}`);
  }
  // The driver: every function over objects that throw at different
  // points, arrays of numbers and of mixed values, and throw indices.
  out.push("var objs = [null, {}, { p: { q: 1 } }, { p: null }, { p: { q: 2 }, q: { q: 3 } }, undefined];");
  out.push("var arrs = [[1, 2, 3, 4, 5, 6, 7, 8], [1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5], [1, 's', 3, null, 5, {}, 7, 8], [{ z: 1 }, { z: 2 }, { z: 3 }, { z: 4 }, { z: 5 }, { z: 6 }, { z: 7 }, { z: 8 }], [{}, {}, null, {}, {}, {}, {}, {}]];");
  const calls = [];
  for (let k = 0; k < nf; k++) {
    if (warm) {
      // Warm up on a run that mostly does not throw, so that mode `on`
      // compiles the function from inside its loop.
      calls.push(`for (var w = 0; w < 1300; w++) { try { f${k}(8, objs[2 + (w & 2)], arrs[w % 2], 99, 99); } catch (x) {} }`);
      calls.push("log.length = 0;");
    }
    const n = 6 + r.below(10);
    for (let c = 0; c < n; c++) {
      calls.push(`log.push('f${k}#${c} ' + (function () { try { return f${k}(${r.below(9)}, objs[${r.below(6)}], arrs[${r.below(5)}], ${r.below(9)}, ${r.below(9)}); } catch (x) { return 'threw ' + x.name; } })());`);
    }
  }
  out.push(...calls);
  out.push("console.log(log.join('\\n'));");
  return out.join("\n");
}
