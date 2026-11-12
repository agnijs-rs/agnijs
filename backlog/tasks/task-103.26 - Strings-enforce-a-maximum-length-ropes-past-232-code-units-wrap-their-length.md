---
id: TASK-103.26
title: >-
  Strings: enforce a maximum length; ropes past 2^32 code units wrap their
  length
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: high
ordinal: 290000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). Only `repeat`, `padStart` and `padEnd` check a maximum length (2^30, `builtins/string.rs`). `Heap::concat` (`heap.rs`) builds a rope with `len: total as u32` and no check, so doubling a 1 Mi-character string twelve times yields a string whose `length` is 0 (probe: `let s='x'.repeat(1<<20); for(let i=0;i<12;i++) s=s+s; s.length` returned 0). Ropes cost 16 bytes each, so the memory limit does not stop this, and when such a rope is flattened, `flatten` charges the truncated length while copying the real one. V8 and SpiderMonkey throw a RangeError (Invalid string length) once a string would pass their maximum.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every operation that creates a string (concatenation, ropes, template literals, join, replace, String.prototype methods, JSON.stringify, String.fromCharCode, and the rest) throws a RangeError instead of exceeding one documented maximum length
- [ ] #2 No string length is ever truncated; the rope length cannot wrap
- [ ] #3 A regression test covers the doubling case and a concatenation just past the maximum
<!-- AC:END -->
