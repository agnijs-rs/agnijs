---
id: TASK-103.13
title: TypeScript support through Oxc behind an optional seam
status: To Do
assignee: []
created_date: '2026-11-09T02:15:00Z'
updated_date: '2026-11-09T02:28:00Z'
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
