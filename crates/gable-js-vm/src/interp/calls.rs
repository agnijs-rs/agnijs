//! Calling and constructing: frame setup for bytecode functions, dispatch
//! to natives, bound functions and proxies, and function object creation.

use super::{ConstructKind, Flow, Frame, FrameKind, RunOutcome};
use crate::object::{BoundData, Class, ConstructorKind, FunctionData, ObjectData, ThisMode};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};
use gable_engine_api::RealmId;
use gable_js_compiler::bytecode::{CodeBlock, CodeKind};
use gable_js_parser::ast::FunctionKind;
use std::rc::Rc;

impl Vm {
    // ---- predicates ------------------------------------------------------------

    pub fn is_callable(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.heap.obj(o).class.is_callable(),
            _ => false,
        }
    }

    pub fn is_constructor(&self, v: Value) -> bool {
        let Value::Object(o) = v else { return false };
        match &self.heap.obj(o).class {
            Class::Function(f) => f.constructor_kind != ConstructorKind::None,
            Class::Native(n) => n.constructor,
            Class::Bound(b) => self.is_constructor(Value::Object(b.target)),
            Class::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    /// GetFunctionRealm.
    pub fn function_realm(&mut self, f: ObjId) -> JsResult<RealmId> {
        match &self.heap.obj(f).class {
            Class::Function(d) => Ok(d.realm),
            Class::Native(n) => Ok(n.realm),
            Class::Bound(b) => {
                let t = b.target;
                self.function_realm(t)
            }
            Class::Proxy(p) => match p.target {
                Some(t) if p.handler.is_some() => self.function_realm(t),
                _ => Err(self.throw_type("Cannot perform operation on a revoked proxy")),
            },
            _ => Ok(self.current_realm),
        }
    }

    /// GetPrototypeFromConstructor.
    pub fn get_prototype_from_constructor(
        &mut self,
        new_target: Value,
        default: Intrinsic,
    ) -> JsResult<ObjId> {
        let Value::Object(nt) = new_target else {
            return Ok(self.intrinsic(default));
        };
        let proto = self.get_prop(nt, PropKey::Str(self.names.prototype), new_target)?;
        match proto {
            Value::Object(p) => Ok(p),
            _ => {
                let realm = self.function_realm(nt)?;
                Ok(self.intrinsic_in(realm, default))
            }
        }
    }

    /// OrdinaryCreateFromConstructor for an ordinary object.
    pub fn ordinary_create_from_constructor(
        &mut self,
        new_target: Value,
        default: Intrinsic,
        class: Class,
    ) -> JsResult<ObjId> {
        let proto = self.get_prototype_from_constructor(new_target, default)?;
        Ok(self.alloc(ObjectData::new(Some(proto), class)))
    }

    // ---- calls from bytecode ----------------------------------------------------------

    /// `Call`: run a bytecode function in the same loop when possible.
    pub(crate) fn call_from_frame(
        &mut self,
        f: Value,
        this: Value,
        args: Vec<Value>,
    ) -> JsResult<Flow> {
        if let Value::Object(fo) = f {
            if let Class::Function(fd) = &self.heap.obj(fo).class {
                if fd.code.function_kind == FunctionKind::Normal {
                    if fd.is_class_constructor {
                        let realm = fd.realm;
                        let name = self.function_name_for_error(fo);
                        return Err(self.throw_in(
                            realm,
                            ErrorType::TypeError,
                            &format!("Class constructor {name} cannot be invoked without 'new'"),
                        ));
                    }
                    self.push_function_frame(
                        fo,
                        this,
                        args,
                        Value::Undefined,
                        ConstructKind::None,
                        false,
                    )?;
                    return Ok(Flow::Reload);
                }
            }
        }
        let v = self.call_value(f, this, &args)?;
        self.frames.last_mut().expect("call with no frame").acc = v;
        Ok(Flow::Next)
    }

    /// `Construct`: like `call_from_frame`, for `new`.
    pub(crate) fn construct_from_frame(
        &mut self,
        f: Value,
        args: Vec<Value>,
        new_target: Value,
    ) -> JsResult<Flow> {
        if !self.is_constructor(f) {
            let d = self.describe_for_error(f);
            return Err(self.throw_type(&format!("{d} is not a constructor")));
        }
        let Value::Object(fo) = f else { unreachable!() };
        if let Class::Function(fd) = &self.heap.obj(fo).class {
            let derived = fd.constructor_kind == ConstructorKind::Derived;
            let (this, kind) = if derived {
                (Value::Empty, ConstructKind::Derived)
            } else {
                let obj = self.ordinary_create_from_constructor(
                    new_target,
                    Intrinsic::ObjectPrototype,
                    Class::Ordinary,
                )?;
                (Value::Object(obj), ConstructKind::Base)
            };
            self.push_function_frame(fo, this, args, new_target, kind, false)?;
            return Ok(Flow::Reload);
        }
        let v = self.construct_value(f, &args, new_target)?;
        self.frames.last_mut().expect("construct with no frame").acc = v;
        Ok(Flow::Next)
    }

    /// Push a frame for a bytecode function. `this` is coerced here for
    /// plain calls.
    pub(crate) fn push_function_frame(
        &mut self,
        fo: ObjId,
        this: Value,
        args: Vec<Value>,
        new_target: Value,
        construct: ConstructKind,
        exit: bool,
    ) -> JsResult<()> {
        let (code, env, realm, this_mode, module) = {
            let fd = self
                .heap
                .obj(fo)
                .function()
                .expect("push_function_frame on a non-function");
            (fd.code.clone(), fd.env, fd.realm, fd.this_mode, fd.module)
        };
        let this = match (construct, this_mode) {
            (ConstructKind::None, ThisMode::Global) => {
                if this.is_nullish() {
                    Value::Object(self.realm_of(realm).global)
                } else if this.is_object() {
                    this
                } else {
                    Value::Object(self.to_object(this)?)
                }
            }
            _ => this,
        };
        let argc = args.len();
        let frame = Frame {
            code: code.clone(),
            pc: 0,
            base: 0,
            acc: Value::Undefined,
            env,
            base_env: env,
            env_depth: 0,
            this,
            callee: Some(fo),
            new_target,
            args: if code.keeps_arguments {
                args.clone()
            } else {
                Vec::new()
            },
            argc,
            realm,
            exit,
            construct,
            kind: FrameKind::Function,
            generator: None,
            module,
        };
        self.push_frame(frame)?;
        let base = self.frames.last().unwrap().base;
        let pb = base + code.param_base as usize;
        for i in 0..code.param_regs as usize {
            self.regs[pb + i] = args.get(i).copied().unwrap_or(Value::Undefined);
        }
        Ok(())
    }

    // ---- calls from Rust -------------------------------------------------------------

    /// Call(F, thisArgument, argumentsList).
    pub fn call_value(&mut self, f: Value, this: Value, args: &[Value]) -> JsResult<Value> {
        let Value::Object(fo) = f else {
            let d = self.describe_for_error(f);
            return Err(self.throw_type(&format!("{d} is not a function")));
        };
        self.call_object(fo, this, args)
    }

    pub fn call_object(&mut self, fo: ObjId, this: Value, args: &[Value]) -> JsResult<Value> {
        match &self.heap.obj(fo).class {
            Class::Function(fd) => {
                if fd.is_class_constructor {
                    let realm = fd.realm;
                    let name = self.function_name_for_error(fo);
                    return Err(self.throw_in(
                        realm,
                        ErrorType::TypeError,
                        &format!("Class constructor {name} cannot be invoked without 'new'"),
                    ));
                }
                let kind = fd.code.function_kind;
                match kind {
                    FunctionKind::Normal => self.run_function(
                        fo,
                        this,
                        args.to_vec(),
                        Value::Undefined,
                        ConstructKind::None,
                    ),
                    FunctionKind::Generator | FunctionKind::AsyncGenerator => self.run_function(
                        fo,
                        this,
                        args.to_vec(),
                        Value::Undefined,
                        ConstructKind::None,
                    ),
                    FunctionKind::Async => {
                        crate::interp::generator::call_async_function(self, fo, this, args.to_vec())
                    }
                }
            }
            Class::Native(_) => self.call_native(fo, this, args, Value::Undefined),
            Class::Bound(b) => {
                let target = b.target;
                let bthis = b.this;
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                self.call_object(target, bthis, &all)
            }
            Class::Proxy(_) => crate::builtins::proxy::proxy_call(self, fo, this, args),
            _ => {
                let d = self.describe_for_error(Value::Object(fo));
                Err(self.throw_type(&format!("{d} is not a function")))
            }
        }
    }

    /// Construct(F, argumentsList, newTarget).
    pub fn construct_value(
        &mut self,
        f: Value,
        args: &[Value],
        new_target: Value,
    ) -> JsResult<Value> {
        if !self.is_constructor(f) {
            let d = self.describe_for_error(f);
            return Err(self.throw_type(&format!("{d} is not a constructor")));
        }
        let Value::Object(fo) = f else { unreachable!() };
        match &self.heap.obj(fo).class {
            Class::Function(fd) => {
                let derived = fd.constructor_kind == ConstructorKind::Derived;
                let (this, kind) = if derived {
                    (Value::Empty, ConstructKind::Derived)
                } else {
                    let obj = self.ordinary_create_from_constructor(
                        new_target,
                        Intrinsic::ObjectPrototype,
                        Class::Ordinary,
                    )?;
                    (Value::Object(obj), ConstructKind::Base)
                };
                self.run_function(fo, this, args.to_vec(), new_target, kind)
            }
            Class::Native(_) => self.call_native(fo, Value::Undefined, args, new_target),
            Class::Bound(b) => {
                let target = b.target;
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                let nt = if new_target == f {
                    Value::Object(target)
                } else {
                    new_target
                };
                self.construct_value(Value::Object(target), &all, nt)
            }
            Class::Proxy(_) => crate::builtins::proxy::proxy_construct(self, fo, args, new_target),
            _ => unreachable!(),
        }
    }

    /// Run a bytecode function to completion in a nested interpreter loop.
    pub(crate) fn run_function(
        &mut self,
        fo: ObjId,
        this: Value,
        args: Vec<Value>,
        new_target: Value,
        construct: ConstructKind,
    ) -> JsResult<Value> {
        self.in_native(|vm| {
            vm.push_function_frame(fo, this, args, new_target, construct, true)?;
            match vm.run()? {
                RunOutcome::Return(v) => Ok(v),
                other => panic!("function run ended with {other:?}"),
            }
        })
    }

    // ---- function objects -------------------------------------------------------------

    /// InstantiateFunctionObject / OrdinaryFunctionCreate + MakeConstructor.
    pub fn create_function(
        &mut self,
        code: Rc<CodeBlock>,
        env: Option<crate::value::EnvId>,
        module: Option<ObjId>,
    ) -> ObjId {
        let realm = self.current_realm;
        let proto_intrinsic = match code.function_kind {
            FunctionKind::Normal => Intrinsic::FunctionPrototype,
            FunctionKind::Generator => Intrinsic::GeneratorFunctionPrototype,
            FunctionKind::Async => Intrinsic::AsyncFunctionPrototype,
            FunctionKind::AsyncGenerator => Intrinsic::AsyncGeneratorFunctionPrototype,
        };
        let proto = self.intrinsic(proto_intrinsic);
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
        let fd = FunctionData {
            code: code.clone(),
            env,
            realm,
            home_object: None,
            constructor_kind,
            this_mode,
            is_class_constructor: code.kind == CodeKind::ClassConstructor,
            class_data: None,
            module,
        };
        let f = self.alloc(ObjectData::new(Some(proto), Class::Function(Box::new(fd))));
        let len_key = PropKey::Str(self.names.length);
        self.heap.add_own_property(
            f,
            len_key,
            Property::data(
                Value::Number(code.param_count as f64),
                Attrs::CONFIGURABLE_ONLY,
            ),
        );
        let name = self.str(&code.name);
        let name_key = PropKey::Str(self.names.name);
        self.heap
            .add_own_property(f, name_key, Property::data(name, Attrs::CONFIGURABLE_ONLY));
        // Web-compatible extension (as in every major engine): plain sloppy
        // functions carry own undefined caller/arguments properties instead
        // of inheriting %Function.prototype%'s poison pills.
        if !code.strict
            && code.kind == CodeKind::Function
            && code.function_kind == FunctionKind::Normal
        {
            for n in ["caller", "arguments"] {
                let k = self.key(n);
                self.heap.add_own_property(
                    f,
                    k,
                    Property::data(Value::Undefined, Attrs::CONFIGURABLE_ONLY),
                );
            }
        }
        match code.function_kind {
            FunctionKind::Normal if is_ctor && code.kind != CodeKind::ClassConstructor => {
                let p = self.new_object();
                let ck = PropKey::Str(self.names.constructor);
                self.heap
                    .add_own_property(p, ck, Property::data(Value::Object(f), Attrs::METHOD));
                let pk = PropKey::Str(self.names.prototype);
                self.heap.add_own_property(
                    f,
                    pk,
                    Property::data(Value::Object(p), Attrs(Attrs::WRITABLE)),
                );
            }
            FunctionKind::Generator | FunctionKind::AsyncGenerator => {
                let pp = if code.function_kind == FunctionKind::Generator {
                    Intrinsic::GeneratorPrototype
                } else {
                    Intrinsic::AsyncGeneratorPrototype
                };
                let pp = self.intrinsic(pp);
                let p = self.new_object_with_proto(Some(pp));
                let pk = PropKey::Str(self.names.prototype);
                self.heap.add_own_property(
                    f,
                    pk,
                    Property::data(Value::Object(p), Attrs(Attrs::WRITABLE)),
                );
            }
            _ => {}
        }
        f
    }

    /// ClassDefinitionEvaluation: the constructor and prototype objects.
    pub(crate) fn create_class(
        &mut self,
        code: Rc<CodeBlock>,
        heritage: Value,
        name: Value,
        fi: usize,
    ) -> JsResult<(ObjId, ObjId)> {
        let (proto_parent, ctor_parent) = match heritage {
            Value::Empty => (
                Some(self.intrinsic(Intrinsic::ObjectPrototype)),
                self.intrinsic(Intrinsic::FunctionPrototype),
            ),
            Value::Null => (None, self.intrinsic(Intrinsic::FunctionPrototype)),
            h => {
                if !self.is_constructor(h) {
                    let d = self.describe_for_error(h);
                    return Err(self.throw_type(&format!(
                        "Class extends value {d} is not a constructor or null"
                    )));
                }
                let Value::Object(ho) = h else { unreachable!() };
                let pp = self.get_prop(ho, PropKey::Str(self.names.prototype), h)?;
                let pp = match pp {
                    Value::Object(o) => Some(o),
                    Value::Null => None,
                    _ => {
                        return Err(self.throw_type(
                            "Class extends value does not have valid prototype property",
                        ))
                    }
                };
                (pp, ho)
            }
        };
        let proto = self.new_object_with_proto(proto_parent);
        let env = self.frames[fi].env;
        let module = self.frames[fi].module;
        let f = self.create_function(code, env, module);
        self.heap.obj_mut(f).proto = Some(ctor_parent);
        if let Some(fd) = self.heap.obj_mut(f).function_mut() {
            fd.home_object = Some(proto);
        }
        if !name.is_empty() {
            self.set_function_name(f, name, None);
        }
        let pk = PropKey::Str(self.names.prototype);
        self.heap
            .add_own_property(f, pk, Property::data(Value::Object(proto), Attrs::NONE));
        let ck = PropKey::Str(self.names.constructor);
        self.heap
            .add_own_property(proto, ck, Property::data(Value::Object(f), Attrs::METHOD));
        Ok((f, proto))
    }

    /// SetFunctionName(F, name, prefix).
    pub fn set_function_name(&mut self, f: ObjId, name: Value, prefix: Option<&str>) {
        let base: Value = match name {
            Value::Symbol(s) => {
                let sd = self.heap.sym(s);
                match sd.description {
                    Some(d) if sd.is_private() => Value::String(d),
                    Some(d) => {
                        let d = self.heap.str_to_string(d);
                        self.str(&format!("[{d}]"))
                    }
                    None => self.str(""),
                }
            }
            Value::String(s) => Value::String(s),
            Value::Undefined | Value::Empty => self.str(""),
            other => {
                let s = self
                    .to_string(other)
                    .unwrap_or_else(|_| self.names.anonymous);
                Value::String(s)
            }
        };
        let full = match prefix {
            Some(p) => {
                let Value::String(b) = base else {
                    unreachable!()
                };
                let p = self.heap.alloc_str(&format!("{p} "));
                Value::String(self.heap.concat(p, b))
            }
            None => base,
        };
        let key = PropKey::Str(self.names.name);
        let prop = Property::data(full, Attrs::CONFIGURABLE_ONLY);
        if self.heap.has_own_property(f, key) {
            self.heap.replace_own_property(f, key, prop);
        } else {
            self.heap.add_own_property(f, key, prop);
        }
    }

    /// BoundFunctionCreate.
    pub fn bound_function_create(
        &mut self,
        target: ObjId,
        this: Value,
        args: Vec<Value>,
    ) -> JsResult<ObjId> {
        let proto = self.get_prototype_of(target)?;
        let data = BoundData { target, this, args };
        Ok(self.alloc(ObjectData::new(proto, Class::Bound(Box::new(data)))))
    }

    /// A short description of a value for "x is not a function" messages.
    pub fn describe_for_error(&mut self, v: Value) -> String {
        match v {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => gable_js_parser::number::to_js_string(n),
            Value::String(s) => format!("\"{}\"", self.heap.str_to_string(s)),
            Value::Symbol(s) => self.display_key(PropKey::Sym(s)),
            Value::BigInt(b) => format!("{}n", self.heap.bigint(b).0),
            Value::Object(o) => {
                if self.heap.obj(o).is_callable() {
                    let n = self.function_name_for_error(o);
                    if n.is_empty() {
                        "function".into()
                    } else {
                        format!("function {n}")
                    }
                } else if self.heap.obj(o).is_array() {
                    "[object Array]".into()
                } else {
                    "[object Object]".into()
                }
            }
            Value::Empty => "<empty>".into(),
        }
    }

    pub(crate) fn function_name_for_error(&mut self, f: ObjId) -> String {
        let key = PropKey::Str(self.names.name);
        match self.heap.get_own_property(f, key).and_then(|p| p.value()) {
            Some(Value::String(s)) => self.heap.str_to_string(s),
            _ => String::new(),
        }
    }

    /// Call `f` ignoring interrupts and OOM as JS exceptions (used for
    /// host-visible errors): converts a `Fault` to a thrown value.
    pub fn fault_to_value(&mut self, fault: Fault) -> Value {
        match fault {
            Fault::Throw(v) => v,
            Fault::Interrupt => self.make_error(crate::vm::ErrorType::Error, "interrupted"),
            Fault::OutOfMemory => {
                self.make_error(crate::vm::ErrorType::RangeError, "out of memory")
            }
        }
    }
}
