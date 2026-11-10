# Binary size

What the engine costs in a shipped binary, which parts an embedder can leave out, and how to trim ECMA-402's locale data to an app's locales (TASK-103.5).

## Method

- **The binary.** `crates/agnijs-js-vm/examples/size_probe.rs` is a whole program: one realm, one script read from the command line, its completion value printed. The script is not known at build time, so every built-in stays reachable. The linker can drop only code that no script could reach. The parser, compiler, interpreter, GC, all built-ins, the regex engine and (by default) Intl are in it.
- **The profile.** `[profile.size]` in the root `Cargo.toml` is the release profile with `debug = 0`, `strip = "symbols"`, `lto = "fat"` and `codegen-units = 1`. It keeps `panic = "unwind"`, so a C API can still turn a panic into an error with `catch_unwind`. `opt-level` stays at 3: these are the sizes of the build that runs at full speed. `opt-level = "s"` or `panic = "abort"` would shrink them further, and the second gives up the C API's panic safety.
- **The script.** `node scripts/size-report.mjs [--wasm] [--data name=<ICU4X_DATA_DIR> ...]` builds the probe in each configuration, each in its own target directory under `$CARGO_TARGET_DIR/size-report`, and prints the table. CI runs it in the `size-profiles` job and adds the table to the job summary.
- **The platform.** The numbers below were measured on 2026-11-10 with rustc 1.99.0, for x86_64-pc-windows-msvc and wasm32-unknown-unknown. The wasm file is the raw `cargo build` output, with no `wasm-opt`. 1 MB is 10^6 bytes.

## Results

| Configuration | native (win-x64) | wasm32 | of which Intl (native) |
| --- | --- | --- | --- |
| Default: `intl`, crate-default ICU4X data | 20.12 MB | 18.75 MB | 16.84 MB |
| `intl`, datagen `recommended` (the same set, regenerated) | 20.12 MB | 18.75 MB | 16.84 MB |
| `intl`, 6 locales (en, de, fr, es, ja, zh) | 9.06 MB | 8.31 MB | 5.78 MB |
| `intl`, 6 locales, no segmenter models | 6.74 MB | 5.99 MB | 3.46 MB |
| `intl`, en | 8.31 MB | 7.61 MB | 5.03 MB |
| `intl`, en, no segmenter models | 5.99 MB | 5.29 MB | 2.71 MB |
| No Intl (`--no-default-features --features linear-regex`) | 3.28 MB | 2.86 MB | 0 |

The "of which Intl" column is the difference from the no-Intl build. Regenerating `recommended` with datagen builds to the same size as the crate-default data, which shows that the recipe below has no overhead of its own.

Where the default build's Intl bytes go, from the sizes ICU4X records on each baked marker (about 14 MB of data in all, the rest being code):

- time zone names: about 3.4 MB
- language, region and script display names (`Intl.DisplayNames`): about 3.3 MB
- currency names and patterns: about 2.3 MB for the extended currency data alone
- segmenter dictionaries and LSTM models (CJK, Thai, Lao, Khmer, Burmese word breaks): about 2.3 MB, measured as the build-size difference
- date range patterns for the non-Gregorian calendars: about 1.2 MB
- collation tailorings: about 0.9 MB

Almost all of these scale with the locale count, which is why a one-locale build saves so much.

### Comparison

The same probe (one context, evaluate the command-line script, print the result), built with the same `size` profile on the same machine:

| Engine | Intl | native (win-x64) |
| --- | --- | --- |
| QuickJS-NG via `rquickjs` 0.14 (C, built by `cc`) | none (QuickJS has no `Intl`) | 1.21 MB |
| Boa 0.22 (`annex-b`) | none | 10.81 MB |
| Boa 0.22 (`annex-b`, `intl_bundled`) | ICU4X, its bundled data | 25.99 MB |
| AgniJS, no Intl | none | 3.28 MB |
| AgniJS, en, no segmenter models | ICU4X, trimmed | 5.99 MB |
| AgniJS, default | ICU4X, recommended locales | 20.12 MB |

- QuickJS-NG is the smallest engine by far: about 2 MB less than AgniJS without Intl. It has no ECMA-402, so a QuickJS app that needs locale formatting ships its own.
- Without Intl, AgniJS is about a third of Boa's size. With Intl, it is about 6 MB smaller than Boa with its bundled data. Trimmed to one locale, AgniJS with ECMA-402 is still smaller than Boa without it.
- Neither comparison engine builds for wasm32-unknown-unknown as configured here: `rquickjs-sys` needs a C compiler for wasm, and Boa's `getrandom` needs its `wasm_js` feature. Their wasm numbers are left out rather than built in a different setup.
- Hermes is not measured. It is C++ with a CMake build and has no Rust embedding to put in the same probe.

## Profiles

### Default: `intl` with the crate-default data

The full ECMA-402 implementation on ICU4X's `recommended` locale set (CLDR's modern, moderate and basic coverage levels). test262 `intl402` passes 981/981 and the main suite 42231/42231, for scripts and for bytecode (`--precompile`).

### No Intl: `default-features = false`

```toml
agnijs-js-vm = { version = "...", default-features = false, features = ["linear-regex"] }
```

What it drops:

