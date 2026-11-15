---
id: TASK-104.6
title: Rust API semver policy and a CI check
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
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
- [ ] #1 docs/stability.md states which crates and items are public, what counts as breaking, how versions are bumped before and after the first release, how bytecode format changes are versioned and the MSRV rule, replacing the open Rust API question
- [ ] #2 A cargo-semver-checks job in CI compares the public crates with the base of the push or pull request and fails on a breaking change that the version does not allow
<!-- AC:END -->
