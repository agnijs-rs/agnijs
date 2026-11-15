---
id: TASK-103.18
title: Fuzzing, disclosure policy and stability policy
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-15T20:29:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 274000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Build public trust for an engine that runs untrusted code: continuous fuzzing (OSS-Fuzz or equivalent) with a structure-aware JavaScript fuzzer such as Fuzzilli plus parser, bytecode loader and regex fuzz targets; a security disclosure policy (SECURITY.md); and a semver and stability policy covering the Rust API, the C API ABI and the bytecode format. Keep local fuzz runs bounded, because the maintainer machine is resource constrained. Model: Opus for the fuzz harnesses, Sonnet for the policy documents.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Fuzz targets exist for the parser, the compiler, the bytecode loader, the regex engines and the engine interface, and run continuously in CI or OSS-Fuzz
- [x] #2 SECURITY.md and a stability policy are published, and any crash found is fixed with a regression test
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Inventory existing fuzzing (regex differential, C API fuzz, bytecode/snapshot crafted-input tests, JIT differential) and the bug classes the Nov 2026 reviews found.
2. New crate crates/agnijs-fuzz (publish = false): one function per target (parse, compile, bytecode, snapshot, regex, eval, json, intl, cdp, source-map, import-map), a seeded mutating runner binary with bounded defaults (--iters/--time/--seed, crash inputs saved to target/fuzz), and a smoke test.
3. fuzz/ cargo-fuzz project (own workspace) wrapping the same functions for coverage-guided libFuzzer runs.
4. CI: a bounded, fixed-iteration run of every target in ci.yml; a scheduled fuzz.yml running cargo-fuzz per target with a cached corpus and crash artifacts.
5. Fix any crash found (test-first) or file a task.
6. docs/fuzzing.md, SECURITY.md (TODO contact placeholder), docs/stability.md aligned with c-api.md and ADR 0010/0021/0022; update security-model.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Inventory (before this task): regex differential test (agnijs-js-regex tests/differential.rs), C API fuzz (agnijs-capi tests/fuzz.rs), crafted bytecode/snapshot tests (agnijs-js-vm tests/bytecode_fuzz.rs, snapshot.rs; agnijs-js-compiler tests/serialize.rs), source map hostile_maps test, and the JIT differential fuzzer (tools/jit-fuzz). None coverage-guided; no target for the parser, compiler, JSON, Intl, CDP messages, WebSocket frames or import maps.

Added crates/agnijs-fuzz (publish = false, forbid(unsafe_code)): 13 targets, one function of a byte string each (parse, compile, eval, json, intl, regex, regex-program, bytecode, snapshot, cdp, cdp-frames, source-map, import-map). A target panics only on a broken promise: an engine panic, a poisoned or unusable engine afterwards, our own output that does not decode back, matchers that disagree, a CDP reply that is not JSON. Every engine has fuel, an interrupt and a 64 MiB limit. bytecode and snapshot reseal their input so mutants get past the checksum. The agnijs-fuzz binary is a seeded mutating runner (stable Rust; --iters/--scale/--time/--seed/--out, --repro, --seeds, --list); failing inputs go to target/fuzz-inputs (not target/fuzz, which the `fuzz` profile builds into). fuzz/ is a cargo-fuzz project (own workspace, excluded from the engine's) with the same 13 targets for libFuzzer.

CI: new `fuzz` job in ci.yml (ubuntu, `fuzz` profile = release + debug-assertions + overflow-checks, `all --seed 1`, fixed iteration counts, so deterministic; ~30 s of fuzzing after a ~1.5 min build locally; failing inputs uploaded). New fuzz.yml: nightly cron + workflow_dispatch, matrix of the 13 targets, cargo-fuzz under ASan with --debug-assertions, 300 s per target by default, corpus kept in the Actions cache and minimised, crash artifacts uploaded. Not OSS-Fuzz; the targets are in the form OSS-Fuzz builds.

Results: default run (seed 1, fuzz profile) and a 20x run (seed 2, release, about 6.2 M inputs) over all 13 targets found no crashes, so there was nothing to fix or file. Validation: clippy -D warnings --no-deps on agnijs-fuzz (all targets), cargo test -p agnijs-fuzz (debug), the CI command run locally, `cargo +nightly check --bins` in fuzz/ (cargo-fuzz itself cannot run on this Windows machine; the first fuzz.yml run is its real test), both workflows parsed as YAML. No engine code changed.

Docs: docs/fuzzing.md (targets, how to run bounded/long/libFuzzer, other fuzzers, CI, results, adding a target), SECURITY.md, docs/stability.md (C ABI from c-api.md and ADR 0021; Rust API: no promise yet, policy deferred to TASK-104 AC #5; bytecode and snapshot tags per ADR 0010/0022; CLI; CDP; MSRV). docs/security-model.md and README now link them.

Owner decisions needed:
1. SECURITY.md security contact: placeholder TODO-security-contact@example.invalid, marked TODO(owner).
2. GitHub private vulnerability reporting is disabled on agnijs-rs/agnijs (API: enabled=false). SECURITY.md names it as a channel with a TODO: enable it (Settings > Code security) or drop the line.
3. SECURITY.md response aims (ack 7 days, assessment 30 days, 90-day embargo) are proposals marked TODO(owner).
4. Open stability questions listed at the end of docs/stability.md: Rust API semver policy and semver-checks CI (TASK-104); whether a release loads the previous release's bytecode/snapshot format; whether CLI flags and exit codes get a compatibility promise; whether CI should diff the C ABI against the last release.
<!-- SECTION:NOTES:END -->
