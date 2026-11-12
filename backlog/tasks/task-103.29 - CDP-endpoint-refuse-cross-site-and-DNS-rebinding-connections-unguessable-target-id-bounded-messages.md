---
id: TASK-103.29
title: >-
  CDP endpoint: refuse cross-site and DNS-rebinding connections, unguessable
  target id, bounded messages
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T14:52:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: high
ordinal: 293000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). `CdpServer` (`agnijs-js-cdp/src/server.rs`) serves `/json/list`, `/json/version` and the WebSocket upgrade without looking at the Host or Origin headers, and the target id is `agnijs-<port>` (predictable). Browsers let any web page open a WebSocket to `ws://127.0.0.1:9229/agnijs-9229`, and DNS rebinding lets a page read `/json/list` as well, so a page the developer visits while `agnijs --inspect` or an embedder's debug endpoint is up can attach and call `Runtime.evaluate`: arbitrary JavaScript with every capability the host granted (the CLI's module loader reads files). Node fixed the same class of bug (CVE-2018-7160) with a Host check and random UUID ids. `CdpServer::listen` also binds whatever address it is given with no warning for non-loopback addresses (the CLI accepts `--inspect=0.0.0.0:9229`), and fragmented WebSocket messages are reassembled without a total limit (each frame is capped at 64 MiB in `ws.rs`, the message is not).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The HTTP discovery routes and the WebSocket upgrade refuse a Host header that is not an IP literal or localhost, and refuse a browser Origin unless the embedder allows it
- [ ] #2 The target id in the WebSocket path is random per server and not derivable from the port
- [ ] #3 Reassembled WebSocket messages have a total size limit, and exceeding it closes the connection
- [ ] #4 Listening on a non-loopback address requires an explicit opt-in in the API, and the CLI warns when given one
- [ ] #5 Tests cover a wrong Host, a cross-site Origin, a guessed id and an oversized fragmented message
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests in crates/agnijs-js-cdp/tests/endpoint_security.rs: wrong Host (all routes + upgrade), cross-site Origin, embedder-allowed Origin, random id + guessed id 404, oversized fragmented and single-frame message (close 1009), non-loopback listen refused.
2. server.rs: CdpOptions (allow_remote, allow_origin, max_message_size); listen() = loopback-only defaults, listen_with() opts in; Host must be IP literal or localhost; Origin refused unless devtools:// or allowed; target id = random v4 UUID (SipHash over std RandomState keys); per-message limit enforced by read_frame_limited before allocation; request head capped at 64 KiB.
3. ws.rs: read_frame_limited; read_message bounds the reassembled total.
4. CLI inspect.rs: resolve address, warn and opt in for non-loopback.
5. docs/security-model.md: describe the protections instead of the gaps.
6. Gates: cdp + cli tests, clippy -D warnings (touched crates, --no-deps), rustfmt own hunks, workspace check, default test262.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: the new tests need CdpOptions/listen_with, so they did not compile against the old server; the old server had none of the checks. Green: agnijs-js-cdp 4 unit + 3 cdp_client + 8 endpoint_security, agnijs-cli 24 integration + 3 unit (incl. only_non_loopback_addresses_are_remote) pass. clippy -D warnings clean on agnijs-js-cdp and agnijs-cli (--no-deps; dependency crates have warnings that were already there under the current clippy). rustfmt: new test file and inspect.rs fully formatted; server.rs/ws.rs only my hunks (remaining diffs are lines that were already there). cargo check --workspace --all-targets clean. test262 default: 43192 passed, 0 regressions. No engine code touched, so no precompile/snapshot/intl402/QuickJS runs. Randomness: no new dependency; the id is SipHash over std RandomState's OS-seeded keys (two hashes, 122 random bits). Also capped the HTTP request head at 64 KiB (small, related). The CLI warning is unit-tested via remote_addresses() rather than by binding 0.0.0.0 in a test (avoids firewall prompts).
<!-- SECTION:NOTES:END -->
