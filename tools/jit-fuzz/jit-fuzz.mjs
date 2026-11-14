#!/usr/bin/env node
// Differential fuzzing of the baseline JIT against the interpreter (docs/jit-fuzz.md).
//
// Each program runs in the agnijs command with the JIT off (the oracle), AGNIJS_JIT=on and
// AGNIJS_JIT=force. Stdout, the exit status and the first line of stderr must agree. A program
// that disagrees is saved with a report and a minimized copy, and the run exits 1.
//
// Usage: node tools/jit-fuzz/jit-fuzz.mjs [options] [file.js ...]
//
//   With files: compare those files only (no regressions, no generated programs).
//   Without:    the regression programs in tools/jit-fuzz/regressions, then generated programs.
//
//   --bin <path>          agnijs built with the JIT (default $AGNIJS_BIN, else target/release/agnijs)
//                         cargo build --release --locked -p agnijs-cli --features agnijs-js-vm/jit
//   --seed <n>            first seed (default 1)
//   --count <n>           run this many seeds, however long it takes
//   --time <seconds>      otherwise stop starting seeds after this long (default 120)
//   --gen <names>         comma-separated: handlers, mixed, numeric (default all three); every
//                         seed runs one program from each
//   --warm <mix|on|off>   the warm-up variant (handlers and numeric): every function is run hot
//                         first so that mode `on` compiles it. mix (default): seeds 3, 7, 11, ...
//   --stress              also run forced under AGNIJS_GC_STRESS (and the interpreter under it
//                         when that disagrees, to tell a collector bug from a JIT one)
//   --stress-every <n>    collect every n safe points (default 16; 1 is very slow)
//   --node                also compare the interpreter's stdout with Node's, for generators and
//                         regressions whose output does not depend on error messages
//   --jobs <n>            programs run at once (default: cores - 1, at most 4)
//   --timeout <seconds>   per run (default 30; 4x under GC stress)
//   --out <dir>           divergences and scratch files (default target/jit-fuzz)
//   --minimize-time <s>   time for minimizing each divergence (default 60; 0 does not minimize)
//   --no-regressions      skip the regression programs
//   --only-regressions    run the regression programs only
//   --print <gen>:<seed>[:warm]   write that generated program to stdout and exit
//
// Reproduce a divergence with the command its report gives, for example
//   node tools/jit-fuzz/jit-fuzz.mjs --gen handlers --seed 17 --count 1 --warm off --no-regressions

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import * as handlers from "./gen-handlers.mjs";
import * as mixed from "./gen-mixed.mjs";
import * as numeric from "./gen-numeric.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "../..");
const GENERATORS = { handlers, mixed, numeric };

// ---- options --------------------------------------------------------------------------------

const argv = process.argv.slice(2);
const VALUED = new Set(["--bin", "--seed", "--count", "--time", "--gen", "--warm", "--stress-every", "--jobs", "--timeout", "--out", "--minimize-time", "--print"]);
const FLAGS = new Set(["--stress", "--node", "--no-regressions", "--only-regressions", "--help", "-h"]);
const opts = {};
const files = [];
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (VALUED.has(a)) {
    if (i + 1 >= argv.length) usage(`${a} needs a value`);
    opts[a] = argv[++i];
  } else if (FLAGS.has(a)) opts[a] = true;
  else if (a.startsWith("-")) usage(`unknown option ${a}`);
  else files.push(path.resolve(a));
}
if (opts["--help"] || opts["-h"]) {
  const lines = readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n");
  const end = lines.findIndex((l) => !l.startsWith("//") && !l.startsWith("#!"));
  process.stdout.write(lines.slice(1, end).map((l) => l.replace(/^\/\/ ?/, "")).join("\n") + "\n");
  process.exit(0);
}

function usage(msg) {
  console.error(`jit-fuzz: ${msg} (--help for usage)`);
  process.exit(2);
}
function num(name, dflt) {
  if (opts[name] === undefined) return dflt;
  const n = Number(opts[name]);
  if (!Number.isFinite(n) || n < 0) usage(`${name} wants a non-negative number`);
  return n;
}

