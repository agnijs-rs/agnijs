---
id: TASK-100.34
title: 'Regex: deeply nested groups must not overflow the stack'
status: To Do
assignee: []
created_date: '2026-11-09T21:12:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-100
ordinal: 283000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The regex pattern validator and the tree compiler in agnijs-js-regex recurse on group nesting. On a 2 MB stack they overflow at about 2,100 and 1,500 nested groups, so a deep regex literal or new RegExp(...) call on untrusted input can crash the process instead of throwing a SyntaxError (found during TASK-100.31, see the ADR 0013 addendum). Add a nesting depth limit or make both passes iterative, with tests for deeply nested groups, classes and lookarounds, and cover it in the regex fuzz targets. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A pattern nested far past the limit throws SyntaxError (or matches correctly) and never overflows the stack, on the default 2 MB thread stack
- [ ] #2 Regression tests cover deep groups, lookarounds and v-mode classes, and differential fuzzing includes deep-nesting cases
<!-- AC:END -->
