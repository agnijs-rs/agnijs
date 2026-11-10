# AgniJS

A pure-Rust, embeddable JavaScript engine: full ES2025 (test262-complete, with the Annex B parts browsers rely on) and ECMA-402 `Intl` on ICU4X, with a bytecode interpreter, precise garbage collector, ES modules, a debugger interface and a Chrome DevTools Protocol adapter. No JIT, no C dependencies in the engine crates, one build on Windows, macOS, Linux and Android.

AgniJS began as the engine of [Gable](https://github.com/jcannon98188/gable) Native and was extracted into this repository with its full history (TASK-100.12). Gable depends on it by pinned git revision; it is meant to be useful to any embedder.

> Status: pre-release. The embedding surface is the engine interface in `agnijs-engine-api` (a handle-based, host-driven trait, ADR 0006). A generic embedding API, semver policy and embedder docs are tracked in the backlog (TASK-104). Nothing is published to crates.io yet and the crate names may change before then.

## Conformance

test262 is the correctness gate. The pinned commit is in `crates/agnijs-js-test262/PIN`; baselines live in `crates/agnijs-js-test262/baselines/` and exclusions, each with a reason, in `exclusions/`.

| Suite | Result |
| --- | --- |
| test262, source scripts and modules | 42231 / 42231 |
| test262, every script and module run from bytecode (`--precompile`) | 42231 / 42231 |
| test262 `intl402` (reported separately) | 981 / 981 |

## Layout

| Crate | Contents |
| --- | --- |
| `agnijs-engine-api` | The engine interface and its conformance test kit |
| `agnijs-js-parser` | Lexer, parser, AST, early errors |
| `agnijs-js-compiler` | AST to bytecode, bytecode format and serialization |
| `agnijs-js-vm` | Interpreter, object model, heap and GC, built-ins, jobs, modules |
| `agnijs-js-regex` | Our own regex engine behind the seam: parser, backtracking matcher, linear-time matcher for ReDoS-safe patterns, serializable programs |
| `agnijs-js-intl` | The ICU4X seam and ECMA-402 services |
| `agnijs-js-debugger` | The debugger interface |
| `agnijs-js-cdp` | The Chrome DevTools Protocol adapter |
| `agnijs-js-test262` | The test262 runner, baselines and the ADR 0016 boundary tests |
| `agnijs-js-bench` | Micro-benchmarks and the benchmark protocol |
| `agnijs-engine-quickjs` | An adapter over QuickJS-NG, used as a comparison engine |

The specification is [`docs/js-engine-spec.md`](docs/js-engine-spec.md); decisions are recorded as ADRs in [`docs/adr`](docs/adr). Performance findings are in [`docs/js-engine-performance-findings.md`](docs/js-engine-performance-findings.md).

## Building and testing

```
cargo build --workspace
cargo test --workspace --locked
cargo run --release -p agnijs-js-test262 --bin test262 -- fetch                       # clone test262 at the pinned commit
cargo run --release -p agnijs-js-test262 --bin test262 -- run --engine agnijs         # main baseline
cargo run --release -p agnijs-js-test262 --bin test262 -- run --engine agnijs --precompile
cargo run --release -p agnijs-js-test262 --bin test262 -- run --engine agnijs --only-intl402
node scripts/check-licenses.mjs                                                       # dependency license check
```

## Features and binary size

`agnijs-js-vm`'s default features are `intl` (ECMA-402 on ICU4X) and `linear-regex`. Building without `intl` drops the `Intl` global and all of ICU4X: about 3 MB instead of about 20 MB, stripped. A custom ICU4X data directory trimmed to an app's locales sits in between. [`docs/binary-size.md`](docs/binary-size.md) has the numbers, what each profile drops, the data-trimming recipe and a comparison with QuickJS-NG and Boa.

Rust 1.85 or newer. Work is tracked with [Backlog.md](https://github.com/MrLesk/Backlog.md) in `backlog/`; see `AGENTS.md`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option. Every crate in this workspace declares `license = "MIT OR Apache-2.0"`. Third-party notices, including the Unicode License for the ICU4X data, are in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
