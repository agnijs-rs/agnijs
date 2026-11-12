---
id: TASK-103.26
title: >-
  Strings: enforce a maximum length; ropes past 2^32 code units wrap their
  length
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T14:14:00Z'
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
- [x] #1 Every operation that creates a string (concatenation, ropes, template literals, join, replace, String.prototype methods, JSON.stringify, String.fromCharCode, and the rest) throws a RangeError instead of exceeding one documented maximum length
- [x] #2 No string length is ever truncated; the rope length cannot wrap
- [x] #3 A regression test covers the doubling case and a concatenation just past the maximum
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Failing tests first (crates/agnijs-js-vm/tests/string_length.rs): the doubling probe ('x'.repeat(1<<20) doubled 12 times) throws RangeError instead of length 0; a rope of exactly the maximum keeps its length, one more unit throws; a per-path test with a small test-only maximum covers concat/+, template literals, +=, join, replace/replaceAll, repeat, padStart/padEnd, concat(), JSON.stringify, String.fromCharCode/fromCodePoint, String.raw, normalize/case mapping, encodeURI/escape, and that nothing is truncated.
2. One maximum: MAX_STRING_LENGTH = 2^30 code units (string.rs). Reasons: the cap repeat/pad already use (no behaviour change there); between V8 (2^29-24) and SpiderMonkey (2^30-2); twice it is 2^31 < 2^32, so a u32 rope length cannot wrap even before the check; a flat string is at most 1 GiB one-byte / 2 GiB two-byte.
3. Heap::concat returns Option (None past the max) and Vm::concat turns that into RangeError 'Invalid string length' at every call site; Vm::str_from_units / string_from_builder become fallible with the same check; builder loops (join, JSON.stringify, replace, typed-array join) check as they grow so they never build an oversize buffer.
4. Backstop in Heap::alloc_string for any path not covered: an oversize string is never stored (the empty string is substituted, the real buffer dropped), a heap flag plus an attention bit are set, and the next safe point (attention_slow) or the return to the host raises the RangeError.
5. Docs: security-model.md memory-limit gaps (string length, join, overshoot), plus the limits section. Gates incl. micro-benchmark geomean (jit off) vs 6477187, alternating builds, 3 runs each.

6. As built, two additions for speed: op_add keeps its number and string fast paths inline (op_add_slow for the rest), and concat/substring store through Heap::store_string, skipping the backstop's redundant check. Benchmark baseline fa02ecc (same engine code as 6477187).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first (tests/string_length.rs before the fix, the unbounded join test skipped because without the fix it allocates terabytes): 5 of 6 failed. Doubling 12 times gave '' (length wrapped to 0); a rope one unit past 2^30 had length 1073741825; with a test maximum of 1000, every path made strings of 1001 units.

