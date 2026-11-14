//! The virtual machine: heap, realms, frames, roots and the services every
//! other module builds on (error creation, atoms, root scopes).

use crate::heap::Heap;
use crate::interp::Frame;
use crate::object::{CallArgs, Class, NativeData, NativeFn, NativeKind, NativeTag, ObjectData};
use crate::realm::{Intrinsic, Realm};
use crate::value::{Attrs, ObjId, PropKey, Property, ShapeId, StrId, SymId, Value};
use agnijs_engine_api::{EngineId, Host, RealmId};
use hashbrown::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;

/// Why execution stopped abnormally.
#[derive(Clone, Copy, Debug)]
pub enum Fault {
    /// A JavaScript exception carrying the thrown value.
    Throw(Value),
    /// The host's interrupt hook fired; not catchable by JavaScript.
    Interrupt,
    /// The memory limit was exceeded; not catchable by JavaScript.
    OutOfMemory,
    /// The fuel budget was spent (`Vm::set_fuel`); not catchable by
    /// JavaScript.
    OutOfFuel,
}

pub type JsResult<T> = Result<T, Fault>;

/// Error constructors, by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorType {
    Error,
    TypeError,
    RangeError,
    ReferenceError,
    SyntaxError,
    EvalError,
    URIError,
    AggregateError,
    SuppressedError,
}

impl ErrorType {
    pub fn name(self) -> &'static str {
        match self {
            ErrorType::Error => "Error",
            ErrorType::TypeError => "TypeError",
            ErrorType::RangeError => "RangeError",
            ErrorType::ReferenceError => "ReferenceError",
            ErrorType::SyntaxError => "SyntaxError",
            ErrorType::EvalError => "EvalError",
            ErrorType::URIError => "URIError",
            ErrorType::AggregateError => "AggregateError",
            ErrorType::SuppressedError => "SuppressedError",
        }
    }
    pub fn prototype(self) -> Intrinsic {
        match self {
            ErrorType::Error => Intrinsic::ErrorPrototype,
            ErrorType::TypeError => Intrinsic::TypeErrorPrototype,
            ErrorType::RangeError => Intrinsic::RangeErrorPrototype,
            ErrorType::ReferenceError => Intrinsic::ReferenceErrorPrototype,
            ErrorType::SyntaxError => Intrinsic::SyntaxErrorPrototype,
            ErrorType::EvalError => Intrinsic::EvalErrorPrototype,
            ErrorType::URIError => Intrinsic::URIErrorPrototype,
            ErrorType::AggregateError => Intrinsic::AggregateErrorPrototype,
            ErrorType::SuppressedError => Intrinsic::SuppressedErrorPrototype,
        }
    }
    pub fn constructor(self) -> Intrinsic {
        match self {
            ErrorType::Error => Intrinsic::Error,
            ErrorType::TypeError => Intrinsic::TypeError,
            ErrorType::RangeError => Intrinsic::RangeError,
            ErrorType::ReferenceError => Intrinsic::ReferenceError,
            ErrorType::SyntaxError => Intrinsic::SyntaxError,
            ErrorType::EvalError => Intrinsic::EvalError,
            ErrorType::URIError => Intrinsic::URIError,
            ErrorType::AggregateError => Intrinsic::AggregateError,
            ErrorType::SuppressedError => Intrinsic::SuppressedError,
        }
    }
}

/// A pending job (promise reaction or thenable resolution).
pub enum Job {
    Reaction {
        reaction: crate::builtins::promise::Reaction,
        argument: Value,
    },
    ResolveThenable {
        promise: ObjId,
        thenable: Value,
        then: Value,
    },
    /// Finalization callbacks and other host-scheduled work.
    Call { func: Value, args: Vec<Value> },
}

/// Pre-interned atoms for names the engine uses constantly.
macro_rules! names {
    ($($field:ident = $text:expr),* $(,)?) => {
        #[allow(non_snake_case)]
        pub struct Names { $(pub $field: StrId,)* }
        impl Names {
            fn new(heap: &mut Heap) -> Names {
                Names { $($field: heap.atom($text),)* }
            }
            pub fn all(&self) -> Vec<StrId> {
                vec![$(self.$field,)*]
            }
        }
    };
}