- **The `Intl` global** and all of ICU4X's formatting data. `agnijs-js-intl` is not linked. The only ICU4X data left is the normalizer (`String.prototype.normalize` is ECMA-262).
- **The locale-sensitive methods' ECMA-402 behaviour.** They take what ECMA-262 alone specifies, and they ignore their `locales` and `options` arguments without validating them:
  - Number and BigInt `toLocaleString` return what `toString` returns.
  - Date `toLocaleString`, `toLocaleDateString` and `toLocaleTimeString` return the `toString`, `toDateString` and `toTimeString` forms.
  - `String.prototype.localeCompare` compares the NFC forms by code point, so canonically equivalent strings compare equal.
  - `toLocaleUpperCase` and `toLocaleLowerCase` use the root case mapping, so Turkish dotless i is not applied.
  - Array and typed-array `toLocaleString` call each element's `toLocaleString` with no arguments.

The C API crate passes the same features through: build `agnijs-capi` with `--no-default-features --features linear-regex`.

test262's main suite passes 42231/42231 in this build, for scripts and for bytecode. `intl402` does not apply. `crates/agnijs-js-vm/tests/intl_feature.rs` checks each of the behaviours above.

### Trimmed Intl data: `ICU4X_DATA_DIR`

The engine keeps the whole ECMA-402 API and holds CLDR data only for the locales an app names. ICU4X has the mechanism built in: when the `ICU4X_DATA_DIR` environment variable is set at build time, every ICU4X data crate (and the seam's own provider in `agnijs-js-intl`) compiles the baked data in that directory in place of its default set.

What it drops:

- **Locales not in the list.** A locale the data lacks falls back to the root locale: `new Intl.NumberFormat('sw')` works and formats in the root style. `supportedLocalesOf` reports only the locales the build has. Nothing throws, so code written for a wider build degrades to the default style instead of failing.
- **With `--segmenter-models none`**, the CJK dictionaries and the Southeast Asian LSTM models. `Intl.Segmenter` with `granularity: "word"` then finds no word breaks inside runs of Chinese, Japanese, Thai, Lao, Khmer or Burmese. Grapheme and sentence segmentation and every other script are unaffected. This saves about 2.3 MB whatever the locale list.

Kept on purpose: every calendar, numbering system and collation of the locales in the list. The seam's formatters take any calendar or numbering system a tag names (`-u-ca-`, `-u-nu-`), and `Intl.supportedValuesOf` reports them, so dropping them would change the API rather than the data.

The bundled tables in `agnijs-js-intl/src/cldr_tables.rs` (unit and currency names for 17 locales, which ICU4X does not provide) are compiled whatever the data directory. They are about 0.2 MB.

### Debugger and CDP

The Chrome DevTools Protocol adapter is its own crate (`agnijs-js-cdp`, ADR 0017). The engine does not depend on it, so an app links it only by naming it, and it is in none of the builds above. The debugger interface (`agnijs-js-debugger`, about 740 lines of plain types) and the VM's debug hooks are compiled into every build. No cargo feature drops them yet, so there is no "no debugger" row.

## Recipe: trimming the data

1. Install icu4x-datagen at the engine's ICU4X version, with the `unstable` markers the seam uses (currency, relative time, display names):

   ```
   cargo install icu4x-datagen --version ~2.4.0 --locked --features unstable
   ```

2. Generate the data for the app's locales:

   ```
   node crates/agnijs-js-intl/tools/gen-icu4x-data.js --out path/to/icu-data --locales en,de,fr [--segmenter-models none]
   ```

   `--locales` takes BCP 47 tags, or one of ICU4X's sets: `recommended` (what the default build has), `modern` or `full`. The script checks the datagen version, runs `icu4x-datagen --format baked --markers all`, checks the unstable markers are there, and writes the list to `agnijs-locales.txt` beside the data. On its first run datagen downloads the CLDR, ICU export, tzdb and segmenter-model sources from GitHub and unicode.org.

3. Build the app with the variable set:

   ```
   ICU4X_DATA_DIR=/absolute/path/to/icu-data cargo build --release
   ```

   The variable applies to the whole build, so the app's other ICU4X users (Parley, for example) get the same data. Cargo does not track the directory's contents. After regenerating the data into the same path, run `cargo clean` or point the variable at a new directory.

4. Check it: `cargo test -p agnijs-js-vm --test intl_feature` with the same variable set confirms that `supportedLocalesOf` reports exactly the generated locales.

## What CI checks

The `size-profiles` job in `.github/workflows/ci.yml` runs on Linux:

- **No Intl:** the `intl_feature` tests and the C API tests, then the test262 main suite against the main baseline, for scripts and for bytecode.
- **Trimmed data:** generates `en,de` data with no segmenter models, then runs the `intl_feature` tests and the test262 main suite against it.
- **wasm32-unknown-unknown:** builds the probe with and without Intl.
- **Size report:** prints the table above, with the trimmed row added, into the job summary.

The default profile's main, `--precompile` and `intl402` baselines run in the existing jobs.

Measured locally on the same commit, beyond what CI runs: `intl402` passes 981/981 on datagen's `recommended` and `modern` sets, and the main suite passes 42231/42231 on English-only data.
