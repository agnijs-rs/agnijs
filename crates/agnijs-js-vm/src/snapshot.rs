//! Realm snapshots (TASK-101.2, ADR 0019 J11): the heap reachable from one
//! initialised realm, written as data and restored into a new VM, so an
//! embedder's startup (built-ins plus its bootstrap scripts) becomes a load.
//!
//! What a snapshot holds: the realm (global object, global environment and
//! lexicals, intrinsics, template objects, `var` names), every object,
//! environment, string, symbol and BigInt reachable from it, the code
//! blocks its functions and environments run (each tree once, through the
//! bytecode serializer, so inline caches and feedback load empty and JIT
//! code is never part of it), the registered symbols it reaches and the
//! scripts recorded for `Engine::script_source` and a later debugger.
//!
//! Built-in functions are Rust function pointers, which cannot be written.
//! They go by index into the native registry: the natives of a fresh realm
//! in creation order, built once per process ([`registry`]) and
//! fingerprinted (names, lengths, tags, the engine version, the bytecode
//! format and the `intl` feature), so a snapshot from another build is
//! refused as a version mismatch before anything is decoded.
//!
//! What it refuses, with [`Error::SnapshotUnsupported`] and the path to
//! the offending value: host objects and host functions (no serialisation
//! hook yet), natives with captured state that is not a constant of the
//! registry (promise resolving functions, `Promise.all` element closures,
//! bound Intl methods, revocable-proxy revokers: the `Rc<Cell>` state of
//! ADR 0019 J11), promises with reactions, generators, modules, iterators
//! and the other internal states listed in [`unsupported_class`], objects
//! of another realm, and a VM that is running, has pending jobs or has
//! modules loaded in the realm. Handles are per engine and not part of a
//! snapshot.
//!
//! Loading treats the bytes as hostile. The container tag and the
//! registry fingerprint are checked first, then a checksum over the body;
//! every count is bounded by the bytes left, every index is checked
//! against its table, every class payload against its invariants (array
//! lengths, typed-array bounds, callable accessors, acyclic prototype,
//! environment, bound-function and proxy chains, environments that match
//! their scopes, intrinsics of the expected class), and anything wrong is
//! [`Error::BytecodeCorrupt`]. No input makes the loader panic. Code blocks
//! carry the bytecode loader's trust model (ADR 0010): the serializer
//! verifies what indexes a block's own pools; operands that depend on run
//! time context are taken as produced, except environment accesses, which
//! fault instead of panicking when a snapshot pairs code with the wrong
//! environment.
//!
//! Layout, little-endian, inside the `AGJS` container with tag
//! [`format_tag`]: the registry fingerprint (u64), a checksum of the body
//! (u64), then the body: the table counts, strings, BigInts, code trees,
//! symbols, objects, environments, the realm and the scripts. Objects and
//! environments are numbered in discovery order from the realm's roots, so
//! the same heap writes the same bytes.

use crate::bigint::BigIntData;
use crate::builtins::collections::MapData;
use crate::builtins::promise::{PromiseData, PromiseStatus, Reactions};
use crate::builtins::proxy::ProxyData;
use crate::builtins::typedarray::{BufferData, DataViewData, ElementKind, TypedArrayData};
use crate::env::{DeclEnv, Environment, ExtraBinding, ObjectEnv};
use crate::object::{
    ArrayData, BoundData, ClassData, Class, Elements, FieldDef, FieldKey, FunctionData, NativeData, NativeFn,
    NativeKind, NativeTag, ObjectData,
};
use crate::realm::{Intrinsic, LexBinding, Realm};
use crate::string::Units;
use crate::symbol::{PrivateElement, SymbolData, SymbolKind};
use crate::value::{Attrs, BigId, EnvId, ObjId, PropKey, PropSlot, Property, StrId, SymId, Value};
use crate::vm::Vm;
use agnijs_engine_api::{self as api, Error, RealmId};
use agnijs_js_compiler::bytecode::{BindingKind, CodeBlock, CodeKind, Constant};
use agnijs_js_debugger::ScriptKind;
use hashbrown::HashMap;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::OnceLock;

/// The snapshot layout version. Bump it with any change to the layout
/// below; the container tag also names the bytecode format.
pub const VERSION: &str = "agnijs-snap-1";

/// The container's format tag: the snapshot layout and the bytecode format
/// its code trees use.
pub fn format_tag() -> String {
    format!("{VERSION}+{}", crate::engine::FORMAT)
}

// ---- the native registry ------------------------------------------------------------------------

struct NativeEntry {
    f: NativeFn,
    tag: NativeTag,
    constructor: bool,
    /// Captured constants (typed-array constructors and DataView methods
    /// carry their element kind); never heap references.
    slots: Vec<Value>,
}

struct Registry {
    entries: Vec<NativeEntry>,
    /// The class code of each intrinsic of a fresh realm.
    intrinsics: Vec<u8>,
    /// The class code of a fresh realm's global object.
    global: u8,
    fingerprint: u64,
}

struct NullHost;
impl api::Host for NullHost {}

/// The natives of a fresh realm, in creation order, deduplicated. Built
/// once per process, at the cost of one realm creation.
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(build_registry)
}

fn build_registry() -> Registry {
    let mut vm = Vm::new(Rc::new(NullHost));
    let mut fp = Fnv::new();
    fp.str(VERSION);
    fp.str(crate::engine::FORMAT);
    fp.str(env!("CARGO_PKG_VERSION"));
    fp.u64(cfg!(feature = "intl") as u64);
    let mut entries: Vec<NativeEntry> = Vec::new();
    let ids: Vec<u32> = vm.heap.objects.iter().map(|(i, _)| i).collect();
    for i in ids {
        let o = vm.heap.obj(ObjId(i));
        let Class::Native(n) = &o.class else { continue };
        let NativeKind::Fn(f) = n.func else { continue };
        if !n.slots.iter().all(|v| is_constant(*v)) {
            continue;
        }
        let (tag, constructor, slots) = (n.tag, n.constructor, n.slots.clone());
        if entries.iter().any(|e| same_native(e, f, tag, constructor, &slots)) {
            continue;
        }
        let name_key = PropKey::Str(vm.names.name);
        let len_key = PropKey::Str(vm.names.length);
        let name = vm.heap.get_own_property(ObjId(i), name_key).and_then(|p| p.value());
        let len = vm.heap.get_own_property(ObjId(i), len_key).and_then(|p| p.value());
        match name {
            Some(Value::String(s)) => {
                let units = vm.heap.units(s).to_vec();
                fp.u64(units.len() as u64);
                for u in units {
                    fp.u64(u as u64);
                }
            }
            _ => fp.u64(u64::MAX),
        }
        fp.u64(len.and_then(|v| v.as_number()).map(f64::to_bits).unwrap_or(0));
        fp.u64(tag_code(tag) as u64 | (constructor as u64) << 8 | (slots.len() as u64) << 16);
        for s in &slots {
            fp.u64(constant_bits(*s));
        }
        entries.push(NativeEntry {
            f,
            tag,
            constructor,
            slots,
        });
    }
    let realm = vm.realm_of(RealmId(0));
    let intrinsics: Vec<u8> = realm
        .intrinsics()
        .iter()
        .map(|&o| class_code(&vm.heap.obj(o).class))
        .collect();
    let global = class_code(&vm.heap.obj(realm.global).class);
    fp.u64(entries.len() as u64);
    for &c in &intrinsics {
        fp.u64(c as u64);
    }
    Registry {
        entries,
        intrinsics,
        global,
        fingerprint: fp.finish(),
    }
}

fn same_native(e: &NativeEntry, f: NativeFn, tag: NativeTag, constructor: bool, slots: &[Value]) -> bool {
    e.f as usize == f as usize
        && e.tag == tag
        && e.constructor == constructor
        && e.slots.len() == slots.len()
        && e.slots.iter().zip(slots).all(|(a, b)| constant_bits(*a) == constant_bits(*b))
}

/// A slot value that names no heap value.
fn is_constant(v: Value) -> bool {
    matches!(v, Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_))
}

fn constant_bits(v: Value) -> u64 {
    match v {
        Value::Undefined => 1,
        Value::Null => 2,
        Value::Bool(b) => 3 + b as u64,
        Value::Number(n) => n.to_bits() ^ 0x5bd1_e995,
        _ => 0,
    }
}

fn tag_code(t: NativeTag) -> u8 {
    match t {
        NativeTag::None => 0,
        NativeTag::FunctionCall => 1,
        NativeTag::FunctionApply => 2,
        NativeTag::RegExpExec => 3,
    }
}

/// FNV-1a over 64-bit words.
struct Fnv(u64);

