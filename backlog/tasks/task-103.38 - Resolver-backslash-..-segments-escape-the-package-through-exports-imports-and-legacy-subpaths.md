---
id: TASK-103.38
title: >-
  Resolver: backslash .. segments escape the package through exports, imports
  and legacy subpaths
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
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
- [ ] #1 modern/sub/..\..\x, #internal/..\..\x and plain/..\..\x are refused with InvalidSpecifier or InvalidPackageTarget, as the forward-slash forms are
- [ ] #2 The refusal holds on every platform
- [ ] #3 tests/resolve.rs covers the backslash forms for exports patterns, imports patterns and legacy packages
<!-- AC:END -->
