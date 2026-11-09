---
id: TASK-100.15
title: 'Parser: keep lone surrogates in eval''d source'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:42:00Z'
updated_date: '2026-11-09T19:37:00Z'
labels:
  - ready-for-agent
dependencies: []
references:
  - crates/agnijs-js-test262/exclusions/agnijs.txt
  - docs/adr/0013-regexp-matching-adopts-regress-behind-a-seam.md
  - docs/adr/0010-versioned-bytecode-container.md
parent_task_id: TASK-100
ordinal: 227000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Source text reaches the owned engine's parser as UTF-8, so a lone surrogate in eval'd or Function-constructed source (built with String.fromCharCode) is replaced with U+FFFD before the lexer sees it. Six test262 tests are excluded for this in native/engine/crates/gable-js-test262/exclusions/gable.txt (three under test/language/literals/regexp/S7.8.5_*, plus two under test/annexB/built-ins/RegExp/RegExp-*-escape-BMP.js and one more S7.8.5 case). Fix by lexing UTF-16 (or WTF-8) source so lone surrogates survive. Matching and literal regex patterns in UTF-16 are already unaffected (ADR 0013). Engine crates must stay Gable-agnostic (ADR 0016).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Source text with lone surrogates (from eval, Function and the other dynamic-code entry points) reaches the lexer unchanged, in identifiers where the spec allows, string and template literals, and regex literals
- [x] #2 The six excluded test262 tests pass and their lines are removed from exclusions/gable.txt; the gable and gable --precompile baselines stay at zero failures, with no regressions in the parse-only baseline
- [x] #3 Parser and lexer speed on ordinary ASCII and BMP source does not regress measurably (check with the gable-js-bench startup and bytecode workloads)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Convert UTF-16 source once to a String plus a sorted side table of lone surrogates (offset, unit); each becomes U+FFFD, 3 bytes like WTF-8, so offsets stay valid. Host &str source has an empty table and pays nothing.
2. Lexer consults the table only where values are built: string slow path and escapes, template cooked and raw, regex body (now UTF-16 units). Identifiers keep rejecting the character (lone surrogates are never ID chars).
3. eval (direct and indirect) and CreateDynamicFunction (Function, generator, async, async generator) hand UTF-16 units to new compiler entry points; Function.prototype.toString rebuilds units from the table.
4. Carry the table in SourceText and in the bytecode container: format agnijs-bc-6, decoder verifies order, range and that each entry sits on a U+FFFD; ADR 0010 addendum.
5. Tests first: lone_surrogates.rs (literals, templates, regex, identifiers, toString, string built-ins, JSON, RegExp built-ins, host boundary), serializer round trip and corruption; then remove the six exclusions.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause: eval and Function converted the JS string with from_utf16_lossy before lexing, so lone surrogates became U+FFFD.

Design: agnijs_js_parser::source (from_utf16, lone_at, push_units, units_of) plus Parser/Lexer::with_lone_surrogates, parse_eval_with, parse_script_with; compile_eval_utf16 and compile_dynamic_function_utf16 in the compiler. One-byte (Latin-1) strings take the old path, since they cannot hold a surrogate. TemplateElement.raw and RegExpLiteral.pattern are UTF-16; regex syntax validation gained validate_literal(&[u16], flags).

Host boundary (engine-api docs): JsString and Value::String are lossless UTF-16 both ways, property keys too; eval_script takes &str, which cannot hold a lone surrogate (write the escape); Engine::to_string, JsString::to_string_lossy and Display replace with U+FFFD. Tested in lone_surrogates.rs.

Bytecode: agnijs-bc-6; each source carries count + (u32 offset, u16 unit) entries; the decoder rejects unordered entries, non-surrogate units and entries not on a U+FFFD. ADR 0010 addendum, spec section 4 paragraph.

Results:
- test262 agnijs 42231/42231 and --precompile 42231/42231 (was 42225 with 6 excluded; the 6 now pass), intl402 981/981, RegExp suites 2494/2494 under auto, backtracking-only and linear-when-eligible, parse-only 42495/42495, 0 regressions.
- Gable suites via the path-patched harness: 758 passed, 2 known timing failures (yjs perf, highlighter); harness Cargo.toml and Cargo.lock restored.
- Targeted AGNIJS_GC_STRESS=1 subset (ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight, promises, lone_surrogates) and builtin_equivalence pass; all agnijs-js-vm, parser, compiler, regex, engine-api and test262 crate tests pass.
- Bench, base origin/main a8f1bbe vs new, alternated, 3 rounds, median ms: compile-source 12.89 -> 12.75, bytecode-load 2.14 -> 2.10, engine-startup 0.59 -> 0.56, string-concat 64.09 -> 64.31, string-methods 32.10 -> 32.12 (7 rounds: 31.94 -> 32.11), json-stringify 47.45 -> 46.93, json-parse 44.43 -> 43.65, regex 160.14 -> 152.04, regex-tokenize 46.28 -> 46.24, regex-long-input 7.11 -> 7.43 (7 rounds: 7.29 -> 7.29). All within noise.

Pitfall worth knowing: cargo does not hash a path package's location, so building another worktree into the shared target dir can leave this worktree's artifacts looking fresh (older mtimes). Touch the sources after switching worktrees on one target dir.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Lone surrogates in eval and Function source now reach the lexer unchanged. UTF-16 source is converted once into a String plus a sorted side table of lone surrogates (each stands as U+FFFD, the same 3 bytes as WTF-8, so offsets hold); the lexer puts the units back in string, template (cooked and raw) and regex literal values, identifiers still reject them, and Function.prototype.toString returns the original units. Host &str source has an empty table, so ordinary parsing pays nothing (benches flat). Bytecode format bumped to agnijs-bc-6 to carry and verify the table (ADR 0010 addendum); the host boundary behaviour is documented on the engine-api (JsString lossless, Rust String conversions lossy). The six excluded test262 tests pass and their exclusions are gone: agnijs and --precompile 42231/42231, intl402 981/981, RegExp suites 2494/2494 under all matchers, parse-only 42495/42495, Gable suites 758 passed with the 2 known timing failures.
<!-- SECTION:FINAL_SUMMARY:END -->
