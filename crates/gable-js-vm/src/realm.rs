//! Realms: a global object, a global environment and the intrinsics table.

use crate::env::Environment;
use crate::object::{Class, ObjectData};
use crate::value::{Attrs, EnvId, ObjId, PropKey, Property, Value};
use crate::vm::Vm;
use gable_engine_api::RealmId;
use gable_js_compiler::bytecode::{BindingKind, CodeBlock};
use hashbrown::HashMap;
use indexmap::IndexMap;
use std::rc::Rc;

macro_rules! intrinsics {
    ($($name:ident),* $(,)?) => {
        /// Every well-known intrinsic object of a realm.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u16)]
        pub enum Intrinsic { $($name,)* }
        impl Intrinsic {
            pub const COUNT: usize = [$(Intrinsic::$name),*].len();
            pub const ALL: [Intrinsic; Self::COUNT] = [$(Intrinsic::$name),*];
            pub fn name(self) -> &'static str {
                match self { $(Intrinsic::$name => stringify!($name),)* }
            }
        }
    };
}

intrinsics! {
    ObjectPrototype,
    FunctionPrototype,
    Object,
    Function,
    Array,
    ArrayPrototype,
    ArrayPrototypeValues,
    String,
    StringPrototype,
    Number,
    NumberPrototype,
    Boolean,
    BooleanPrototype,
    Symbol,
    SymbolPrototype,
    BigInt,
    BigIntPrototype,
    Error,
    ErrorPrototype,
    TypeError,
    TypeErrorPrototype,
    RangeError,
    RangeErrorPrototype,
    ReferenceError,
    ReferenceErrorPrototype,
    SyntaxError,
    SyntaxErrorPrototype,
    EvalError,
    EvalErrorPrototype,
    URIError,
    URIErrorPrototype,
    AggregateError,
    AggregateErrorPrototype,
    Math,
    JSON,
    Reflect,
    Eval,
    ThrowTypeError,
    IteratorPrototype,
    Iterator,
    ArrayIteratorPrototype,
    StringIteratorPrototype,
    ForInIteratorPrototype,
    GeneratorFunction,
    GeneratorFunctionPrototype,
    GeneratorPrototype,
    AsyncFunction,
    AsyncFunctionPrototype,
    AsyncGeneratorFunction,
    AsyncGeneratorFunctionPrototype,
    AsyncGeneratorPrototype,
    AsyncIteratorPrototype,
    AsyncFromSyncIteratorPrototype,
    Promise,
    PromisePrototype,
    PromiseResolve,
    RegExp,
    RegExpPrototype,
    RegExpStringIteratorPrototype,
    Date,
    DatePrototype,
    Map,
    MapPrototype,
    MapIteratorPrototype,
    Set,
    SetPrototype,
    SetIteratorPrototype,
    WeakMap,
    WeakMapPrototype,
    WeakSet,
    WeakSetPrototype,
    WeakRef,
    WeakRefPrototype,
    FinalizationRegistry,
    FinalizationRegistryPrototype,
    ArrayBuffer,
    ArrayBufferPrototype,
    SharedArrayBuffer,
    SharedArrayBufferPrototype,
    DataView,
    DataViewPrototype,
    TypedArray,
    TypedArrayPrototype,
    Int8Array,
    Int8ArrayPrototype,
    Uint8Array,
    Uint8ArrayPrototype,
    Uint8ClampedArray,
    Uint8ClampedArrayPrototype,
    Int16Array,
    Int16ArrayPrototype,
    Uint16Array,
    Uint16ArrayPrototype,
    Int32Array,
    Int32ArrayPrototype,
    Uint32Array,
    Uint32ArrayPrototype,
    Float32Array,
    Float32ArrayPrototype,
    Float64Array,
    Float64ArrayPrototype,
    BigInt64Array,
    BigInt64ArrayPrototype,
    BigUint64Array,
    BigUint64ArrayPrototype,
    Float16Array,
    Float16ArrayPrototype,
    Proxy,
    Atomics,
    IteratorHelperPrototype,
    WrapForValidIteratorPrototype,
    Intl,
}

/// A global lexical binding (`let`, `const`, `class` at script top level).
#[derive(Clone, Copy, Debug)]
pub struct LexBinding {
    pub value: Value,
    pub kind: BindingKind,
}

pub struct Realm {
    pub id: RealmId,
    pub global: ObjId,
    pub global_env: EnvId,
    pub global_lexicals: IndexMap<Rc<str>, LexBinding>,
    intrinsics: Vec<ObjId>,
    /// Template objects by (code block address, constant index).
    pub template_cache: HashMap<(usize, u32), (Rc<CodeBlock>, ObjId)>,
    /// `$262`-style host data: scripts evaluated in this realm, by name.
    pub script_names: Vec<String>,
    /// Names declared by `var` and function declarations of scripts
    /// (the global environment record's [[VarNames]]).
    pub var_names: hashbrown::HashSet<Rc<str>>,
}

impl Realm {
    #[inline]
    pub fn intrinsic(&self, i: Intrinsic) -> ObjId {
        self.intrinsics[i as usize]
    }

    pub fn set_intrinsic(&mut self, i: Intrinsic, obj: ObjId) {
        self.intrinsics[i as usize] = obj;
    }

    pub fn intrinsics(&self) -> &[ObjId] {
        &self.intrinsics
    }

    pub fn lexical(&self, name: &str) -> Option<&LexBinding> {
        self.global_lexicals.get(name)
    }
}

impl Vm {
    /// Create a realm with the standard library installed and return its id.
    pub fn create_realm(&mut self) -> RealmId {
        let id = RealmId(self.realms.len() as u32);
        let saved = self.current_realm;
        // The object prototype has no prototype; every other intrinsic
        // hangs off it. Placeholder ids are replaced by the installers.
        let object_prototype = self.alloc(ObjectData::new(None, Class::Ordinary));
        let global = self.alloc(ObjectData::new(Some(object_prototype), Class::Ordinary));
        let global_env = self.heap.alloc_env(Environment::Global(id));
        let realm = Realm {
            id,
            global,
            global_env,
            global_lexicals: IndexMap::new(),
            intrinsics: vec![object_prototype; Intrinsic::COUNT],
            template_cache: HashMap::new(),
            script_names: Vec::new(),
            var_names: hashbrown::HashSet::new(),
        };
        self.realms.push(realm);
        self.current_realm = id;
        crate::builtins::install(self, id);
        // `globalThis`
        let key = PropKey::Str(self.names.globalThis);
        self.heap.add_own_property(
            global,
            key,
            Property::data(Value::Object(global), Attrs::METHOD),
        );
        self.current_realm = if saved.0 as usize >= self.realms.len() {
            id
        } else {
            saved
        };
        if self.realms.len() == 1 {
            self.current_realm = id;
        }
        id
    }
}
