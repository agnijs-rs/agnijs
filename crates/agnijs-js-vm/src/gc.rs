//! The collector: precise, non-moving mark-sweep over the arenas.
//!
//! Roots are the interpreter frames and register file, the scoped root
//! stack, the host handle table, every realm, the pre-interned names and
//! well-known symbols, the symbol registry, the job queue and module
//! records. Interned strings are weak: dead atoms leave the table.
//!
//! Shapes are traced, not roots (TASK-100.22): a live object marks its
//! shape, a live shape marks its parent and the keys it owns in its key
//! table, and the inline caches of every block a live function or frame
//! runs mark the shapes their entries name. The sweep frees the other
//! shapes and key tables and reuses their ids; a block whose caches were
//! not traced is cleared before its next use (`Vm::fresh_ic`).
//!
//! Marking runs over the mark bits moved out of the arenas into a
//! [`Tracer`], which marks each reference as it is reported and keeps one
//! work list; weak containers are recorded as they are marked, so weak
//! processing visits only them. The sweep hands dead objects' and scopes'
//! small buffers to later allocations (`heap::BufPool`) instead of freeing
//! them one by one, and totals the live size as it goes.
//!
//! Collection runs only from [`Vm::maybe_collect`] at interpreter safe
//! points (between instructions) and only when no native frame is on the
//! stack, so Rust locals in built-ins never hold stale ids (nor shape ids).
//! There is no `unsafe` code here or in the heap; the arenas are plain
//! vectors.

use std::rc::Rc;

use agnijs_js_compiler::bytecode::{CodeBlock, IcEntry};

use crate::env::Environment;
use crate::interp::Frame;
use crate::object::{Class, Elements, ObjectData};
use crate::string::Repr;
use crate::value::{BigId, EnvId, ObjId, PropKey, PropSlot, ShapeId, StrId, SymId, Value};
use crate::vm::{Job, Vm};

/// Work items during marking: marked values whose references are not yet
/// reported.
enum Work {
    Obj(ObjId),
    /// A rope (flat strings have no references).
    Str(StrId),
    Sym(SymId),
    Env(EnvId),
    Shape(ShapeId),
}

/// The marking state of one collection: the arenas' mark bits (moved out
/// of the heap for the duration), the work list, and what the sweep and
/// the weak pass need to know. Heap values report their references to it
/// through `trace(&self, t: &mut Tracer)`; each report marks at once.
pub struct Tracer<'v> {
    vm: &'v Vm,
    objs: Vec<bool>,
    strs: Vec<bool>,
    syms: Vec<bool>,
    bigs: Vec<bool>,
    envs: Vec<bool>,
    shapes: Vec<bool>,
    /// Per key table: the longest marked shape over it, so far; its keys
    /// up to there have been marked.
    table_len: Vec<u32>,
    work: Vec<Work>,
    /// Marked WeakMaps, WeakSets, WeakRefs and FinalizationRegistries.
    weak: Vec<ObjId>,
    /// Blocks whose inline caches were traced (and so stay valid).
    blocks: Vec<Rc<CodeBlock>>,
}

#[inline]
fn set(marks: &mut [bool], i: u32) -> bool {
    let m = &mut marks[i as usize];
    !std::mem::replace(m, true)
}

