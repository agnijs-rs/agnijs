# 11. The engine has no JIT tier in v1 and is designed to run without one on every platform

Date: 2026-11-08

## Status

Accepted

## Context

The spec says "no JIT in v1; leave room in the bytecode and object model for a later tier" (section 3). Gable Native targets iOS among its four platforms, and iOS forbids writable-and-executable memory for third-party apps, so a JIT can never run there. A tier that works on three platforms and not the fourth would split the performance model, the test matrix and the bug surface.

## Decision

- **v1 ships an interpreter only.** Performance work goes into the interpreter: dispatch, inline caches on shapes (M3), string and shape tuning, GC pacing and the benchmark gate (M9). This is the model Hermes chose for the same reason.
- **A JIT is out of scope for v1 and is not a goal of any milestone.** Should one be considered later, it must be optional at run time (the interpreter remains the reference on W^X platforms), and every test262 and Gable test must pass with it disabled.
- **What "room for a later tier" means in practice:** fixed register counts per function, a side-table design for inline caches, stable shapes, and no interpreter-only assumptions in the object model. The compiler does not emit anything a JIT would need beyond the existing bytecode.

## Consequences

- Parity with a JIT engine (V8, JSC) on compute-heavy benchmarks is not a v1 target; the switch-over rule in spec section 11 is judged against the interim baseline the maintainer sets in M9.
- There is one execution engine to debug, profile and keep conformant across all four platforms.
- If an application needs more peak throughput than the interpreter gives, the answer in v1 is moving the hot path into Rust host functions through the engine interface, not a JIT.
