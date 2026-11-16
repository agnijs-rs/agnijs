# Changelog

What changed in each workspace version for embedders of the public crates, `agnijs` and `agnijs-engine-api`. The versioning rules are in [docs/stability.md](docs/stability.md). Nothing is published to crates.io yet; a version names the commit that set it, which is what a git-pinned embedder moves to.

## 0.3.0 (2026-11-16)

Breaking. The embedding surface changes once now so that later additions to it are not breaking ([ADR 0026's addendum](docs/adr/0026-the-agnijs-crate-is-the-public-embedding-surface-over-the-engine-interface.md)).

- **The hook enums are `#[non_exhaustive]`:** `ModuleSource`, `ConsoleLevel`, `RejectionEvent`, `RegexMatcher` and `ErrorKind`. A new variant is no longer a breaking change. To update: give each `match` on them a wildcard arm. Each enum's rustdoc says what to do with a variant you do not know (for example, log an unknown `ConsoleLevel` as `Log`, and ignore an unknown `RejectionEvent`).
- **`Engine` is sealed.** It has a hidden supertrait, so it cannot be implemented outside this workspace (which was never supported), and a new `Engine` method may come in a minor version without a default body. Calling it is unchanged: `&mut dyn Engine` and `Box<dyn Engine>` work as before.
- **`RealmId` carries its engine.** It was a public tuple struct `RealmId(pub u32)`; its fields are now private, and `RealmId::engine()` returns the `EngineId` that issued it. A realm id passed to another engine is `Error::UnknownRealm`, where before it named that engine's realm with the same index. To update: get realm ids only from `main_realm()` and `create_realm()`; code that built one with `RealmId(n)` or read `.0` has no replacement, because an index alone no longer names a realm. Hash-map keys and comparisons work as before.
- **MSRV 1.88**, from a declared 1.85 that the dependencies had outgrown (ICU4X 2.x needs 1.88). A new CI job builds the default features with it. The `agnijs` command needs 1.95, the opt-in JIT 1.97.

The C ABI is unchanged (still 1.2): its integer realm ids and constants are the same.

Policy decided in this version ([docs/stability.md](docs/stability.md)): bytecode stays a same-build cache in every release, and snapshots get no promise across releases before 1.0; from the first release the CLI promises its exit codes and core flags only; a CI comparison of the C ABI against the last release comes with the first release. Security reports go through GitHub private vulnerability reporting only ([SECURITY.md](SECURITY.md)).

## 0.2.0 (2026-11-15)

Breaking.

- `api::Error` and `agnijs::Error` are `#[non_exhaustive]`.
- Implementor-only items (`HandleRegistry`, `next_engine_id`, the `bytecode` container module, the conformance kit) are `#[doc(hidden)]` and outside the semver policy.
- The semver policy and its CI check ([docs/stability.md](docs/stability.md#the-rust-api)) start here.

## 0.1.0

The engine as extracted from Gable (TASK-100.12), before any API promise.
