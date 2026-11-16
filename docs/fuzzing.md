# Fuzzing

AgniJS runs code it does not trust, so every parser and loader of outside input is fuzzed. This page lists the fuzzers, says how to run each one for a short bounded run or a long one, and what runs in CI (TASK-103.18).

## What a fuzz target checks

Each target is a function of one byte string in `crates/agnijs-fuzz/src/targets.rs`. It returns normally for any input the component may refuse or fail on: a syntax error, a thrown exception, a refused container, the fuel or memory limit. It panics when the input breaks a promise:

- a panic anywhere in the engine;
- an engine left poisoned or unusable after the input (each engine target finishes by collecting garbage and evaluating `6 * 7`);
- output of our own that does not load back: a compiled code tree or bytecode container that does not decode, or decodes differently;
- two implementations that disagree: the backtracking and linear regex matchers, or a regex program and its reloaded bytes;
- a CDP reply that is not valid JSON.

Every engine the targets create has a fuel budget, an interrupt that fires after a fixed number of polls and a 64 MiB memory limit, so no input can hang a run.

## The targets

| Target | Input | What it exercises |
| --- | --- | --- |
| `parse` | JavaScript | The parser, as a script and as a module, under the native stack budget |
| `compile` | JavaScript | Parse and compile; the code tree and the bytecode container must round-trip |
| `eval` | JavaScript | The engine interface: scripts, or modules (input starting `//@module`) with static and dynamic imports, top-level await and the job queue |
| `json` | JSON text | `JSON.parse` and `JSON.stringify`; `stringify(parse(x))` must be a fixed point |
| `intl` | Lines: locale, two options, text | The `Intl` constructors and locale-sensitive methods with fuzzed locales and options |
| `temporal` | Lines: a duration, a unit, a rounding mode, `relativeTo`, a number | `Temporal` parsing, construction, rounding, totals and comparison with fuzzed strings and options |
| `regex` | Lines: flags, pattern, subject | Regex syntax and matching under all three matcher policies, which must agree; compiled programs must reload |
| `regex-program` | Bytes | The verifier of compiled regex programs, which bytecode carries |
| `bytecode` | Bytes | The bytecode loader past the checksum (the input is resealed), then running whatever loads |
| `snapshot` | Bytes | The snapshot loader past the tag, fingerprint and checksum, then a probe script |
| `cdp` | Lines of JSON | DevTools protocol messages against a debuggee, before and while paused at `debugger;` |
| `cdp-frames` | Bytes | The WebSocket frame reader of the CDP server, with its size limit |
| `source-map` | JSON or a `data:` URL | Source Map v3 parsing and lookups, base64 and percent decoding |
| `import-map` | Lines: a map, then specifiers | Import map parsing and specifier resolution |

