//! The collector: precise, non-moving mark-sweep over the arenas.
//!
//! Roots are the interpreter frames and register file, the scoped root
//! stack, the host handle table, every realm, the pre-interned names and
//! well-known symbols, the symbol registry, the job queue, module records
//! and shape keys. Interned strings are weak: dead atoms leave the table.
//!
//! Collection runs only from [`Vm::maybe_collect`] at interpreter safe
//! points (between instructions) and only when no native frame is on the
//! stack, so Rust locals in built-ins never hold stale ids. There is no
//! `unsafe` code here or in the heap; the arenas are plain vectors.

use crate::env::Environment;
use crate::interp::Frame;
use crate::object::{Class, Elements, ObjectData};
use crate::string::Repr;
use crate::value::{BigId, EnvId, ObjId, PropKey, PropSlot, StrId, SymId, Value};
use crate::vm::{Job, Vm};

/// Collects the references a heap value holds.
#[derive(Default)]
pub struct Tracer {
    pub values: Vec<Value>,
    pub envs: Vec<EnvId>,
}

impl Tracer {
    #[inline]
    pub fn value(&mut self, v: Value) {
        match v {
            Value::String(_) | Value::Symbol(_) | Value::BigInt(_) | Value::Object(_) => {
                self.values.push(v)
            }
            _ => {}
        }
    }
    #[inline]
    pub fn obj(&mut self, o: ObjId) {
        self.values.push(Value::Object(o));
    }
    #[inline]
    pub fn opt_obj(&mut self, o: Option<ObjId>) {
        if let Some(o) = o {
            self.values.push(Value::Object(o));
        }
    }
    #[inline]
    pub fn str(&mut self, s: StrId) {
        self.values.push(Value::String(s));
    }
    #[inline]
    pub fn opt_str(&mut self, s: Option<StrId>) {
        if let Some(s) = s {
            self.values.push(Value::String(s));
        }
    }
    #[inline]
    pub fn sym(&mut self, s: SymId) {
        self.values.push(Value::Symbol(s));
    }
    #[inline]
    pub fn env(&mut self, e: EnvId) {
        self.envs.push(e);
    }
    #[inline]
    pub fn opt_env(&mut self, e: Option<EnvId>) {
        if let Some(e) = e {
            self.envs.push(e);
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
}

/// Work items during marking.
enum Work {
    Obj(ObjId),
    Str(StrId),
    Sym(SymId),
    Env(EnvId),
}

impl ObjectData {
    pub fn trace(&self, t: &mut Tracer) {
        t.opt_obj(self.proto);
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
        let mut work: Vec<Work> = Vec::with_capacity(1024);
        let mut buf = Tracer::default();

        // ---- roots ----
        let mut roots = Tracer::default();
        for f in &self.frames {
            f.trace(&mut roots);
        }
        roots.values(&self.regs);
        roots.values(&self.roots);
        roots.values(&self.keep_alive);
        for h in self.handles.iter() {
            roots.value(*h);
        }
        for p in &self.pending_rejections {
            roots.obj(*p);
        }
        for (p, _) in &self.rejection_events {
            roots.obj(*p);
        }
        for r in &self.realms {
            roots.obj(r.global);
            roots.env(r.global_env);
            for b in r.global_lexicals.values() {
                roots.value(b.value);
            }
            for &o in r.intrinsics() {
                roots.obj(o);
            }
            for (_, o) in r.template_cache.values() {
                roots.obj(*o);
            }
        }
        for s in self.names.all() {
            roots.str(s);
        }
        for s in self.well_known.all() {
            roots.sym(s);
        }
        for (k, s) in &self.symbol_registry {
            roots.str(*k);
            roots.sym(*s);
        }
        for j in &self.jobs {
            j.trace(&mut roots);
        }
        for m in self.modules.values() {
            roots.obj(*m);
        }
        for shape in &self.heap.shapes {
            for (k, _) in &shape.keys {
                roots.key(*k);
            }
        }
        self.mark_from(&mut roots, &mut work);

        // ---- mark ----
        while let Some(item) = work.pop() {
            buf.values.clear();
            buf.envs.clear();
            match item {
                Work::Obj(o) => self.heap.obj(o).trace(&mut buf),
                Work::Env(e) => self.heap.env(e).trace(&mut buf),
                Work::Sym(s) => {
                    let d = self.heap.sym(s);
                    buf.opt_str(d.description);
                    match d.private {
                        crate::symbol::PrivateElement::Method(m) => buf.value(m),
                        crate::symbol::PrivateElement::Accessor { get, set } => {
                            buf.value(get);
                            buf.value(set);
                        }
                        crate::symbol::PrivateElement::Field => {}
                    }
                }
                Work::Str(s) => {
                    if let Repr::Rope { left, right, .. } = &self.heap.string(s).repr {
                        buf.str(*left);
                        buf.str(*right);
                    }
                }
            }
            self.mark_from(&mut buf, &mut work);
        }

        // ---- weak processing ----
        crate::builtins::collections::process_weak(self);

        // ---- host objects: hand dead wrappers' data to their finalisers ----
        let dead_hosts: Vec<u32> = self
            .heap
            .objects
            .iter()
            .filter(|(i, o)| matches!(o.class, Class::Host(_)) && !self.heap.objects.is_marked(*i))
            .map(|(i, _)| i)
            .collect();
        for i in dead_hosts {
            let o = self.heap.objects.get_mut(i);
            if let Class::Host(h) = std::mem::replace(&mut o.class, Class::Ordinary) {
                self.pending_finalizers.push((h.class, h.data));
            }
        }

        // ---- sweep ----
        self.heap.prune_intern_table();
        self.heap.objects.sweep(|_, _| {});
        self.heap.strings.sweep(|_, _| {});
        self.heap.symbols.sweep(|_, _| {});
        self.heap.bigints.sweep(|_, _| {});
        self.heap.envs.sweep(|_, _| {});

        self.heap.live_bytes = self.heap.approx_live_bytes();
        self.heap.allocated_since_gc = 0;
        self.heap.gc_threshold = (self.heap.live_bytes * 2).max(8 * 1024 * 1024);
    }

    fn mark_from(&mut self, t: &mut Tracer, work: &mut Vec<Work>) {
        for v in t.values.drain(..) {
            match v {
                Value::Object(o) => {
                    if self.heap.objects.mark(o.0) {
                        work.push(Work::Obj(o));
                    }
                }
                Value::String(s) => {
                    if self.heap.strings.mark(s.0) {
                        work.push(Work::Str(s));
                    }
                }
                Value::Symbol(s) => {
                    if self.heap.symbols.mark(s.0) {
                        work.push(Work::Sym(s));
                    }
                }
                Value::BigInt(b) => {
                    self.mark_bigint(b);
                }
                _ => {}
            }
        }
        for e in t.envs.drain(..) {
            if self.heap.envs.mark(e.0) {
                work.push(Work::Env(e));
            }
        }
    }

    fn mark_bigint(&mut self, b: BigId) {
        self.heap.bigints.mark(b.0);
    }

    /// Whether an object survived marking (valid only during weak
    /// processing, between mark and sweep).
    pub(crate) fn is_marked(&self, o: ObjId) -> bool {
        self.heap.objects.is_marked(o.0)
    }

    pub(crate) fn is_value_marked(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.heap.objects.is_marked(o.0),
            Value::Symbol(s) => self.heap.symbols.is_marked(s.0),
            _ => true,
        }
    }
}