impl Fnv {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn u64(&mut self, v: u64) {
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        let mut words = b.chunks_exact(8);
        for w in &mut words {
            let mut a = [0u8; 8];
            a.copy_from_slice(w);
            self.u64(u64::from_le_bytes(a));
        }
        let rest = words.remainder();
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.u64(u64::from_le_bytes(last));
        self.u64(b.len() as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

fn checksum(body: &[u8]) -> u64 {
    let mut h = Fnv::new();
    h.bytes(body);
    h.finish()
}

// ---- class codes --------------------------------------------------------------------------------

mod code {
    pub const ORDINARY: u8 = 0;
    pub const ARRAY: u8 = 1;
    pub const FUNCTION: u8 = 2;
    pub const NATIVE: u8 = 3;
    pub const BOUND: u8 = 4;
    pub const ERROR: u8 = 5;
    pub const BOOLEAN: u8 = 6;
    pub const NUMBER: u8 = 7;
    pub const STRING: u8 = 8;
    pub const SYMBOL: u8 = 9;
    pub const BIGINT: u8 = 10;
    pub const ARGUMENTS: u8 = 11;
    pub const DATE: u8 = 12;
    pub const REGEXP: u8 = 13;
    pub const MAP: u8 = 14;
    pub const SET: u8 = 15;
    pub const WEAK_MAP: u8 = 16;
    pub const WEAK_SET: u8 = 17;
    pub const PROMISE: u8 = 18;
    pub const ARRAY_BUFFER: u8 = 19;
    pub const TYPED_ARRAY: u8 = 20;
    pub const DATA_VIEW: u8 = 21;
    pub const PROXY: u8 = 22;
    pub const WEAK_REF: u8 = 23;
    /// Classes a snapshot does not carry (only compared, never written).
    pub const UNSUPPORTED: u8 = 100;
}

fn class_code(c: &Class) -> u8 {
    match c {
        Class::Ordinary => code::ORDINARY,
        Class::Array(_) => code::ARRAY,
        Class::Function(_) => code::FUNCTION,
        Class::Native(_) => code::NATIVE,
        Class::Bound(_) => code::BOUND,
        Class::Error => code::ERROR,
        Class::Boolean(_) => code::BOOLEAN,
        Class::Number(_) => code::NUMBER,
        Class::String(_) => code::STRING,
        Class::Symbol(_) => code::SYMBOL,
        Class::BigInt(_) => code::BIGINT,
        Class::Arguments(None) => code::ARGUMENTS,
        Class::Date(_) => code::DATE,
        Class::RegExp(_) => code::REGEXP,
        Class::Map(_) => code::MAP,
        Class::Set(_) => code::SET,
        Class::WeakMap(_) => code::WEAK_MAP,
        Class::WeakSet(_) => code::WEAK_SET,
        Class::Promise(_) => code::PROMISE,
        Class::ArrayBuffer(_) => code::ARRAY_BUFFER,
        Class::TypedArray(_) => code::TYPED_ARRAY,
        Class::DataView(_) => code::DATA_VIEW,
        Class::Proxy(_) => code::PROXY,
        Class::WeakRef(_) => code::WEAK_REF,
        _ => code::UNSUPPORTED,
    }
}

/// Why an object's class cannot go into a snapshot, if it cannot.
fn unsupported_class(c: &Class) -> Option<&'static str> {
    Some(match c {
        Class::Arguments(Some(_)) => "a mapped arguments object",
        Class::ForIn(_) => "a for-in iterator",
        Class::ArrayIterator(_) => "an array iterator",
        Class::StringIterator { .. } => "a string iterator",
        Class::Generator(_) => "a generator",
        Class::MapIterator(_) | Class::SetIterator(_) => "a Map or Set iterator",
        Class::FinalizationRegistry(_) => "a FinalizationRegistry",
        Class::Namespace(_) => "a module namespace",
        Class::Module(_) => "a module record",
        Class::AsyncFromSyncIterator(_) => "an async-from-sync iterator",
        Class::RegExpStringIterator(_) => "a RegExp string iterator",
        Class::IteratorHelper(_) => "an iterator helper",
        Class::WrapForValidIterator(_) => "a wrapped iterator",
        Class::Host(_) => "a host object (host classes have no snapshot hook)",
        #[cfg(feature = "intl")]
        Class::Intl(_) => "an Intl service object",
        _ => return None,
    })
}

fn element_kind_code(k: ElementKind) -> u8 {
    match k {
        ElementKind::Int8 => 0,
        ElementKind::Uint8 => 1,
        ElementKind::Uint8Clamped => 2,
        ElementKind::Int16 => 3,
        ElementKind::Uint16 => 4,
        ElementKind::Int32 => 5,
        ElementKind::Uint32 => 6,
        ElementKind::Float16 => 7,
        ElementKind::Float32 => 8,
        ElementKind::Float64 => 9,
        ElementKind::BigInt64 => 10,
        ElementKind::BigUint64 => 11,
    }
}

fn element_kind(c: u8) -> Option<ElementKind> {
    Some(match c {
        0 => ElementKind::Int8,
        1 => ElementKind::Uint8,
        2 => ElementKind::Uint8Clamped,
        3 => ElementKind::Int16,
        4 => ElementKind::Uint16,
        5 => ElementKind::Int32,
        6 => ElementKind::Uint32,
        7 => ElementKind::Float16,
        8 => ElementKind::Float32,
        9 => ElementKind::Float64,
        10 => ElementKind::BigInt64,
        11 => ElementKind::BigUint64,
        _ => return None,
    })
}

const BINDING_KINDS: [BindingKind; 10] = [
    BindingKind::Var,
    BindingKind::Let,
    BindingKind::Const,
    BindingKind::Class,
    BindingKind::Function,
    BindingKind::Param,
    BindingKind::CatchParam,
    BindingKind::This,
    BindingKind::Internal,
    BindingKind::FuncName,
];

fn binding_code(k: BindingKind) -> u8 {
    BINDING_KINDS.iter().position(|&b| b == k).unwrap_or(0) as u8
}

const SCRIPT_KINDS: [ScriptKind; 4] = [ScriptKind::Script, ScriptKind::Module, ScriptKind::Eval, ScriptKind::Function];

fn script_code(k: ScriptKind) -> u8 {
    SCRIPT_KINDS.iter().position(|&s| s == k).unwrap_or(0) as u8
}

// ---- value tags ---------------------------------------------------------------------------------

const V_UNDEFINED: u8 = 0;
const V_NULL: u8 = 1;
const V_FALSE: u8 = 2;
const V_TRUE: u8 = 3;
const V_NUMBER: u8 = 4;
const V_STRING: u8 = 5;
const V_SYMBOL: u8 = 6;
const V_BIGINT: u8 = 7;
const V_OBJECT: u8 = 8;
const V_EMPTY: u8 = 9;

const K_INDEX: u8 = 0;
const K_STR: u8 = 1;
const K_SYM: u8 = 2;

const NONE: u32 = u32::MAX;

// ---- writing ------------------------------------------------------------------------------------

/// Where a value was first reached from, for refusal messages.
#[derive(Clone, Copy)]
enum From {
    Root(&'static str),
    Obj(u32, Edge),
    Env(u32, Edge),
}

#[derive(Clone, Copy)]
enum Edge {
    Key(PropKey),
    Internal(&'static str),
}

/// A numbering of heap ids in discovery order.
struct Table {
    index: HashMap<u32, u32>,
    order: Vec<u32>,
}

impl Table {
    fn new() -> Table {
        Table {
            index: HashMap::new(),
            order: Vec::new(),
        }
    }
    /// The number of `id`, and whether it is new.
    fn add(&mut self, id: u32) -> (u32, bool) {
        if let Some(&i) = self.index.get(&id) {
            return (i, false);
        }
        let i = self.order.len() as u32;
        self.index.insert(id, i);
        self.order.push(id);
        (i, true)
    }
    fn get(&self, id: u32) -> u32 {
        self.index[&id]
    }
}

struct Writer<'v> {
    vm: &'v mut Vm,
    realm: RealmId,
    objects: Table,
    envs: Table,
    strings: Table,
    symbols: Table,
    bigints: Table,
    obj_from: Vec<From>,
    env_from: Vec<From>,
    /// Referenced code blocks, unique by address, in discovery order.
    code: Vec<Rc<CodeBlock>>,
    code_index: HashMap<usize, u32>,
    native_index: HashMap<(usize, u8, bool), Vec<u32>>,
    /// Objects and environments still to scan.
    pending_objects: usize,
    pending_envs: usize,
}

fn refuse<T>(m: String) -> Result<T, Error> {
    Err(Error::SnapshotUnsupported(m))
}

impl<'v> Writer<'v> {
    fn obj(&mut self, o: ObjId, from: From) {
        let (_, new) = self.objects.add(o.0);
        if new {
            self.obj_from.push(from);
        }
    }

    fn env(&mut self, e: EnvId, from: From) {
        let (_, new) = self.envs.add(e.0);
        if new {
            self.env_from.push(from);
        }
    }

    fn value(&mut self, v: Value, from: From) {
        match v {
            Value::Object(o) => self.obj(o, from),
            Value::String(s) => {
                self.strings.add(s.0);
            }
            Value::Symbol(s) => self.sym(s, from),
            Value::BigInt(b) => {
                self.bigints.add(b.0);
            }
            _ => {}
        }
    }

    fn sym(&mut self, s: SymId, from: From) {
        let (_, new) = self.symbols.add(s.0);
        if new {
            let d = self.vm.heap.sym(s);
            let desc = d.description;
            let private = d.private;
            if let Some(desc) = desc {
                self.strings.add(desc.0);
            }
            match private {
                PrivateElement::Field => {}
                PrivateElement::Method(v) => self.value(v, from),
                PrivateElement::Accessor { get, set } => {
                    self.value(get, from);
                    self.value(set, from);
                }
            }
        }
    }

    fn key(&mut self, k: PropKey, from: From) {
        match k {
            PropKey::Index(_) => {}
            PropKey::Str(s) => {
                self.strings.add(s.0);
            }
            PropKey::Sym(s) => self.sym(s, from),
        }
    }

    fn slot(&mut self, s: PropSlot, from: From) {
        match s {
            PropSlot::Data(v) => self.value(v, from),
            PropSlot::Accessor { get, set } => {
                self.value(get.value(), from);
                self.value(set.value(), from);
            }
        }
    }

    fn code(&mut self, c: &Rc<CodeBlock>) {
        let addr = Rc::as_ptr(c) as usize;
        if !self.code_index.contains_key(&addr) {
            self.code_index.insert(addr, self.code.len() as u32);
            self.code.push(c.clone());
        }
    }

    fn path(&mut self, mut at: From) -> String {
        let mut parts: Vec<String> = Vec::new();
        loop {
            match at {
                From::Root(r) => {
                    parts.push(r.to_string());
                    break;
                }
                From::Obj(i, edge) => {
                    parts.push(self.edge(edge));
                    at = self.obj_from[i as usize];
                }
                From::Env(i, edge) => {
                    parts.push(self.edge(edge));
                    at = self.env_from[i as usize];
                }
            }
            if parts.len() > 32 {
                parts.push("...".into());
                break;
            }
        }
        parts.reverse();
        parts.concat()
    }

    fn edge(&mut self, e: Edge) -> String {
        match e {
            Edge::Key(PropKey::Index(i)) => format!("[{i}]"),
            Edge::Key(PropKey::Str(s)) => format!(".{}", self.vm.heap.str_to_string(s)),
            Edge::Key(PropKey::Sym(s)) => match self.vm.heap.sym(s).description {
                Some(d) => format!("[{}]", self.vm.heap.str_to_string(d)),
                None => "[symbol]".into(),
            },
            Edge::Internal(n) => format!(".[[{n}]]"),
        }
    }

