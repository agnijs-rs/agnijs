// Build the size probes docs/binary-size.md reports in each configuration
// and print their sizes, natively and (with --wasm) for
// wasm32-unknown-unknown.
//
// Usage: node scripts/size-report.mjs [--wasm] [--data name=<ICU4X_DATA_DIR> ...]
//                                     [--debugger] [--compare] [--debugger-residue]
//
// Always measured: the default features (crate-default ICU4X data) and
// `--no-default-features --features linear-regex` (no Intl), both with
// agnijs-js-vm's `size_probe` example. Each `--data` adds a build with that
// custom ICU4X data (from crates/agnijs-js-intl/tools/gen-icu4x-data.js).
//
// --debugger adds the default build with the debugger in use
// (`size_probe_debugger`) and with the debugger and the CDP endpoint
// (agnijs-js-cdp's `size_probe_cdp`).
//
// --compare adds QuickJS-NG and Boa, built from the pinned comparison
// workspace in tools/size-compare (its own Cargo.lock), native only.
//
// --debugger-residue (MSVC targets only) builds the default probe once more
// with a linker map and sums the functions and data of the debugger
// (agnijs_js_debugger and agnijs_js_vm::debug) still in it: the most a
// feature that dropped the debugger could save from a build that does not
// use it.
//
// All builds use [profile.size] and run one at a time. Each feature/data
// combination builds in its own target directory under
// $CARGO_TARGET_DIR/size-report, so switching does not rebuild the others
// and probes with the same features share one.
import { execFileSync } from "node:child_process";
import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const argv = process.argv.slice(2);
const has = (flag) => argv.includes(flag);
const wasm = has("--wasm");
const noIntl = ["--no-default-features", "--features", "linear-regex"];

const vmProbe = { pkg: "agnijs-js-vm", example: "size_probe" };
const configs = [{ name: "default (full Intl)", ...vmProbe, features: [], env: {}, dir: "default" }];
if (has("--debugger")) {
  configs.push(
    { name: "default, debugger in use", pkg: "agnijs-js-vm", example: "size_probe_debugger", features: [], env: {}, dir: "default" },
    { name: "default, debugger and CDP", pkg: "agnijs-js-cdp", example: "size_probe_cdp", features: [], env: {}, dir: "default" },
  );
}
configs.push({ name: "no Intl", ...vmProbe, features: noIntl, env: {}, dir: "no-intl" });
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--data") {
    const [name, dir] = argv[++i].split(/=(.*)/s);
    configs.push({ name: `Intl, ${name}`, ...vmProbe, features: [], env: { ICU4X_DATA_DIR: path.resolve(dir) }, dir: `data-${i}` });
  }
}
const compareRoot = path.join(root, "tools", "size-compare");
if (has("--compare")) {
  const c = { cwd: compareRoot, env: {}, dir: "compare", nativeOnly: true };
  configs.push(
    { name: "QuickJS-NG (rquickjs 0.14.0)", pkg: "quickjs-probe", bin: "quickjs-probe", features: [], ...c },
    { name: "Boa 0.22.0", pkg: "boa-probe", bin: "boa-probe", features: [], ...c },
    { name: "Boa 0.22.0, intl_bundled", pkg: "boa-probe", bin: "boa-probe", features: ["--features", "intl"], ...c, dir: "compare-intl" },
  );
}

const baseTarget = path.join(process.env.CARGO_TARGET_DIR || path.join(root, "target"), "size-report");
const exe = (name, triple) => (triple ? `${name}.wasm` : process.platform === "win32" ? `${name}.exe` : name);
const targets = [null];
if (wasm) targets.push("wasm32-unknown-unknown");

// `linkArgs` go to the final link only (through `cargo rustc`), never to
// dependencies or build scripts.
function build(c, triple, linkArgs = []) {
  const targetDir = path.join(baseTarget, c.dir);
  const args = [linkArgs.length ? "rustc" : "build", "--locked", "--profile", "size", "-p", c.pkg, ...(c.example ? ["--example", c.example] : ["--bin", c.bin]), ...c.features];
  if (triple) args.push("--target", triple);
  if (linkArgs.length) args.push("--", ...linkArgs.flatMap((a) => ["-C", `link-arg=${a}`]));
  const env = { ...process.env, CARGO_TARGET_DIR: targetDir, ...c.env };
  if (!c.env.ICU4X_DATA_DIR) delete env.ICU4X_DATA_DIR;
  execFileSync("cargo", args, { cwd: c.cwd || root, env, stdio: ["ignore", "inherit", "inherit"] });
  const name = c.example || c.bin;
  return path.join(targetDir, ...(triple ? [triple] : []), "size", ...(c.example ? ["examples"] : []), exe(name, triple));
}

const rows = [];
for (const c of configs) {
  const row = { name: c.name };
  for (const t of targets) {
    if (t && c.nativeOnly) continue;
    row[t || "native"] = statSync(build(c, t)).size;
  }
  rows.push(row);
}

const mb = (n) => (n === undefined ? "n/a" : (n / 1e6).toFixed(2) + " MB");
console.log(`\n| Configuration | native (${process.platform}-${process.arch}) | bytes |${wasm ? " wasm32 |" : ""}`);
console.log(`|---|---|---|${wasm ? "---|" : ""}`);
for (const r of rows) {
  console.log(`| ${r.name} | ${mb(r.native)} | ${r.native ?? ""} |${wasm ? ` ${mb(r["wasm32-unknown-unknown"])} |` : ""}`);
}

// The debugger's share of the default probe, from an MSVC linker map: each
// symbol's size is the distance to the next symbol in its section. Code
// the optimizer inlined into other functions is counted where it landed,
// so this is the out-of-line part only.
let residue = null;
if (has("--debugger-residue")) {
  if (process.platform !== "win32") throw new Error("--debugger-residue reads an MSVC linker map; run it on Windows");
  // Same target directory as the default row: only the final link reruns.
  const mapFile = path.join(baseTarget, configs[0].dir, "size_probe.map");
  const out = build(configs[0], null, [`/MAP:${mapFile}`]);
  const syms = [];
  for (const line of readFileSync(mapFile, "utf8").split(/\r?\n/)) {
    const m = /^\s*([0-9a-f]{4}):([0-9a-f]{8})\s+(\S+)\s+[0-9a-f]{16}/i.exec(line);
    if (m && m[1] !== "0000") syms.push({ sec: m[1], off: parseInt(m[2], 16), name: m[3] });
  }
  syms.sort((a, b) => a.sec.localeCompare(b.sec) || a.off - b.off);
  const isDebugger = (n) => /agnijs_js_debugger|12agnijs_js_vm5debug|agnijs_js_vm\.\.debug\.\./.test(n);
  let bytes = 0, count = 0;
  for (let i = 0; i + 1 < syms.length; i++) {
    const s = syms[i];
    if (syms[i + 1].sec !== s.sec || !isDebugger(s.name)) continue;
    bytes += syms[i + 1].off - s.off;
    count++;
  }
  residue = { bytes, count, size: statSync(out).size };
}

if (residue) {
  console.log(`\nDebugger code and data left in the default probe (${mb(residue.size)} with a linker map): ` +
    `${residue.count} symbols, ${(residue.bytes / 1e3).toFixed(1)} kB.`);
}
