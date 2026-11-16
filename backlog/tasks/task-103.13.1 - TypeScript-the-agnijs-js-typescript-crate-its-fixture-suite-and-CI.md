---
id: TASK-103.13.1
title: 'TypeScript: the agnijs-js-typescript crate, its fixture suite and CI'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
updated_date: '2026-11-16T17:52:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 347000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The transpile API over Oxc (ADR 0029): both levels, the refusals, diagnostics with positions, the input-sized worker stack, and the CI that keeps it out of the default build.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 transpile() turns TypeScript into JavaScript plus a Source Map v3 in Mode::Transform and Mode::StripOnly, with documented options
- [x] #2 Syntax errors and the crate's refusals are Error::Syntax diagnostics with byte ranges and 1-based UTF-16 line and column
- [x] #3 Pathological nesting up to max_source_len returns a result or an error, never a stack overflow, in debug and release
- [x] #4 A fixture suite covers supported syntax and the refused or erasable-only edges, compared byte for byte, and runs in a typescript CI job on Linux, Windows and macOS
- [x] #5 No engine crate depends on the crate (boundary test); the msrv, standalone and build-test jobs exclude it
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
New crate `agnijs-js-typescript` (Oxc 0.158.0, pinned exactly; rust-version 1.97, opt-in like the JIT): `transpile(file, source, &Options) -> Result<Output, Error>` in Mode::Transform (enums, namespaces, parameter properties, legacy decorators with metadata through a one-line helper prelude, JSX classic/automatic) and Mode::StripOnly (erasable syntax only), with a Source Map v3 whose positions agree with the engine's (UTF-16 columns, ECMAScript line terminators).

- Diagnostics: Error::Syntax with byte ranges and 1-based UTF-16 line/column, sorted, capped at MAX_DIAGNOSTICS (100); TooLarge before parsing (max_source_len, default 1 MiB); InvalidOptions; Resource (worker spawn); Internal (an Oxc panic, caught on the worker).
- Hostile input: the pipeline runs on a worker thread sized from the input (16 MiB + 1 KiB/byte release, 4 KiB/byte debug). 24 nesting patterns at 1 MiB pass in debug and release (tests/limits.rs, also run in release in CI). Oxc's quadratic printer indentation is avoided by dropping indentation past a budget; nested `switch` is quadratic in Oxc semantic (about 2.3 s release for the worst 1 MiB), documented in the ADR addendum.
- Refusals: `accessor`, `import x = require()`, `export =`, decorators without experimental_decorators, references to names exported by another block of a merged namespace (Oxc leaves them unbound); in strip-only also enums, instantiated namespaces, parameter properties, value aliases and decorators.
- Tests: 19 byte-for-byte fixtures (twice each, LF-only, CRLF mapping test), 7 engine runs of the output, unit tests, limits.
- CI: new `typescript` job on Linux/Windows/macOS; build-test, standalone and msrv exclude the crate; boundary test `typescript_stays_out_of_the_engine_and_the_c_api` walks the resolved non-dev graph of the engine crates and the C API.
- ADR 0029 addendum records what hostile input and the fixtures found.

Gates: test262 0 regressions (default, --precompile, --snapshot, --only-intl402, QuickJS-NG); bench geomean 1.0054 (26 workloads, 4 interleaved rounds, JIT off; engine sources unchanged); size_probe default byte-identical (20,569,088); licence check passed; fuzz smoke passed; MSRV 1.88 compiled every crate (local link step hit a Windows PDB long-symbol error, unrelated; CI msrv runs on Ubuntu).
<!-- SECTION:FINAL_SUMMARY:END -->
