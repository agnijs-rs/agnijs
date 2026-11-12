# Binary size

What the engine costs in a shipped binary, which parts an embedder can leave out, and how to trim ECMA-402's locale data to an app's locales (TASK-103.5).

## Method

- **The binary.** `crates/agnijs-js-vm/examples/size_probe.rs` is a whole program: one realm, one script read from the command line, its completion value printed. The script is not known at build time, so every built-in stays reachable. The linker can drop only code that no script could reach. The parser, compiler, interpreter, GC, all built-ins, the regex engine and (by default) Intl are in it.
- **The debugger probes.** `crates/agnijs-js-vm/examples/size_probe_debugger.rs` is the same program made as a `Debuggee`. When `AGNIJS_PROBE_DEBUG` is set, it attaches a client that logs each pause, sets a breakpoint and pauses on exceptions. `crates/agnijs-js-cdp/examples/size_probe_cdp.rs` also starts the Chrome DevTools Protocol server when `AGNIJS_PROBE_INSPECT` names an address. Both decide at run time, so the whole debugger, and in the second case the whole adapter, stays reachable.
- **The comparison probes.** `tools/size-compare` is a workspace of its own, with its own `Cargo.lock`, so its pins never touch the engine's dependency graph. It holds the same program for QuickJS-NG (through `rquickjs` =0.14.0, the version the engine's QuickJS-NG backend and benchmarks lock, with no optional features) and for Boa (`boa_engine` =0.22.0 with `annex-b`, and optionally `intl_bundled`). It uses the same `size` profile.
- **The profile.** `[profile.size]` in the root `Cargo.toml` is the release profile with `debug = 0`, `strip = "symbols"`, `lto = "fat"` and `codegen-units = 1`. It keeps `panic = "unwind"`, so a C API can still turn a panic into an error with `catch_unwind`. `opt-level` stays at 3: these are the sizes of the build that runs at full speed. `opt-level = "s"` or `panic = "abort"` would shrink them further, and the second gives up the C API's panic safety.
- **The script.** `node scripts/size-report.mjs [--wasm] [--data name=<ICU4X_DATA_DIR> ...] [--debugger] [--compare] [--debugger-residue]` builds each probe in each configuration and prints the table, in MB and in bytes. Builds run one at a time, and each feature and data combination gets its own target directory under `$CARGO_TARGET_DIR/size-report`. The size is that of the stripped executable `cargo` writes. CI runs the script in the `size-profiles` job and adds the table to the job summary.
- **The debugger residue.** `--debugger-residue` (MSVC only) links the default probe once more with `/MAP` and adds up the symbols of `agnijs_js_debugger` and `agnijs_js_vm::debug` that are still in it. A symbol's size is the distance to the next symbol in its section. Code the optimizer inlined into other functions counts where it landed, so this is the debugger's out-of-line share only.
- **The platform.** The numbers below were measured on 2026-11-11 at commit 8dc7f41, with rustc 1.99.0 on x86_64-pc-windows-msvc. The trimmed data was generated with icu4x-datagen 2.4.0 through `crates/agnijs-js-intl/tools/gen-icu4x-data.js`. 1 MB is 10^6 bytes.

To reproduce them (the build takes about an hour and some 20 GB of target directories):

```
node crates/agnijs-js-intl/tools/gen-icu4x-data.js --out icu/six --locales en,de,fr,es,ja,zh
node crates/agnijs-js-intl/tools/gen-icu4x-data.js --out icu/six-nomodels --locales en,de,fr,es,ja,zh --segmenter-models none
node crates/agnijs-js-intl/tools/gen-icu4x-data.js --out icu/en-nomodels --locales en --segmenter-models none
node scripts/size-report.mjs --debugger --compare --debugger-residue \
  --data "6 locales=icu/six" --data "6 locales, no segmenter models=icu/six-nomodels" \
  --data "en, no segmenter models=icu/en-nomodels"
```

## Results

Native, x86_64-pc-windows-msvc, stripped:

| Configuration | Size | Bytes | of which Intl |
| --- | --- | --- | --- |
| Full: default features (`intl`, crate-default ICU4X data) | 20.47 MB | 20,469,760 | 16.84 MB |
| Full, debugger in use | 20.47 MB | 20,470,784 | 16.84 MB |
| Full, debugger and CDP server | 20.82 MB | 20,815,360 | 16.84 MB |
| No debugger (derived, see below) | at least 20.33 MB | 20,469,760 − at most 141,000 | 16.84 MB |
| Trimmed Intl: 6 locales (en, de, fr, es, ja, zh) | 9.41 MB | 9,408,000 | 5.78 MB |
| Trimmed Intl: 6 locales, no segmenter models | 7.09 MB | 7,086,080 | 3.46 MB |
| Trimmed Intl: en, no segmenter models | 6.34 MB | 6,341,120 | 2.71 MB |
| No Intl (`--no-default-features --features linear-regex`) | 3.63 MB | 3,627,520 | 0 |

