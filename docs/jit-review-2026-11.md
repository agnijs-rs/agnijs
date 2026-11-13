# Baseline JIT soundness review, November 2026

A review of the baseline JIT (feature `jit`) for correctness and memory-safety defects, at commit `fea680f` (origin/main, 2026-11-13), on Windows 11 x64 with rustc 1.99.0. It found one wrong-result defect and two low-severity ones. They are filed as backlog tasks and not fixed here. (They were fixed afterwards, each under its task: the Status column says how. The rest of this document is the review as it was written.)

Finding nothing in an area is not proof that the area is sound. The section [What was not reached](#what-was-not-reached) lists what this review did not cover.

## Findings

| Task | Severity | Status | What |
| --- | --- | --- | --- |
| TASK-103.34 | High (wrong results; exposes stale memory bytes to script) | Confirmed: script reproduction from a clean build, in modes `on` and `force`; a red Rust test. **Fixed**: liveness has the edge from a throwing instruction to its handler, and a register taken unguarded is spilled and written back as the value it held, never as bits boxed as a number (`docs/design/jit-deopt-and-gc-maps.md`, section 8). The side-exit variant this review could not reproduce was reproduced and is covered too (`tests/jit_deopt.rs`). | A local not yet assigned reads as a garbage number in a `catch` handler after a throw inside a loop whose registers were speculated to be numbers. |
| TASK-103.35 | Low | Confirmed with a Rust test against a toy runtime; not reachable from JavaScript without another bug. **Fixed**: the two trampolines share one helper that parks the payload and answers `EXIT_PANIC`, as the generic path's does (`exec::hit_or_out`; tests in `crates/agnijs-js-jit/tests/enter.rs` and `tests/calls.rs`). | A panic in `Runtime::fast` or `Runtime::global` is swallowed by its trampoline; the instruction is re-run by the generic path and compiled code goes on. |
| TASK-103.36 | Low | Latent: not reproduced, and argued unreachable today | The call thunk hands a directly called function's leftover pc to the caller's call site, which compares it with its own `pc + 1`. |

No memory-corruption defect was found. TASK-103.34 carries the `security` label because the garbage number is made of bytes the script should never see (one run showed the bits of what looks like an address).

### TASK-103.34 in short

The VM guesses that a loop's arithmetic registers hold numbers, and accepts `undefined` in the live frame as "not assigned yet". The code generator loads such a register without a guard wherever the liveness analysis says it is dead. The analysis follows normal control flow only: it has no edge from a throwing instruction to its handler. When an instruction with known register access (`GetNamed`, `Call`, `LdaGlobal`, ...) throws before the first assignment, the frame is materialised with the register's unguarded payload bytes boxed as a number, and the handler reads that.

```js
function f(n, o) {
  var x, y = 0;
  try { for (var i = 0; i < n; i++) { o.p; x = i; y = x + 1; } }
  catch (e) { return typeof x + ":" + x; }
  return "no throw " + y;
}
console.log(f(10, null)); // interpreter: undefined:undefined; AGNIJS_JIT=force: number:1.02e-311
```

Forcing the guard at every entry made every reproduction agree with the interpreter, which confirms the cause. That experimental binary was also used for part of the fuzzing below, so that this defect could not hide another one in the same programs.

## Scope

Read in full:

- `crates/agnijs-js-jit/src`: `exec.rs` (the only `unsafe`), `codegen.rs`, `analysis.rs`, `calls.rs`, `lib.rs`;
- `crates/agnijs-js-vm/src/interp/jit.rs`, `interp/ic.rs`, and the parts of `interp/mod.rs`, `interp/step.rs`, `interp/calls.rs`, `interp/globals.rs`, `object.rs`, `heap.rs`, `gc.rs` and `env.rs` that the JIT reads, writes or relies on;
- ADR 0019, ADR 0020 with its addenda, `docs/design/jit-deopt-and-gc-maps.md` (I1 to I5), `docs/security-model.md`.

For each emitted sequence the questions were: is every guard enough for what the code after it assumes; can a collection, a getter, a Proxy trap, an array resize or a re-entrant call invalidate an assumption between guard and use; can compiled code and the interpreter disagree about observable state.

## Method and run counts

**Reading.** The assumptions of each sequence were written down and checked against the interpreter's own fast paths and against every writer of the state the sequence reads.

**Differential testing.** Generated and hand-written scripts were run with the `agnijs` CLI (`cargo build --release -p agnijs-cli --features agnijs-js-vm/jit`) with the JIT off, `AGNIJS_JIT=on` and `AGNIJS_JIT=force`, comparing stdout, exit code and the first stderr line. Two generators were written for this review (Node scripts, kept outside the repository):

- *Generator 1*: three to seven untyped functions per program over a pool of objects of many kinds (plain, prototype chains, arrays with holes, classes with accessors, arguments objects, typed arrays, a Proxy, primitives), with loops, `try`/`catch`/`finally` that log every local, type-changing assignments, calls through `call`, `apply` and `new`, recursion, closures, and a `side()` function that deletes and adds properties and globals, swaps prototypes, installs accessors, freezes objects and resizes arrays in the middle of everything.
- *Generator 2*: mostly non-throwing numeric code with typed local pools, so that loops are compiled with number speculation and direct calls, plus the events that can break a guess: a throw caught in the same function, a local that changes type, a skipped first assignment, getters and callees with side effects. A warm-up variant runs every function 1300 times first, so that mode `on` compiles it.

| Run | Binary | Programs | Result |
| --- | --- | --- | --- |
| Generator 1, off / on / force | clean | 760 | 1 divergence: TASK-103.34 |
| Generator 1, off / on / force | experimental (guard forced) | 400 | 0 |
| Generator 2, off / on / force | experimental | 400 | 0 |
| Generator 2 with warm-up, off / on / force | experimental | 200 | 0 |
| Generator 2, force against Node 24 | experimental | 300 | 0 |
| Generator 1, force under `AGNIJS_GC_STRESS=1` | experimental | 80 | 69 agree; 11 ran past the 120 s limit (see below) |
| Deep recursion, 26 shapes at two depths, off / on / force | clean | 52 | 0, no crash |
| Hand-written probes (two files, about 200 checks), off / on / force | clean | 2 | 0; both also match Node apart from the note below |

The 11 GC-stress runs that hit the time limit produced no output and no wrong answer. One of them (seed 2002) was run again with the JIT off under the same stress: the interpreter alone had not finished after six minutes of CPU time when it was stopped, so the limit is at least partly the cost of a collection at every safe point. The other ten were not checked that way; see [What was not reached](#what-was-not-reached).

**QuickJS-NG** was not on this machine as a command, so Node 24 stood in as the outside oracle for Generator 2 (which logs error names only, not messages).

**Miri.** `cargo +nightly miri test -p agnijs-js-jit --lib -- exec::tests calls::tests`: 8 tests pass (the frame handle's reads and the layout probes). Machine code cannot run under Miri.

**Rust tests.** Two temporary tests confirmed TASK-103.34 (forced and tiered, both red) and TASK-103.35 (red, with its sibling for `slow` green). They were deleted; their text is in the tasks.

**Sanitizers.** None was run: AddressSanitizer on `x86_64-pc-windows-msvc` needs a nightly build of the whole graph and does not see machine code the process generates.

Heavy-lock time used: about 35 minutes.

## Reviewed with no finding

- **Executable memory.** Mapped read-write, filled, made read-execute, flushed; never writable and executable at once on the Windows path. Code is freed only when the last `Rc<Compiled>` goes; runs, the callee table and the retired list each hold one, and the retired list is cleared only when no compiled frame is on the list.
- **Entry and trampolines.** `Ctx` and `Shared` outlive the run and are touched through raw pointers only; panics in `slow`, `call`, `poll`, `returned`, the deopt and the "left" trampolines are parked and resumed (the exception is TASK-103.35).
- **Inline cache hits.** The guards equal the interpreter's (`ic_get`, `ic_set`): receiver class and dictionary word, shape, and for prototype hits the prototype id and epoch. Slot addresses are bounds-checked against the overflow length; the data-or-accessor test reads the live slot. No call separates guard from use. Shape ids freed by a collection cannot hit: the block's caches are cleared on entry, and the thunk compares the callee's epoch.
- **Dense elements.** Class mask, dense test (both possible niches), index round trip, bound and hole checks match `keyed_get_fast` and `keyed_set_fast`. Frozen, sealed, sparse, typed, mapped-arguments and prototype-backed arrays behave alike in every tier and in Node.
- **The call thunk.** Every field a call sets is written with a value of its type; the frame vector and register file are only pushed into spare capacity and re-read after the call; the code block's count is taken on push and given back only above one; `this` substitution, arity, realm switch and both polls match `push_function_frame` and `do_return`. Class constructors, generators, async functions, bound functions, natives, proxies and blocks that keep their arguments never enter the table.
- **Global sites.** Every writer of a dictionary and of the global lexical table moves the stamp before entries can move; an accessor or a dead-zone binding fails the tag test.
- **Environment slots.** Each record's kind and the slot index are checked; `with`, direct `eval` and per-iteration bindings agree across tiers.
- **GC roots.** Spilled boxed registers are traced through the handle; the accumulator and return values are written to a frame before any call that can collect; directly called functions keep nothing in machine state.
- **Native stack.** The floor check held in 26 recursion shapes (plain, mutual, getters, setters, `call`/`apply`, bound, constructors, natives that call back, proxies, iterators) at depth 200,000.
- **Operators.** Number fast paths, int32 conversion, equality of every pair of kinds, truthiness and NaN ordering were checked by reading and by an exhaustive pairwise run over 23 edge values.
- **Bytecode operands.** The loader checks every register operand and refuses blocks that can run past their end, so compiled code's unchecked register addressing rests on it.

## Observations that are not defects

- **Aliasing model.** The addresses given to machine code (`CallView`, the heap bases, the global stamp) are raw pointers taken from short-lived `&mut` borrows, and Rust code uses the same fields through other references between machine-code accesses. Rust cannot exploit this, since the accesses are in code it does not see, but it is outside what Miri or the aliasing models can vouch for.
- **Code size and compile time.** Blocks in these programs compiled to about 130 to 170 KB of machine code and 90 ms each, far above the figures in ADR 0020. One warm-up program ran in 0.5 s interpreted and 1.4 s in mode `on`, nearly all of it compile time. This is a tiering cost, not a soundness matter.
- **Number to string on ties (not the JIT).** `String(2 ** -25)` gives `2.9802322387695313e-8` where V8 gives `...12`. Both round-trip; the specification recommends the even digit on a tie but allows either. Not filed.

## What was not reached

- **macOS and Linux, AArch64.** `MAP_JIT`, per-thread write protection, `mprotect` and the instruction-cache flush were read but not run. Everything here ran on Windows x64.
- **GC stress in depth.** Only 80 programs ran forced under `AGNIJS_GC_STRESS=1`, and 11 of them did not finish in 120 s. The one that was checked is as slow in the interpreter, but a JIT-specific stall in the other ten is not ruled out. A stress run needs smaller programs or a longer limit, and an interpreter-under-stress run beside each.
- **The debugger.** Attaching while compiled frames wait in a call out (`jit_materialize_all`, the `materialized` flag) was read only; no test drove a debug client.
- **Interrupts, fuel and the memory limit under compiled code.** Read only; the existing tests cover them and none was added.
- **Cross-realm direct calls and snapshots.** The realm checks in the thunk and the global sites were read; no script exercised `ShadowRealm` or a restored snapshot under the JIT.
- **`bytecode_fuzz` and test262 with the JIT.** Not re-run here; CI runs both.
- **Mode `on` beyond the warm-up generator.** The tiering states (`CALLED`, `DIRECT`, `DEMAND`) were exercised by 200 programs only.
- **Cranelift itself.** Its code generation was taken as correct.
