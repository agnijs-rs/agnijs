# 27. Temporal adopts temporal_rs behind the agnijs-js-temporal seam, with the IANA data Intl already links

Date: 2026-11-15

## Status

Accepted

## Context

Temporal reached Stage 4 after the ES2025 cut, and developers notice when it is missing (TASK-103.8). test262 at the pinned commit has 4,613 tests under `test/built-ins/Temporal`, 2,040 under `test/intl402/Temporal` and 81 more tagged `Temporal` elsewhere (`Date.prototype.toTemporalInstant`, `Intl.DateTimeFormat` with Temporal arguments). Temporal is large: nine types, about 300 methods, ISO 8601 and RFC 9557 parsing, every calendar ECMA-402 lists (Chinese, Hebrew, Islamic and the rest), IANA time zones with their transitions, and duration rounding whose results the spec pins to the nanosecond. The engine already has the inputs Temporal needs from outside: the host's clock and time zone (ADR 0014), ICU4X for calendars (ADR 0015), and the IANA database that `agnijs-js-intl` compiles in through `jiff`'s `jiff-tzdb` for `Intl.DateTimeFormat`.

Three ways to get there were considered.

**temporal_rs** (the Boa project, `MIT OR Apache-2.0`, 0.2.6 in August 2026). A Rust implementation of the Temporal specification written for engines: it is the Temporal library of V8, Boa and Kiesel, which all run test262 against it. Calendars come from ICU4X's `icu_calendar` (2.x, the version we link), time zones from a pluggable `TimeZoneProvider`. Without its default features it reads neither the system clock nor the system zone: the embedder passes both in, and it embeds no time zone data unless asked. Its `compiled_data` feature compiles in the ICU4X calendar data and reads the IANA database from `jiff-tzdb`. Its errors are a kind (range, type, syntax, assertion) and a static message. It needs Rust 1.86.

**Building on ICU4X ourselves.** `icu_calendar` does the calendar arithmetic and `jiff` or `jiff-tzdb` the time zone transitions; the engine would write the rest: the ISO and RFC 9557 parser, the epoch-nanosecond and duration arithmetic (normalized time durations, `RoundRelativeDuration`, `BalanceTimeDuration`), the calendar field resolution of ECMA-402 for every calendar, offset disambiguation, and every abstract operation the spec states in mathematical values. That is the part temporal_rs exists for, and the part where engines found their bugs; by the size of temporal_rs (23,000 lines) and Boa's VM glue, months of work before the suites pass.

**jiff.** A mature date-time library (Unlicense or MIT) whose design follows Temporal, already in our dependency graph for Intl. It is not a Temporal implementation: it supports the ISO calendar only, its rounding, parsing and error behaviour are its own rather than the spec's, and it has no Temporal property-bag or calendar field model. Using it means building the spec layer on top, as above, with a library whose semantics we would have to work around rather than call.

## Decision

**temporal_rs is the Temporal library, behind a seam crate, `agnijs-js-temporal`.** The rules follow the RegExp (ADR 0013) and Intl (ADR 0015) seams:

- **The seam exposes only its own types.** The VM sees `agnijs_js_temporal` types, errors (`Error { kind: Range | Type | Internal, message }`) and option enums, never a `temporal_rs` or ICU4X type. An upgrade, or a replacement with another library or our own code, touches one crate. temporal_rs's assertion errors, which mean a bug in the library, become `Internal` and the engine throws a plain `Error` for them; nothing panics.
- **The VM owns what JavaScript observes.** `builtins/temporal` reads property bags and options in specification order, performs the coercions (`ToIntegerWithTruncation`, `ToPrimitive` of strings), checks receivers and decides which error is thrown first, as Boa's and V8's glue layers do. temporal_rs is called with Rust values only.
- **No ambient authority.** temporal_rs is built without its default `sys` features, so it cannot read the clock or the zone. `Temporal.Now` takes the time from a new `Host::now_ns` hook, whose default is `now_ms` scaled (so a host that pins `now_ms` for `Date` pins Temporal too), and the zone from `Host::default_time_zone`, the hook `Intl` already uses. Both are listed in `docs/determinism.md`.
- **One IANA database.** Time zones use temporal_rs's compiled provider over `jiff-tzdb`, the crate `agnijs-js-intl` already links for `Intl.DateTimeFormat`. The two cannot disagree about an offset, the data is not linked twice, and an update of the database is one `cargo update`. ICU4X's calendar data is likewise the copy Intl links.
- **`temporal` is a cargo feature of `agnijs-js-vm`, off by default while the built-ins are completed** (TASK-103.8.x), so the default build's size and startup do not change until Temporal is whole. The test262 runner and the fuzz crate turn it on. The seam crate declares `rust-version = "1.86"`, temporal_rs's minimum, as the JIT crate declares its own; the workspace stays at 1.85 and builds without the feature need nothing newer. A snapshot records the feature in its registry fingerprint, as it does `intl`.
- **test262 measures Temporal separately until it is complete.** `test262 run --only-temporal` runs the Temporal directories and every test tagged `Temporal` or `canonical-tz` against `baselines/agnijs-temporal.json`, which lists the tests still expected to fail and shrinks with each subtask. The main baseline, and its zero-regression gate, are untouched until the last subtask folds the Temporal tests in and removes Temporal from `post-es2025-features.txt`.

## Consequences

- The engine gets the calendar and time zone semantics V8 ships, and shares their fixes: a test262 failure inside the arithmetic is a temporal_rs issue to report or patch upstream, not engine code to write.
- The dependency adds temporal_rs, `timezone_provider`, `ixdtf` (the RFC 9557 parser), `calendrical_calculations` and `tzif` with `combine`; all are `MIT OR Apache-2.0` or `Unicode-3.0`, and `scripts/check-licenses.mjs` passes. ICU4X and `jiff-tzdb` are shared with Intl.
- temporal_rs is pre-1.0. A minor version may change its API; the seam absorbs that, and the pinned version only moves in a commit that runs the Temporal suites.
- Builds without `intl` still get Temporal's non-ISO calendars, because the calendar data comes with `icu_calendar`, not with the Intl seam; `toLocaleString` then falls back to `toString` as the other built-ins do (ADR 0015 addendum).
- Until the feature is on by default, embedders who want Temporal enable `agnijs-js-vm/temporal` themselves. Turning it on by default is TASK-103.8.10, with the size and startup cost measured then.
