# AgniJS engine performance findings

This is the investigation in TASK-100.14. It explains why the AgniJS engine is slower or heavier than the interim engine (QuickJS-NG) on the M9 benchmarks ([switch-over report](https://github.com/jcannon98188/gable/blob/main/docs/js-engine-switch-over.md)) and the micro-benchmarks (`crates/agnijs-js-bench`). It measures and explains only. Nothing in the engine changed. Each cause worth fixing has its own backlog ticket, listed in [the tickets](#the-tickets).

All numbers come from the integration tip `3ade2ef`, on an i9-9900KF under Windows 11, release builds.

## Summary

The causes, ranked by measured impact:

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

- **Micro-benchmarks:** `target/release/bench.exe run --engine agnijs --engine quickjs --iterations 5 --warmup 1 --output micro.json`. The geomean is 2.87x, the same as the checked-in baseline.
- **Gable benchmarks** (run from the Gable repository, which keeps the harness): `node native/engine-harness/js/run-bench.mjs --engine agnijs --engine quickjs --iterations 5 --warmup 1`. The geomean is 2.86x, the same as M9's 2.89x.

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

map-set is the only workload where the AgniJS engine is faster, by 4.5x. It is not investigated further.

### Gable benchmarks

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

**c. Parser and compiler allocation.** compile-source is 2.30x and ui-bundle-startup 2.07x. In compile-source, `RtlAllocateHeap` plus `RtlFreeHeap` are 36% self, and in ui-bundle-startup 32%. That comes from boxed AST nodes, `Rc<str>` names and SipHash `HashMap`s in scope analysis.

Fix: an arena or index-based AST, interned identifiers, and a faster hasher for compiler-internal maps. **Effort M to L, risk low**: this is the front end only. TASK-100.26.

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

Suggested order: TASK-100.23 and TASK-100.30 are quick. TASK-100.17 to TASK-100.20 together close most of the CPU gap. TASK-100.16 closes the memory gap and is easier after TASK-100.19, which removes the most common native frames. TASK-100.22 matters more once TASK-100.16 lands, because collections become more frequent.
