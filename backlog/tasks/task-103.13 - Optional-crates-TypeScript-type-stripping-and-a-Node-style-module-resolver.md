---
id: TASK-103.13
title: TypeScript support through Oxc behind an optional seam
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T02:15:00Z'
updated_date: '2026-11-16T16:50:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 248000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Run .ts files directly. Level 1 (type stripping with source positions preserved, as Node does) and level 2 (the non-erasable syntax: enum, namespace, parameter properties, decorators, JSX) through the oxc_transformer crate, behind an optional seam in a separate crate. The engine core keeps its own parser and takes no dependency on Oxc or TypeScript (ADR 0016); the seam hands the engine plain JavaScript plus a source map, so errors, stack traces and the debugger (ADR 0017) report original TypeScript positions. No type checking: tsrs (the Rust port of the TypeScript checker, experimental) is out of scope here and is only a possible tooling option. Start with a short ADR: Oxc adopt versus a custom strip-only transform in our own parser (smaller, erasable syntax only), compile-time and binary-size cost, and the seam shape, modelled on gable-js-regex (ADR 0013) and gable-js-intl (ADR 0015). Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An ADR decides the Oxc adoption, the seam, the version pinning policy and the fallback, with measured binary-size and compile-time cost of the optional crate
- [ ] #2 The optional crate turns TypeScript source into JavaScript plus a source map through oxc_transformer: type annotations, interfaces and type-only imports and exports are stripped, and enums, namespaces, parameter properties, decorators and JSX are transformed, with documented options and the pinned TypeScript version level
- [ ] #3 Errors, stack traces and debugger breakpoints show original .ts positions, verified through the CDP adapter; syntax errors in TypeScript surface as host-visible diagnostics with positions
- [ ] #4 The engine core and its test262 baselines gain no dependency or change; the crate is excluded from the default engine build and from the standalone-export CI job unless enabled, and ADR 0016 boundary tests pass
- [ ] #5 A fixture suite covers the supported TypeScript syntax and the unsupported or erasable-only edges, and is run in CI
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. ADR 0029: Oxc 0.158 (pinned exactly) in agnijs-js-typescript, no engine crate depends on it; worker thread with a stack sized from the input; measured size and compile cost (done first, pushed alone).
2. TASK-103.13.1: the crate: transpile() with Mode::Transform and Mode::StripOnly, refusals (TC39 decorators, accessor, import =/export =), decorator helper prelude, diagnostics with UTF-16 positions, limits; fixture suite; boundary test; typescript CI job; msrv/standalone/build-test exclusions.
3. TASK-103.13.2: original .ts positions end to end: Error.stack and Exception::stack through SourceWithMap/set_source_map, CDP breakpoints and exceptions; the agnijs CLI typescript feature (.ts/.mts/.cts/.tsx); an example.
4. TASK-103.13.3: the typescript fuzz target (agnijs-fuzz feature + cargo-fuzz target), docs (README, stability, security model, determinism, embedding guide, CHANGELOG).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
ADR 0029 written first. Measured Oxc 0.158 alone: 2.50 MB stripped (size profile), 51 s clean release / 30 s debug, 68 new lock entries with no existing version changed, MSRV 1.97. No recursion guard in Oxc: 451 B stack per source byte release worst (nested parens), 1,704 B debug; hence the worker thread sized from input length. Scope unchanged by the ADR: legacy decorators only (TC39 decorators refused, as the engine lacks them), CLI feature added as the 'run .ts directly' front end.
<!-- SECTION:NOTES:END -->