names! {
    empty = "",
    length = "length",
    prototype = "prototype",
    constructor = "constructor",
    name = "name",
    message = "message",
    cause = "cause",
    errors = "errors",
    stack = "stack",
    stackTraceLimit = "stackTraceLimit",
    prepareStackTrace = "prepareStackTrace",
    value = "value",
    done = "done",
    next = "next",
    return_ = "return",
    throw = "throw",
    toString = "toString",
    valueOf = "valueOf",
    toJSON = "toJSON",
    get = "get",
    set = "set",
    writable = "writable",
    enumerable = "enumerable",
    configurable = "configurable",
    callee = "callee",
    caller = "caller",
    arguments = "arguments",
    then = "then",
    resolve = "resolve",
    reject = "reject",
    undefined = "undefined",
    null = "null",
    true_ = "true",
    false_ = "false",
    object = "object",
    function = "function",
    number = "number",
    string = "string",
    boolean = "boolean",
    symbol = "symbol",
    bigint = "bigint",
    NaN = "NaN",
    Infinity = "Infinity",
    default = "default",
    raw = "raw",
    index = "index",
    input = "input",
    groups = "groups",
    indices = "indices",
    lastIndex = "lastIndex",
    source = "source",
    flags = "flags",
    global = "global",
    globalThis = "globalThis",
    eval = "eval",
    proto_ = "__proto__",
    anonymous = "anonymous",
    size = "size",
    has = "has",
    add = "add",
    keys = "keys",
    values = "values",
    entries = "entries",
    join = "join",
    byteLength = "byteLength",
    byteOffset = "byteOffset",
    buffer = "buffer",
    maxByteLength = "maxByteLength",
    description = "description",
    status = "status",
    reason = "reason",
    fulfilled = "fulfilled",
    rejected = "rejected",
    async_ = "async",
    with = "with",
    deleteProperty = "deleteProperty",
    defineProperty = "defineProperty",
    getOwnPropertyDescriptor = "getOwnPropertyDescriptor",
    getPrototypeOf = "getPrototypeOf",
    setPrototypeOf = "setPrototypeOf",
    isExtensible = "isExtensible",
    preventExtensions = "preventExtensions",
    ownKeys = "ownKeys",
    apply = "apply",
    construct = "construct",
    exec = "exec",
    dotAll = "dotAll",
    hasIndices = "hasIndices",
    ignoreCase = "ignoreCase",
    multiline = "multiline",
    sticky = "sticky",
    unicode = "unicode",
    unicodeSets = "unicodeSets",
    toISOString = "toISOString",
    Symbol_iterator = "Symbol.iterator",
    Symbol_asyncIterator = "Symbol.asyncIterator",
    Symbol_hasInstance = "Symbol.hasInstance",
    Symbol_toPrimitive = "Symbol.toPrimitive",
    Symbol_toStringTag = "Symbol.toStringTag",
    Symbol_species = "Symbol.species",
    Symbol_isConcatSpreadable = "Symbol.isConcatSpreadable",
    Symbol_unscopables = "Symbol.unscopables",
    Symbol_match = "Symbol.match",
    Symbol_matchAll = "Symbol.matchAll",
    Symbol_replace = "Symbol.replace",
    Symbol_search = "Symbol.search",
    Symbol_split = "Symbol.split",
    Symbol_dispose = "Symbol.dispose",
    Symbol_asyncDispose = "Symbol.asyncDispose",
    IntlLegacyConstructedSymbol = "IntlLegacyConstructedSymbol",
    this_binding = "*this*",
    new_target_binding = "*new.target*",
    func_binding = "*func*",
    default_export = "*default*",
}

/// Well-known symbols, shared by every realm.
pub struct WellKnown {
    pub iterator: SymId,
    pub async_iterator: SymId,
    pub has_instance: SymId,
    pub to_primitive: SymId,
    pub to_string_tag: SymId,
    pub species: SymId,
    pub is_concat_spreadable: SymId,
    pub unscopables: SymId,
    pub match_: SymId,
    pub match_all: SymId,
    pub replace: SymId,
    pub search: SymId,
    pub split: SymId,
    pub dispose: SymId,
    pub async_dispose: SymId,
}

impl WellKnown {
    fn new(heap: &mut Heap, names: &Names) -> WellKnown {
        let mk = |heap: &mut Heap, d: StrId| {
            heap.alloc_symbol(crate::symbol::SymbolData::new(
                Some(d),
                crate::symbol::SymbolKind::WellKnown,
            ))
        };
        WellKnown {
            iterator: mk(heap, names.Symbol_iterator),
            async_iterator: mk(heap, names.Symbol_asyncIterator),
            has_instance: mk(heap, names.Symbol_hasInstance),
            to_primitive: mk(heap, names.Symbol_toPrimitive),
            to_string_tag: mk(heap, names.Symbol_toStringTag),
            species: mk(heap, names.Symbol_species),
            is_concat_spreadable: mk(heap, names.Symbol_isConcatSpreadable),
            unscopables: mk(heap, names.Symbol_unscopables),
            match_: mk(heap, names.Symbol_match),
            match_all: mk(heap, names.Symbol_matchAll),
            replace: mk(heap, names.Symbol_replace),
            search: mk(heap, names.Symbol_search),
            split: mk(heap, names.Symbol_split),
            dispose: mk(heap, names.Symbol_dispose),
            async_dispose: mk(heap, names.Symbol_asyncDispose),
        }
    }
    pub fn all(&self) -> [SymId; 15] {
        [
            self.iterator,
            self.async_iterator,
            self.has_instance,
            self.to_primitive,
            self.to_string_tag,
            self.species,
            self.is_concat_spreadable,
            self.unscopables,
            self.match_,
            self.match_all,
            self.replace,
            self.search,
            self.split,
            self.dispose,
            self.async_dispose,
        ]
    }
}

/// Persistent root slots for the engine interface's handles.
pub type HandleTable = agnijs_engine_api::HandleRegistry<Value>;

/// Limits.
pub const MAX_FRAMES: usize = 20_000;
pub const MAX_NATIVE_DEPTH: u32 = 1200;

