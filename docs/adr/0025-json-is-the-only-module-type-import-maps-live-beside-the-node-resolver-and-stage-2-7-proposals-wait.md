# 25. JSON is the engine's only module type, import maps live beside the Node resolver, and decorators and ShadowRealm wait at Stage 2.7

Date: 2026-11-11

## Status

Accepted

## Context

TASK-103.20 groups five module and language features beyond the ES2026 ticket (TASK-103.7): import attributes, JSON modules, import maps, decorators and ShadowRealm. For each it asks whether it is in scope now or waits on its proposal, given what test262 covers.

At the test262 pin (`e4fbf57`, `crates/agnijs-js-test262/PIN`):

| Feature | Where in test262's `features.txt` | Tests | TC39 / standard status (2026-11) |
|---|---|---|---|
| `import-attributes` | Standard | 101 files | ES2025 |
| `json-modules` | Standard | 14 files | ES2025 |
| import maps | — | none (a host feature) | WHATWG HTML; WPT has 22 data-driven files |
| `decorators` | Proposed | 31 files: 24 syntax-only, 7 in `staging/` | Stage 2.7, demoted from Stage 3 in May 2026 ("test262 is insufficient", no engine shipped it, no active champion); decorator metadata demoted with it |
| `ShadowRealm` | Proposed | 64 files, all in `built-ins/ShadowRealm` | Stage 2.7 (demoted to Stage 2 in September 2023, back to 2.7 since; last update February 2025, open web-integration questions) |

TASK-103.7 did not cover any of them: its list was the ES2026 built-ins and explicit resource management. Import attributes and JSON modules were already implemented in the loader before this ticket and pass every gated test (the agnijs baseline has no expected failures). QuickJS-NG's one failure (`json-invalid.js`) is already in its baseline.

## Decision

**Import attributes and JSON modules: in scope, done, and hardened.** The loader already accepts only the `type` key (any other key is a `SyntaxError`) and `ModuleSource::Json` builds a synthetic module whose default export is the parsed value. Two holes are closed:

- **`json` is the only module type the engine knows.** A `type` other than `json` (`css`, `text`, `bytes`, the empty string) is a `TypeError` before the host is asked to resolve, rather than being loaded as JavaScript. An importer that asks for a non-code module must never get code run in its place. A host that wants another type is a future engine change (a new `ModuleSource` variant), not a host-side workaround. The Stage 3 `import-text` and `import-bytes` proposals would add `text` and `bytes` that way.
- **A JSON request always gets a JSON module.** The module map is keyed by `(realm, resolved specifier)`, as before, but a `type: "json"` request that finds a JavaScript record there, or whose host answers with `Source` or `Bytecode`, is a `TypeError`. The reverse is host policy: a plain import the host answers with `Json` still loads as JSON, as it always has.

**Import maps: in scope, as `agnijs_js_resolve::ImportMap`.** The WHATWG algorithm ("parse an import map string", the normalization steps, "resolve a module specifier", "resolve an imports match") is pure logic over URLs, like the Node algorithm of ADR 0023, so it goes in the same optional crate as a second, independent resolver. The engine still knows nothing of it, and no engine crate depends on the crate. An embedder parses the map once with a base URL and calls `ImportMap::resolve(specifier, referrer_url)` from `Host::resolve_module`, using the referrer's resolved URL, or the document or entry URL for a root load.

- URLs are parsed by the `url` crate (WHATWG URL Standard). It is a new dependency of `agnijs-js-resolve` only. It reuses the ICU4X normalizer and properties crates the workspace already has (through `idna`), so it adds five small crates and none of them reach an engine build.
- The public API takes and returns strings, so `url` types do not leak into it.
- Object keys are taken in the order `JSON.parse` and property enumeration give them, which "parse a JSON string to an Infra value" goes through: a repeated key keeps its first place and its last value, and array-index keys come first. This uses the crate's key-ordered JSON reader.
- Maps sort in descending UTF-16 code-unit order, so exact keys and longer prefixes are tried first.
- Parse warnings (ignored keys, entries with no valid address) are kept on the map for the host to report. Errors are an `ImportMapError` the host turns into its rejection message.
- Integrity metadata is parsed and exposed (`ImportMap::integrity`); checking it is the fetching host's job.
- **Not included:** merging several import maps (a browser concern: a document can hold several `<script type="importmap">` elements, with the "resolved module set" bookkeeping that goes with it) and anything to do with fetching.
- **Coverage:** the WPT data-driven import map tests are the oracle, run by a unit test when `AGNIJS_WPT_IMPORT_MAPS` names a WPT `import-maps/data-driven/resources` directory (284 checks across 22 files, all passing at WPT `48d5a8e`). They are not vendored: they are a check to run when the code changes, not part of the default suite.

**Decorators: wait.** The proposal is back at Stage 2.7, and its semantics have changed several times. test262 holds only syntax tests, which are no oracle for the evaluation order, the `addInitializer` timing and the auto-accessor storage the feature is mostly about. Implementing it now would mean the parser, the compiler and new bytecode, against a target that may still move. Revisit when it returns to Stage 3 with semantic tests.

**ShadowRealm: wait.** The JavaScript side is mostly settled, and the engine has what it needs (several realms per engine, `Engine::create_realm`). But the proposal is at Stage 2.7 with its host and web-integration questions open, and an embedder that wants isolation already has realms through the engine interface (`Engine::create_realm`) and the C API (`agnijs_realm_new`). Revisit at Stage 3: the 64 `built-ins/ShadowRealm` tests would then gate it.

Both stay in the Proposed section of `features.txt`, which the runner skips. They need no entry in `post-es2025-features.txt`.

## Consequences

- A host that loaded `with { type: "css" }` (or any non-JSON type) as JavaScript now gets a `TypeError`. No test262 test in the gate depends on the old behaviour; the `import-text` and `import-bytes` tests are proposed and skipped.
- The test262 numbers are unchanged: every import-attributes and json-modules test already passed, and the Stage 2.7 features stay skipped.
- `agnijs-js-resolve` now holds two resolvers. They share nothing but the JSON reader. An embedder uses either one, or tries the import map first and falls back to Node resolution on `ImportMapError::Unmapped`.
- When the test262 pin moves, check the Proposed section again: if decorators or ShadowRealm reach Stage 3 with semantic tests, this decision is reopened.
