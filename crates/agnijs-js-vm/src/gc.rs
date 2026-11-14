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
//! points (between instructions), native frames on the stack or not
//! (TASK-100.16). Rust code (a built-in calling back, a host call, a job,
//! module evaluation, a resumed generator) reaches JavaScript only through
//! a nested run loop, and everything its locals can hold exists when that
//! loop is entered: the loop records the heap's allocation serial
//! (`Vm::retain_from`), and a collection under it treats every heap value
//! born before the innermost such bound as a root, tracing it like any
//! other. Values born inside the loop reach Rust only when the loop
//! returns, after which the next loop entry covers them. So no built-in
//! has to root its locals by hand, the collector stays precise and
//! non-moving, and nothing scans the Rust stack (ADR 0019 J6). Frames in
//! the same loop hold nothing across a safe point by construction: the
//! dispatch loop writes its state back before one. What the retention
//! keeps that no frame holds is garbage until a collection at an outer
//! level, which a loop exit asks for when the amount is not trivial
//! (`Vm::exit_gc_at`: it runs at the next safe point at or above that
//! level, not in a deeper loop that would keep the same garbage); the
//! pacing counts it out, and a collection wanted inside a loop that has
//! allocated little of the garbage so far is put off (within a bound) for
//! the loop to end.
//!
//! The natives that call back once per element opt out of the bound with
//! a callback series (`Vm::callback_series`): they root what they hold on
//! `Vm::roots` and their callbacks retain from the enclosing loop's bound,
//! so a collection in one callback frees the earlier ones' garbage. The
//! audit for each is at its call site; in short, between calls each holds
//! only its receiver, callback and `thisArg` (rooted for the series), the
//! key of the element in hand (an index, or a string rooted for the
//! iteration), a result array made before the series (rooted), and the
//! element or result being passed on, which lives in the arguments of the
//! call in progress or is rooted across a define that may run a trap:
//! `Array.prototype.forEach`, `map`, `filter`, `some`, `every`, `find`,
//! `findIndex`, `findLast`, `findLastIndex`, `Map.prototype.forEach` and
//! `Set.prototype.forEach`. Natives that hold a fresh value across a call
//! (`reduce` holds the accumulator while reading the next element through
//! a getter, `Array.from` holds the iterator result, `replace` the match)
//! keep the plain bound. `AGNIJS_GC_TRACE=1` prints a line per collection
//! with the depth, bounds, retained bytes and the exit frames on the stack.
//! There is no `unsafe` code here or in the heap; the arenas are plain
//! vectors.

use std::rc::Rc;

use agnijs_js_compiler::bytecode::{CodeBlock, ConstString, IcEntry};

use crate::env::Environment;
use crate::interp::Frame;
use crate::object::{Class, Elements, ObjectData};
use crate::string::Repr;
use crate::value::{BigId, EnvId, ObjId, PropKey, PropSlot, ShapeId, StrId, SymId, Value};
use crate::vm::{Job, Vm};

