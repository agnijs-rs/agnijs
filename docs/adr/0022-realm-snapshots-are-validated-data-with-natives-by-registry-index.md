# 22. A realm snapshot is validated data: natives go by registry index, code through the bytecode serializer, and what cannot be written is refused

Date: 2026-11-11

## Status

Accepted

## Context

TASK-101.2 makes engine startup a load. An embedder creates a realm, runs its bootstrap scripts (polyfills, a framework, a bundle) and then wants every later engine to start in that state without running them again. J11 of ADR 0019 names what stood in the way:

- `NativeKind::Fn` holds Rust function pointers, which mean nothing in another process or build.
- Some natives carry shared `Rc<Cell<_>>` state: promise resolving functions, `Promise.all` element closures and their counters.
- `HostObject` data is opaque to the engine and has no serialisation hook.

A snapshot can also come from anywhere the host keeps files, so the loader faces the same threat as the bytecode loader (ADR 0010): stale, damaged or crafted bytes.

## Decision

**`Engine::snapshot(realm)` writes the heap reachable from one realm, and `agnijs_js_vm::make_engine_from_snapshot(host, bytes)` restores it into a new engine whose main realm it becomes.**

The heap reachable from one realm covers:

- the global object, global environment and lexicals;
- the intrinsics, template objects and `var` names;
- every reachable object, environment, string, symbol and BigInt;
- the registered symbols it reaches;
- the scripts recorded for `script_source` and the debugger.

Engines without snapshots keep the trait's default, `Error::Unsupported`.

**Built-in functions are written as an index into a native registry.** The registry is the natives of a fresh realm in creation order, deduplicated by function, tag, constructor flag and constant slots. Install-time natives with captured constants, such as typed-array constructors and the DataView accessors, carry their element kind in those slots. The registry is built once per process. Its fingerprint covers:

- each entry's name, length, tag and slots;
- the intrinsic classes;
- the engine version, the bytecode format and the `intl` feature.

The fingerprint leads the payload, so a snapshot from another build is a `BytecodeMismatch` before anything is decoded. The container is the ADR 0010 `AGJS` container, with tag `agnijs-snap-1+<bytecode format>`.

**Code goes through the bytecode serializer, each tree once.** Inline caches, feedback and global caches therefore load empty, and JIT code is never written; the tiering counters start again. A function or environment names its code block by its preorder position across the written trees.

**State a snapshot cannot carry is refused, not approximated.** `Error::SnapshotUnsupported` names the path from a root to the offending value, for example `globalThis.app.hooks.onLoad`. The refused state:

- host objects and host functions: no hook yet;
- natives with captured heap state or `Rc<Cell>` flags and counters: promise resolvers, `Promise.all` elements, bound Intl methods, proxy revokers;
- promises with pending reactions;
- generators, modules, namespaces, iterators, mapped `arguments`, `FinalizationRegistry` and Intl service objects;
- objects of another realm;
- a VM that is running, has pending jobs or has modules loaded in the realm.

Handles belong to an engine and are not part of a snapshot. Weak collections and `WeakRef` targets are written as strong edges: what they reach is written, and the next collection after a restore applies weakness again.

**Loading treats the bytes as hostile and never panics.**

- The tag, the fingerprint and then a checksum over the body are checked first.
- Every count is bounded by the bytes left, and every index is checked against its table.
- Every class payload is checked against its invariants:
  - array lengths cover their elements;
  - typed arrays and DataViews are aligned and inside their buffers;
  - accessor halves and bound targets are callable;
  - private elements are keyed by private symbols;
  - collection keys are unique, and weak keys can be held weakly.
- Prototype, environment, bound-function and proxy chains must be acyclic.
- A declarative environment's slots must match its scope.
- Exactly one global environment must exist.
- The intrinsics must have the classes a fresh realm gives them.

Any failure is `BytecodeCorrupt`. Code trees keep the bytecode trust model of ADR 0010: the serializer checks what indexes a block's own pools, and operands that depend on run-time context are taken as produced. The one exception is environment access. `LdaEnv` and `StaEnv` used to panic when an environment lacked the slot the code reads. A damaged snapshot can pair code with the wrong environment, so these accesses now fall back to the slow path, which throws instead. The fast path costs the same as before: the existing bounds check now picks a branch where it used to panic.

A restore builds no realm through the installers, runs no JavaScript, and does not collect garbage until the first safe point after it. The collector's pacing restarts from the loaded live size.

The C API gets no snapshot entry points yet: the ticket does not ask for them, and they can follow on the same two calls.

## Consequences

- Startup from a snapshot skips installing the built-ins and running the bootstrap scripts. `agnijs-js-bench` measures it with the `snapshot-startup`, `bundle-cold` and `bundle-snapshot` workloads, and test262 runs with every test realm restored from a snapshot (`test262 run --snapshot`). On the first measurement (release build, i9-9900KF, medians of 10 engines), a fresh realm starts in 0.48 ms against 0.59 ms cold, and the ~100 KB bundle with its first call starts in 2.04 ms against 2.79 ms from bytecode. The gain is modest because a restore still allocates every object and decodes every code tree, and the bundle's top level does little work beyond defining its functions. A restore that maps the heap image, or shares decoded code between engines, would go further.
- The first restore in a process builds the native registry, which costs one realm creation. A registry generated at build time would remove that cost.
- A snapshot is tied to the exact engine build, as bytecode is. Hosts regenerate snapshots when they upgrade the engine, and a mismatch tells them to.
- Bootstrap code that leaves pending promises, generators or host functions in the realm cannot be snapshotted until those cases are covered:
  - heap-resident state for resolver natives;
  - a host serialisation hook;
  - generator frames as data.
  Each is a follow-up ticket. The refusal message points at the value to remove.
- The loader validates heap structure fully, but the bytecode trust model still applies to the code inside a snapshot, as it does to the bytecode loader. A bytecode verifier would close both.
