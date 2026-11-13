---
id: TASK-103.34
title: >-
  JIT: an unassigned local reads as a garbage number in a catch handler after a
  throw in a speculated loop
status: To Do
assignee: []
created_date: '2026-11-13T22:38:00Z'
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
- [ ] #1 The reproduction scripts in the description print the same output with AGNIJS_JIT unset, on and force
- [ ] #2 A register compiled code took unguarded is never written into the interpreter frame as a number: no stale payload bytes reach JavaScript or a debugger
- [ ] #3 Regression tests in tests/jit_deopt.rs cover the forced case, the tiered case and a throw coming from a callee, and pass under AGNIJS_GC_STRESS=1
- [ ] #4 The liveness or materialisation rule that changed is stated in docs/design/jit-deopt-and-gc-maps.md
<!-- AC:END -->
