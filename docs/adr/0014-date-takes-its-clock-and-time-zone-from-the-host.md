# 14. Date takes its clock and time zone from the host; buffers and Atomics are single-agent plain memory

Date: 2026-11-08

## Status

Accepted

## Context

M5 adds `Date`, the buffer family (`ArrayBuffer`, `SharedArrayBuffer`, typed arrays, `DataView`) and `Atomics`. All three touch the platform in some way: `Date` needs the current time and the local time zone, `SharedArrayBuffer` and `Atomics` exist to share memory between agents, and buffers need a policy for allocation failure. The spec keeps the engine pure Rust with no platform-specific code (ADR 0006: the host supplies everything non-ECMAScript) and says an engine instance is single-threaded with its own heap; multi-agent support is a later milestone.

## Decision

- **The `Host` trait gains three default-implemented hooks**: `now_ms()` (the system clock, from `std::time::SystemTime`), `time_zone_offset_ms(epoch_ms, is_utc)` (defaulting to zero, so the engine's default local time zone is UTC) and `time_zone_name(epoch_ms)` (defaulting to `"Coordinated Universal Time"`). `Date` calls only these. A host that knows its platform time zone implements the two time-zone hooks; the engine never links a time-zone database. Tests pin the clock by implementing `now_ms`.
- **`Date` parsing is the Date Time String Format first, then a lenient fallback** that accepts the engine's own `toString` and `toUTCString` output, month and weekday names, `GMT`/`UTC`/`Z` with `±hhmm` offsets and parenthesised zone names. Anything else is `NaN`; there is no attempt to match V8's wider heuristics, which the spec leaves implementation-defined.
- **Buffers are `Vec<u8>` with a 4 GiB cap.** A request above the cap, or one the allocator refuses (`try_reserve_exact`), is a `RangeError` as the spec allows for allocation failure. Resizable buffers resize the vector in place; views recompute their length from the buffer on every access, so out-of-bounds and length-tracking behaviour fall out of one `ta_length` function.
- **`SharedArrayBuffer` is an `ArrayBuffer` flagged `shared`**: it cannot be detached, grows instead of resizing and shares no memory with another agent because there is none. `Atomics` operations are plain read-modify-write on the bytes. `Atomics.wait` and `Atomics.waitAsync` validate their arguments and return `"not-equal"` or `"timed-out"` without blocking (no other agent can `notify`); `Atomics.notify` returns 0. The multi-agent test262 tests (`atomicsHelper.js`, `Atomics/wait`, `Atomics/waitAsync`, `CanBlockIsFalse`) stay excluded with that reason, as the spec directs.

## Consequences

- A Gable Native host gets correct local times by implementing two small hooks; until it does, `Date` behaves as if the device were in UTC, which is deterministic and test-friendly.
- Sharing a buffer between threads later means replacing `Vec<u8>` behind `BufferData` with a shared backing store and giving `Atomics` real atomics; the built-ins' validation and conversion code does not change.
- test262 after M5: 42225 of 42225 in-scope tests pass. Six tests remain excluded with a tracked cause: a lone surrogate placed in eval'd source text with `String.fromCharCode` is lost because the parser lexes UTF-8 source (a parser follow-up, not a Date or RegExp issue).