    fn refuse_obj<T>(&mut self, i: u32, what: &str) -> Result<T, Error> {
        let at = self.obj_from[i as usize];
        let p = self.path(at);
        refuse(format!("{what} at {p}"))
    }

    /// Discover everything reachable, refusing what cannot be written.
    fn scan(&mut self) -> Result<(), Error> {
        loop {
            if self.pending_objects < self.objects.order.len() {
                let i = self.pending_objects as u32;
                self.pending_objects += 1;
                self.scan_object(i)?;
            } else if self.pending_envs < self.envs.order.len() {
                let i = self.pending_envs as u32;
                self.pending_envs += 1;
                self.scan_env(i)?;
            } else {
                return Ok(());
            }
        }
    }

    fn scan_object(&mut self, i: u32) -> Result<(), Error> {
        let id = ObjId(self.objects.order[i as usize]);
        let at = |e| From::Obj(i, e);
        let o = self.vm.heap.obj(id);
        if let Some(what) = unsupported_class(&o.class) {
            return self.refuse_obj(i, what);
        }
        let proto = o.proto();
        let mut named: Vec<(PropKey, PropSlot)> = Vec::new();
        match &o.dict {
            Some(d) => named.extend(d.iter().map(|(k, p)| (*k, p.slot))),
            None => named.extend(self.vm.heap.shape_keys(o.shape).iter().map(|(k, _)| *k).zip(o.slots().copied())),
        }
        let elements: Vec<(u32, PropSlot)> = match &o.elements {
            Elements::Dense(v) => v
                .iter()
                .enumerate()
                .filter(|(_, x)| !x.is_empty())
                .map(|(i, x)| (i as u32, PropSlot::Data(*x)))
                .collect(),
            Elements::Sparse(m) => m.iter().map(|(k, p)| (*k, p.slot)).collect(),
        };
        // The class payload's references.
        let mut refs: Vec<(Value, &'static str)> = Vec::new();
        let mut envs: Vec<(EnvId, &'static str)> = Vec::new();
        let mut keys: Vec<PropKey> = Vec::new();
        let mut code: Option<Rc<CodeBlock>> = None;
        let realm = self.realm;
        match &o.class {
            Class::Function(f) => {
                if f.realm != realm {
                    return self.refuse_obj(i, "a function of another realm");
                }
                if f.module.is_some() {
                    return self.refuse_obj(i, "a module function");
                }
                code = Some(f.code.clone());
                if let Some(e) = f.env {
                    envs.push((e, "Environment"));
                }
                if let Some(h) = f.home_object {
                    refs.push((Value::Object(h), "HomeObject"));
                }
                if let Some(cd) = &f.class_data {
                    for field in &cd.fields {
                        match field.key {
                            FieldKey::Prop(k) => keys.push(k),
                            FieldKey::Private(s) => refs.push((Value::Symbol(s), "Fields")),
                        }
                        if let Some(init) = field.init {
                            refs.push((Value::Object(init), "Fields"));
                        }
                    }
                    for &s in &cd.private_methods {
                        refs.push((Value::Symbol(s), "PrivateMethods"));
                    }
                }
            }
            Class::Native(n) => {
                if n.realm != realm {
                    return self.refuse_obj(i, "a built-in of another realm");
                }
                let NativeKind::Fn(f) = n.func else {
                    return self.refuse_obj(i, "a host function (host functions have no snapshot hook)");
                };
                if n.flag.is_some() || n.counter.is_some() || !n.slots.iter().all(|v| is_constant(*v)) {
                    return self.refuse_obj(
                        i,
                        "a built-in closure with captured state (a promise resolving function, a \
                         Promise.all element, a bound Intl method or a proxy revoker)",
                    );
                }
                let (tag, constructor, slots) = (n.tag, n.constructor, n.slots.clone());
                if self.find_native(f, tag, constructor, &slots).is_none() {
                    return self.refuse_obj(i, "a built-in function the native registry does not know");
                }
            }
            Class::Bound(b) => {
                refs.push((Value::Object(b.target), "BoundTargetFunction"));
                refs.push((b.this, "BoundThis"));
                refs.extend(b.args.iter().map(|v| (*v, "BoundArguments")));
            }
            Class::String(s) => refs.push((Value::String(*s), "StringData")),
            Class::Symbol(s) => refs.push((Value::Symbol(*s), "SymbolData")),
            Class::BigInt(b) => refs.push((Value::BigInt(*b), "BigIntData")),
            Class::RegExp(r) => {
                refs.push((Value::String(r.source), "OriginalSource"));
                refs.push((Value::String(r.flags), "OriginalFlags"));
            }
            Class::Map(m) | Class::Set(m) | Class::WeakMap(m) | Class::WeakSet(m) => {
                for (k, v) in m.entries.iter().flatten() {
                    refs.push((*k, "MapData"));
                    refs.push((*v, "MapData"));
                }
            }
            Class::Promise(p) => {
                if !matches!(p.reactions, Reactions::None) {
                    return self.refuse_obj(i, "a promise with pending reactions");
                }
                refs.push((p.result, "PromiseResult"));
            }
            Class::TypedArray(t) => refs.push((Value::Object(t.buffer), "ViewedArrayBuffer")),
            Class::DataView(d) => refs.push((Value::Object(d.buffer), "ViewedArrayBuffer")),
            Class::Proxy(p) => {
                if let Some(t) = p.target {
                    refs.push((Value::Object(t), "ProxyTarget"));
                }
                if let Some(h) = p.handler {
                    refs.push((Value::Object(h), "ProxyHandler"));
                }
            }
            Class::WeakRef(v) => refs.push((*v, "WeakRefTarget")),
            _ => {}
        }
        if let Some(p) = proto {
            self.obj(p, at(Edge::Internal("Prototype")));
        }
        for (k, s) in named {
            self.key(k, at(Edge::Key(k)));
            self.slot(s, at(Edge::Key(k)));
        }
        for (k, s) in elements {
            self.slot(s, at(Edge::Key(PropKey::Index(k))));
        }
        for (v, n) in refs {
            self.value(v, at(Edge::Internal(n)));
        }
        for (e, n) in envs {
            self.env(e, at(Edge::Internal(n)));
        }
        for k in keys {
            self.key(k, at(Edge::Internal("Fields")));
        }
        if let Some(c) = code {
            self.code(&c);
        }
        Ok(())
    }

    fn find_native(&mut self, f: NativeFn, tag: NativeTag, constructor: bool, slots: &[Value]) -> Option<u32> {
        if self.native_index.is_empty() {
            for (i, e) in registry().entries.iter().enumerate() {
                self.native_index
                    .entry((e.f as usize, tag_code(e.tag), e.constructor))
                    .or_default()
                    .push(i as u32);
            }
        }
        let candidates = self.native_index.get(&(f as usize, tag_code(tag), constructor))?;
        candidates
            .iter()
            .copied()
            .find(|&i| same_native(&registry().entries[i as usize], f, tag, constructor, slots))
    }

    fn scan_env(&mut self, i: u32) -> Result<(), Error> {
        let id = EnvId(self.envs.order[i as usize]);
        let at = |n| From::Env(i, Edge::Internal(n));
        let mut values: Vec<Value> = Vec::new();
        let mut code: Option<Rc<CodeBlock>> = None;
        let (parent, object) = match self.vm.heap.env(id) {
            Environment::Declarative(d) => {
                values.extend(d.slots.iter().copied());
                if let Some(extra) = &d.extra {
                    values.extend(extra.values().map(|b| b.value));
                }
                code = d.scope.as_ref().map(|(c, _)| c.clone());
                (d.parent, None)
            }
            Environment::Object(o) => (o.parent, Some(o.object)),
            Environment::Global(r) => {
                if *r != self.realm {
                    let p = self.path(self.env_from[i as usize]);
                    return refuse(format!("the global environment of another realm at {p}"));
                }
                (None, None)
            }
        };
        for v in values {
            self.value(v, at("Bindings"));
        }
        if let Some(o) = object {
            self.obj(o, at("BindingObject"));
        }
        if let Some(p) = parent {
            self.env(p, at("OuterEnv"));
        }
        if let Some(c) = code {
            self.code(&c);
        }
        Ok(())
    }
}

/// Byte output.
#[derive(Default)]
struct Out {
    b: Vec<u8>,
}

impl Out {
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }
    fn text(&mut self, s: &str) {
        self.len(s.len());
        self.b.extend_from_slice(s.as_bytes());
    }
}

impl Vm {
    /// Write a snapshot of `realm` (see the module documentation).
    pub fn write_snapshot(&mut self, realm: RealmId) -> Result<Vec<u8>, Error> {
        if realm.0 as usize >= self.realms.len() {
            return Err(Error::UnknownRealm);
        }
        if !self.frames.is_empty() || self.native_depth != 0 {
            return refuse("a snapshot is taken between calls, not while JavaScript runs".into());
        }
        if !self.jobs.is_empty() {
            return refuse("jobs are pending; run them first".into());
        }
        if self.modules.keys().any(|(r, _)| *r == realm) {
            return refuse("the realm has modules loaded".into());
        }
        let mut w = Writer {
            vm: self,
            realm,
            objects: Table::new(),
            envs: Table::new(),
            strings: Table::new(),
            symbols: Table::new(),
            bigints: Table::new(),
            obj_from: Vec::new(),
            env_from: Vec::new(),
            code: Vec::new(),
            code_index: HashMap::new(),
            native_index: HashMap::new(),
            pending_objects: 0,
            pending_envs: 0,
        };
        // Roots, in a fixed order.
        let r = w.vm.realm_of(realm);
        let global = r.global;
        let global_env = r.global_env;
        let intl_fallback = r.intl_fallback;
        let intrinsics: Vec<ObjId> = r.intrinsics().to_vec();
        let lexicals: Vec<(Rc<str>, LexBinding)> = r.global_lexicals.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let mut templates: Vec<(Rc<CodeBlock>, u32, ObjId)> =
            r.template_cache.iter().map(|((_, c), (code, o))| (code.clone(), *c, *o)).collect();
        let script_names = r.script_names.clone();
        let mut var_names: Vec<Rc<str>> = r.var_names.iter().cloned().collect();
        var_names.sort();
        w.obj(global, From::Root("globalThis"));
        w.env(global_env, From::Root("the global environment"));
        for (name, b) in &lexicals {
            let _ = name;
            w.value(b.value, From::Root("a global lexical binding"));
        }
        for (i, &o) in intrinsics.iter().enumerate() {
            let _ = i;
            w.obj(o, From::Root("an intrinsic"));
        }
        for (_, _, o) in &templates {
            w.obj(*o, From::Root("a template object"));
        }
        w.sym(intl_fallback, From::Root("the realm"));
        let mut scripts: Vec<(String, Rc<CodeBlock>, ScriptKind)> = w
            .vm
            .scripts
            .iter()
            .filter(|(_, s)| s.realm == realm && s.kind != ScriptKind::Module)
            .map(|(n, s)| (n.clone(), s.code.clone(), s.kind))
            .collect();
        scripts.sort_by(|a, b| a.0.cmp(&b.0));
        w.scan()?;
        for (_, c, _) in &scripts {
            w.code(c);
        }
        for (c, _, _) in &templates {
            w.code(c);
        }

        // Code: the referenced blocks not inside another referenced
        // block's tree are the roots; a reference is a block's position in
        // the preorder walk of all root trees.
        let mut covered: hashbrown::HashSet<usize> = hashbrown::HashSet::new();
        for c in &w.code {
            let mut stack: Vec<&Rc<CodeBlock>> = c.functions.iter().collect();
            while let Some(b) = stack.pop() {
                if covered.insert(Rc::as_ptr(b) as usize) {
                    stack.extend(b.functions.iter());
                }
            }
        }
        let roots: Vec<Rc<CodeBlock>> = w
            .code
            .iter()
            .filter(|c| !covered.contains(&(Rc::as_ptr(c) as usize)))
            .cloned()
            .collect();
        let mut block_index: HashMap<usize, u32> = HashMap::new();
        for root in &roots {
            for b in preorder(root) {
                let n = block_index.len() as u32;
                block_index.insert(Rc::as_ptr(&b) as usize, n);
            }
        }
        let cref = |c: &Rc<CodeBlock>| block_index[&(Rc::as_ptr(c) as usize)];

        // Symbols name their registry keys.
        let registered: HashMap<u32, StrId> = w.vm.symbol_registry.iter().map(|(k, s)| (s.0, *k)).collect();
        for &s in &w.symbols.order {
            if let Some(k) = registered.get(&s) {
                w.strings.add(k.0);
            }
        }
        let well_known = w.vm.well_known.all();

        let mut out = Out::default();
        out.len(w.strings.order.len());
        out.len(w.bigints.order.len());
        out.len(roots.len());
        out.len(w.symbols.order.len());
        out.len(w.objects.order.len());
        out.len(w.envs.order.len());
        // Strings.
        let strings = w.strings.order.clone();
        for s in strings {
            let atom = w.vm.heap.string(StrId(s)).atom;
            match w.vm.heap.units(StrId(s)) {
                Units::One(b) => {
                    out.u8(atom as u8);
                    out.len(b.len());
                    out.b.extend_from_slice(b);
                }
                Units::Two(u) => {
                    out.u8(atom as u8 | 2);
                    out.len(u.len());
                    for x in u {
                        out.b.extend_from_slice(&x.to_le_bytes());
                    }
                }
            }
        }
        for &b in &w.bigints.order {
            let bytes = w.vm.heap.bigint(BigId(b)).0.to_signed_bytes_le();
            out.len(bytes.len());
            out.b.extend_from_slice(&bytes);
        }
        for root in &roots {
            let bytes = agnijs_js_compiler::serialize::encode(root);
            out.len(bytes.len());
            out.b.extend_from_slice(&bytes);
        }
        let mut natives: Vec<u32> = Vec::with_capacity(w.objects.order.len());
        for i in 0..w.objects.order.len() {
            let id = ObjId(w.objects.order[i]);
            let n = match &w.vm.heap.obj(id).class {
                Class::Native(n) => match n.func {
                    NativeKind::Fn(f) => Some((f, n.tag, n.constructor, n.slots.clone())),
                    _ => None,
                },
                _ => None,
            };
            natives.push(match n {
                Some((f, tag, constructor, slots)) => w.find_native(f, tag, constructor, &slots).unwrap_or(0),
                None => 0,
            });
        }
        let ctx = Ctx {
            objects: &w.objects,
            envs: &w.envs,
            strings: &w.strings,
            symbols: &w.symbols,
            bigints: &w.bigints,
        };
        for &s in &w.symbols.order {
            let d = w.vm.heap.sym(SymId(s));
            match d.kind {
                SymbolKind::WellKnown => {
                    out.u8(0);
                    let i = well_known.iter().position(|w| w.0 == s).unwrap_or(0);
                    out.u8(i as u8);
                }
                SymbolKind::Registered => {
                    out.u8(1);
                    let key = registered.get(&s).copied().or(d.description).map(|k| k.0);
                    match key {
                        Some(k) => out.u32(ctx.strings.get(k)),
                        None => out.u32(NONE),
                    }
                }
                SymbolKind::Normal | SymbolKind::Private => {
                    out.u8(if d.kind == SymbolKind::Normal { 2 } else { 3 });
                    out.u32(d.description.map(|x| ctx.strings.get(x.0)).unwrap_or(NONE));
                    match d.private {
                        PrivateElement::Field => out.u8(0),
                        PrivateElement::Method(v) => {
                            out.u8(1);
                            ctx.value(&mut out, v);
                        }
                        PrivateElement::Accessor { get, set } => {
                            out.u8(2);
                            ctx.value(&mut out, get);
                            ctx.value(&mut out, set);
                        }
                    }
                }
            }
        }
        for (&o, &entry) in w.objects.order.iter().zip(&natives) {
            write_object(w.vm, &ctx, &cref, &mut out, ObjId(o), entry);
        }
        for &e in &w.envs.order {
            match w.vm.heap.env(EnvId(e)) {
                Environment::Declarative(d) => {
                    out.u8(0);
                    match &d.scope {
                        Some((c, idx)) => {
                            out.u32(cref(c));
                            out.u32(*idx);
                        }
                        None => {
                            out.u32(NONE);
                            out.u8(d.is_var_scope as u8);
                        }
                    }
                    out.len(d.slots.len());
                    for &v in &d.slots {
                        ctx.value(&mut out, v);
                    }
                    out.u32(d.parent.map(|p| w.envs.get(p.0)).unwrap_or(NONE));
                    match &d.extra {
                        Some(x) => {
                            out.len(x.len());
                            for (name, b) in x.iter() {
                                out.text(name);
                                ctx.value(&mut out, b.value);
                                out.u8(binding_code(b.kind));
                                out.u8(b.deletable as u8);
                            }
                        }
                        None => out.len(0),
                    }
                }
                Environment::Object(oe) => {
                    out.u8(1);
                    out.u32(ctx.objects.get(oe.object.0));
                    out.u8(oe.with as u8);
                    out.u32(oe.parent.map(|p| w.envs.get(p.0)).unwrap_or(NONE));
                }
                Environment::Global(_) => out.u8(2),
            }
        }
        // The realm.
        out.u32(ctx.objects.get(global.0));
        out.u32(w.envs.get(global_env.0));
        out.u32(ctx.symbols.get(intl_fallback.0));
        out.len(intrinsics.len());
        for o in &intrinsics {
            out.u32(ctx.objects.get(o.0));
        }
        out.len(lexicals.len());
        for (name, b) in &lexicals {
            out.text(name);
            ctx.value(&mut out, b.value);
            out.u8(binding_code(b.kind));
        }
        templates.sort_by_key(|(c, i, _)| (cref(c), *i));
        out.len(templates.len());
        for (c, i, o) in &templates {
            out.u32(cref(c));
            out.u32(*i);
            out.u32(ctx.objects.get(o.0));
        }
        out.len(script_names.len());
        for n in &script_names {
            out.text(n);
        }
        out.len(var_names.len());
        for n in &var_names {
            out.text(n);
        }
        out.len(scripts.len());
        for (name, c, kind) in &scripts {
            out.text(name);
            out.u32(cref(c));
            out.u8(script_code(*kind));
        }

        let mut payload = Vec::with_capacity(out.b.len() + 16);
        payload.extend_from_slice(&registry().fingerprint.to_le_bytes());
        payload.extend_from_slice(&checksum(&out.b).to_le_bytes());
        payload.extend_from_slice(&out.b);
        Ok(api::bytecode::wrap(&format_tag(), &payload))
    }
}

fn preorder(root: &Rc<CodeBlock>) -> Vec<Rc<CodeBlock>> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(b) = stack.pop() {
        stack.extend(b.functions.iter().rev().cloned());
        out.push(b);
    }
    out
}

/// The numberings an object body refers through.
struct Ctx<'t> {
    objects: &'t Table,
    envs: &'t Table,
    strings: &'t Table,
    symbols: &'t Table,
    bigints: &'t Table,
}

impl Ctx<'_> {
    fn value(&self, out: &mut Out, v: Value) {
        match v {
            Value::Undefined => out.u8(V_UNDEFINED),
            Value::Null => out.u8(V_NULL),
            Value::Bool(false) => out.u8(V_FALSE),
            Value::Bool(true) => out.u8(V_TRUE),
            Value::Number(n) => {
                out.u8(V_NUMBER);
                out.f64(n);
            }
            Value::String(s) => {
                out.u8(V_STRING);
                out.u32(self.strings.get(s.0));
            }
            Value::Symbol(s) => {
                out.u8(V_SYMBOL);
                out.u32(self.symbols.get(s.0));
            }
            Value::BigInt(b) => {
                out.u8(V_BIGINT);
                out.u32(self.bigints.get(b.0));
            }
            Value::Object(o) => {
                out.u8(V_OBJECT);
                out.u32(self.objects.get(o.0));
            }
            Value::Empty => out.u8(V_EMPTY),
        }
    }

