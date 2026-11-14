---
id: TASK-103.54
title: >-
  String length: toHex and toBase64 allocate the whole result before checking
  the limit
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T10:21:00Z'
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
- [x] #1 toHex and toBase64 past the string limit throw RangeError before allocating the result
- [x] #2 The other sites named in the description check their output length up front
- [x] #3 string_length.rs covers toHex and toBase64
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first: toHex and toBase64 of 16 MiB under a 1,000-unit maximum peaked at 64 MiB; anchor of 8 MiB of quotes and toString(2) of a 2^26-bit BigInt at 88 and 72 MiB under 16 MiB. Commit b4fdc53. Gates: agnijs-js-vm and agnijs-js-regex suites green; clippy: no warnings on changed lines; test262 0 regressions in default, --precompile, --snapshot, --only-intl402, vs QuickJS-NG, the GC-stress slice and the JIT force and tiered slices.

Benchmarks (JIT off, 4 interleaved rounds against edbdb50, 26 workloads): geomean +0.23%.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
toHex and toBase64 compute their output length and check it before building the Latin-1 bytes the string keeps (no copy); the HTML methods, BigInt to string (lower bound from the bit length), SymbolDescriptiveString and Intl.ListFormat check up front too. string_length.rs covers each one unit past and exactly at the maximum; memory_limit_peak.rs shows no peak where there were 64 to 88 MiB. Commit b4fdc53.
<!-- SECTION:FINAL_SUMMARY:END -->
