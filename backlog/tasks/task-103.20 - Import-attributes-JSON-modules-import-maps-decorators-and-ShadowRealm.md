---
id: TASK-103.20
title: Import attributes, JSON modules, import maps, decorators and ShadowRealm
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-12T03:13:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 276000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Language and module features beyond the ES2026 ticket (TASK-103.7): import attributes and JSON modules, import maps on the host module loading interface, decorators once the proposal stabilizes, and ShadowRealm. First check what TASK-103.7 already covers and what test262 holds for each, and record the decision per feature. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A short ADR records, for each feature, whether it is in scope now or waits on the proposal, with the test262 coverage
- [x] #2 Each in-scope feature passes its test262 tests and lands without regressing the baselines
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Survey (done): at the test262 pin, import-attributes and json-modules are Standard (ES2025) and already pass in the agnijs gate (101 + 14 test files, no expected failures). decorators and ShadowRealm are in the Proposed section; both are TC39 Stage 2.7 (decorators demoted from Stage 3 in May 2026, ShadowRealm at 2.7 since 2024). Import maps are a WHATWG host feature with no test262 coverage.
2. ADR 0025: per-feature decision. Import attributes/JSON modules are in (already done, plus hardening). Import maps are in, as an optional module of agnijs-js-resolve. Decorators and ShadowRealm wait on the proposals.
3. Harden JSON modules in the VM loader: a type: json request that hits a cached JavaScript module record throws a TypeError, and an unsupported type value (anything but json) throws a TypeError instead of loading as JavaScript. Add VM tests.
4. Import maps: agnijs_js_resolve::ImportMap implements the WHATWG algorithm (parse with a base URL, imports, scopes, integrity, trailing-slash packages, backtracking guard, null blocks) over the url crate. Add unit tests, a host test that drives Engine::load_module (JSON modules included), and an optional run of the WPT data-driven import-map tests when AGNIJS_WPT_IMPORT_MAPS is set.
5. Gates: test262 default, --precompile, --snapshot, --only-intl402, no-Intl build, QuickJS baseline, GC-stress and JIT slices, clippy on touched crates, licenses check, bench geomean within 1%.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Survey: at the test262 pin, import-attributes (101 files) and json-modules (14 files) are Standard and already pass with no expected failures. decorators (31 files: 24 syntax-only, 7 staging) and ShadowRealm (64 files) are Proposed. Per the TC39 proposals table both are now Stage 2.7: decorators and decorator metadata were demoted from Stage 3 at the May 2026 meeting (insufficient test262 coverage, no shipping engine, no active champion), and ShadowRealm has been at 2.7 since its 2023 demotion, last updated February 2025. Decision recorded in ADR 0025.

Implemented: (1) The VM loader refuses a type attribute other than json with a TypeError before resolving, so it is not loaded as JavaScript. A type: json request that finds a JavaScript record in the module map, or that gets Source or Bytecode back from the host, is a TypeError. (2) agnijs_js_resolve::ImportMap, the WHATWG import map algorithm over the url crate. The WPT data-driven import map tests (22 files, 284 checks, WPT 48d5a8e) all pass via AGNIJS_WPT_IMPORT_MAPS. There is a host test through Engine::load_module with JSON modules and scopes.

Verification (2026-11-11, Windows x86_64 i9-9900KF), commit 398e696 (rebased on origin/main b8d20aa; the gates ran on cb6931e over cf960c1, and default test262 plus the CLI and resolver tests were rerun after the rebase):
- test262 agnijs: default, --precompile and --snapshot each 43192/43192, 0 regressions, 0 newly passing (unchanged: import-attributes and json-modules already passed; decorators and ShadowRealm stay skipped as proposed). --only-intl402 981/981. No baseline changed.
- No-Intl build (linear-regex): default and --precompile 43192/43192, 0 regressions.
- QuickJS-NG baseline: 0 regressions (43073/43157, unchanged; json-invalid.js was already an expected failure).
- GC stress: host_driven at AGNIJS_GC_STRESS=1 (17 passed). The CI test262 slice plus test/language/import, expressions/dynamic-import and module-code at AGNIJS_GC_STRESS=16: 7190/7190.
- JIT: the CI test262 slice plus import and dynamic-import, AGNIJS_JIT=force and on: 4251/4251 each.
- host_driven with AGNIJS_TEST_SNAPSHOT=1: 17 passed. cargo test --workspace --locked: all 85 test binaries pass.
- WPT import maps (import-maps/data-driven/resources, WPT 48d5a8e): 284 checks across 22 files pass.
- clippy -D warnings: agnijs-js-resolve is clean (two pre-existing lints in lib.rs fixed). No lint in the touched VM files (modules.rs, tests/host_driven.rs); the VM's 29 other warnings predate this task. rustfmt: the new files are formatted, and the modified files only in this task's hunks.
- License check passes (177 packages). The url crate brings url, idna, idna_adapter, form_urlencoded and percent-encoding, all MIT OR Apache-2.0, and reuses the ICU4X crates already in the tree.
- C API not touched (no header change).

