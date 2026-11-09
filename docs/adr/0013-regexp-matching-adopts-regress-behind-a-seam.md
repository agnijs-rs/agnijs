# 13. RegExp matching adopts the `regress` backtracking engine behind a seam that owns the ECMAScript semantics

Date: 2026-11-08

## Status

Accepted

## Context

M1 validated regular expression *syntax* with our own grammar (ADR 0007) and left the matcher open: adopt an existing Rust engine or write one. The spec asks M5 for the full ES2025 RegExp built-in (named and duplicate named groups, lookbehind, `u` and `v` modes with property escapes and set operations, match indices, modifiers, sticky and global semantics, `RegExp.escape`) and for a recorded decision on the engine.

The candidates were `regex` (RE2-style automaton, linear time, but no backreferences or lookaround and a different syntax), `fancy-regex` (backtracking over `regex`, byte oriented, no UTF-16 semantics) and `regress` (a backtracking engine written to the ECMAScript grammar, with UTF-16 input, `u`/`v` modes, lookbehind, backreferences, named groups and property escapes). A homegrown matcher was costed at several weeks before it would pass test262's 2000 RegExp tests.

## Decision

- **`regress` is the matcher**, used through `agnijs-js-regex::Regex` (`compile`, `exec`, `exec_sticky`, `group_count`, `group_names`). Nothing outside that crate names `regress`; the VM's `builtins/regexp.rs` holds every ECMAScript algorithm (RegExpAlloc/Initialize, RegExpBuiltinExec with the `d` indices and duplicate-name `groups` rules, the `@@match`/`@@matchAll`/`@@replace`/`@@search`/`@@split` protocol, `RegExp.escape`, the string iterator) against the seam only.
- **The seam resolves what the engine does not.** Our grammar check runs first on the UTF-16 pattern (so literals, `new RegExp` and lone-surrogate patterns report the same errors). Capture-group names are read by the seam and *stripped* before the engine sees the pattern: `(?<name>` becomes `(` and `\k<name>` becomes the numbered references of every group with that name (duplicate names live in different alternatives, so at most one holds a capture and `(?:\1\2)` matches exactly the participating group). This gave correct duplicate-name backreferences and astral group names without the `u` flag, both of which `regress` got wrong, and keeps the backend replaceable by anything that matches numbered groups over UTF-16.
- **Sticky and global are VM concerns.** The engine only searches from an index; the VM implements `lastIndex`, the sticky start check, `AdvanceStringIndex` and the unicode-mode surrogate-pair alignment.
- **Compiled matchers are shared.** A per-VM cache keyed by (pattern, flags) means a literal in a loop body compiles once; a one-entry cache of the last input's UTF-16 form keeps global match/replace/split linear in the input length (without it a 200k-character global replace copied the input per match and took 51 s; it now takes 0.26 s).

## Measurements (release build, `test262 eval`)

| Workload | Time |
| --- | --- |
| `/a+b/` over 100k characters, ten times | 48 ms |
| 10k `exec` calls of `/\d{3}-\d{4}/` | 61 ms |
| `/^x+$/gm` match over 1M characters | 229 ms |
| 200k-character global replace | 263 ms |
| `/\p{Lu}+/u` over 100k characters | 44 ms |
| `/(a\|aa)+$/` on 30 a's and a `!` | 298 ms |
| `/(a+)+$/` on 28 a's and a `!` (catastrophic) | 14.9 s |

test262: the RegExp, RegExpStringIteratorPrototype, `language/literals/regexp`, Annex B RegExp and `String.prototype` directories run 3236/3245; the nine remaining failures are six eval-of-lone-surrogate tests (the lexer reads UTF-8, see below) and three tests that need Map or Date.

## Consequences

- Backtracking means pathological patterns are exponential, as in every production JavaScript engine without a linear fallback; the host's interrupt mechanism (ADR 0006) remains the guard for untrusted patterns. A linear-time fallback for patterns without backreferences or lookaround is a possible M9 addition behind the same seam.
- Replacing the engine touches one crate: the seam's contract is "find the leftmost match from an index over UTF-16 units with numbered captures".
- Patterns that reach eval with lone surrogates in the *source text* are lexed as UTF-8 and lose the surrogate; this is a parser-level limitation (not a RegExp one) tracked for the parser.

## Addendum (TASK-102): a linear-time matcher behind the same seam

Date: 2026-11-08

