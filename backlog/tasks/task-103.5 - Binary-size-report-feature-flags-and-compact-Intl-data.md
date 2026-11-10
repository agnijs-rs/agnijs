---
id: TASK-103.5
title: Binary size report, feature flags and compact Intl data
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-10T23:16:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 240000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Publish real size numbers and shrink them: stripped release binary size of the engine with and without Intl, with trimmed locale data (ICU4X data slicing by locale and component), feature flags to drop Intl, the debugger and the CDP adapter, and a comparison against QuickJS-NG, Boa and Hermes. Compact Intl data is a selling point against QuickJS, which has little. Model: Sonnet for measurement and docs; escalate any engine change to Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A size report with method and numbers for several configurations (full, no Intl, trimmed Intl, no debugger) on a release build, and the comparison with QuickJS-NG and Boa
- [x] #2 Documented cargo features and a data-trimming recipe let an embedder choose a smaller build, with CI checking that the configurations build and pass the relevant baselines
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure the engine's stripped size per configuration with a size probe example, a [profile.size] and scripts/size-report.mjs (native and wasm32).
2. Make Intl a default cargo feature of agnijs-js-vm; without it the toLocale* methods take their ECMA-262 behaviour (tested in tests/intl_feature.rs, test262 main suite as the baseline).
3. Trim ICU4X data with ICU4X's ICU4X_DATA_DIR mechanism and a datagen wrapper (tools/gen-icu4x-data.js); fix the seam provider's glob imports so custom data compiles.
4. Compare with QuickJS-NG (rquickjs) and Boa under the same probe and profile.
5. docs/binary-size.md, ADR 0015 addendum, README; a CI size-profiles job.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Sizes (size profile: release, stripped, fat LTO, cgu 1; win-x64 / wasm32): default 20.12 / 18.75 MB; no Intl 3.28 / 2.86; 6 locales 9.06 / 8.31; 6 locales no segmenter models 6.74 / 5.99; en 8.31 / 7.61; en no models 5.99 / 5.29; datagen recommended = default. QuickJS-NG (rquickjs 0.14) 1.21 MB; Boa 0.22 10.81 MB, with intl_bundled 25.99 MB (native only; neither builds for wasm32-unknown-unknown as configured).
Verification: default main 42231/42231, --precompile 42231/42231, intl402 981/981; no Intl main and --precompile 42231/42231; intl402 981/981 on datagen recommended and modern; main 42231/42231 on en-only data; intl_feature tests in default, no-intl and en-only builds; agnijs-capi tests with and without intl; benches within noise of 3b555d0 (0 slower beyond 5%).
Not done: a no-debugger configuration. The debugger interface (agnijs-js-debugger) and the VM's debug hooks are compiled into every build and there is no feature to drop them; the CDP adapter is already a separate crate the engine does not link. Hermes is not compared (C++/CMake, no Rust embedding).
Commit deb40af on main.
<!-- SECTION:NOTES:END -->
