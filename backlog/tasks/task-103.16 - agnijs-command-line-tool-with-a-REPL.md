---
id: TASK-103.16
title: agnijs command line tool with a REPL
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-12T02:23:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 272000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A small agnijs binary with run, eval and repl subcommands, using the debugger and CDP adapter (ADR 0017) for --inspect, and printing uncaught errors with source-mapped stack traces. It makes the project easy to try and serves as a smoke-test tool. Optional flags select the regex matcher mode and the memory and fuel limits. Distribution: the npm organization agnijs-rs (maintainer created it) hosts the CLI as a scoped package such as @agnijs-rs/cli, with prebuilt binaries per platform. Publishing needs the maintainer credentials. Model: Sonnet.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 agnijs run file.js, eval and repl work on Windows, macOS and Linux; the REPL has history and multi-line input
- [ ] #2 --inspect connects Chrome DevTools through the CDP adapter, and the CLI is covered by tests in CI
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. New crate crates/agnijs-cli (binary agnijs) on the public embedding layer: agnijs::Context for evaluation, agnijs-js-resolve for Node-style module resolution, agnijs-js-cdp + make_debuggee for --inspect. No engine crate depends on it; it stays out of the no-std/size builds.
2. Small additive agnijs API: Context::from_engine and Context::from_snapshot so the CLI can restore snapshots and wrap a debuggable engine.
3. Subcommands: run (file or - for stdin; script/module by flag, extension, package.json type, or syntax detection), eval (-p prints), repl, compile (bytecode), snapshot. Bare agnijs FILE runs; bare agnijs is the REPL on a TTY, stdin otherwise.
4. Host: file loader with source maps (sourceMappingURL files and data: URLs -> set_source_map / SourceWithMap), bytecode files, JSON modules, import.meta.url, console to stdout/stderr, regex matcher, unhandled rejection tracking. Event loop with setTimeout/setInterval/clear*.
5. Errors: uncaught errors print their V8-format stack (source mapped); exit codes 0 ok, 1 uncaught error/rejection, 2 usage or I/O, 3 fuel/memory limit, 13 unsettled top-level await.
6. Flags: --fuel, --max-memory, --regex-matcher, --seed, --snapshot, --inspect[=addr], --inspect-brk[=addr].
7. REPL: rustyline with a history file, multi-line input via unexpected-end-of-input detection, top-level await by async wrapping, promise results awaited/printed, value inspector.
8. Integration tests spawning the binary; CI job building and testing the crate on ubuntu/windows/macos plus clippy.
9. Gates: test262 modes unchanged, clippy -D warnings, rustfmt, rebase, push, watch CI.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as crates/agnijs-cli (binary agnijs) on agnijs::Context + agnijs-js-resolve. Additive agnijs API: Context::from_engine, Context::from_snapshot.
- A JS prelude (src/prelude.js) adds timers, queueMicrotask and a Node-like inspect/console formatter. The engine console ToStrings its arguments, so the CLI wraps console.* in JS. Under --inspect console is left raw so DevTools gets the values.
- --inspect wraps a make_debuggee engine in an Engine adapter (src/inspect.rs) that serves CDP messages at each entry point. --inspect-brk requests the pause at the first non-agnijs: evaluation, so it stops in the program, not in the prelude.
- Syntax errors: the engine drops the span from host-level compile errors, so the CLI re-parses the main file with agnijs-js-parser to print a frame (engine follow-up).
- Module bytecode with imports panics in Engine::eval_bytecode (agnijs-js-vm modules.rs:558, "unloaded module request"). The CLI loads module bytecode through load_module + ModuleSource::Bytecode instead (engine follow-up).
- License allowlist gains BSL-1.0 (Boost; clipboard-win and error-code under rustyline on Windows).
- Tests: 22 integration tests spawn the binary, including a CDP client on --inspect-brk (pause, breakpoint, evaluateOnCallFrame, consoleAPICalled), plus 2 unit tests. All pass on Windows locally. CI job cli runs them and clippy on ubuntu/windows/macos.
- Pre-existing, not mine: clippy bool_assert_comparison in crates/agnijs/tests/embed.rs:207.
- Not done: npm distribution (@agnijs-rs/cli) needs maintainer credentials. Interactive TTY REPL checked through the pipe path only; rustyline handles the TTY.
<!-- SECTION:NOTES:END -->
