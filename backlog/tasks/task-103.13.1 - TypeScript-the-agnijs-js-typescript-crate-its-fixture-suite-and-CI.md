---
id: TASK-103.13.1
title: 'TypeScript: the agnijs-js-typescript crate, its fixture suite and CI'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 347000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The transpile API over Oxc (ADR 0029): both levels, the refusals, diagnostics with positions, the input-sized worker stack, and the CI that keeps it out of the default build.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 transpile() turns TypeScript into JavaScript plus a Source Map v3 in Mode::Transform and Mode::StripOnly, with documented options
- [ ] #2 Syntax errors and the crate's refusals are Error::Syntax diagnostics with byte ranges and 1-based UTF-16 line and column
- [ ] #3 Pathological nesting up to max_source_len returns a result or an error, never a stack overflow, in debug and release
- [ ] #4 A fixture suite covers supported syntax and the refused or erasable-only edges, compared byte for byte, and runs in a typescript CI job on Linux, Windows and macOS
- [ ] #5 No engine crate depends on the crate (boundary test); the msrv, standalone and build-test jobs exclude it
<!-- AC:END -->
