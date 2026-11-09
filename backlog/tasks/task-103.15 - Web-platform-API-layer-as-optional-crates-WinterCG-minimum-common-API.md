---
id: TASK-103.15
title: Web-platform API layer as optional crates (WinterCG minimum common API)
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 271000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Optional crates, outside the core engine, that provide the globals most scripts expect: console, URL and URLSearchParams, TextEncoder and TextDecoder, AbortController and AbortSignal, structuredClone, timers, queueMicrotask, EventTarget and Event, fetch (behind a host-supplied transport), Streams, Blob and File, and crypto.getRandomValues. The target list is the WinterCG minimum common API. Each piece is separable so embedders take only what they need, and the engine core gains no dependency on them. Reuse what Gable needs for its host globals (spec section 8) and decide the Blob and File question raised in the M9 report. Model: Opus for the design and the stream and fetch pieces, Sonnet for the self-contained ones (URL, encoding, console).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A short ADR scopes the layer, the crate split and how host hooks (network, time, randomness) are supplied
- [ ] #2 Each crate is tested against the matching Web Platform Tests subset or a documented fixture set, and the engine core gains no dependency on any of them
<!-- AC:END -->
