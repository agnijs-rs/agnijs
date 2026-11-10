// Job order: promise reactions, async functions and generators.
var out = [];
const log = (s) => out.push(s);
Promise.resolve().then(() => log("then 1")).then(() => log("then 2"));
(async () => {
  log("async start");
  await null;
  log("after await 1");
  await new Promise((r) => r());
  log("after await 2");
})();
globalThis.queueMicrotask?.(() => log("microtask"));
Promise.reject(new Error("x")).catch((e) => log("caught " + e.message));
Promise.all([1, Promise.resolve(2), new Promise((r) => r(3))]).then((v) => log("all " + v));
Promise.race([new Promise(() => {}), Promise.resolve("r")]).then((v) => log("race " + v));
function* g() { const x = yield 1; log("gen got " + x); yield x * 2; }
const it = g();
log("gen " + it.next().value + " " + it.next(21).value);
(async () => { for await (const v of (async function* () { yield "a"; yield "b"; })()) log("for await " + v); })();
log("sync end");
