---
id: TASK-100.12
title: Extract the engine into its own repository
status: To Do
assignee: []
created_date: '2026-11-08T23:06:00Z'
updated_date: '2026-11-09T04:00:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.10
  - TASK-100.11
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 224000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The engine is valuable as a release on its own: a pure-Rust, embeddable, test262-complete ES2025 engine with real Intl on ICU4X has no equivalent (Boa is the nearest and lags badly on Intl; the production engines are C/C++). The split waits until M9 (TASK-100.10) because M7 to M9 change the engine and Gable's adapter together and the switch-over gate runs Gable's suites against the engine, both far easier in one workspace. TASK-100.11 makes the split mechanical. History must be preserved: native/engine is already its own Cargo workspace, so a git subtree split carries its full history. Gable then consumes the engine as a git dependency pinned by revision; the engine CI (test262 baselines, intl402, GC stress, Miri, ASan) moves with it. Follow-up, to be ticketed after this and M7 when the adapter's real surface is visible: a generic embedding API (eval and module APIs, host functions, value handles with a documented lifetime story), semver policy, fuzzing corpus, security disclosure policy and embedder docs. A name and a license must be decided before the first external dependent.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The new repository holds the full history of native/engine (subtree split), builds and passes its CI there with the test262 and intl402 baselines unchanged
- [ ] #2 Gable depends on the engine by pinned git revision; native/engine is removed from the Gable repository and Gable's own build, tests and native-engine CI pass against the dependency
- [ ] #3 The engine repository has a README, a license and the recorded name; ADRs that concern only the engine move with it and Gable's ADR index links to them
- [ ] #4 A follow-up task for the generic embedding surface exists, written from the adapter as it actually landed in M7 to M9
- [ ] #5 Gable keeps only the dependency, the adapter and harness, a short adoption ADR, the switch-over report, and a CLAUDE.md note that engine code and tickets live in the agnijs repository; the spec, engine-only ADRs, performance findings, benchmarks and engine CI have moved, and Gable's ADR index links to each moved ADR
- [ ] #6 Every agnijs-* crate declares license = "MIT OR Apache-2.0"; the README has a license section; THIRD-PARTY-NOTICES covers the Unicode data; a cargo deny or cargo license run shows every dependency is compatible
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Sonnet for the mechanical rename to agnijs-* (scripted, verified by the ADR 0016 boundary tests and the standalone-export CI job) and the repository split. Creating the repository and any publishing needs the maintainer.

Ordering (maintainer, 2026-11-08): the extraction happens before the rest of the post-v1 work (TASK-101.2, 101.3, 101.5). Rename to agnijs-* and the agnijs-rs GitHub org apply here.

Plan (maintainer, 2026-11-08): the maintainer creates the repository in the agnijs-rs org and picks the license first. The subtree split, rename and Gable dependency switch run after TASK-102 merges, so its regex-crate changes do not conflict.

Scope of what stays in the Gable repository (maintainer, 2026-11-08): after the split the engine is just a dependency of Gable Native. Gable keeps only: the pinned git dependency and a line in the native docs naming the engine; the adapter and native/engine-harness (Gable's own suites and benchmarks run on the engine); one short ADR recording the decision to adopt the engine and switch over from QuickJS-NG, linking to the agnijs repository; the M9 switch-over report as evidence; any Gable-facing glossary terms; and the QuickJS-NG fallback only until the one-release overlap ends. Everything else moves: docs/js-engine-spec.md, the engine-only ADRs (0006, 0010, 0013, 0015, 0016, 0017 and any other that does not concern Gable), the performance findings, the benchmarks, test262 setup, engine CI, and the engine agent rules. Moved ADRs keep their numbers; Gable's ADR index links to each. Add a one-line note to Gable's CLAUDE.md saying engine code and tickets live in the agnijs repository. Engine tickets already moved to the agnijs backlog with IDs unchanged. License is dual MIT or Apache-2.0: set license = "MIT OR Apache-2.0" in every agnijs-* crate, add a README license section and a THIRD-PARTY-NOTICES file for the ICU4X Unicode data, and run cargo deny or cargo license to confirm every dependency is compatible. The agent may push straight to main in the agnijs repository.
<!-- SECTION:NOTES:END -->
