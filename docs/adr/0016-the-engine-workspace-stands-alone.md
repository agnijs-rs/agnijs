# 16. The engine workspace stands alone: Gable reaches in only through the engine interface

Date: 2026-11-08

## Status

Accepted

## Context

The AgniJS engine (then `native/engine` in the Gable repository) was planned to leave this repository and ship as its own release once M9 lands (TASK-100.12). That split should be a `git subtree split` and nothing more, which only holds if nothing under `native/engine` depends on Gable's code, vocabulary or build setup. M7 (TASK-100.8) is where the engine-interface adapter lands and M8 and M9 bring the debugger and Gable's own suites, so this is the point where Gable-specific code would most easily leak into the engine. The regex seam (ADR 0013) and the Intl seam (ADR 0015) already show the shape: a dependency sits behind one crate and nothing else names it. This ADR applies the same idea to Gable itself, from the other side.

## Decision

**`agnijs-engine-api` is the only surface shared with Gable.** The engine interface (ADR 0006) is the boundary; the `Host` trait is how an embedder reaches in. Gable-specific adaptation (the Renderer's node handles, Gable's error handler, HMR policy, host globals, the template interpreter's needs) is written on Gable's side of that trait, in the Host runtime, never in an engine crate. When M7 to M9 need something new from the engine, it is added to the interface in embedder-neutral terms (a host hook, a handle type, a capability) and Gable implements it.

**No engine crate depends on anything outside the workspace.** Every path dependency resolves to a member in this repository; no Gable crate (such as `gable-designer`), npm package or repository file outside the workspace is read by a build script, a test or a generator. Test262 is fetched into `test262/`; the Intl generators resolve paths from the repository root.

**Engine docs and identifiers speak of the host or the embedder.** Crate docs, public Rust identifiers and JS-visible names do not use Gable's framework vocabulary (Host runtime, Renderer, error handler, template, `@gable/` packages, Gable Native). Gable appears in ADR context and history, not in APIs. The QuickJS-NG adapter's host-object class is `HostObject`, the conformance kit's `import.meta.url` scheme is `test://`.

**The `gable` prefix is the engine's working name, renamed once.** Crate names (`gable-js-*`, `gable-engine-*`), the engine id `gable`, the `agnijs-bc-1` bytecode tag and `AGNIJS_GC_STRESS` name the engine, not the framework, and none of them assumes Gable is the embedder. TASK-100.12 requires a name to be decided before the first external dependent; the prefix is replaced in one mechanical rename at that point rather than twice (once to a placeholder now, once to the chosen name), which would also collide with the milestone branches in flight.

**The rule is enforced, not just written down.** `crates/agnijs-js-test262/tests/boundary.rs` runs with the workspace tests and fails when a path dependency leaves the workspace, when a non-member `gable*` crate enters the dependency graph, or when engine sources use the framework terms above. The `standalone` job in `.github/workflows/native-engine.yml` exports only the tracked files of `native/engine` (`git archive HEAD:native/engine`) into an empty directory outside the checkout and runs `cargo build --workspace --locked` and `cargo test --workspace --locked` there.

## Considered options

Renaming the crates now to a neutral placeholder: satisfies the letter of "no Gable in names" today but forces a second rename once the real name is chosen and conflicts with every in-flight milestone branch. Keeping Gable-specific helpers in an engine crate behind a feature flag: compiles out, but the code and its vocabulary still move with the split and would have to be untangled then. A review checklist without a test: the rule decays silently while M7 to M9 change the engine and Gable's adapter together.

## Update: the split happened (TASK-100.12)

The engine now lives in its own repository (`agnijs-rs/agnijs`), split with `git subtree split` and renamed in one mechanical pass: crates `gable-*` became `agnijs-*`, the engine id `gable` became `agnijs`, the bytecode tag `gable-bc-N` became `agnijs-bc-N`, `GABLE_GC_STRESS` and the other `GABLE_*` variables became `AGNIJS_*`, and the test262 baselines and exclusions files `gable*` became `agnijs*`. The rule stays and is still enforced: `boundary.rs` now fails when any `gable*` crate enters the dependency graph, when a path dependency leaves the workspace, or when engine sources use the framework terms, and the `standalone` CI job still builds an export of the tracked files alone. Gable depends on the engine by pinned git revision and keeps its own harness and adapter code on its side of `agnijs-engine-api`.

## Consequences

- TASK-100.12's repository split is mechanical: the subtree already builds alone (the CI job proves it on every engine change) and carries no Gable references beyond the working name.
- M7 to M9 must express any new need as an engine-interface capability. If something cannot be expressed that way, that is a design question for the interface, raised rather than worked around inside the engine.
- The vocabulary check is a substring scan; a legitimate use of a listed term (for example an ECMAScript concept that shares a word) needs the list adjusted with a reason, not a workaround in the source.
- The working-name rename is a known, bounded follow-up inside TASK-100.12.