const genNames = (opts["--gen"] || "handlers,mixed,numeric").split(",").map((s) => s.trim()).filter(Boolean);
for (const g of genNames) if (!GENERATORS[g]) usage(`unknown generator ${g} (handlers, mixed, numeric)`);
const warmMode = opts["--warm"] || "mix";
if (!["mix", "on", "off"].includes(warmMode)) usage("--warm is mix, on or off");

if (opts["--print"]) {
  const [g, s, w] = opts["--print"].split(":");
  if (!GENERATORS[g] || !Number.isInteger(Number(s))) usage("--print wants <gen>:<seed>[:warm]");
  process.stdout.write(GENERATORS[g].generate(Number(s), { warm: w === "warm" }));
  process.exit(0);
}

const firstSeed = num("--seed", 1);
const count = opts["--count"] !== undefined ? num("--count", 0) : null;
const timeBudgetMs = num("--time", 120) * 1000;
const stress = !!opts["--stress"];
const stressEvery = num("--stress-every", 16);
const useNode = !!opts["--node"];
const jobs = Math.max(1, num("--jobs", Math.max(1, Math.min(4, (os.availableParallelism?.() ?? os.cpus().length) - 1))));
const timeoutMs = num("--timeout", 30) * 1000;
const outDir = path.resolve(opts["--out"] || path.join(ROOT, "target", "jit-fuzz"));
const minimizeMs = num("--minimize-time", 60) * 1000;
const runRegressions = files.length === 0 && !opts["--no-regressions"];
const runGenerated = files.length === 0 && !opts["--only-regressions"];
const bin = path.resolve(opts["--bin"] || process.env.AGNIJS_BIN || path.join(ROOT, "target", "release", process.platform === "win32" ? "agnijs.exe" : "agnijs"));

const tmpDir = path.join(outDir, "tmp");
mkdirSync(tmpDir, { recursive: true });

// ---- running one program ------------------------------------------------------------------

const MODES = [
  { name: "off", env: {} },
  { name: "on", env: { AGNIJS_JIT: "on" } },
  { name: "force", env: { AGNIJS_JIT: "force" } },
];
if (stress) MODES.push({ name: "force+gc", env: { AGNIJS_JIT: "force", AGNIJS_GC_STRESS: String(stressEvery) }, slow: true });
const OFF_GC = { name: "off+gc", env: { AGNIJS_GC_STRESS: String(stressEvery) }, slow: true };

function run(cmd, args, env, limitMs) {
  return new Promise((resolve) => {
    const clean = { ...process.env };
    for (const k of Object.keys(clean)) if (k.startsWith("AGNIJS_")) delete clean[k];
    const t0 = Date.now();
    let out = "";
    let err = "";
    let timedOut = false;
    let p;
    try {
      p = spawn(cmd, args, { env: { ...clean, ...env }, windowsHide: true });
    } catch (e) {
      resolve({ out: "", err: String(e), code: null, sig: null, timedOut: false, ms: 0 });
      return;
    }
    const timer = setTimeout(() => {
      timedOut = true;
      p.kill("SIGKILL");
    }, limitMs);
    p.stdout.on("data", (d) => {
      if (out.length < 1 << 24) out += d;
    });
    p.stderr.on("data", (d) => {
      if (err.length < 1 << 16) err += d;
    });
    p.on("error", (e) => {
      err += String(e);
    });
    p.on("close", (code, sig) => {
      clearTimeout(timer);
      resolve({ out: out.replace(/\r\n/g, "\n"), err: err.replace(/\r\n/g, "\n"), code, sig, timedOut, ms: Date.now() - t0 });
    });
  });
}

const runMode = (file, mode) => run(bin, [file], mode.env, mode.slow ? timeoutMs * 4 : timeoutMs);
const runNode = (file) => run(process.execPath, ["--stack-size=4000", path.join(HERE, "node-run.cjs"), file], {}, timeoutMs);
const key = (r) => JSON.stringify([r.out, r.code, r.sig, r.err.split("\n")[0]]);

