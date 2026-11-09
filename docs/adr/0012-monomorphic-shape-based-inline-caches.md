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

## Addendum (TASK-100.20, 2026-11-09): polymorphic sites, any prototype depth, accessors, arrays and strings

The monomorphic cache covered only plain own and direct-prototype data properties, and every miss re-interned the name. Arrays, strings, polymorphic sites and accessors were where the time went (performance findings, finding 5). The decision above changes as follows; the bytecode does not.

- **Feedback is one lazily allocated block per `CodeBlock`** (`CodeBlock::feedback`, a `OnceCell<Feedback>`), holding the IC sites, the interned name keys and the string constants. The compiler records only `ic_count`. The first VM to run the block owns its feedback (`Feedback::owner`, from a process-wide counter); another VM running the same block uses no feedback and takes the generic path, so ids from one heap are never read against another (ADR 0019 J4). Feedback is never serialised.
- **Each site holds up to four entries** (`IcEntry`, fixed-size `#[repr(C)]` plain data, ADR 0019 J4). A fill replaces an entry for the same receiver shape, or a stale prototype entry, else takes a free one; a fifth shape makes the site megamorphic, which is sticky and goes straight to the generic path.
- **Entry kinds**: own data, own accessor, prototype data and prototype accessor at any depth up to 16, array `length`, string `length`, and data properties on the string prototype chain. Entries for the key `length` are flagged so they never apply to an Array or String receiver. A cached accessor calls its getter or setter directly (an interpreter frame when the callee is inlinable) without the generic lookup.
- **Prototype entries are validated by one global epoch, not by re-checking each holder.** Filling a prototype entry marks every object from the receiver's prototype to the holder as *watched*. `Heap::proto_epoch` is bumped when a watched object adds, replaces or removes a named property, goes to dictionary mode, changes its prototype (all prototype writes go through `Heap::set_proto`), or is collected. A prototype hit checks the receiver's shape, that its prototype is still the recorded object, and that the epoch is unchanged, then reads the holder's slot live and checks that it is still the recorded kind (data or accessor). An epoch at `u32::MAX` saturates, and from then on prototype entries never hit.
- **Names are interned once per block.** The encoded key of each `NameIdx` is cached in the feedback block, and its atom is pinned (`Heap::pinned`, a GC root), so a miss does no hashing. String constants up to 32 code units are atoms cached per collection epoch, so `o['k']` and `super.m` key loads do not hash either.
- **`GetKeyed` / `SetKeyed` have a dense-array fast path** in the dispatch loop: an ordinary or Array object, a Number key that is an integral array index, and a present, non-hole dense element (dense elements are always writable data properties; elements with other attributes live in sparse storage). Strings, arguments objects, typed arrays, namespaces and proxies take the generic path.

Soundness still rests on the three facts above. Shapes are never freed, so the receiver shape check stands. Ids reused after a collection are covered because collecting a watched object bumps the epoch, and the receiver itself is checked by shape. Any structural change to a cached holder or to an object between receiver and holder is a structural change to a watched object. `tests/ic.rs` runs each path, including invalidation by redefinition, freezing, deletion, prototype change and collection, with and without GC stress mode. `tests/ic_stats.rs`, built with the `ic-stats` feature, pins that each targeted access pattern reaches an all-hit steady state.
