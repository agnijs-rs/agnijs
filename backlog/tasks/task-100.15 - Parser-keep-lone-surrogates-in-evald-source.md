---
id: TASK-100.15
title: 'Parser: keep lone surrogates in eval''d source'
status: To Do
assignee: []
created_date: '2026-11-09T01:42:00Z'
labels:
  - ready-for-agent
dependencies: []
references:
  - native/engine/crates/gable-js-test262/exclusions/gable.txt
  - docs/adr/0013-regexp-on-regress-behind-the-gable-js-regex-seam.md
parent_task_id: TASK-100
ordinal: 227000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Source text reaches the owned engine's parser as UTF-8, so a lone surrogate in eval'd or Function-constructed source (built with String.fromCharCode) is replaced with U+FFFD before the lexer sees it. Six test262 tests are excluded for this in native/engine/crates/gable-js-test262/exclusions/gable.txt (three under test/language/literals/regexp/S7.8.5_*, plus two under test/annexB/built-ins/RegExp/RegExp-*-escape-BMP.js and one more S7.8.5 case). Fix by lexing UTF-16 (or WTF-8) source so lone surrogates survive. Matching and literal regex patterns in UTF-16 are already unaffected (ADR 0013). Engine crates must stay Gable-agnostic (ADR 0016).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Source text with lone surrogates (from eval, Function and the other dynamic-code entry points) reaches the lexer unchanged, in identifiers where the spec allows, string and template literals, and regex literals
- [ ] #2 The six excluded test262 tests pass and their lines are removed from exclusions/gable.txt; the gable and gable --precompile baselines stay at zero failures, with no regressions in the parse-only baseline
- [ ] #3 Parser and lexer speed on ordinary ASCII and BMP source does not regress measurably (check with the gable-js-bench startup and bytecode workloads)
<!-- AC:END -->
