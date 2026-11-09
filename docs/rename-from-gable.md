# Rename from the Gable working names

The engine was built inside the Gable repository as `native/engine` under working names with the prefix `gable`. TASK-100.12 split it out with its full history (`git subtree split`) and renamed it in one scripted pass. Use this table to read old commits, issues and ADR history.

| Kind | Before | After |
| --- | --- | --- |
| Crates | `gable-engine-api`, `gable-engine-quickjs` | `agnijs-engine-api`, `agnijs-engine-quickjs` |
| Crates | `gable-js-parser`, `-compiler`, `-vm`, `-regex`, `-intl`, `-debugger`, `-cdp`, `-test262`, `-bench` | `agnijs-js-parser`, `-compiler`, `-vm`, `-regex`, `-intl`, `-debugger`, `-cdp`, `-test262`, `-bench` |
| Rust paths | `gable_engine_api::`, `gable_js_vm::` and so on | `agnijs_engine_api::`, `agnijs_js_vm::` and so on |
| Directories | `native/engine/crates/gable-*` | `crates/agnijs-*` |
| Engine id (`EngineInfo::name`, `--engine`) | `gable` | `agnijs` |
| Benchmark engine ids | `gable`, `gable-backtracking`, `gable-linear` | `agnijs`, `agnijs-backtracking`, `agnijs-linear` |
| Bytecode container magic | `GBJS` | `AGJS` |
| Bytecode format tag | `gable-bc-N` | `agnijs-bc-N` |
| Environment variables | `GABLE_GC_STRESS`, `GABLE_REGEX_FUZZ_CASES`, `_SEED`, `_TRACE` | `AGNIJS_GC_STRESS`, `AGNIJS_REGEX_FUZZ_CASES`, `_SEED`, `_TRACE` |
| test262 baselines | `baselines/gable.json`, `gable-intl402.json` | `baselines/agnijs.json`, `agnijs-intl402.json` |
| test262 exclusions | `exclusions/gable.txt` | `exclusions/agnijs.txt` |
| CDP identity | `Browser: gable`, `gable instance` | `Browser: agnijs`, `agnijs instance` |
| Prose | "the Gable engine" | "the AgniJS engine" |
| Repository URL | `github.com/jcannon98188/gable` | `github.com/agnijs-rs/agnijs` |

The pre-container M2 bytecode (`GBLC` magic, `gable-bc-1` tag) was not renamed: the rename made its loader accept only `agnijs-bc-1`, a tag no build ever wrote, so the loader was removed and such bytes are refused as a format mismatch (ADR 0010). Unchanged on purpose: Gable-side names (`gable-engine-harness`, `gable-harness`, `@@gable-test` protocol lines) which belong to the Gable repository; and the Gable vocabulary that `agnijs-js-test262/tests/boundary.rs` forbids in engine sources.

ADR numbers are shared with the Gable repository and kept. ADRs 0006 to 0017 moved here; 0001 to 0004 and 0018 (Gable adopts AgniJS) stay there.