The linear-time fallback named under Consequences now exists, and it is on by default. `regress` is still the matcher for everything the fallback cannot run, and for ordinary patterns, where it is faster.

### The matcher

`agnijs-js-regex/src/linear.rs` is a Pike VM with leftmost-first thread priority. It runs one pass over the input, with work bounded by program size times input length.

- **Parsing.** It parses the numbered-groups pattern the backtracking matcher sees, mirroring our grammar: Annex B octal and identity escapes, `\c`, modifiers, and `u`/`v` code-point mode.
- **Character semantics come from `regress`.** Each class, escape and case-folded literal becomes a one-character predicate `^(?:X)$`, compiled by `regress` under the pattern's flags and memoized per character. Case folding, property escapes and `v`-mode set operations therefore cannot diverge.
- **ECMAScript repetition semantics are kept:**
  - captures inside a quantified group are reset on each iteration;
  - an iteration past the minimum that consumes nothing is rejected. Per-nesting-level "consumed" bits are part of the thread state, at most four levels deep.
- **Counted quantifiers** are unrolled. Programs are capped at 20,000 instructions and 2^18 thread-table entries.
- **Optimisation.** When no thread is alive, the matcher skips ahead to the next position where the first character of the pattern can match.

### Routing (`MatcherMode`, decided per `exec` call)

A pattern goes to the backtracking matcher when the linear matcher cannot run it. That covers:
- backreferences and lookaround;
- `v`-mode strings (`\q{}` and properties of strings);
- outside `u` mode: braced `\u{}`, and an escaped lead surrogate followed by another `\u` escape (see the bugs below);
- programs past the size caps.

`Regex::linear_eligibility` reports the reason.

For an eligible pattern, `Auto` estimates the backtracking worst case:
- **Exponential.** A repeated body that itself contains a quantifier with max > 1, or an alternation, makes the worst case exponential. Such patterns always run linear.
- **No unbounded loop.** The estimate is `size × choices`, where choices is the product of alternation widths and the bounded-quantifier ranges. This is independent of the input.
- **Otherwise.** The estimate is `size × choices × starts × (len + 1)^loops`. Starts is 1 for a sticky attempt or a pattern anchored by a non-multiline `^` in every alternative, else `len + 1`.

Backtracking is kept when the estimate is at most `max(BACKTRACK_BUDGET, 16 × size × (len + 1))`, with `BACKTRACK_BUDGET = 2^20` steps. The second term is the linear matcher's own cost: a pattern whose backtracking is already linear in the input stays where it is, however long the input. One loop under `^`, and sticky tokenizers, are the common cases. The estimate overcounts sequential loops whose character sets cannot overlap (`[a-z]+\d+@`). Such patterns move to the linear matcher on long inputs although backtracking would have been fine. That costs speed, never correctness.

`BacktrackingOnly` never runs the linear matcher. `LinearWhenEligible` runs it for every eligible pattern; it is meant for hosts that take untrusted patterns, and for measurement.

### Embedder control

- **`Host::regex_matcher()`** (agnijs-engine-api, `RegexMatcher::{Auto, BacktrackingOnly, LinearWhenEligible}`, default `Auto`) is read once by `Vm::new`. Each engine has its own policy, fixed for its lifetime, with no process-global switch. The test262 runner exposes it as `--regex-matcher`, and agnijs-js-bench as the engines `agnijs-backtracking` and `agnijs-linear`.
- **Cargo features.** The `linear` feature of `agnijs-js-regex`, forwarded as `linear-regex` on `agnijs-js-vm` (default on), compiles the matcher out. The workspace depends on `agnijs-js-regex` with `default-features = false`, so the parser's dependency does not switch it back on.
- **Boundary.** None of this names Gable; the crates stay host-agnostic (ADR 0016).

### Verification

- **Differential fuzzing** (`tests/differential.rs`) compares the two matchers on random patterns across flags `dgimsuyv`. Patterns cover:
  - captures, named and duplicate-named groups;
  - modifiers, every quantifier form, assertions, classes, escapes;
  - Annex B forms and `u`/`v` syntax.

  Inputs are short random strings, lone surrogates included. Each pattern is compared on `exec` from every start, sticky from every start, and the global `lastIndex` loop with `AdvanceStringIndex`.

  `cargo test` runs 10,000 eligible patterns × 6 inputs (about 740k compared outcomes), and CI adds a bounded 200,000-pattern release run. Two 200,000-pattern seeds, about 14.8M outcomes each, show zero mismatches. A watchdog aborts and names the pattern if one case hangs.
