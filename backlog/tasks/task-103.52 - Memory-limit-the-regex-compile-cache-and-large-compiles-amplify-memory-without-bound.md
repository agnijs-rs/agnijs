---
id: TASK-103.52
title: >-
  Memory limit: the regex compile cache and large compiles amplify memory
  without bound
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 317000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: low to medium (a memory-limit bypass by amplification). docs/security-model.md admits that the engine's own tables are not counted, and promises the process stays within a small factor of the limit ("under three times" in the peak test). This cache breaks that bound with no fixed factor.

Affected code: crates/agnijs-js-vm/src/builtins/regexp.rs around lines 44 and 73 to 79. The regex compile cache keeps up to 1,024 entries, each a UTF-16 copy of the pattern plus the compiled program, none of it counted, and it keeps them after the script drops every RegExp. Compiled programs are about 80 times the size of their pattern.

Minimal repro:

    const src = '(a)'.repeat(5000); for (let i = 0; i < 1000; i++) new RegExp('x' + i + src);

Command: `agnijs run --max-memory 16M <file>` (debug build at fea680f).

Expected: the process stays within a small multiple of 16 MiB, or the script fails with out of memory.
Actual: exit 0 with a peak working set of 525 MiB, and nothing is kept alive by the script. Related observations: one `new RegExp('(a)'.repeat(2e6))` peaks at 505 MiB, and twelve `new Function` compiles of one 1.2 MB source reach 190 to 280 MiB. That is bytecode, which the docs list as uncounted, but the amplification is just as unbounded.

Suggested fix: cap the cache by bytes, not by entry count. Do not cache programs above a size threshold. Charge compiled program size (regex and bytecode) to the memory limit while the program is reachable, or at least check the limit before a large compile. Update the "Approximate sizes" gap in security-model.md to name compile-time amplification.

Test that should catch it: a memory_limit.rs test that compiles many distinct large patterns under a small limit and checks the process peak or the engine's accounting.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Compiling many large distinct patterns under a small limit stays within the documented factor or fails with OutOfMemory
- [ ] #2 The cache is bounded by bytes, not only by entry count
- [ ] #3 docs/security-model.md names compile-time amplification in its gaps
<!-- AC:END -->