The `bytecode` and `snapshot` targets reseal their input (they recompute the length and checksum after mutation), because a checksum would otherwise refuse almost every mutant before the decoder sees it. Bytecode and snapshots are trusted input ([security-model.md](security-model.md#bytecode-and-snapshots-are-trusted-input)): what these targets hold the loader to is that a container that loads cannot panic the engine, not that it computes anything meaningful.

## Running the bounded fuzzer

`agnijs-fuzz` (in `crates/agnijs-fuzz`) is a seeded mutating fuzzer that needs only stable Rust. It runs each target's seed inputs, then mutants of them: text targets get token, line and character edits, binary targets byte and field edits. Mutants that a target accepts (that parse, load or compile) are kept and mutated further. It is not coverage-guided, so it finds shallow bugs fast and is fully reproducible: the same seed and iteration count run the same inputs on every platform.

```sh
# Every target at its default size (about 30 s in release), seed 1. This is what CI runs.
cargo run --profile fuzz -p agnijs-fuzz -- all

# Some targets, longer: 10 times the default, or for 10 minutes each, with another seed.
cargo run --profile fuzz -p agnijs-fuzz -- parse regex --scale 10 --seed 7
cargo run --profile fuzz -p agnijs-fuzz -- bytecode --time 600

# List the targets and their default iteration counts.
cargo run --profile fuzz -p agnijs-fuzz -- --list
```

The `fuzz` profile is release with debug assertions and overflow checks on, so a broken invariant panics instead of wrapping. Plain `--release` works too and is a little faster.

When an input fails, the runner prints the panic, saves the input to `target/fuzz-inputs/<target>-seed<N>-iter<I>.txt` (or `.bin`), prints the command that reproduces it, and exits 1:

```sh
cargo run --profile fuzz -p agnijs-fuzz -- --repro bytecode target/fuzz-inputs/bytecode-seed1-iter812.bin
```

`--out DIR` puts failing inputs elsewhere. `crates/agnijs-fuzz/tests/smoke.rs` runs every target's seeds and a few mutants as part of `cargo test`, so the targets themselves cannot rot.

## Running libFuzzer

The `fuzz/` directory is a cargo-fuzz project that hands libFuzzer's inputs to the same target functions, for open-ended coverage-guided runs under AddressSanitizer. It is a workspace of its own, excluded from the engine's, because it needs nightly Rust and a C++ toolchain. cargo-fuzz supports Linux and macOS.

```sh
cargo install cargo-fuzz
cp Cargo.lock fuzz/Cargo.lock          # use the engine's dependency versions
cargo run --release -p agnijs-fuzz -- --seeds parse fuzz/corpus/parse   # start from the seeds
cargo +nightly fuzz run --debug-assertions parse fuzz/corpus/parse -- -max_total_time=600 -timeout=20 -rss_limit_mb=4096
```

The target names are those in the table above. Set `ASAN_OPTIONS=detect_stack_use_after_return=0`: AddressSanitizer's fake stacks move locals off the thread's stack, and the engine's native stack budget measures depth from a local's address. A crash is saved under `fuzz/artifacts/<target>/`; reproduce it with `cargo +nightly fuzz run <target> <file>`, or on stable with the bounded runner's `--repro`.

## Other fuzzers

These predate `agnijs-fuzz` and stay where they are, close to what they test:

| Fuzzer | What it does | Bounded by | In CI |
| --- | --- | --- | --- |
| `crates/agnijs-js-regex/tests/differential.rs` | Random patterns and subjects; the linear matcher against the backtracker (ADR 0013) | `AGNIJS_REGEX_FUZZ_CASES` | 200,000 cases, Ubuntu |
| `crates/agnijs-capi/tests/fuzz.rs` | Random C API call sequences; mutated scripts run through the C API and the Rust engine, compared; mutated bytecode | `AGNIJS_CAPI_FUZZ_ITERS`, `AGNIJS_CAPI_FUZZ_SEED` | 10x, all three platforms |
| `crates/agnijs-js-vm/tests/bytecode_fuzz.rs` | Crafted script, module and snapshot code, resealed and run, also with the JIT | `BYTECODE_FUZZ_ITERS`, `BYTECODE_FUZZ_SEED` | With `jit`, all three platforms |
| `crates/agnijs-js-vm/tests/snapshot.rs` (`fuzzed_snapshots_load_or_fail_without_panicking`) | Damaged snapshots | `SNAPSHOT_FUZZ_ITERS` | Default size |
| `crates/agnijs-js-compiler/tests/serialize.rs` (`corrupted_bytes_never_panic`) | Damaged bytecode trees | Fixed | Default size |
| `tools/jit-fuzz` | Generated programs run by the interpreter and by the JIT (on and forced), compared ([jit-fuzz.md](jit-fuzz.md)) | `--count`, `--time` | 40 programs, Ubuntu |

## What runs in CI

- **Every push and pull request** (`ci.yml`): the `fuzz` job runs `agnijs-fuzz all --seed 1` in the `fuzz` profile, about a minute after the build. The seed and iteration counts are fixed, so the job runs the same inputs every time and is not flaky; a failure is a real bug in the change, or one the change exposed. Failing inputs are uploaded as the `fuzz-failures` artifact. The fuzzers in the table above run in their own jobs.
- **Nightly and on demand** (`fuzz.yml`): each libFuzzer target runs for 5 minutes (the `seconds` input of a manual run changes it), starting from the seeds and the corpus the previous run saved in the Actions cache, under AddressSanitizer with debug assertions. A crash fails that target's job and uploads `fuzz/artifacts/<target>`. These runs are not reproducible and do not gate merges; a failure there is a bug to file.

AgniJS is not in OSS-Fuzz. The `fuzz/` targets are in the form OSS-Fuzz builds, so applying is a matter of a project file and a build script once the project wants it.

## Results

The first runs, in November 2026 (TASK-103.18), found no crashes: the default run, and 20 times the default with another seed, over every target. The surface review of the same month (`docs/surface-review-2026-11.md`) had already found and fixed the panics a hand review could find (TASK-103.37 and TASK-103.41 to TASK-103.43). Bugs found later are filed as backlog tasks and fixed with a regression test; the failing input goes into the test.

## Adding a target

1. Write the function in `crates/agnijs-fuzz/src/targets.rs`: return for every outcome the component may legitimately produce, panic on a broken promise, and give any engine fuel, an interrupt and a memory limit (`engine()`, `limit()`).
2. Add seeds in `src/seeds.rs` and a `Target` entry in `src/lib.rs`, with a default iteration count that keeps the target to a few seconds in release.
3. Add `fuzz/fuzz_targets/<name>.rs` and a `[[bin]]` in `fuzz/Cargo.toml`, and the name to the matrix in `.github/workflows/fuzz.yml`.
4. Run it for a while (`--scale 20`) before you push, and fix what it finds.
