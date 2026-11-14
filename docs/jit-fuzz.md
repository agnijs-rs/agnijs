# Differential fuzzing of the JIT

`tools/jit-fuzz/jit-fuzz.mjs` runs random programs in the `agnijs` command with the JIT off, `AGNIJS_JIT=on` and `AGNIJS_JIT=force`, and reports any program whose stdout, exit status or first line of stderr differs from the interpreter's. The interpreter is the oracle (ADR 0019): a compiled tier that prints something else is wrong. It is the generators of the November 2026 soundness review (`docs/jit-review-2026-11.md`), kept so that the review's check can be run again. On the review's starting binary they found TASK-103.34 (3 programs out of 210, and 1 out of 760); on the fixed binary they find nothing.

The Rust tests compare the tiers too (`crates/agnijs-js-vm/tests/jit_differential.rs`, `jit_deopt.rs`, `crates/agnijs-js-bench/tests/jit_workloads.rs`), on programs written for them. This target adds programs nobody wrote.

## Running it

Build the command with the JIT, then run the script (Node 18 or later; no packages):

```
cargo build --release --locked -p agnijs-cli --features agnijs-js-vm/jit
node tools/jit-fuzz/jit-fuzz.mjs
```

The default run takes about two minutes: the regression programs, then generated programs from seed 1 until 120 seconds have passed. It exits 0 when every program agrees and 1 when one does not. It refuses a binary that does not compile under `AGNIJS_JIT=force` (one built without the `jit` feature would agree with itself and prove nothing).

| Option | |
| --- | --- |
| `--bin <path>` | the binary (default `$AGNIJS_BIN`, else `target/release/agnijs`) |
| `--seed <n>` | first seed (default 1) |
| `--count <n>` | run exactly this many seeds, however long that takes (use this for reproducible coverage, as CI does) |
| `--time <s>` | otherwise, stop starting seeds after this many seconds (default 120) |
| `--gen <list>` | `handlers`, `mixed`, `numeric`, comma-separated (default all three); each seed runs one program from each |
| `--warm <mix\|on\|off>` | the warm-up variant (`handlers` and `numeric`): each function runs hot first so that mode `on` compiles it; `mix` (default) warms seeds 3, 7, 11, ... |
| `--stress` | also run forced under `AGNIJS_GC_STRESS`; when that disagrees, the interpreter runs under the same stress too, to tell a collector bug from a JIT one |
| `--stress-every <n>` | collect every *n* safe points (default 16; 1 is very slow) |
| `--node` | also compare the interpreter's stdout with Node's, for the generators and regressions whose output does not depend on error messages |
| `--jobs <n>` | programs at once (default: cores - 1, at most 4) |
| `--timeout <s>` | per run (default 30; four times that under GC stress) |
| `--out <dir>` | reports (default `target/jit-fuzz`) |
| `--minimize-time <s>` | time to minimize each divergence (default 60; 0 skips) |
| `--no-regressions`, `--only-regressions` | skip, or run only, the regression programs |
| `--print <gen>:<seed>[:warm]` | write one generated program to stdout |
| `file.js ...` | compare these files only |

A longer run, for example before a change to the JIT lands: `node tools/jit-fuzz/jit-fuzz.mjs --time 1800 --stress --node`. Seeds are independent, so runs can be split by `--seed`.

## What counts as a divergence

- **output**: stdout or the first stderr line differ, same exit status;
- **exit**: the exit status or signal differs (a crash, or an uncaught exception in one tier only);
- **hang**: a JIT mode ran past `--timeout` while the interpreter took less than a fifth of it.

A JIT run that times out when the interpreter was itself slow, or under GC stress (where a run can be hundreds of times slower), is counted as inconclusive, not as a divergence. A program the interpreter cannot finish in time is skipped.

`--node` compares the interpreter's stdout with Node's (the file runs as a classic script, `tools/jit-fuzz/node-run.cjs`). A difference there is an interpreter bug or a known difference between engines, not a JIT one; it is reported the same way.

## Reports