impl<'v> Tracer<'v> {
    #[inline]
    pub fn value(&mut self, v: Value) {
        match v {
            Value::Object(o) => self.obj(o),
            Value::String(s) => self.str(s),
            Value::Symbol(s) => self.sym(s),
            Value::BigInt(b) => self.bigint(b),
            _ => {}
        }
    }
    #[inline]
    pub fn obj(&mut self, o: ObjId) {
        if set(&mut self.objs, o.0) {
            self.work.push(Work::Obj(o));
        }
    }
    #[inline]
    pub fn opt_obj(&mut self, o: Option<ObjId>) {
        if let Some(o) = o {
            self.obj(o);
        }
    }
    #[inline]
    pub fn str(&mut self, s: StrId) {
        if set(&mut self.strs, s.0)
            && matches!(self.vm.heap.string(s).repr, Repr::Rope { .. })
        {
            self.work.push(Work::Str(s));
        }
    }
    #[inline]
    pub fn opt_str(&mut self, s: Option<StrId>) {
        if let Some(s) = s {
            self.str(s);
        }
    }
    #[inline]
    pub fn sym(&mut self, s: SymId) {
        if set(&mut self.syms, s.0) {
            self.work.push(Work::Sym(s));
        }
    }
    #[inline]
    fn bigint(&mut self, b: BigId) {
        set(&mut self.bigs, b.0);
    }
    #[inline]
    pub fn env(&mut self, e: EnvId) {
        if set(&mut self.envs, e.0) {
            self.work.push(Work::Env(e));
        }
    }
    #[inline]
    pub fn opt_env(&mut self, e: Option<EnvId>) {
        if let Some(e) = e {
            self.env(e);
        }
    }
    #[inline]
    pub fn key(&mut self, k: PropKey) {
        match k {
            PropKey::Str(s) => self.str(s),
            PropKey::Sym(s) => self.sym(s),
            PropKey::Index(_) => {}
        }
    }
    #[inline]
    pub fn slot(&mut self, s: PropSlot) {
        match s {
            PropSlot::Data(v) => self.value(v),
            PropSlot::Accessor { get, set } => {
                self.value(get);
                self.value(set);
            }
        }
    }
    pub fn values(&mut self, vs: &[Value]) {
        for &v in vs {
            self.value(v);
        }
    }
    #[inline]
    pub fn shape(&mut self, s: ShapeId) {
        if set(&mut self.shapes, s.0) {
            self.work.push(Work::Shape(s));
        }
    }

    /// The inline caches of a block that may run again: the shapes their
    /// entries name stay alive, and the block's feedback stays valid
    /// across this collection. Traced once per collection.
    pub fn code(&mut self, code: &Rc<CodeBlock>) {
        let vm = self.vm;
        let Some(fb) = code.feedback_if(vm.feedback_owner) else {
            return;
        };
        if fb.gc_mark.get() == vm.gc_count {
            return;
        }
        fb.gc_mark.set(vm.gc_count);
        // Entries from before an earlier shape-freeing collection may name
        // reused ids; clear them rather than keep arbitrary shapes alive.
        vm.fresh_ic(fb);
        for site in fb.ic.iter() {
            for c in &site.entries {
                let e = c.get();
                if matches!(
                    e.kind,
                    IcEntry::KIND_OWN_DATA
                        | IcEntry::KIND_OWN_ACCESSOR
                        | IcEntry::KIND_PROTO_DATA
                        | IcEntry::KIND_PROTO_ACCESSOR
                ) {
                    self.shape(ShapeId(e.shape));
                }
            }
        }
        self.blocks.push(code.clone());
    }

