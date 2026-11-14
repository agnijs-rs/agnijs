---
id: TASK-103.34
title: >-
  JIT: an unassigned local reads as a garbage number in a catch handler after a
  throw in a speculated loop
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:38:00Z'
updated_date: '2026-11-14T04:46:00Z'
labels:
  - bug
  - security
  - jit
dependencies: []
references:
  - docs/jit-review-2026-11.md
  - crates/agnijs-js-jit/src/analysis.rs
  - crates/agnijs-js-jit/src/codegen.rs
  - crates/agnijs-js-vm/src/interp/jit.rs
parent_task_id: TASK-103
priority: high
type: bug
ordinal: 299000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the JIT soundness review of 2026-11 (`docs/jit-review-2026-11.md`). Severity: high (wrong results), and it exposes uninitialised bytes to JavaScript, hence the `security` label.

## What goes wrong

With the JIT on (`AGNIJS_JIT=on` or `force`), a local that has not been assigned yet can read as a garbage number in a `catch` block of the same function, where the interpreter gives `undefined`. The number is the register's stale payload bytes reinterpreted as an `f64`: bytes left by an earlier value in that register-file slot or by Rust padding. One run printed `1.3198813829285e-311`, whose bits are a 0x0000026E... pattern (it looks like a heap address), so this also leaks process memory contents to script.

## Reproduction (commit fea680f, Windows x64, release build)

Build: `cargo build --release --locked -p agnijs-cli --features agnijs-js-vm/jit`

`lead2.js`:

```js
function f(n, o) {
  var x;
  var y = 0;
  try {
    for (var i = 0; i < n; i++) {
      o.p;        // throws on the first iteration when o is null
      x = i;
      y = x + 1;
    }
  } catch (e) {
    return typeof x + ":" + x;
  }
  return "no throw " + y;
}
console.log(f(10, null));
console.log(f(10, {}));
console.log(f(10, null));
```

- `agnijs lead2.js` (interpreter): `undefined:undefined`, `no throw 10`, `undefined:undefined`.
- `AGNIJS_JIT=force agnijs lead2.js`: `number:1.3198813829285e-311`, `no throw 10`, `number:2.08e-322` (the numbers vary).

Mode `on` (the tiered mode) is affected too. Warm the function up first:

```js
// f as above
for (var k = 0; k < 3000; k++) f(50, {});
var seen = {};
for (var k = 0; k < 20; k++) { var r = f(50, null); seen[r] = (seen[r] || 0) + 1; }
console.log(JSON.stringify(seen));
```

- interpreter: `{"undefined:undefined":20}`
- `AGNIJS_JIT=on`: `{"number:2.08e-322":20}`

The differential fuzzer of the review also hit it (generator seed 552: an unassigned `d` logged from a `catch` as `7.475e-321`).

## Root cause

Three pieces combine (`crates/agnijs-js-jit/src/analysis.rs`, `codegen.rs`, `crates/agnijs-js-vm/src/interp/jit.rs`):

1. `Vm::jit_guess` speculates that a loop-arithmetic register is a number when the live frame holds `Number` **or `Undefined`** ("a local not assigned before the loop: dead at the entry, so unguarded there").
2. `Lower::load_cached` guards a speculated register at an entry (or after a generic call out) only when `analysis::liveness` says it is live there. A register that is dead takes the 8 payload bytes of whatever value the file holds as its number bits, unguarded.
3. `liveness` follows only normal control flow (`successors`): it has no edge from an instruction that can throw to the handler of its `try` range. So a local that every normal path writes before reading is "dead" even when a handler reads it.

Then `Lower::point` lists every dirty cached register at every call out, dead ones included ("the frame the interpreter gets is complete, as a debugger expects it"), as `Location::SpillNumber`. When an instruction with known register access (`GetNamed`, `Call`, `LdaGlobal`, `CheckTdz`, ...) throws before the first write, the fault exit materialises the frame (`exec::call_out` -> `Runtime::materialize` -> `Vm::jit_write_state`) and overwrites the register's real value (`undefined`, or any non-number) with `Value::Number(garbage)`. The interpreter then runs the handler and reads it. (A `Throw` instruction does not trigger it: it is generic, so liveness treats it as reading every register.)

The comment in `load_cached` says a non-number "shows as a number only in a debugger's view of a variable not yet assigned"; a handler in the same function is the case it misses. By the same reasoning a guard-failure side exit (`leave_at` with dirty registers) followed by a throw in the interpreter should show it too; that path was not reproduced separately.

