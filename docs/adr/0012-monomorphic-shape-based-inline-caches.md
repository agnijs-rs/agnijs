# 12. Named property access uses monomorphic, shape-based inline caches kept in a side table

Date: 2026-11-08

## Status

Accepted

## Context

The spec (section 3) asks M3 for "shapes with inline caches" and ADR 0011 relies on inline caches as the interpreter's main property-access optimisation. The object model gives us the ingredients: every ordinary object has a shape that fixes the key *and attributes* of each slot, attribute changes and deletes move an object to dictionary mode, shapes are never freed, and object ids are reused after collection. The question was how much caching to do and how to keep it provably sound against that model.

## Decision

- **One cache entry per `GetNamed` / `SetNamed` site, allocated by the compiler** as a `u16` index into a per-code-block side table (`CodeBlock::ic`, `Cell<IcEntry>`). The compiler only counts slots; the VM owns what the entries mean. The bytecode stays a plain data format and a later tier (ADR 0011) can read the same table.
- **Monomorphic.** An entry remembers the receiver shape it last saw and where the property lived: an own slot, or a data slot of the receiver's *direct* prototype (the method-call case). A miss overwrites the entry.
- **Reads hit when** the receiver has the cached shape, is not in dictionary mode and has no exotic class; for a prototype hit the receiver's prototype must still *be* the cached holder id and have the cached holder shape, and the slot must hold a data property. Accessors, deeper prototype chains, arrays, strings, typed arrays, arguments objects, proxies and module namespaces take the generic path.
- **Writes hit only for an own, writable data property** of an object with the cached shape. Adding a property, writing to a prototype property or hitting a setter all go through `put_value`. No transition caching in this milestone.
- **Soundness argument** (recorded in the module doc of `interp/ic.rs`): a shape id can never come to mean a different layout because shapes are never freed; changing attributes or deleting a property changes the shape or moves the object to dictionary mode, which never hits; and because object ids are reused, a prototype hit re-checks identity *and* shape of the holder, so whatever object now owns that id, equal shape means equal layout. The `tests/ic.rs` suite runs each scenario with and without GC stress mode.

## Consequences

- Hot own-property reads and direct-prototype method lookups skip key interning, hashing and the prototype walk.
- Polymorphic sites thrash the single entry; a short polymorphic cache and transition (add-property) caching are the obvious next steps and can be added behind the same table without changing the bytecode.
- Anything that changes the object model (slot layout, dictionary-mode rules, shape lifetime) must keep the three facts above true or revisit this ADR.

## Addendum (TASK-100.24, 2026-11-09): global bindings have a sibling cache keyed by position

A bare global name (`LdaGlobal`, `StaGlobal`) resolves to a global lexical or to a property of the global object, which is in dictionary mode and so never hits the shape cache above. Each `CodeBlock` carries a second side table, `CodeBlock::globals`, with one `GlobalEntry` per name (indexed by `NameIdx`, allocated on the block's first global access, never serialised; ADR 0019 J4). An entry records the realm and the *position* of the binding in the realm's `global_lexicals` IndexMap or in the global object's dictionary IndexMap, so a hit does no interning and no hashing.

Soundness rests on two facts and one re-check: global lexicals are only ever appended, so a lexical position is stable for the life of the realm; a dictionary position is stable until a property is removed, and removing a property from a global object bumps that realm's `epoch`, as does declaring a global lexical (which may shadow a cached property), and a property entry hits only with the epoch it was recorded under. A hit re-reads the binding's kind (data and writable for a write, initialised, not `const`), so redefinition as an accessor or read-only property, a `const` and the TDZ all fall through to the generic path with no invalidation. `tests/globals.rs` runs each scenario with and without GC stress mode.
