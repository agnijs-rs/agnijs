---
id: TASK-103.13.3
title: 'TypeScript: fuzz target and documentation'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
updated_date: '2026-11-16T19:40:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 349000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The transpiler takes source an embedder may not trust, so it is fuzzed like the other front ends, and embedders need the documentation to use it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 agnijs-fuzz has a typescript target behind a typescript feature, run by the fuzz CI job and the smoke test, with a cargo-fuzz target in fuzz/
- [x] #2 README, stability, security model, determinism, embedding guide and CHANGELOG describe the crate, its limits and its policy
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
agnijs-fuzz has a `typescript` target behind a `typescript` feature (crates/agnijs-fuzz/src/targets/typescript.rs), seeded from the crate's fixtures and hand-written inputs, with a cargo-fuzz target in fuzz/. It checks diagnostics (sorted, bounded, inside the file), deterministic output, the source map, and that the output parses and runs under the engine's limits. CI runs it in the fuzz job (`all --features typescript`), the typescript job (smoke and clippy) and fuzz.yml's libFuzzer matrix; default and MSRV builds of agnijs-fuzz are unchanged.

About 3 million inputs (seeds 2 to 60 at --scale 25) found Oxc 0.158 accepting invalid input and printing code the engine cannot parse or that means something else: lexer errors lost on a later `<` (identifier escapes, HTML comments in modules), nameless `function<T>(){}` declarations, import/export in scripts and nested blocks, legacy decorators on class expressions, and await/arguments in exported enums and namespaces. The crate refuses each now, with a fixture; ADR 0029's addendum lists them (worth reporting upstream). Early errors Oxc leaves to the engine are accepted by the target.

Docs: README, CHANGELOG (Unreleased), stability (crate outside the semver policy for now, its 1.97 MSRV opt-in), security model (transpiling section and checklist item), determinism, embedding guide (TypeScript modules on examples/embed-typescript), CLI README and fuzzing.md.

Gates: tests and clippy for touched crates with and without the features, licence check, fuzz smoke with and without the feature and `all --seed 1`, MSRV 1.88 check, test262 0 regressions in all five modes, bench geomean and size_probe unchanged (no engine code changed).
<!-- SECTION:FINAL_SUMMARY:END -->
