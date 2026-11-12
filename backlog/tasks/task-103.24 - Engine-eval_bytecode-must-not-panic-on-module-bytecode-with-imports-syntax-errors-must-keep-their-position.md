---
id: TASK-103.24
title: >-
  Engine: eval_bytecode must not panic on module bytecode with imports; syntax
  errors must keep their position
status: To Do
assignee: []
created_date: '2026-11-12T03:25:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
ordinal: 288000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while building the CLI (TASK-103.16). eval_bytecode panics on module bytecode that has imports (agnijs-js-vm modules.rs near line 558), which is a panic reachable from a public engine path; this conflicts with the rule that the C API and engine public paths never panic (TASK-103.3 AC 4 and 5). Separately, syntax errors reported to the host lose their source position, so the CLI re-parses the file to recover it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 eval_bytecode on module bytecode with imports returns an error or resolves the imports through the host, and never panics; a regression test covers it, including through the C API
- [ ] #2 A syntax error reported to the host through the engine interface carries its line and column, and the CLI's re-parse workaround is removed
- [ ] #3 The fuzz or panic-audit tests from TASK-103.3 include bytecode with imports and pass
<!-- AC:END -->
