---
id: TASK-103.18
title: Fuzzing, disclosure policy and stability policy
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 274000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Build public trust for an engine that runs untrusted code: continuous fuzzing (OSS-Fuzz or equivalent) with a structure-aware JavaScript fuzzer such as Fuzzilli plus parser, bytecode loader and regex fuzz targets; a security disclosure policy (SECURITY.md); and a semver and stability policy covering the Rust API, the C API ABI and the bytecode format. Keep local fuzz runs bounded, because the maintainer machine is resource constrained. Model: Opus for the fuzz harnesses, Sonnet for the policy documents.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Fuzz targets exist for the parser, the compiler, the bytecode loader, the regex engines and the engine interface, and run continuously in CI or OSS-Fuzz
- [ ] #2 SECURITY.md and a stability policy are published, and any crash found is fixed with a regression test
<!-- AC:END -->