// Compare one file across the modes. Returns null when the interpreter did not finish in time
// (nothing to compare against), else { base, results, diverged: [{ mode, kind }] }.
async function compare(file, { node = false } = {}) {
  const base = await runMode(file, MODES[0]);
  if (base.timedOut) return null;
  const results = [["off", base]];
  const diverged = [];
  for (const mode of MODES.slice(1)) {
    const r = await runMode(file, mode);
    results.push([mode.name, r]);
    if (r.timedOut) {
      // A hang only when the interpreter took a small part of the limit. Under GC stress a run
      // can be hundreds of times slower than without, so a timeout there proves nothing.
      if (mode.slow) stats.stressTimeouts++;
      else if (base.ms * 5 < timeoutMs) diverged.push({ mode: mode.name, kind: "hang" });
      else stats.slowTimeouts++;
      continue;
    }
    if (key(r) !== key(base)) diverged.push({ mode: mode.name, kind: r.code !== base.code || r.sig !== base.sig ? "exit" : "output" });
  }
  if (stress && diverged.some((d) => d.mode === "force+gc")) {
    // Tell a collector bug (the interpreter under stress disagrees too) from a JIT one.
    const r = await runMode(file, OFF_GC);
    results.push([OFF_GC.name, r]);
  }
  if (node) {
    const r = await runNode(file);
    results.push(["node", r]);
    if (!r.timedOut && r.out !== base.out) diverged.push({ mode: "node", kind: "output" });
  }
  return { base, results, diverged };
}

// ---- minimizing ---------------------------------------------------------------------------

// Group lines into units whose brackets balance (a whole function, a whole loop), so that a cut
// along units keeps the syntax. Strings and line comments are skipped; a line that leaves the
// count negative ends its unit anyway.
function units(lines) {
  const out = [];
  let cur = [];
  let depth = 0;
  for (const line of lines) {
    let q = null;
    for (let i = 0; i < line.length; i++) {
      const c = line[i];
      if (q) {
        if (c === "\\") i++;
        else if (c === q) q = null;
      } else if (c === '"' || c === "'" || c === "`") q = c;
      else if (c === "/" && line[i + 1] === "/") break;
      else if ("{([".includes(c)) depth++;
      else if ("})]".includes(c)) depth--;
    }
    cur.push(line);
    if (depth <= 0) {
      out.push(cur.join("\n"));
      cur = [];
      depth = 0;
    }
  }
  if (cur.length) out.push(cur.join("\n"));
  return out;
}

// Delta debugging (ddmin, complements only): drop chunks while the program still diverges.
// Each round's candidates run `jobs` at a time; the first (lowest) one that still fails wins,
// so the result does not depend on timing.
async function ddmin(items, fails, deadline) {
  let n = 2;
  while (items.length >= 2 && Date.now() < deadline) {
    const size = Math.ceil(items.length / n);
    const cands = [];
    for (let i = 0; i < items.length; i += size) cands.push(items.slice(0, i).concat(items.slice(i + size)));
    let found = null;
    for (let b = 0; b < cands.length && !found && Date.now() < deadline; b += jobs) {
      const batch = cands.slice(b, b + jobs);
      const rs = await Promise.all(batch.map((c) => (c.length ? fails(c) : false)));
      const k = rs.indexOf(true);
      if (k >= 0) found = batch[k];
    }
    if (found) {
      items = found;
      n = Math.max(n - 1, 2);
    } else {
      if (n >= items.length) break;
      n = Math.min(items.length, n * 2);
    }
  }
  return items;
}

// Shrink a divergent program: first by balanced units, then by lines. A cut that breaks the
// syntax fails in every mode alike, so it is never kept.
async function minimize(src, div) {
  if (minimizeMs === 0 || div.kind === "hang") return null;
  const deadline = Date.now() + minimizeMs;
  const mode = div.mode === "node" ? null : MODES.find((m) => m.name === div.mode);
  const seen = new Map();
  let tests = 0;
  const fails = async (parts) => {
    const text = parts.join("\n") + "\n";
    const h = createHash("sha1").update(text).digest("hex");
    if (seen.has(h)) return seen.get(h);
    const p = (async () => {
      const file = path.join(tmpDir, `min-${h.slice(0, 16)}.js`);
      writeFileSync(file, text);
      tests++;
      const base = await runMode(file, MODES[0]);
      let bad = false;
      if (!base.timedOut) {
        const r = mode ? await runMode(file, mode) : await runNode(file);
        bad = !r.timedOut && (mode ? key(r) !== key(base) : r.out !== base.out);
      }
      rmSync(file, { force: true });
      return bad;
    })();
    seen.set(h, p);
    return p;
  };
  const lines = src.split("\n").filter((l) => l.trim() !== "");
  if (!(await fails(lines))) return { text: null, note: "the program no longer diverges when run again (blank lines removed)" };
  const byUnits = await ddmin(units(lines), fails, deadline);
  const byLines = await ddmin(byUnits.join("\n").split("\n"), fails, deadline);
  const late = Date.now() >= deadline ? " (time limit reached; not minimal)" : "";
  return { text: byLines.join("\n") + "\n", note: `${lines.length} -> ${byLines.length} lines in ${tests} runs${late}` };
}

