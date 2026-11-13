---
id: TASK-103.37
title: 'CLI: a sourceMappingURL with a non-ASCII byte after % panics the process'
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
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
- [ ] #1 A script or module whose data: sourceMappingURL has a multi-byte character after % runs (the map is ignored) instead of panicking
- [ ] #2 A trailing escape such as %41 at the end of the URL is decoded
- [ ] #3 agnijs-cli has a test for both cases
<!-- AC:END -->