    /// Mark everything reachable from the work list.
    fn drain(&mut self) {
        let vm = self.vm;
        while let Some(item) = self.work.pop() {
            match item {
                Work::Obj(o) => {
                    let d = vm.heap.obj(o);
                    if matches!(
                        d.class,
                        Class::WeakMap(_)
                            | Class::WeakSet(_)
                            | Class::WeakRef(_)
                            | Class::FinalizationRegistry(_)
                    ) {
                        self.weak.push(o);
                    }
                    d.trace(self);
                }
                Work::Env(e) => vm.heap.env(e).trace(self),
                Work::Sym(s) => {
                    let d = vm.heap.sym(s);
                    self.opt_str(d.description);
                    match d.private {
                        crate::symbol::PrivateElement::Method(m) => self.value(m),
                        crate::symbol::PrivateElement::Accessor { get, set } => {
                            self.value(get);
                            self.value(set);
                        }
                        crate::symbol::PrivateElement::Field => {}
                    }
                }
                Work::Str(s) => {
                    if let Repr::Rope { left, right, .. } = vm.heap.string(s).repr {
                        self.str(left);
                        self.str(right);
                    }
                }
                Work::Shape(s) => {
                    let sh = vm.heap.shape(s);
                    if let Some(p) = sh.parent {
                        self.shape(p);
                    }
                    let t = sh.table.0 as usize;
                    let have = self.table_len[t];
                    if sh.len > have {
                        self.table_len[t] = sh.len;
                        for (k, _) in &vm.heap.key_tables[t].keys[have as usize..sh.len as usize] {
                            self.key(*k);
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn is_marked(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.objs[o.0 as usize],
            Value::Symbol(s) => self.syms[s.0 as usize],
            _ => true,
        }
    }

    /// Ephemerons: a WeakMap value lives while its key does. Repeats until
    /// no entry's value is newly reached.
    fn ephemerons(&mut self) {
        let vm = self.vm;
        loop {
            let mut i = 0;
            while i < self.weak.len() {
                let o = self.weak[i];
                i += 1;
                if let Class::WeakMap(m) = &vm.heap.obj(o).class {
                    for (k, v) in m.entries.iter().flatten() {
                        if self.is_marked(*k) && !self.is_marked(*v) {
                            self.value(*v);
                        }
                    }
                }
            }
            if self.work.is_empty() {
                break;
            }
            self.drain();
        }
    }
}

impl ObjectData {
    pub fn trace(&self, t: &mut Tracer) {
        t.opt_obj(self.proto);
        t.shape(self.shape);
        for s in &self.slots {
            t.slot(*s);
        }
        if let Some(d) = &self.dict {
            for (k, p) in d.iter() {
                t.key(*k);
                t.slot(p.slot);
            }
        }
        match &self.elements {
            Elements::Dense(v) => t.values(v),
            Elements::Sparse(m) => {
                for p in m.values() {
                    t.slot(p.slot);
                }
            }
        }
        self.class.trace(t);
    }
}

impl Class {
    pub fn trace(&self, t: &mut Tracer) {
        match self {
            Class::Ordinary
            | Class::Error
            | Class::Boolean(_)
            | Class::Number(_)
            | Class::Date(_)
            | Class::Array(_) => {}
            Class::Function(f) => {
                t.code(&f.code);
                t.opt_env(f.env);
                t.opt_obj(f.home_object);
                t.opt_obj(f.module);
                if let Some(cd) = &f.class_data {
                    for field in &cd.fields {
                        match &field.key {
                            crate::object::FieldKey::Prop(k) => t.key(*k),
                            crate::object::FieldKey::Private(s) => t.sym(*s),
                        }
                        t.opt_obj(field.init);
                    }
                    for s in &cd.private_methods {
                        t.sym(*s);
                    }
                }
            }
            Class::Native(n) => t.values(&n.slots),
            Class::Bound(b) => {
                t.obj(b.target);
                t.value(b.this);
                t.values(&b.args);
            }
            Class::String(s) => t.str(*s),
            Class::Symbol(s) => t.sym(*s),
            Class::BigInt(b) => t.value(Value::BigInt(*b)),
            Class::Arguments(m) => {
                if let Some(m) = m {
                    t.env(m.env);
                }
            }
            Class::ForIn(f) => {
                t.opt_obj(f.object);
                for k in &f.keys {
                    t.key(*k);
                }
                for k in &f.visited {
                    t.key(*k);
                }
            }
            Class::ArrayIterator(a) => t.opt_obj(a.target),
            Class::StringIterator { string, .. } => t.opt_str(*string),
            Class::Generator(g) => g.trace(t),
            Class::Promise(p) => p.trace(t),
            Class::RegExp(r) => r.trace(t),
            Class::Map(m) | Class::Set(m) | Class::WeakMap(m) | Class::WeakSet(m) => m.trace(t),
            Class::Intl(d) => d.trace(t),
            Class::MapIterator(i) | Class::SetIterator(i) => t.opt_obj(i.target),
            Class::WeakRef(v) => {
                // Weak: the target is handled after marking.
                let _ = v;
            }
            Class::FinalizationRegistry(f) => f.trace(t),
            Class::ArrayBuffer(_) => {}
            Class::TypedArray(a) => t.obj(a.buffer),
            Class::DataView(d) => t.obj(d.buffer),
            Class::Proxy(p) => {
                t.opt_obj(p.target);
                t.opt_obj(p.handler);
            }
            Class::Namespace(n) => {
                t.obj(n.module);
                for s in &n.exports {
                    t.str(*s);
                }
            }
            Class::Module(m) => m.trace(t),
            Class::AsyncFromSyncIterator { iterator, next }
            | Class::WrapForValidIterator { iterator, next } => {
                t.value(*iterator);
                t.value(*next);
            }
            Class::RegExpStringIterator(r) => {
                t.value(r.regexp);
                t.value(r.string);
            }
            Class::IteratorHelper(h) => h.trace(t),
            Class::Host(_) => {
                // Host objects reference JS values only through handles,
                // which are roots on their own.
            }
        }
    }
}

impl Environment {
    pub fn trace(&self, t: &mut Tracer) {
        match self {
            Environment::Declarative(d) => {
                t.values(&d.slots);
                if let Some(e) = &d.extra {
                    for b in e.values() {
                        t.value(b.value);
                    }
                }
                t.opt_env(d.parent);
            }
            Environment::Object(o) => {
                t.obj(o.object);
                t.opt_env(o.parent);
            }
            Environment::Global(_) => {}
        }
    }
}

impl Frame {
    pub fn trace(&self, t: &mut Tracer) {
        t.code(&self.code);
        t.value(self.acc);
        t.value(self.this);
        t.value(self.new_target);
        t.opt_obj(self.callee);
        t.opt_env(self.env);
        t.values(&self.args);
        t.opt_obj(self.generator);
        t.opt_obj(self.module);
        t.opt_env(self.base_env);
    }
}

impl Job {
    pub fn trace(&self, t: &mut Tracer) {
        match self {
            Job::Reaction { reaction, argument } => {
                reaction.trace(t);
                t.value(*argument);
            }
            Job::ResolveThenable {
                promise,
                thenable,
                then,
            } => {
                t.obj(*promise);
                t.value(*thenable);
                t.value(*then);
            }
            Job::Call { func, args } => {
                t.value(*func);
                t.values(args);
            }
        }
    }
}

/// What marking leaves for the sweep.
struct Marked {
    objs: Vec<bool>,
    strs: Vec<bool>,
    syms: Vec<bool>,
    bigs: Vec<bool>,
    envs: Vec<bool>,
    shapes: Vec<bool>,
    table_len: Vec<u32>,
    weak: Vec<ObjId>,
    blocks: Vec<Rc<CodeBlock>>,
}

impl Vm {
    /// Collect at a safe point when the heap asks for it.
    #[inline]
    pub fn maybe_collect(&mut self) -> bool {
        if self.native_depth != 0 {
            return false;
        }
        if self.gc_requested || self.heap.wants_gc() {
            self.collect();
            return true;
        }
        false
    }

    /// Run a full collection now. The caller guarantees no unrooted values
    /// are live in Rust locals (see the module docs).
    pub fn collect(&mut self) {
        self.gc_requested = false;
        self.gc_count += 1;
        self.regex_input_cache.clear();
        let marked = self.mark();
        self.heap.objects.restore_marks(marked.objs);
        self.heap.strings.restore_marks(marked.strs);
        self.heap.symbols.restore_marks(marked.syms);
        self.heap.bigints.restore_marks(marked.bigs);
        self.heap.envs.restore_marks(marked.envs);

        // ---- weak processing (needs the mark state) ----
        crate::builtins::collections::process_weak(self, &marked.weak);

        self.sweep(&marked.shapes, &marked.table_len, &marked.blocks);
    }

    /// Mark everything reachable from the roots, ephemerons included.
    fn mark(&mut self) -> Marked {
        let mut shapes = vec![false; self.heap.shapes.len()];
        shapes[0] = true;
        let mut t = Tracer {
            objs: self.heap.objects.take_marks(),
            strs: self.heap.strings.take_marks(),
            syms: self.heap.symbols.take_marks(),
            bigs: self.heap.bigints.take_marks(),
            envs: self.heap.envs.take_marks(),
            shapes,
            table_len: vec![0; self.heap.key_tables.len()],
            work: Vec::with_capacity(1024),
            weak: Vec::new(),
            blocks: Vec::new(),
            vm: &*self,
        };
        let vm = t.vm;

        // ---- roots ----
        for f in &vm.frames {
            f.trace(&mut t);
        }
        t.values(&vm.regs);
        t.values(&vm.roots);
        t.values(&vm.keep_alive);
        if let Some(d) = &vm.debug {
            t.values(&d.roots());
        }
        for s in vm.heap.single_chars.iter().flatten() {
            t.str(*s);
        }
        for s in &vm.heap.pinned {
            t.str(*s);
        }
        for h in vm.handles.iter() {
            t.value(*h);
        }
        for p in &vm.pending_rejections {
            t.obj(*p);
        }
        for (p, _) in &vm.rejection_events {
            t.obj(*p);
        }
        for r in &vm.realms {
            t.obj(r.global);
            t.env(r.global_env);
            for b in r.global_lexicals.values() {
                t.value(b.value);
            }
            for &o in r.intrinsics() {
                t.obj(o);
            }
            for (_, o) in r.template_cache.values() {
                t.obj(*o);
            }
            t.sym(r.intl_fallback);
        }
        for s in vm.names.all() {
            t.str(s);
        }
        for s in vm.well_known.all() {
            t.sym(s);
        }
        for (k, s) in &vm.symbol_registry {
            t.str(*k);
            t.sym(*s);
        }
        for j in &vm.jobs {
            j.trace(&mut t);
        }
        for m in vm.modules.values() {
            t.obj(*m);
        }

        // ---- mark ----
        t.drain();
        t.ephemerons();

        Marked {
            objs: t.objs,
            strs: t.strs,
            syms: t.syms,
            bigs: t.bigs,
            envs: t.envs,
            shapes: t.shapes,
            table_len: t.table_len,
            weak: t.weak,
            blocks: t.blocks,
        }
    }

    /// Free what marking did not reach, keeping small buffers for reuse,
    /// and re-arm the collector from the live size.
    fn sweep(&mut self, shapes: &[bool], table_len: &[u32], blocks: &[Rc<CodeBlock>]) {
        use crate::heap::{ENV_BYTES, OBJECT_BYTES, STRING_BYTES, SYMBOL_BYTES};
        self.heap.prune_intern_table();
        let mut live_bytes = 0usize;
        // A dead watched object's id may be reused by an object an inline
        // cache would mistake for it.
        let mut watched_died = false;
        {
            let heap = &mut self.heap;
            // The program allocates up to the threshold before the next
            // collection, which bounds the buffers it can take from a pool.
            let budget = heap.gc_threshold;
            heap.slot_pool.restart(budget);
            heap.value_pool.restart(budget);
            let slot_pool = &mut heap.slot_pool;
            let value_pool = &mut heap.value_pool;
            let finalizers = &mut self.pending_finalizers;
            heap.objects.sweep(
                |o| live_bytes += OBJECT_BYTES + o.extra_bytes(),
                |mut o| {
                    watched_died |= o.watched;
                    // Host objects: hand dead wrappers' data to their
                    // finalisers.
                    if matches!(o.class, Class::Host(_)) {
                        if let Class::Host(h) = std::mem::replace(&mut o.class, Class::Ordinary) {
                            finalizers.push((h.class, h.data));
                        }
                    }
                    slot_pool.give(std::mem::take(&mut o.slots));
                    if let Elements::Dense(v) = &mut o.elements {
                        value_pool.give(std::mem::take(v));
                    }
                },
            );
            heap.envs.sweep(
                |e| live_bytes += ENV_BYTES + e.slot_count() * 16,
                |e| {
                    if let Environment::Declarative(d) = e {
                        value_pool.give(d.slots);
                    }
                },
            );
            heap.strings.sweep(|s| live_bytes += STRING_BYTES + s.byte_len(), |_| {});
            heap.symbols.sweep(|_| live_bytes += SYMBOL_BYTES, |_| {});
            heap.bigints.sweep(|_| live_bytes += 48, |_| {});
        }
        if watched_died {
            self.heap.bump_proto_epoch();
        }
        if self.heap.sweep_shapes(shapes, table_len) != 0 {
            // Every shape a traced block's entries name survived.
            for b in blocks {
                if let Some(fb) = b.feedback_if(self.feedback_owner) {
                    fb.shape_epoch.set(self.heap.shape_epoch);
                }
            }
        }

        self.heap.live_bytes = live_bytes;
        self.heap.allocated_since_gc = 0;
        self.heap.rearm_threshold();
        self.refresh_attention();
    }

    /// Whether a value survived marking (valid only during weak
    /// processing, between mark and sweep).
    pub(crate) fn is_value_marked(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.heap.objects.is_marked(o.0),
            Value::Symbol(s) => self.heap.symbols.is_marked(s.0),
            _ => true,
        }
    }
}
