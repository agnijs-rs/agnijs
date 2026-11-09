# 8. The engine compiles to register bytecode with an accumulator and resolves scopes at compile time

Date: 2026-11-08

## Status

Accepted

## Context

Milestone M2 of the Gable JS engine (`docs/js-engine-spec.md`, section 3) needs an intermediate form between the parser's AST (ADR 0007) and the interpreter. The spec asks for a single-pass compiler to register bytecode with an accumulator, compile-time scope resolution with a dynamic fallback for `with` and sloppy direct `eval`, resumable frames for generators and async functions, and a format that can later be serialised (M7) and fed to a JIT tier (not in v1, ADR 0011). Open choices were the instruction encoding, how exceptions and `finally` are expressed, how much of the spec's declaration-instantiation algorithms runs at compile time, and how much of the ES "Reference" abstraction the bytecode exposes.

## Decision

- **Register machine with an accumulator** (`agnijs-js-compiler`, `bytecode.rs`). `Instr` is a fixed-size Rust enum; operands are register indices (`u16`), constant/name/function indices (`u32`) and relative jump offsets. Most instructions read registers and write `acc`, or read `acc` and write a register or a property. Registers are frame slots whose count the compiler fixes per function; the interpreter keeps every frame's registers in one shared register file so the collector finds them in one place.
- **Scope resolution at compile time.** A scope analysis over the AST decides where each binding lives: a register when no inner function captures it, a heap environment slot `(depth, slot)` when captured, a global name, or a dynamic name when the reference sits inside `with` or in a function that contains a sloppy direct `eval`. Pseudo-bindings (`*this*`, `*new.target*`, `*func*`) and private names (`#x`) use the same machinery and are never shadowable by dynamic scopes. Temporal-dead-zone checks are emitted only where the analysis cannot prove initialisation.
- **References as instruction pairs.** The spec's Reference Records are not reified. For plain names the compiler emits a load or store. For dynamic names, compound assignments and destructuring under `with`, it first emits `ResolveDynamic` into a register (an object environment, a declarative environment id, the global or unresolvable) and then `LdaDynamicRef`/`StaDynamicRef`/`LdaRefThis` against that register, which gives the spec's evaluation order (resolve before the right-hand side) without a general Reference type. Member references keep `obj`/`key` in registers after `RequireObjectCoercible` and `ToPropertyKey`.
- **Exceptions through a handler table.** Each `CodeBlock` carries `Handler {start, end, target, env_depth}` ranges; the interpreter walks the table on a throw and unwinds environments to the recorded depth. `finally` is compiled as a completion-kind register plus a value register with a jump table for the pending break/continue/return, so no exception object is allocated for control flow. Uncatchable engine faults (the derived-constructor return check) bypass the table.
- **Declaration instantiation at run time, driven by compile-time tables.** `CodeBlock::declarations` lists var, lexical, function and Annex B declarations; `DeclareGlobals` and the eval/module instantiation in the interpreter apply the spec's GlobalDeclarationInstantiation and EvalDeclarationInstantiation checks against the live global object, which cannot be known when compiling.
- **Generators and async functions are resumable frames.** `Yield` and `Await` return to the Rust caller with the frame saved (`SavedFrame`: the frame plus its registers); resumption pushes it back as an exit frame. `YieldRaw` hands a delegated iterator's result object out unchanged for `yield*`.
- **One compiler for scripts, modules, eval and functions.** Eval code receives `EvalOptions` (strictness, enclosing function/method/derived-constructor context, visible private names) from the caller's frame so early errors and `new.target`/`super` resolution match the spec.

## Consequences

- No `unsafe` is needed anywhere in the compiler or interpreter; the register file is a `Vec<Value>` and every heap reference is an arena index (ADR 0009).
- The bytecode is a Rust enum, not a byte stream; serialisation (M7) is a separate encoding step behind the container header (ADR 0010), which lets the enum keep changing during M2 to M6 without format churn.
- Inline caches (M3) attach to the property instructions by instruction index; nothing in the format reserves cache slots yet, so M3 adds a per-code-block side table rather than widening instructions.
- Every `with`/direct-`eval` function pays for dynamic resolution of all shadowable names inside it, by design; code outside those constructs is unaffected.
- The compiler's register allocation is a simple stack (`alloc`/`mark`/`free_to`); a later tier may want liveness, which the fixed-count-per-function layout permits.
