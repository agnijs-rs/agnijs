---
id: TASK-103.37
title: 'CLI: a sourceMappingURL with a non-ASCII byte after % panics the process'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T06:25:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 302000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a panic reachable from untrusted source text; the CLI is documented as not a sandbox, but it must not crash on input it reads).

Affected code: crates/agnijs-cli/src/host.rs, `percent_decode` (about line 147), called by `source_map_for` for every script and every loaded module that ends with `//# sourceMappingURL=data:,...` (not base64) or a relative map path.

Minimal repro. Save as pct.js (UTF-8):

    console.log("ran");
    //# sourceMappingURL=data:,%aé1

Command: `agnijs run pct.js` (debug build of agnijs-cli at fea680f).

Expected: "ran" is printed. A map that does not decode is ignored.
Actual: nothing runs, and the process exits with 101:
`panicked at crates\agnijs-cli\src\host.rs:153:49: end byte index 3 is not a char boundary; it is inside 'é'`.
The same comment in a module loaded by `await import("./dep.mjs")` crashes the process mid-run. The importer's `.catch` never runs.

Root cause: the function walks bytes but slices the `&str` with `&s[i + 1..i + 3]`. When a multi-byte character starts at i+1 or i+2, that slice is not on a char boundary and panics. A second bug in the same function: the guard is `i + 2 < b.len()`, so an escape in the last three bytes (`...%41`) is never decoded.

Suggested fix: decode with byte arithmetic, as `agnijs_js_debugger::source_map::percent_decode` already does (`(c as char).to_digit(16)` on `b[i + 1]` and `b[i + 2]`), with `i + 2 < len` fixed to `i + 2 <= len - 1`. Or reuse that function. The CLI copy is the only one with the bug.

Test that should catch it: a unit test in agnijs-cli for `source_map_for` / `percent_decode` with a non-ASCII byte after `%`, a trailing `%41`, and a lone `%` at the end. Also an integration test running a script that carries such a comment.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A script or module whose data: sourceMappingURL has a multi-byte character after % runs (the map is ignored) instead of panicking
- [x] #2 A trailing escape such as %41 at the end of the URL is decoded
- [x] #3 agnijs-cli has a test for both cases
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red unit tests for the CLI percent decoder and data: URLs, plus a CLI integration test running a script and a module with the bad escape; and a drive-relative path with a multi-byte name through absolute().
2. Replace the CLI's string-slicing percent_decode with the debugger's byte-based one (made pub).
3. Fix absolute() splitting C:<multi-byte> inside a character.
4. Sweep for other from_str_radix/byte-offset slicing on user text.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed in caa2984. The CLI's str-slicing percent decoder is replaced by the debugger's byte-based one (agnijs_js_debugger::source_map::percent_decode, made public), which keeps a % not followed by two hex digits; a bad escape now yields an unparsable map that is ignored. Same class in the same file: absolute() split C: paths at byte 3, inside a multi-byte character for drive-relative paths like C:é.js; fixed. Tests: host.rs unit tests (decoder, trailing %41, data: URLs, absolute()) and tests/cli.rs a_source_map_url_with_a_bad_escape_does_not_crash for a script and an imported module; red before the fix.

Gates on the rebased tree (base 32efdce): workspace tests pass; test262 0 regressions in quickjs, parse, agnijs default, --precompile, --snapshot, --only-intl402; GC-stress slice and GC-stress unit tests pass; JIT feature VM tests and AGNIJS_JIT=force/on slices 0 regressions; C API fault-injection, fuzz (10x) and cbindgen header drift clean; clippy -D warnings clean on capi, cli, resolve and debugger, and no clippy finding on the touched lines in parser and vm (their pre-existing findings are elsewhere); benchmark with JIT off against 8af8d4a, 4 interleaved rounds of 10 iterations: geomean 0.985, compile-source 0.971, json-stringify 0.981, json-parse 0.983.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A data: sourceMappingURL with a multi-byte character after % no longer panics the CLI; the map is ignored and the script or module runs. A trailing escape is still decoded. The CLI now shares the debugger's byte-based decoder, and absolute() no longer splits a path inside a character. Commit caa2984.
<!-- SECTION:FINAL_SUMMARY:END -->
