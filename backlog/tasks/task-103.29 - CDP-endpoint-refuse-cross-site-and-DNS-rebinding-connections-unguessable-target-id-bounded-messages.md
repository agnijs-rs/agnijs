---
id: TASK-103.29
title: >-
  CDP endpoint: refuse cross-site and DNS-rebinding connections, unguessable
  target id, bounded messages
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
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
