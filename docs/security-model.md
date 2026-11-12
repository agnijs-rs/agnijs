# The AgniJS security model

This page says what an embedder can rely on when AgniJS runs code it does not trust, what it cannot rely on, and what it must do itself. Each claim names the code or the test behind it. Where the engine falls short of what a sandbox needs, the gap is stated next to the claim with the task that tracks it. Read those gaps before you rely on a limit.

AgniJS is pre-release. This page describes `main` as of TASK-103.27 (November 2026).

## Threat model

**The attacker** supplies JavaScript source: a script, a module, or a string passed to `eval`. The attacker may also choose the data that script works on, such as regex patterns, JSON text and deeply nested literals. The attacker wants one of three things:

- to reach host resources the embedder did not grant;
- to corrupt memory or crash the host process;
- to keep the host's thread, memory or CPU from doing other work (denial of service).

**Trusted** are the embedder and everything it supplies:

- the `Host` implementation, host functions and host classes;
- bytecode and snapshot bytes;
- source maps it registers;
- the process the engine runs in, and the machine.

**Out of scope:**

- an attacker who can write bytecode or snapshot bytes the host loads (see [Bytecode and snapshots](#bytecode-and-snapshots-are-trusted-input));
- an attacker who can reach the debugging endpoint (see [The debugger and CDP](#the-debugger-and-the-cdp-endpoint-are-full-control));
- an attacker with code running in the same process or on the same core (see [Side channels](#side-channels)).

## Guarantees

### No ambient authority

A fresh realm has the ECMAScript globals, `Intl` (unless built without the `intl` feature) and `console`. Nothing else:

- no file system;
- no network;
- no timers, `setTimeout` or `queueMicrotask`;
- no `require`, `process`, DOM or `performance`.

The engine has no event loop, clock, module file system or time zone of its own. Everything it needs from outside goes through the `Host` trait (`crates/agnijs-engine-api/src/host.rs`), and every method there has a safe default. Each one below is a capability the host grants by overriding it:

| What script can reach | Where it comes from | Default |
| --- | --- | --- |
| `console.*` output | `Host::console` | discarded |
| `Date.now()`, `new Date()` | `Host::now_ms` | the system clock, whole milliseconds |
| `Math.random` | `Host::random_seed` | the clock mixed with a per-process counter |
| Local time zone, default locale and zone | `Host::time_zone_offset_ms`, `time_zone_name`, `default_locale`, `default_time_zone` | UTC, `en-US` |
| `import`, `import()` | `Host::resolve_module`, `Host::load_module` | every load fails: `module not found` |
| `import.meta` contents | `Host::import_meta` | empty |
| `eval`, `new Function` and the other function constructors | `Host::can_compile_strings` | allowed |
| Functions and objects with native behaviour | `Engine::new_function`, `register_host_class`, `new_host_object` | none |
| Other realms' objects | values the host passes between realms | none |

The `console` object routes every call to the host as already formatted strings (`builtins/global.rs`). `Atomics.wait` never blocks: the engine runs single-threaded and the call returns `"timed-out"` or `"not-equal"` at once (`do_wait` in `builtins/atomics.rs`). The VM crate starts no threads and does not read the clock or the environment while JavaScript runs. `Heap::new` reads the `AGNIJS_GC_STRESS` debugging variable once, when an engine is created.

`Host::can_compile_strings` is checked by direct eval, indirect eval and `CreateDynamicFunction`, which covers `Function`, `AsyncFunction`, `GeneratorFunction` and `AsyncGeneratorFunction` (`interp/globals.rs`, `builtins/function.rs`). It is tested by the conformance kit (`compile_strings_can_be_disabled` in `crates/agnijs-engine-api/src/conformance.rs`) and in the C API, where it is `AGNIJS_FLAG_NO_STRING_COMPILATION` (`string_compilation_can_be_refused` in `crates/agnijs-capi/tests/basics.rs`).

The CLI (`agnijs`) is a host like any other. Its prelude adds timers and `queueMicrotask` (`crates/agnijs-cli/src/prelude.js`), and its module loader reads files. Do not treat it as a sandbox.

### Memory safety of the engine

The engine crates forbid `unsafe` code at their crate roots, so the compiler rejects any:

- `agnijs-engine-api`
- `agnijs-js-parser`
- `agnijs-js-compiler`
- `agnijs-js-vm`
- `agnijs-js-regex`
- `agnijs-js-intl`
- `agnijs-js-debugger`
- `agnijs-js-cdp`

The heap is index arenas with no raw pointers (ADR 0009; `gc.rs` and `heap.rs` say so in their module docs). Two crates do contain `unsafe`:

- **`agnijs-js-jit`.** It sets `#![deny(unsafe_code)]` and allows it only in `exec.rs`: executable memory, the entry trampoline and the frame handle.
- **`agnijs-capi`.** It is the FFI boundary.

[Unsafe code in the FFI and JIT crates](#unsafe-code-in-the-ffi-and-jit-crates) says what that means for you. CI also runs the collector tests under Miri (`miri` job in `.github/workflows/ci.yml`).

### The C API does not panic into the host

Every exported function runs under `catch_unwind` (`ffi` in `crates/agnijs-capi/src/guard.rs`). A caught panic returns `AGNIJS_ERR_PANIC` and poisons that engine. The crate:

- refuses to build with `panic=abort` (`compile_error!` in `crates/agnijs-capi/src/lib.rs`);
- denies `unwrap`, `expect`, indexing and slicing through Clippy.

Ids are checked on every use, so a stale, foreign or forged id is refused and never dereferenced. Tests:

- hostile inputs in `tests/hostile.rs`, for example `garbage_and_stale_ids_are_refused` and `misbehaving_host_functions_become_errors`;
- panic injection in `tests/fault_injection.rs`;
- bounded fuzzing in `tests/fuzz.rs`.

ADR 0021 and [c-api.md](c-api.md) have the details.

### Bytecode and snapshots from another build are refused

Bytecode carries a format tag (`agnijs-bc-N`), and the payload has a checksum. Snapshots add a native-registry fingerprint. A container from another engine or build is refused with `BytecodeMismatch`, and damage is refused with `BytecodeCorrupt`. The loader also checks every operand that indexes a block's own pools (ADR 0010 and ADR 0022). Tests:

- `another_engines_bytecode_is_a_version_mismatch` and `a_corrupt_payload_is_refused_without_running` in `crates/agnijs-js-vm/tests/bytecode.rs`;
- `corrupted_bytes_never_panic` and `operands_outside_their_pools_are_rejected_by_verification` in `crates/agnijs-js-compiler/tests/serialize.rs`;
- `fuzzed_snapshots_load_or_fail_without_panicking` in `crates/agnijs-js-vm/tests/snapshot.rs`.

This guards against accidents, not attackers: see [Bytecode and snapshots are trusted input](#bytecode-and-snapshots-are-trusted-input).

## Limits

Only the interpreter's frame caps, the native stack budget and the regex routing are on by default. A host that runs untrusted code sets the others.

### Memory limit

**What it is.** `Engine::set_memory_limit(Some(bytes))`, or `memory_limit` and `agnijs_set_memory_limit` in the C API. When the counted heap passes the limit, the running call fails with `Error::OutOfMemory` (`AGNIJS_OUT_OF_MEMORY`), and the engine stays usable.

**How it is enforced.** Allocations add their size to a counter (`Heap::charge` and `note_growth` in `heap.rs`). The counter covers:

- object headers, slots, elements and dictionaries (`ObjectData::extra_bytes` in `object.rs`);
- strings, including flattened ropes;
- environments, symbols and BigInts.

The collection threshold is capped at the distance to the limit (`rearm_threshold`), so a collection runs before the limit is declared passed. The check itself runs at the dispatch loop's safe points: `attention_slow` in `interp/mod.rs`, which also applies under native frames. Tests:

- `memory_limits_are_enforced` (conformance kit);
- `memory_limit_faults_instead_of_growing` and `a_memory_limit_stops_a_loop_that_only_grows_arrays` (`tests/gc.rs`);
- `a_memory_limit_stops_a_huge_string_and_a_runaway_script` and `the_memory_limit_applies_inside_jobs_and_can_be_raised_again` (`tests/host_driven.rs`);
- `the_memory_limit_still_applies_under_a_native_frame` (`tests/gc_native_frames.rs`);
- `the_memory_limit_stops_runaway_allocation` (C API);
- `the_memory_limit_stops_a_growing_heap` (CLI).

**What it does not cover.**

- **Storage nothing charges.** ArrayBuffer and SharedArrayBuffer backing stores are not counted, and neither are the entries of Map, Set, WeakMap and WeakSet. `memory_usage` leaves them out too. A probe with a 16 MiB limit allocated 256 MiB of ArrayBuffers and a Map of 2,000,000 entries without an error. TASK-103.25.
- **Overshoot inside one call.** The check runs between instructions, after a built-in has allocated. One call can allocate up to 2 GiB for a string (`repeat`, `padEnd`, the 2^30-unit cap in `builtins/string.rs`) or 4 GiB for an ArrayBuffer (`MAX_BUFFER_BYTES` in `builtins/arraybuffer.rs`) before the limit is noticed. `Array.prototype.join` has no cap at all. TASK-103.25.
- **String length.** Ropes cost 16 bytes whatever their length, and `Heap::concat` stores the length as `u32` with no check. Doubling a string past 2^32 code units gives it `length` 0 (probe: `let s='x'.repeat(1<<20); for(let i=0;i<12;i++) s=s+s; s.length` is 0). TASK-103.26.
- **The process allocator.** Rust's allocator aborts the process when the operating system refuses memory. The limit is meant to be reached well before that, but the gaps above can get there first.
- **Engines and realms.** The limit is per engine, and realms in one engine share it.

### Fuel (instruction budget)

**What it is.** `Engine::set_fuel(Some(n))`, or `agnijs_set_fuel`. JavaScript may dispatch `n` more bytecode instructions, counted across realms, promise jobs and callbacks from native code. When none are left, the call fails with `Error::OutOfFuel`. JavaScript cannot catch that, and no `finally` runs. Every later call fails too until the host sets a new budget. The count is deterministic.

**How it is enforced.** The fuel flag is part of the attention word, and the dispatch loop's slow path charges one unit per instruction (`attention_slow_interp` in `interp/mod.rs`, `charge_fuel` in `vm.rs`). While a budget is set, compiled code is never entered, so the JIT cannot skip the count. Tests in `crates/agnijs-js-vm/tests/fuel.rs`:

- `an_endless_loop_runs_out_of_fuel`
- `javascript_cannot_catch_running_out_of_fuel`
- `a_spent_budget_fails_every_call_until_the_host_sets_a_new_one`
- `the_budget_spans_callbacks_from_natives_and_promise_jobs`
- `the_jit_does_not_change_what_a_program_costs`

**What it does not cover.** A call to a native built-in costs one instruction, whatever the built-in does. `Array(1e7).join()`, a sort, or an exponential regex match costs a handful of units. Fuel bounds bytecode, not time. TASK-103.28 covers the work inside built-ins.

### Wall-clock interrupt

**What it is.** `Host::interrupt` returns true to stop the running call with `Error::Interrupted`. In the C API this is the `interrupt` hook, or `agnijs_engine_request_interrupt` from any thread. The engine has no timer of its own. The host keeps a deadline, and the hook compares against it. An interrupt skips `finally` blocks and unwinds through host functions. The engine stays usable.

**How it is enforced.** `Vm::poll_interrupt` (`vm.rs`) runs at the interpreter's function entries, loop back-edges and call returns. The JIT polls at the same points (`interp/jit.rs`), so recursion without loops is stopped as well (TASK-100.33). The host hook itself is called once every 4096 polls. Tests:

- `a_recursive_function_with_no_loops_is_interrupted_by_the_host` and `a_chain_of_calls_returning_without_loops_is_interrupted_by_the_host` (`tests/dispatch.rs`);
- `an_interrupt_stops_a_runaway_job_and_the_engine_carries_on` and `an_interrupt_unwinds_through_host_functions_and_skips_finally_blocks` (`tests/host_driven.rs`);
- `an_interrupt_stops_a_compiled_loop` (`tests/jit_differential.rs`);
- `the_interrupt_hook_stops_an_infinite_loop` and `another_thread_can_request_an_interrupt` (C API).

**What it does not cover.**

- **Long native calls.** The regex matchers and native built-ins never poll. Patterns the linear-time matcher cannot run (backreferences, lookaround), and every pattern under `RegexMatcher::BacktrackingOnly`, backtrack with no step budget. A probe ran `/(?=(a+)+b)/.test('a'.repeat(23))` for 3.9 s in a debug build with an interrupt hook that always said stop, and the hook was never called; each extra character doubles the time. `Array(3000000).join(',')` behaves the same way. Until TASK-103.28, a script that reaches one of these can hold the thread for as long as the call takes. The doc comment on `RegexMatcher::BacktrackingOnly`, which calls the interrupt "the only way out", is wrong today.
- **Host code.** Time spent in host functions and `Host` hooks is the host's to bound.
- **Cadence.** The interrupt is a poll, not preemption. A call returns only at the next poll that reaches the host.

### Native stack depth

**What it is.** Two layers:

- **Frame caps.** The interpreter caps JavaScript frames at 20,000 (`MAX_FRAMES`) and nested native reentry at 1,200 (`MAX_NATIVE_DEPTH`, `vm.rs`). Past either cap, a catchable `RangeError: Maximum call stack size exceeded` is thrown (`push_frame` in `interp/mod.rs`, `Vm::in_native`).
- **Stack budget.** The parser, the compiler, the regex parser and recursive built-ins (JSON, `flat`, proxies, bound functions) stop at a native stack budget (`agnijs_js_parser::stack`, from `crates/agnijs-js-regex/src/stack.rs`, checked by `Vm::check_stack`).

**How it is enforced.** Every engine installs the budget around each call from the host that compiles or runs code: `Vm::enter`, `eval_script`, `compile_script`, `compile_module` and `eval_bytecode` in `crates/agnijs-js-vm/src/engine.rs`. That covers `agnijs::Context::new` and `with_host`, `agnijs_js_vm::make_engine`, `Vm::new`, snapshot restores, debuggees and the CLI. The default is 512 KiB (`agnijs_js_parser::stack::DEFAULT_BUDGET`). The engine may use up to 256 KiB more before it notices (`HEADROOM`), so a call needs 768 KiB of free stack: a 2 MiB thread (the Rust default) and a 1 MiB one (the Windows main thread) both have it. A host function that calls back into the engine keeps the outer, tighter limit, so reentry never adds stack. The C API installs the same budget in `engine_op` (`crates/agnijs-capi/src/registry.rs`) and hands its `stack_budget` option to the engine. Tests:

- `crates/agnijs-js-vm/tests/stack_budget.rs` and `crates/agnijs/tests/stack_budget.rs`: 300,000-deep array literals, 30,000 nested functions, deep `JSON.parse`, `eval`, `Function`, `JSON.stringify`, `flat`, proxies and regex groups through `Vm::new`, `make_engine`, `Context::new` and `Context::with_host`, on 2 MiB and 1 MiB threads;
- `deeply_nested_input_is_an_error_not_a_crash` in `crates/agnijs-cli/tests/cli.rs`;
- `deeply_nested_source_fails_cleanly_on_a_small_stack` and `the_stack_budget_option_is_checked_and_sets_the_depth` in `crates/agnijs-capi/tests/hostile.rs`.

The regex backtracker uses an explicit stack, so no pattern or input recurses on the native stack (`backtrack.rs`; `crates/agnijs-js-regex/tests/nesting.rs`).

**Changing it.** `Engine::set_stack_budget(Some(bytes))` sets the budget, and `stack_budget()` reads it. A `Context` reaches them through `context.engine()`. A larger budget on a larger thread accepts deeper nesting. The deepest nesting the default accepts, measured on Windows x64 in November 2026:

| Input | Release build | Debug build |
| --- | --- | --- |
| Array literals `[[[...]]]` | 160 | 18 |
| Object literals `{a:{a:...}}` | 133 | 14 |
| Function declarations | 185 | 50 |
| Regex groups | 256 (the regex nesting limit) | 116 |
| `JSON.parse` arrays | 1,202 | 263 |

Doubling the budget roughly doubles each depth. A debug build uses about ten times the stack per level. An embedder whose own debug builds run scripts with deep nesting can raise the budget on a larger thread, or build the engine crates with `opt-level = 1` in its dev profile. `set_stack_budget(None)` opts out. The engine then installs nothing, and only a budget the host installs itself (`agnijs_js_parser::stack::StackBudget::enter` around the call) or the thread's real stack bounds the recursion.

**What it does not cover.**

- **Too little stack at the call.** The budget is measured from where the host calls in, not from the thread's real limit, which safe code cannot read. A call made with less than budget plus headroom free can still overflow. Call in near the top of a thread, or lower the budget.
- **An opted-out engine.** With `None` and no budget of the host's own, deeply nested input overflows the thread's stack and ends the process.
- **Other engines.** The QuickJS engine behind the same interface keeps its own limits and returns `Unsupported` from `set_stack_budget`.

### Regular expressions (ReDoS)

**What it is.** `Host::regex_matcher` picks the policy per engine (ADR 0013 addenda):

- **`Auto`** (the default):
  - Patterns with nested quantifiers run on a linear-time Pike VM.
  - Other patterns the linear matcher can run backtrack under a step budget proportional to the linear cost, then finish on the linear matcher (`route` and `search` in `crates/agnijs-js-regex/src/lib.rs`).
- **`LinearWhenEligible`** runs every eligible pattern linearly. It is meant for hosts that take untrusted patterns.
- **`BacktrackingOnly`** always backtracks.

**How it is enforced.** `crates/agnijs-js-regex/tests/redos.rs` checks that pathological patterns finish in linear time under `Auto`. `routing.rs` checks which matcher each policy picks. Compiled regex programs in bytecode are verified before use (ADR 0010, `agnijs-bc-7` addendum).

**What it does not cover.**

- **Ineligible patterns.** Patterns with backreferences or lookaround are not eligible for the linear matcher under any policy. They backtrack with no budget, and today the interrupt cannot stop them (TASK-103.28).
- **Untrusted patterns.** A host that accepts patterns from untrusted users should refuse such patterns before they reach the engine, or run them in a process it can kill.
- **Builds without the linear matcher.** The `linear-regex` feature can be turned off. Bytecode written by such a build marks every pattern ineligible, so they all backtrack wherever that bytecode is loaded.

## Non-guarantees

### Side channels

The engine makes no attempt to resist timing or speculative-execution attacks:

- no Spectre mitigations (no index masking, no site isolation, no process separation);
- no constant-time operations;
- no protection against cache, branch-predictor or power analysis.

What helps is what the engine does not hand out. Script gets only the clock the host gives it, through `Host::now_ms`, in whole milliseconds by default. There is no `performance.now()`. There are no threads or shared memory with another agent, so `SharedArrayBuffer` cannot build a timer. `WeakRef` and `FinalizationRegistry` do let script observe when the collector runs.

Run code of different trust levels in different processes when a side channel matters. Realms and engines in one process share an address space and a core.

### Realms are not a security boundary

Realms in one engine share a heap, a memory limit, fuel and the interrupt. An object the host passes from realm A to realm B gives B a path into A's intrinsics, for example through `obj.constructor.constructor`, as in every JavaScript engine. Use one engine per tenant, and one process per tenant where a crash or side channel matters. ShadowRealm is not implemented (ADR 0025).

### Host bugs

Every capability the host grants is attacker-reachable. Treat each host function as an API exposed to the attacker:

- validate its arguments;
- bound its work and its allocations, which the memory limit, fuel and interrupt do not see;
- never let it return host internals.

Module loading is a capability. `agnijs-js-resolve` implements Node's algorithm and does not confine paths: `../` and absolute specifiers resolve wherever they point (ADR 0023). Its package `exports` and `imports` targets are checked (`InvalidPackageTarget`), but confining loads to a directory is the host's job, in `Host::load_module`.

### Bytecode and snapshots are trusted input

The checksum is FNV-1a, which catches accidents and is not a signature. Anyone who can write the bytes can reseal them. Beyond the pools it checks, the loader trusts operands that depend on run-time context, such as environment depth and slot, and module import slots (ADR 0010 M7 addendum; ADR 0022). Altered bytecode that still verifies can compute wrong values, and a regex program that verifies can match the wrong strings. It may also reach a VM invariant and panic. In the C API that poisons the engine; in the Rust API the panic unwinds into the embedder. TASK-103.30 tracks a verifier.

Load bytecode and snapshots only from a source you trust. If they cross a trust boundary (a download, a cache directory other users can write), authenticate them first, for example with a MAC or a signature over the whole container, keyed by something the attacker does not hold. A snapshot also carries the heap it was taken from, including any secrets the bootstrap left in it.

### The debugger and the CDP endpoint are full control

The debugger is a separate capability: `agnijs_js_vm::make_debuggee` returns a `Debuggee`, which an `Engine` alone does not expose (ADR 0017). An embedder that never creates one has no debugger. An attached client, however, can:

- evaluate any code in any frame;
- read and write every variable;
- call every capability the host granted.

`CdpServer` (`crates/agnijs-js-cdp/src/server.rs`) has no authentication. Today it also has these weaknesses (TASK-103.29):

- **No header checks.** It does not check the `Host` or `Origin` header, and its target id is `agnijs-<port>`. Any web page in a browser on the same machine can open `ws://127.0.0.1:9229/agnijs-9229` and drive the engine. With DNS rebinding it can read `/json/list` too. For `agnijs --inspect`, that includes reading files through the module loader.
- **Any address.** It binds whatever address it is given. The CLI defaults to `127.0.0.1` but accepts `--inspect=0.0.0.0:9229` without a warning.
- **Unbounded messages.** It reassembles fragmented WebSocket messages without a total limit. Each frame is capped at 64 MiB (`ws.rs`).

Never expose the endpoint where untrusted code or people can reach it. Do not leave it listening while browsing. Never enable it in production builds.

Source maps are parsed defensively, and hostile maps are rejected without a panic (`hostile_maps_are_rejected_without_panicking` in `crates/agnijs-js-debugger/tests/source_map.rs`). The engine never fetches a map itself.

### Unsafe code in the FFI and JIT crates

- **The C API.** It checks what it can: NULL, alignment, length overflow, ids, threads and struct sizes. It cannot check that a non-NULL pointer covers the length passed, or that a callback does not unwind a C++ exception. Both are undefined behaviour on the caller's side ([c-api.md](c-api.md#what-never-happens-and-what-still-can)).
- **The JIT** (`jit` feature, off by default, in progress under TASK-101.3). It writes machine code into memory that is never writable and executable at once:
  - On most platforms the pages are written read-write, then made read-execute (`VirtualProtect` on Windows, `mprotect` elsewhere).
  - On Apple silicon it uses a `MAP_JIT` mapping that is written only while this thread's JIT write protection is lifted.
  - The instruction cache is flushed before the first call (`ExecMemory::new` and the `sys` and `macos` modules in `crates/agnijs-js-jit/src/exec.rs`).

  The memory safety of the generated code rests on the code generator's correctness, which is tested differentially against the interpreter (`tests/jit_differential.rs`) and not proved. A JIT bug can be a memory-safety bug where an interpreter bug cannot. Leave the `jit` feature off for untrusted code unless you accept that. A fuel budget or an attached debugger keeps the JIT from running.

### Panics in the Rust API

The engine is meant never to panic on any script. test262 runs in full in CI, and the C API's fuzzer runs mutated scripts differentially. But the Rust API has no `catch_unwind` boundary. A panic in the engine, or in your host function or `Host` hook, unwinds into your code. If you catch it, whether the engine is still consistent is undefined today (TASK-103.31). Discard the engine after a caught panic. Continuous fuzzing is tracked by TASK-103.18.

### What the engine cannot stop

Some failures are outside the engine's control:

- The operating system can refuse memory, and Rust's allocator then aborts.
- The host's thread can have less stack than the budget plus headroom.
- A host bug can corrupt the process.
- A dependency the engine links (ICU4X data, `serde_json` in the CDP crate, Cranelift under `jit`) can have a bug.

## Embedder checklist

For code you do not trust:

1. **One engine per trust domain**, and one process per domain where a crash or a side channel matters.
2. **A memory limit** (`set_memory_limit`), set well below what the process can afford. It does not yet count ArrayBuffers or collection entries (TASK-103.25), so also cap the process with the operating system (a job object, cgroup or `RLIMIT_AS`) or run the engine in a child process you can kill.
3. **A wall-clock deadline** through `Host::interrupt` (or `agnijs_engine_request_interrupt` from a watchdog thread). Add **fuel** (`set_fuel`) when you need a deterministic bound. Neither stops a long regex match or a native loop yet (TASK-103.28). A watchdog that can kill the process is the only hard bound today.
4. **A native stack bound.** Every engine sets one by default; call in with the budget plus headroom free (768 KiB by default), and do not opt out with `set_stack_budget(None)`.
5. **The regex policy.** Keep `Auto`, or pick `LinearWhenEligible`, and keep the default `linear-regex` feature. Screen untrusted patterns that use backreferences or lookaround.
6. **No string compilation** unless the script needs it: `Host::can_compile_strings` returns false, or `AGNIJS_FLAG_NO_STRING_COMPILATION`.
7. **Only the capabilities the script needs.** Validate and bound every host function. Confine `Host::load_module` to an allow-list or directory. Do not hand script objects from another realm or tenant.
8. **A fixed clock and seed** (`now_ms`, `random_seed` or `set_random_seed`) when you want neither wall time nor entropy exposed. See [determinism.md](determinism.md).
9. **No untrusted bytecode or snapshots.** Authenticate anything that crossed a trust boundary before `eval_bytecode`, `ModuleSource::Bytecode` or `make_engine_from_snapshot`.
10. **No debugger in production.** Never bind CDP to a non-loopback address, and do not leave it listening (TASK-103.29).
11. **The `jit` feature off** unless you accept code-generator bugs as memory-safety bugs.
12. **Treat a panic as fatal to the engine.** Discard it after a caught panic. A C API engine reports `AGNIJS_ERR_POISONED`.

## Example: granting capabilities one by one

The host below grants one capability, a `fetchConfig(key)` function that reads from an allow-listed map. It refuses `eval`, gives a frozen clock and a fixed seed, and serves one module from memory. It sets a memory limit, fuel and a deadline. The native stack budget is on by default, so it needs no line here.

```rust
use agnijs::{Context, Host};
use agnijs::api::{Error as EngineError, ModuleSource, RealmId};
use std::rc::Rc;
use std::time::{Duration, Instant};

struct Sandbox {
    deadline: Instant,
}

impl Host for Sandbox {
    // No eval, no new Function.
    fn can_compile_strings(&self, _: RealmId) -> bool { false }
    // Stop the script when its time is up.
    fn interrupt(&self) -> bool { Instant::now() >= self.deadline }
    // No wall time and no entropy.
    fn now_ms(&self) -> f64 { 0.0 }
    fn random_seed(&self) -> u64 { 42 }
    // Only one module exists, and it is not on disk.
    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        match resolved {
            "app:greeting" => Ok(ModuleSource::Source("export default 'hi';".into())),
            _ => Err(format!("module not allowed: {resolved}")),
        }
    }
}

fn main() -> agnijs::Result<()> {
    let host = Rc::new(Sandbox { deadline: Instant::now() + Duration::from_secs(1) });
    let mut js = Context::with_host(host);
    js.engine().set_memory_limit(Some(64 << 20));
    js.engine().set_fuel(Some(50_000_000))?;

    // The one capability: read a value from an allow-list.
    js.register_try_fn("fetchConfig", |(key,): (String,)| match key.as_str() {
        "theme" => Ok("dark".to_string()),
        _ => Err(agnijs::JsError::range_error("unknown key")),
    })?;

    let theme: String = js.eval("fetchConfig('theme')")?;
    assert_eq!(theme, "dark");

    let e = js.eval::<i32>("eval('1')").unwrap_err();
    assert!(e.is_js("EvalError"));

    let e = js.eval::<()>("for (;;) {}").unwrap_err();
    assert!(matches!(
        e,
        agnijs::Error::Engine(EngineError::Interrupted | EngineError::OutOfFuel)
    ));
    Ok(())
}
```

What the script can do here is exactly this:

- call `fetchConfig`;
- import `app:greeting`;
- use the ECMAScript built-ins;
- write to a `console` that goes nowhere.

The C equivalent sets the same things through `AgnijsEngineOptions`: `memory_limit`, `stack_budget`, `interrupt`, `now_ms`, `AGNIJS_FLAG_NO_STRING_COMPILATION` in `flags`, and the module hooks. It adds `agnijs_set_fuel` and `agnijs_new_function`; see [c-api.md](c-api.md).

## Reporting a vulnerability

A disclosure policy (SECURITY.md) is tracked by TASK-103.18. Until it exists, report privately to the maintainer rather than in a public issue.
