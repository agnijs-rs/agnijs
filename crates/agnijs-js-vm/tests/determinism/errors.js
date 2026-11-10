// Error messages and stacks. Recursion depth limits are left out: where
// the native stack runs out depends on the platform (docs/determinism.md).
var out = [];
const msg = (f) => { try { f(); return "no error"; } catch (e) { return String(e); } };
out.push(msg(() => null.x));
out.push(msg(() => undefined()));
out.push(msg(() => { const c = 1; c = 2; }));
out.push(msg(() => new Array(-1)));
out.push(msg(() => JSON.parse("{bad")));
out.push(msg(() => BigInt(1.5)));
out.push(msg(() => Symbol() + ""));
out.push(msg(() => x_not_defined));
function inner() { throw new TypeError("deep"); }
function outer() { inner(); }
try { outer(); } catch (e) { out.push(String(e.stack).replace(/\n/g, "|")); }
out.push(String(new AggregateError([1], "agg").errors.length));
out.push(String(new Error("m", { cause: 7 }).cause));
