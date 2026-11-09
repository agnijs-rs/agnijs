---
id: TASK-103.16
title: agnijs command line tool with a REPL
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
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
