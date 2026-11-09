---
id: TASK-100.11
title: Keep the engine workspace Gable-agnostic
status: Done
assignee:
  - '@claude'
created_date: '2026-11-08T23:06:00Z'
updated_date: '2026-11-08T23:17:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.7
documentation:
  - docs/js-engine-spec.md
  - docs/adr/0015-intl-on-icu4x-behind-the-gable-js-intl-seam.md
  - docs/adr/0006-engine-interface-is-handle-based-and-host-driven.md
parent_task_id: TASK-100
ordinal: 223000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Gable JS engine (native/engine, TASK-100) is planned to become its own release outside Gable once M9 lands and a few optimization passes are done (see TASK-100.12). Extraction is only a mechanical repository split if nothing under the engine workspace depends on Gable code, vocabulary or build setup. M7 (TASK-100.8) is where the engine-interface adapter lands, so the boundary rule has to be in place before it, and applied as M7 to M9 land. The engine-api crate (gable-engine-api, ADR 0006) is the only surface shared with Gable; the host trait is how Gable reaches in. The Intl seam (ADR 0015) and the regex seam (ADR 0013) are the pattern to follow.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 No crate in native/engine depends on any Gable crate, template vocabulary or renderer type; Gable-specific adaptation lives on the Gable side of the host trait
- [x] #2 A CI job builds and tests the engine workspace from a checkout containing only native/engine (for example a copy into a temporary directory), proving it builds standalone
- [x] #3 Engine-crate names, docs and public identifiers do not assume Gable (Gable may appear in ADR context and history, not in APIs); a short note in docs/js-engine-spec.md records the boundary rule for M7 to M9
- [x] #4 The rule is recorded as an ADR in docs/adr
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Boundary test in the engine workspace (gable-js-test262 tests/boundary.rs, the workspace tooling crate): via cargo metadata, every path package in the resolved graph lives under native/engine and is a workspace member; no dependency on a Gable framework crate (gable-designer, @gable packages). Red first by asserting against the current graph, then a vocabulary scan of crate sources and manifests for Gable framework terms (Host runtime, Renderer, @gable/, gable-ui, Gable Native, designer, template interpreter, gable:// scheme).
2. Fix what the scan finds: crate docs say 'embedder'/'host' instead of Host runtime and Gable error handler; QuickJS class GableHostObject -> HostObject; <gable-root> -> <root>; conformance kit test:// scheme instead of gable://.
3. Standalone check: scripts/check-standalone.mjs (Node) copies native/engine (excluding target, test262) to a temp dir and runs cargo build/test --workspace --locked there; run it locally.
4. CI: native-engine.yml job 'standalone' that copies native/engine to RUNNER_TEMP and builds and tests there.
5. ADR 0016 recording the boundary rule (the gable- prefix is the engine's working name; the one rename happens with the name decision in TASK-100.12); short note in docs/js-engine-spec.md section 10 for M7-M9.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red/green: boundary.rs path-dependency and framework-crate tests failed against a temporary path dependency on a fake gable-fake-renderer crate outside native/engine (both reported it), passed once removed; the vocabulary test failed on 5 sites (Host runtime in engine-api docs, gable:// in the conformance kit) and passed after the fixes. Also renamed QuickJS class GableHostObject -> HostObject, <gable-root> -> <root>, 'not Gable bytecode' -> 'not a GBJS container'. cargo test --workspace --locked: all 24 result lines ok. Standalone: git archive HEAD:native/engine extracted into an empty scratch directory outside the repo; cargo build --workspace --locked and cargo test --workspace --locked both succeed there (24 ok, 0 failed, boundary 3/3). Decision recorded in ADR 0016: the gable- crate prefix, engine id gable, gable-bc-1 tag and GABLE_GC_STRESS are the engine's working name and are renamed once in TASK-100.12 when the name is decided, rather than now (would collide with in-flight milestone branches and force a second rename).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The engine workspace now enforces its independence from Gable. crates/gable-js-test262/tests/boundary.rs (runs with the workspace tests) fails if a path dependency leaves native/engine, if a non-member gable* crate enters the dependency graph, or if engine sources use Gable framework vocabulary (Host runtime, Renderer, error handler, @gable/, gable-ui, Gable Native, gable-designer, template interpreter, gable://). Engine-interface docs now say host/embedder; the QuickJS-NG host class is HostObject; the conformance kit uses test://. A new standalone job in native-engine.yml exports only native/engine via git archive into RUNNER_TEMP and runs cargo build/test --workspace --locked there; the same export built and tested locally with all tests passing. ADR 0016 records the rule and the spec section 10 has the M7-M9 boundary note. Open point for review: crate names keep the gable- prefix as the engine's working name (ADR 0016 explains why); the one rename belongs to TASK-100.12 when the standalone name is chosen. The new CI job has not run on GitHub yet; it will on the next push touching native/engine.
<!-- SECTION:FINAL_SUMMARY:END -->
