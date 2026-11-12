# AgniJS engine performance findings

This is the investigation in TASK-100.14. It explains why the AgniJS engine is slower or heavier than the interim engine (QuickJS-NG) on the M9 benchmarks ([switch-over report](https://github.com/jcannon98188/gable/blob/main/docs/js-engine-switch-over.md)) and the micro-benchmarks (`crates/agnijs-js-bench`). It measures and explains only. Nothing in the engine changed. Each cause worth fixing has its own backlog ticket, listed in [the tickets](#the-tickets).

The investigation's numbers come from the integration tip `3ade2ef`, on an i9-9900KF under Windows 11, release builds. They are kept below as the historical baseline. [Re-measurement, 2026-11-11](#re-measurement-2026-11-11) has current numbers, taken after TASK-100.16 to TASK-100.35 and the baseline JIT.

## Re-measurement, 2026-11-11

This is a fresh head-to-head at `main` `8972adb`, using the same commands as the [method](#benchmarks). Nothing in the engine changed for it.

**Headline.** On the 20 micro-benchmarks that existed at `3ade2ef`, AgniJS is now faster than QuickJS-NG: the geomean time ratio is **0.92x**, down from 2.90x (the 2.87x quoted below is the checked-in baseline's figure; the per-workload ratios below give 2.90x). The Gable benchmarks are still slower but the gap has halved: **1.46x**, down from 2.86x. Peak RSS on the Gable set is **1.92x** QuickJS-NG's (geomean), down from 3.98x. With the baseline JIT on (`--features jit`, `AGNIJS_JIT=on`), the micro geomean is **0.81x**.

### Setup

- **Machine:** the same one as `3ade2ef`. Intel Core i9-9900KF at 3.60 GHz, 16 logical CPUs, 64 GiB, Windows 11 Pro 10.0.26200. It is a desktop with other applications running, not an isolated host. rustc 1.99.0 and Node v24.21.0.
- **Build:** release profile (`debug = 1`, no LTO). The bench crate turns on `linear-regex` and `intl` for `agnijs-js-vm`. The JIT build adds `--features jit` and uses a separate target directory.
- **Micro-benchmarks:** `bench run --engine agnijs --engine quickjs --iterations 5 --warmup 1`, run three times back to back. The JIT build was run twice.
- **Gable benchmarks:** `run-bench.mjs --engine agnijs --engine quickjs --iterations 5 --warmup 1`, run three times. The harness came from Gable `7cd58eb`, copied out of the tree, with its pinned git dependency replaced by path dependencies on this commit's crates. The bench sources (`bench/*.bench.ts` and `packages/ui`) have not changed since `3ade2ef`. The harness change since then is only the rename from `gable` to `agnijs`.
- **How the figures are taken:** ms is the median of each run's per-pair median. The ratio is the median of the per-run ratios, and the range is the lowest and highest per-run ratio. Peak RSS is the lowest of the runs; see the RSS caveat below.

### Micro-benchmarks (interpreter)

| workload | agnijs ms | quickjs ms | ratio | ratio range | agnijs peak RSS MiB | quickjs peak RSS MiB | ratio at 3ade2ef | speed-up vs QuickJS since |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| loop-arith | 103.4 | 131.0 | 0.79 | 0.78 to 0.80 | 6.4 | 5.4 | 3.92 | 5.0x |
| loop-array-index | 95.0 | 85.4 | 1.11 | 1.09 to 1.17 | 6.5 | 5.5 | 6.73 | 6.1x |
| prop-monomorphic | 80.4 | 72.4 | 1.10 | 1.08 to 1.11 | 6.9 | 5.6 | 6.42 | 5.8x |
| prop-polymorphic | 125.8 | 88.5 | 1.42 | 1.39 to 1.45 | 6.9 | 5.7 | 6.93 | 4.9x |
| prop-write-alloc | 78.2 | 160.9 | 0.48 | 0.47 to 0.49 | 9.7 | 5.5 | 2.13 | 4.4x |
| string-concat | 62.9 | 82.0 | 0.75 | 0.74 to 0.77 | 9.8 | 7.4 | 1.74 | 2.3x |
| string-methods | 32.7 | 35.5 | 0.91 | 0.90 to 0.92 | 9.6 | 5.6 | 1.71 | 1.9x |
| array-methods | 317.9 | 148.5 | 2.16 | 2.12 to 2.18 | 9.0 | 5.6 | 4.12 | 1.9x |
| map-set | 122.7 | 1030.9 | 0.12 | 0.12 to 0.12 | 14.9 | 6.0 | 0.22 | 1.8x |
| closures | 61.8 | 72.6 | 0.85 | 0.85 to 0.85 | 10.2 | 5.5 | 2.98 | 3.5x |
| calls-recursive | 54.8 | 33.7 | 1.63 | 1.60 to 1.70 | 6.4 | 5.6 | 5.21 | 3.2x |
| method-calls | 117.7 | 74.9 | 1.57 | 1.57 to 1.64 | 6.6 | 5.5 | 4.87 | 3.1x |
| regex | 101.1 | 135.7 | 0.75 | 0.72 to 0.77 | 10.3 | 5.6 | 2.56 | 3.4x |
| regex-tokenize | 29.7 | 21.5 | 1.37 | 1.37 to 1.39 | 10.3 | 5.6 | - | new |
| regex-long-input | 1.74 | 3.06 | 0.57 | 0.56 to 0.58 | 8.3 | 6.3 | - | new |
| regex-redos | 0.01 | 96.2 | 0.0001 | - | 6.6 | 5.5 | - | new |
| json-stringify | 48.1 | 141.4 | 0.34 | 0.34 to 0.35 | 10.2 | 6.4 | 1.22 | 3.6x |
| json-parse | 44.7 | 84.7 | 0.53 | 0.52 to 0.53 | 15.0 | 7.9 | 1.53 | 2.9x |
| promise-chain | 15.7 | 22.4 | 0.70 | 0.69 to 0.74 | 59.9 | 22.4 | 2.80 | 4.0x |
| async-await | 15.0 | 27.2 | 0.55 | 0.55 to 0.56 | 8.9 | 5.6 | 3.24 | 5.9x |
| compile-source | 13.2 | 8.82 | 1.50 | 1.49 to 1.50 | 25.9 | 8.6 | 2.30 | 1.5x |
| bytecode-load | 2.14 | 0.90 | 2.38 | 2.37 to 2.52 | 21.5 | 7.9 | 5.28 | 2.2x |
| engine-startup | 0.61 | 0.16 | 3.80 | 3.45 to 3.88 | 6.6 | 5.5 | 6.65 | 1.7x |
| bundle-cold | 2.64 | 1.00 | 2.65 | 2.64 to 2.83 | 14.1 | 7.9 | - | new |
| snapshot-startup | 0.59 | - | - | - | 6.7 | - | - | new, AgniJS only |
| bundle-snapshot | 2.09 | - | - | - | 13.4 | - | - | new, AgniJS only |

The last column is the old ratio divided by the new one. It measures relative progress, because QuickJS-NG's own times are mostly within a few percent of `3ade2ef` on this machine.

**Geomeans:**

- Time, the 20 workloads present at `3ade2ef`: **0.918x** (per run 0.917, 0.923, 0.919). At `3ade2ef` it was 2.90x.
- Time, all 24 paired workloads: 0.661x. This figure is dominated by regex-redos, a catastrophic-backtracking pattern that the linear-time matcher finishes in microseconds while QuickJS-NG takes 96 ms. Without regex-redos (23 workloads) it is 0.958x.
- Peak RSS, the 20 old workloads: **1.64x** QuickJS-NG's. The same workloads at `00e3b62` (the checked-in baseline in `crates/agnijs-js-bench/baselines`, measured with a clean floor) were 3.18x.

**Still worst:** engine-startup 3.80x, bundle-cold 2.65x, bytecode-load 2.38x, array-methods 2.16x, calls-recursive 1.63x, method-calls 1.57x, compile-source 1.50x and prop-polymorphic 1.42x. Startup, bytecode load and compile are fixed per-realm and front-end costs, and they moved least (1.5x to 2.2x). array-methods is the slowest steady-state workload, as before.

**Best:** regex-redos (by construction), map-set 0.12x, json-stringify 0.34x, prop-write-alloc 0.48x, json-parse 0.53x and async-await 0.55x.

**Moved most since `3ade2ef`:** loop-array-index 6.1x, async-await 5.9x, prop-monomorphic 5.8x, loop-arith 5.0x, prop-polymorphic 4.9x, prop-write-alloc 4.4x and promise-chain 4.0x. These are the dispatch, inline-cache, call and promise causes (findings 2 to 5 and 9b). Moved least: compile-source 1.5x, engine-startup 1.7x, map-set 1.8x, array-methods 1.9x and string-methods 1.9x.

### Micro-benchmarks with the baseline JIT

`--features jit`, `AGNIJS_JIT=on`, two runs. This is a different binary from the interpreter rows above, so the two tables are not one run with a switch flipped. The JIT is off by default and the Gable benchmarks were not run with it.

| workload | agnijs JIT ms | quickjs ms | ratio | interpreter ratio | agnijs JIT peak RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| loop-arith | 33.5 | 133.3 | 0.25 | 0.79 | 9.1 |
| loop-array-index | 52.0 | 87.7 | 0.59 | 1.11 | 10.9 |
| prop-monomorphic | 47.9 | 74.3 | 0.64 | 1.10 | 12.6 |
| prop-polymorphic | 68.2 | 89.9 | 0.76 | 1.42 | 12.7 |
| prop-write-alloc | 74.6 | 164.7 | 0.45 | 0.48 | 8.8 |
| string-concat | 65.6 | 84.2 | 0.78 | 0.75 | 10.0 |
| string-methods | 32.7 | 36.2 | 0.90 | 0.91 | 9.7 |
| array-methods | 326.0 | 152.0 | 2.15 | 2.16 | 9.0 |
| map-set | 122.3 | 1037.9 | 0.12 | 0.12 | 15.0 |
| closures | 60.8 | 72.8 | 0.84 | 0.85 | 10.2 |
| calls-recursive | 55.9 | 33.7 | 1.66 | 1.63 | 6.5 |
| method-calls | 115.6 | 74.8 | 1.55 | 1.57 | 6.6 |
| regex | 109.4 | 138.2 | 0.79 | 0.75 | 10.2 |
| regex-tokenize | 31.0 | 22.1 | 1.40 | 1.37 | 17.4 |
| regex-long-input | 1.65 | 3.50 | 0.47 | 0.57 | 8.2 |
| regex-redos | 0.01 | 99.5 | 0.0001 | 0.0001 | 6.7 |
| json-stringify | 50.8 | 142.9 | 0.36 | 0.34 | 10.3 |
| json-parse | 49.5 | 85.9 | 0.58 | 0.53 | 15.1 |
| promise-chain | 15.8 | 22.8 | 0.69 | 0.70 | 60.1 |
| async-await | 15.1 | 27.0 | 0.56 | 0.55 | 8.9 |
| compile-source | 13.8 | 9.02 | 1.53 | 1.50 | 26.0 |
| bytecode-load | 2.38 | 0.88 | 2.70 | 2.38 | 21.8 |
| engine-startup | 0.66 | 0.16 | 4.10 | 3.80 | 6.7 |
| bundle-cold | 2.85 | 1.00 | 2.86 | 2.65 | 14.3 |

Geomean time over the 20 old workloads: **0.806x** (per run 0.802, 0.809). Over the 23 paired workloads without regex-redos it is 0.852x. Peak RSS is 1.82x QuickJS-NG's, against 1.64x for the interpreter.

The JIT pays off only on the loop and property workloads: loop-arith is 3.1x faster than the interpreter, and loop-array-index, prop-monomorphic and prop-polymorphic are 1.7x to 1.9x faster. The call-heavy workloads (calls-recursive, method-calls) are unchanged, which suggests those paths do not reach compiled code. Startup, bytecode load and bundle-cold are 8 to 11% slower. The JIT build adds 3 to 6 MiB of peak RSS on the loop and property workloads.

### Gable benchmarks

| benchmark | agnijs ms | quickjs ms | ratio | ratio range | agnijs peak RSS MiB | quickjs peak RSS MiB | RSS ratio | ratio at 3ade2ef | RSS ratio at 3ade2ef | speed-up vs QuickJS since |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| rxjs-operators | 179.8 | 101.1 | 1.77 | 1.70 to 1.79 | 8.8 | 6.6 | 1.33 | 4.06 | 1.42 | 2.3x |
| rxjs-higher-order | 274.3 | 187.2 | 1.47 | 1.46 to 1.47 | 13.1 | 6.5 | 2.02 | 2.98 | 18.6 | 2.0x |
| rxjs-subject-fanout | 127.5 | 81.5 | 1.57 | 1.56 to 1.59 | 13.9 | 6.6 | 2.11 | 3.91 | 5.79 | 2.5x |
| signal-churn | 484.3 | 241.8 | 2.00 | 2.00 to 2.01 | 12.5 | 9.1 | 1.37 | 4.16 | 2.88 | 2.1x |
| signal-create | 90.8 | 66.5 | 1.36 | 1.35 to 1.40 | 16.7 | 7.9 | 2.11 | 2.92 | 5.78 | 2.1x |
| template-mount | 75.2 | 54.6 | 1.38 | 1.37 to 1.40 | 41.4 | 17.8 | 2.33 | 2.38 | 2.91 | 1.7x |
| template-update | 40.5 | 29.0 | 1.40 | 1.39 to 1.40 | 14.4 | 10.0 | 1.44 | 2.64 | 2.56 | 1.9x |
| list-diff | 872.5 | 811.9 | 1.07 | 1.07 to 1.09 | 123.8 | 40.9 | 3.03 | 1.69 | 5.80 | 1.6x |
| ui-bundle-startup | 101.0 | 76.1 | 1.34 | 1.29 to 1.34 | 41.5 | 19.3 | 2.15 | 2.07 | 2.26 | 1.6x |

**Geomeans:** time **1.462x** (per run 1.463, 1.455, 1.467), down from 2.86x. Peak RSS **1.92x**, down from 3.98x.

**Still worst (time):** signal-churn 2.00x, rxjs-operators 1.77x, rxjs-subject-fanout 1.57x, rxjs-higher-order 1.47x and template-update 1.40x. These are the call-dense, `Function.prototype.call`-heavy bundles, so calls and native-to-JS re-entry (finding 4) are still the likely gap. **Best:** list-diff 1.07x, ui-bundle-startup 1.34x, signal-create 1.36x and template-mount 1.38x.

**Still worst (memory):** list-diff 3.03x (124 against 41 MiB) is the only benchmark above TASK-100.35's 2.5x bound. Next are template-mount 2.33x, ui-bundle-startup 2.15x, rxjs-subject-fanout and signal-create 2.11x, and rxjs-higher-order 2.02x.

**Moved most since `3ade2ef`:** for time, rxjs-subject-fanout 2.5x, rxjs-operators 2.3x, signal-churn and signal-create 2.1x, and rxjs-higher-order 2.0x. For memory, rxjs-higher-order went from 124 to 13 MiB, list-diff from 238 to 124 MiB and signal-create from 46 to 17 MiB. Moved least: list-diff and ui-bundle-startup at 1.6x. ui-bundle-startup is dominated by the parser and compiler, which matches compile-source moving least in the micro set.

### Variance and what is uncertain

- **Run-to-run:** the geomeans moved by under 1% between runs: 0.917 to 0.923 micro, 1.455 to 1.467 Gable. Per-workload ratios mostly stayed within 3%. The exceptions are engine-startup (11%, a sub-millisecond pair), regex, bundle-cold, promise-chain and loop-array-index (7%), calls-recursive and bytecode-load (6%), and rxjs-operators (5%). The JIT runs moved by up to 15% on bytecode-load and under 7% elsewhere. Treat any single-workload change under about 5% as noise.
- **The micro RSS columns at `3ade2ef` are inflated.** The first `bench run` after a build reports every pair about 15 MiB higher, as the baselines README already notes: the floor was 20 MiB in this session's first run and 5.5 MiB in the next two. The historical micro table below shows QuickJS-NG at 20 MiB on every workload, so it was a first run, and its RSS is not comparable with the figures above. That is why the micro memory comparison uses the clean `00e3b62` baseline. The Gable harness did not show the effect: its RSS stayed within 1 MiB across runs.
- **Micro RSS is mostly floor.** About 5.5 MiB of every figure is the harness and both engines' static data. A 1.6x ratio on 9 against 5.6 MiB says little about heap efficiency. The Gable figures carry more weight.
- **One machine, not isolated.** No CPU pinning and no frequency locking, with other applications running. No other heavy job ran during the measurements, but background load cannot be ruled out. QuickJS-NG's times mostly match `3ade2ef` within about 3%, which suggests the machine state is comparable. The exceptions are list-diff (+3.4%) and ui-bundle-startup (+8%), so those two ratios may read slightly better than the engine alone would justify.
- **The workload set grew.** The new regex, snapshot and bundle workloads are excluded from the like-for-like geomeans. regex-redos makes any geomean that includes it meaningless.
- **The JIT figures are from two runs of a separate build**, with default thresholds. They are not tuned and not representative of Gable workloads, which were not run under the JIT.
- **The Gable harness ran against a patched copy**, with path dependencies on this commit instead of its pinned revision `fa65c08`. The Cargo.lock entries for third-party crates may differ slightly from Gable's.

## Summary

As measured at `3ade2ef`. Most of these causes have since been worked on; see the re-measurement above for where each workload stands now. The causes, ranked by measured impact:

1. **The collector never runs while JavaScript runs under the embedding.** Module bodies, host calls, promise jobs and every callback from a native function run inside a native frame. The collector is deferred there, so a long synchronous call keeps all of its garbage. This is the whole peak-memory gap: rxjs-higher-order peaks at 124 MiB against 6.7 MiB, list-diff at 238 MiB against 41 MiB, and a two-line reproduction at 901 MiB against 5.4 MiB.
2. **Interpreter dispatch costs more than the work.** `step` is an out-of-line call per instruction with a 1.2 KiB stack frame. `pc` and the accumulator live in the frame vector behind bounds checks, and the run loop tests four flags before every instruction. Together this is 35 to 55% of all samples on the Gable benchmarks and about half of loop-arith.
3. **Too many instructions.** `Star` and `Ldar` are 50 to 60% of everything executed. loop-arith runs 23 instructions per iteration of `s = (s + i * 7) % 1000003`. The cost of cause 2 is multiplied by this count.
4. **Calls are expensive.** Every call allocates a `Vec` for its arguments, builds a 160-byte frame and clones an `Rc`. A native that calls back into JavaScript starts a fresh run loop: `Function.prototype.call`, `apply`, `forEach`, `map`, `reduce`, `sort` comparators, getters and promise reactions. `Function.prototype.call` sits under 94 to 100% of the samples in every Gable benchmark, because bundled code dispatches through it.
5. **Property access misses the inline cache in common cases.** The cache is monomorphic and covers only own data properties of plain objects. Array `length`, every property of an array or string, polymorphic sites and accessors all take the slow path, and every miss re-interns the property name.
6. **The heap allocator is everywhere.** `RtlAllocateHeap` and `RtlFreeHeap` take 13 to 20% of the Gable benchmarks and 30 to 40% of the promise, JSON and regex micro-benchmarks. Objects hold separate `Vec`s, functions cost about five allocations, and string constants are allocated again every time they are loaded.
7. **Each collection has a high fixed cost.** One collection takes about 4 ms even when only 400 KB is live. Sweeping frees every dead object's `Vec`s one at a time, and the root scan walks every shape key.
8. **The collector undercounts the heap.** Only fixed object sizes count towards the next collection. Element, slot and dictionary growth do not, so array-methods ran zero collections with an 82 MB heap.
9. Smaller, separate causes: global variable reads, promise and `await` internals, the parser and compiler allocation pattern, realm creation, bytecode loading, built-in string, regex and JSON paths, and the anchored-regex scan.

Causes 2 to 5 are one interpreter-performance problem seen from four sides. The fixes are separate tickets because each one can be measured on its own, but the gain is multiplicative. Cause 1 is independent and is the only memory problem that matters.

## Method

### Benchmarks

- **Micro-benchmarks:** `target/release/bench.exe run --engine agnijs --engine quickjs --iterations 5 --warmup 1 --output micro.json`. At `3ade2ef` the geomean was 2.87x, the same as the checked-in baseline.
- **Gable benchmarks** (run from the Gable repository, which keeps the harness): `node native/engine-harness/js/run-bench.mjs --engine agnijs --engine quickjs --iterations 5 --warmup 1`. At `3ade2ef` the geomean was 2.86x, the same as M9's 2.89x.

### Sampling profiler

No profiler that works without admin rights ships with Windows, so this investigation added a small one: `tools/winprof`. It is not part of any workspace and CI does not build it. It starts the target, suspends each thread about once per millisecond, walks the stack with dbghelp and writes folded stacks with inline frames and source lines. `analyze.mjs` next to it prints self time, inclusive time and per-line tables.

```sh
cargo build --release --manifest-path tools/winprof/Cargo.toml
set WINPROF_SYMPATH=%CD%\native\engine\target\release\deps;%CD%\native\engine\target\release
native\tools\winprof\target\release\winprof.exe out.folded 1 native\engine\target\release\bench.exe child --engine agnijs --workload loop-arith --iterations 5 --warmup 1 --scale 1
node native\tools\winprof\analyze.mjs out.folded 40 [focus-substring] [--lines]
```

For the Gable benchmarks (in the Gable repository), profile `native/engine-harness/target/release/gable-harness.exe bench --engine agnijs --name <name> --warmup 1 --iterations 5 native/engine-harness/out/bench/<file>.bench.mjs` with `WINPROF_SYMPATH` pointing at the harness target. Both release profiles already have `debug = 1`, so symbols and line tables are present.

Percentages below are shares of the samples that touch engine code. Idle threads are excluded.

### Counters

A temporary, uncommitted build added counters to `agnijs-js-vm`:

- an opcode histogram;
- inline-cache hits and misses, broken down by cause;
- the number of collections and the time spent in them;
- collector checks that were deferred;
- frames pushed and native re-entries into the run loop;
- global loads and string-constant allocations.

The build was reverted before the commit. Counter figures are for one iteration with no warmup (`bench child gable <name> 1 0 1`).

### Reproductions

The minimal reproductions below are ES modules run with `gable-harness run --engine agnijs|quickjs <file>`, timed with `Date.now()`. Each one is repeated in the matching ticket.

## Results at a glance

### Micro-benchmarks

At `3ade2ef`. The "gable" columns are the AgniJS engine under its old name. The RSS columns come from a first run after a build and are about 15 MiB high (see the re-measurement caveats).

| workload | gable ms | quickjs ms | ratio | gable peak RSS MiB | quickjs peak RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| loop-arith | 527.5 | 134.6 | 3.92 | 6.8 | 20.2 |
| loop-array-index | 613.4 | 91.2 | 6.73 | 21.4 | 20.3 |
| prop-monomorphic | 470.8 | 73.4 | 6.42 | 21.9 | 20.3 |
| prop-polymorphic | 627.9 | 90.7 | 6.93 | 21.9 | 20.4 |
| prop-write-alloc | 347.9 | 163.5 | 2.13 | 35.5 | 20.2 |
| string-concat | 143.6 | 82.7 | 1.74 | 33.6 | 22.2 |
| string-methods | 61.6 | 36.1 | 1.71 | 34.8 | 20.4 |
| array-methods | 617.6 | 149.8 | 4.12 | 105.6 | 20.3 |
| map-set | 233.3 | 1051.3 | 0.22 | 45.9 | 20.7 |
| closures | 218.0 | 73.2 | 2.98 | 38.5 | 20.2 |
| calls-recursive | 179.1 | 34.4 | 5.21 | 21.3 | 20.3 |
| method-calls | 362.2 | 74.4 | 4.87 | 33.6 | 20.3 |
| regex | 354.8 | 138.8 | 2.56 | 39.2 | 20.3 |
| json-stringify | 173.3 | 141.7 | 1.22 | 31.9 | 21.1 |
| json-parse | 133.3 | 87.1 | 1.53 | 40.0 | 22.4 |
| promise-chain | 65.9 | 23.5 | 2.80 | 161.7 | 37.1 |
| async-await | 89.0 | 27.4 | 3.24 | 40.3 | 20.2 |
| compile-source | 20.7 | 9.0 | 2.30 | 44.1 | 23.4 |
| bytecode-load | 4.48 | 0.85 | 5.28 | 41.4 | 22.5 |
| engine-startup | 1.06 | 0.16 | 6.65 | 22.0 | 20.4 |

At `3ade2ef`, map-set was the only workload where the AgniJS engine was faster, by 4.5x. It is not investigated further. In the 2026-11-11 re-measurement, 13 of the 24 paired workloads are faster.

### Gable benchmarks

At `3ade2ef`.

| benchmark | gable ms | quickjs ms | ratio | gable peak RSS MiB | quickjs peak RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| rxjs-operators | 418.7 | 103.1 | 4.06 | 9.4 | 6.6 |
| rxjs-higher-order | 562.0 | 188.7 | 2.98 | 124.4 | 6.7 |
| rxjs-subject-fanout | 310.8 | 79.6 | 3.91 | 38.2 | 6.6 |
| signal-churn | 976.7 | 235.0 | 4.16 | 26.2 | 9.1 |
| signal-create | 190.4 | 65.2 | 2.92 | 46.2 | 8.0 |
| template-mount | 124.8 | 52.5 | 2.38 | 51.8 | 17.8 |
| template-update | 76.4 | 28.9 | 2.64 | 25.9 | 10.1 |
| list-diff | 1324.7 | 785.4 | 1.69 | 237.7 | 41.0 |
| ui-bundle-startup | 145.9 | 70.5 | 2.07 | 44.0 | 19.5 |

### Where the time goes in the Gable benchmarks

Self time in the interpreter (`Vm::step` plus `Vm::run`) and in the system allocator, with inclusive time for the call machinery:

| benchmark | step + run self | allocator self | push_function_frame incl. | name_key incl. | collect incl. |
| --- | ---: | ---: | ---: | ---: | ---: |
| signal-churn | 55.1% | 7.6% | 8.8% | 5.3% | <1% |
| rxjs-operators | 46.5% | 12.7% | 13.1% | 3.9% | <1% |
| rxjs-subject-fanout | 43.2% | 12.4% | 10.6% | 4.2% | 1.9% |
| signal-create | 44.8% | 14.2% | 7.4% | 2.3% | 4.4% |
| rxjs-higher-order | 35.4% | 16.2% | 7.2% | 4.3% | 5.4% |
| template-mount | 33.6% | 15.8% | 4.2% | 3.2% | 13.2% |
| list-diff | 26.4% | 13.4% | 3.0% | 4.5% | 9.9% |
| ui-bundle-startup | 2.6% | 32.3% | - | - | - |

ui-bundle-startup is dominated by the front end. Compiling the module graph is 74% of the time: the parser is 38% and the compiler 26%.

## Findings, ranked

Impact is the measured share of time or memory the cause is responsible for on the benchmarks where it shows. Effort is S (a day or so), M (a few days) or L (a week or more). Risk is the chance of a behaviour or soundness regression. test262 and the Gable suites are the oracle for every fix.

### 1. The collector is deferred for all embedded execution (memory)

**Evidence.** `Vm::maybe_collect` returns at once when `native_depth != 0` (`gc.rs`), and `in_native` raises `native_depth` around every native function and every host entry. The following all run under at least one native frame:

- the harness's `engine::call`;
- module evaluation (`execute_module` goes through `call_value`);
- promise jobs;
- every native that calls back, such as `Function.prototype.call` and `Array.prototype.map`.

The engine collects only between jobs (`promise.rs:493`) and between benchmark iterations (`engine.rs`). It never collects inside them. The profiles agree: in list-diff, template-mount and rxjs-higher-order, every `Vm::collect` sample comes from the job loop or the iteration boundary, none from the run loop.

Reproduction (`gc-top.mjs`):

```js
function work() { let keep = 0; for (let i = 0; i < 2000000; i++) { const o = { a: i, b: [i, i + 1] }; keep += o.b.length; } return keep; }
console.log(work());
```

| | gable peak RSS | quickjs peak RSS |
| --- | ---: | ---: |
| `work()` at module top level | 900.7 MiB | 5.4 MiB |
| `work()` inside `[0].forEach(...)` | 900.8 MiB | 5.4 MiB |

The micro-benchmarks do not show this, because they run as classic scripts, where `native_depth` is 0.

There is a secondary time cost. Once `allocated_since_gc` passes the threshold inside a deferred region, every instruction takes the run loop's slow branch: `maybe_collect` returns false, the threshold stays exceeded, and the memory-limit test runs before each instruction.

**Fix direction.** Let the collector run at interpreter safe points while native frames are on the stack. Values that natives hold must then be rooted: a handle or root stack that natives push onto, or natives that keep their temporaries in VM-visible slots. `Function.prototype.call` and `apply`, and the host and module entry points, can avoid a native frame entirely (see finding 4), which removes the deferral from the most common paths before general rooting lands.

**Impact.** All of the peak-memory gap in the Gable set: 124 MiB against 6.7 MiB, 238 against 41, 52 against 18, 46 against 8. It also limits how much memory any long synchronous call can use. **Effort L, risk high** (a missed root is a use-after-free, so the stress-GC mode must run over test262 and the Gable suites). TASK-100.16.

**Done (TASK-100.16).** The collector now runs at safe points under native frames. Rooting is by construction rather than per site: every arena slot records a birth serial, a run loop entered under a native frame records the serial at entry, and a collection under it treats everything born before the innermost such bound as a root. Rust code can only hold values that existed when it entered the loop, so no native needs a handle stack and the collector stays precise, non-moving and free of stack maps (ADR 0009 addendum, ADR 0019 J6). Three pacing measures keep the over-approximation cheap: retained bytes are left out of the next threshold, a loop that keeps retaining puts its collections off, and a loop exit asks for one collection at or above its own level (`Vm::exit_gc_at`). The array and collection iteration natives (`forEach`, `map`, `filter`, `some`, `every`, `find*`, `Map`/`Set.prototype.forEach`) opt out through `Vm::callback_series`: they root what they hold on `Vm::roots` and their callbacks retain only from the enclosing bound, so the garbage of earlier callbacks is collected during later ones. `AGNIJS_GC_TRACE=1` prints one line per collection. Reproduction peak working set: 781 MiB to 18 MiB at module top level, 781 to 18 inside `forEach` (QuickJS-NG 5.4). Gable benches: rxjs-higher-order 106 to 35 MiB, list-diff 228 to 165, signal-create 40 to 30, rxjs-subject-fanout 28 to 19; the remaining gap to QuickJS-NG is the 8 MiB default threshold plus the live set's growth factor and, for list-diff, garbage born before the host call that the depth-1 loop still retains. Times are within the run-to-run band except list-diff (0.81 to 0.88 s, +8%) and template-mount (69 to 76 ms, +10%): both now pay for collections inside the timed call where the pre-change engine paid for none (it collected between iterations, outside the timing). Gating the exit collection on a threshold's worth of retained bytes removed template-mount's extra collection but let the retention compound across host calls (signal-create 29 to 41 MiB, rxjs-subject-fanout 20 to 32, list-diff 182 to 244), so it was reverted: a loop's retained garbage is retained again by every later loop at that level, and only a collection at an outer level frees it. The stress mode now reaches code inside callbacks, jobs and host calls; its first run over the Gable suites found a pre-existing collector bug (a WeakMap value that was a string or a bigint was never marked through its key), fixed in the same task.

**Done (TASK-100.35, the remaining gap).** Two pacing changes. First, the free safe points, where no native frame is on the stack (a host call's entry, the boundary between jobs), collect once half the threshold has been allocated (`Heap::wants_free_gc`) instead of waiting for the whole threshold: garbage left since the last collection was otherwise kept by every nested run loop entered after it, and list-diff kept 17 to 75 MB of pre-flush garbage through each flush job. That alone took rxjs-higher-order from 34.9 to 21.3 MiB and list-diff from 164.7 to 130.7, at no time cost (rxjs-higher-order 290 to 261 ms, the rest within the band). Second, the threshold floor is 2 MiB instead of 8: a collection with a small live set marks in about 0.2 ms, so four times as many of them cost nothing measurable (micro-benchmark geomean 0.988 against the 8 MiB floor, no workload outside the run-to-run band), while every bench with a live set under 1 MB lost half or more of its peak: rxjs-higher-order 13.3 MiB (QuickJS-NG 6.7), rxjs-subject-fanout 13.8 (6.6), signal-create 18.7 (8.0), the reproduction 9.0 (5.3, was 18.2), prop-write-alloc 16.3 to 9.6, map-set 37.1 to 14.8, closures 20.3 to 10.0. The growth factor stays at 2x: 1.5x bought list-diff 6% of memory (132.6 to 124.5 MiB) for 3% of time, and its gap is not the factor. list-diff still peaks at about 3x QuickJS-NG (131 against 42 MiB): its live set is 20 MB, so its threshold is 41 MB, each in-series collection still retains up to half a threshold of the jobs' garbage from before the flush, and the resident set is about 1.6x the counted bytes. Closing that needs either a cheaper way to free pre-loop garbage inside a series (a collection at the series' own bound rather than the enclosing loop's) or a lower factor for large heaps, both left open. `AGNIJS_GC_THRESHOLD` (bytes) and `AGNIJS_GC_GROWTH` (percent) override the floor and the factor for experiments.

### 2. Dispatch overhead

**Evidence.** Disassembling `Vm::step` in `bench.exe` shows a prologue that pushes eight registers, reserves `0x4D8` bytes of stack, saves `xmm6` and sets up an exception-handling state on every call. `Instr` is passed by pointer. Then comes the jump table.

In loop-arith, the `step` prologue and epilogue (`step.rs:48` and `:1258`) take about 22% of the samples. The run loop around it (`interp/mod.rs:97-131`) takes about 25%. The run loop does the following per instruction:

- tests `heap.stress`, `gc_requested`, `allocated_since_gc >= gc_threshold` and `debug_hot`;
- loads `self.frames[fi].pc` (bounds-checked);
- copies the 12-byte `Instr`;
- stores `pc + 1`;
- matches on the returned `Flow`.

Inside `step`, the accumulator is `self.frames[fi].acc` and registers are `self.regs[base + r]`, each bounds-checked. `Flow::Reload` clones the `Rc<CodeBlock>` on every call and return.

`Vm::step` plus `Vm::run` self time is 26 to 55% of every Gable benchmark (table above). Part of that is the instructions' real work. The prologue, epilogue and loop share measured in loop-arith is about half of the total.

**Fix direction.**

- Keep `pc`, `acc`, `base` and the register-window pointer in locals of a single dispatch function. Write them back only at calls, returns, throws and safe points.
- Inline the opcode bodies into the loop and move the rare opcodes out of line.
- Fold the four flags into one "attention" word that is set when any of them changes.
- Avoid bounds checks on register access, with a verified frame size per `CodeBlock`. The bytecode verifier already exists for deserialized code.

**Impact.** Dispatch-bound code (all of the Gable set except ui-bundle-startup, and the loop, prop and call micro-benchmarks) should gain 1.3x to 2x. **Effort M to L, risk medium**: it touches every opcode, but test262 is a strong oracle. TASK-100.17.

### 3. Bytecode shape: accumulator moves dominate

**Evidence.** The opcode histogram:

| workload | instructions | Star | Ldar | per iteration |
| --- | ---: | ---: | ---: | --- |
| loop-arith | 69.0 M | 30.4% | 26.1% | 23 per loop iteration |
| prop-monomorphic | 35.1 M | 37.1% | 20.0% | 35 per inner iteration |
| calls-recursive | 13.1 M | 32.5% | 17.5% | 20 per `fib` call |

loop-arith's body, `s = (s + i * 7) % 1000003; i++`, needs about eight register-machine operations. The compiler also emits `ToNumeric` for a postfix `i++` whose value is unused, which is 4.3% of the loop's instructions.

**Fix direction.**

- Register-operand forms for the hot binary operators and comparisons (`Add r1, r2`, or with an immediate).
- Fused compare-and-branch.
- Drop `ToNumeric` when the old value of a postfix update is unused.
- A peephole pass that removes `Star rX; Ldar rX` pairs and dead moves.
- `Instr` should stay small. It is 12 bytes now.

**Impact.** Instruction count falls by 40 to 50% on loops and calls. Combined with finding 2, that is the bulk of the CPU gap. **Effort M to L, risk medium**: the compiler changes, and serialized bytecode needs a format tag bump under ADR 0010. TASK-100.18.

### 4. Call overhead and native re-entry

**Evidence.**

- `args_vec!` (`step.rs:84`) copies the arguments into a new `Vec<Value>` for every `Call`, `CallMethod` and `Construct`. In rxjs-operators, 107 of the 115 allocation samples under the interpreter come from that line (`calls.rs:332`, from `step.rs:84`). In calls-recursive, `RtlAllocateHeap` plus `RtlFreeHeap` are about 9%.
- `push_function_frame` is 7 to 13% inclusive across the Gable set. It clones the code `Rc`, builds a 160-byte `Frame`, resizes `regs` and copies the parameters.
- A native that calls JavaScript goes `call_object` → `run_function` → `in_native` → a new `Vm::run` loop. array-methods makes 2.4 M such re-entries, and method-calls makes 300 K, one per getter call.
- `Function.prototype.call` (`proto_call`) is on 94 to 100% of the stacks in every Gable benchmark, because bundled class and RxJS code calls through it. So all of that code runs in nested run loops under a native frame, which also feeds finding 1.

Reproduction (1 M iterations each, module code):

| | gable ms | quickjs ms |
| --- | ---: | ---: |
| empty loop | 129 | 37 |
| `f(s)` | 276 | 63 |
| `f.call(null, s)` | 361 | 96 |
| `f.apply(null, [s])` | 548 | 197 |
| `Math.abs(1)` | 286 | 80 |
| `arr.forEach` (1 M callbacks) | 244 | 45 |
| getter `p.x` | 351 | 89 |

**Fix direction.**

- Pass arguments as a slice of the caller's register window, which is what `call_from_frame` needs anyway.
- Make frame setup cheaper: no `Rc` clone, pre-sized registers.
- Let `Function.prototype.call` and `apply` push the target's frame directly in the interpreter instead of nesting a run loop.
- Give natives a way to call JavaScript functions that reuses the current run loop where they can, for array iteration methods, getters and setters.

**Impact.** 10 to 25% on call-heavy Gable benchmarks, more on calls-recursive, method-calls and array-methods. The `call`/`apply` part also removes most native frames from the Gable stacks. **Effort M, risk medium**: re-entrancy, exceptions and generator frames need care. TASK-100.19.

### 5. Inline-cache coverage

**Evidence.** Inline-cache counters, one iteration:

| workload | GetNamed hits | misses | miss causes |
| --- | ---: | ---: | --- |
| prop-monomorphic | 2.0 M | 1.0 M | all 1.0 M are `arr.length` |
| prop-polymorphic | 0 | 3.0 M | 2.0 M shape changes, 1.0 M arrays |
| loop-array-index | 0 | 1.5 M | all arrays |
| method-calls | 0.75 M | 0.90 M | 0.30 M shape, 0.30 M accessor, 0.30 M array |
| array-methods | 0 | 0.30 M | all arrays |

The inline cache (`interp/ic.rs`) is monomorphic, and `is_plain` excludes arrays, strings, arguments objects, typed arrays, namespaces, proxies and dictionary-mode objects. That is why "inline caches are not helping": they work for plain own properties, but the time goes to arrays, polymorphic sites, accessors and globals (finding 9a).

Every miss also calls `name_key`, which re-interns the property name: a string hash plus an intern-table lookup, and a clone of `code.names[i]`. That costs 2 to 5% inclusive across the Gable set and 12 to 14% (as `Heap::atom`) in prop-polymorphic.

`GetKeyed` has no fast path for an array with an in-range integer index. It goes through `to_property_key`, `get_value`, `get_prop`, `exotic_of` and `own_property`.

Reproduction (1 M reads): one shape, 261 ms; four shapes, 301 ms; `arr.length`, 217 ms; a getter, 351 ms. QuickJS takes 42 to 89 ms for all of them.

**Fix direction.**

- Intern each `CodeBlock`'s names once, at load or compile time, so a miss never re-hashes.
- Polymorphic entries (four shapes).
- An array `length` case and an array or string prototype-method case.
- Accessor entries that call the getter directly (with finding 4).
- A dense-array fast path for `GetKeyed` and `SetKeyed`.

**Impact.** prop-polymorphic, loop-array-index and prop-monomorphic are the worst micro ratios (6.4x to 6.9x). On the Gable set, the name re-interning alone is 2 to 5%. **Effort M, risk low to medium**: invalidation on shape and prototype change has to stay right. TASK-100.20.

### 6. Allocation weight of objects, functions and strings

**Evidence.**

- Every object is an `ObjectData` (120 bytes) with a separate `slots: Vec<PropSlot>` (32 bytes per slot) and a separate elements `Vec`.
- `create_function` allocates the object, a `Box<FunctionData>`, the slots `Vec`, the `length` and `name` properties, a name string, and for constructors a prototype object. It is 28% of closures, 6.6% of rxjs-higher-order and 4.2% of template-mount.
- `LdaConst` of a string constant calls `alloc_str_utf16` on every execution. Counts per iteration: string-concat 400 K, prop-write-alloc 300 K, map-set 200 K, method-calls 150 K. A loop that only reads `"abc".length` takes 284 ms against 53 ms.
- System allocator self time is 30 to 40% in promise-chain, async-await, json-parse and regex, 27% in closures, and 13 to 20% in the Gable set.

Reproduction (1 M each): closure creation 456 ms against 234 ms; an object literal `{ a, b }` 389 ms against 189 ms.

**Fix direction.**

- Inline the first few slots in `ObjectData`.
- Create function `prototype` objects lazily, and `length` and `name` as shape-described lazy properties.
- Intern string constants once per `CodeBlock` as GC-rooted heap strings.
- Decide whether a faster allocator in the host binaries (`gable-harness`, `bench`) is worth it. That choice belongs to the embedder, not to the engine crates. Measure it with the same benchmark pair.

**Impact.** 10 to 30% on allocation-heavy workloads, and it lowers sweep cost (finding 7). **Effort M to L, risk medium**: the object layout change touches the heap, GC tracing and the debugger. TASK-100.21.

**Done (TASK-100.21).** The first four named slots live inline in a `#[repr(C)]` object (still 160 bytes), a function is two heap blocks (precomputed rooted shapes for its eager properties, `FunctionData` boxes recycled by the sweep), string constants and function names survive collections with their block, and a property add that misses is cached as a shape transition. closures went from 92 to 63 ms (1.45x) and prop-write-alloc from 126 to 76 ms (1.65x), with lower peak memory on both and on most of the Gable set. rxjs-operators is about 6% slower, which profiling attributes to dispatch-loop stalls rather than to any changed path.

### 7. Fixed cost per collection

**Evidence.**

| workload | collections | time in GC | share of the run | live after GC |
| --- | ---: | ---: | ---: | ---: |
| prop-write-alloc | 12 | 47.8 ms | 14% | about 0.4 MB |
| closures | 7 | 36 ms | 17% | about 0.4 MB |
| string-concat | 6 | 23 ms | 16% | about 0.5 MB |
| promise-chain | 5 (6 iterations) | 109 ms | about 28% | 22.8 MB |

About 4 ms per collection with only 400 KB live is high. Sweep time is 60 to 80% `RtlFreeHeap`, because each dead `ObjectData` drops its own `Vec`s, plus `NtFreeVirtualMemory`. The root scan visits every key of every shape. In the Gable set, `collect` is 9.9% of list-diff and 13.2% of template-mount, and that is with collections only at job boundaries.

**Fix direction.**

- Make the sweep cheaper with fewer per-object heap blocks (finding 6) and by reusing freed slots' buffers.
- Trace shapes from live objects instead of treating all shape keys as roots.
- Consider lazy sweeping.

Once finding 1 lets the collector run during execution, collections become more frequent, so this cost matters more. **Effort M, risk medium.** TASK-100.22.

**Done (TASK-100.22).** Shapes are traced, marking writes mark bits directly and records weak containers, and the sweep recycles small buffers and totals live bytes. A prop-write-alloc collection (394 KB live) went from about 2 ms to 0.6 ms; time in `Vm::collect` fell 46% on list-diff and 45% on template-mount. Lazy sweeping was not needed.

### 8. Heap accounting ignores growth

**Evidence.** Only fixed sizes count towards `allocated_since_gc`: 160 bytes per object, 64 per environment, 40 plus the length per string. Growth of elements, slots and dictionaries is never counted. array-methods ran zero collections over six iterations while `heap_bytes` reached 81.7 MB across only 21 K objects, with a peak RSS of 105.6 MiB against 20.3 MiB. The memory limit in the run loop uses the same counters, so it undercounts too.

Reproduction: 20 000 arrays of 1 000 pushed elements peak at 323 MiB against 5.6 MiB. In a module, finding 1 also contributes.

**Fix direction.** Count the bytes of `Vec` growth for elements, slots, dictionaries and string buffers as they happen, and keep `gc_threshold = max(2 x live, 8 MiB)`. Once growth is counted, that threshold is reasonable; "the collector is too lazy" was the accounting. **Effort S, risk low.** TASK-100.23.

### 9. Smaller causes, each with its own ticket

**a. Global variable reads.** `load_global` does three hash lookups per read:

1. `global_lexicals` (an `IndexMap<Rc<str>>` with SipHash);
2. `self.key(n)`, which interns the name;
3. `get_own_property` on the global object, which is in dictionary mode (another SipHash `IndexMap`).

It is 16 to 26% inclusive in prop-monomorphic, loop-array-index, prop-polymorphic, method-calls and calls-recursive (`fib` is a global, 656 K loads). It is under 2% in the Gable set, because bundles use module scope. A global read takes 176 ms against 42 ms per 1 M reads.

Fix: a per-site global cache keyed by a global-object version or property cell. **Effort S to M, risk low.** TASK-100.24.

**b. Promise and `await` internals.**

- `new_promise_capability` and `new_native_closure` create real function objects for resolving functions and `await` reactions that script never sees. These are 28.6% of async-await.
- GC is 32 to 34% of async-await and the allocator about 40%.
- Each pending promise holds about 1.1 KB live, and promise-chain peaks at 161.7 MiB against 37.1 MiB.

Fix: internal reaction records instead of closures where the function is unobservable, and a smaller promise layout. **Effort M, risk medium**: the ordering of jobs must stay exactly per spec. TASK-100.25.

**Done (TASK-100.25).** A capability for `%Promise%` that script cannot see (from `then`, `Promise.resolve`, `Promise.reject`, `Promise.try`, async functions and async generators) is internal: it has no resolving functions, and the engine settles it once through the same resolve steps. `await` and the async generator's return step queue reactions whose handler is a record (the suspended body and its realm), not a native closure, and awaiting a primitive skips the wrapper promise. A promise keeps one record per `then` and holds its first one inline. Resolving functions handed to script (the `Promise` constructor, `withResolvers`, thenables, subclasses, the combinators) are unchanged. promise-chain went from 38.4 to 18.3 ms and from 134 to 57 MiB peak; async-await from 58.2 to 16.0 ms; the 100 000-`then` module reproduction from 243 to 104 ms (QuickJS 198 ms).

**c. Parser and compiler allocation.** compile-source is 2.30x and ui-bundle-startup 2.07x. In compile-source, `RtlAllocateHeap` plus `RtlFreeHeap` are 36% self, and in ui-bundle-startup 32%. That comes from boxed AST nodes, `Rc<str>` names and SipHash `HashMap`s in scope analysis.

Fix: an arena or index-based AST, interned identifiers, and a faster hasher for compiler-internal maps. **Effort M to L, risk low**: this is the front end only. TASK-100.26.

**Done (TASK-100.26).** Most of the allocation was in the lexer, not the AST. Tokens now borrow from the source: an identifier without escapes is a slice, decimal literals and plain strings are scanned in place, and an escape no longer allocates a `Vec`. The parser interns names in one set, and the parser and compiler maps use foldhash instead of SipHash. The compiler reuses its instruction, offset and statement buffers across functions and copies each one out at exact length; static property keys are borrowed. An arena AST was not needed. The bytecode is unchanged: every test262 file and the Gable bench bundles, compiled as both script and module, produce identical code trees, serialised bytes and error positions. compile-source went from 19.7 to 12.0 ms (1.65x), and ui-bundle-startup from 136.4 to 99.2 ms (1.37x; peak RSS 43.6 to 41.7 MiB). Parsing and compiling the @gable/ui bundle went from 866 000 to 379 000 allocations. With the allocations gone, fat LTO with one codegen unit is worth only about 2% on compile-source, so the release profile is unchanged.

**d. Realm creation.** engine-startup is 1.06 ms against 0.16 ms. `Heap::shape_add` is 35% inclusive. Each new transition clones the parent's whole key list and rebuilds a `HashMap` index of every key, so building an object with n properties is O(n²) in time and allocation. Built-in prototypes add dozens of methods each. The other big costs are `RtlFreeHeap` at teardown (23%) and `new_native_in` (23%). TASK-101.2 (realm snapshots) is the long-term answer.

The quadratic shapes are a separate, cheap fix: share key storage with the parent, and build the index lazily or only above eight keys, since `lookup` already searches linearly up to eight. Object literals with many keys also benefit. **Effort S, risk low.** TASK-100.27.

**e. Bytecode loading, including the lazy debug info.** bytecode-load is 5.28x (4.48 ms against 0.85 ms). `decode_bytecode` is 66% of it.

- `(0..n).map(..).collect::<Result<Vec<_>>>()` cannot reserve capacity, because the `Result` adapter reports a lower bound of 0. Vec growth and `RtlReAllocateHeap` alone are 15%.
- Decoding the M8 debug info (`get_debug_info` plus `verify_debug_info`) is 3.4%, which matches the 4 to 12% slowdown TASK-100.9 measured.

Fix: decode with known capacities, decode debug info lazily on first debugger or stack-trace use (the lazy decode noted in ADR 0010), and verify in one pass. **Effort S, risk low.** TASK-100.28.

**f. Built-in allocation (regex, string methods, JSON).**

- In regex, `exec` and `replace` allocate the match array, the capture strings and the UTF-16 copies (`RtlAllocateHeap` 22.8% self). regress itself (`try_at_pos`) is about 2%.
- In string-methods, `split` and `toLowerCase` go through char-iterator case mapping and re-encode with `alloc_str_utf16`.
- In json-stringify, number formatting (grisu plus `to_js_string`) is 22%.
- json-parse grows `Vec`s without reserving.

**Effort M, risk low**: test262 covers these built-ins well. TASK-100.29.

**Done (TASK-100.29).** The built-ins copied strings to UTF-16, built intermediate `Vec`s, formatted with `format!` and materialised a match array per `exec`. Now:

- Strings are built in a one-byte or two-byte `StrBuilder` that widens on demand, and slices, split parts, trims and captures are substrings of the original.
- Searching compares one-byte and two-byte units in place. Case mapping has an ASCII and a Latin-1 path and returns the receiver when nothing changes.
- The regex cache is a hash table keyed by the pattern's units, so a hit allocates nothing. Two-byte input is matched in place and short one-byte input is widened into a reused buffer.
- `@@match`, `@@replace`, `@@search` and `@@split` call the built-in `exec` without building its result array when `exec` is unmodified. A `$` template is expanded straight into the output.
- JSON parses the source in its own width without copying it, stringifies into one builder, and formats numbers into a stack buffer.

regex went from 291.3 to 155.9 ms (1.87x), string-methods from 56.5 to 32.1 ms (1.76x), json-parse from 113.3 to 44.5 ms (2.55x) and json-stringify from 175.7 to 47.0 ms (3.74x). No other micro or Gable benchmark got more than about 3% slower; ui-bundle-startup got 5% faster. Allocations per call: `exec` with two captures went from 13 to 7, a global `replace` from 85 to 22, `JSON.parse` of a small document from 34 to 8, and `JSON.stringify` from 50 to 9 (`crates/agnijs-js-bench/tests/builtin_allocs.rs`).

Splitting by a regex still costs about 155 allocations for 3 parts, because regress sets up again at each position; a sticky-free scan is a follow-up. The rewrite also fixed a panic: `/(?:)/gu` with `lastIndex` inside a surrogate pair sliced backwards.

**g. A failed anchored regex scans the whole input.**

```js
const s = "a".repeat(1e6), re = /^x/;
for (let i = 0; i < 200; i++) re.exec(s);
```

The failed `exec` is linear in the input. On the AgniJS engine it costs 90 ms per 200 calls at n = 1e5, 903 ms at 1e6 and 3.6 s at 4e6, about 4.5 ns per character. QuickJS takes 2.3 times as long and Node takes constant time. `agnijs_js_regex::Regex::exec` calls `find_from_utf16(input, start)`, which tries every start position.

Fix: when every alternative of a non-multiline pattern starts with `^`, try only the start position (and when `lastIndex > 0` without the `m` flag, fail at once). **Effort S, risk low.** TASK-100.30.

## The build profile

The engine's release profile (`Cargo.toml`) sets `debug = 1` and nothing else: no LTO, and the default 16 codegen units. That is not a cause. A build with `lto = "fat"` and `codegen-units = 1` was measured on eight workloads:

| workload | default ms | fat LTO, 1 CGU ms |
| --- | ---: | ---: |
| loop-arith | 525.1 | 503.6 |
| prop-monomorphic | 459.6 | 471.2 |
| closures | 222.6 | 216.1 |
| calls-recursive | 181.6 | 186.3 |
| method-calls | 367.2 | 365.1 |
| json-parse | 137.0 | 130.0 |
| promise-chain | 64.3 | 64.3 |
| compile-source | 24.2 | 20.5 |

Everything is within noise (plus or minus 5%) except compile-source, at about 15%. The interpreter's costs are structural, not missed inlining across crates. It is skipped. A front-end ticket (9c) can turn LTO on if it confirms the compile-source gain.

## Skipped

These were measured and judged too small to ticket on their own. Most disappear inside the fixes above.

- `Vm::intrinsic` appears in profiles (3 to 4% inclusive in the Gable set), but it is a trivial accessor. The samples belong to the inlined code around it.
- `to_boolean`, `Shape::lookup`, `Units::hash_str`, `is_plain` and `Arena::get` bounds checks each take 0.5 to 3%. They shrink with findings 2 and 5.
- `poll_interrupt` on backward jumps is under 1%.
- `RandomState` SipHash in the compiler's maps is about 2% of compile-source, part of 9c if it is touched at all.
- The `ToNumeric` for an unused postfix update is about 4% of loop instructions. It is folded into finding 3 rather than ticketed alone.
- map-set: the AgniJS engine is already 4.5x faster.
- json-stringify (1.22x) has no single dominant cause beyond 9f.

## The tickets

All are children of TASK-100, labelled `ready-for-agent` and assigned to a Fable agent. Each one carries its reproduction and the benchmark that proves the fix.

| ticket | finding | effort | risk |
| --- | --- | --- | --- |
| TASK-100.16 | 1. Collect while native frames are on the stack | L | high |
| TASK-100.17 | 2. Interpreter dispatch loop | M-L | medium |
| TASK-100.18 | 3. Fewer accumulator moves in the bytecode | M-L | medium |
| TASK-100.19 | 4. Cheaper calls and native-to-JS re-entry | M | medium |
| TASK-100.20 | 5. Inline-cache coverage and pre-interned names | M | low-medium |
| TASK-100.21 | 6. Allocation weight of objects, functions and string constants | M-L | medium |
| TASK-100.22 | 7. Fixed cost per collection | M | medium |
| TASK-100.23 | 8. Count heap growth towards collection and the memory limit | S | low |
| TASK-100.24 | 9a. Global variable cache | S-M | low |
| TASK-100.25 | 9b. Promise and await internals | M | medium |
| TASK-100.26 | 9c. Parser and compiler allocation | M-L | low |
| TASK-100.27 | 9d. Quadratic shape transitions at realm creation | S | low |
| TASK-100.28 | 9e. Bytecode loading and lazy debug info | S | low |
| TASK-100.29 | 9f. Built-in allocation: regex, string methods, JSON | M | low |
| TASK-100.30 | 9g. Anchored regex early exit | S | low |
| TASK-100.35 | 1 (follow-up). Pacing: free safe points and the threshold floor | S | low |

Suggested order: TASK-100.23 and TASK-100.30 are quick. TASK-100.17 to TASK-100.20 together close most of the CPU gap. TASK-100.16 closes the memory gap and is easier after TASK-100.19, which removes the most common native frames. TASK-100.22 matters more once TASK-100.16 lands, because collections become more frequent.