pub struct Vm {
    pub heap: Heap,
    pub realms: Vec<Realm>,
    pub frames: Vec<Frame>,
    /// Register file: each frame owns `regs[base .. base + register_count]`.
    pub regs: Vec<Value>,
    /// Scoped roots for values native code holds across allocations.
    pub roots: Vec<Value>,
    pub handles: HandleTable,
    pub host: Rc<dyn Host>,
    /// Depth of native (Rust) frames (built-ins, host entries, jobs) on the
    /// stack. Bounds native recursion; a nested run loop under one records
    /// `retain_from` so the collector can run there.
    pub native_depth: u32,
    /// The native stack budget installed around every call from the host
    /// (`Engine::set_stack_budget`); `None` installs none.
    pub(crate) stack_budget: Option<usize>,
    /// Retention bound of the innermost nested run loop (`Vm::run` entered
    /// while `native_depth > 0`): the heap's allocation serial at entry, or
    /// 0 at the top level. A collection treats every heap value born before
    /// it as a root, which covers everything the Rust frames under the loop
    /// can hold (see `gc.rs`). Nested loops save and restore it.
    pub retain_from: u64,
    /// When the last collection retained garbage for frames that have since
    /// returned, leaving a loop asks for the collection that frees it, but
    /// only from a loop whose `retain_from` is at most this bound: a deeper
    /// loop would retain the same garbage again (`Vm::exit_gc_due`).
    pub(crate) exit_gc_at: Option<u64>,
    /// `retain_from` at the last collection.
    pub(crate) retain_at_gc: u64,
    /// `retain_from` of the loop in which a threshold collection was last
    /// put off (`Vm::maybe_collect`); leaving that loop puts the threshold
    /// back.
    pub(crate) put_off_at: u64,
    /// The callback series a native is running (`Vm::callback_series`):
    /// its native depth and the bound the run loops it calls into retain
    /// from (the enclosing loop's), instead of their own entry.
    pub(crate) series: Option<(u32, u64)>,
    /// `native_depth` when the current run loop was entered: a native at
    /// `run_depth + 1` was called by that loop's instruction directly, with
    /// no other Rust frame holding values in between.
    pub(crate) run_depth: u32,
    /// Set by `CheckDerivedReturn` just before it throws: the throwing
    /// frame's own handlers are skipped once.
    pub(crate) uncatchable: bool,
    /// Instructions JavaScript may still run while `fuel_on` (TASK-103.1,
    /// `Vm::set_fuel`); the dispatch loop charges one per instruction.
    pub(crate) fuel: u64,
    /// A fuel budget is set; mirrored by `heap::ATTENTION_FUEL`.
    pub(crate) fuel_on: bool,
    /// `Math.random`'s xorshift64* state; zero until the first draw seeds
    /// it from `Host::random_seed` (or `Vm::set_random_seed` does).
    pub(crate) rng: u64,
    pub current_realm: RealmId,
    pub names: Names,
    pub well_known: WellKnown,
    /// `Symbol.for` registry, by key atom.
    pub symbol_registry: HashMap<StrId, SymId>,
    pub jobs: VecDeque<Job>,
    pub engine_id: EngineId,
    /// Scripts and source modules by name (`Engine::script_source`), with
    /// their code so an attached debugger can list and break in them.
    pub scripts: HashMap<String, crate::debug::ScriptEntry>,
    /// Module records by resolved specifier (per realm).
    pub modules: HashMap<(RealmId, String), ObjId>,
    pub interrupt_counter: u32,
    /// Objects kept alive by the host (`WeakRef` targets this turn, pending
    /// finalization registries, module records under evaluation).
    pub keep_alive: Vec<Value>,
    /// Promises rejected without a handler, reported after the job queue
    /// drains (`HostPromiseRejectionTracker`).
    pub pending_rejections: Vec<ObjId>,
    /// `(promise, handled)` events for promises reported earlier.
    pub rejection_events: Vec<(ObjId, bool)>,
    /// Orders async module evaluation (`[[AsyncEvaluation]]` order).
    pub async_order_counter: u64,
    /// Frames unwound by the fault in flight (innermost first).
    pub throw_stack: Vec<crate::stack_trace::UnwoundFrame>,
    /// Stack-trace state: host source maps, line tables (`crate::stack_trace`).
    pub stacks: crate::stack_trace::StackState,
    pub throw_stack_value: Option<Value>,
    /// Host classes by id (`register_host_class`).
    pub host_classes: Vec<agnijs_engine_api::HostClassDef>,
    /// Compiled regular expressions by (pattern, flags); see builtins::regexp.
    pub regex_cache: crate::builtins::regexp::RegexCache,
    /// The widened input of a RegExp exec over a short one-byte string,
    /// reused between execs.
    pub regex_scratch: Vec<u16>,
    /// The matcher policy from [`Host::regex_matcher`], fixed at creation.
    pub regex_mode: agnijs_js_regex::MatcherMode,
    /// The UTF-16 form of the last few long strings RegExp exec ran against,
    /// most recent first, so a global match/replace/split or a sticky
    /// tokenizer over a long string stays linear. String ids are reused
    /// only after a collection, which clears this.
    pub regex_input_cache: Vec<(StrId, Rc<Vec<u16>>)>,
    /// Host objects whose wrapper died, awaiting their class finaliser.
    pub pending_finalizers: Vec<(
        agnijs_engine_api::HostClassId,
        Box<dyn agnijs_engine_api::HostObject>,
    )>,
    pub gc_count: u64,
    /// Set by `collect_garbage` requests from the host or `$262.gc()`.
    pub gc_requested: bool,
    /// Debugger state; `None` unless a client is attached.
    pub(crate) debug: Option<Box<crate::debug::DebugState>>,
    /// Set when a panic unwound through a host entry (TASK-103.31); shared
    /// with the unwind guards, which cannot borrow the VM.
    pub(crate) poisoned: Rc<std::cell::Cell<bool>>,
    /// Tested on the dispatch loop's slow path: a client is attached.
    pub(crate) debug_hot: bool,
    /// Reused buffer for the argument list of an inlined
    /// `Function.prototype.apply` (TASK-100.19).
    pub(crate) arg_scratch: Vec<Value>,
    /// This VM's claim on a code block's run-time feedback
    /// (`CodeBlock::feedback`): ids, shapes and keys in it are this VM's.
    pub(crate) feedback_owner: u32,
    /// The shapes a new function object and its prototype object start
    /// with, by [`FnShape`] (`ShapeId(0)` until first used). Rooted, so
    /// creating a function adds its eager properties without a
    /// transition lookup.
    pub(crate) fn_shapes: [ShapeId; FnShape::COUNT],
    /// Inline-cache counters (feature `ic-stats`).
    #[cfg(feature = "ic-stats")]
    pub ic_stats: crate::interp::ic::IcStats,
    /// Instructions dispatched by `Vm::run` (feature `instr-count`).
    #[cfg(feature = "instr-count")]
    pub instr_count: u64,
    /// The baseline JIT's mode, counters and pending exits (feature `jit`).
    #[cfg(feature = "jit")]
    pub(crate) jit: crate::interp::jit::JitState,
}

