# 21. The C API is an id-based, versioned ABI over the engine interface that never panics or aborts its host

Date: 2026-11-10

## Status

Accepted

## Context

TASK-103.3 opens the engine to embedders that are not written in Rust: C, C++, and every language with a C foreign-function interface. The engine interface (ADR 0006) is already handle-based and host-driven, but it is a Rust trait: it hands out `&mut dyn Engine`, takes closures and `Rc`s, and reports failure through Rust enums. None of that can cross a C boundary as it is.

The maintainer's hard rule is that the C API must never panic or abort the host process. A C host cannot recover from a Rust panic unwinding into its frames (that is undefined behaviour) or from `abort`. It also passes in whatever its bugs produce:

- null, freed, stale or foreign handles
- double frees and use after the engine is gone
- calls from the wrong thread
- invalid UTF-8
- lengths that overflow
- callbacks that fail or call back in at awkward moments

Deep nesting in untrusted source must not overflow the host's stack either, though the engine had no native stack guard: its Rust API simply recursed.

## Decision

**A separate crate, `agnijs-capi`, built as `cdylib` and `staticlib`, wraps `agnijs-engine-api` and nothing else.** The engine crates stay `#![forbid(unsafe_code)]`. All `unsafe` lives in the FFI crate, confined to three places, each with a stated invariant:

- reading and writing caller memory
- calling host function pointers
- the engine lease

**The header is generated and checked in.** cbindgen produces `include/agnijs.h` from the crate. CI regenerates it and fails on any difference.

**Everything the host holds is a 64-bit id, never a pointer.** An id packs a 16-bit kind tag, a 16-bit generation and a 32-bit slot index into a per-engine table. Engines, realms, values, buffers, classes and hook sinks all work this way.

- **Checked on every use.** Every id is checked for kind, generation, engine and liveness, so a misused id is a status code, not undefined behaviour.
- **Slots retire.** Slots retire instead of wrapping their generation, so a stale id is never mistaken for a new one.
- **Values are rooted references.** A value is a rooted reference the host frees explicitly. Freeing the engine frees everything it handed out.

**No allocation crosses the boundary.** Text is copied into caller memory with a size query. Bytes, such as bytecode images and exception text, stay in engine-owned buffers that the host frees by id.

**Every function returns a status, and nothing panics through.**

- **Status codes.** Zero is success, positive codes are JavaScript outcomes (exception, interrupted, out of memory), and negative codes are call errors with a thread-local message.
- **Panics.** Every exported function runs under `catch_unwind`. A caught panic returns `AGNIJS_ERR_PANIC` and poisons that engine: later calls return `AGNIJS_ERR_POISONED`, and freeing it stays safe.
- **Lints.** Clippy denies `unwrap`, `expect`, `panic!`, indexing, slicing, unchecked arithmetic and truncating casts in the crate.
- **Build profile.** The crate refuses to build with `panic=abort`.
- **Callbacks.** Callbacks are `extern "C-unwind"` and run under `catch_unwind`, and their results are validated.

**Engines are single-threaded, and reentrancy goes through a lease.**

- **Threads.** Engines live in a thread-local registry. A global owner map only turns a cross-thread call into `AGNIJS_ERR_WRONG_THREAD`, and it carries the interrupt flag, the one thing any thread may set.
- **Lending.** A call that runs JavaScript takes the engine out of its slot. When the engine hands `&mut dyn Engine` to a host callback, the lease lends it, and calls back in from that callback use the lent reference, one at a time.
- **Limits.** Nesting is capped by `max_reentrancy` (default 32, at most 256). Calls from hooks that have no engine get `AGNIJS_ERR_BUSY`, as does freeing an engine from its own callback.

**The engine gains an optional native stack budget.**

- **The mechanism.** It is a thread-local lowest stack address in `agnijs_js_regex::stack`, re-exported by the parser. It is checked at the recursive productions of:
  - the parser and the regex parser and validator
  - the compiler's analysis and code generation
  - native reentry, JSON, `Array.prototype.flat`, proxies and bound functions
