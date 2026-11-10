# Deterministic mode

AgniJS can run a program so that it gives the same bytes on every run, on every platform and in every build of one engine version: debug or release, with or without the JIT. Replays, lockstep simulations, test snapshots and metered sandboxes need this. Deterministic mode is not a switch. The engine has no hidden inputs except the ones listed below, so a host gets determinism by supplying each of those inputs as a fixed value (TASK-103.1).

## Turning it on

From Rust, implement these `Host` methods with fixed values:

| Input | `Host` method | Default |
| --- | --- | --- |
| `Math.random` | `random_seed` (read once, at the first draw; `Engine::set_random_seed` reseeds) | the clock |
| `Date.now()`, `new Date()` | `now_ms` | the system clock |
| The local time zone (`Date` local time, `toString`) | `time_zone_offset_ms`, `time_zone_name` | UTC |
| The `Intl` default locale and time zone | `default_locale`, `default_time_zone` | `en-US`, `UTC` |
| Time limits | use fuel (`Engine::set_fuel`), not `interrupt` | no limit |

From C, call `agnijs_set_random_seed` right after `agnijs_engine_new` and set the `now_ms` hook in `AgnijsEngineOptions`. The C API always uses UTC and `en-US`. Bound running time with `agnijs_set_fuel`, not the interrupt hook.

Everything else that reaches a program comes from the host itself: module sources, host functions, host classes, `console`, the order in which the host runs jobs and calls in. A host that wants determinism keeps those deterministic too.

## What is guaranteed

With those inputs fixed:

- **Results.** Values, strings, exceptions and their messages, property and collection order, and promise job order depend only on the program and the inputs. No engine behaviour a program can see depends on hash seeds, addresses, threads or timing. Property order, `Map` and `Set` order and `for-in` order are insertion order. The engine's hash maps are used only for lookups or are sorted before use. The parser and compiler emit the same bytecode for the same source every time.
- **Numbers.** Arithmetic, `Math.sqrt`, `fround`, `round`, `trunc`, `floor`, `ceil`, `abs`, `sign`, `clz32`, `imul`, `min`, `max`, conversion to and from strings, `toFixed`, `toPrecision`, `toExponential` and `BigInt` are exact IEEE 754 or exact by specification.
- **Bytes.** Typed arrays are little-endian on every platform, and `DataView` uses the byte order each call asks for. A NaN is stored as the one quiet NaN (`0x7ff8000000000000` for `Float64`, `0x7fc00000` for `Float32`, `0x7e00` for `Float16`), whichever arithmetic made it. x86 and ARM make different NaNs, and the specification leaves the pattern to the implementation.
- **`Math.random`.** The sequence depends only on the seed and on how many values have been drawn. Every realm of an engine draws from one stream. The generator (splitmix64 seeding into xorshift64\*) is part of the engine version.
- **Fuel.** Fuel costs one unit per bytecode instruction. The count depends only on the program and the engine version, never on the platform or the tier: a budget keeps execution in the interpreter. Running out happens at the same instruction every time.

The corpus in `crates/agnijs-js-vm/tests/determinism/` pins this. CI checks each program's output byte for byte against the checked-in `.txt` beside it. It does this on Linux, Windows and macOS, both with the interpreter and with every block compiled (`AGNIJS_JIT=force`). The same test checks that each program costs the same fuel on every run, and that metering does not change its output. To add a case, write a program that pushes lines onto a global `out`, then run `AGNIJS_BLESS=1 cargo test -p agnijs-js-vm --test determinism` to write its `.txt`.

## What remains nondeterministic

Each of these is outside the guarantee. Fix them in the host, or keep programs away from them:

- **Transcendental `Math` functions.** `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `exp`, `expm1`, `log`, `log1p`, `log2`, `log10`, `cbrt`, `hypot`, `**` and `Math.pow` call the platform's libm. When the exact result is representable (`2 ** 10`, `Math.cbrt(27)`) every libm gives it. Other results can differ in the last bit between operating systems and C libraries. They are stable on one platform. If you need these to match across platforms, provide your own implementations as host functions.
- **Garbage collection timing.** When a collection runs depends on the engine's allocation accounting. That accounting is stable for one engine version and pointer width, but it changes between versions and between 32-bit and 64-bit targets. It also changes when the host calls `collect_garbage` or when `AGNIJS_GC_STRESS` is set. Programs see collection timing only through `WeakRef.prototype.deref` and `FinalizationRegistry` callbacks, so deterministic programs avoid both.
- **The memory limit.** `OutOfMemory` comes from the same accounting. Where it strikes is stable on one engine version and pointer width.
- **Native stack depth.** The "Maximum call stack size exceeded" `RangeError` comes from two sources. One is a fixed limit of 20,000 frames. The other is the native stack budget: deeply nested source, deep native reentry and recursive built-ins check how much real stack is left, and that differs between platforms, build profiles and tiers. Deterministic programs do not recurse to the limit.
- **Interrupts.** `Host::interrupt` and `request_interrupt` usually depend on a wall clock or another thread, so where they stop a program is not reproducible. Use fuel for a reproducible limit.
- **Engine version.** Unicode data, `Intl` locale data (ICU4X, compiled in), bytecode and so fuel costs, GC accounting and the random generator can all change between engine versions. The corpus changes only in a commit that says so.
- **Host defaults.** The defaults for `random_seed` and `now_ms` read the clock. A host that does not override them is not deterministic.