    fn key(&self, out: &mut Out, k: PropKey) {
        match k {
            PropKey::Index(i) => {
                out.u8(K_INDEX);
                out.u32(i);
            }
            PropKey::Str(s) => {
                out.u8(K_STR);
                out.u32(self.strings.get(s.0));
            }
            PropKey::Sym(s) => {
                out.u8(K_SYM);
                out.u32(self.symbols.get(s.0));
            }
        }
    }

    fn slot(&self, out: &mut Out, s: PropSlot) {
        match s {
            PropSlot::Data(v) => {
                out.u8(0);
                self.value(out, v);
            }
            PropSlot::Accessor { get, set } => {
                out.u8(1);
                self.value(out, get.value());
                self.value(out, set.value());
            }
        }
    }

    fn opt_obj(&self, out: &mut Out, o: Option<ObjId>) {
        out.u32(o.map(|o| self.objects.get(o.0)).unwrap_or(NONE));
    }

    fn opt_usize(&self, out: &mut Out, v: Option<usize>) {
        match v {
            Some(v) => {
                out.u8(1);
                out.u64(v as u64);
            }
            None => out.u8(0),
        }
    }
}

fn write_object(vm: &Vm, ctx: &Ctx, cref: &dyn Fn(&Rc<CodeBlock>) -> u32, out: &mut Out, id: ObjId, native: u32) {
    let o = vm.heap.obj(id);
    out.u8(class_code(&o.class));
    ctx.opt_obj(out, o.proto());
    out.u8(o.extensible as u8);
    match &o.dict {
        Some(d) => {
            out.len(d.len());
            for (k, p) in d.iter() {
                ctx.key(out, *k);
                ctx.slot(out, p.slot);
                out.u8(p.attrs.0);
            }
        }
        None => {
            let keys = vm.heap.shape_keys(o.shape);
            out.len(keys.len());
            for ((k, a), s) in keys.iter().zip(o.slots()) {
                ctx.key(out, *k);
                ctx.slot(out, *s);
                out.u8(a.0);
            }
        }
    }
    match &o.elements {
        Elements::Dense(v) => {
            out.u8(0);
            out.len(v.len());
            for &x in v {
                ctx.value(out, x);
            }
        }
        Elements::Sparse(m) => {
            out.u8(1);
            out.len(m.len());
            for (i, p) in m.iter() {
                out.u32(*i);
                ctx.slot(out, p.slot);
                out.u8(p.attrs.0);
            }
        }
    }
    match &o.class {
        Class::Array(a) => {
            out.u32(a.length);
            out.u8(a.length_writable as u8);
        }
        Class::Function(f) => {
            out.u32(cref(&f.code));
            out.u32(f.env.map(|e| ctx.envs.get(e.0)).unwrap_or(NONE));
            ctx.opt_obj(out, f.home_object);
            match &f.class_data {
                Some(cd) => {
                    out.u8(1);
                    out.len(cd.fields.len());
                    for field in &cd.fields {
                        match field.key {
                            FieldKey::Prop(k) => {
                                out.u8(0);
                                ctx.key(out, k);
                            }
                            FieldKey::Private(s) => {
                                out.u8(1);
                                out.u32(ctx.symbols.get(s.0));
                            }
                        }
                        ctx.opt_obj(out, field.init);
                        out.u8(field.anon as u8);
                    }
                    out.len(cd.private_methods.len());
                    for s in &cd.private_methods {
                        out.u32(ctx.symbols.get(s.0));
                    }
                }
                None => out.u8(0),
            }
        }
        Class::Native(_) => out.u32(native),
        Class::Bound(b) => {
            out.u32(ctx.objects.get(b.target.0));
            ctx.value(out, b.this);
            out.len(b.args.len());
            for &a in &b.args {
                ctx.value(out, a);
            }
        }
        Class::Boolean(b) => out.u8(*b as u8),
        Class::Number(n) | Class::Date(n) => out.f64(*n),
        Class::String(s) => out.u32(ctx.strings.get(s.0)),
        Class::Symbol(s) => out.u32(ctx.symbols.get(s.0)),
        Class::BigInt(b) => out.u32(ctx.bigints.get(b.0)),
        Class::RegExp(r) => {
            out.u32(ctx.strings.get(r.source.0));
            out.u32(ctx.strings.get(r.flags.0));
        }
        Class::Map(m) | Class::Set(m) | Class::WeakMap(m) | Class::WeakSet(m) => {
            out.len(m.live);
            for (k, v) in m.entries.iter().flatten() {
                ctx.value(out, *k);
                ctx.value(out, *v);
            }
        }
        Class::Promise(p) => {
            out.u8(match p.status {
                PromiseStatus::Pending => 0,
                PromiseStatus::Fulfilled => 1,
                PromiseStatus::Rejected => 2,
            });
            out.u8(p.is_handled as u8);
            ctx.value(out, p.result);
        }
        Class::ArrayBuffer(b) => {
            out.u8(b.detached as u8 | (b.shared as u8) << 1);
            ctx.opt_usize(out, b.max_byte_length);
            out.u64(b.bytes.len() as u64);
            out.b.extend_from_slice(&b.bytes);
        }
        Class::TypedArray(t) => {
            out.u32(ctx.objects.get(t.buffer.0));
            out.u8(element_kind_code(t.kind));
            out.u64(t.byte_offset as u64);
            ctx.opt_usize(out, t.length);
        }
        Class::DataView(d) => {
            out.u32(ctx.objects.get(d.buffer.0));
            out.u64(d.byte_offset as u64);
            ctx.opt_usize(out, d.byte_length);
        }
        Class::Proxy(p) => {
            ctx.opt_obj(out, p.target);
            ctx.opt_obj(out, p.handler);
            out.u8(p.callable as u8 | (p.constructor as u8) << 1);
        }
        Class::WeakRef(v) => ctx.value(out, *v),
        _ => {}
    }
}

// ---- reading ------------------------------------------------------------------------------------

fn corrupt<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::BytecodeCorrupt(format!("snapshot: {}", m.into())))
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        match self.pos.checked_add(n) {
            Some(end) if end <= self.b.len() => {
                let s = &self.b[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            _ => corrupt("truncated"),
        }
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
    fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => corrupt("bad flag"),
        }
    }
    /// A count of items at least `min` bytes each, bounded by what is left.
    fn count(&mut self, min: usize) -> Result<usize, Error> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min.max(1)) > self.b.len() - self.pos {
            return corrupt("count beyond the input");
        }
        Ok(n)
    }
    fn usize(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u64()?).or_else(|_| corrupt("size out of range"))
    }
    fn opt_usize(&mut self) -> Result<Option<usize>, Error> {
        Ok(if self.bool()? { Some(self.usize()?) } else { None })
    }
    fn text(&mut self) -> Result<Rc<str>, Error> {
        let n = self.count(1)?;
        match std::str::from_utf8(self.take(n)?) {
            Ok(s) => Ok(Rc::from(s)),
            Err(_) => corrupt("a name is not UTF-8"),
        }
    }
}