Variants tried: the throw coming from a callee (`Call` is a known-access instruction) reproduces. A `try`/`finally` whose `finally` reads the local did not: the `finally` block is also reached by normal control flow, so liveness sees the read and the entry guard is emitted.

Confirmed by experiment: forcing the guard in `load_cached` (`if self.live_at(pc, i) || true`) makes all three reproductions agree with the interpreter.

## Suggested fix (for the implementer to judge)

Any of these, in rising order of precision:

- do not list (and do not write back) a speculated register at a point until it has been guarded or written on every path to that point, i.e. never materialise bits that were taken unguarded;
- add exception edges to `liveness`: every instruction inside a handler's range that can call out has the handler's target as a successor (then the entry guard fails for an `undefined` local that a handler reads, and the block is recompiled without that guess);
- stop accepting `Undefined` in `jit_guess`.

The debugger's view has the same defect and would be fixed by the first option too.

## Test that should catch it

In `crates/agnijs-js-vm/tests/jit_deopt.rs` (run during the review as a temporary test and red: the forced and the tiered run both returned `number:6.9514752079242e-310`; not committed):

```rust
#[test]
fn an_unassigned_local_is_undefined_in_a_handler_after_a_throw_in_a_speculated_loop() {
    same(
        "function f(n, o) { var x; var y = 0; try { for (var i = 0; i < n; i++) { o.p; x = i; y = x + 1; } } \
         catch (e) { return typeof x + ':' + x; } return 'no throw ' + y; } f(10, null)",
        "undefined:undefined",
    );
}
```

