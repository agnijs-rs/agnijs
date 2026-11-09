---
id: TASK-102
title: Linear-time regex matcher for ReDoS-safe patterns
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T02:03:00Z'
updated_date: '2026-11-09T04:36:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.10
references:
  - docs/adr/0013-regexp-matching-adopts-regress-behind-a-seam.md
  - docs/adr/0006-engine-interface-is-handle-based-and-host-driven.md
ordinal: 234000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Backtracking regex engines have exponential worst cases (ReDoS): with regress, /(a+)+$/ on 28 a's and a '!' takes 14.9 s (ADR 0013). Every JS engine has this because ES semantics are defined by backtracking, but a safe, embeddable engine that is immune for the common case is a real selling point for AgniJS consumers running patterns on untrusted input. Add a linear-time matcher (Pike VM / Thompson NFA with ECMAScript leftmost-first priority, captures, case folding and Unicode modes) inside the gable-js-regex seam (ADR 0013), used for patterns without backreferences or lookaround; everything else stays on regress. Routing must be conservative: when unsure, use regress. Keep the engine Gable-agnostic (ADR 0016); crate rename is TASK-100.12. Suitable for an Opus or a Fable agent; the differential fuzzer against regress is the main safety net and is an acceptance criterion, not optional.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Eligible patterns (no backreferences, no lookaround) run on a linear-time matcher with time proportional to pattern size times input length; the 14.9 s /(a+)+$/ case and the other pathological patterns in ADR 0013 finish in milliseconds
- [x] #2 A differential fuzzer compares the new matcher with regress on random eligible patterns and inputs across flags (i, m, s, u, v, y, g, d), including captures, named groups, empty matches and lastIndex behaviour, and runs a documented minimum number of cases in CI with zero mismatches
- [x] #3 The existing RegExp test262 suites, the gable-js-regex tests and the gable and --precompile baselines show no regressions; ineligible patterns still use regress and the routing rule is documented and tested, including the conservative fallback
- [x] #4 gable-js-bench has regex workloads showing the cost of the linear matcher against regress on ordinary patterns, and a step or size trigger keeps ordinary patterns on the faster matcher where that is safe; the choice is recorded in an ADR addendum to 0013
- [x] #5 The linear matcher is ON by default and embedders can opt out: a per-engine or per-realm host option (auto by default, backtracking_only for V8-style behavior, set at creation time, not global) and a cargo feature that compiles the linear matcher out entirely; the mode never changes which strings match, only how, and the RegExp test262 suites and the differential tests are run in both modes in CI
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. gable-js-regex: new linear module (cargo feature 'linear', default on): parse the stripped numbered-group pattern into an AST, route (backrefs, lookaround, v-mode string classes, nesting/size caps -> regress), compile to a Pike VM program (counted quantifiers unrolled, ES capture reset per iteration, ES empty-iteration check via per-thread consumed bits keyed into thread dedup).
2. Character semantics are delegated to regress: every class/escape/icase atom becomes a single-character predicate compiled by regress and memoized per code unit/point, so case folding, property escapes and v-mode set operations match regress exactly.
3. Regex gains a MatcherMode (Auto, BacktrackingOnly, LinearWhenEligible); Auto keeps a pattern on regress when an estimated worst-case backtracking step count (input length and quantifier structure) is under a budget, otherwise the Pike VM.
4. Differential fuzzer test (deterministic PRNG, documented minimum case count, env override) comparing both matchers over random eligible patterns, flags, start indexes, sticky and global iteration; ReDoS regression tests.
5. gable-js-vm: EngineOptions { regex } via make_engine_with, feature forwarding; test262 runner flag --regex-matcher; bench engine kinds for backtracking-only and linear-forced plus regex workloads.
6. CI: test262 RegExp dirs and regex tests in backtracking-only mode and with the feature compiled out. ADR 0013 addendum. Verify test262 gable/precompile/intl402 baselines.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): implement with an Opus agent. Fable is reserved for performance work, the JIT and AOT.

Implementation (2026-11-08):
- Plan deviation: the host option is a Host trait method, Host::regex_matcher() -> RegexMatcher {Auto (default), BacktrackingOnly, LinearWhenEligible}. Vm::new reads it once, so the policy is per engine and fixed at creation. This replaces the EngineOptions/make_engine_with planned earlier: no new constructor needed in any layer (C API, CDP, bench, test262).
- Cargo: gable-js-regex feature 'linear' (default on). The workspace dependency uses default-features = false. The gable-js-vm feature 'linear-regex' (default) turns it on.
- Auto routing: a nested quantifier means linear. Otherwise the backtracking worst-case estimate (size x choices x starts x (len+1)^loops; starts = 1 if anchored or sticky) is compared with max(2^20, 16 x size x (len+1)).
- Fuzzer bugs found in the backtracking path:
  - escaped lead surrogate outside u mode;
  - non-u \u{;
  - \b under vi;
  - a regress hang on (?:(_|)+){2}K;
  - a seam bug: sticky wrapper's extra group made \N (N = groups + 1) a backreference. Fixed in pin_group_count_escapes.
- Ineligible patterns are routed to regress so both policies agree.
- Forcing the linear matcher everywhere is slow on test262 property-escape tests (300 s vs 86 s); follow-up perf possible.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added a linear-time Pike VM matcher (gable-js-regex/src/linear.rs) behind the ADR 0013 seam. It is on by default.

- Routing: eligible patterns (no backreferences or lookaround) are sent to it by a documented worst-case cost model. Nested quantifiers always run linear; patterns that are cheap or already linear stay on regress. Everything else uses regress.
- Opt-out: Host::regex_matcher() selects Auto, BacktrackingOnly or LinearWhenEligible per engine at creation time. Cargo features gable-js-regex/linear and gable-js-vm/linear-regex compile the matcher out.
- Tooling: test262 runner --regex-matcher; bench engines gable-backtracking and gable-linear, plus regex-tokenize, regex-long-input and regex-redos workloads.
- Documentation: ADR 0013 addendum records the design, routing, numbers and the regress divergences found.

Evidence:
- ReDoS tests: (a+)+$ and others on 5k chars finish in milliseconds; regex-redos runs 0.01 ms vs 114 ms on backtracking.
- Differential fuzzer: 10k patterns x 6 inputs in cargo test, and a bounded 200k-pattern release run in CI. Two 200k seeds locally gave zero mismatches.
- test262 RegExp suites: 2505/2505 under all three policies.
- Full test262: gable 42225/42225, --precompile 42225/42225, intl402 981/981.
- Workspace tests pass. VM and regex tests also pass without the feature.
- Fixed a pre-existing sticky bug: \N with N = groups + 1.
<!-- SECTION:FINAL_SUMMARY:END -->