/// The tables a body refers into while it is read.
struct Tables {
    objects: Vec<ObjId>,
    envs: Vec<EnvId>,
    strings: Vec<StrId>,
    symbols: Vec<SymId>,
    bigints: Vec<BigId>,
    blocks: Vec<Rc<CodeBlock>>,
}

impl Tables {
    fn obj(&self, i: u32) -> Result<ObjId, Error> {
        self.objects.get(i as usize).copied().map_or_else(|| corrupt("object index"), Ok)
    }
    fn opt_obj(&self, i: u32) -> Result<Option<ObjId>, Error> {
        if i == NONE {
            Ok(None)
        } else {
            self.obj(i).map(Some)
        }
    }
    fn env(&self, i: u32) -> Result<EnvId, Error> {
        self.envs.get(i as usize).copied().map_or_else(|| corrupt("environment index"), Ok)
    }
    fn opt_env(&self, i: u32) -> Result<Option<EnvId>, Error> {
        if i == NONE {
            Ok(None)
        } else {
            self.env(i).map(Some)
        }
    }
    fn str(&self, i: u32) -> Result<StrId, Error> {
        self.strings.get(i as usize).copied().map_or_else(|| corrupt("string index"), Ok)
    }
    fn opt_str(&self, i: u32) -> Result<Option<StrId>, Error> {
        if i == NONE {
            Ok(None)
        } else {
            self.str(i).map(Some)
        }
    }
    fn sym(&self, i: u32) -> Result<SymId, Error> {
        self.symbols.get(i as usize).copied().map_or_else(|| corrupt("symbol index"), Ok)
    }
    fn block(&self, i: u32) -> Result<Rc<CodeBlock>, Error> {
        self.blocks.get(i as usize).cloned().map_or_else(|| corrupt("code index"), Ok)
    }

    /// A value; `Empty` only where `empty_ok`.
    fn value(&self, r: &mut Reader, empty_ok: bool) -> Result<Value, Error> {
        Ok(match r.u8()? {
            V_UNDEFINED => Value::Undefined,
            V_NULL => Value::Null,
            V_FALSE => Value::Bool(false),
            V_TRUE => Value::Bool(true),
            V_NUMBER => Value::Number(r.f64()?),
            V_STRING => Value::String(self.str(r.u32()?)?),
            V_SYMBOL => Value::Symbol(self.sym(r.u32()?)?),
            V_BIGINT => Value::BigInt(
                self.bigints
                    .get(r.u32()? as usize)
                    .copied()
                    .map_or_else(|| corrupt("BigInt index"), Ok)?,
            ),
            V_OBJECT => Value::Object(self.obj(r.u32()?)?),
            V_EMPTY if empty_ok => Value::Empty,
            _ => return corrupt("bad value tag"),
        })
    }

    fn key(&self, vm: &Vm, r: &mut Reader) -> Result<PropKey, Error> {
        Ok(match r.u8()? {
            K_INDEX => {
                let i = r.u32()?;
                if i > PropKey::MAX_INDEX {
                    return corrupt("index key out of range");
                }
                PropKey::Index(i)
            }
            K_STR => {
                let s = self.str(r.u32()?)?;
                let d = vm.heap.string(s);
                let idx = match d.units() {
                    Units::One(b) => PropKey::index_from_bytes(b),
                    Units::Two(u) => PropKey::index_from_units(u),
                };
                if !d.atom || idx.is_some() {
                    return corrupt("a string key that is not a property-name atom");
                }
                PropKey::Str(s)
            }
            K_SYM => PropKey::Sym(self.sym(r.u32()?)?),
            _ => return corrupt("bad key tag"),
        })
    }

