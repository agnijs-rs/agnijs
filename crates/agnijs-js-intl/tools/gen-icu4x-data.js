// Generate a custom ICU4X baked-data directory for a chosen locale list, for
// an embedder who wants a smaller engine (docs/binary-size.md). Build the
// engine with ICU4X_DATA_DIR pointing at the output and every ICU4X data
// crate (and this crate's own provider) compiles that data instead of the
// crate-default set.
//
// Usage:
//   node gen-icu4x-data.js --out <dir> --locales en,de,fr [--segmenter-models none]
//        [--datagen <path to icu4x-datagen>]
//
// --locales is a comma list of BCP 47 tags or one of ICU4X's sets
// (recommended, which is what the crate-default data holds; modern; full).
// --segmenter-models is `recommended` (the default: the CJK dictionary and
// the Southeast Asian LSTM models, as the crate-default data) or `none`
// (Intl.Segmenter keeps the rule-based breaks only: no word breaks inside
// Chinese, Japanese, Thai, Lao, Khmer or Burmese runs).
//
// It needs icu4x-datagen 2.4 built with the `unstable` feature, since the
// seam uses unstable markers (currency, relative time, display names):
//   cargo install icu4x-datagen --version ~2.4.0 --locked --features unstable
// Datagen downloads the CLDR, ICU export, tzdb and LSTM sources it needs from
// GitHub and unicode.org on its first run.
//
// The output directory also gets `agnijs-locales.txt`, the requested list,
// which the engine's intl_feature test reads to check the build holds only
// those locales.
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  if (i < 0) return fallback;
  const v = process.argv[i + 1];
  if (v === undefined || v.startsWith("--")) throw new Error(`${name} needs a value`);
  return v;
}

const out = arg("--out");
if (!out) {
  console.error("usage: node gen-icu4x-data.js --out <dir> --locales en,de [--segmenter-models none] [--datagen <exe>]");
  process.exit(2);
}
const locales = arg("--locales", "recommended").split(",").map((s) => s.trim()).filter(Boolean);
const models = arg("--segmenter-models", "recommended").split(",").map((s) => s.trim()).filter(Boolean);
const datagen = arg("--datagen", "icu4x-datagen");

const version = execFileSync(datagen, ["--version"], { encoding: "utf8" }).trim();
if (!/ 2\.4\.\d+$/.test(version)) {
  throw new Error(`need icu4x-datagen 2.4.x to match the engine's ICU4X, found "${version}"`);
}

execFileSync(
  datagen,
  ["--format", "baked", "--markers", "all", "--locales", ...locales, "--segmenter-models", ...models, "--out", out, "--overwrite"],
  { stdio: ["ignore", "inherit", "inherit"] },
);

// `--markers all` only covers the stable markers unless datagen was built
// with `unstable`; the build would then fail far from the cause.
for (const marker of ["currency_extended_data_v1", "datetime_relative_day_long_v1", "plurals_ranges_v1"]) {
  if (!fs.existsSync(path.join(out, `${marker}.rs.data`))) {
    throw new Error(`${marker} missing from ${out}: rebuild icu4x-datagen with --features unstable`);
  }
}
fs.writeFileSync(path.join(out, "agnijs-locales.txt"), locales.join("\n") + "\n");
console.log(`\nWrote ${out}. Build the engine with:\n  ICU4X_DATA_DIR=${path.resolve(out)} cargo build --release`);
