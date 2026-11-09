---
id: TASK-100.6
title: 'Engine M5: regex, Date, collections, typed arrays, Proxy and BigInt'
status: Done
assignee: []
created_date: '2026-11-07T22:36:00Z'
updated_date: '2026-11-08T17:42:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.5
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 207000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The remaining ECMAScript built-ins. Spec sections 2 and 6.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 regress is evaluated against test262 and pathological patterns and the adopt-or-build decision is recorded in an ADR; the regex engine sits behind a seam crate
- [x] #2 RegExp, Date, Map, Set, WeakMap, WeakSet, WeakRef, FinalizationRegistry, ArrayBuffer, typed arrays, DataView, Proxy, Reflect, BigInt and single-agent SharedArrayBuffer and Atomics pass their test262 directories or are excluded with reasons (multi-agent tests are excluded as such)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
M5 built-ins landed on integration/js-engine:
- RegExp on regress behind the gable-js-regex seam (ADR 0013): named/duplicate-named groups, lookbehind, u/v modes, d indices, modifiers, RegExp.escape, Symbol methods, string iterator; per-VM compiled-pattern cache and one-entry UTF-16 input cache.
- Map/Set (ES2025 set methods, groupBy), WeakMap/WeakSet/WeakRef/FinalizationRegistry with ephemeron marking in gc.rs and ClearKeptObjects after the job queue drains.
- Date (full ES2025 + Annex B) on new Host hooks now_ms/time_zone_offset_ms/time_zone_name (ADR 0014); ISO parsing plus a lenient toString/toUTCString parser.
- ArrayBuffer (resizable, transfer, detached), SharedArrayBuffer (growable), %TypedArray% + 12 constructors incl. Float16Array, DataView, single-agent Atomics; Engine::detach_array_buffer wired.
- VM fixes found by the suite: TypedArray [[HasProperty]]/[[Set]] through other receivers, IsTypedArrayFixedLength over resizable buffers, DataView byteLength/byteOffset return 0 when out of bounds (current spec).
test262: 42225/42225 in-scope tests pass with the permanent exclusions; six eval-of-lone-surrogate tests excluded with a tracked parser cause.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
M5 delivered on integration/js-engine. regress adopted as the matcher behind the gable-js-regex seam (ADR 0013, with measurements and the pathological-pattern caveat). RegExp, Date, Map, Set, WeakMap, WeakSet, WeakRef, FinalizationRegistry, ArrayBuffer, SharedArrayBuffer, typed arrays (incl. Float16Array), DataView, Proxy, Reflect, BigInt and single-agent Atomics all pass their test262 directories: 42225/42225 in-scope tests, zero-failure baseline. Multi-agent Atomics tests are excluded as such; six eval-of-lone-surrogate tests are excluded with a tracked parser cause. Date reads its clock and time zone through new Host hooks (ADR 0014). Workspace tests (vm lib + conformance, gc, ic, regex, quickjs conformance) all pass.
<!-- SECTION:FINAL_SUMMARY:END -->