    fn slot(&self, r: &mut Reader) -> Result<PropSlot, Error> {
        match r.u8()? {
            0 => Ok(PropSlot::Data(self.value(r, false)?)),
            1 => {
                let get = self.value(r, false)?;
                let set = self.value(r, false)?;
                if !matches!(get, Value::Undefined | Value::Object(_)) || !matches!(set, Value::Undefined | Value::Object(_)) {
                    return corrupt("an accessor half that is not a function");
                }
                Ok(PropSlot::accessor(get, set))
            }
            _ => corrupt("bad slot tag"),
        }
    }
}

fn attrs(r: &mut Reader, slot: &PropSlot) -> Result<Attrs, Error> {
    let a = r.u8()?;
    if a > Attrs::ALL.0 || (matches!(slot, PropSlot::Accessor { .. }) && a & Attrs::WRITABLE != 0) {
        return corrupt("bad attributes");
    }
    Ok(Attrs(a))
}

/// Restore a snapshot into a new VM with one realm, `RealmId(0)`.
pub fn restore(host: Rc<dyn api::Host>, bytes: &[u8]) -> Result<Vm, Error> {
    let payload = api::bytecode::unwrap(&format_tag(), bytes)?;
    if payload.len() < 16 {
        return corrupt("truncated header");
    }
    let mut r = Reader { b: payload, pos: 0 };
    let fingerprint = r.u64()?;
    let expected = registry().fingerprint;
    if fingerprint != expected {
        return Err(Error::BytecodeMismatch {
            expected: format!("{} natives {expected:016x}", format_tag()),
            found: format!("natives {fingerprint:016x}"),
        });
    }
    let sum = r.u64()?;
    let body = &payload[16..];
    if checksum(body) != sum {
        return corrupt("checksum mismatch");
    }
    let mut vm = Vm::bare(host);
    load(&mut vm, body)?;
    Ok(vm)
}

/// Objects whose payload needs the realm (RegExp compilation) or other
/// objects' payloads (validation), finished after every body is read.
struct Deferred {
    regexps: Vec<(ObjId, StrId, StrId)>,
}

fn load(vm: &mut Vm, body: &[u8]) -> Result<(), Error> {
    let reg = registry();
    let mut r = Reader { b: body, pos: 0 };
    let n_strings = r.count(5)?;
    let n_bigints = r.count(4)?;
    let n_roots = r.count(4)?;
    let n_symbols = r.count(2)?;
    let n_objects = r.count(11)?;
    let n_envs = r.count(1)?;
    let mut t = Tables {
        objects: Vec::with_capacity(n_objects),
        envs: Vec::with_capacity(n_envs),
        strings: Vec::with_capacity(n_strings),
        symbols: Vec::with_capacity(n_symbols),
        bigints: Vec::with_capacity(n_bigints),
        blocks: Vec::new(),
    };
    for _ in 0..n_strings {
        let flags = r.u8()?;
        if flags > 3 {
            return corrupt("bad string flags");
        }
        let atom = flags & 1 != 0;
        let n = r.count(if flags & 2 != 0 { 2 } else { 1 })?;
        let id = if flags & 2 != 0 {
            let raw = r.take(n * 2)?;
            let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            if atom {
                vm.heap.atom_utf16(&units)
            } else {
                vm.heap.alloc_str_utf16(units)
            }
        } else {
            let b = r.take(n)?;
            if atom {
                vm.heap.atom_units(Units::One(b))
            } else {
                vm.heap.alloc_str_latin1(b.to_vec())
            }
        };
        t.strings.push(id);
    }
    for _ in 0..n_bigints {
        let n = r.count(1)?;
        let b = r.take(n)?;
        let v = num_bigint::BigInt::from_signed_bytes_le(b);
        t.bigints.push(vm.heap.alloc_bigint(BigIntData(v)));
    }
    for _ in 0..n_roots {
        let n = r.count(1)?;
        let bytes = r.take(n)?;
        let root = match agnijs_js_compiler::serialize::decode(bytes) {
            Ok(c) => c,
            Err(e) => return corrupt(format!("code: {e}")),
        };
        if let Err(e) = vm.verify_debug_sections(&root) {
            return Err(e);
        }
        t.blocks.extend(preorder(&root));
    }
    // Placeholders, so bodies can refer forward.
    for _ in 0..n_objects {
        let id = vm.heap.alloc_object(ObjectData::new(None, Class::Ordinary));
        t.objects.push(id);
    }
    for _ in 0..n_envs {
        let id = vm.heap.alloc_env(Environment::synthetic(0, None, false));
        t.envs.push(id);
    }
    let well_known = vm.well_known.all();
    for _ in 0..n_symbols {
        let id = match r.u8()? {
            0 => {
                let i = r.u8()? as usize;
                *well_known.get(i).map_or_else(|| corrupt("well-known symbol index"), Ok)?
            }
            1 => {
                let key = t.str(r.u32()?)?;
                if !vm.heap.string(key).atom {
                    return corrupt("a registry key that is not an atom");
                }
                let s = vm.heap.alloc_symbol(SymbolData::new(Some(key), SymbolKind::Registered));
                if vm.symbol_registry.insert(key, s).is_some() {
                    return corrupt("a registry key twice");
                }
                s
            }
            k @ (2 | 3) => {
                let desc = t.opt_str(r.u32()?)?;
                let kind = if k == 2 { SymbolKind::Normal } else { SymbolKind::Private };
                let mut d = SymbolData::new(desc, kind);
                d.private = match r.u8()? {
                    0 => PrivateElement::Field,
                    1 => PrivateElement::Method(t.value(&mut r, false)?),
                    2 => PrivateElement::Accessor {
                        get: t.value(&mut r, false)?,
                        set: t.value(&mut r, false)?,
                    },
                    _ => return corrupt("bad private element"),
                };
                if kind == SymbolKind::Normal && !matches!(d.private, PrivateElement::Field) {
                    return corrupt("a private element on an ordinary symbol");
                }
                vm.heap.alloc_symbol(d)
            }
            _ => return corrupt("bad symbol kind"),
        };
        t.symbols.push(id);
    }
    let mut deferred = Deferred { regexps: Vec::new() };
    for i in 0..n_objects {
        read_object(vm, &t, &mut r, t.objects[i], reg, &mut deferred)?;
    }
    let mut global_envs = 0usize;
    for i in 0..n_envs {
        let env = match r.u8()? {
            0 => {
                let c = r.u32()?;
                let (scope, is_var_scope) = if c == NONE {
                    (None, r.bool()?)
                } else {
                    let code = t.block(c)?;
                    let idx = r.u32()?;
                    let Some(info) = code.scopes.get(idx as usize) else {
                        return corrupt("scope index");
                    };
                    let v = info.is_var_scope;
                    (Some((code, idx)), v)
                };
                let n = r.count(1)?;
                let mut slots = Vec::with_capacity(n);
                for _ in 0..n {
                    slots.push(t.value(&mut r, true)?);
                }
                if let Some((code, idx)) = &scope {
                    if code.scopes[*idx as usize].bindings.len() != n {
                        return corrupt("an environment that does not match its scope");
                    }
                }
                let parent = t.opt_env(r.u32()?)?;
                let n = r.count(4)?;
                let extra = if n == 0 {
                    None
                } else {
                    let mut m = indexmap::IndexMap::with_capacity(n);
                    for _ in 0..n {
                        let name = r.text()?;
                        let value = t.value(&mut r, true)?;
                        let kind = binding_kind(r.u8()?)?;
                        let deletable = r.bool()?;
                        if m.insert(name, ExtraBinding { value, kind, deletable }).is_some() {
                            return corrupt("an eval binding twice");
                        }
                    }
                    Some(Box::new(m))
                };
                Environment::Declarative(DeclEnv {
                    slots,
                    scope,
                    extra,
                    parent,
                    is_var_scope,
                })
            }
            1 => {
                let object = t.obj(r.u32()?)?;
                let with = r.bool()?;
                let parent = t.opt_env(r.u32()?)?;
                Environment::Object(ObjectEnv { object, with, parent })
            }
            2 => {
                global_envs += 1;
                Environment::Global(RealmId(0))
            }
            _ => return corrupt("bad environment kind"),
        };
        let grown = env.slot_count() * 16;
        *vm.heap.env_mut(t.envs[i]) = env;
        vm.heap.note_growth(grown);
    }
    // The realm.
    let global = t.obj(r.u32()?)?;
    let global_env = t.env(r.u32()?)?;
    let intl_fallback = t.sym(r.u32()?)?;
    if global_envs != 1 || !matches!(vm.heap.env(global_env), Environment::Global(_)) {
        return corrupt("the realm needs exactly one global environment");
    }
    if vm.heap.sym(intl_fallback).kind != SymbolKind::Normal {
        return corrupt("the realm's Intl fallback symbol");
    }
    let n = r.count(4)?;
    if n != Intrinsic::COUNT {
        return corrupt("intrinsic count");
    }
    let mut intrinsics = Vec::with_capacity(n);
    for _ in 0..n {
        intrinsics.push(t.obj(r.u32()?)?);
    }
    let mut realm = Realm::restored(RealmId(0), global, global_env, intrinsics, intl_fallback);
    let n = r.count(6)?;
    for _ in 0..n {
        let name = r.text()?;
        let value = t.value(&mut r, true)?;
        let kind = binding_kind(r.u8()?)?;
        if realm.global_lexicals.insert(name, LexBinding { value, kind }).is_some() {
            return corrupt("a global lexical twice");
        }
    }
    let n = r.count(12)?;
    for _ in 0..n {
        let code = t.block(r.u32()?)?;
        let c = r.u32()?;
        let o = t.obj(r.u32()?)?;
        if !matches!(code.constants.get(c as usize), Some(Constant::Template(_))) {
            return corrupt("a template object for a non-template constant");
        }
        if !matches!(vm.heap.obj(o).class, Class::Array(_)) {
            return corrupt("a template object that is not an array");
        }
        let key = (Rc::as_ptr(&code) as usize, c);
        if realm.template_cache.insert(key, (code, o)).is_some() {
            return corrupt("a template object twice");
        }
    }
    let n = r.count(4)?;
    for _ in 0..n {
        realm.script_names.push(r.text()?.to_string());
    }
    let n = r.count(4)?;
    for _ in 0..n {
        realm.var_names.insert(r.text()?);
    }
    let n = r.count(9)?;
    for _ in 0..n {
        let name = r.text()?.to_string();
        let code = t.block(r.u32()?)?;
        let kind = match SCRIPT_KINDS.get(r.u8()? as usize) {
            Some(ScriptKind::Module) | None => return corrupt("bad script kind"),
            Some(k) => *k,
        };
        vm.scripts.insert(
            name,
            crate::debug::ScriptEntry {
                code,
                kind,
                realm: RealmId(0),
            },
        );
    }
    if r.pos != body.len() {
        return corrupt("trailing bytes");
    }
    vm.realms.push(realm);
    vm.current_realm = RealmId(0);
    validate(vm, &t, reg)?;
    for (o, source, flags) in deferred.regexps {
        let (matcher, parsed) = match crate::builtins::regexp::compile(vm, source, flags) {
            Ok(x) => x,
            Err(_) => return corrupt("a RegExp that does not compile"),
        };
        vm.heap.obj_mut(o).class = Class::RegExp(Box::new(crate::builtins::regexp::RegExpData {
            source,
            flags,
            matcher,
            parsed,
        }));
    }
    // A loaded heap is the live set: pace the next collection from it.
    vm.heap.live_bytes = vm.heap.approx_live_bytes();
    vm.heap.allocated_since_gc = 0;
    vm.heap.rearm_threshold();
    Ok(())
}

