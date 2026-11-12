---
id: TASK-103.9
title: Security model and capability-based sandboxing guide
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-12T06:29:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 244000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Write and publish the security model: the engine has no ambient authority (no DOM, no globals beyond ECMAScript, host capabilities granted one by one), what the limits protect against (memory, instructions, wall time, stack, regex), what the engine does not protect against (side channels, host bugs), and how bytecode must be authenticated by the host (the checksum is not a signature, see the ADR 0010 addendum). Review the engine against the model and file issues. Model: Sonnet for the document and review, Opus for any fix.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A security model document in docs/ states the threat model, the guarantees, the non-guarantees and the embedder checklist
- [x] #2 The engine is reviewed against it; every gap found is filed as a task, and the document is linked from the embedding guide
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Read the spec, ADRs 0009/0010/0013/0017/0021/0022/0023/0025, the Host trait, the limits in the VM, the C API guard and the CDP server.
2. Probe each limit with throwaway tests (memory, string length, stack, interrupt vs regex and native loops); delete them afterwards.
3. Write docs/security-model.md citing code and tests; file each gap as a task under TASK-103.
4. Link it from README, the embedding cookbook and c-api.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Probes (debug build, throwaway tests, deleted): 16 MiB limit let 64x4 MiB ArrayBuffers and a 2e6-entry Map through (heap_bytes < 200 KB) but stopped a 2e6-element array; doubling a 1 Mi string 12 times gives length 0 (u32 wrap in Heap::concat); 300,000 '[' via eval_script and JSON.parse overflowed a 2 MiB Rust thread (no stack budget outside the C API); an always-true interrupt hook was polled 0 times during /(?=(a+)+b)/ on 23 a's (3.9 s), /(a+)+b/ under BacktrackingOnly (3.9 s) and Array(3e6).join (0.7 s). Code reading: CDP has no Host/Origin check, target id agnijs-<port>, unbounded message reassembly; bytecode context-dependent operands are trusted. Gaps filed: TASK-103.25 to TASK-103.31. No engine code changed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added docs/security-model.md: threat model (untrusted source; host, bytecode, snapshots and debugger trusted), guarantees (no ambient authority with a table of every Host capability, forbid(unsafe_code) across engine crates, the C API never panicking, bytecode/snapshot version and checksum checks), each limit (memory, fuel, interrupt, stack, regex) with how it is enforced, the tests behind it and what it does not cover, non-guarantees (side channels, realms, host bugs and path confinement, trusted bytecode/snapshots, CDP, unsafe in capi and JIT, Rust-API panics), a 12-point embedder checklist and a Rust sandbox-host example checked against the real signatures. Every claim cites a file or test name. Linked from README (new 'Running untrusted code' section), docs/embedding-cookbook.md (prose only, the page is doctested) and docs/c-api.md.

Review gaps filed (parent TASK-103, labels ready-for-agent,security): TASK-103.25 memory limit misses ArrayBuffers, collection entries and single huge native allocations; TASK-103.26 no maximum string length, rope length wraps at 2^32; TASK-103.27 Rust embedding has no stack budget, deep input kills the process; TASK-103.28 interrupt never reaches regex backtracking or native loops (also makes the RegexMatcher::BacktrackingOnly doc comment wrong); TASK-103.29 CDP accepts cross-site/DNS-rebinding connections, predictable target id, unbounded messages; TASK-103.30 crafted bytecode/snapshots can reach VM invariants; TASK-103.31 Rust API panic semantics undefined. TASK-103.18 already covers fuzzing and SECURITY.md.

Docs-only change: no engine behaviour changed, no tests added.
<!-- SECTION:FINAL_SUMMARY:END -->