/// A distinct, non-zero feedback owner per VM.
/// The eager own properties of a new function object (or of a
/// constructor's prototype object), in property order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FnShape {
    /// `length`, `name`: arrows, methods, async functions, class
    /// constructors (their `prototype` comes from the class).
    Plain = 0,
    /// `length`, `name`, `prototype`: strict functions and generators.
    WithPrototype = 1,
    /// `length`, `name`, `caller`, `arguments`, `prototype`: sloppy
    /// functions.
    Sloppy = 2,
    /// A constructor's prototype object: `constructor`.
    ProtoObject = 3,
}

impl FnShape {
    pub(crate) const COUNT: usize = 4;
}

fn next_feedback_owner() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Vm {
    pub fn new(host: Rc<dyn Host>) -> Vm {
        let mut vm = Vm::bare(host);
        let main = vm.create_realm();
        vm.current_realm = main;
        vm
    }

    /// A VM with its atoms and well-known symbols but no realm yet: what a
    /// snapshot is restored into (`crate::snapshot`).
    pub(crate) fn bare(host: Rc<dyn Host>) -> Vm {
        let mut heap = Heap::new();
        heap.shapes.push(crate::object::Shape::root());
        let names = Names::new(&mut heap);
        let well_known = WellKnown::new(&mut heap, &names);
        let engine_id = agnijs_engine_api::next_engine_id();
        let regex_mode = match host.regex_matcher() {
            agnijs_engine_api::RegexMatcher::Auto => agnijs_js_regex::MatcherMode::Auto,
            agnijs_engine_api::RegexMatcher::BacktrackingOnly => agnijs_js_regex::MatcherMode::BacktrackingOnly,
            agnijs_engine_api::RegexMatcher::LinearWhenEligible => {
                agnijs_js_regex::MatcherMode::LinearWhenEligible
            }
        };
        Vm {
            heap,
            realms: Vec::new(),
            frames: Vec::new(),
            regs: Vec::new(),
            roots: Vec::new(),
            handles: HandleTable::new(engine_id),
            host,
            native_depth: 0,
            stack_budget: Some(agnijs_js_parser::stack::DEFAULT_BUDGET),
            retain_from: 0,
            exit_gc_at: None,
            retain_at_gc: 0,
            put_off_at: 0,
            series: None,
            run_depth: 0,
            uncatchable: false,
            fuel: 0,
            fuel_on: false,
            rng: 0,
            current_realm: RealmId(0),
            names,
            well_known,
            symbol_registry: HashMap::new(),
            jobs: VecDeque::new(),
            regex_cache: Default::default(),
            regex_scratch: Vec::new(),
            regex_mode,
            regex_input_cache: Vec::new(),
            engine_id,
            scripts: HashMap::new(),
            modules: HashMap::new(),
            interrupt_counter: 0,
            keep_alive: Vec::new(),
            pending_rejections: Vec::new(),
            rejection_events: Vec::new(),
            async_order_counter: 0,
            throw_stack: Vec::new(),
            stacks: Default::default(),
            throw_stack_value: None,
            host_classes: Vec::new(),
            pending_finalizers: Vec::new(),
            gc_count: 0,
            gc_requested: false,
            debug: None,
            poisoned: Rc::default(),
            debug_hot: false,
            arg_scratch: Vec::new(),
            feedback_owner: next_feedback_owner(),
            fn_shapes: [ShapeId(0); FnShape::COUNT],
            #[cfg(feature = "ic-stats")]
            ic_stats: Default::default(),
            #[cfg(feature = "instr-count")]
            instr_count: 0,
            #[cfg(feature = "jit")]
            jit: crate::interp::jit::JitState::from_env(),
        }
    }

    // ---- realms --------------------------------------------------------------

    #[inline]
    pub fn realm(&self) -> &Realm {
        &self.realms[self.current_realm.0 as usize]
    }

    #[inline]
    pub fn realm_mut(&mut self) -> &mut Realm {
        let r = self.current_realm.0 as usize;
        &mut self.realms[r]
    }

    #[inline]
    pub fn realm_of(&self, id: RealmId) -> &Realm {
        &self.realms[id.0 as usize]
    }

    #[inline]
    pub fn intrinsic(&self, i: Intrinsic) -> ObjId {
        self.realm().intrinsic(i)
    }

    #[inline]
    pub fn intrinsic_in(&self, realm: RealmId, i: Intrinsic) -> ObjId {
        self.realm_of(realm).intrinsic(i)
    }

    // ---- roots ----------------------------------------------------------------

    /// Begin a root scope; pass the result to [`Vm::pop_roots`].
    #[inline]
    pub fn root_mark(&self) -> usize {
        self.roots.len()
    }

    #[inline]
    pub fn root(&mut self, v: Value) -> Value {
        self.roots.push(v);
        v
    }

    #[inline]
    pub fn pop_roots(&mut self, mark: usize) {
        self.roots.truncate(mark);
    }

    // ---- strings -----------------------------------------------------------------

    #[inline]
    pub fn atom(&mut self, s: &str) -> StrId {
        self.heap.atom(s)
    }

    #[inline]
    pub fn key(&mut self, s: &str) -> PropKey {
        if let Some(i) = PropKey::index_from_bytes(s.as_bytes()) {
            return PropKey::Index(i);
        }
        PropKey::Str(self.heap.atom(s))
    }

    #[inline]
    pub fn str(&mut self, s: &str) -> Value {
        Value::String(self.heap.alloc_str(s))
    }

    /// Lower the maximum string length below [`crate::MAX_STRING_LENGTH`]
    /// (never above it), so tests can reach it without gigabytes.
    #[doc(hidden)]
    pub fn set_max_string_length(&mut self, units: usize) {
        self.heap.max_string_len = units.min(crate::MAX_STRING_LENGTH);
    }

    /// The RangeError for a string past the maximum length.
    #[cold]
    #[inline(never)]
    pub fn string_too_long<T>(&mut self) -> JsResult<T> {
        self.range_error("Invalid string length")
    }

    /// A RangeError unless `len` code units fit in one string, and the
    /// memory-limit error unless at least `len` bytes fit under the limit
    /// (a builder checks as it grows, and every string costs at least a
    /// byte per unit).
    #[inline]
    pub fn check_string_len(&mut self, len: usize) -> JsResult<()> {
        if len > self.heap.max_string_len {
            return self.string_too_long();
        }
        self.check_memory(len)
    }

    /// The memory-limit error (`Fault::OutOfMemory`, which JavaScript
    /// cannot catch) if `bytes` of new storage would take the heap past
    /// the limit (`Heap::would_exceed`). Built-ins that allocate a large
    /// block call this first, so one call cannot overshoot the limit by
    /// the size of the block (TASK-103.25).
    #[inline]
    pub fn check_memory(&mut self, bytes: usize) -> JsResult<()> {
        if self.heap.would_exceed(bytes) {
            return Err(Fault::OutOfMemory);
        }
        Ok(())
    }

    /// Check and charge a native list of `n` items of `item` bytes that is
    /// sized by something nothing counted bounds (a string's or a typed
    /// array's length), before it is allocated (TASK-103.53). The charge
    /// lets the polls of the loops that go on to read it see it.
    pub fn charge_list(&mut self, n: usize, item: usize) -> JsResult<()> {
        let bytes = n.saturating_mul(item);
        self.check_memory(bytes)?;
        self.heap.charge(bytes);
        Ok(())
    }

    /// The RangeError the heap's backstop left (`Heap::string_too_long`),
    /// if any, clearing it.
    #[cold]
    pub(crate) fn take_string_too_long(&mut self) -> JsResult<()> {
        if !std::mem::take(&mut self.heap.string_too_long) {
            return Ok(());
        }
        self.refresh_attention();
        self.string_too_long()
    }

    /// The longest string allowed, in code units.
    #[inline]
    pub fn max_string_length(&self) -> usize {
        self.heap.max_string_len
    }

    /// `a` followed by `b`, or a RangeError past the maximum length.
    #[inline]
    pub fn concat(&mut self, a: StrId, b: StrId) -> JsResult<StrId> {
        match self.heap.concat(a, b) {
            Some(s) => {
                if self.heap.memory_limit.is_some() {
                    self.check_rope(s)?;
                }
                Ok(s)
            }
            None => self.string_too_long(),
        }
    }

    /// A long concatenation is a rope, which allocates its flat buffer only
    /// when it is read; the memory limit counts that buffer now, so
    /// doubling a string does not get past the limit as ropes.
    #[cold]
    fn check_rope(&mut self, s: StrId) -> JsResult<()> {
        match self.heap.rope_flat_bytes(s) {
            Some(bytes) => self.check_memory(bytes),
            None => Ok(()),
        }
    }

    /// A string from UTF-16 code units, or a RangeError past the maximum
    /// length.
    pub fn str_from_units(&mut self, units: Vec<u16>) -> JsResult<Value> {
        self.check_string_len(units.len())?;
        Ok(Value::String(self.heap.alloc_str_utf16(units)))
    }

    /// A property key from an existing string id (interning it).
    pub fn key_from_str(&mut self, s: StrId) -> PropKey {
        {
            let u = self.heap.units(s);
            let idx = match u {
                crate::string::Units::One(b) => PropKey::index_from_bytes(b),
                crate::string::Units::Two(w) => PropKey::index_from_units(w),
            };
            if let Some(i) = idx {
                return PropKey::Index(i);
            }
        }
        PropKey::Str(self.heap.intern(s))
    }

    /// The string form of a property key (indices become decimal strings).
    pub fn key_to_string(&mut self, key: PropKey) -> StrId {
        match key {
            PropKey::Index(i) => self.number_to_string(i as f64),
            PropKey::Str(s) => s,
            PropKey::Sym(_) => panic!("key_to_string on a symbol"),
        }
    }

    /// The value form of a property key (string or symbol).
    pub fn key_to_value(&mut self, key: PropKey) -> Value {
        match key {
            PropKey::Sym(s) => Value::Symbol(s),
            k => Value::String(self.key_to_string(k)),
        }
    }

    pub fn display(&mut self, v: Value) -> String {
        match v {
            Value::String(s) => self.heap.str_to_string(s),
            other => format!("{other:?}"),
        }
    }

    // ---- objects -------------------------------------------------------------------

    pub fn alloc(&mut self, data: ObjectData) -> ObjId {
        self.heap.alloc_object(data)
    }

    /// A plain object inheriting from %Object.prototype% of the current realm.
    pub fn new_object(&mut self) -> ObjId {
        let proto = self.intrinsic(Intrinsic::ObjectPrototype);
        self.alloc(ObjectData::new(Some(proto), Class::Ordinary))
    }

    pub fn new_object_with_proto(&mut self, proto: Option<ObjId>) -> ObjId {
        self.alloc(ObjectData::new(proto, Class::Ordinary))
    }

    /// Define a data property directly (storage level, no validation).
    pub fn define_raw(&mut self, obj: ObjId, key: PropKey, value: Value, attrs: Attrs) {
        if self.heap.has_own_property(obj, key) {
            self.heap
                .replace_own_property(obj, key, Property::data(value, attrs));
        } else {
            self.heap
                .add_own_property(obj, key, Property::data(value, attrs));
        }
    }

    pub fn define_raw_str(&mut self, obj: ObjId, name: &str, value: Value, attrs: Attrs) {
        let k = self.key(name);
        self.define_raw(obj, k, value, attrs);
    }

    pub fn define_accessor_raw(
        &mut self,
        obj: ObjId,
        key: PropKey,
        get: Value,
        set: Value,
        attrs: Attrs,
    ) {
        let p = Property::accessor(get, set, attrs.with_writable(false));
        if self.heap.has_own_property(obj, key) {
            self.heap.replace_own_property(obj, key, p);
        } else {
            self.heap.add_own_property(obj, key, p);
        }
    }

    /// Create a native function object in the current realm.
    pub fn new_native(&mut self, name: &str, length: u32, f: NativeFn) -> ObjId {
        let realm = self.current_realm;
        self.new_native_in(realm, name, length, f, false)
    }

    pub fn new_native_ctor(&mut self, name: &str, length: u32, f: NativeFn) -> ObjId {
        let realm = self.current_realm;
        self.new_native_in(realm, name, length, f, true)
    }

    pub fn new_native_in(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: NativeFn,
        constructor: bool,
    ) -> ObjId {
        let proto = self.intrinsic_in(realm, Intrinsic::FunctionPrototype);
        let data = NativeData {
            func: NativeKind::Fn(f),
            tag: NativeTag::None,
            realm,
            constructor,
            slots: Vec::new(),
            flag: None,
            counter: None,
        };
        let obj = self.alloc(ObjectData::new(Some(proto), Class::Native(Box::new(data))));
        let len_key = PropKey::Str(self.names.length);
        self.heap.add_own_property(
            obj,
            len_key,
            Property::data(Value::from_u32(length), Attrs::CONFIGURABLE_ONLY),
        );
        let name_key = PropKey::Str(self.names.name);
        // Anonymous natives (resolving functions, element closures) share
        // the empty atom instead of allocating an empty string each.
        let name_val = if name.is_empty() {
            Value::String(self.names.empty)
        } else {
            self.str(name)
        };
        self.heap.add_own_property(
            obj,
            name_key,
            Property::data(name_val, Attrs::CONFIGURABLE_ONLY),
        );
        obj
    }

    /// A native function whose `name` is a string value (symbol-derived
    /// names like `[Symbol.iterator]`).
    pub fn new_native_named(&mut self, name: Value, length: u32, f: NativeFn) -> ObjId {
        let realm = self.current_realm;
        let proto = self.intrinsic_in(realm, Intrinsic::FunctionPrototype);
        let data = NativeData {
            func: NativeKind::Fn(f),
            tag: NativeTag::None,
            realm,
            constructor: false,
            slots: Vec::new(),
            flag: None,
            counter: None,
        };
        let obj = self.alloc(ObjectData::new(Some(proto), Class::Native(Box::new(data))));
        let len_key = PropKey::Str(self.names.length);
        self.heap.add_own_property(
            obj,
            len_key,
            Property::data(Value::from_u32(length), Attrs::CONFIGURABLE_ONLY),
        );
        let name_key = PropKey::Str(self.names.name);
        self.heap.add_own_property(
            obj,
            name_key,
            Property::data(name, Attrs::CONFIGURABLE_ONLY),
        );
        obj
    }

    /// A native closure with captured slots.
    pub fn new_native_closure(
        &mut self,
        name: &str,
        length: u32,
        f: NativeFn,
        slots: Vec<Value>,
    ) -> ObjId {
        let obj = self.new_native(name, length, f);
        self.heap.obj_mut(obj).native_mut().unwrap().slots = slots;
        obj
    }

    /// Set captured slot `i` of a native function (growing the slot list).
    pub fn set_native_slot(&mut self, func: ObjId, i: usize, v: Value) {
        let n = self.heap.obj_mut(func).native_mut().unwrap();
        if n.slots.len() <= i {
            n.slots.resize(i + 1, Value::Undefined);
        }
        n.slots[i] = v;
    }

    /// Captured slot `i` of the running native closure.
    pub fn native_slot(&self, callee: ObjId, i: usize) -> Value {
        self.heap
            .obj(callee)
            .native()
            .map(|n| n.slots.get(i).copied().unwrap_or(Value::Undefined))
            .unwrap_or(Value::Undefined)
    }

    /// Install a method on `obj`.
    pub fn method(&mut self, obj: ObjId, name: &str, length: u32, f: NativeFn) -> ObjId {
        let func = self.new_native(name, length, f);
        let key = self.key(name);
        self.heap
            .add_own_property(obj, key, Property::data(Value::Object(func), Attrs::METHOD));
        func
    }

    /// Install a symbol-keyed method on `obj`.
    pub fn method_sym(
        &mut self,
        obj: ObjId,
        sym: SymId,
        display: &str,
        length: u32,
        f: NativeFn,
    ) -> ObjId {
        let name = self.str(&format!("[{display}]"));
        let func = self.new_native_named(name, length, f);
        self.heap.add_own_property(
            obj,
            PropKey::Sym(sym),
            Property::data(Value::Object(func), Attrs::METHOD),
        );
        func
    }

    /// Install an accessor (getter and optional setter) on `obj`.
    pub fn accessor(&mut self, obj: ObjId, name: &str, getter: NativeFn, setter: Option<NativeFn>) {
        let gname = format!("get {name}");
        let g = self.new_native(&gname, 0, getter);
        let s = match setter {
            Some(f) => {
                let sname = format!("set {name}");
                Value::Object(self.new_native(&sname, 1, f))
            }
            None => Value::Undefined,
        };
        let key = self.key(name);
        self.heap.add_own_property(
            obj,
            key,
            Property::accessor(Value::Object(g), s, Attrs::CONFIGURABLE_ONLY),
        );
    }

    pub fn accessor_sym(&mut self, obj: ObjId, sym: SymId, display: &str, getter: NativeFn) {
        let gname = self.str(&format!("get [{display}]"));
        let g = self.new_native_named(gname, 0, getter);
        self.heap.add_own_property(
            obj,
            PropKey::Sym(sym),
            Property::accessor(Value::Object(g), Value::Undefined, Attrs::CONFIGURABLE_ONLY),
        );
    }

    /// Install a constant data property.
    pub fn constant(&mut self, obj: ObjId, name: &str, value: Value) {
        let key = self.key(name);
        self.heap
            .add_own_property(obj, key, Property::data(value, Attrs::NONE));
    }

    // ---- errors -----------------------------------------------------------------

    /// Create an error object of the given type in the current realm.
    pub fn make_error(&mut self, ty: ErrorType, message: &str) -> Value {
        let proto = self.intrinsic(ty.prototype());
        let obj = self.alloc(ObjectData::new(Some(proto), Class::Error(None)));
        self.attach_error_stack(obj);
        let msg = self.str(message);
        let key = PropKey::Str(self.names.message);
        self.heap
            .add_own_property(obj, key, Property::data(msg, Attrs::METHOD));
        Value::Object(obj)
    }

    pub fn throw(&mut self, ty: ErrorType, message: &str) -> Fault {
        Fault::Throw(self.make_error(ty, message))
    }

    /// An error whose prototype comes from `realm` rather than the current
    /// realm.
    pub fn throw_in(&mut self, realm: RealmId, ty: ErrorType, message: &str) -> Fault {
        let saved = self.current_realm;
        self.current_realm = realm;
        let e = self.make_error(ty, message);
        self.current_realm = saved;
        Fault::Throw(e)
    }

    pub fn throw_type(&mut self, message: &str) -> Fault {
        self.throw(ErrorType::TypeError, message)
    }

    pub fn throw_range(&mut self, message: &str) -> Fault {
        self.throw(ErrorType::RangeError, message)
    }

    pub fn throw_reference(&mut self, message: &str) -> Fault {
        self.throw(ErrorType::ReferenceError, message)
    }

    pub fn throw_syntax(&mut self, message: &str) -> Fault {
        self.throw(ErrorType::SyntaxError, message)
    }

    /// Convenience: `Err(TypeError)`.
    pub fn type_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_type(message))
    }

    pub fn range_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_range(message))
    }

    pub fn reference_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_reference(message))
    }

    pub fn syntax_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_syntax(message))
    }

    // ---- host -----------------------------------------------------------------------

    /// Poll the host's interrupt hook every so often. The interpreter polls
    /// at function entry, at loop back-edges and after a call returns (ADR
    /// 0019 J7), and native built-ins once per iteration of a loop over
    /// their input (TASK-103.28); the host hook itself runs once per 4096
    /// polls.
    #[inline]
    pub fn poll_interrupt(&mut self) -> JsResult<()> {
        self.interrupt_counter = self.interrupt_counter.wrapping_add(1);
        if self.interrupt_counter & 0xfff == 0 {
            return self.ask_host_interrupt();
        }
        Ok(())
    }

    /// The poll for native loops (TASK-103.53): the interrupt, as
    /// [`Vm::poll_interrupt`], and the memory limit. A native loop runs no
    /// safe point, so without this a loop that makes values from a bare
    /// length or a native iterator allocates its whole result before the
    /// limit is checked. Once the counter passes the collection threshold
    /// it fails with `OutOfMemory` when `Heap::would_exceed` refuses nothing
    /// more, so one call takes at most twice the limit, garbage included.
    /// Rust-side lists such a loop builds must be charged as they grow
    /// (`Heap::charge`) for the counter to see them.
    #[inline]
    pub fn poll(&mut self) -> JsResult<()> {
        if self.heap.attention & crate::heap::ATTENTION_GC != 0 {
            self.check_memory(0)?;
        }
        self.poll_interrupt()
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn ask_host_interrupt(&mut self) -> JsResult<()> {
        if self.host.interrupt() {
            return Err(Fault::Interrupt);
        }
        // A native's poll is where a requested debugger pause reaches a
        // long native call (TASK-103.33); the interpreter's own polls leave
        // it to `debug_check` at the next instruction.
        if self.debug.is_some() {
            self.pause_in_native();
        }
        Ok(())
    }

    /// Set the fuel budget (TASK-103.1): how many more bytecode instructions
    /// may be dispatched, or `None` for no budget. See
    /// [`agnijs_engine_api::Engine::set_fuel`]. A budget raises
    /// `ATTENTION_FUEL`, so the dispatch loop without one is unchanged and
    /// compiled code is never entered while one is set (the count is the
    /// interpreter's, whatever the tier).
    pub fn set_fuel(&mut self, fuel: Option<u64>) {
        self.fuel_on = fuel.is_some();
        self.fuel = fuel.unwrap_or(0);
        self.refresh_attention();
    }

    /// Reseed `Math.random` (TASK-103.1, deterministic mode); see
    /// [`agnijs_engine_api::Engine::set_random_seed`].
    pub fn set_random_seed(&mut self, seed: u64) {
        self.rng = crate::builtins::math::rng_state(seed);
    }

    /// The fuel left, or `None` when no budget is set.
    pub fn fuel(&self) -> Option<u64> {
        self.fuel_on.then_some(self.fuel)
    }

    /// Charge the instruction about to be dispatched to the fuel budget.
    #[inline]
    pub(crate) fn charge_fuel(&mut self) -> JsResult<()> {
        if self.fuel_on {
            if self.fuel == 0 {
                return Err(Fault::OutOfFuel);
            }
            self.fuel -= 1;
        }
        Ok(())
    }

    /// Ask for a collection at the next safe point.
    pub(crate) fn request_gc(&mut self) {
        self.gc_requested = true;
        self.heap.attention |= crate::heap::ATTENTION_GC;
    }

    /// A collection asked for by a loop exit is due at this safe point:
    /// the current loop retains no more than the loop that asked.
    #[inline]
    pub(crate) fn exit_gc_due(&self) -> bool {
        matches!(self.exit_gc_at, Some(at) if self.retain_from <= at)
    }

    /// Recompute the attention word (`Heap::attention`, ADR 0019 J7) from
    /// its sources.
    pub(crate) fn refresh_attention(&mut self) {
        let mut a = 0;
        if self.debug_hot {
            a |= crate::heap::ATTENTION_DEBUG;
        }
        if self.fuel_on {
            a |= crate::heap::ATTENTION_FUEL;
        }
        if self.heap.string_too_long {
            a |= crate::heap::ATTENTION_STRING;
        }
        if self.heap.stress
            || self.gc_requested
            || self.exit_gc_due()
            || self.heap.allocated_since_gc >= self.heap.gc_threshold
        {
            a |= crate::heap::ATTENTION_GC;
        }
        self.heap.attention = a;
    }

    /// Run `f`, a native's loop of calls back into JavaScript, as a
    /// callback series: the run loops it enters directly retain only what
    /// the enclosing loop retains, so a collection in one callback frees
    /// the garbage the earlier ones left and everything else the native's
    /// caller has dropped. `roots` are the heap values the native keeps in
    /// its locals for the whole series; they go on the root stack
    /// (`Vm::roots`) until `f` returns. The native promises that between
    /// its calls it holds no other heap value except through the heap, the
    /// arguments of the call in progress, or a value it pushed on
    /// `Vm::roots` itself (the stack is cut back to its length at entry).
    /// Nested series save and restore the enclosing one; deeper natives are
    /// not covered and retain from their own entry. A series native that
    /// was itself called by another native (`Reflect.apply`, a bound
    /// function's target...), whose Rust frame may hold fresh values,
    /// retains everything born before now instead of inheriting the
    /// enclosing loop's bound.
    pub(crate) fn callback_series<T>(&mut self, roots: &[Value], f: impl FnOnce(&mut Vm) -> T) -> T {
        let base = self.roots.len();
        self.roots.extend_from_slice(roots);
        let bound = if self.native_depth == self.run_depth + 1 {
            self.retain_from
        } else {
            self.heap.alloc_serial
        };
        let saved = self.series.replace((self.native_depth, bound));
        let r = f(self);
        self.series = saved;
        self.roots.truncate(base);
        r
    }

    /// Fails with a RangeError when an embedder's stack budget
    /// (`agnijs_js_parser::stack`) is used up; called by built-ins that
    /// recurse on their input (JSON, `flat`, proxies, bound functions).
    /// Without a budget it never fails.
    #[inline]
    pub fn check_stack(&mut self) -> JsResult<()> {
        if agnijs_js_parser::stack::exhausted() {
            return Err(self.throw_range("Maximum call stack size exceeded"));
        }
        Ok(())
    }

    /// Run `f` as a native frame (bounds native recursion; see
    /// `native_depth`).
    #[inline]
    pub fn in_native<T>(&mut self, f: impl FnOnce(&mut Vm) -> JsResult<T>) -> JsResult<T> {
        // An embedder's stack budget (`agnijs_js_parser::stack`) bounds it
        // too, for hosts that run on small thread stacks.
        if self.native_depth >= MAX_NATIVE_DEPTH || agnijs_js_parser::stack::exhausted() {
            return Err(self.throw_range("Maximum call stack size exceeded"));
        }
        self.native_depth += 1;
        let r = f(self);
        self.native_depth -= 1;
        r
    }

    /// Call a native function object.
    pub fn call_native(
        &mut self,
        callee: ObjId,
        this: Value,
        args: &[Value],
        new_target: Value,
    ) -> JsResult<Value> {
        let (func, realm) = match &self.heap.obj(callee).class {
            Class::Native(n) => (
                match &n.func {
                    NativeKind::Fn(f) => NativeKind::Fn(*f),
                    NativeKind::Host(h) => NativeKind::Host(h.clone()),
                },
                n.realm,
            ),
            _ => unreachable!("call_native on a non-native"),
        };
        let saved_realm = self.current_realm;
        self.current_realm = realm;
        let r = self.in_native(|vm| match func {
            NativeKind::Fn(f) => f(
                vm,
                &CallArgs {
                    this,
                    args,
                    new_target,
                    callee,
                },
            ),
            NativeKind::Host(h) => {
                crate::engine::call_host_function(vm, &h, this, args, new_target)
            }
        });
        self.current_realm = saved_realm;
        r
    }
}