plus a tiered variant that calls `f(50, {})` 3000 times first and then `f(50, null)` (also red), and one where the throw comes from a callee. The review's first generator (described in the review document) reached it once in 760 programs.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The reproduction scripts in the description print the same output with AGNIJS_JIT unset, on and force
- [x] #2 A register compiled code took unguarded is never written into the interpreter frame as a number: no stale payload bytes reach JavaScript or a debugger
- [x] #3 Regression tests in tests/jit_deopt.rs cover the forced case, the tiered case and a throw coming from a callee, and pass under AGNIJS_GC_STRESS=1
- [x] #4 The liveness or materialisation rule that changed is stated in docs/design/jit-deopt-and-gc-maps.md
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests first: analysis unit test (a register a handler reads is live at the throwing instruction), handoff mock tests (a register taken unguarded keeps its own value at a fault and in a debugger's view), tests/jit_deopt.rs (forced, tiered with warm-up, throw from a callee; the review's two scripts in modes on and force).
2. Liveness: every instruction inside a handler's range that can leave compiled code (or poll) gets an edge to the handler's target; the edge is not killed by the instruction's own write.
3. Materialisation: a speculated register carries the tag it was loaded with (number once guarded or written). Points spill tag and bits as a boxed value, write-backs use the same tag, and Location::SpillNumber goes away, so nothing can box unguarded bits as a number (covers the debugger's view and side exits too).
4. Vm::jit_guess: accept Undefined only where the register really is dead at the frame's pc, by the same liveness.
5. Audit the other consumers of exception edges and of liveness (entries, reload after generic call outs, leave_at, materialize_all, left_trampoline) and note findings.
6. Docs: jit-deopt-and-gc-maps.md section 8, ADR 0020 addendum wording, review status column, security-model.md.
7. Gates as listed in the brief.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first. Before the fix: 5 of the new tests in tests/jit_deopt.rs failed (forced and tiered both returned `number:6.95e-310` for `undefined:undefined`), then the side-exit variant once its local was made a guess candidate; analysis::tests had `x` dead at the instruction that can throw; the handoff mock listed the unassigned register as a denormal number. The review could not reproduce the guard-failure side exit followed by a throw; `a_throw_after_a_side_exit_reads_the_frame_the_exit_wrote` does.

What changed, and why both halves. (1) analysis.rs: liveness has an edge from every instruction in a handler's range that can reach the runtime (not in `always_inline`, or a back-edge, which polls) to the handler's target, leaving from before the instruction's own write. (2) codegen.rs, handoff.rs, exec.rs: a speculated register is `Repr::Number { bits, tag }`; the tag is the number tag once guarded or written and the register file's tag word after an unguarded load; spills, write-backs and copies move both words; `Location::SpillNumber` is removed, so the runtime has no way to box bits. (3) interp/jit.rs: `jit_guess` accepts `undefined` only for a register dead at the frame's pc (`agnijs_js_jit::live_registers`). The edge alone would not meet AC 2: a register dead on every path, a handler included, was still materialised as a number for a debugger attaching or a side exit. With (2) the frame is right whatever liveness says, and liveness only decides where a tag test is skipped. Compiled code never follows an exception edge itself (a fault always leaves through the interpreter), so with (2) in place the edge is defence in depth; it costs an entry guard on registers a handler reads.

Exception-edge and liveness consumers checked: `entries` (a handler target is not an entry; the interpreter runs to the next one), `SafePoint::next` and `continues_at` (a handler pc is never listed, so the frame is materialised), `reload_cached` after a generic instruction (same analysis), `leave_at` side exits and `jit_materialize_all` (both now list values), `left_trampoline` (materialises the caller at the call's safe point), the collector's `FrameHandle::live` (an unguarded register's spilled copy equals the register file's value, which is a root). Same bug class elsewhere: `jit_guess` claimed 'dead at the entry' without asking, so every forced script body with a loop compiled with a guess about a live unassigned register and failed its entry guard at once; fixed here, and it is why two deopt counts in tests/jit_deopt.rs dropped by one. Mapped `arguments` objects were checked for aliasing registers behind compiled code's back: they map to environment slots, not registers. Not changed: section 3 of the design document still describes `FrameState::raw`, which was never built (section 8 now says so).

Gates on the final tree (Windows x64, before the rebase onto main 8af8d4a; JIT crate tests, VM suite with jit plain and forced, test262 default and the JIT slices, and clippy were run again after it). clippy `-p agnijs-js-jit --all-targets --no-deps -- -D warnings` clean; no warning in a touched VM file. rustfmt applied to the changed hunks only. test262: 0 regressions in default (43,192), --precompile, --snapshot, --only-intl402 (981) and QuickJS-NG (43,157); JIT forced and tiered slices (3,615) 0 regressions; GC-stress slice (5,959) passes with the JIT off and forced. VM suite with the jit feature passes plain and with AGNIJS_JIT=force. Under AGNIJS_GC_STRESS=1: jit_deopt (13), jit_differential (21), jit_inline_cache (14) and jit_calls (17 of 18) pass. Not run under stress: `collections_find_the_frames_machine_code_pushed` of jit_calls, stopped after 25 minutes (it allocates about 130,000 objects three times over, interpreter first; not timed on the starting commit). bytecode_fuzz in release with jit, plain and forced: pass. jit_workloads: pass. Miri on exec::tests and calls::tests: 9 pass. There is no JIT fuzz test in the tree; a differential generator written for this fix ran 800 programs with the JIT off, on and forced (200 warmed up, 40 under GC stress) with no divergence, and found 3 divergences in 210 programs on the starting binary.

Benchmarks, starting commit against the fix, one path, five alternating rounds: feature off geomean 1.005 (26 workloads; no source outside the JIT changed), JIT on 0.990 (26), JIT forced 1.003 (15). UNMET: 'no regression on the JIT workloads'. prop-monomorphic is 5 to 7% slower with the JIT on or forced (1.057 on, 1.051 forced; 1.069, 1.050, 1.050, 1.074 in four later runs of experimental builds). Ruled out by experiment: the carried tag word (1.050 with every tag a constant), the shared constant (1.050), the wider spill (1.074 with the 8-byte spill restored on top). JIT statistics are identical before and after in mode on; the machine code is 2% larger. Cause unknown; the other loop workloads move by 4% either way under cold-path-only changes. Pushed with this open by the orchestrator's decision; TASK-103.56 tracks it; numbers in docs/jit-review-2026-11.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A local not yet assigned no longer reads as a garbage number in a `catch` block under the JIT.

Three changes. The liveness analysis has the missing edge from an instruction that can throw to its handler. A register compiled code takes without a guard keeps the tag word it was loaded with until it is written, and is spilled, written back and copied with it, so a frame materialised for a fault, a side exit or a debugger holds the register's value; `Location::SpillNumber` is gone and the runtime cannot box bits. The VM's guess counts `undefined` as 'nothing yet' only where the register is dead by the same analysis.

Verified: the review's two scripts print the interpreter's output with AGNIJS_JIT unset, on and force (the starting binary printed `number:8.38e-312` forced and `number:17` tiered); 13 tests in tests/jit_deopt.rs, also under AGNIJS_GC_STRESS=1; unit tests in analysis and handoff; test262 0 regressions in every mode; 800 differential programs with no divergence. The rule is in docs/design/jit-deopt-and-gc-maps.md, section 8.

Open: prop-monomorphic is 5 to 7% slower with the JIT on or forced, cause unknown after three experiments (TASK-103.56). One jit_calls test was not run under GC stress (too slow there).
<!-- SECTION:FINAL_SUMMARY:END -->
