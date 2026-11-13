---
id: TASK-103.49
title: RegExp Symbol.replace preallocates captures from a custom exec result length
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 314000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Script aborts the host process (`memory allocation of N bytes failed`), with or without a memory limit.

Affected code: crates/agnijs-js-vm/src/builtins/regexp.rs around lines 1160 to 1167, in `RegExp.prototype[Symbol.replace]`. It reserves the capture vector from `result.length` of whatever a user-defined `exec` returns. In the same loop, `PropKey::Index(n as u32)` truncates indices above 2^32.

Minimal repro:

    const re = /a/; re.exec = () => ({ length: 2 ** 40, 0: 'a', index: 0 }); 'a'.replace(re, 'b');

Command: `agnijs eval "<the line above>"` (debug build at fea680f).

Expected: the spec reads captures 1 to nCaptures-1 one at a time. The engine should either do that under the interrupt and memory limit, or refuse an absurd capture count with a RangeError. The process must not abort.
Actual: the process aborts on the reservation.

Suggested fix: do not reserve from the attacker's length. Clamp `nCaptures`, for example to 2^32-1, matching the u32 key type, and build the list incrementally while polling the interrupt and the memory limit. Use a key type that cannot truncate for indices of 2^32 and above.

Test that should catch it: a VM test with a custom exec result of length 2^40 and 2^32+1 for `replace`, `match` and `matchAll` (check whether they share the pattern), expecting no abort.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A custom exec result with length 2^40 does not abort the process
- [ ] #2 Capture indices of 2^32 and above do not truncate
- [ ] #3 A VM test covers replace and any other method that reads exec results the same way
<!-- AC:END -->
