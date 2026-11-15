---
id: TASK-103.18
title: Fuzzing, disclosure policy and stability policy
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-15T23:43:00Z'
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
- [x] #1 Fuzz targets exist for the parser, the compiler, the bytecode loader, the regex engines and the engine interface, and run continuously in CI or OSS-Fuzz
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

Update after rebasing on TASK-104 (2026-11-15): TASK-104.6 settled the Rust API semver policy and added the cargo-semver-checks job, and updated docs/stability.md, so owner decision 4's first item is resolved. The other open questions remain at the end of that page.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added fuzzing, a security disclosure policy and a stability policy.

- crates/agnijs-fuzz: 13 targets covering the parser, the compiler, eval through the engine interface, JSON, Intl, regex (the matchers must agree), regex programs, the bytecode and snapshot loaders, CDP messages and WebSocket frames, source maps and import maps. It comes with a seeded, reproducible runner (stable Rust) and a smoke test. fuzz/ runs the same targets under cargo-fuzz and libFuzzer.
- CI: the `fuzz` job in ci.yml runs every target with a fixed seed on each push (about 30 s of fuzzing, deterministic). fuzz.yml runs libFuzzer under ASan on all 13 targets nightly and on demand, with a cached corpus.
- Docs: SECURITY.md, docs/stability.md and docs/fuzzing.md, with links from security-model.md and the README.

Verification:
- CI passed on every job (run 43766289765 on d411c1d).
- A manual fuzz.yml run (43762098993, 60 s per target) passed after the fix for the musl default triple.
- Local runs: the default run and a 20x run (about 6.2 M inputs). No crashes were found, so nothing needed fixing or filing.
- A flaky wall-clock test from TASK-103.55 surfaced in CI and was made robust in d411c1d.

Follow-ups for the owner (details in the notes):
- the security contact address;
- turning on GitHub private vulnerability reporting;
- confirming the response times;
- the open questions at the end of docs/stability.md. The Rust semver policy has since been settled by TASK-104.6, which updated that page.
<!-- SECTION:FINAL_SUMMARY:END -->
