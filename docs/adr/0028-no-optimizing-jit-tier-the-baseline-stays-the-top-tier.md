# 28. No optimizing JIT tier: the baseline stays the top tier, and performance work goes to the interpreter and the runtime

Date: 2026-11-16

## Status

Accepted. This is the go or no-go decision TASK-101.4 asks for: **no-go, to be reopened on the conditions at the end.**

## Context

ADR 0019 planned an optimizing tier "if the baseline justifies it": type specialization from inline-cache feedback, inlining, and guarded deoptimization to interpreter frames. The baseline JIT is built (ADR 0020 and its addenda, TASK-101.3 and its subtasks), reviewed for soundness (`docs/jit-review-2026-11.md`) and fuzzed in CI (`docs/jit-fuzz.md`). It stays off by default and is not built for iOS or WebAssembly.

The question is whether the next tier is the best use of the next block of performance work. The engine's first embedder is Gable, whose workloads are bundled framework code: RxJS pipelines, a signal graph, a template interpreter and keyed list diffing. The evidence below was measured for this decision at `0f03d8f`; the method and the full tables are in `docs/js-engine-performance-findings.md`, section "Performance-tier close-out, 2026-11-16". Ratios are time over the interpreter's in the same binary, 9 interleaved rounds.

## Evidence

### What the baseline tier gains, by kind of workload

| Kind of code | Workloads | Mode `on` | Forced |
| --- | --- | --- | --- |
| Loops over numbers, array elements and object properties | loop-arith, loop-array-index, prop-monomorphic, prop-polymorphic | 0.23 to 0.33 | 0.24 to 0.33 |
| Calls of plain functions the tier compiles | calls-recursive, closures | 0.36, 0.79 | 0.38, 0.69 |
| Calls through getters and `super` | method-calls | 1.02 | 0.75 |
| Allocation, strings, array methods, collections, promises | prop-write-alloc, string-concat, array-methods, map-set, promise-chain | 0.99 to 1.03 | 1.09 to 1.33 |
| Built-ins doing the work (regex, JSON) | regex, regex-long-input, json-parse, json-stringify | 1.00 to 1.01 | 1.00 to 1.21 |
| Start-up, loading, compiling | six workloads | 0.95 to 1.03 | 1.4 to 2600 (compile time) |
| Gable: rxjs and signals | five benches | 0.72 to 1.04, geomean 0.95 | 0.72 to 1.03, geomean 0.89 |
| Gable: templates and list diffing | three benches | 1.02 to 1.03 | 1.09 to 1.20 |

The tier does what a baseline tier does: three to four times on tight loops, nothing on code that spends its time in the runtime. In mode `on` the micro-benchmark geomean is 0.78, and Gable's is 0.98. Two of Gable's nine benches gain (1.3 to 1.4 times), three are level even when every function is compiled, three lose when forced, and tiered the six that gain nothing pay about 2% and 5 to 27 MiB of peak memory for blocks that were compiled and then given up.

### Where Gable's time goes

In the interpreter, by sampling profile:

| | Share of time |
| --- | --- |
| The dispatch loop, with the inline-cache hits and number arithmetic inlined into it | 21 to 48% |
| Property access out of line (cache misses, megamorphic sites, dictionaries, keyed access, defining properties) | 15 to 44% |
| Calls: frame push and pop, arguments, closures and environments | 8 to 29% |
| The collector | 0 to 13% |
| The system allocator | under 3% |
| Regular expressions | under 1.5% |

