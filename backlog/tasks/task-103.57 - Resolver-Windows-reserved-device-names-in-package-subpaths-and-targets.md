---
id: TASK-103.57
title: 'Resolver: Windows reserved device names in package subpaths and targets'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-14T04:47:00Z'
updated_date: '2026-11-14T09:42:00Z'
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
- [x] #1 Measured on Windows 11 whether a package-relative path ending in CON, NUL, COM1 or CON.js opens a device through std::fs, and the result recorded in the task notes
- [ ] #2 If it does: such segments in package subpaths and exports/imports targets are refused with InvalidSpecifier or InvalidPackageTarget on Windows, with tests in crates/agnijs-js-resolve/tests/resolve.rs
- [x] #3 If it does not: docs/security-model.md says so and the task is closed with that evidence
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Measured 2026-11-14 on Windows 11 Pro 10.0.26200 (scratch program, std::fs, path joined with '/' under %TEMP%/agnijs-devnames/node_modules/plain as the resolver joins it). Per name: std::fs::metadata / is_file / File::open / read.
- CON, con, AUX, PRN, COM1, COM9, LPT1, CONIN$, CONOUT$: NotFound (os 2) / false / NotFound. Not mapped to a device inside a directory path.
- CON.js, NUL.js, COM1.js, nul.txt, 'CON.', 'CON ', 'CON .js', CON..js, COM¹: NotFound / false / NotFound.
- NUL, nul: metadata fails (os error 1, ERROR_INVALID_FUNCTION) / false / File::open succeeds and a read returns 0 bytes at once (the null device; no blocking).
- index.js control: a file, 16 bytes. No stray files created.
So only a bare NUL segment still reaches a device, and it never blocks. The resolver accepts a candidate only when FileSystem::is_file (std::fs::metadata(..).is_file()) holds: load_path, with_extension, load_index, load_dir, read_package, and the exports/imports Target::Found results (lib.rs:351, 376). metadata fails for NUL, so plain/NUL or a target pattern that yields NUL resolves to not-found and load_module is never asked to read it. Even a host that read such a path directly would get an empty module. No resolver change needed (AC #2 does not apply); documented in docs/security-model.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Measured on Windows 11 Pro 10.0.26200 through std::fs: inside a package directory, CON, AUX, PRN, COM1, LPT1, CONIN$, CONOUT$, CON.js and the trailing-dot and trailing-space forms are plain names (NotFound); only a bare NUL opens a device, the null device, which reads empty and never blocks, and its metadata fails. The resolver accepts only paths whose metadata says they are files (subpaths and exports/imports targets alike), so plain/NUL resolves to not-found and load_module never reads a device. No resolver change: AC #2 does not apply. docs/security-model.md records the finding.
<!-- SECTION:FINAL_SUMMARY:END -->
