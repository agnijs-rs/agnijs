# Engine micro-benchmark baselines

Results files written by `bench run --output` (schema `engine-bench-1`). These are rough, early numbers for spotting large gaps and regressions while the engine grows. They are not the M9 benchmark gate, which measures the embedder's own workloads and sets its own tolerance.

## Running

From `native/engine`:

```sh
cargo build --release -p gable-js-bench
target/release/bench list
target/release/bench run --output new.json                # both engines, every workload
target/release/bench run --filter json --engine gable       # a subset
target/release/bench compare crates/gable-js-bench/baselines/<file>.json new.json
target/release/bench report new.json
```

Each (engine, workload) pair runs in its own child process, so the peak memory figure belongs to that pair alone. A pair runs one warmup and five measured iterations by default (`--warmup`, `--iterations`); the table shows the median. Every workload returns a checksum and the run fails if the engines disagree, so a fast time cannot come from skipped work. `compare` treats changes within 10% (`--threshold`) as noise. The benchmarks never run under `cargo test`; only the harness's own unit tests and a small-scale checksum smoke test do.

Memory columns:

- `peak_rss_bytes`: the child process's peak resident set (peak working set on Windows). It includes the harness binary and both engines' static data, so about 5 to 7 MiB is a floor rather than workload memory (see `engine-startup`).
- `heap_bytes` and `object_count`: what each engine's `memory_usage()` reports after the last iteration, before any explicit collection. The two engines count differently, so compare them within an engine, not across.

## Baseline 2026-11: `2026-11-windows-x86_64-i9-9900kf.json`

Machine: Windows 11 Pro (10.0.26200), x86_64, Intel Core i9-9900KF at 3.60 GHz, 16 logical CPUs, rustc 1.99.0, release profile (`debug = 1`, no LTO). Engine at `integration/js-engine` 00e3b62 (after M7). Desktop machine, not isolated: other applications were running.

| workload | owned (ms) | QuickJS-NG (ms) | owned / QuickJS | owned peak (MiB) | QuickJS peak (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: |
| loop-arith | 520.50 | 133.83 | 3.89x | 6.8 | 5.4 |
| loop-array-index | 617.92 | 87.49 | 7.06x | 6.9 | 5.4 |
| prop-monomorphic | 466.13 | 73.59 | 6.33x | 7.4 | 5.6 |
| prop-polymorphic | 626.31 | 89.72 | 6.98x | 7.4 | 5.7 |
| prop-write-alloc | 356.55 | 167.88 | 2.12x | 21.2 | 5.4 |
| string-concat | 139.62 | 83.63 | 1.67x | 19.2 | 7.3 |
| string-methods | 60.12 | 35.44 | 1.70x | 20.3 | 5.7 |
| array-methods | 621.78 | 153.93 | 4.04x | 91.2 | 5.6 |
| map-set | 237.61 | 1033.10 | 0.23x | 31.5 | 5.9 |
| closures | 217.24 | 74.03 | 2.93x | 24.0 | 5.5 |
| calls-recursive | 188.21 | 34.01 | 5.53x | 6.9 | 5.5 |
| method-calls | 366.70 | 73.97 | 4.96x | 19.2 | 5.5 |
| regex | 323.51 | 136.57 | 2.37x | 25.0 | 5.6 |
| json-stringify | 171.94 | 139.82 | 1.23x | 17.6 | 6.3 |
| json-parse | 134.08 | 86.23 | 1.55x | 25.4 | 7.9 |
| promise-chain | 60.08 | 23.27 | 2.58x | 145.1 | 22.4 |
| async-await | 91.25 | 27.30 | 3.34x | 95.4 | 5.5 |
| compile-source | 20.74 | 8.88 | 2.33x | 27.9 | 8.6 |
| bytecode-load | 4.25 | 0.89 | 4.77x | 25.7 | 7.7 |
| engine-startup | 1.02 | 0.16 | 6.22x | 7.3 | 5.5 |

Geometric mean of owned / QuickJS over the 20 workloads: 2.87x (the owned engine is slower).

### Noise

Three back-to-back runs on the same build: the median of a pair moved by at most 3% for most pairs, up to 5 to 8% for `prop-polymorphic`, `method-calls`, `promise-chain` and `engine-startup` (QuickJS), and 10.6% for `regex` on the owned engine (one slow run). Within one run, the spread between the fastest and slowest iteration is usually under 10% but reaches 70% for `promise-chain` on the owned engine (iterations that hit a collection) and 25 to 30% on the sub-millisecond pairs. The owned/QuickJS ratios moved by at most 0.5 between runs (`prop-polymorphic`, 6.62x to 7.08x). Peak memory was identical to 0.1 MiB across the three runs, except that the very first run after a fresh build reported every pair about 15 MiB higher (probably the first-execution scan of the new binary); discard the first run after a rebuild.

### Findings

- **The interpreter core is the largest gap.** Plain loops, array indexing, property reads and calls are 4x to 7x slower than QuickJS-NG: `loop-array-index` 7.1x, `prop-polymorphic` 7.0x, `prop-monomorphic` 6.3x, `calls-recursive` 5.5x, `method-calls` 5.0x, `loop-arith` 3.9x. Since monomorphic property reads (6.3x) are barely better than polymorphic ones (7.0x), either the inline caches are not hitting on these sites or their hit path costs about as much as the miss; dispatch and call-frame cost are the other suspects. These workloads spend almost no time in built-ins, so they measure dispatch, operand access, property lookup and frame setup.
- **Engine startup is 6.2x slower** (1.0 ms against 0.16 ms per engine with its main realm), and **bytecode load is 4.8x slower** (4.3 ms against 0.9 ms). Bytecode does pay off on the owned engine (4.9x faster than compiling the same script from source) but less than on QuickJS-NG (10x). Compiling from source is 2.3x slower.
- **The owned engine holds far more memory under allocation-heavy work.** `promise-chain` peaks at 145 MiB against 22 MiB, `async-await` at 95 MiB against 5.5 MiB, `array-methods` at 91 MiB against 5.6 MiB, and `array-methods` still reports 78 MB live heap after its last iteration where QuickJS-NG reports 0.1 MB. Garbage survives a long time before a collection runs (GC pacing), which also shows as the iteration spread on `promise-chain`.
- **Promise and async throughput is 2.6x to 3.3x slower**, on top of the memory above.
- **Built-in-heavy work is closest to parity**: `json-stringify` 1.2x, `json-parse` 1.6x, `string-concat` 1.7x, `string-methods` 1.7x, `prop-write-alloc` 2.1x, `regex` 2.4x (the owned regex seam wraps `regress`).
- **`map-set` is the one workload where the owned engine is faster** (0.23x, 238 ms against 1033 ms). QuickJS-NG is unusually slow here, plausibly in its hashing of freshly built string keys (`'k' + i`) or in `Set.prototype.delete`; worth a narrower workload before relying on it.

No engine tuning was done in this task; these numbers describe the engine as it stands after M7.
