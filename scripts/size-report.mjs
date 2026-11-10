// Build the size probe (crates/agnijs-js-vm/examples/size_probe.rs) in each
// configuration docs/binary-size.md reports and print its size, natively and
// for wasm32-unknown-unknown.
//
// Usage: node scripts/size-report.mjs [--wasm] [--data name=<ICU4X_DATA_DIR> ...]
//
// Always measured: the default features (crate-default ICU4X data) and
// `--no-default-features --features linear-regex` (no Intl). Each `--data`
// adds a build with that custom ICU4X data (from
// crates/agnijs-js-intl/tools/gen-icu4x-data.js). Each configuration builds
// in its own target directory under target/size-report so switching does not
// rebuild the others. Builds run one at a time.
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const argv = process.argv.slice(2);
const wasm = argv.includes("--wasm");
const configs = [
  { name: "default (full Intl)", features: [], env: {} },
  { name: "no Intl", features: ["--no-default-features", "--features", "linear-regex"], env: {} },
];
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--data") {
    const [name, dir] = argv[++i].split(/=(.*)/s);
    configs.push({ name: `Intl, ${name}`, features: [], env: { ICU4X_DATA_DIR: path.resolve(dir) } });
  }
}

const baseTarget = path.join(process.env.CARGO_TARGET_DIR || path.join(root, "target"), "size-report");
const targets = [{ triple: null, file: process.platform === "win32" ? "size_probe.exe" : "size_probe" }];
if (wasm) targets.push({ triple: "wasm32-unknown-unknown", file: "size_probe.wasm" });

const rows = [];
for (const [i, c] of configs.entries()) {
  const targetDir = path.join(baseTarget, String(i));
  const row = { name: c.name };
  for (const t of targets) {
    const args = ["build", "--locked", "--profile", "size", "-p", "agnijs-js-vm", "--example", "size_probe", ...c.features];
    if (t.triple) args.push("--target", t.triple);
    const env = { ...process.env, CARGO_TARGET_DIR: targetDir, ...c.env };
    if (!c.env.ICU4X_DATA_DIR) delete env.ICU4X_DATA_DIR;
    execFileSync("cargo", args, { cwd: root, env, stdio: ["ignore", "inherit", "inherit"] });
    const out = path.join(targetDir, ...(t.triple ? [t.triple] : []), "size", "examples", t.file);
    row[t.triple || "native"] = statSync(out).size;
  }
  rows.push(row);
}

const mb = (n) => (n / 1e6).toFixed(2) + " MB";
console.log(`\n| Configuration | native (${process.platform}-${process.arch}) |${wasm ? " wasm32 |" : ""}`);
console.log(`|---|---|${wasm ? "---|" : ""}`);
for (const r of rows) {
  console.log(`| ${r.name} | ${mb(r.native)} |${wasm ? ` ${mb(r["wasm32-unknown-unknown"])} |` : ""}`);
}
