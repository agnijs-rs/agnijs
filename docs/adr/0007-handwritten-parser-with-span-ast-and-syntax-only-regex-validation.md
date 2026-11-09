# 7. The engine parser is handwritten, produces a span-carrying ESTree-shaped AST, and validates regex literals syntax-only

Date: 2026-11-08

## Status

Accepted

## Context

Milestone M1 of the Gable JS engine (`docs/js-engine-spec.md`, section 3 and section 12) needs a parser that passes every in-scope test262 parse-positive and parse-negative test, covers Annex B, and gives the debugger and source maps positions. Several choices were open: generated versus handwritten parser, how to represent positions, how to handle the cover grammars, and how far regular-expression literals are checked at parse time when the regex engine itself lands in M5.

## Decision

- **Handwritten lexer and recursive-descent parser** (`agnijs-js-parser`). The lexer is parser-driven: `/` and `}` are lexed as punctuators and the parser asks for a rescan (`rescan_regexp`, `rescan_template_continuation`) when the grammar says a regex literal or a template continuation is expected. No parser generator; ECMAScript's cover grammars and contextual keywords (`yield`, `await`, `let`, `async`, `of`, `static`, `get`, `set`) are easier to handle by hand.
- **Positions are byte offsets** (`Span {start, end}` on every node) plus a `LineTable` that converts an offset to a 1-based line and a UTF-16 column on demand. Storing offsets keeps nodes small; the debugger (M8) and source maps need line and UTF-16 column, which the table gives in O(log n).
- **AST shape follows ESTree** so the compiler (M2) and tooling can lean on a well-known structure. Differences: patterns are a separate `Pattern` enum, strings are UTF-16 (`Rc<[u16]>`), functions carry `strict`, `simple_params` and `has_direct_eval` computed during parsing, parentheses are kept as `Expression::Paren` because several early errors depend on them.
- **Cover grammars by conversion.** Parenthesised expressions and object/array literals are parsed as expressions and converted to patterns when `=>` or `=` follows (`expr_to_pattern`). Errors that are only errors in one reading (`{a = 1}` outside a pattern, duplicate `__proto__`) are deferred in `pending_cover` and raised once the reading is known. `yield`/`await` expressions inside a possible arrow head are recorded in `pending_yield_await` and raised when the head turns out to be parameters.
- **Early errors in the parser**, through a scope stack (`ScopeStack`: function top / block / catch, with Annex B's block-function and catch-parameter allowances) and a private-name environment per class body whose unresolved references bubble to the enclosing class.
- **Regex literals are validated syntax-only** by `agnijs_js_regex::syntax::validate`, our own recogniser for the ES2025 pattern grammar including Annex B's legacy syntax in non-unicode mode, the `v` flag's class-set grammar, named groups with the duplicate-named-groups rule, and regex modifiers. Only Unicode property names and values are checked by compiling a one-escape pattern with the `regress` crate, which carries the Unicode tables we would otherwise have to vendor. Matching semantics are M5's business.
- **Sloppy-mode `f() = 1` and `for (f() in o)` are accepted by the parser** (Annex B web-compat behaviour: a runtime `ReferenceError`), matching test262's expectations.

## Consequences

- The parser is checked by the test262 runner's `--engine parse` mode: positive tests must parse (sloppy and strict), tests negative in the `parse` phase must fail, harness files must parse. At the pinned test262 commit the parse pass has no failures and no exclusions (`baselines/parse.json` is empty), including intl402.
- When M5 replaces or wraps `regress`, the property-name check in `syntax.rs` must be pointed at the new tables; nothing else in the parser depends on `regress`.
- Byte offsets mean a consumer that wants line/column pays for a `LineTable` per source; the compiler should build it once per script and share it with the debugger.
- Keeping `Paren` nodes means later passes must call `unparen()` where parentheses are semantically transparent.
