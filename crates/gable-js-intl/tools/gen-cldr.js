// Generate crates/gable-js-intl/src/cldr_tables.rs from CLDR JSON for the
// configured locale list (`locales` below). Usage: node gen-cldr.js <cldr-dir>.
// See ADR 0015.
const fs = require("fs");
const path = require("path");
// The engine workspace root (this script lives in crates/gable-js-intl/tools).
const root = path.resolve(__dirname, "../../..") + "/";
// A directory holding the CLDR JSON downloads: cldr-units/<loc>.json,
// cldr-numbers/<loc>.json, cldr-names/<loc>.json, cldr-datefields/<loc>.json
// (from the cldr-json npm packages of the same CLDR release ICU4X 2.4 uses).
const scratch = (process.argv[2] || ".").replace(//?$/, "/");
const locales = ["en", "en-GB", "en-IN", "de", "de-CH", "ja", "ko", "zh", "zh-Hant", "fr", "es", "it", "pt", "pt-PT", "ru", "ar", "hi"];
const harness = fs.readFileSync(root + "test262/harness/testIntl.js", "utf8");
const u = harness.match(/function allSimpleSanctionedUnits\(\) \{[\s\S]*?return \[([\s\S]*?)\];/);
const units = [...u[1].matchAll(/"([a-z-]+)"/g)].map((x) => x[1]).sort();
const q = (s) => JSON.stringify(s);
const widths = ["long", "short", "narrow"];

function findKey(obj, unit) {
  return Object.keys(obj).find((k) => {
    const i = k.indexOf("-");
    return i > 0 && k.slice(i + 1) === unit && !k.slice(0, i).includes("-");
  });
}
function counts(entry) {
  if (!entry) return null;
  const out = [];
  for (const [k, v] of Object.entries(entry)) {
    const m = k.match(/^unitPattern-count-(\w+)$/);
    if (m) out.push([m[1], v]);
  }
  return out.length ? out : null;
}

let out = `//! Generated tables from Unicode CLDR JSON: do not edit by hand. Regenerate
//! with the \`gen-cldr.js\` script described in ADR 0015. The locale list is
//! the crate's bundled-unit-data configuration; other locales fall back to
//! their language, then to \`en\`.
//!
//! - \`UNIT_LOCALES\`: per locale, the \`per\` compound patterns (long, short,
//!   narrow) and each sanctioned unit's plural patterns per width.
//! - \`NUMBER_MISC\`: per locale and numbering system, the symbols ICU4X's
//!   decimal data does not carry (percent sign, exponent, infinity, NaN) and
//!   the approximately/range patterns.

pub struct UnitLocale {
    pub per: [&'static str; 3],
    pub units: &'static [(&'static str, [&'static [(&'static str, &'static str)]; 3])],
}

pub struct NumberMisc {
    pub percent_sign: &'static str,
    pub exponential: &'static str,
    pub infinity: &'static str,
    pub nan: &'static str,
    pub approximately: &'static str,
    pub range: &'static str,
}

pub struct DisplayNamesLocale {
    pub calendar: &'static [(&'static str, &'static str)],
    pub fields: &'static [(&'static str, [&'static str; 3])],
}

pub static UNIT_LOCALES: &[(&str, UnitLocale)] = &[
`;
let unitCount = 0;
for (const loc of locales) {
  const j = JSON.parse(fs.readFileSync(`${scratch}cldr-units/${loc}.json`, "utf8")).main[loc].units;
  const per = widths.map((w) => j[w].per.compoundUnitPattern);
  const all = [...units];
  for (const a of units) for (const b of units) if (findKey(j.long, `${a}-per-${b}`)) all.push(`${a}-per-${b}`);
  out += `    (${q(loc)}, UnitLocale {\n        per: [${per.map(q).join(", ")}],\n        units: &[\n`;
  for (const unit of all) {
    const ws = widths.map((w) => counts(j[w][findKey(j[w], unit)]));
    if (ws.some((x) => !x)) continue;
    unitCount++;
    out += `            (${q(unit)}, [${ws.map((c) => `&[${c.map(([k, v]) => `(${q(k)}, ${q(v)})`).join(", ")}]`).join(", ")}]),\n`;
  }
  out += `        ],\n    }),\n`;
}
out += `];\n\npub static NUMBER_MISC: &[(&str, &[(&str, NumberMisc)])] = &[\n`;
for (const loc of locales) {
  const n = JSON.parse(fs.readFileSync(`${scratch}cldr-numbers/${loc}.json`, "utf8")).main[loc].numbers;
  const systems = Object.keys(n).filter((k) => k.startsWith("symbols-numberSystem-")).map((k) => k.slice("symbols-numberSystem-".length));
  out += `    (${q(loc)}, &[\n`;
  for (const nu of systems) {
    const s = n[`symbols-numberSystem-${nu}`];
    const m = n[`miscPatterns-numberSystem-${nu}`] || n["miscPatterns-numberSystem-latn"];
    out += `        (${q(nu)}, NumberMisc { percent_sign: ${q(s.percentSign)}, exponential: ${q(s.exponential)}, infinity: ${q(s.infinity)}, nan: ${q(s.nan)}, approximately: ${q(m.approximately)}, range: ${q(m.range)} }),\n`;
  }
  out += `    ]),\n`;
}
// Appended to gen-cldr.js: DISPLAY_NAMES table.
out += `];\n\npub static DISPLAY_NAMES: &[(&str, DisplayNamesLocale)] = &[\n`;
const fieldMap = { era: "era", year: "year", quarter: "quarter", month: "month", weekOfYear: "week", weekday: "weekday", day: "day", dayPeriod: "dayperiod", hour: "hour", minute: "minute", second: "second", timeZoneName: "zone" };
for (const loc of locales) {
  const d = JSON.parse(fs.readFileSync(`${scratch}cldr-names/${loc}.json`, "utf8")).main[loc].localeDisplayNames.types.calendar;
  const f = JSON.parse(fs.readFileSync(`${scratch}cldr-datefields/${loc}.json`, "utf8")).main[loc].dates.fields;
  out += `    (${q(loc)}, DisplayNamesLocale {\n        calendar: &[`;
  const cal = Object.entries(d).filter(([k]) => k !== "_core" && !k.includes("-alt-")).sort();
  out += cal.map(([k, v]) => `(${q(k)}, ${q(v)})`).join(", ");
  out += `],\n        fields: &[`;
  out += Object.entries(fieldMap).map(([code, key]) => `(${q(code)}, [${q(f[key].displayName)}, ${q((f[key + "-short"] || f[key]).displayName)}, ${q((f[key + "-narrow"] || f[key + "-short"] || f[key]).displayName)}])`).join(", ");
  out += `],\n    }),\n`;
}
out += `];\n`;
fs.writeFileSync(root + "crates/gable-js-intl/src/cldr_tables.rs", out);
console.log(`locales ${locales.length}, unit entries ${unitCount}`);
