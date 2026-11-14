# The AgniJS security model

This page says what an embedder can rely on when AgniJS runs code it does not trust, what it cannot rely on, and what it must do itself. Each claim names the code or the test behind it. Where the engine falls short of what a sandbox needs, the gap is stated next to the claim with the task that tracks it. Read those gaps before you rely on a limit.

AgniJS is pre-release. This page describes `main` as of TASK-103.25 (November 2026).

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

- **`agnijs-js-jit`.** It sets `#![deny(unsafe_code)]` and allows it only in `exec.rs`: executable memory, the entry trampoline, the frame handle and the probes that find where a `Vec`, an `Rc` and an `Option` keep their words (run under Miri).
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

Bytecode carries a format tag (`agnijs-bc-N`), and the payload has a checksum. Snapshots add a native-registry fingerprint. A container from another engine or build is refused with `BytecodeMismatch`, and damage is refused with `BytecodeCorrupt`. The loader also checks every operand that indexes a block's own pools, and the structure of a module record. The interpreter checks the operands that depend on the frame they run in, so a container that loads cannot panic the engine (ADR 0010 and ADR 0022, TASK-103.30 addenda). Tests:

- `another_engines_bytecode_is_a_version_mismatch` and `a_corrupt_payload_is_refused_without_running` in `crates/agnijs-js-vm/tests/bytecode.rs`;
- `corrupted_bytes_never_panic` and `operands_outside_their_pools_are_rejected_by_verification` in `crates/agnijs-js-compiler/tests/serialize.rs`;
- `fuzzed_snapshots_load_or_fail_without_panicking` in `crates/agnijs-js-vm/tests/snapshot.rs`;
- `crafted_script_bytecode_runs_without_panicking`, `crafted_module_bytecode_runs_without_panicking` and `crafted_snapshot_code_runs_without_panicking` in `crates/agnijs-js-vm/tests/bytecode_fuzz.rs`. They alter code, reseal it and run whatever loads, also with the `jit` feature.

