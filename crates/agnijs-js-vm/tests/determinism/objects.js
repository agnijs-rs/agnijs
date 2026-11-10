// Enumeration order is defined by insertion, never by a hash.
var out = [];
const o = { b: 1, 2: "two", a: 2, 1: "one", [Symbol("s")]: 3, "-1": "neg", 10: "ten" };
delete o.b;
o.b = 4;
out.push(Object.keys(o).join(","));
out.push(Reflect.ownKeys(o).map(String).join(","));
out.push(JSON.stringify(o));
out.push(JSON.stringify({ z: [1, { y: null, x: undefined }], w: " \ud800" }, null, 1).replace(/\n/g, "|"));
const m = new Map([["k1", 1], ["k2", 2], ["k3", 3]]);
m.delete("k2");
m.set("k2", 22).set(NaN, "nan").set(-0, "zero");
out.push([...m].map(([k, v]) => `${String(k)}=${v}`).join(","));
const s = new Set([3, 1, 2]);
s.delete(1);
s.add(1);
out.push([...s].join(","));
const people = [["ann", 3], ["bob", 1], ["cat", 3], ["dan", 2], ["eve", 1], ["fay", 3]];
out.push(people.slice().sort((x, y) => x[1] - y[1]).map((p) => p[0]).join(","));
out.push(["b", "a", "B", "aa", "10", "9", "é"].sort().join(","));
out.push([10, 9, 1, 100, -1].sort().join(","));
class A { m() {} get g() { return 1; } static s() {} }
out.push(Object.getOwnPropertyNames(A.prototype).join(","));
out.push(Object.getOwnPropertyNames(A).join(","));
out.push(Object.entries({ c: 3, a: 1, b: 2 }).flat().join(","));
for (const k in { y: 1, __proto__: { x: 2, y: 3 } }) out.push("for-in " + k);
