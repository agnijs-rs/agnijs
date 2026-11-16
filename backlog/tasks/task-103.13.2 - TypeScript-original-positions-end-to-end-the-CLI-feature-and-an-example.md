---
id: TASK-103.13.2
title: 'TypeScript: original positions end to end, the CLI feature and an example'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
updated_date: '2026-11-16T18:11:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 348000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Transpiled code must report .ts positions everywhere the engine reports positions, and the CLI should run .ts files directly when built with the feature.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Error.stack and Exception::stack of transpiled modules and scripts show original .ts lines and columns
- [x] #2 A CDP client sets a breakpoint through the transpiled code's source map and pauses at the original .ts line
- [x] #3 agnijs built with the typescript feature runs .ts, .mts, .cts and .tsx files and prints TypeScript diagnostics with positions; default builds are unchanged
- [x] #4 An example embeds the crate in a Host::load_module and runs in CI
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Transpiled TypeScript now reports .ts positions end to end, the CLI runs TypeScript with an opt-in feature, and an example embeds the crate.

- Positions: tests in agnijs-js-typescript/tests/run.rs check Error.stack for an imported module (`at boom (lib/thrower.ts:5:40)`) and Exception::stack for a script registered with set_source_map. tests/cdp.rs drives a WebSocket DevTools client: it reads the map from Debugger.scriptParsed, sets a breakpoint on the generated position of a .ts line, pauses there, maps the pause back to the original line, and evaluates on the call frame.
- CLI: the `typescript` feature (off by default, Rust 1.97) runs .ts, .mts, .cts and .tsx files, and imports that resolve to them. Extensionless imports also find .ts files. Diagnostics print with a code frame of the TypeScript, and uncaught errors quote the TypeScript line. `compile` refuses TypeScript. Without the feature, a .ts file runs as JavaScript, as before.
- Example: examples/embed-typescript transpiles in Host::load_module. CI tests it and clippies it in the typescript job.
- CI: fixed the size-profiles icu4x-datagen install, which failed when rust-cache restored the binary while its own cache missed.

Gates:
- test262 had 0 regressions in default, --precompile, --snapshot and --only-intl402 runs, and QuickJS-NG stayed at its baseline.
- Bench geomean 0.9973.
- Default size_probe unchanged (20,569,088 bytes).
- Licence check, fuzz smoke, cargo +1.88 check and clippy -D warnings all pass.

Found, not fixed: Exception::stack gives class and object methods an empty function name, even in plain JS (Error.stack names them).
<!-- SECTION:FINAL_SUMMARY:END -->