Each divergent program is saved in `--out` as `<name>.js`, with `<name>.txt` (each mode's exit status, time and stderr, the first differing line, and the command that reproduces it) and `<name>.min.js`, a smaller program that still diverges the same way in the first divergent mode. The minimizer deletes balanced units (a whole function, a whole loop) and then single lines while the divergence holds, testing `--jobs` candidates at a time. A cut that breaks the syntax fails alike in every mode, so it is never kept.

A seed reproduces its program exactly: generators use their own seeded generator and nothing else. The report gives the command, for example:

```
node tools/jit-fuzz/jit-fuzz.mjs --gen handlers --seed 17 --count 1 --warm off --no-regressions
```

## The generators

- **`handlers`** (`gen-handlers.mjs`): loops whose locals are guessed to be numbers inside `try`/`catch`/`finally`, with throws before, between and after the assignments, locals that change type, nested handlers, `continue` and `break` across `finally`, and handlers that log every local. Written for the TASK-103.34 fix.
- **`mixed`** (`gen-mixed.mjs`, the review's generator 1): untyped functions over objects of many kinds (prototype chains, holes, classes with accessors, `arguments`, typed arrays, a Proxy, primitives), calls through `call`, `apply` and `new`, recursion, closures, and a `side()` function that deletes and adds properties and globals, swaps prototypes, installs accessors, freezes objects and resizes arrays in the middle of everything. It logs error messages, so it is not compared with Node.
- **`numeric`** (`gen-numeric.mjs`, the review's generator 2): mostly non-throwing numeric code with typed local pools, so that loops compile with number speculation and direct calls, plus the events that break a guess: a throw caught in the same function, a local that changes type, a skipped first assignment, getters and callees with side effects.

`handlers` and `numeric` are the review's generators byte for byte: a seed gives the program it gave then. `mixed` emitted a SyntaxError in about a quarter of its programs (`"use strict"` in a function with a default parameter, `-1 ** x`, `--1`, `typeof this ** x`), which therefore tested nothing; those are fixed without changing what the generator draws, so every program that parsed then parses to the same tree now.

## Regression programs

`tools/jit-fuzz/regressions/` holds programs that run in every default run, before the generated ones. A file containing `jit-fuzz: node-safe` is also compared with Node under `--node`.

| File | What |
| --- | --- |
| `handlers-17.js`, `handlers-1006.js`, `handlers-1019.js` | the three programs the pre-fix binary got wrong, minimized (TASK-103.34) |
| `catch-reads-unassigned-local.js` | the review's reproduction of TASK-103.34 |
| `catch-reads-unassigned-local-warm.js` | the same compiled by mode `on` while it does not throw |
| `finally-reads-unassigned-local.js` | the same through `finally`, a throwing callee and a stale heap value |
| `callee-leftover-pc.js` | callers whose call site sits at 40 pcs (TASK-103.36; argued unreachable from script, kept as a guard) |
| `probes-calls-and-caches.js`, `probes-globals-envs-elements.js` | the review's hand-written probes, about 200 checks |
| `operator-edges.js` | every number operator over 23 edge values pairwise |

When a run finds a new divergence, fix it with a Rust test as usual and add the `.min.js` here with a header that names the task.

## Checked against a pre-fix binary

November 2026, Windows 11 x64, 4 jobs. The pre-fix binary is the review's build of `fea680f`, before TASK-103.34 to TASK-103.36 were fixed; the fixed one is `32efdce`.

| Binary | Run | Result |
| --- | --- | --- |
| pre-fix | default (regressions, then 120 s: seeds 1 to 81, 243 programs) | 7 divergences: the six TASK-103.34 regressions, and generated `handlers-17` (mode force), which the minimizer cut from 85 lines to 17 |
| pre-fix | the review's three programs (seeds 17, 1006, 1019), minimize time 150 s | all three diverge in mode force; each minimized to 17 to 19 lines, to a fixpoint |
| fixed | default with `--node` (seeds 1 to 171, 512 programs) | 0 divergences |
| fixed | `--stress --seed 1000 --time 240` (164 programs, `AGNIJS_GC_STRESS=16`) | 0 divergences, no timeouts |

The TASK-103.36 regression agrees on both binaries: the review argued that hazard unreachable from script, and the file is kept as a guard. TASK-103.35 is a panic in a trampoline that no script can cause today, so it has no program here.

TASK-103.34 shows in about one `handlers` program in seventy, so a short run is a smoke test, not a proof: a run of a few minutes found it here because seed 17 is early. The default run takes about two minutes with a binary that agrees; each divergence adds up to `--minimize-time`.

## In CI

The `jit` job runs `--count 40` (seeds 1 to 40, 120 programs, and the regressions) on Ubuntu, after the test262 slice has built the engine crates in release with the same features; it takes under a minute locally with 3 jobs. The seed count is fixed, so each CI run covers the same programs and a failure reproduces. On a failure the reports in `target/jit-fuzz` are kept as the `jit-fuzz-divergences` artifact.

Longer runs are for a machine with time to spare: `--time 1800 --stress --node`, or split by `--seed` across machines.