Only the first row is what compiling removes. With every function compiled, the benches where the tier wins spend 60% of their time in machine code. The ones that stay level spend 33 to 40% there, and the rest in the runtime the machine code calls: 1.5 to 3.8 calls out of compiled code for every call it makes itself, against 0.2 to 0.3 where it wins. ADR 0020 names what those call outs are: property access at sites with more than four shapes (78% of signal-churn's), `new`, getters and setters, `Function.prototype.call` and `apply`, `typeof x === "..."`.

### What an optimizing tier would plausibly buy

An optimizing tier makes machine code faster and moves some calls into it. It does nothing for time spent in the runtime.

- **On the kinds of code the baseline already wins**, the usual step from a baseline to an optimizing compiler is another factor of two to four: unboxed numbers across a whole loop, shape checks hoisted out of it, small callees inlined. For scale, the weval paper measures SpiderMonkey on Octane at 5.4 times its interpreter for the native baseline compiler and a further 3.9 times for the optimizing one (`docs/aot-research.md`, source F3). So loop-arith might go from 0.33 to 0.1 of the interpreter. No embedder has asked for that, and ADR 0011's answer for a hot numeric kernel, a host function in Rust, is faster still.
- **On Gable's rxjs and signal benches**, take the forced profile as the starting point and suppose the optimizing tier halves the time spent in machine code, which is generous for polymorphic, call-dense code with no numeric loops in it. Time falls by 17 to 31%: rxjs-operators from 0.76 to about 0.53 of the interpreter, the three level benches from 1.0 to about 0.8 to 0.85. Inlining would also remove some frame pushes, so say 1.2 to 1.5 times over the forced baseline on these five.
- **On the template and list benches**, less than that: they are the ones with the most time in out-of-line property access (22 to 44%) and the collector (8 to 13%), and forced code loses there today.
- **Only for code that gets hot enough.** An optimizing compile costs more than the baseline's, which is already 25 µs per bytecode instruction and seconds for a bundle. Start-up and first interaction, which is much of what a UI framework does, would see none of it.
- **Not on iOS**, where no JIT runs and where Gable must ship.

### What it would cost

- **Size.** The baseline tier is about 9,000 lines of Rust in the JIT crate and the VM's glue, 2,500 lines of tests, two ADRs, a design document and ten tasks, for a tier that by design reuses the interpreter's frames and semantics. An optimizing tier is a larger and different thing: its own intermediate representation above Cranelift's or a much heavier use of it, call-target and arithmetic feedback the bytecode does not collect yet, inlining with frame states that rebuild several interpreter frames from one machine frame, lazy deoptimization when an assumption is invalidated under a running frame (ADR 0019), and on-stack replacement in both directions.
- **Soundness.** The baseline speculates about one thing, that a register holds numbers, and has one kind of side exit. The November 2026 review of it still found a wrong-result defect that exposed stale memory bytes to script (TASK-103.34), in the interaction of that one guess with exception handlers, and two latent ones. An optimizing tier guesses shapes, call targets, array kinds and types, removes checks on the strength of those guesses, and keeps values in forms the collector and the debugger must be told about at every exit. Each is the class of defect that review was looking for. The engine's security model (`docs/security-model.md`) treats script as untrusted, so every such defect is a security defect.
- **Maintenance.** Every later change to the object model, the bytecode, the collector or the call path would have a third implementation to keep in step, where two already cost: the padding regression of TASK-101.3.3, the tiering regressions of TASK-100.36, and the review's findings each came from a change elsewhere meeting the JIT.
- **Memory and compile latency.** A compilation holds about 150 bytes for each byte of code while it runs (the findings document, "Tiered JIT follow-up"), and compiled functions average 11 to 35 KB each.

## Decision

- **No optimizing tier.** TASK-101.4 closes as no-go. The baseline JIT stays as it is: optional, off by default, the top tier.
- **No further tiering or code-generation tuning of the baseline for its own sake.** Its open items in ADR 0020 ("What this did not reach") stay open. The ones that are runtime work serve the interpreter too and are taken up as that, below.
- **Performance work goes where every platform and every tier gains**, in this order of expected return on Gable's workloads:
  1. **Property access the caches miss.** A cache for megamorphic sites keyed by shape and name, shared by the interpreter and compiled code. Out-of-line property access is 15 to 44% of Gable's time, and megamorphic `GetNamed` is 78% of what compiled signal-churn leaves machine code for.
  2. **The call path.** `Function.prototype.call` and `apply`, getters and setters, and `new` as frames pushed by the dispatch loop without a native frame between; cheaper frame set-up. Calls are 8 to 29% of the time, and these are also the calls compiled code cannot make itself.
  3. **Bytecode that says more per instruction.** A fused `typeof x === "..."`, and whatever the profiles show next. Dispatch is 21 to 48%.
  4. **The gap to QuickJS-NG.** On these same benches the interpreter still takes 1.46 times QuickJS-NG's time, an interpreter with no JIT at all. That gap is, by construction, interpreter and runtime work, and it is larger than what the baseline tier gives Gable.
  5. **Start-up and memory**, which a UI framework's users meet first: start-up is 3.8 times QuickJS-NG's on the micro-benchmark and peak memory 1.9 times on Gable's.
- These are directions, not tasks. Tasks are created when the work is scheduled.

## Consequences

- TASK-101's tiers end at the baseline. ADR 0019's section on an optimizing tier stays as the design to start from if this decision is reopened; nothing in the constraints J1 to J11 is relaxed, since the baseline tier depends on them.
- The frame-state and deoptimization machinery of TASK-101.3.1 keeps its one user, the number guess. It is not grown.
- Peak throughput on numeric code stays at the baseline's three to four times the interpreter. An embedder who needs more moves the kernel into a host function (ADR 0011).
- JIT-less platforms lose nothing by this decision, and gain from where the work goes instead.

## When to reopen

All of these, not any one:

1. The interpreter and runtime work above has been done or has stopped paying, so that Gable's forced profile shows most of the time in machine code on most benches, not on two of nine.
2. An embedder has a workload the baseline tier does not serve and a host function cannot, on a platform where a JIT may run.
3. The baseline tier has been run on by default somewhere for long enough that its own defect rate is known, with the differential fuzzing still finding nothing.