/// `AGNIJS_GC_TRACE=1`: print one line per collection to stderr.
fn gc_trace() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("AGNIJS_GC_TRACE").is_some_and(|v| v != "0"))
}

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
                self.value(get.value());
                self.value(set.value());
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
        // String constants materialised since the last collection survive
        // this one with the block, so a loop does not allocate them again.
        let prev = vm.gc_count - 1;
        for cell in fb.strings.iter().chain(std::iter::once(&fb.name)) {
            let e = cell.get();
            if e.epoch == prev {
                self.str(StrId(e.id));
                cell.set(ConstString { id: e.id, epoch: vm.gc_count });
            }
        }
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
                } else if e.kind == IcEntry::KIND_OWN_ADD {
                    self.shape(ShapeId(e.shape));
                    self.shape(ShapeId(e.holder));
                }
            }
        }
        self.blocks.push(code.clone());
    }

    /// Marked slots per arena: objects, strings, symbols, BigInts,
    /// environments.
    fn marked_counts(&self) -> [usize; 5] {
        let count = |m: &[bool]| m.iter().filter(|&&b| b).count();
        [
            count(&self.objs),
            count(&self.strs),
            count(&self.syms),
            count(&self.bigs),
            count(&self.envs),
        ]
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

    /// Whether a heap value has been reached; values that are not on the
    /// heap count as reached. A WeakMap value that is a string or a bigint
    /// has to be marked like any other, so those arenas are checked too.
    #[inline]
    fn is_marked(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.objs[o.0 as usize],
            Value::Symbol(s) => self.syms[s.0 as usize],
            Value::String(s) => self.strs[s.0 as usize],
            Value::BigInt(b) => self.bigs[b.0 as usize],
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
                    for (k, v) in m.iter() {
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
        t.opt_obj(self.proto());
        t.shape(self.shape);
        for s in self.slots() {
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
            Class::Error(s) => {
                if let Some(s) = s {
                    s.trace(t);
                }
            }
            Class::Ordinary
            | Class::RawJSON
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
            #[cfg(feature = "intl")]
            Class::Intl(d) => d.trace(t),
            Class::MapIterator(i) | Class::SetIterator(i) => t.opt_obj(i.target),
            Class::WeakRef(v) => {
                // Weak: the target is handled after marking.
                let _ = v;
            }
            Class::FinalizationRegistry(f) => f.trace(t),
            Class::DisposableStack(d) => d.trace(t),
            Class::CallSite(d) => d.trace(t),
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
            Class::AsyncFromSyncIterator(r) | Class::WrapForValidIterator(r) => {
                t.value(r.iterator);
                t.value(r.next);
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
    /// Estimated bytes marked only by the retention pass.
    retained_bytes: usize,
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
    ///
    /// A threshold collection inside a nested run loop that has allocated
    /// less than half of what was allocated since the last collection
    /// would retain more than it frees, so it is put off until the loop
    /// ends or twice the threshold has been allocated, whichever is first.
    /// Requested and stress collections run at once.
    #[inline]
    pub fn maybe_collect(&mut self) -> bool {
        if self.gc_requested || self.exit_gc_due() {
            self.collect();
            return true;
        }
        if !self.heap.wants_gc() {
            return false;
        }
        if !self.heap.stress && self.retain_from != 0 && self.put_off_collection() {
            return false;
        }
        self.collect();
        true
    }

    /// Whether a threshold collection under the current loop should wait
    /// (see [`Vm::maybe_collect`]); when it should, the threshold is raised
    /// so the dispatch loop's fast path is kept until then, and put back
    /// when this loop (or an enclosing one) ends (`Vm::run`).
    #[cold]
    fn put_off_collection(&mut self) -> bool {
        let heap = &mut self.heap;
        let in_loop = heap.alloc_serial - self.retain_from;
        let since_gc = heap.alloc_serial - heap.serial_at_gc;
        if in_loop * 2 >= since_gc {
            return false;
        }
        let bound = heap.gc_threshold_base * 2;
        if heap.allocated_since_gc >= bound {
            return false;
        }
        if let Some(limit) = heap.memory_limit {
            if heap.live_bytes + bound > limit {
                return false;
            }
        }
        heap.gc_threshold = bound;
        self.put_off_at = self.retain_from;
        true
    }

    /// Run a full collection now. The caller guarantees that Rust locals
    /// hold no heap value born after `retain_from` (see the module docs).
    /// With `AGNIJS_GC_TRACE=1` every collection prints one line to stderr.
    pub fn collect(&mut self) {
        let trace = gc_trace();
        let started = trace.then(std::time::Instant::now);
        let allocated = self.heap.allocated_since_gc;
        self.gc_requested = false;
        self.gc_count += 1;
        self.regex_input_cache.clear();
        let marked = self.mark();
        if let Some(t0) = started {
            // Every exit frame (the one a native or the host called into)
            // with the JS frame under it, whose instruction called that
            // native; innermost first.
            let stack: Vec<String> = self
                .frames
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, f)| f.exit)
                .map(|(i, f)| {
                    let caller = i.checked_sub(1).map(|j| &self.frames[j]).map(|c| {
                        let (line, col) = Vm::line_col(&c.code, c.pc);
                        format!("{}@{}:{}", c.code.name, line, col)
                    });
                    format!("{i}:{}>{}", caller.unwrap_or_default(), f.code.name)
                })
                .collect();
            eprintln!(
                "gc#{} depth={} run_depth={} series={:?} retain_from={} alloc_serial={} allocated={} live={} retained={} mark_us={} stack={}",
                self.gc_count,
                self.native_depth,
                self.run_depth,
                self.series,
                self.retain_from,
                self.heap.alloc_serial,
                allocated,
                self.heap.live_bytes,
                marked.retained_bytes,
                t0.elapsed().as_micros(),
                stack.join(",")
            );
        }
        self.heap.serial_at_gc = self.heap.alloc_serial;
        self.retain_at_gc = self.retain_from;
        if self.exit_gc_due() {
            self.exit_gc_at = None;
        }
        self.heap.objects.restore_marks(marked.objs);
        self.heap.strings.restore_marks(marked.strs);
        self.heap.symbols.restore_marks(marked.syms);
        self.heap.bigints.restore_marks(marked.bigs);
        self.heap.envs.restore_marks(marked.envs);

        // ---- weak processing (needs the mark state) ----
        crate::builtins::collections::process_weak(self, &marked.weak);

        self.heap.retained_bytes = marked.retained_bytes;
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
        // What compiled frames hold outside the register file, through
        // their handles (the GC maps, TASK-101.3.1): never the native stack.
        #[cfg(feature = "jit")]
        for jf in &vm.jit.frames {
            jf.handle.live(&mut |v| t.value(v));
        }
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
        for &s in &vm.fn_shapes {
            t.shape(s);
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

        // ---- retention for the Rust frames under a nested run loop ----
        // Everything born before the innermost loop's entry may be held in
        // a Rust local (module docs); what was not reached from the roots
        // is kept and traced like a root. Its size, estimated from the
        // arena counts, is left out of the pacing basis.
        let mut retained_bytes = 0;
        if vm.retain_from != 0 {
            use crate::heap::{BIGINT_BYTES, ENV_BYTES, OBJECT_BYTES, STRING_BYTES, SYMBOL_BYTES};
            let before = t.marked_counts();
            let from = vm.retain_from;
            for i in vm.heap.objects.born_before(from) {
                t.obj(ObjId(i));
            }
            for i in vm.heap.strings.born_before(from) {
                t.str(StrId(i));
            }
            for i in vm.heap.symbols.born_before(from) {
                t.sym(SymId(i));
            }
            for i in vm.heap.bigints.born_before(from) {
                t.bigint(BigId(i));
            }
            for i in vm.heap.envs.born_before(from) {
                t.env(EnvId(i));
            }
            t.drain();
            let after = t.marked_counts();
            retained_bytes = (after[0] - before[0]) * OBJECT_BYTES
                + (after[1] - before[1]) * STRING_BYTES
                + (after[2] - before[2]) * SYMBOL_BYTES
                + (after[3] - before[3]) * BIGINT_BYTES
                + (after[4] - before[4]) * ENV_BYTES;
        }
        t.ephemerons();

        Marked {
            retained_bytes,
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
            heap.fn_pool.clear();
            // No more functions than objects fit under the threshold.
            let fn_pool_max = budget / OBJECT_BYTES;
            let fn_pool = &mut heap.fn_pool;
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
                    if matches!(o.class, Class::Function(_)) && fn_pool.len() < fn_pool_max {
                        if let Class::Function(f) = std::mem::replace(&mut o.class, Class::Ordinary) {
                            fn_pool.push(f);
                        }
                    }
                    slot_pool.give(o.take_slots());
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
            heap.bigints.sweep(|b| live_bytes += b.heap_bytes(), |_| {});
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