fn binding_kind(c: u8) -> Result<BindingKind, Error> {
    BINDING_KINDS.get(c as usize).copied().map_or_else(|| corrupt("bad binding kind"), Ok)
}

fn read_object(
    vm: &mut Vm,
    t: &Tables,
    r: &mut Reader,
    id: ObjId,
    reg: &Registry,
    deferred: &mut Deferred,
) -> Result<(), Error> {
    let class = r.u8()?;
    let proto = t.opt_obj(r.u32()?)?;
    let extensible = r.bool()?;
    vm.heap.obj_mut(id).set_proto_raw(proto);
    let n = r.count(3)?;
    for _ in 0..n {
        let key = t.key(vm, r)?;
        if matches!(key, PropKey::Index(_)) {
            return corrupt("an index key among named properties");
        }
        let slot = t.slot(r)?;
        let a = attrs(r, &slot)?;
        if vm.heap.has_own_property(id, key) {
            return corrupt("a property twice");
        }
        vm.heap.add_own_property(id, key, Property { slot, attrs: a });
    }
    let elements = match r.u8()? {
        0 => {
            let n = r.count(1)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(t.value(r, true)?);
            }
            if v.len() > PropKey::MAX_INDEX as usize {
                return corrupt("too many elements");
            }
            Elements::Dense(v)
        }
        1 => {
            let n = r.count(6)?;
            let mut m = BTreeMap::new();
            let mut last: Option<u32> = None;
            for _ in 0..n {
                let i = r.u32()?;
                if i > PropKey::MAX_INDEX || last.is_some_and(|l| i <= l) {
                    return corrupt("sparse indices out of order");
                }
                last = Some(i);
                let slot = t.slot(r)?;
                let a = attrs(r, &slot)?;
                m.insert(i, Property { slot, attrs: a });
            }
            Elements::Sparse(Box::new(m))
        }
        _ => return corrupt("bad elements tag"),
    };
    let extent = match &elements {
        Elements::Dense(v) => v.len() as u64,
        Elements::Sparse(m) => m.keys().next_back().map_or(0, |&k| k as u64 + 1),
    };
    let grown = match &elements {
        Elements::Dense(v) => v.len() * crate::heap::ELEMENT_BYTES,
        Elements::Sparse(m) => m.len() * crate::heap::DICT_ENTRY_BYTES,
    };
    vm.heap.obj_mut(id).elements = elements;
    vm.heap.note_growth(grown);
    let payload = match class {
        code::ORDINARY => Class::Ordinary,
        code::ARRAY => {
            let length = r.u32()?;
            let length_writable = r.bool()?;
            if (length as u64) < extent {
                return corrupt("an array shorter than its elements");
            }
            Class::Array(ArrayData {
                length,
                length_writable,
            })
        }
        code::FUNCTION => {
            let code = t.block(r.u32()?)?;
            if !matches!(
                code.kind,
                CodeKind::Function | CodeKind::Arrow | CodeKind::Method | CodeKind::ClassConstructor | CodeKind::ClassInitializer
            ) {
                return corrupt("a function over non-function code");
            }
            let env = t.opt_env(r.u32()?)?;
            let home_object = t.opt_obj(r.u32()?)?;
            let class_data = if r.bool()? {
                let n = r.count(6)?;
                let mut fields = Vec::with_capacity(n);
                for _ in 0..n {
                    let key = match r.u8()? {
                        0 => FieldKey::Prop(t.key(vm, r)?),
                        1 => FieldKey::Private(t.sym(r.u32()?)?),
                        _ => return corrupt("bad field key"),
                    };
                    let init = t.opt_obj(r.u32()?)?;
                    let anon = r.bool()?;
                    fields.push(FieldDef { key, init, anon });
                }
                let n = r.count(4)?;
                let mut private_methods = Vec::with_capacity(n);
                for _ in 0..n {
                    private_methods.push(t.sym(r.u32()?)?);
                }
                Some(Box::new(ClassData {
                    fields,
                    private_methods,
                }))
            } else {
                None
            };
            Class::Function(Box::new(function_data(code, env, home_object, class_data)))
        }
        code::NATIVE => {
            let i = r.u32()?;
            let Some(e) = reg.entries.get(i as usize) else {
                return corrupt("native index");
            };
            Class::Native(Box::new(NativeData {
                func: NativeKind::Fn(e.f),
                tag: e.tag,
                realm: RealmId(0),
                constructor: e.constructor,
                slots: e.slots.clone(),
                flag: None,
                counter: None,
            }))
        }
        code::BOUND => {
            let target = t.obj(r.u32()?)?;
            let this = t.value(r, false)?;
            let n = r.count(1)?;
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(t.value(r, false)?);
            }
            Class::Bound(Box::new(BoundData { target, this, args }))
        }
        code::ERROR => Class::Error,
        code::BOOLEAN => Class::Boolean(r.bool()?),
        code::NUMBER => Class::Number(r.f64()?),
        code::STRING => Class::String(t.str(r.u32()?)?),
        code::SYMBOL => Class::Symbol(t.sym(r.u32()?)?),
        code::BIGINT => Class::BigInt(
            t.bigints
                .get(r.u32()? as usize)
                .copied()
                .map_or_else(|| corrupt("BigInt index"), Ok)?,
        ),
        code::ARGUMENTS => Class::Arguments(None),
        code::DATE => Class::Date(r.f64()?),
        code::REGEXP => {
            let source = t.str(r.u32()?)?;
            let flags = t.str(r.u32()?)?;
            deferred.regexps.push((id, source, flags));
            Class::Ordinary
        }
        code::MAP | code::SET | code::WEAK_MAP | code::WEAK_SET => {
            let weak = class == code::WEAK_MAP || class == code::WEAK_SET;
            let n = r.count(2)?;
            let mut m = MapData::new(weak);
            for _ in 0..n {
                let k = t.value(r, false)?;
                let v = t.value(r, false)?;
                if weak && !can_be_held_weakly(vm, k) {
                    return corrupt("a weak collection key that cannot be held weakly");
                }
                if matches!(k, Value::Number(n) if n == 0.0 && n.is_sign_negative()) {
                    return corrupt("a -0 collection key");
                }
                let key = crate::builtins::collections::map_key(vm, k);
                if !m.set(key, k, v) {
                    return corrupt("a collection key twice");
                }
            }
            let m = Box::new(m);
            match class {
                code::MAP => Class::Map(m),
                code::SET => Class::Set(m),
                code::WEAK_MAP => Class::WeakMap(m),
                _ => Class::WeakSet(m),
            }
        }
        code::PROMISE => {
            let status = match r.u8()? {
                0 => PromiseStatus::Pending,
                1 => PromiseStatus::Fulfilled,
                2 => PromiseStatus::Rejected,
                _ => return corrupt("bad promise status"),
            };
            let is_handled = r.bool()?;
            let result = t.value(r, false)?;
            Class::Promise(Box::new(PromiseData {
                status,
                is_handled,
                result,
                reactions: Reactions::None,
            }))
        }
        code::ARRAY_BUFFER => {
            let flags = r.u8()?;
            if flags > 3 {
                return corrupt("bad buffer flags");
            }
            let max_byte_length = r.opt_usize()?;
            let n = r.usize()?;
            let bytes = r.take(n)?.to_vec();
            let detached = flags & 1 != 0;
            if (detached && !bytes.is_empty()) || max_byte_length.is_some_and(|m| bytes.len() > m) {
                return corrupt("a buffer beyond its bounds");
            }
            if max_byte_length.is_some_and(|m| m as f64 > crate::builtins::arraybuffer::MAX_BUFFER_BYTES) {
                return corrupt("a buffer maximum beyond the engine's limit");
            }
            vm.heap.note_growth(bytes.len());
            Class::ArrayBuffer(Box::new(BufferData {
                bytes,
                detached,
                shared: flags & 2 != 0,
                max_byte_length,
            }))
        }
        code::TYPED_ARRAY => {
            let buffer = t.obj(r.u32()?)?;
            let Some(kind) = element_kind(r.u8()?) else {
                return corrupt("bad element kind");
            };
            let byte_offset = r.usize()?;
            let length = r.opt_usize()?;
            Class::TypedArray(Box::new(TypedArrayData {
                buffer,
                kind,
                byte_offset,
                length,
            }))
        }
        code::DATA_VIEW => {
            let buffer = t.obj(r.u32()?)?;
            let byte_offset = r.usize()?;
            let byte_length = r.opt_usize()?;
            Class::DataView(Box::new(DataViewData {
                buffer,
                byte_offset,
                byte_length,
            }))
        }
        code::PROXY => {
            let target = t.opt_obj(r.u32()?)?;
            let handler = t.opt_obj(r.u32()?)?;
            let flags = r.u8()?;
            if flags > 3 || target.is_some() != handler.is_some() || (flags & 2 != 0 && flags & 1 == 0) {
                return corrupt("bad proxy");
            }
            Class::Proxy(Box::new(ProxyData {
                target,
                handler,
                callable: flags & 1 != 0,
                constructor: flags & 2 != 0,
            }))
        }
        code::WEAK_REF => {
            let v = t.value(r, false)?;
            if !can_be_held_weakly(vm, v) {
                return corrupt("a WeakRef target that cannot be held weakly");
            }
            Class::WeakRef(v)
        }
        _ => return corrupt("bad class"),
    };
    let o = vm.heap.obj_mut(id);
    o.class = payload;
    o.extensible = extensible;
    Ok(())
}