- **ReDoS regression tests** (`tests/redos.rs`) run on 5,000-character inputs, each finishing in milliseconds:
  - `(a+)+$`, `(a*)*!x`, `(a|aa)+$`, `(a|a?)+!x`, `^(\w+\s?)*$`, `(x+x+)+y`, `(.*a){12}`;
  - polynomial cases such as `\s+$` on 200k spaces.
- **Routing tests** are in `tests/routing.rs`, and the per-engine host option is tested in `agnijs-js-vm/tests/regexp.rs`.
- **test262.** The RegExp, `String.prototype` match/matchAll/replace/replaceAll/search/split, `language/literals/regexp` and Annex B RegExp directories pass 2505/2505 under all three policies, against the same baseline. CI runs `backtracking-only` on every OS and `linear-when-eligible` on Linux. The full gable, `--precompile` and intl402 runs are unchanged.

### Measurements (agnijs-js-bench, release, i9-9900KF, median of 5)

| Workload | auto | backtracking only | linear when eligible | QuickJS-NG |
| --- | --- | --- | --- | --- |
| `regex` (captures, exec, global replace) | 383 ms | 360 ms | 431 ms | 136 ms |
| `regex-tokenize` (sticky tokenizer) | 101 ms | 103 ms | 86 ms | 22 ms |
| `regex-long-input` (40k chars, incl. `\s+$`) | 7.6 ms | 2.4 ms | 7.7 ms | 3.1 ms |
| `regex-redos` (`/(a+)+$/`, 20 a's) | 0.01 ms | 114 ms | 0.01 ms | 93 ms |

`(a+)+$` on 28 a's, the 14.9 s row above, now returns in microseconds; with 5,000 a's it still finishes in milliseconds.

The cost of forcing the linear matcher everywhere shows in test262's property-escape tests, which run `\p{…}+` over strings of whole Unicode ranges: 300 s for the RegExp directories against 86 s under `auto`. The per-character predicate lookup for non-ASCII input is the slow part; there is room to speed it up.

### Divergences found in `regress` (kept, routed around)

The fuzzer found differences that are bugs in the backtracking matcher, not in the linear one. Those patterns stay on the backtracking matcher, so both policies return the same results and the existing behaviour is unchanged:

- Outside `u` mode, `/\uD83D\uDE00/` never matches the two code units it names, because the escapes are joined into one code point. An escaped lead followed by another `\u` escape also misses: `/\uD83D\u0062?/` does not find the lone lead in `"a\uD83D\uDE00"`.
- Outside `u` mode, `\u{2}` is read as a code-point escape rather than `u` repeated twice.
- `\b` and `\B` under `vi` do not use the case-folded word characters. The linear matcher's word test matches `regress` here, so they agree.
- `/(?:(_|)+){2}K/` on `"__"` never returns from the backtracking matcher. The fuzzer limits quantifier nesting to stay clear of it.
- A search started in the middle of a surrogate pair in `u` mode can return an inverted range. The VM never starts there, because it aligns first.

The fuzzer also exposed a seam bug, now fixed. The sticky wrapper `(?:P)()|` adds a group, which turned an Annex B escape `\N` with N = group count + 1 (for example `\8` with seven groups) into a backreference. `pin_group_count_escapes` now spells such escapes out before wrapping.

### Anchored patterns try index 0 only (TASK-100.30)

Date: 2026-11-09

A failed `exec` of `/^x/` was linear in the input: the seam asked the backtracking matcher for the leftmost match from `start`, and `regress` tried every position, as did the Pike VM's skip-ahead (3.6 s for 200 calls over 4M characters). `Regex::compile` now records whether every alternative of the numbered-groups pattern begins with a `^` that the m flag does not reach (`Regex::starts_anchored`): directly, or inside capturing, non-capturing and modifier groups that do not add `m`, never through lookaround or a quantified group. For such a pattern `exec` answers `None` at once when `start > 0` (a global regex whose `lastIndex` moved on) and otherwise runs the exact-start attempt at 0 that `exec_sticky` already had, so both matchers stop at index 0. Semantics are unchanged by construction: a non-multiline `^` only holds at index 0, so the leftmost match from 0 is the match at 0. The routing cost model already counted one start for anchored patterns, so nothing moves between matchers. The differential fuzzer checks the shortcut against the same pattern behind an empty `(?:)`, which hides the anchor from it.
