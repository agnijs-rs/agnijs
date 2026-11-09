// Generate crates/gable-js-intl/src/tables.rs from test262's harness tables
// (ECMA-402 numbering-system digits and sanctioned units) and IANA zone1970.tab.
const fs = require("fs");
const path = require("path");
const root = path.resolve(__dirname, "../../..") + "/";
const harness = fs.readFileSync(root + "test262/harness/testIntl.js", "utf8");
// A directory holding IANA zone1970.tab. Usage: node gen-tables.js <dir>.
const scratch = (process.argv[2] || ".").replace(//?$/, "/");

// numberingSystemDigits
const m = harness.match(/var numberingSystemDigits = \{([\s\S]*?)\n\};/);
const digits = [];
for (const line of m[1].split("\n")) {
  const mm = line.match(/^\s*([a-z]+):\s*"([^"]+)"/);
  if (mm) digits.push([mm[1], eval("\""+mm[2]+"\"")]);
}
digits.sort((a, b) => (a[0] < b[0] ? -1 : 1));

// sanctioned units
const u = harness.match(/function allSimpleSanctionedUnits\(\) \{[\s\S]*?return \[([\s\S]*?)\];/);
const units = [...u[1].matchAll(/"([a-z-]+)"/g)].map((x) => x[1]).sort();

// zone1970.tab: cc list, coordinates, TZ, comments
const tab = fs.readFileSync(scratch + "zone1970.tab", "utf8");
const byRegion = new Map();
for (const line of tab.split("\n")) {
  if (!line || line.startsWith("#")) continue;
  const [ccs, , tz] = line.split("\t");
  for (const cc of ccs.split(",")) {
    if (!byRegion.has(cc)) byRegion.set(cc, []);
    byRegion.get(cc).push(tz);
  }
}
const regions = [...byRegion.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1));

let out = `//! Generated tables: do not edit by hand. Regenerate with the
//! \`gen-tables.js\` script described in ADR 0015.
//!
//! - \`NUMBERING_SYSTEM_DIGITS\`: ECMA-402's table of numbering systems with
//!   simple digit mappings (Unicode CLDR), as (name, ten digits).
//! - \`SANCTIONED_UNITS\`: the sanctioned simple unit identifiers.
//! - \`ZONES_BY_REGION\`: IANA zone1970.tab, (ISO 3166 region, zones).

pub static NUMBERING_SYSTEM_DIGITS: &[(&str, &str)] = &[
`;
for (const [k, v] of digits) out += `    ("${k}", "${v}"),\n`;
out += `];

pub static SANCTIONED_UNITS: &[&str] = &[
`;
for (const x of units) out += `    "${x}",\n`;
out += `];

pub static ZONES_BY_REGION: &[(&str, &[&str])] = &[
`;
for (const [cc, zones] of regions) out += `    ("${cc}", &[${zones.map((z) => `"${z}"`).join(", ")}]),\n`;
out += `];\n`;
fs.writeFileSync(root + "crates/gable-js-intl/src/tables.rs", out);
console.log(`digits ${digits.length}, units ${units.length}, regions ${regions.length}`);
