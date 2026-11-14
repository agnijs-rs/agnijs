---
id: TASK-103.38
title: >-
  Resolver: backslash .. segments escape the package through exports, imports
  and legacy subpaths
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T06:25:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 303000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: low to medium, security. docs/security-model.md says the resolver does not confine `../` or absolute specifiers. It also says package `exports` and `imports` targets are checked (InvalidPackageTarget), and that check is what an embedder relies on when it allows only bare specifiers. Backslashes get past it on every platform, not only Windows.

Affected code: crates/agnijs-js-resolve/src/lib.rs.
- `resolve_target` (about line 386) checks the substituted target for `.`, `..` and `node_modules` segments with `rest.split('/')` only.
- `parse_package_name` (about line 498) checks the subpath of a bare specifier the same way.
- `join` and `normalize` (about lines 523 to 546) then replace every `\` with `/` and fold `..`. So a `..\` segment that passed the checks climbs directories.

Minimal repro, using the crate's own fixture tree (tests/fixtures/project), resolving from src/main.js with `Resolver::new(StdFs, Options::default())`:

    modern/sub/../../../../src/util      -> Err(InvalidSpecifier)                      (control)
    modern/sub/..\..\..\..\src/util      -> Ok(".../tests/fixtures/project/src/util.js")
    plain/..\..\src/util                 -> Ok(".../tests/fixtures/project/src/util.js")
    #internal/..\..\src/util             -> Ok(".../tests/fixtures/project/src/util.js")

Command: a test in crates/agnijs-js-resolve/tests that prints `resolver.resolve(spec, root + "/src/main.js")`, run with `cargo test -p agnijs-js-resolve`.

Expected: InvalidSpecifier / InvalidPackageTarget, as for the forward-slash form. Node's `invalidSegmentRegEx` is `(^|\\|/)(\.\.?|node_modules)(\\|/|$)`, which covers both separators. Node's package-name check also refuses `\`.
Actual: `modern`'s `./sub/*` export pattern resolves to a file outside node_modules/modern. The legacy package `plain` and the `#internal/*` import pattern climb the same way.

Root cause: the segment checks split on `/` only, while path joining treats `\` as a separator.

Suggested fix: split on both `/` and `\` (or refuse any `\` in a subpath or pattern match, as Node refuses `%2F`/`%5C`) in `resolve_target` and `parse_package_name`. Consider also checking that the joined result still starts with the package directory, as a backstop.

Test that should catch it: extend `exports_targets_may_not_climb_out_of_the_package` and `malformed_specifiers_are_invalid_not_panics` in tests/resolve.rs with the backslash forms above (exports pattern, imports pattern, legacy subpath).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 modern/sub/..\..\x, #internal/..\..\x and plain/..\..\x are refused with InvalidSpecifier or InvalidPackageTarget, as the forward-slash forms are
- [x] #2 The refusal holds on every platform
- [x] #3 tests/resolve.rs covers the backslash forms for exports patterns, imports patterns and legacy packages
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests in crates/agnijs-js-resolve/tests/resolve.rs: backslash, percent-encoded, trailing-dot/space and node_modules forms through exports, imports and legacy subpaths; a 30,000-spec fuzz of mixed separators, encoded dots, drive prefixes, UNC and colons that asserts every result stays inside the package.
2. Fix in src/lib.rs: one subpath check (subpath_is_plain) that splits on / and \, refuses decoded separators, dot segments after trimming trailing dots and spaces, colons and NUL; package names refuse dot-only and colon segments.
3. Backstop: after joining, is_inside checks the path is at or below the package directory for exports/imports targets and legacy subpaths.
4. Document confinement in the module doc and docs/security-model.md.
5. Sweep other path joins for the same class.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed in d914320. One subpath check (subpath_is_plain) for bare-specifier subpaths and exports/imports substitutions splits on / and \ and refuses dot segments (also percent-encoded and with trailing dots or spaces), encoded separators, colons and NUL, and in targets empty and node_modules segments. Package names refuse dot-only and colon segments. Backstop: is_inside checks the joined path stays at or below the package directory for exports/imports targets and legacy subpaths. Pure string logic, so the refusal is the same on every platform. Tests: backslash, encoded and trailing-dot forms through exports, imports and legacy packages, plus a 30,000-specifier fuzz; red before the fix. Sweep: the import map is URL-based and unaffected; the CLI's sourceMappingURL file read is not a confinement path. Windows reserved device names (CON, NUL, COM1) in subpaths are filed as TASK-103.57.

Gates on the rebased tree (base 32efdce): workspace tests pass; test262 0 regressions in quickjs, parse, agnijs default, --precompile, --snapshot, --only-intl402; GC-stress slice and GC-stress unit tests pass; JIT feature VM tests and AGNIJS_JIT=force/on slices 0 regressions; C API fault-injection, fuzz (10x) and cbindgen header drift clean; clippy -D warnings clean on capi, cli, resolve and debugger, and no clippy finding on the touched lines in parser and vm (their pre-existing findings are elsewhere); benchmark with JIT off against 8af8d4a, 4 interleaved rounds of 10 iterations: geomean 0.985, compile-source 0.971, json-stringify 0.981, json-parse 0.983.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Backslash, percent-encoded and trailing-dot `..` forms can no longer climb out of a package through exports patterns, imports patterns or legacy subpaths. A single subpath check covers every separator form, and a lexical backstop after the join keeps package paths inside the package. Relative and absolute specifiers stay unconfined, as documented (ADR 0023, docs/security-model.md). Commit d914320. Follow-up: TASK-103.57 (Windows device names).
<!-- SECTION:FINAL_SUMMARY:END -->
