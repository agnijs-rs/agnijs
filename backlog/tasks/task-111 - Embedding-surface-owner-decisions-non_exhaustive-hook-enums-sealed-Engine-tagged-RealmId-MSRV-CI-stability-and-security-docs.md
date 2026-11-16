---
id: TASK-111
title: >-
  Embedding surface owner decisions: non_exhaustive hook enums, sealed Engine,
  tagged RealmId, MSRV CI, stability and security docs
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-16T14:39:00Z'
updated_date: '2026-11-16T16:09:00Z'
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
- [x] #1 Hook enums are #[non_exhaustive] and every in-repo match compiles; the C API maps every variant without panicking
- [x] #2 Engine is sealed; VM, QuickJS-NG adapter and CLI wrapper implement the seal
- [x] #3 A RealmId from another engine is Error::UnknownRealm on every engine (tested)
- [ ] #4 CI has an MSRV job building the workspace default features with the declared rust-version (1.88: the declared 1.85 no longer built, see notes); docs/stability.md states the MSRV rule
- [ ] #5 Workspace version is 0.3.0 with CHANGELOG, README and stability docs updated; semver CI passes
- [x] #6 docs/stability.md records decisions 5-7 and has no answered open items; SECURITY.md has no email and no response-time numbers
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. engine-api: #[non_exhaustive] on the five hook enums; hidden sealed supertrait on Engine; RealmId becomes {engine, index} with hidden constructor/index accessor and public engine(). 2. VM keeps its own internal u32 realm index type (no change to frames/envs/JIT layout) and converts and checks the engine tag at the Engine boundary; QuickJS-NG adapter checks the tag too; CLI Inspected wrapper, C API registry and tests adapt. 3. Wildcard arms everywhere needed; C API maps every variant. 4. Tests: foreign realm id is UnknownRealm (lifetimes.rs, conformance kit). 5. MSRV CI job on 1.85 excluding temporal and jit. 6. Bump to 0.3.0, CHANGELOG.md (new), README, stability.md, ADR 0026 addendum. 7. Commit 2: docs decisions and SECURITY.md. 8. Gates, push, CI.

Changed during the work: step 5 targets 1.88, not 1.85 (see notes).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
MSRV: on main the declared 1.85 no longer built the default features. cargo +1.85 refuses ICU4X 2.4/icu_experimental (1.88), rquickjs 0.14 (1.87), idna_adapter, temporal_rs and timezone_provider (1.86). On 1.87 the ICU crates are refused, and on 1.88 the workspace checks cleanly. So rust-version is 1.88, which fits the rule (a minor bump, released June 2025, more than six months old). agnijs-cli declares 1.95 (rustyline 19 uses cfg_select!; verified that 1.94 fails and 1.95 passes). The temporal crate now uses the workspace value. The msrv job reads the version from Cargo.toml and builds with --exclude agnijs-js-jit --exclude agnijs-cli on Ubuntu. On Windows, 1.88 hits an MSVC PDB symbol limit (LNK4319) when linking timezone_provider's long symbols, so the local check was cargo check, and the Linux CI job is the build. The new MSRV makes clippy suggest is_multiple_of and as_chunks: the seven VM sites use them, and the clippy lint set equals the base's (33). Pre-existing lints remain in the touched crates, among them quickjs type_complexity/wrong_self_convention, step.rs hint_for, collections.rs, base64.rs and intl abstract_ops (same on origin/main).

Behaviour: the QuickJS-NG adapter checks the realm tag in enter() too, so run_jobs refuses a foreign realm id (the new conformance test caught it). An unknown ModuleSource is a load failure, an unknown RegexMatcher is Auto, an unknown ErrorKind is Error. The C API logs an unknown ConsoleLevel as LOG and drops an unknown RejectionEvent. A VM restored from a snapshot is a new engine, so ids from the VM that wrote the snapshot are refused (snapshot test adjusted).

Gable: native/engine-harness/src/host.rs:91 matches RejectionEvent exhaustively and needs a wildcard arm when it moves its pin past 0.3.0. Nothing else there is affected: it uses RealmId as an opaque value, and ConsoleLevel already has a wildcard.

SECURITY.md: the 90-day embargo was dropped along with the response times, because it reads like a fix-time promise. It is replaced by 'keep it private until the fix is public or a date is agreed'. The owner must enable private vulnerability reporting in the GitHub repository settings.

Verified locally before the push. cargo test --workspace passes. test262 shows 0 regressions in default, --precompile, --snapshot, --only-intl402 and QuickJS-NG. The capi fault-injection and release fuzz tests pass. The cbindgen header is unchanged. Clippy -D warnings on capi with fault-injection is clean, and on the touched crates the lint set equals origin/main's. agnijs-fuzz all --seed 1 passes. The jit feature checks. cargo +1.88 check of the msrv job's set is clean, and 1.87 is refused. cargo semver-checks against origin/main passes at 0.3.0. Benchmark (JIT off, 4 interleaved rounds against origin/main f1722ee, median per workload): geomean new/base 1.0093. The as_chunks rewrites first gave 1.0136 (regex-long-input +15%); with them reverted, regex-long-input matches base. snapshot-startup and bundle-snapshot looked 6% slower but match base at 40 iterations (noise).
<!-- SECTION:NOTES:END -->
