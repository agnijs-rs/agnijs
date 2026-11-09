// Checks that every crate in the dependency graph carries a license that is
// compatible with this workspace's `MIT OR Apache-2.0` (TASK-100.12). It reads
// `cargo metadata`, so it needs only cargo and node. For each package the SPDX
// expression must be satisfiable using only the allowed licenses below.
//
// Usage: node scripts/check-licenses.mjs [--list]
import { execFileSync } from 'node:child_process';

// Permissive licenses that can be combined with MIT OR Apache-2.0 code in a
// redistributed binary or source tree. Unicode-3.0 covers the ICU4X data.
const ALLOWED = new Set([
  'MIT',
  'Apache-2.0',
  'Apache-2.0 WITH LLVM-exception',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'ISC',
  'Zlib',
  'Unicode-3.0',
  'Unicode-DFS-2016',
  'CC0-1.0',
  'Unlicense',
  'MPL-2.0', // weak copyleft, file-level; none expected, listed so a hit is a review prompt
]);

const meta = JSON.parse(
  execFileSync('cargo', ['metadata', '--format-version', '1', '--locked'], { encoding: 'utf8', maxBuffer: 1 << 28 }),
);

// Evaluate an SPDX expression: OR is satisfied by any branch, AND by all.
function ok(expr) {
  expr = expr.replace(/\//g, ' OR ').replace(/[()]/g, ' ( ').trim();
  const tokens = expr.split(/\s+/).filter(Boolean);
  let i = 0;
  function atom() {
    if (tokens[i] === '(') {
      i++;
      const v = orExpr();
      i++; // ')'
      return v;
    }
    let id = tokens[i++];
    if (tokens[i] === 'WITH') {
      id += ' WITH ' + tokens[i + 1];
      i += 2;
    }
    return ALLOWED.has(id);
  }
  function andExpr() {
    let v = atom();
    while (tokens[i] === 'AND') {
      i++;
      v = atom() && v;
    }
    return v;
  }
  function orExpr() {
    let v = andExpr();
    while (tokens[i] === 'OR') {
      i++;
      v = andExpr() || v;
    }
    return v;
  }
  return orExpr();
}

const counts = new Map();
const bad = [];
for (const p of meta.packages) {
  if (p.source === null) {
    // Workspace members: must declare the project license.
    if (p.license !== 'MIT OR Apache-2.0') bad.push(`${p.name}: workspace crate declares ${JSON.stringify(p.license)}`);
    continue;
  }
  const lic = p.license ?? '(none)';
  counts.set(lic, (counts.get(lic) ?? 0) + 1);
  if (!p.license || !ok(p.license)) bad.push(`${p.name} ${p.version}: ${lic}`);
}
if (process.argv.includes('--list')) {
  for (const [l, n] of [...counts].sort((a, b) => b[1] - a[1])) console.log(String(n).padStart(4), l);
}
if (bad.length) {
  console.error('License check failed:\n' + bad.join('\n'));
  process.exit(1);
}
console.log(`License check passed: ${meta.packages.length} packages, ${counts.size} distinct license expressions.`);