Design as built:
- MAX_STRING_LENGTH = 2^30 UTF-16 code units (string.rs, re-exported from the crate root). It is the cap repeat/padStart/padEnd already used, so no behaviour changes there. It sits between V8 (2^29-24) and SpiderMonkey (2^30-2). 2 x max < 2^32, so a u32 rope length cannot wrap, and a flat string is at most 1 GiB one-byte / 2 GiB two-byte.
- Heap::concat returns None past the maximum; Vm::concat throws RangeError 'Invalid string length'. Every concat site (+, +=, templates, String.prototype.concat, set_function_name prefixes, Error.prototype.toString, split's flags) goes through it.
- Vm::str_from_units and string_from_builder are fallible with the same check (fromCharCode, fromCodePoint, case mapping, normalize, String.raw, RegExp source/toString, ...). set_function_name and case_convert now return JsResult.
- Builders check as they grow (Vm::check_string_len): Array join/toString/toLocaleString, typed array join/toLocaleString, GetSubstitution, replaceAll, RegExp Symbol.replace, String.raw, JSON.stringify (per property and element), encodeURI/encodeURIComponent/escape. join refuses up front when the separators alone pass the maximum (new Array(2**31).join() throws at once).
- Backstop: Heap::alloc_string replaces any string past the maximum with the empty string and sets Heap::string_too_long plus ATTENTION_STRING; attention_slow, and Vm::enter on the way back to the host, raise the RangeError. concat and substring store through store_string (already within the maximum), so the hot paths skip the redundant check.
- Test-only Vm::set_max_string_length (hidden; clamped to the real maximum) lets the per-path test use a 1000-unit maximum.
- Performance: op_add keeps its number and string fast paths inline and moves the rest out of line (op_add_slow). Without that, the concat error path pushed op_add out of line, and calls-recursive (fib, number +) and string-concat each lost about 6%.

Gates on the final code (Windows x64):
- cargo test --workspace: all ok (90 suites). string_length.rs: 7/7.
- test262 default / --precompile / --snapshot: 43192 run, 0 regressions each; --only-intl402: 981, 0 regressions. QuickJS-NG baseline: 43157 run, 84 baseline failures, 0 regressions.
- C API: fault-injection 56 tests ok; fuzz 5 ok; capi clippy -D warnings clean; no header change.
- CLI: tests ok (2 + 24); CLI clippy -D warnings clean. agnijs-js-vm clippy: no warnings in my hunks (the pre-existing ones remain).
- GC stress: test262 slice under AGNIJS_GC_STRESS=16 5959/5959, 0 regressions; vm string_length/stack_budget/regexp and capi basics/hostile/fault_injection under AGNIJS_GC_STRESS=1 ok.
- rustfmt: my hunks clean (string_length.rs formatted whole).

Micro-benchmark (agnijs-js-bench, release, jit feature off, 26 workloads, 5 iterations each): baseline fa02ecc (the same engine code as 6477187, which only adds CI and docs) against the final code, separate target dirs, alternating base/new, 3 runs each. Per-run geomeans (ms): base 17.792, 17.833, 17.768; new 17.920, 17.868, 17.892. Geomean of per-workload median ratios: +0.69% (within 1%). The largest single workloads are prop-polymorphic +3.5%, regex-tokenize +3.3% and bundle-cold +3.2%, none of which touch the changed paths. Between builds that differed only in heap.rs, single workloads moved by ±3% (prop-polymorphic -2.9% to +3.2%), so these are code layout. string-concat +1.9%; it was +6.2% before the op_add split. An earlier full run of an intermediate version gave +0.20%.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Strings now have one documented maximum length, 2^30 UTF-16 code units (MAX_STRING_LENGTH in string.rs). Any operation that would make a longer string throws a catchable RangeError 'Invalid string length'. Heap::concat refuses before it builds a rope, so the u32 rope length can no longer wrap (2 x max < 2^32). Fallible str_from_units/string_from_builder and growth checks cover the builders: join, replace/replaceAll/Symbol.replace, String.raw, JSON.stringify, URI encoding, escape. join refuses up front when the separators alone pass the maximum. A backstop in Heap::alloc_string stores the empty string for anything missed and raises the error at the next safe point or on return to the host. docs/security-model.md has a new String length section under Limits; the old gap bullet is gone and the overshoot bullet is revised. Tests: crates/agnijs-js-vm/tests/string_length.rs (doubling, one past the max, refused join, every path at a 1000-unit test maximum, host-side error). Gates: workspace tests, test262 in all four modes, and the QuickJS baseline all have 0 regressions; C API fault-injection/fuzz/clippy, CLI, GC-stress slice and stress runs are green. Micro-benchmark geomean (jit off, alternating, 3 runs each) is +0.69% against fa02ecc. Risks: a path that reaches only the backstop runs on with an empty string until the next safe point, so its side effects happen. A builder can briefly hold the maximum plus one piece (up to 4 GiB) before refusing; TASK-103.25 covers memory overshoot.
<!-- SECTION:FINAL_SUMMARY:END -->
