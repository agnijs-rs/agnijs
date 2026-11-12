---
id: TASK-103.5
title: Binary size report, feature flags and compact Intl data
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-12T04:40:00Z'
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
- [x] #1 A size report with method and numbers for several configurations (full, no Intl, trimmed Intl, no debugger) on a release build, and the comparison with QuickJS-NG and Boa
- [x] #2 Documented cargo features and a data-trimming recipe let an embedder choose a smaller build, with CI checking that the configurations build and pass the relevant baselines
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure the engine's stripped size per configuration with a size probe example, a [profile.size] and scripts/size-report.mjs (native and wasm32).
2. Make Intl a default cargo feature of agnijs-js-vm; without it the toLocale* methods take their ECMA-262 behaviour (tested in tests/intl_feature.rs, test262 main suite as the baseline).
3. Trim ICU4X data with ICU4X's ICU4X_DATA_DIR mechanism and a datagen wrapper (tools/gen-icu4x-data.js); fix the seam provider's glob imports so custom data compiles.
4. Compare with QuickJS-NG (rquickjs) and Boa under the same probe and profile.
5. docs/binary-size.md, ADR 0015 addendum, README; a CI size-profiles job.

6. (2026-11-11) Close AC1: debugger and CDP probes, the pinned QuickJS-NG/Boa comparison workspace tools/size-compare, a linker-map measurement of the debugger code left in the default build (the no-debugger row, since there is no feature and adding one would change engine code), and a fresh run of every configuration on one commit.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Sizes (size profile: release, stripped, fat LTO, cgu 1; win-x64 / wasm32): default 20.12 / 18.75 MB; no Intl 3.28 / 2.86; 6 locales 9.06 / 8.31; 6 locales no segmenter models 6.74 / 5.99; en 8.31 / 7.61; en no models 5.99 / 5.29; datagen recommended = default. QuickJS-NG (rquickjs 0.14) 1.21 MB; Boa 0.22 10.81 MB, with intl_bundled 25.99 MB (native only; neither builds for wasm32-unknown-unknown as configured).
Verification: default main 42231/42231, --precompile 42231/42231, intl402 981/981; no Intl main and --precompile 42231/42231; intl402 981/981 on datagen recommended and modern; main 42231/42231 on en-only data; intl_feature tests in default, no-intl and en-only builds; agnijs-capi tests with and without intl; benches within noise of 3b555d0 (0 slower beyond 5%).
Not done: a no-debugger configuration. The debugger interface (agnijs-js-debugger) and the VM's debug hooks are compiled into every build and there is no feature to drop them; the CDP adapter is already a separate crate the engine does not link. Hermes is not compared (C++/CMake, no Rust embedding).
Commit deb40af on main.

2026-11-11, re-measured at 8dc7f41 (rustc 1.99.0, win-x64, [profile.size], stripped), all from one run of node scripts/size-report.mjs --debugger --compare --debugger-residue plus three --data sets (icu4x-datagen 2.4.0): full 20,469,760 B; full with debugger in use 20,470,784; full with debugger and CDP 20,815,360; no Intl 3,627,520; 6 locales 9,408,000; 6 locales no models 7,086,080; en no models 6,341,120; QuickJS-NG rquickjs 0.14.0 1,236,480; Boa 0.22.0 10,810,880; Boa intl_bundled 25,994,752. No debugger: derived, not built; the default probe's MSVC linker map still holds 548 debugger symbols (agnijs_js_debugger, agnijs_js_vm::debug), 141.0 kB out of line, so a no-debugger build is at least 20.33 MB (inlined hook checks not counted). Engine core grew 0.35 MB since deb40af; Intl shares unchanged. wasm32 not re-measured (doc keeps the deb40af numbers, labelled). Script fix: --debugger-residue passes /MAP through cargo rustc to the final link only (RUSTFLAGS reached build scripts and broke them) and the table now prints before that step, with byte counts. Probes smoke-run (Intl format, debugger breakpoint pause, QuickJS, Boa). No engine code changed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Size report for AgniJS is complete in docs/binary-size.md: method, reproduction commands, numbers and caveats for full (20.47 MB), full with debugger in use (20.47) and with debugger plus CDP (20.82), no debugger (derived from a linker map: at least 20.33, at most 141 kB saved), three trimmed-Intl builds (9.41 / 7.09 / 6.34 MB) and no Intl (3.63 MB), all stripped release ([profile.size]) builds on win-x64 at 8dc7f41, with QuickJS-NG via rquickjs 0.14.0 (1.24 MB) and Boa 0.22.0 (10.81 MB, 25.99 MB with intl_bundled) in the same probe and profile from a pinned workspace in tools/size-compare. scripts/size-report.mjs gains --debugger, --compare and --debugger-residue. AC2 was already met (features, data-trimming recipe, size-profiles CI job). Risks and follow-ups: there is no cargo feature to drop the debugger, so the no-debugger row is a bound and not a build; the measured gain is small (0.7% of full, 3.9% of no-Intl). wasm32 numbers were not re-measured. Hermes is not compared (no Rust embedding).
<!-- SECTION:FINAL_SUMMARY:END -->
