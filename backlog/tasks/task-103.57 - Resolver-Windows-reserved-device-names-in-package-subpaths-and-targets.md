---
id: TASK-103.57
title: 'Resolver: Windows reserved device names in package subpaths and targets'
status: To Do
assignee: []
created_date: '2026-11-14T04:47:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 321000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while fixing TASK-103.38. The resolver now refuses dot segments, separators in every form, colons and NUL in package subpaths and exports/imports targets, and checks the joined path stays inside the package. It does not refuse Windows reserved device names (CON, NUL, AUX, PRN, COM1 to COM9, LPT1 to LPT9, also with an extension such as CON.js or with trailing dots and spaces). On Windows versions and APIs that still map these names to devices, a bare specifier such as plain/CON or a target pattern match could make the host's load_module open a device instead of a file inside the package; reading CON blocks on the console. The path stays lexically inside the package, so this is not a confinement bypass, but it is attacker-reachable from import specifiers. Whether current Windows 11 still maps such names inside a directory path needs to be measured first.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Measured on Windows 11 whether a package-relative path ending in CON, NUL, COM1 or CON.js opens a device through std::fs, and the result recorded in the task notes
- [ ] #2 If it does: such segments in package subpaths and exports/imports targets are refused with InvalidSpecifier or InvalidPackageTarget on Windows, with tests in crates/agnijs-js-resolve/tests/resolve.rs
- [ ] #3 If it does not: docs/security-model.md says so and the task is closed with that evidence
<!-- AC:END -->