Bench (agnijs-js-bench, agnijs engine, 3 interleaved runs per side, median of run medians), head cb6931e against base cf960c1:

| workload | base (ms) | head (ms) | head/base |
| --- | ---: | ---: | ---: |
| loop-arith | 105.38 | 106.19 | 1.008 |
| loop-array-index | 97.46 | 97.32 | 0.999 |
| prop-monomorphic | 83.51 | 84.38 | 1.011 |
| prop-polymorphic | 131.99 | 132.07 | 1.001 |
| prop-write-alloc | 80.48 | 80.23 | 0.997 |
| string-concat | 64.52 | 64.81 | 1.005 |
| string-methods | 33.54 | 33.29 | 0.992 |
| array-methods | 332.24 | 329.63 | 0.992 |
| map-set | 123.83 | 126.24 | 1.019 |
| closures | 64.33 | 62.64 | 0.974 |
| calls-recursive | 56.40 | 55.87 | 0.990 |
| method-calls | 122.86 | 120.73 | 0.983 |
| regex | 106.63 | 107.22 | 1.006 |
| regex-tokenize | 30.54 | 30.76 | 1.007 |
| regex-long-input | 1.77 | 1.76 | 0.998 |
| regex-redos | 0.01 | 0.01 | 0.992 |
| json-stringify | 50.15 | 49.34 | 0.984 |
| json-parse | 50.20 | 49.07 | 0.978 |
| promise-chain | 16.34 | 16.34 | 1.000 |
| async-await | 15.57 | 15.38 | 0.987 |
| compile-source | 14.08 | 14.00 | 0.994 |
| bytecode-load | 2.16 | 2.15 | 0.996 |
| engine-startup | 0.62 | 0.64 | 1.024 |
| snapshot-startup | 0.54 | 0.54 | 0.997 |
| bundle-cold | 2.72 | 2.84 | 1.044 |
| bundle-snapshot | 2.13 | 2.07 | 0.972 |

Geomean head/base 0.9978 over 26 workloads, within the 1% gate. The change is off the hot path: two attribute checks per module request.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
ADR 0025 records the decision for each feature. (1) Import attributes and JSON modules (ES2025) were already in and passing; they are now hardened. json is the only module type: any other type value is a TypeError before resolution, rather than being loaded as JavaScript. A type: json request that finds a JavaScript record, or gets Source or Bytecode back from the host, is a TypeError. (2) Import maps ship as agnijs_js_resolve::ImportMap, the WHATWG algorithm over the url crate: parse, resolve, integrity and warnings, with string-only API types. The WPT data-driven suite passes (284 checks across 22 files, opt-in via AGNIJS_WPT_IMPORT_MAPS), and Rust API tests drive Engine::load_module through the map. (3) Decorators and ShadowRealm wait at TC39 Stage 2.7, and test262 keeps them as Proposed. Gates: test262 43192/43192 in default, --precompile, --snapshot and no-Intl; intl402 981/981; QuickJS at 0 regressions; the GC-stress and JIT slices green; bench geomean 0.9978. CI run 43143019351 is green on build-test for all three OSes. Commit 398e696. Risks: a host that loaded non-json types as JavaScript now gets a TypeError. Multiple import maps are not merged. Lone surrogates in import map JSON are rejected by serde_json.
<!-- SECTION:FINAL_SUMMARY:END -->
