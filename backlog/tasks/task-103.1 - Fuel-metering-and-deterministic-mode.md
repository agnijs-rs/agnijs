---
id: TASK-103.1
title: Fuel metering and deterministic mode
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 236000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Instruction-count (fuel) budgets that give the same result on every run, on top of the existing interrupt and memory limits (TASK-100.8), plus a deterministic mode: seeded Math.random, fixed or host-supplied Date and time zone, no ambient clocks, stable iteration where the spec leaves order open. Useful for plugin hosts, serverless, games, blockchains and reproducible tests. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An embedder sets a fuel budget per call or realm; exhaustion stops execution with a host-visible error, deterministically at the same instruction on every run and platform
- [ ] #2 Deterministic mode gives byte-identical results across runs, platforms and engine builds for a documented test corpus, and every remaining source of nondeterminism is documented or host-supplied
- [ ] #3 Overhead with fuel disabled is within noise on gable-js-bench; test262 baselines unchanged
<!-- AC:END -->