fn can_be_held_weakly(vm: &Vm, v: Value) -> bool {
    match v {
        Value::Object(_) => true,
        Value::Symbol(s) => vm.heap.sym(s).kind != SymbolKind::Registered,
        _ => false,
    }
}

/// A function object's data as `Vm::create_function` derives it from its
/// code.
fn function_data(
    code: Rc<CodeBlock>,
    env: Option<EnvId>,
    home_object: Option<ObjId>,
    class_data: Option<Box<ClassData>>,
) -> FunctionData {
    use crate::object::{ConstructorKind, ThisMode};
    use agnijs_js_parser::ast::FunctionKind;
    let is_ctor = code.function_kind == FunctionKind::Normal
        && matches!(code.kind, CodeKind::Function | CodeKind::ClassConstructor);
    let constructor_kind = if !is_ctor {
        ConstructorKind::None
    } else if code.is_derived_constructor {
        ConstructorKind::Derived
    } else {
        ConstructorKind::Base
    };
    let this_mode = if code.kind == CodeKind::Arrow {
        ThisMode::Lexical
    } else if code.strict {
        ThisMode::Strict
    } else {
        ThisMode::Global
    };
    FunctionData {
        is_class_constructor: code.kind == CodeKind::ClassConstructor,
        code,
        env,
        realm: RealmId(0),
        home_object,
        constructor_kind,
        this_mode,
        class_data,
        module: None,
    }
}

/// Cross-object invariants, checked once every body is in place.
fn validate(vm: &Vm, t: &Tables, reg: &Registry) -> Result<(), Error> {
    let callable = |v: Value| match v {
        Value::Object(o) => vm.heap.obj(o).class.is_callable(),
        Value::Undefined => true,
        _ => false,
    };
    let n = t.objects.len();
    // Chains through prototypes, bound targets and proxy targets end.
    for &start in &t.objects {
        let mut steps = 0usize;
        let mut at = Some(start);
        while let Some(o) = at {
            steps += 1;
            if steps > n + 1 {
                return corrupt("a prototype chain with a cycle");
            }
            at = vm.heap.obj(o).proto();
        }
        let mut steps = 0usize;
        let mut at = Some(start);
        while let Some(o) = at {
            steps += 1;
            if steps > n + 1 {
                return corrupt("a bound-function or proxy chain with a cycle");
            }
            at = match &vm.heap.obj(o).class {
                Class::Bound(b) => Some(b.target),
                Class::Proxy(p) => p.target,
                _ => None,
            };
        }
    }
    for &e in &t.envs {
        let mut steps = 0usize;
        let mut at = Some(e);
        while let Some(x) = at {
            steps += 1;
            if steps > t.envs.len() + 1 {
                return corrupt("an environment chain with a cycle");
            }
            at = vm.heap.env(x).parent();
        }
    }
    for &id in &t.objects {
        let o = vm.heap.obj(id);
        let mut accessors_ok = true;
        let mut check = |s: &PropSlot| {
            if let PropSlot::Accessor { get, set } = s {
                accessors_ok &= callable(get.value()) && callable(set.value());
            }
        };
        match &o.dict {
            Some(d) => d.values().for_each(|p| check(&p.slot)),
            None => o.slots().for_each(&mut check),
        }
        if let Elements::Sparse(m) = &o.elements {
            m.values().for_each(|p| check(&p.slot));
        }
        if !accessors_ok {
            return corrupt("an accessor half that is not callable");
        }
        match &o.class {
            Class::Function(f) => {
                if let Some(cd) = &f.class_data {
                    for field in &cd.fields {
                        if let FieldKey::Private(s) = field.key {
                            if vm.heap.sym(s).kind != SymbolKind::Private {
                                return corrupt("a private field keyed by an ordinary symbol");
                            }
                        }
                        if let Some(init) = field.init {
                            if !matches!(vm.heap.obj(init).class, Class::Function(_)) {
                                return corrupt("a field initialiser that is not a function");
                            }
                        }
                    }
                    if cd.private_methods.iter().any(|s| vm.heap.sym(*s).kind != SymbolKind::Private) {
                        return corrupt("a private method keyed by an ordinary symbol");
                    }
                }
            }
            Class::Bound(b) => {
                if !vm.heap.obj(b.target).class.is_callable() {
                    return corrupt("a bound function over a non-callable target");
                }
            }
            Class::Proxy(p) => {
                if let Some(target) = p.target {
                    let c = vm.heap.obj(target).class.is_callable();
                    if c != p.callable || (p.constructor && !vm.is_constructor(Value::Object(target))) {
                        return corrupt("a proxy that does not match its target");
                    }
                }
            }
            Class::TypedArray(ta) => {
                let Class::ArrayBuffer(b) = &vm.heap.obj(ta.buffer).class else {
                    return corrupt("a typed array over a non-buffer");
                };
                let size = ta.kind.size();
                if ta.byte_offset % size != 0 {
                    return corrupt("a misaligned typed array");
                }
                if b.max_byte_length.is_none() && !b.detached {
                    let Some(len) = ta.length else {
                        return corrupt("a length-tracking view over a fixed buffer");
                    };
                    let end = len.checked_mul(size).and_then(|x| x.checked_add(ta.byte_offset));
                    if end.is_none_or(|e| e > b.bytes.len()) {
                        return corrupt("a typed array beyond its buffer");
                    }
                }
            }
            Class::DataView(dv) => {
                let Class::ArrayBuffer(b) = &vm.heap.obj(dv.buffer).class else {
                    return corrupt("a DataView over a non-buffer");
                };
                if b.max_byte_length.is_none() && !b.detached {
                    let Some(len) = dv.byte_length else {
                        return corrupt("a length-tracking view over a fixed buffer");
                    };
                    if len.checked_add(dv.byte_offset).is_none_or(|e| e > b.bytes.len()) {
                        return corrupt("a DataView beyond its buffer");
                    }
                }
            }
            _ => {}
        }
    }
    for &s in &t.symbols {
        match vm.heap.sym(s).private {
            PrivateElement::Field => {}
            PrivateElement::Method(v) => {
                if !matches!(v, Value::Object(_)) || !callable(v) {
                    return corrupt("a private method that is not a function");
                }
            }
            PrivateElement::Accessor { get, set } => {
                if !callable(get) || !callable(set) {
                    return corrupt("a private accessor that is not a function");
                }
            }
        }
    }
    let realm = &vm.realms[0];
    if class_code(&vm.heap.obj(realm.global).class) != reg.global {
        return corrupt("the global object's class");
    }
    for (i, &o) in realm.intrinsics().iter().enumerate() {
        if reg.intrinsics.get(i) != Some(&class_code(&vm.heap.obj(o).class)) {
            return corrupt(format!("intrinsic {} has the wrong class", Intrinsic::ALL[i].name()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnijs_engine_api::Engine;

    /// A closure handed an environment without the bindings its code
    /// reads (what a damaged snapshot can pair) throws instead of
    /// panicking, on the fast path, the slow path and after a collection.
    #[test]
    fn code_over_the_wrong_environment_throws() {
        let mut vm = Vm::new(Rc::new(NullHost));
        let realm = vm.main_realm();
        vm.eval_script(realm, "a.js", "var f = (() => { let a = 1, b = 2; return () => a + b; })();")
            .unwrap();
        let bytes = vm.write_snapshot(realm).unwrap();
        let mut vm = restore(Rc::new(NullHost), &bytes).unwrap();
        let realm = vm.main_realm();
        let global = vm.realm_of(realm).global;
        let key = PropKey::Str(vm.heap.atom_units(Units::One(b"f")));
        let Some(Value::Object(f)) = vm.heap.get_own_property(global, key).and_then(|p| p.value()) else {
            panic!("f")
        };
        let empty = vm.heap.alloc_env(Environment::synthetic(0, None, false));
        vm.heap.obj_mut(f).function_mut().unwrap().env = Some(empty);
        for _ in 0..3 {
            let err = vm.eval_script(realm, "b.js", "f()").unwrap_err();
            assert!(err.to_string().contains("environment"), "{err}");
        }
    }
}

/// Helpers for fuzzing the loader past its checksum. Not a stable API.
#[doc(hidden)]
pub mod testing {
    use super::*;

    fn body_start(bytes: &[u8]) -> Option<usize> {
        let tag = format_tag();
        let header = 4 + tag.len() + 1;
        (bytes.len() >= header + 16).then_some(header + 16)
    }

    /// Recompute the checksum after editing the body, so a mutation
    /// reaches the decoder instead of the checksum test.
    pub fn reseal(bytes: &mut [u8]) {
        let Some(start) = body_start(bytes) else { return };
        let sum = checksum(&bytes[start..]);
        bytes[start - 8..start].copy_from_slice(&sum.to_le_bytes());
    }

    /// The byte range holding the encoded code trees, whose contents the
    /// bytecode trust model (ADR 0010) governs.
    pub fn code_span(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
        let start = body_start(bytes)?;
        let mut r = Reader { b: &bytes[start..], pos: 0 };
        let walk = |r: &mut Reader| -> Result<std::ops::Range<usize>, Error> {
            let strings = r.count(5)?;
            let bigints = r.count(4)?;
            let roots = r.count(4)?;
            r.take(12)?;
            for _ in 0..strings {
                let flags = r.u8()?;
                let n = r.count(1)?;
                r.take(if flags & 2 != 0 { n * 2 } else { n })?;
            }
            for _ in 0..bigints {
                let n = r.count(1)?;
                r.take(n)?;
            }
            let from = r.pos;
            for _ in 0..roots {
                let n = r.count(1)?;
                r.take(n)?;
            }
            Ok(from..r.pos)
        };
        walk(&mut r).ok().map(|s| s.start + start..s.end + start)
    }
}
