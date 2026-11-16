# Ahead-of-time compilation to native code: research note

TASK-101.5, November 2026. This note asks whether AgniJS should compile JavaScript to native code at build time, for the platforms where a JIT cannot run. It compares the options with prior art, estimates cost and payoff from the engine's own measurements, and recommends **defer**. Nothing in the engine changed for it.

## Summary

- **The need is real but narrow.** A JIT cannot run in a third-party iOS or iPadOS app or inside a WebAssembly module, and the baseline JIT is not built for those targets (ADR 0019). There the engine is the interpreter plus bytecode precompilation and snapshots. Ahead-of-time (AOT) native code is the only way to run faster than the interpreter on them.
- **The engine can do it without a new design.** ADR 0019 and ADR 0020 left the route open: the baseline tier keeps the interpreter's frames, its machine code has no relocations, and Cranelift can write object files. An AOT tier would be the baseline tier's code generator run at build time, with the interpreter as the fallback for everything else.
- **What it would buy is what the baseline JIT buys when forced, at best.** That is measured: numeric, property and call loops run 1.3 to 4 times faster, and Gable's own benches run at 0.98 of the interpreter's time on average, between 0.72 and 1.20 ([our measurements](#what-a-baseline-aot-tier-would-buy)). An AOT tier has no running program to take type guesses from, so it would do somewhat worse than forced mode on numeric loops unless it also ships a profile.
- **Prior art agrees.** Every system that compiles untyped JavaScript ahead of time without speculation lands at baseline-JIT level or below: about 2.2 to 2.8 times an interpreter on Octane for weval, 6 to 46% on real programs for static Hermes, and no gain at all on an untyped micro-benchmark there. The large gains come from a sound typed dialect (static Hermes with types), which is a different language, or from giving up `eval` and conformance (Porffor).
- **The costs are concrete.** Native code from the code generator as it stands is about 26 times the size of the bytecode it sits beside, it cannot be downloaded to an iOS app the way bytecode can, Cranelift's iOS target is untested upstream, and the tier adds a build step, an artefact format and a test matrix that only matter on platforms this project has no CI for.

**Recommendation: defer.** Do not build it now, and do not rule it out. [When to reopen](#when-to-reopen) lists what would change the answer, and [prototype scope](#prototype-scope-if-reopened) what a first step would be. Until then the JIT-less platforms get interpreter and runtime work, which every platform shares.

## Where a JIT cannot run

| Platform | Rule | Consequence |
| --- | --- | --- |
| iOS, iPadOS | App Review Guideline 2.5.2: apps "may not ... download, install, or execute code which introduces or changes features or functionality of the app" [A1]. An Apple engineer's forum answer: iOS does not let general apps generate code at run time, third-party browsers aside [A3]. JIT is available to alternative browser engines under an entitlement, in the EU [A4]. | No JIT. Native code that is in the signed app bundle is ordinary app code, so AOT output linked into the app is allowed. |
| iOS, delivery | The Developer Program License Agreement, section 3.3.1(B), lets an app download interpreted code under conditions, and forbids downloading executable code [A2]. | Bytecode and source can be updated over the air; AOT native code cannot. An app that ships AOT code and updates its JavaScript over the air runs the update in the interpreter. |
| WebAssembly | A module's code is fixed when it is instantiated [W1]. | No JIT inside the module. AOT would have to emit WebAssembly, which Cranelift does not. Out of scope here. |
| Android | Ordinary apps may map executable memory: the platform policy allows `execmem` to app domains "for WebView and other application-specific JIT compilers" [D1]. Since Android 10 an app cannot `execve` files from its data directory, and should load native code only from its package [D2]. | The JIT runs where the start-up probe succeeds (ADR 0019). AOT is not needed, except on builds that refuse executable mappings. |
| Windows, Linux, macOS | The JIT runs unless process policy forbids dynamic code (Arbitrary Code Guard, a hardened policy, no `allow-jit` entitlement). | AOT only matters for locked-down processes. |

So the payoff of AOT is almost entirely iOS. What a JIT-less mode costs an engine built around a JIT gives the scale: V8 measured its own JIT-less mode at about 40% slower on Speedometer 2.0 and 80% slower on the Web Tooling Benchmark [V1]. AgniJS starts from the other side, with an interpreter that is the reference tier.

## Prior art

All performance figures below were measured by the projects' own authors, on their own benchmarks, and are not comparable with one another.

| System | Input | How | Dynamic code (`eval`) | Result its authors report | Status |
| --- | --- | --- | --- | --- | --- |
| Static Hermes (`shermes`), Meta [H1 to H7] | Untyped JavaScript, or a sound typed dialect (Flow syntax; TypeScript partly, by conversion) | Hermes's compiler to native code, through C in the versions described; untyped operations become calls to runtime helpers | Bytecode and native code can be mixed in one program | Typed n-body: 5511 ms interpreted, 565 ms native. Untyped factorial loop: 2086 ms interpreted, 2388 ms native. Real untyped programs: 6 to 46% faster than bytecode | In development. Not in React Native: Hermes V1 "does not yet contain JS-to-native compilation" (React Native 0.82, October 2025) |
| Porffor [P1 to P3] | All of JavaScript, untyped | Its own compiler to C, and to WebAssembly; no interpreter and no JIT at all | Not stated in what was read | No speed figures found. About 50% of test262 in October 2024 | Alpha |
| weval with SpiderMonkey [F1 to F3] | All of JavaScript | Partial evaluation of the engine's baseline interpreter on each function's bytecode, to WebAssembly, with a precompiled corpus of inline-cache stubs | The interpreter stays in the module | Octane: 2.77 times the interpreter (blog), 2.17 times (paper, different baseline); a native baseline JIT reaches about 5 times | In StarlingMonkey behind `--aot`, described as experimental |
| Hopc [S1, S2] | All of JavaScript (strict mode is the focus) | JavaScript to Scheme to C to native, with hidden classes, inline caches, type analysis and several versions of a function | By translating to Scheme and interpreting that | "For many tests ... in the range 1x..2x" of V8's time on JetStream2 subsets; weak on floating point, regular expressions and small methods | Research system |
| Bun `build --compile --aot` [B1] | JavaScript and TypeScript | Not stated | Strings of code are interpreted; an undetected dynamic evaluation throws | None published in what was read | Experimental |
| Static TypeScript [T1] | A typed subset of TypeScript | Compiled to machine code against a C++ runtime | Not in the subset | Runs in 16 kB of RAM | Shipped in MakeCode |

Not AOT compilation, although sometimes described that way: QuickJS's `qjsc` embeds bytecode in a C file, and the interpreter runs it [Q1]; Javy embeds a JavaScript runtime in a WebAssembly module [J1]; `deno compile` and plain `bun build --compile` bundle the program with a copy of the runtime [B1, N1]. AgniJS already has the equivalent of these: precompiled bytecode (ADR 0010) and realm snapshots (ADR 0022).

### What the prior art shows

1. **Untyped JavaScript compiled without speculation runs at baseline-JIT speed, not at JIT speed.** weval is the cleanest case, because it compiles exactly what the interpreter would do: 2.2 to 2.8 times the interpreter on Octane, half of what the same engine's native baseline JIT reaches and far below its optimizing JIT (the paper puts that a further 3.86 times above the native baseline) [F1, F3]. Static Hermes's author shows the bottom of the range: an untyped loop compiled to native code calls a runtime helper per operation and was slightly slower than the interpreter, 2388 ms against 2086 ms, until one line let the compiler infer a number (278 ms) [H3]. His talks summarise it as moderate gains for untyped code and large ones with types [H5].
2. **The large gains need types that are true, and that changes the language.** Static Hermes makes its typed dialect sound by changing semantics: checked casts, arrays without holes that throw out of bounds, nominal classes [H2]. Hopc gets within a factor of two of V8 on many tests with whole-program analysis and several compiled versions per function, at five minutes of compile time for a library the size of moment.js [S2]. Neither is a tier under an existing conformant engine.
3. **An interpreter stays in the picture.** Every system that accepts all of JavaScript keeps one for `eval`, `new Function` and code that arrives later (weval, Hermes, Bun), or does poorly on such code (Hopc), or does not run it (Porffor). For AgniJS this is no cost: the interpreter is the reference tier and stays.
4. **Native code is several times larger than bytecode.** Static Hermes's executables grow from 16.8 to 44.0 MB, 16.9 to 34.6 MB and 26.3 to 69.8 MB (runtime included) [H6]. weval turns 7.5 MiB of Octane source into 44 MiB of added WebAssembly [F3].
5. **Nobody ships it broadly yet.** Static Hermes was announced in 2023 and is still not in React Native three years later [H4, H7]; weval and Bun's `--aot` are marked experimental; Porffor is alpha. The engines that run on iOS in production today run an interpreter there.

## Options for AgniJS

### A. Baseline AOT: the baseline tier's code generator, run at build time

ADR 0019 anticipated this: AOT reuses the code generator and the runtime-helper contract (J10), produces a separate artefact with its own format tag bound to the engine build, and continues in the interpreter (same frames, J1) for anything it does not compile. What exists already:

- The baseline tier compiles a bytecode block to one machine function that takes a context pointer. Helpers are reached through pointers in the context and constants are immediates, so the code has no relocations (ADR 0020). It can be written to an object file as it is.
- Inline caches are data the machine code reads at run time, filled by the runtime on a miss. Nothing about them is known at compile time, so AOT code uses them as JIT code does. The same holds for the callee table and the global sites.
- Every guard has a generic path at the same `pc`, and calls, property misses and everything unusual go through the interpreter's own code. So conformance is inherited, and a function the AOT tier refuses (generators, async functions, derived constructors, module and `eval` code today) simply runs interpreted.
- Cranelift writes object files through `cranelift-object` [C1], for x86-64, AArch64, s390x and riscv64 [C2].

What does not exist:

- **The layout descriptions are computed at start-up.** The heap, frame and environment layouts machine code relies on are measured by probes when the engine starts (`HeapLayout`, `CallLayout`, `EnvLayout`). AOT code is generated on another machine, for another target, so the layouts must be constants of the engine build, checked when the artefact is loaded. That means a stable `repr` for every type involved on every target, where today a failed probe just turns a fast path off.
- **Number guesses come from the live frame.** The baseline tier guesses which registers hold numbers by looking at the frame it is about to compile for, and recompiles when the guess fails (ADR 0020, the addendum on TASK-101.3.1). AOT has no frame. It can compile without guesses, guess from the bytecode alone and carry a fallback version, or ship a profile from a training run. The first loses most of the gain on numeric loops; the others add size or a build step.
- **Tiering decisions come from run-time counters.** Mode `on` compiles only what is hot and gives up blocks that do not pay. AOT must choose at build time: everything (the size cost below, and forced mode's losses on code that is mostly calls into the runtime), or a profile.
- **An artefact, a loader and a link step.** The object code has to reach the app: a static library the embedder links, with a table from code-block identity to entry points, validated against the bytecode it was compiled from and the engine build. Embedders then have a build-time tool to run for each target architecture.
- **A target nobody upstream tests.** Wasmtime lists `aarch64-apple-ios` as a tier 3 target without CI, and using Cranelift outside Wasmtime as tier 3 too [C3]. `rustc_codegen_cranelift` shows `cranelift-object` working on macOS AArch64 [C4], which is the same instruction set and object format, but it does not list iOS. This project has no iOS CI either.

### B. A typed dialect (the static Hermes route)

A sound typed subset with its own semantics, a type checker, and a second compiler for it. It is where the tenfold gains are, and it is a different product: AgniJS's purpose is to run ECMAScript as specified, measured by test262, and its embedders ship bundles of ordinary untyped JavaScript. **No-go.**

### C. Whole-program compilation with no interpreter (the Porffor route)

Gives up `eval`, the debugger, hot update and the conformance baselines, for a benefit option A already provides. **No-go.**

### D. Partial evaluation of the interpreter (the weval route)

weval specialises an interpreter that has been compiled to WebAssembly, and its output is WebAssembly. It would suit a WebAssembly build of AgniJS and does nothing for native iOS. It also reaches about half of a hand-written baseline compiler's gain in its authors' measurements, and AgniJS already has the hand-written baseline. **Not applicable** to the iOS problem; worth a second look only if a WebAssembly embedder asks for speed.

### E. No AOT: the interpreter, precompiled bytecode and snapshots

What JIT-less targets have today. Start-up is already build-time work: bytecode needs no parse and a snapshot no realm set-up. Throughput is the interpreter's, which the last round of work brought to 0.92 times QuickJS-NG's time on the micro-benchmarks and 1.46 times on Gable's (`docs/js-engine-performance-findings.md`).

## What a baseline AOT tier would buy

The baseline JIT in forced mode is the model: it compiles every eligible block on first sight, as an AOT tier would, and it has number guesses an AOT tier would lack, so it is an upper bound. Compile time is not part of the comparison, because AOT pays it at build time. Measured for this note and for ADR 0028 at `0f03d8f`: i9-9900KF, Windows 11, release build with `--features jit`, one binary, the modes taking turns within each round, 9 rounds, each figure the median over the rounds of each run's median sample (5 iterations after 1 warm-up). Ratios are time over the interpreter's (`AGNIJS_JIT=off`) in the same binary.

| Workloads | Forced, over the interpreter |
| --- | --- |
| Loops over numbers, array elements and properties (four micro-benchmarks) | 0.24 to 0.33 |
| Calls: calls-recursive, closures, method-calls | 0.38, 0.69, 0.75 |
| Allocation, strings, array methods, collections, promises (five) | 1.09 to 1.33 |
| Regex and JSON (five) | 0.93 to 1.21 |
| Gable rxjs-operators, rxjs-subject-fanout | 0.76, 0.72 |
| Gable rxjs-higher-order, signal-churn, signal-create | 1.03, 1.00, 1.01 |
| Gable template-mount, template-update, list-diff | 1.20, 1.15, 1.09 |
| Geomean, 19 steady micro-benchmarks | 0.75 |
| Geomean, Gable's eight benches without start-up | 0.98 |

The full tables, the method's details and the profiles are in `docs/js-engine-performance-findings.md`, section "Performance-tier close-out, 2026-11-16".

Read as an AOT forecast:

- **Code that is loops over numbers and plain objects would run about three times faster**, less whatever the missing number guesses cost. In forced mode those guesses are what lets a loop keep its counters unboxed; without them each arithmetic instruction keeps its tag check and its values stay in the register file.
- **Gable would gain nothing on average**: two benches 1.3 to 1.4 times faster, three unchanged, three 9 to 20% slower. The profiles say why. Where compiled code wins, 60% of the time is spent in machine code; where it does not, 33 to 40% is, and the rest is in the runtime that machine code calls for property access the caches miss, for getters, `new` and `call`, and for allocation. AOT code calls the same runtime.
- **Start-up would not improve.** Precompiled bytecode and snapshots already remove parsing and realm set-up. AOT adds the cost of mapping the code in.
- **Compiling everything is the wrong selection.** Forced mode compiles code that is mostly calls into the runtime as eagerly as it compiles loops, and loses on the template benches for it. An AOT tier that wanted the gains without the losses would need a profile from a training run to choose what to compile, which is a second build step for the embedder.

### Code size

Measured with forced mode, which compiles every function that runs (the findings document has the table):

- A generated script whose 300 functions all run: 160 KB of source, 0.68 MB of bytecode container, **17.4 MB of machine code** in 505 compiled blocks, 6 s to compile. That is 26 times the bytecode.
- Gable's template bundle: 266 KB of source, 1.03 MB of bytecode, and 8.1 to 8.7 MB of machine code for the 260 to 294 functions its benches run.
- Gable's UI bundle: 1.42 MB of source, 4.91 MB of bytecode. Start-up alone runs 163 compilable functions, 2.4 MB of machine code. The whole bundle compiled was not measured; at the generated script's ratio to bytecode it would be over 100 MB, which is an extrapolation, not a measurement.

The bytecode stays in the app as well, because the interpreter needs it for everything that leaves compiled code. So AOT adds to the app's size; it does not replace anything.

These figures are an order of magnitude worse than the prior art's (static Hermes 2 to 2.7 times at the executable level, weval about 6 bytes of WebAssembly per byte of source). The cause is known and is the baseline tier's open compile-time item in ADR 0020: every property site is expanded inline four ways, and every instruction after a call into the runtime is an entry point. A JIT that compiles a few hot functions can afford that. An AOT tier cannot, so shared out-of-line cache stubs and fewer entry points would be its first piece of work, before anything in the cost table below.

## Cost

| Item | Size | Notes |
| --- | --- | --- |
| Smaller code: shared out-of-line cache stubs, fewer entry points | M to L | Needed first; see [code size](#code-size). It would also cut the JIT's compile time and memory. |
| Layouts as build constants, checked at load, on every target | M | Touches the `repr` of `Value`, object headers, frames and environments; today's probes become assertions. |
| Object emission and the artefact format | M | `cranelift-object`, a block table, a format tag bound to the engine build and to the bytecode's hash. |
| Loader and embedder build integration | M to L | A build tool per target, a static library to link, registration at engine start; documentation and an example for Xcode. |
| Number guesses without a live frame, or a profile format | M | Or accept losing most of the numeric gain. |
| Selection: what to compile | S to M | Everything, or a profile from a training run. |
| Testing | L | test262, the JIT differential tests and the fuzz target in an AOT mode on desktop targets is straightforward. iOS needs a device or simulator runner this project does not have. |
| Soundness | ongoing | Every defect class of the baseline JIT applies, and AOT code cannot be switched off by a run-time probe: a wrong artefact is linked into the app. The November 2026 review found one wrong-result defect in the baseline tier (`docs/jit-review-2026-11.md`). |

Sizes are those of `docs/js-engine-performance-findings.md`: S about a day, M a few days, L a week or more, for the kind of agent-driven work this repository is built with. Together that is several weeks before the first measurement on a device, and a permanent second delivery form to keep conformant.

## Recommendation: defer

The route is sound and the engine is shaped for it, but the case does not close today:

1. **The payoff is modest where it matters.** The embedder that motivates iOS support is Gable, and Gable's workloads gain nothing on average (0.98 of the interpreter's time over eight benches, from 0.72 to 1.20) from compiled code even with a JIT's advantages. The workloads that gain a lot (numeric and property loops) are the ones ADR 0011 already tells embedders to move into host functions.
2. **The cost lands on a platform we cannot test.** No iOS CI, an upstream code generator that does not test the target, and a tier whose defects ship inside the app binary.
3. **It forfeits over-the-air updates for the compiled part**, which is a main reason to run a JavaScript engine in an app at all.
4. **The same effort spent on the interpreter and runtime pays on every platform**, iOS included. ADR 0028 lists that work: megamorphic property access, calls through `call` and `apply`, getters, allocation and collection.
5. **Nothing is lost by waiting.** The constraints that keep the route open (J1, J10, relocation-free code, the helper contract) are already kept for the JIT's sake.

Because the recommendation is not "go", no follow-up tasks are created (acceptance criterion 2 of TASK-101.5 applies only to a go).

### When to reopen

- An embedder needs more throughput on iOS than the interpreter gives, on code that cannot move into host functions, and can accept no over-the-air updates for it.
- The baseline tier, forced, beats the interpreter on that embedder's workloads by a margin worth the size, say a geomean of 0.8 or better. For Gable that needs the call-target feedback and megamorphic-cache work ADR 0020 lists as not reached.
- Someone can test on iOS: a simulator job in CI at least.

### Prototype scope, if reopened

Smallest experiment that answers "does it work and what does it cost", on a desktop target first:

1. A build tool that reads a bytecode container, runs the baseline code generator with guesses off over every eligible block, and writes one object file plus a block table through `cranelift-object`.
2. Layouts frozen as constants for that one target, asserted at load.
3. A loader that maps block identity to entry points and marks those blocks compiled, so `Vm::jit_run` enters them; everything else is the interpreter.
4. Measure on macOS AArch64 with the JIT feature's run-time half off: test262 in that mode, `agnijs-js-bench` and Gable's benches against the interpreter, and the artefact's size against the bytecode's.
5. Only then the iOS link step.

## Sources

Read in November 2026. Most pages were read through a fetching tool that returns extracts, and the two papers [F3, S2] from their PDF text. Sentences in quotation marks were confirmed against the page text for [A1], [H3], [H4], [F1] and [V1]; other figures are as the extracts gave them.

- [A1] Apple, App Review Guidelines, 2.5.2. https://developer.apple.com/app-store/review/guidelines/
- [A2] Apple Developer Program License Agreement, 3.3.1(B). https://developer.apple.com/support/terms/apple-developer-program-license-agreement/
- [A3] Apple Developer Forums, answer by an Apple DTS engineer, July 2024. https://developer.apple.com/forums/thread/759238
- [A4] Apple, Using alternative browser engines in the European Union. https://developer.apple.com/support/alternative-browser-engines/
- [B1] Bun documentation, single-file executables and their compatibility notes. https://bun.com/docs/compiler/executables.md and https://bun.com/docs/compiler/compatibility.md
- [C1] `cranelift-object` 0.137.1. https://docs.rs/cranelift-object/latest/cranelift_object/
- [C2] Cranelift README. https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/README.md
- [C3] Wasmtime, stability tiers. https://docs.wasmtime.dev/stability-tiers.html
- [C4] `rustc_codegen_cranelift` README. https://github.com/rust-lang/rustc_codegen_cranelift
- [D1] Android Open Source Project, `system/sepolicy`, `private/app.te`. https://android.googlesource.com/platform/system/sepolicy/+/refs/heads/main/private/app.te
- [D2] Android 10 behavior changes. https://developer.android.com/about/versions/10/behavior-changes-10
- [F1] C. Fallin, "Compilation of JavaScript to Wasm, Part 3: Partial Evaluation" and the AOT post before it, August 2024. https://cfallin.org/blog/2024/08/27/aot-js/ and https://cfallin.org/blog/2024/08/28/weval/
- [F2] ComponentizeJS. https://github.com/bytecodealliance/ComponentizeJS
- [F3] C. Fallin and M. Bernstein, "Partial Evaluation, Whole-Program Compilation", PACMPL 9 (PLDI), 2025. https://arxiv.org/abs/2411.10559
- [H1] Hermes, `static_h` branch README and `doc/Branches.md`. https://github.com/facebook/hermes/tree/static_h
- [H2] Hermes, `doc/TypedLanguage.md`. https://github.com/facebook/hermes/blob/static_h/doc/TypedLanguage.md
- [H3] T. Mikov, "How to speed up a micro-benchmark 300x", September 2023. https://tmikov.blogspot.com/2023/09/how-to-speed-up-micro-benchmark-300x.html
- [H4] React Native 0.82 release post, October 2025. https://reactnative.dev/blog/2025/10/08/react-native-0.82
- [H5] T. Mikov, slides, React Native EU 2023 and React Universe 2024. https://speakerdeck.com/tmikov2023/static-hermes-react-native-eu-2023-announcement and https://speakerdeck.com/tmikov2023/hermes-better-performance-with-bytecode-translation-react-universe-2024
- [H6] T. Mikov, slides, "Compiling a real-world CLI tool to native with Static Hermes". https://speakerdeck.com/tmikov2023/compiling-a-real-world-cli-tool-to-native-with-static-hermes
- [H7] React Native 0.84 release post, February 2026. https://reactnative.dev/blog/2026/02/11/react-native-0.84
- [J1] Javy. https://github.com/bytecodealliance/javy
- [N1] Deno, `deno compile`. https://docs.deno.com/runtime/reference/cli/compile/
- [P1] Porffor README. https://github.com/CanadaHonk/porffor
- [P2] Porffor site. https://porffor.dev/
- [P3] CanadaHonk, "Porffor passes 50% of test262", October 2024. https://honk.foo/porffor-50
- [Q1] QuickJS documentation, `qjsc`. https://bellard.org/quickjs/quickjs.html
- [S1] M. Serrano, "JavaScript AOT compilation", DLS 2018 (abstract only). https://conf.researchr.org/details/dls-2018/dls-2018/3/JavaScript-AOT-Compilation
- [S2] M. Serrano, "Of JavaScript AOT compilation performance", PACMPL 5 (ICFP), 2021. DOI 10.1145/3473575.
- [T1] T. Ball, P. de Halleux, M. Moskal, "Static TypeScript", MPLR 2019. https://www.microsoft.com/en-us/research/publication/static-typescript/
- [V1] V8 blog, "JIT-less V8", March 2019. https://v8.dev/blog/jitless
- [W1] WebAssembly, security. https://webassembly.org/docs/security/

### What could not be verified

- **Static Hermes:** any test262 figure from Meta; how native code and bytecode call each other; whether its native back end still goes through C; how `eval` runs in a native executable. The run-time and size figures of [H6] come from an extract of slides, without the talk.
- **Porffor:** a current test262 figure, any speed measurement, and what it does with `eval`.
- **weval:** whether anyone runs it in production, and its size cost on an ordinary application as opposed to Octane.
- **Hopc:** a summary ratio against V8, binary sizes, and whether it is still maintained.
- **Bun `--aot`:** which compiler it uses, when it appeared, and any figure. The documentation page was the only source.
- **Apple:** the wording of 3.3.1(B) was read from an extract of a very long page, not confirmed character for character, and the agreement's date was not found. No Apple document was found that states the iOS code-generation rule directly; [A3] is a forum answer. Guideline 2.5.6 mentions Japan beside the EU for alternative engines, and the entitlement page as read names only the EU. JavaScriptCore's behaviour for third-party apps (interpreter only) was not confirmed from a primary source and is not relied on here.
- **Android:** builds that remove `execmem` from apps (hardened distributions, enterprise policy) were not researched; ADR 0019's probe covers them either way.
- **Cranelift:** whether `cranelift-object` output for `aarch64-apple-ios` works. No source says; it was not tried here.
- **Consoles** and other closed platforms were not researched.
- **AssemblyScript and NectarJS** were not looked at.
