---
id: TASK-103.5
title: Binary size report, feature flags and compact Intl data
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 240000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Publish real size numbers and shrink them: stripped release binary size of the engine with and without Intl, with trimmed locale data (ICU4X data slicing by locale and component), feature flags to drop Intl, the debugger and the CDP adapter, and a comparison against QuickJS-NG, Boa and Hermes. Compact Intl data is a selling point against QuickJS, which has little. Model: Sonnet for measurement and docs; escalate any engine change to Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A size report with method and numbers for several configurations (full, no Intl, trimmed Intl, no debugger) on a release build, and the comparison with QuickJS-NG and Boa
- [ ] #2 Documented cargo features and a data-trimming recipe let an embedder choose a smaller build, with CI checking that the configurations build and pass the relevant baselines
<!-- AC:END -->
