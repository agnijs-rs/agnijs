# Architecture decision records

ADR numbers are shared with the Gable repository, where the engine began; they were kept when the engine moved (TASK-100.12), so the sequence has gaps. The ADRs that concern Gable alone stay in [the Gable repository](https://github.com/jcannon98188/gable/tree/main/docs/adr), including ADR 0004 (Gable's decision to own a JavaScript engine) and the adoption ADR that records Gable depending on this repository.

Crate names in these records use the current `agnijs-*` names; where a record says "the Gable engine" it means this engine under its former working name.

- [The engine interface is handle-based and host-driven](0006-engine-interface-is-handle-based-and-host-driven.md)
- [7. The engine parser is handwritten, produces a span-carrying ESTree-shaped AST, and validates regex literals syntax-only](0007-handwritten-parser-with-span-ast-and-syntax-only-regex-validation.md)
- [8. The engine compiles to register bytecode with an accumulator and resolves scopes at compile time](0008-register-bytecode-with-accumulator-and-compile-time-scope-resolution.md)
- [9. The heap is a set of index arenas collected by precise mark-sweep at interpreter safe points](0009-heap-is-index-arenas-with-precise-mark-sweep-at-safe-points.md)
- [10. Precompiled code ships in a versioned container that is rejected on mismatch](0010-versioned-bytecode-container.md)
- [11. The engine has no JIT tier in v1 and is designed to run without one on every platform](0011-no-jit-tier-in-v1.md)
- [12. Named property access uses monomorphic, shape-based inline caches kept in a side table](0012-monomorphic-shape-based-inline-caches.md)
- [13. RegExp matching adopts the `regress` backtracking engine behind a seam that owns the ECMAScript semantics](0013-regexp-matching-adopts-regress-behind-a-seam.md)
- [14. Date takes its clock and time zone from the host; buffers and Atomics are single-agent plain memory](0014-date-takes-its-clock-and-time-zone-from-the-host.md)
- [15. Intl is ICU4X behind the agnijs-js-intl seam, with bundled CLDR tables and spec-shaped workarounds](0015-intl-on-icu4x-behind-the-agnijs-js-intl-seam.md)
- [16. The engine workspace stands alone: Gable reaches in only through the engine interface](0016-the-engine-workspace-stands-alone.md)
- [17. The debugger is a capability beside the engine interface, and CDP lives in its own crate](0017-debugger-is-a-capability-beside-the-engine-interface-with-cdp-in-its-own-crate.md)
- [19. Later tiers share the interpreter's frames, and the interpreter, bytecode, object model and collector keep the constraints that make them possible](0019-jit-readiness-constraints-tiering-and-deoptimization.md) (0018 is Gable's adoption ADR)
- [20. The baseline JIT generates code with Cranelift's code generator alone, never speculates, and leaves compiled code for every call](0020-baseline-jit-on-cranelift-codegen-with-no-speculation.md)
