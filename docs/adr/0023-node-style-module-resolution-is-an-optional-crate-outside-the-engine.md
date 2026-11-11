# 23. Node-style module resolution is an optional crate in the workspace, and the engine core knows nothing of it

Date: 2026-11-11

## Status

Accepted

## Context

The engine resolves and loads modules only through the host (`Host::resolve_module` and `Host::load_module`, ADR 0006). It has no notion of files, `node_modules` or `package.json`. Embedders that run code from npm packages have to reimplement the Node algorithm: relative and absolute paths, extension and directory-index lookup, `node_modules` walking, package `exports` and `imports` with conditions and `*` patterns, and self-reference by package name.

The question (TASK-103.14) is whether that belongs in AgniJS or in each embedder, and where it would live if it belongs here.

## Decision

**It is in scope, as a separate crate, `agnijs-js-resolve`, in this workspace.** The algorithm is fully specified, it is pure logic over a file system, and every embedder that wants npm packages needs the same thing. Putting it here gives one implementation with one fixture suite.

**The engine core does not depend on it.** No engine crate (`agnijs-engine-api`, `agnijs-js-vm`, the compiler, parser and the C API) lists `agnijs-js-resolve` as a dependency, and the resolver crate does not depend on `agnijs-engine-api` or any engine crate. It is a library an embedder calls from inside its own `Host::resolve_module`. Only the resolver's dev-dependencies reach the engine, so that an integration test can drive a real host.

**The file system is a trait.** The resolver reads through a small `FileSystem` trait (`is_file`, `is_dir`, `read_to_string`), with a `StdFs` implementation over `std::fs`. An embedder with a virtual or bundled file system implements the trait. Paths are strings with `/` separators; `StdFs` accepts Windows paths and the resolver returns `/`-separated paths.

**Supported:**

- relative (`./`, `../`) and absolute specifiers;
- extension resolution (configurable list; default `.js`, `.mjs`, `.cjs`, `.json`), directory resolution by `package.json` `main` and then `index`;
- bare specifiers by walking up `node_modules`, scoped packages included;
- package `exports`: string, array, conditions object (in key order), subpaths, `*` patterns, `null` targets, and self-reference by the package's own name;
- package `imports` (`#specifier`);
- a configurable condition set (default `import` and `default`).

**Not supported:** CommonJS `require` semantics beyond what resolution needs, Yarn Plug'n'Play, `NODE_PATH`, `file:` and `data:` URLs, `node:` builtins (the embedder answers those before calling the resolver), symlink canonicalisation (the resolver returns the path it found), and TypeScript path mapping. These are embedder or bundler policy.

## Consequences

- Embedders opt in by adding the crate. Builds that do not, pay nothing, and the engine's dependency graph is unchanged.
- The resolver is tested against a documented fixture tree under `crates/agnijs-js-resolve/tests/fixtures`, plus a test that drives a real host through `Engine::load_module`.
- `package.json` parsing keeps object key order, which condition matching depends on, without turning on `serde_json`'s `preserve_order` for the whole workspace.
