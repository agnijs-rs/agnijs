---
id: TASK-100.1
title: 'Engine M0: workspace, engine interface, test262 runner and CI'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-07T22:36:00Z'
updated_date: '2026-11-07T23:39:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 202000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Everything later is measured by test262 and built against the engine interface, so the runner, the CI matrix and the interface crate come first. Spec sections 7, 10 and 11.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Cargo workspace under native/engine/ with the crates in spec section 10 builds on Windows, macOS and Linux in CI
- [x] #2 gable-engine-api defines the engine interface (spec section 7) with a conformance test kit; the Rust API shapes are recorded in an ADR
- [x] #3 A QuickJS-NG adapter passes the conformance kit
- [x] #4 The test262 runner pins a commit, derives the ES2025 feature list, runs strict twice, applies the exclusions file (a missing reason is an error), reports per-directory pass rates and fails on regression against a checked-in baseline
- [ ] #5 CI runs the runner on three OSes for pull requests touching native/engine
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Cargo workspace at native/engine/ with the ten crates from spec section 10 (stubs for the engine crates that later milestones fill)
2. gable-engine-api: handle-based Engine trait (persistent value handles, no lifetimes), Host hooks trait (module loader, import.meta, rejection tracker, console), JsError, conformance kit as a macro that instantiates the same test set for any factory; ADR 0005 records the shapes
3. gable-engine-quickjs: adapter over rquickjs (QuickJS-NG) passing the kit
4. gable-js-test262: pinned-commit fetcher, frontmatter parser, ES2025 feature cut from features.txt minus a checked-in post-2025 list, strict double run, exclusions file with mandatory reasons, per-directory report, baseline compare
5. CI: native-engine.yml on windows/ubuntu/macos for PRs touching native/engine, runs cargo test and the runner against the baseline
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
M0 results (2026-11-07): workspace of ten crates builds; gable-engine-api conformance kit has 21 tests and the QuickJS-NG adapter (rquickjs 0.14 / QuickJS-NG 0.16.2) passes all of them. ADR 0005 records the API shapes. test262 runner: pin e4fbf577 (crates/gable-js-test262/PIN), ES2025 cut = features.txt Standard section minus post-es2025-features.txt (maintainer should review that list), exclusions/quickjs.txt (7 rules with reasons), strict double run, per-directory report, baseline compare. Full run under QuickJS-NG on Windows: 42196 tests, 42114 passed, 82 failed (all QuickJS-NG behaviour: Date setters with NaN, compound-assignment evaluation order, with+unscopables, growable SharedArrayBuffer, Promise.try wrapping, TLA edge cases, Annex B call-expression assignment targets), 6714 skipped by feature cut or exclusion, 41 s on 16 threads. Baseline checked in at baselines/quickjs.json. CI workflow .github/workflows/native-engine.yml (ubuntu/windows/macos) is written but has not run yet: this branch has not been pushed, so AC 1 and AC 5 are verified only on Windows locally.

ADR numbering: the engine-interface ADR is docs/adr/0006 (0005 was taken by the renderer ADR created in parallel).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered the M0 foundation on integration/js-engine: the native/engine Cargo workspace (ten crates, stubs for later milestones), gable-engine-api (handle-based Engine and Host traits, error enum, bytecode container, 21-test conformance kit as a macro), gable-engine-quickjs (QuickJS-NG adapter passing the kit; re-entrancy over rquickjs, raw realm creation inside host callbacks, compile-strings via wrapper swap), gable-js-test262 (fetch/run CLI, pinned commit, ES2025 feature cut, exclusions with mandatory reasons, strict double run, modules, async protocol, timeouts, rayon, per-directory report, baseline compare) and the three-OS CI workflow. Verification: cargo test --workspace passes (22 adapter tests incl. the kit, 9 runner tests); the full test262 run under QuickJS-NG passes 42114/42196 with 82 engine-behaviour failures recorded in baselines/quickjs.json and 0 regressions on re-run. Risks: CI has only been exercised locally on Windows (AC 1 and 5 await the first push); post-es2025-features.txt is the implementer's reading of which Standard-section features post-date ES2025 and needs maintainer review; the adapter reports new_target as None and treats handles as strong roots (ADR 0005).
<!-- SECTION:FINAL_SUMMARY:END -->
