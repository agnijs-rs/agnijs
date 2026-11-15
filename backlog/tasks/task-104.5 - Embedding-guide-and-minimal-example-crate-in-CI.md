---
id: TASK-104.5
title: Embedding guide and minimal example crate in CI
status: Done
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
updated_date: '2026-11-15T22:40:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: docs
ordinal: 328000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A first external embedder needs one page that walks the whole surface and a crate it can copy. The cookbook covers the Context layer only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 docs/embedding-guide.md walks through embedding: construct an engine, run a script and a module, expose a host function and a host class, drive the job loop, limits and errors, handle lifetimes, with links to the cookbook, the security model, SECURITY.md and the fuzzing corpus
- [x] #2 A minimal example crate in the workspace embeds the engine, runs a script and a module, exposes a host function and runs the job loop; CI builds and runs it and checks its output
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
docs/embedding-guide.md walks the engine interface: the two layers, building an engine and running a script, a host function, a host class with Rust data (factory, prototype methods, host_object_data, the root rule for held handles, finalizers), modules through resolve_module/load_module, the job loop, the Error table, limits (fuel, stack budget, memory; interrupt), handle lifetimes, versioning, and links to the cookbook, security model, SECURITY.md, docs/fuzzing.md and the seed corpus (crates/agnijs-fuzz/src/seeds.rs). Its code blocks run as doctests of agnijs (EmbeddingGuide, like the cookbook): 7 pass. examples/embed-minimal (agnijs-example-embed, publish = false, a workspace member) runs a script, a host function, a two-module graph, the job loop and an exception; tests/output.rs checks its exact stdout and the build-test CI job runs it and that test explicitly. Cookbook and spec link the guide.
<!-- SECTION:FINAL_SUMMARY:END -->