// ---- reporting ----------------------------------------------------------------------------

const divergences = [];

async function report(name, src, res, repro) {
  const first = res.diverged[0];
  const stem = path.join(outDir, name);
  writeFileSync(`${stem}.js`, src);
  let txt = `${name}: diverges in ${res.diverged.map((d) => `${d.mode} (${d.kind})`).join(", ")}\n`;
  if (repro) txt += `reproduce: ${repro}\n`;
  txt += `binary: ${bin}\n\n`;
  for (const [m, r] of res.results) {
    txt += `--- ${m}: exit ${r.code}${r.sig ? ` signal ${r.sig}` : ""}${r.timedOut ? " TIMED OUT" : ""}, ${r.ms} ms\n`;
    if (r.err) txt += `stderr: ${r.err.slice(0, 1500).trimEnd()}\n`;
  }
  for (const d of res.diverged) {
    const other = res.results.find(([m]) => m === d.mode)[1];
    const a = res.base.out.split("\n");
    const b = other.out.split("\n");
    const at = a.findIndex((l, i) => l !== b[i]);
    if (at >= 0 || a.length !== b.length) {
      const i = at >= 0 ? at : Math.min(a.length, b.length);
      txt += `\nfirst differing stdout line (${i + 1}), off vs ${d.mode}:\n  off:      ${String(a[i]).slice(0, 400)}\n  ${d.mode.padEnd(9)} ${String(b[i]).slice(0, 400)}\n`;
    }
  }
  const min = await minimize(src, first);
  if (min) {
    txt += `\nminimized for ${first.mode}: ${min.note}\n`;
    if (min.text) {
      writeFileSync(`${stem}.min.js`, min.text);
      txt += `${stem}.min.js\n`;
    }
  }
  writeFileSync(`${stem}.txt`, txt);
  divergences.push(name);
  console.log(`DIVERGENCE ${name}: ${res.diverged.map((d) => `${d.mode} (${d.kind})`).join(", ")}\n  ${stem}.txt`);
}

// ---- the run ------------------------------------------------------------------------------

async function checkBinary() {
  if (!existsSync(bin)) {
    usage(`no agnijs binary at ${bin}; build it with\n  cargo build --release --locked -p agnijs-cli --features agnijs-js-vm/jit\nor pass --bin`);
  }
  const probe = path.join(tmpDir, "probe.js");
  writeFileSync(probe, "function f(n) { var s = 0; for (var i = 0; i < n; i++) s += i; return s; }\nconsole.log(f(10));\n");
  const r = await run(bin, [probe], { AGNIJS_JIT: "force", AGNIJS_JIT_STATS: "1" }, 30000);
  rmSync(probe, { force: true });
  const m = /agnijs jit: .*compiled: (\d+)/.exec(r.err);
  if (r.out.trim() !== "45" || !m || Number(m[1]) === 0) {
    usage(`${bin} does not compile with AGNIJS_JIT=force (built without the jit feature?)\n  stdout: ${r.out.trim()}\n  stderr: ${r.err.trim().slice(0, 300)}`);
  }
}

const stats = { files: 0, programs: 0, byGen: {}, skipped: 0, baseFailed: 0, stressTimeouts: 0, slowTimeouts: 0 };

async function runFile(file, label, { node = false } = {}) {
  const res = await compare(file, { node });
  stats.files++;
  if (!res) {
    stats.skipped++;
    console.log(`skipped ${label}: the interpreter did not finish in ${timeoutMs / 1000} s`);
    return;
  }
  if (res.diverged.length) await report(label, readFileSync(file, "utf8"), res, `node tools/jit-fuzz/jit-fuzz.mjs ${path.relative(process.cwd(), file)}`);
}

