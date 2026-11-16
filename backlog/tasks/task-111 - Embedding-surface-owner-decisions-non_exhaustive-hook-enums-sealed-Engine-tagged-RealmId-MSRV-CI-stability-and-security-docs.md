---
id: TASK-111
title: >-
  Embedding surface owner decisions: non_exhaustive hook enums, sealed Engine,
  tagged RealmId, MSRV CI, stability and security docs
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-16T14:39:00Z'
updated_date: '2026-11-16T14:39:00Z'
labels:
  - api
  - docs
dependencies: []
ordinal: 346000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Apply the owner's decisions on the open items from TASK-104 and TASK-103.18 (docs/stability.md 'Decisions still open'). Commit 1 (breaking, workspace 0.2.0 -> 0.3.0): #[non_exhaustive] on ModuleSource, ConsoleLevel, RejectionEvent, RegexMatcher, ErrorKind; Engine sealed with a hidden supertrait; RealmId tagged with its EngineId so another engine's realm id is UnknownRealm; MSRV CI job on 1.85 (default features; temporal 1.86 and jit 1.97 excluded). Commit 2 (docs): bytecode stays a same-build cache, no cross-release snapshot promise before 1.0; CLI promise at first release limited to exit codes and core flags; C ABI comparison CI once a first release exists; SECURITY.md contact is GitHub private vulnerability reporting only, no response-time promises.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Hook enums are #[non_exhaustive] and every in-repo match compiles; the C API maps every variant without panicking
- [ ] #2 Engine is sealed; VM, QuickJS-NG adapter and CLI wrapper implement the seal
- [ ] #3 A RealmId from another engine is Error::UnknownRealm on every engine (tested)
- [ ] #4 CI has an MSRV job building the workspace default features on 1.85; docs/stability.md states the MSRV rule
- [ ] #5 Workspace version is 0.3.0 with CHANGELOG, README and stability docs updated; semver CI passes
- [ ] #6 docs/stability.md records decisions 5-7 and has no answered open items; SECURITY.md has no email and no response-time numbers
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. engine-api: #[non_exhaustive] on the five hook enums; hidden sealed supertrait on Engine; RealmId becomes {engine, index} with hidden constructor/index accessor and public engine(). 2. VM keeps its own internal u32 realm index type (no change to frames/envs/JIT layout) and converts and checks the engine tag at the Engine boundary; QuickJS-NG adapter checks the tag too; CLI Inspected wrapper, C API registry and tests adapt. 3. Wildcard arms everywhere needed; C API maps every variant. 4. Tests: foreign realm id is UnknownRealm (lifetimes.rs, conformance kit). 5. MSRV CI job on 1.85 excluding temporal and jit. 6. Bump to 0.3.0, CHANGELOG.md (new), README, stability.md, ADR 0026 addendum. 7. Commit 2: docs decisions and SECURITY.md. 8. Gates, push, CI.
<!-- SECTION:PLAN:END -->
