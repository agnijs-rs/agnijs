---
id: TASK-103.24
title: >-
  Engine: eval_bytecode must not panic on module bytecode with imports; syntax
  errors must keep their position
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T03:25:00Z'
updated_date: '2026-11-12T05:28:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
ordinal: 288000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while building the CLI (TASK-103.16). eval_bytecode panics on module bytecode that has imports (agnijs-js-vm modules.rs near line 558), which is a panic reachable from a public engine path; this conflicts with the rule that the C API and engine public paths never panic (TASK-103.3 AC 4 and 5). Separately, syntax errors reported to the host lose their source position, so the CLI re-parses the file to recover it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 eval_bytecode on module bytecode with imports returns an error or resolves the imports through the host, and never panics; a regression test covers it, including through the C API
- [x] #2 A syntax error reported to the host through the engine interface carries its line and column, and the CLI's re-parse workaround is removed
- [x] #3 The fuzz or panic-audit tests from TASK-103.3 include bytecode with imports and pass
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Tests first (watch them fail): engine-interface tests in agnijs-js-vm/tests/bytecode.rs (module bytecode with imports through eval_bytecode, a missing import, a failed partial graph retried) and stack_traces.rs (syntax error position in Exception.stack and error.stack for eval_script, compile_*, load_module, through the source map); C API tests in basics.rs (eval_bytecode with imports, missing import is EXCEPTION not PANIC, syntax error frame); fuzz.rs: module bytecode with imports, plain and mutated.
2. Semantics: eval_bytecode on module bytecode resolves and loads its requests through the host's resolve/load hooks (referrer = the module's name), exactly like a module the loader answered with bytecode; a load failure is returned as the exception, like a link failure in the same call. Loading follows ECMA-262 LoadRequestedModules: a record stays 'new' (not loaded) until its whole graph loads, so a failed load is retried and a partial graph is never linked.
3. Defence in depth: linking, evaluation and the LdaImport/import.meta hooks return errors instead of expect/unwrap/panic where a host call can reach them (unloaded requests, a record in the wrong state, missing module info or function). Module-info operands (slots, names, request indices) stay trusted as produced, per ADR 0010; no module-info verifier is added.
4. Syntax errors from eval_script, compile_script, compile_module and module loading carry their position: a first frame (script, line, column in UTF-16 units, source-mapped) in Exception.stack and an 'at file:line:col' line in the error's stack text.
5. CLI: drop the syntax_frame re-parse and the agnijs-js-parser dependency; code_frame reads the position from the stack, using the main source text only as a text lookup for code the engine holds no source for.
6. clippy fix crates/agnijs/tests/embed.rs:207.
7. Gates: workspace tests, clippy -D warnings, capi fault injection/fuzz, header drift, CLI tests, test262 default/--precompile/--snapshot/--only-intl402, QuickJS-NG baseline.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Semantics chosen for eval_bytecode on module bytecode with imports: its requests are resolved and loaded through the host's resolve_module/load_module hooks with the module's own name as referrer, the same path a module takes when the host's loader answers with bytecode. Rejecting such bytecode would make module bytecode with imports useless without a loader detour, and the hooks are already the one place the host decides what a specifier means, so running from bytecode should not change that. A load failure (hook error, not found, a syntax error in a dependency) is returned as Err(Exception), the same way a link failure already was in this call; the engine stays usable.

Loading now follows ECMA-262 LoadRequestedModules: ModuleRecord.loaded is the spec's 'new' status. A record is marked loaded only once its whole graph has loaded, so a cached record from an earlier failed load re-tries its missing requests, and linking refuses a record that is not loaded or still has a None request (TypeError "Module '...' cannot be linked: its imports have not loaded"). Before, a failed load left a half-loaded record in the cache and the next import of it reached expect("unloaded module request") in initialize_environment; a_module_graph_that_failed_to_load_is_never_linked_half_loaded covers that.

Panic audit of modules.rs neighbours: converted to errors: initialize_environment's unloaded request and missing module info, inner_module_evaluation on a record in the wrong state, execute_module/execute_async_module without a linked function, the async body's non-promise result, LdaImport outside module code or on an unresolved binding, import.meta outside module code, new_module_record on code without module info. Left as is: the record-kind accessors, linking/evaluation stack pops and native-slot types (internal invariants), and module-info slots/names/request indices, which ADR 0010 treats as trusted as produced (the checksum guards accidental corruption; the bit-flip fuzz loads mutated images and never panicked).

Syntax error position: compile errors become a SyntaxError whose ErrorStack carries an origin frame (name, line, UTF-16 column, through the source map). It is the first frame of Exception.stack for eval_script, compile_script, compile_module and module loading, and the error's stack text gets '    at file:line:col'. The CLI's syntax_frame re-parse and its agnijs-js-parser dependency are gone; its code frame reads the stack.

Verification (worktree agnijs-evalpanic): RED first for all new tests (engine tests failed, partial graph panicked at modules.rs:577, C API returned AGNIJS_ERR_PANIC 'unloaded module request', syntax frame_count 0, fuzz panicked); GREEN after. agnijs-js-vm + agnijs tests pass; agnijs-capi --features fault-injection all pass (basics, fuzz, hostile, fault_injection, lib); agnijs-cli 23/23; clippy -D warnings for capi (fault-injection) and cli and agnijs (--no-deps) clean; agnijs.h unchanged (no C API source change). test262: default 43192/43192, --precompile, --snapshot, --only-intl402 981/981 and QuickJS-NG baseline, all 0 regressions. Micro geomean not run: no interpreter hot path changed (LdaImport only swaps an index unwrap for get().flatten() and a let-else).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
eval_bytecode on module bytecode with imports no longer panics: it loads the module's requests through the host's resolve/load hooks (referrer = the module's name) and returns a load failure as an exception, like a link failure. Module loading now follows ECMA-262's 'new' status, so a graph that failed to load is retried and never linked half-loaded (the root cause of the expect in initialize_environment). Neighbouring host-reachable panics in linking, evaluation, LdaImport and import.meta are errors now; module-info operands stay trusted per ADR 0010. Syntax errors reported to the host carry a source-mapped line/column as the first stack frame and in the stack text, so the CLI's re-parse workaround and its parser dependency are removed. Also fixes the clippy bool_assert_comparison at agnijs/tests/embed.rs:207.

Tests: bytecode.rs (imports via host, missing import is an error then retry works, failed partial graph never linked), stack_traces.rs (position for eval_script/compile_script/compile_module, a loaded module, UTF-16 columns, source map), C API basics.rs (eval_bytecode with imports; missing import is AGNIJS_EXCEPTION; syntax error frame), fuzz.rs module_bytecode_with_imports_never_panics (imports, cycles, JSON, TLA, throwing and flaky dependencies, bit-flipped images, release, jobs, GC), CLI code frame for eval code and an imported module.

Gates: all workspace tests touched pass; capi fault-injection/fuzz pass; clippy -D warnings clean for the CI-gated crates; header unchanged; test262 default, --precompile, --snapshot, --only-intl402 and QuickJS-NG all 0 regressions. Micro benchmarks not run (no hot path changed). Risk: hosts that relied on eval_bytecode of module bytecode with imports panicking/returning PANIC now see their load hooks called.
<!-- SECTION:FINAL_SUMMARY:END -->
