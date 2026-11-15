---
id: TASK-104.6
title: Rust API semver policy and a CI check
status: Done
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
updated_date: '2026-11-15T22:44:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: feature
ordinal: 329000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
docs/stability.md defers the Rust API compatibility policy to TASK-104. Without a written policy and a check, a breaking change to the public crates lands unnoticed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 docs/stability.md states which crates and items are public, what counts as breaking, how versions are bumped before and after the first release, how bytecode format changes are versioned and the MSRV rule, replacing the open Rust API question
- [x] #2 A cargo-semver-checks job in CI compares the public crates with the base of the push or pull request and fails on a breaking change that the version does not allow
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
docs/stability.md now states the Rust API policy: public crates agnijs and agnijs-engine-api (hidden implementor items and every other crate excluded; resolve/debugger/CDP outside until reviewed), what is breaking (cargo-semver-checks major lints plus observable contract changes) and what is not, version bumps before 1.0 (minor per breaking change, in the same commit) and from 1.0 (major, after a deprecation release), bytecode/snapshot tags versioned separately, and the MSRV rule. The table, intro, MSRV section and open decisions are updated; README and ADR 0026 match. New CI job semver runs cargo-semver-checks 0.51.0 on both crates with the PR base or the push's before commit as baseline (falling back to HEAD^). Verified locally: with 0.1.0 it fails on this branch (enum_marked_non_exhaustive, *_now_doc_hidden), with the workspace bumped to 0.2.0 it passes.
<!-- SECTION:FINAL_SUMMARY:END -->
