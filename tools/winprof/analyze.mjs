// Summarize a winprof folded-stack file.
//
//   node analyze.mjs <file.folded> [topN] [focus-substring] [--lines]
//
// Prints self and inclusive time per function as a share of the samples
// that touch engine code; idle-thread stacks are dropped. `focus` keeps only
// stacks containing that substring; `--lines` adds a self-time table per
// source line (from the `@file:line` pseudo-frames).
import fs from "node:fs";

const argv = process.argv.slice(2);
const lines = argv.includes("--lines");
const [file, topArg, focus] = argv.filter((a) => a !== "--lines");
if (!file) {
  console.error("usage: node analyze.mjs <file.folded> [topN] [focus] [--lines]");
  process.exit(2);
}
const top = Number(topArg ?? 30);
const short = (s) =>
  s
    .replace(/agnijs_js_vm::/g, "vm::")
    .replace(/agnijs_js_compiler::/g, "comp::")
    .replace(/agnijs_js_parser::/g, "parse::")
    .replace(/impl\$\d+::/g, "")
    .replace(/<[^<>]*>/g, "")
    .slice(0, 110);
const idle = /NtWaitFor|ZwWaitFor|NtDelayExecution|NtWaitForWorkViaWorkerFactory|NtRemoveIoCompletion/;
const engine = (f) => f.startsWith("vm::") || f.includes("agnijs") || f.includes("regress") || f.includes("quickjs");
const self = new Map();
const selfLine = new Map();
const incl = new Map();
let total = 0;
for (const line of fs.readFileSync(file, "utf8").split("\n")) {
  if (!line.trim()) continue;
  const i = line.lastIndexOf(" ");
  const n = Number(line.slice(i + 1));
  const all = line.slice(0, i).split(";").map(short);
  if (idle.test(all.join(";")) || !all.some(engine)) continue;
  if (focus && !all.some((f) => f.includes(focus))) continue;
  const frames = all.filter((f) => !f.startsWith("@"));
  total += n;
  const leaf = frames[frames.length - 1];
  self.set(leaf, (self.get(leaf) ?? 0) + n);
  const last = all[all.length - 1];
  if (last.startsWith("@")) selfLine.set(last, (selfLine.get(last) ?? 0) + n);
  for (const f of new Set(lines ? all : frames)) incl.set(f, (incl.get(f) ?? 0) + n);
}
const print = (title, m) => {
  console.log(`\n== ${title} (total ${total} samples)`);
  [...m.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, top)
    .forEach(([k, v]) => console.log(`${((100 * v) / total).toFixed(1).padStart(5)}%  ${k}`));
};
print("self", self);
print("inclusive", incl);
if (lines) print("self lines", selfLine);