async function pool(next) {
  const workers = Array.from({ length: jobs }, async () => {
    for (let job = next(); job; job = next()) await job();
  });
  await Promise.all(workers);
}

const t0 = Date.now();
await checkBinary();
console.log(`jit-fuzz: ${bin}\n  modes ${MODES.map((m) => m.name).join(", ")}${useNode ? ", node" : ""}; ${jobs} job(s); out ${outDir}`);

if (files.length) {
  const queue = files.slice();
  await pool(() => {
    const f = queue.shift();
    return f && (() => runFile(f, path.basename(f, ".js"), { node: useNode }));
  });
}

if (runRegressions) {
  const dir = path.join(HERE, "regressions");
  const queue = readdirSync(dir).filter((f) => f.endsWith(".js")).sort();
  const n = queue.length;
  await pool(() => {
    const f = queue.shift();
    if (!f) return null;
    const file = path.join(dir, f);
    const nodeOk = useNode && readFileSync(file, "utf8").includes("jit-fuzz: node-safe");
    return () => runFile(file, `regression-${path.basename(f, ".js")}`, { node: nodeOk });
  });
  console.log(`  ${n} regression programs done (${((Date.now() - t0) / 1000).toFixed(0)} s)`);
}

if (runGenerated) {
  const started = Date.now();
  let seed = firstSeed;
  let lastStatus = Date.now();
  let g = 0;
  await pool(() => {
    if (count !== null ? seed >= firstSeed + count : Date.now() - started > timeBudgetMs) return null;
    const s = seed;
    const gname = genNames[g++];
    if (g === genNames.length) {
      g = 0;
      seed++;
    }
    const gen = GENERATORS[gname];
    const warm = gen.hasWarm && (warmMode === "on" || (warmMode === "mix" && s % 4 === 3));
    return async () => {
      const src = gen.generate(s, { warm });
      const file = path.join(tmpDir, `${gname}-${s}.js`);
      writeFileSync(file, src);
      const res = await compare(file, { node: useNode && gen.nodeSafe });
      rmSync(file, { force: true });
      stats.programs++;
      stats.byGen[gname] = (stats.byGen[gname] || 0) + 1;
      if (!res) {
        stats.skipped++;
        return;
      }
      if (res.base.code !== 0) stats.baseFailed++;
      if (res.diverged.length) {
        const repro = `node tools/jit-fuzz/jit-fuzz.mjs --gen ${gname} --seed ${s} --count 1 --warm ${warm ? "on" : "off"} --no-regressions${stress ? " --stress --stress-every " + stressEvery : ""}${res.diverged.some((d) => d.mode === "node") ? " --node" : ""}`;
        await report(`${gname}-${s}${warm ? "-warm" : ""}`, src, res, repro);
      }
      if (Date.now() - lastStatus > 30000) {
        lastStatus = Date.now();
        console.log(`  ... ${stats.programs} programs, ${divergences.length} divergence(s), ${((Date.now() - t0) / 1000).toFixed(0)} s`);
      }
    };
  });
  const by = Object.entries(stats.byGen).map(([g, n]) => `${g} ${n}`).join(", ");
  const lastSeed = g === 0 ? seed - 1 : seed;
  console.log(`  seeds ${firstSeed}..${lastSeed}: ${stats.programs} programs (${by || "none"})`);
  if (stats.baseFailed) console.log(`  note: ${stats.baseFailed} program(s) exited non-zero in the interpreter (still compared)`);
}

const secs = ((Date.now() - t0) / 1000).toFixed(0);
const inconclusive = stats.stressTimeouts + stats.slowTimeouts;
console.log(`jit-fuzz: ${divergences.length} divergence(s); ${stats.skipped} skipped (interpreter timed out)${inconclusive ? `; ${inconclusive} JIT run(s) timed out where the interpreter was slow or under GC stress` : ""}; ${secs} s`);
if (divergences.length) console.log(`  reports in ${outDir}`);
rmSync(tmpDir, { recursive: true, force: true });
process.exit(divergences.length ? 1 : 0);