- **The error.** When the budget runs out these fail with `SyntaxError` or `RangeError` "Maximum call stack size exceeded".
- **Who sets it.** Without a budget the checks never fire, so the Rust API, test262 and the JIT are unchanged. The C API installs one around every call: `stack_budget`, default 512 KiB, minimum 64 KiB, with 256 KiB of documented headroom. A nested call keeps the tighter limit.

**Versioned structs and an append-only ABI 1.x.**

- **Struct sizes.** Every struct starts with `struct_size`. Smaller sizes are refused. Larger sizes are accepted only when the unknown trailing bytes are zero, so a host built against a newer header works while it leaves the new fields at their defaults.
- **Version numbers.** Additions bump the minor version, and anything incompatible bumps the major. `agnijs_abi_version` reports both.

**Host functions are not constructors in ABI 1.0.** The engine interface has no constructor host functions, so `new_target` is always zero, and a JS wrapper gives a host class its constructor. Adding constructor support later is an append-only change.

**There is no fuel.** The engine counts no instructions. Interrupt hooks, polled while JavaScript runs, and the cross-thread interrupt request bound running time instead. (Superseded by the TASK-103.1 addendum below: ABI 1.1 adds fuel.)

## Considered options

- **Exposing Rust pointers as opaque handles** (`AgnijsEngine *`): the conventional C shape. But a freed or foreign pointer is then undefined behaviour inside the library, which the hard rule forbids. Ids cost a table lookup per call.
- **Reference-counted values that the engine frees when the count drops**, as QuickJS does: this needs every host to get duplication exactly right, and a double free is memory corruption rather than an error. Explicit, checked ids make the same mistakes reportable.
- **Allowing calls from any thread behind a mutex:** this would make every call pay for locking. It would also hide the engine's single-threaded design without making concurrent JavaScript possible.
- **Running each call on a library-owned thread with a large stack:** this would avoid the stack budget, but it adds a thread hop to every call and every callback, and it breaks thread-local state on the host side.
- **Hand-writing the header:** it drifts. Generated plus a drift check keeps the header and library one artifact.

## Consequences

- C and C++ hosts link one library and include one header. The checked-in examples (an event loop in C, a host class in C++) and the C test program run in CI on Linux, Windows and macOS, against both the static and the shared library.
- The remaining ways the process can end are outside the library: system allocation failure (Rust aborts), a calling thread with less free stack than budget plus headroom, and broken caller contracts (a pointer shorter than its length, a C++ exception thrown through a callback). They are documented in `docs/c-api.md`.
- The stack checks are now part of the engine. New recursive paths in the parser, compiler or native built-ins should check `stack::exhausted()`, and the C API's hostile suite sweeps nesting depths to catch a missed one.
- Bindings for other languages (Python, Go, C#) are built on this ABI, not on the Rust trait.

## Addendum (TASK-103.1, 2026-11-10): fuel, ABI 1.1

The engine now meters instructions (`Engine::set_fuel` and `Engine::fuel`), so the C API exposes it as a minor, additive version: `AGNIJS_ABI_VERSION_MINOR` is 1. `agnijs_set_fuel(e, n)` sets a budget of `n` bytecode instructions, `agnijs_clear_fuel` removes it and `agnijs_get_fuel` reads what is left (`AGNIJS_ERR_NOT_FOUND` with no budget). Exhaustion is a new positive status, `AGNIJS_OUT_OF_FUEL` (4): an outcome of running JavaScript like `AGNIJS_OUT_OF_MEMORY`, uncatchable by script, and returned by every later call until the budget is reset. A host function may return it to pass exhaustion on, as it may `AGNIJS_INTERRUPTED`. The budget belongs to the engine, not the realm: it spans realms, host callbacks and promise jobs, and a host meters a single call by setting a budget before it and reading `agnijs_get_fuel` after. Hosts built against 1.0 never see status 4 unless they call the new functions. Interrupt hooks stay the tool for wall-clock limits; fuel is the deterministic one.

ABI 1.1 also adds `agnijs_set_random_seed`, the C side of deterministic mode (`docs/determinism.md`). It is a function rather than an `AgnijsEngineOptions` field because the options struct cannot grow without breaking 1.0 callers: `versioned` refuses a struct smaller than the library's. It also lets a host reseed in the middle of a run.
