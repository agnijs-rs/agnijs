---
id: TASK-103.54
title: >-
  String length: toHex and toBase64 allocate the whole result before checking
  the limit
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 319000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: low to medium. The result is a catchable RangeError, as it should be, but only after a large uncounted native allocation. That contradicts docs/security-model.md, which says every built-in known to make a string this long checks before the backstop.

Affected code: crates/agnijs-js-vm/src/builtins/base64.rs around lines 110 to 146 (`toHex`, `toBase64`). These build a Rust `String` of the whole result, plus a copy, before `check_string_len` and `check_memory` run. Also, from reading the code but not run: `Symbol.prototype.toString` (symbol.rs around line 122), `BigInt.prototype.toString` (builtins/bigint.rs around line 105) and `Intl.ListFormat` go through `vm.str` without an up-front length check. `String.prototype.anchor` and the other HTML methods (`create_html`, string.rs around line 1307) do not check while expanding `"` to `&quot;`.

Minimal repro:

    new Uint8Array(2 ** 29 + 1).toHex()

Command: `agnijs run --max-memory 600M <file>` (debug build at fea680f).

Expected: a RangeError before any large allocation. The output length (2 × byteLength, or 4/3 × byteLength) is known up front.
Actual: a RangeError, but after a peak working set of 2.57 GiB against the 600 MiB limit. `'x'.anchor('"'.repeat(2 ** 27))` under `--max-memory 300M` peaks at 1,421 MiB.

Suggested fix: compute the output length first and call `check_string_len` and `check_memory` before building. Do the same for the other sites named above.

Test that should catch it: string_length.rs tests for `toHex` and `toBase64` with inputs just past the limit, asserting a RangeError and (with a memory limit) no allocation over the limit.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 toHex and toBase64 past the string limit throw RangeError before allocating the result
- [ ] #2 The other sites named in the description check their output length up front
- [ ] #3 string_length.rs covers toHex and toBase64
<!-- AC:END -->