This guards against accidents, not attackers: see [Bytecode and snapshots are trusted input](#bytecode-and-snapshots-are-trusted-input).

## Limits

Only the interpreter's frame caps, the native stack budget, the maximum string length and the regex routing are on by default. A host that runs untrusted code sets the others.

### Memory limit

**What it is.** `Engine::set_memory_limit(Some(bytes))`, or `memory_limit` and `agnijs_set_memory_limit` in the C API. When the counted heap passes the limit, the running call fails with `Error::OutOfMemory` (`AGNIJS_OUT_OF_MEMORY`), and the engine stays usable.

**How it is enforced.** Allocations add their size to a counter (`Heap::charge` and `note_growth` in `heap.rs`). The counter covers:

- object headers, slots, elements and dictionaries (`ObjectData::extra_bytes` in `object.rs`);
- ArrayBuffer and SharedArrayBuffer backing stores, from allocation on and as a resizable buffer grows (`Class::payload_bytes` in `object.rs`, `resize_bytes` in `builtins/arraybuffer.rs`);
- the live entries of Map, Set, WeakMap and WeakSet, at `MAP_ENTRY_BYTES` each (the entry slot plus the key index, 73 bytes on 64-bit targets) plus the contents of string and BigInt keys, which the index copies (`MapData::heap_bytes` and `map_insert` in `builtins/collections.rs`);
- strings, including flattened ropes;
- environments, symbols and BigInts.

`memory_usage` (`agnijs_memory_usage`) and the live size the collector computes after each sweep use the same sizes, so a buffer or a collection that dies leaves the count at the next collection.

The collection threshold is capped at the distance to the limit (`rearm_threshold`), so a collection runs before the limit is declared passed. The check itself runs at the dispatch loop's safe points: `attention_slow` in `interp/mod.rs`, which also applies under native frames.

A built-in that allocates a large block in one step checks the limit before it allocates (`Vm::check_memory` and `Heap::would_exceed` in `heap.rs`), and fails with the same `OutOfMemory` error, which JavaScript cannot catch:

- every ArrayBuffer, SharedArrayBuffer and typed-array buffer allocation, including `slice` and `transfer` (`zeroed` in `builtins/arraybuffer.rs`), and `resize` and `grow`;
- `repeat`, `padStart` and `padEnd`, for the whole result;
- `Array.prototype.join`, `toLocaleString` and the typed-array `join`, for the separators up front, and every builder that checks the [string length](#string-length) as it grows (`Vm::check_string_len` checks the limit too), for each piece;
- concatenation that makes a rope (`Vm::concat`), for the flat string the rope becomes when it is read, so doubling a string cannot pass the limit as unread ropes.

A built-in cannot run the collector, so it does not know how much of what was allocated since the last collection is garbage. It refuses a request when the live size of the last collection plus the request passes the limit, or when everything counted since, garbage included, plus the request passes twice the limit. The first rule refuses what cannot fit. The second bounds what one call can take, however many blocks it allocates. Neither refuses a program that drops what it allocates: a loop that makes and drops a 1 MiB buffer 200 times runs to the end under a 16 MiB limit.

Tests:

- `memory_limits_are_enforced` (conformance kit);
- `memory_limit_faults_instead_of_growing` and `a_memory_limit_stops_a_loop_that_only_grows_arrays` (`tests/gc.rs`);
- `a_memory_limit_stops_a_huge_string_and_a_runaway_script` and `the_memory_limit_applies_inside_jobs_and_can_be_raised_again` (`tests/host_driven.rs`);
- `the_memory_limit_still_applies_under_a_native_frame` (`tests/gc_native_frames.rs`);
- `the_memory_limit_stops_runaway_allocation` and `the_memory_limit_counts_buffers_and_collections_and_refuses_huge_requests` (C API);
- `the_memory_limit_stops_a_growing_heap` (CLI);
- `crates/agnijs-js-vm/tests/memory_limit.rs`: a 16 MiB limit stops 256 MiB of ArrayBuffers, SharedArrayBuffers or typed arrays, resizable growth, a Map or Set of 2,000,000 entries, 2,000,000 WeakMap or WeakSet entries over a thousand keys, long string keys, a 1 GiB `repeat`, `padStart`, `padEnd`, `join` and doubling by `+`, templates and `concat`; dead buffers and entries leave the count and `memory_usage`; the same built-ins work under the limit;
- `crates/agnijs-js-bench/tests/memory_limit_peak.rs`: a global allocator measures what the process allocates while a 16 MiB limit stops 256 MiB of ArrayBuffers, a 2,000,000-entry Map, a 1 GiB `repeat` and a 4 GiB ArrayBuffer. Each stays under 48 MiB, and the allocator refuses anything past 192 MiB, so a regression fails the test instead of allocating gigabytes.

**What it does not cover.**

- **Approximate sizes.** The counter uses fixed per-value sizes, not what the allocator really holds. A vector's spare capacity, the allocator's rounding and the engine's own tables (shapes, bytecode, inline caches, the intern table) are not counted. Expect the process to use more than the limit, by a factor that depends on the workload; the peak test above measured under three times the limit.
- **Map and Set tombstones.** A deleted Map or Set entry leaves an empty slot in insertion order so that live iterators keep their place, and the slot stays until the collection dies. The count covers live entries only, so `for (;;) { m.set(k, v); m.delete(k); }` grows the process by 32 bytes per round without reaching the limit. WeakMap and WeakSet have no iterators and drop their empty slots. Bound such a script with fuel or a deadline. TASK-103.32.
- **Native temporaries.** A built-in's working buffers that are not the block it returns, such as the UTF-16 copy `repeat` and the pad methods build before they store a one-byte result, or a sort's scratch array, are checked (where listed above) but not counted afterwards. They are freed when the call returns.
- **Built-ins that allocate many small values in one call.** `split`, `Object.keys`, `Array.from` over a native iterator and similar calls are bounded by the input they read, which is already counted, but can make several times its size in new values before the next safe point.
- **The process allocator.** Rust's allocator aborts the process when the operating system refuses memory. The limit is meant to be reached well before that, but the gaps above, or a limit set close to what the process can get, can get there first.
- **Engines and realms.** The limit is per engine, and realms in one engine share it.

### Fuel (instruction budget)

**What it is.** `Engine::set_fuel(Some(n))`, or `agnijs_set_fuel`. JavaScript may dispatch `n` more bytecode instructions, counted across realms, promise jobs and callbacks from native code. When none are left, the call fails with `Error::OutOfFuel`. JavaScript cannot catch that, and no `finally` runs. Every later call fails too until the host sets a new budget. The count is deterministic.

**How it is enforced.** The fuel flag is part of the attention word, and the dispatch loop's slow path charges one unit per instruction (`attention_slow_interp` in `interp/mod.rs`, `charge_fuel` in `vm.rs`). While a budget is set, compiled code is never entered, so the JIT cannot skip the count. Tests in `crates/agnijs-js-vm/tests/fuel.rs`:

- `an_endless_loop_runs_out_of_fuel`
- `javascript_cannot_catch_running_out_of_fuel`
- `a_spent_budget_fails_every_call_until_the_host_sets_a_new_one`
- `the_budget_spans_callbacks_from_natives_and_promise_jobs`
- `the_jit_does_not_change_what_a_program_costs`

**What it does not cover.** Fuel counts bytecode only. A call to a native built-in costs one instruction, whatever the built-in does, plus the instructions of any JavaScript it calls back: `Array(1e7).join()`, a sort, or an exponential regex match costs a handful of units. That keeps the count deterministic, but fuel bounds bytecode, not time. The [wall-clock interrupt](#wall-clock-interrupt), which the regex matchers and native loops poll, and the memory limit bound the work inside built-ins.

### Wall-clock interrupt

**What it is.** `Host::interrupt` returns true to stop the running call with `Error::Interrupted`. In the C API this is the `interrupt` hook, or `agnijs_engine_request_interrupt` from any thread. The engine has no timer of its own. The host keeps a deadline, and the hook compares against it. An interrupt skips `finally` blocks and unwinds through host functions. The engine stays usable.

**How it is enforced.** `Vm::poll_interrupt` (`vm.rs`) runs at the interpreter's function entries, loop back-edges and call returns. The JIT polls at the same points (`interp/jit.rs`), so recursion without loops is stopped as well (TASK-100.33). The host hook itself is called once every 4096 polls. Native code polls too (TASK-103.28):

- **Regex matches.** Both matchers count their steps in slices of 65,536 and ask the host at the end of each (`Meter` in `agnijs-js-regex/src/backtrack.rs`, `Regex::exec_polled`). The step budget of `Auto` is spent exactly as before, so polling never changes which matcher finishes a search or what it matches.
- **Native loops.** Built-ins that loop over their input call `Vm::poll_interrupt` once per iteration: the Array and TypedArray methods, `Array.from`, String `split`, `replaceAll` and `repeat`, the RegExp `match`, `replace` and `split` loops, JSON `parse` (once per 4096 values) and `stringify`, Map and Set iteration and set methods, `Object.assign`, `fromEntries`, `groupBy` and the key lists, and the iterator helpers.
- **String search.** `indexOf`, `lastIndexOf`, `includes`, `split` and `replace` with a string pattern compare up to haystack times needle units, and ask the host once per 2^20 units compared (`SearchPoll` in `string.rs`).

Tests:

- `a_recursive_function_with_no_loops_is_interrupted_by_the_host` and `a_chain_of_calls_returning_without_loops_is_interrupted_by_the_host` (`tests/dispatch.rs`);
- `an_interrupt_stops_a_runaway_job_and_the_engine_carries_on` and `an_interrupt_unwinds_through_host_functions_and_skips_finally_blocks` (`tests/host_driven.rs`);
- `an_interrupt_stops_a_compiled_loop` (`tests/jit_differential.rs`);
- `the_interrupt_hook_stops_an_infinite_loop` and `another_thread_can_request_an_interrupt` (C API);
- `crates/agnijs-js-vm/tests/interrupt_native.rs`: exponential lookahead under `Auto`, nested quantifiers under `BacktrackingOnly`, backreferences, global `replace`, `split` and `match` over millions of matches, `join` over a 3,000,000-element array, Array methods over huge array-likes, TypedArray methods, quadratic string searches and JSON, each stopped within 2 s in a debug build, and match results unchanged by polling;
- `a_poll_stops_a_runaway_search_under_every_mode` and `polling_does_not_change_what_matches` (`agnijs-js-regex`);
- `another_thread_can_interrupt_a_runaway_regex_and_a_native_loop` (C API).

**What it does not cover.**

- **Bounded native work between polls.** Some native work does not poll, because it is bounded by the size of one string or buffer: copying or filling bytes (`repeat` and `padStart` builders, `TypedArray.prototype.reverse`, `slice` and `set` between buffers), case mapping, normalization, `encodeURI`, base64 and hex, and the regex matchers' scan for a first character. Each is at most linear in a string of 2^30 units or a buffer of 4 GiB.
- **Debugger pauses.** A pause request (`PauseRequester`) is taken at the next statement. A native call cannot pause partway through, so the pause comes after it returns; the interrupt is what stops it.
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

With the `jit` feature, compiled functions call each other on the native stack. Their frames nest at most `JIT_STACK` (96 KB in an optimised build, `interp/jit.rs`) below where compiled code was entered and never past the budget's limit; beyond that a call is made through the runtime, on the interpreter's frame stack (`machine_frames_do_not_nest_past_the_hosts_stack_budget` and `recursion_past_the_stack_bound_unwinds_and_goes_on` in `tests/jit_calls.rs`).

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
- **AddressSanitizer's fake stacks.** The budget reads the stack position from a local's address. AddressSanitizer's use-after-return detection, on by default on Linux, moves locals into heap memory, and the budget then runs out at random depths. Run sanitizer builds with `ASAN_OPTIONS=detect_stack_use_after_return=0`, as CI does.
- **Other engines.** The QuickJS engine behind the same interface keeps its own limits and returns `Unsupported` from `set_stack_budget`.

### String length

**What it is.** No string is longer than 2^30 UTF-16 code units (`MAX_STRING_LENGTH` in `crates/agnijs-js-vm/src/string.rs`). An operation that would make a longer one throws a catchable `RangeError: Invalid string length` instead. It is always on and cannot be changed. The value sits between V8 (2^29 - 24) and SpiderMonkey (2^30 - 2). Two maximal lengths add up without wrapping the `u32` a rope stores, and a flat string at the maximum takes at most 2 GiB.

**How it is enforced.**

- **Concatenation.** `Heap::concat` returns `None` past the maximum, before it builds a rope, and `Vm::concat` throws. That covers `+`, `+=`, template literals, `String.prototype.concat` and every other concatenation, so a rope's length cannot wrap.
- **Builders.** Built-ins that grow a string piece by piece check the length as they go (`Vm::check_string_len`): `Array.prototype.join`, `toString` and `toLocaleString`, the typed array versions, `replace`, `replaceAll` and `RegExp.prototype[Symbol.replace]` (including `$` substitutions), `String.raw`, `JSON.stringify`, `encodeURI`, `encodeURIComponent` and `escape`. A `join` whose separators alone pass the maximum (`new Array(2 ** 31).join()`) throws before it builds anything. `repeat`, `padStart` and `padEnd` check the final length up front, and strings made from code units (`fromCharCode`, `fromCodePoint`, case mappings, `normalize`) are checked before they are allocated.
- **A backstop.** Every other string allocation checks the length too (`Heap::alloc_string`); only concatenation and substrings, which are already within the maximum, skip it. A path the checks above miss gets the empty string, and the `RangeError` is raised at the next safe point of the dispatch loop, or when the call from the host returns.

Tests in `crates/agnijs-js-vm/tests/string_length.rs`: doubling a string past the maximum, one unit past it by each kind of concatenation, the refused `join`, every path above with the maximum lowered to 1,000 units for the test, and the error the host sees.

**What it does not cover.**

- **The backstop's empty string.** A built-in that reaches the backstop carries on with the empty string until the next safe point, so its result is discarded but its other effects happen. Every built-in known to make a string this long checks before the backstop.
- **Memory.** The maximum bounds one string, not the heap. Many strings near it are the memory limit's job.

### Lists sized by a script's `length`

**What it is.** A script controls the `length` of an array-like for free: a sparse array or a plain object can claim 2^53-1 elements. Native code that builds a Rust list from such a `length` must not preallocate from it.

**How it is enforced.**

- **Argument and capture lists.** CreateListFromArrayLike (`apply`, `Reflect.apply`, `Reflect.construct`, a proxy's `ownKeys` result) and the capture list of `RegExp.prototype[Symbol.replace]` refuse more than 2^22 values (`MAX_LIST_LENGTH` in `crates/agnijs-js-vm/src/ops.rs`) with a catchable `RangeError`, charge the list to the memory limit before allocating it (`Vm::check_list_length`), and poll the interrupt while they fill it. A dense array passed to `apply` takes a fast path that copies the elements that already exist, so its size is the memory limit's job. TASK-103.48, 103.49.
- **Rust API conversion.** `from_value`, `eval::<T>` and host-function arguments convert at most 2^22 array elements and object members per value, summed over the whole tree (`agnijs::MAX_CONVERTED_VALUES`). More gives `Error::Convert`, a `TypeError` in the script for host-function arguments. TASK-103.39.
- **BigInt widths.** `BigInt.asUintN` of a negative value has as many bits as asked for. Past 2^30 bits, the same maximum as `<<`, it throws a `RangeError`, and below that the result is charged to the memory limit before it is built. `asIntN`, and `asUintN` of a non-negative value, return the input when it already fits, so a huge width allocates nothing. TASK-103.44.

Tests in `crates/agnijs-js-vm/tests/untrusted_lengths.rs` and `crates/agnijs/tests/long_arrays.rs`.

**What it does not cover.** A BigInt's size is checked when `asUintN` builds it, but the collector still counts each live BigInt at a fixed size (TASK-103.50).

### Regular expressions (ReDoS)

**What it is.** `Host::regex_matcher` picks the policy per engine (ADR 0013 addenda):

- **`Auto`** (the default):
  - Patterns with nested quantifiers run on a linear-time Pike VM.
  - Other patterns the linear matcher can run backtrack under a step budget proportional to the linear cost, then finish on the linear matcher (`route` and `search` in `crates/agnijs-js-regex/src/lib.rs`).
- **`LinearWhenEligible`** runs every eligible pattern linearly. It is meant for hosts that take untrusted patterns.
- **`BacktrackingOnly`** always backtracks.

**How it is enforced.** `crates/agnijs-js-regex/tests/redos.rs` checks that pathological patterns finish in linear time under `Auto`. `routing.rs` checks which matcher each policy picks. Compiled regex programs in bytecode are verified before use (ADR 0010, `agnijs-bc-7` addendum).

**What it does not cover.**

- **Ineligible patterns.** Patterns with backreferences or lookaround are not eligible for the linear matcher under any policy. They backtrack with no budget. The interrupt stops them: the matcher asks the host every 65,536 steps (TASK-103.28).
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

Module loading is a capability. `agnijs-js-resolve` implements Node's algorithm and does not confine paths: `../` and absolute specifiers resolve wherever they point (ADR 0023). What stays inside a package is a bare specifier's subpath and the targets of its `exports` and `imports`: a `..` or `.` segment in any separator form (`/`, `\`, percent-encoded, or with the trailing dots and spaces Windows drops), a drive or stream colon, or a NUL is refused (`InvalidSpecifier`, `InvalidPackageTarget`), and every package path is checked again after it is joined to stay at or below the package directory (TASK-103.38). The check is lexical: a symlink inside the package is followed. Confining relative and absolute specifiers to a directory is the host's job, in `Host::load_module`.

### Bytecode and snapshots are trusted input

The checksum is FNV-1a, which catches accidents and is not a signature. Anyone who can write the bytes can reseal them. The loader verifies the pools and module records. The interpreter checks the operands that depend on run-time context, such as environment depth and slot, import slots, register kinds and generator state. A failed check throws an `internal error: invalid bytecode` Error rather than panicking (ADR 0010 and ADR 0022, TASK-103.30 addenda). What stays trusted is meaning. Altered bytecode that is still in range can compute wrong values or loop until fuel, the interrupt or the memory limit stops it. A regex program that verifies can match the wrong strings.

Load bytecode and snapshots only from a source you trust. If they cross a trust boundary (a download, a cache directory other users can write), authenticate them first, for example with a MAC or a signature over the whole container, keyed by something the attacker does not hold. A snapshot also carries the heap it was taken from, including any secrets the bootstrap left in it.

### The debugger and the CDP endpoint are full control

The debugger is a separate capability: `agnijs_js_vm::make_debuggee` returns a `Debuggee`, which an `Engine` alone does not expose (ADR 0017). An embedder that never creates one has no debugger. An attached client, however, can:

- evaluate any code in any frame;
- read and write every variable;
- call every capability the host granted.

`CdpServer` (`crates/agnijs-js-cdp/src/server.rs`) has no authentication. It refuses what a web page in a browser on the same machine could send it, the class of bug Node fixed for CVE-2018-7160 (TASK-103.29):

- **Host check.** Every route, the `/json` discovery routes and the WebSocket upgrade, answers 403 unless the `Host` header is an IP literal or `localhost`, with an optional port. A DNS-rebinding page sends its own host name, so it cannot read `/json/list`.
- **Origin check.** Only browsers send `Origin`. A request that carries one gets 403 unless it is the DevTools front end's (`devtools://devtools`, `chrome-devtools://devtools`) or one the embedder allows with `CdpOptions::allow_origin`. Clients that are not browsers (VS Code, `chrome://inspect`, scripts) send none.
- **Random target id.** The WebSocket path is a random version 4 UUID per server, not derived from the port. Any other path gets 404.
- **Loopback only.** `CdpServer::listen` refuses an address that is not loopback (`PermissionDenied`). `CdpServer::listen_with` and `CdpOptions::allow_remote(true)` opt in. The CLI defaults to `127.0.0.1`; given `--inspect=0.0.0.0:9229` it opts in and prints a warning.
- **Bounded messages.** A reassembled WebSocket message may be at most 64 MiB (`CdpOptions::max_message_size`), and so may each frame. A frame that would pass the limit is refused before its payload is read, and the server closes the connection with status 1009. The HTTP request head is capped at 64 KiB.

These checks stop a web page, not a local process: any program on the machine can still connect and run code. Never expose the endpoint where untrusted code or people can reach it, and never enable it in production builds.

Source maps are parsed defensively, and hostile maps are rejected without a panic (`hostile_maps_are_rejected_without_panicking` in `crates/agnijs-js-debugger/tests/source_map.rs`). The engine never fetches a map itself.

### Unsafe code in the FFI and JIT crates

- **The C API.** It checks what it can: NULL, alignment, length overflow, ids, threads and struct sizes. It cannot check that a non-NULL pointer covers the length passed, or that a callback does not unwind a C++ exception. Both are undefined behaviour on the caller's side ([c-api.md](c-api.md#what-never-happens-and-what-still-can)).
- **The JIT** (`jit` feature, off by default, in progress under TASK-101.3). It writes machine code into memory that is never writable and executable at once:
  - On most platforms the pages are written read-write, then made read-execute (`VirtualProtect` on Windows, `mprotect` elsewhere).
  - On Apple silicon it uses a `MAP_JIT` mapping that is written only while this thread's JIT write protection is lifted.
  - The instruction cache is flushed before the first call (`ExecMemory::new` and the `sys` and `macos` modules in `crates/agnijs-js-jit/src/exec.rs`).

  The memory safety of the generated code rests on the code generator's correctness, which is tested differentially against the interpreter (`tests/jit_differential.rs`, `tests/jit_calls.rs`) and not proved. Generated code reads and writes the interpreter's own frames, registers, environments and global bindings at offsets the VM computes in safe code (`HeapLayout`, `CallLayout`); `docs/design/jit-deopt-and-gc-maps.md` states the invariants it is trusted with (I1 to I5). A JIT bug can be a memory-safety bug where an interpreter bug cannot. A review in November 2026 (`docs/jit-review-2026-11.md`) found one that let a script read memory it should not see: a local not yet assigned read, in a `catch` block, as the stale payload bytes of its register taken for a number (TASK-103.34, fixed; `tests/jit_deopt.rs` holds the reproductions). Leave the `jit` feature off for untrusted code unless you accept that. A fuel budget or an attached debugger keeps the JIT from running.

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
2. **A memory limit** (`set_memory_limit`), set well below what the process can afford: the count is approximate, and the process can use a few times the limit (see [Memory limit](#memory-limit)). For a hard bound, also cap the process with the operating system (a job object, cgroup or `RLIMIT_AS`) or run the engine in a child process you can kill.
3. **A wall-clock deadline** through `Host::interrupt` (or `agnijs_engine_request_interrupt` from a watchdog thread). The regex matchers and native loops poll it, so it stops a long match or a loop over a huge array too. Add **fuel** (`set_fuel`) when you need a deterministic bound; fuel does not count work inside native calls, so keep the deadline as well.
4. **A native stack bound.** Every engine sets one by default; call in with the budget plus headroom free (768 KiB by default), and do not opt out with `set_stack_budget(None)`.
5. **The regex policy.** Keep `Auto`, or pick `LinearWhenEligible`, and keep the default `linear-regex` feature. Screen untrusted patterns that use backreferences or lookaround.
6. **No string compilation** unless the script needs it: `Host::can_compile_strings` returns false, or `AGNIJS_FLAG_NO_STRING_COMPILATION`.
7. **Only the capabilities the script needs.** Validate and bound every host function. Confine `Host::load_module` to an allow-list or directory. Do not hand script objects from another realm or tenant.
8. **A fixed clock and seed** (`now_ms`, `random_seed` or `set_random_seed`) when you want neither wall time nor entropy exposed. See [determinism.md](determinism.md).
9. **No untrusted bytecode or snapshots.** Authenticate anything that crossed a trust boundary before `eval_bytecode`, `ModuleSource::Bytecode` or `make_engine_from_snapshot`.
10. **No debugger in production.** Never bind CDP to a non-loopback address, and do not leave it listening.
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