- **Of which Intl** is the difference from the no-Intl build.
- **Full** is ICU4X's `recommended` locale set. On 2026-11-10 (commit deb40af), regenerating `recommended` with datagen built to the same size as the crate-default data, so the recipe below adds nothing of its own. An `en` build with the segmenter models was then 2.32 MB larger than one without them.
- **No debugger.** No cargo feature leaves the debugger out (see [Debugger and CDP](#debugger-and-cdp)), so this row is derived rather than built. The default probe does not use the debugger, so the linker has already dropped most of it. The linker map of the default probe still holds 548 debugger symbols with 141.0 kB of code and data. That is the most that leaving out the out-of-line debugger code would save; the debug hooks inlined into the interpreter are not counted. A program that uses the debugger costs 1 kB more than one that does not, and the CDP server 0.35 MB more.
- **wasm32-unknown-unknown.** The wasm32 builds were not re-measured here (the size-profiles CI job prints them with `--wasm`). On 2026-11-10 they were 18.75 MB full, 2.86 MB with no Intl and 5.29 MB with en and no segmenter models, as raw `cargo build` output with no `wasm-opt`: about 1.4 MB and 0.7 MB below the native sizes of the time.
- The engine core grew by 0.35 MB between deb40af (20.12 MB full, 3.28 MB with no Intl) and 8dc7f41: source maps, the CLI work and the fixes in between. The Intl shares did not change.

Where the default build's Intl bytes go, from the sizes ICU4X records on each baked marker (about 14 MB of data in all, the rest being code):

- time zone names: about 3.4 MB
- language, region and script display names (`Intl.DisplayNames`): about 3.3 MB
- currency names and patterns: about 2.3 MB for the extended currency data alone
- segmenter dictionaries and LSTM models (CJK, Thai, Lao, Khmer, Burmese word breaks): about 2.3 MB, measured as the build-size difference
- date range patterns for the non-Gregorian calendars: about 1.2 MB
- collation tailorings: about 0.9 MB

Almost all of these scale with the locale count, which is why a one-locale build saves so much.

### Comparison

The same probe (one context, evaluate the command-line script, print the result), built with the same `size` profile on the same machine and the same day:

| Engine | Intl | Size | Bytes |
| --- | --- | --- | --- |
| QuickJS-NG via `rquickjs` 0.14.0 (C, built by `cc` with MSVC) | none (QuickJS has no `Intl`) | 1.24 MB | 1,236,480 |
| Boa 0.22.0 (`annex-b`) | none | 10.81 MB | 10,810,880 |
| Boa 0.22.0 (`annex-b`, `intl_bundled`) | ICU4X, its bundled data | 25.99 MB | 25,994,752 |
| AgniJS, no Intl | none | 3.63 MB | 3,627,520 |
| AgniJS, en, no segmenter models | ICU4X, trimmed | 6.34 MB | 6,341,120 |
| AgniJS, full | ICU4X, recommended locales | 20.47 MB | 20,469,760 |

- QuickJS-NG is the smallest engine by far: about 2.4 MB less than AgniJS with no Intl, or about a third of its size. It has no ECMA-402, so a QuickJS app that needs locale formatting ships its own.
- With no Intl, AgniJS is about a third of Boa's size. With Intl, it is about 5.5 MB smaller than Boa with its bundled data. Trimmed to one locale, AgniJS with ECMA-402 is still smaller than Boa without it.
- Neither comparison engine builds for wasm32-unknown-unknown as configured here: `rquickjs-sys` needs a C compiler for wasm, and Boa's `getrandom` needs its `wasm_js` feature. Their wasm numbers are left out rather than built in a different setup.
- Hermes is not measured. It is C++ with a CMake build and has no Rust embedding to put in the same probe.

### Caveats

- These are the sizes of a whole minimal program. A real app adds its own code and shares the standard library, so the engine's share of an app is a little less than these sizes.
- Sizes vary by target and toolchain. These are Windows MSVC executables; ELF and Mach-O builds differ by a few percent, and the size-profiles CI job prints the Linux numbers.
- The QuickJS-NG probe compiles QuickJS-NG's C with the host's C compiler (here MSVC through `cc`). Clang or GCC at `-Os` would give a different size.
- The no-debugger row is a bound from a linker map, not a build. It is the out-of-line share only.

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

The Chrome DevTools Protocol adapter is its own crate (`agnijs-js-cdp`, ADR 0017). The engine does not depend on it, so an app links it only by naming it, and it is in none of the builds above except the "debugger and CDP server" row. Linking it and starting the server costs 0.35 MB.

The debugger interface (`agnijs-js-debugger`) and the VM's debug hooks are compiled into every build, and no cargo feature drops them. Measured, there is little to gain: a program that never makes a `Debuggee` still carries 141 kB of out-of-line debugger code (0.7% of the full build, 3.9% of the no-Intl build), and one that attaches a debugger costs 1 kB more. A feature that left the debugger out could save that 141 kB and the inlined hook checks, and would change engine code. That is why there is none yet.

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
