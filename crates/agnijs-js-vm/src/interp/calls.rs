//! Calling and constructing: frame setup for bytecode functions, dispatch
//! to natives, bound functions and proxies, and function object creation.

use super::{Args, ConstructKind, Flow, Frame, FrameKind, RunOutcome, INLINE_ARGS};
use crate::object::{
    BoundData, Class, ConstructorKind, FunctionData, NativeTag, ObjectData, ThisMode,
};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, PropSlot, Property, ShapeId, StrId, Value};
use crate::vm::{ErrorType, FnShape, Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;
use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, ConstString};
use agnijs_js_parser::ast::FunctionKind;
use std::rc::Rc;

impl Vm {
    // ---- predicates ------------------------------------------------------------

    pub fn is_callable(&self, v: Value) -> bool {
        match v {
            Value::Object(o) => self.heap.obj(o).class.is_callable(),
            _ => false,
        }
    }

    /// IsConstructor.
    #[inline]
    pub fn is_constructor(&self, v: Value) -> bool {
        let Value::Object(o) = v else {
            return false;
        };
        match &self.heap.obj(o).class {
            Class::Function(f) => f.constructor_kind != ConstructorKind::None,
            Class::Native(n) => n.constructor,
            Class::Bound(b) => self.is_constructor_through_bound(b.target),
            Class::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    /// IsConstructor of a bound function's target: a loop, not a recursion,
    /// since a chain of bound functions can be as long as memory allows
    /// (TASK-103.47). Kept out of line so the common case above stays small.
    #[inline(never)]
    fn is_constructor_through_bound(&self, mut o: ObjId) -> bool {
        loop {
            match &self.heap.obj(o).class {
                Class::Function(f) => return f.constructor_kind != ConstructorKind::None,
                Class::Native(n) => return n.constructor,
                Class::Bound(b) => o = b.target,
                Class::Proxy(p) => return p.constructor,
                _ => return false,
            }
        }
    }

    /// GetFunctionRealm. A loop through bound functions and proxies, which
    /// can nest as deep as memory allows (TASK-103.47).
    pub fn function_realm(&mut self, f: ObjId) -> JsResult<RealmId> {
        let mut f = f;
        loop {
            match &self.heap.obj(f).class {
                Class::Function(d) => return Ok(d.realm),
                Class::Native(n) => return Ok(n.realm),
                Class::Bound(b) => f = b.target,
                Class::Proxy(p) => match p.target {
                    Some(t) if p.handler.is_some() => f = t,
                    _ => return Err(self.throw_type("Cannot perform operation on a revoked proxy")),
                },
                _ => return Ok(self.current_realm),
            }
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
    ///
    /// `Function.prototype.call` and `apply` with a bytecode target are
    /// unwrapped here (ADR 0019 J8): the target's frame is pushed in the
    /// current run loop, with no native frame and no nested `Vm::run`. Every
    /// other case, including all the error cases, takes the native.
    pub(crate) fn call_from_frame(
        &mut self,
        f: Value,
        this: Value,
        args: Args,
    ) -> JsResult<Flow> {
        if let Value::Object(fo) = f {
            match &self.heap.obj(fo).class {
                Class::Function(fd) if fd.code.function_kind == FunctionKind::Normal => {
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
                Class::Native(n) if n.tag != NativeTag::None => {
                    let tag = n.tag;
                    if self.call_through(tag, this, args)? {
                        return Ok(Flow::Reload);
                    }
                }
                _ => {}
            }
        }
        let v = self.with_arg_slice(args, |vm, a| vm.call_value(f, this, a))?;
        self.frames.last_mut().expect("call with no frame").acc = v;
        Ok(Flow::Next)
    }

    /// A function whose call can be a frame push in the current run loop: a
    /// plain (not generator, not async) bytecode function that is not a
    /// class constructor.
    #[inline]
    pub(crate) fn inlinable_callee(&self, f: Value) -> Option<ObjId> {
        let Value::Object(fo) = f else { return None };
        match &self.heap.obj(fo).class {
            Class::Function(fd)
                if fd.code.function_kind == FunctionKind::Normal && !fd.is_class_constructor =>
            {
                Some(fo)
            }
            _ => None,
        }
    }

    /// `target.call(...)` or `target.apply(...)` (by `tag`) from bytecode:
    /// push the target's frame and return `true`, or return `false` to let
    /// the native run. Only the effects the native would have are performed
    /// (CreateListFromArrayLike for `apply`), in the same order.
    fn call_through(&mut self, tag: NativeTag, target: Value, args: Args) -> JsResult<bool> {
        let Some(t) = self.inlinable_callee(target) else {
            return Ok(false);
        };
        let this = self.arg_at(args, 0);
        match tag {
            NativeTag::None | NativeTag::RegExpExec => Ok(false),
            NativeTag::FunctionCall => {
                let rest = args.skip(1);
                self.push_function_frame(t, this, rest, Value::Undefined, ConstructKind::None, false)?;
                Ok(true)
            }
            NativeTag::FunctionApply => {
                let list = self.arg_at(args, 1);
                if list.is_nullish() {
                    self.push_function_frame(
                        t,
                        this,
                        Args::Slice(&[]),
                        Value::Undefined,
                        ConstructKind::None,
                        false,
                    )?;
                    return Ok(true);
                }
                let Value::Object(lo) = list else {
                    // The native throws the TypeError.
                    return Ok(false);
                };
                // A dense array without holes is copied as is (what
                // CreateListFromArrayLike reads, with nothing observable);
                // the scratch buffer saves the allocation.
                let mut buf = std::mem::take(&mut self.arg_scratch);
                buf.clear();
                match self.heap.dense_values(lo) {
                    Some(d) => buf.extend_from_slice(d),
                    None => {
                        let list = self.create_list_from_array_like(list);
                        match list {
                            Ok(l) => buf = l,
                            Err(e) => {
                                self.arg_scratch = buf;
                                return Err(e);
                            }
                        }
                    }
                }
                let r = self.push_function_frame(
                    t,
                    this,
                    Args::Slice(&buf),
                    Value::Undefined,
                    ConstructKind::None,
                    false,
                );
                self.arg_scratch = buf;
                r.map(|()| true)
            }
        }
    }

    /// Argument `i` of a call, or undefined.
    #[inline]
    pub(crate) fn arg_at(&self, args: Args, i: usize) -> Value {
        match args {
            Args::Regs { start, len } if i < len => self.regs[start + i],
            Args::Slice(s) => s.get(i).copied().unwrap_or(Value::Undefined),
            _ => Value::Undefined,
        }
    }

    /// Run `k` with the arguments as a slice. Register arguments are copied
    /// to a buffer on the Rust stack (the register file may grow under `k`),
    /// so a native called from bytecode allocates nothing for them.
    #[inline]
    pub(crate) fn with_arg_slice<R>(
        &mut self,
        args: Args,
        k: impl FnOnce(&mut Vm, &[Value]) -> R,
    ) -> R {
        match args {
            Args::Slice(s) => k(self, s),
            Args::Regs { start, len } if len <= INLINE_ARGS => {
                let mut buf = [Value::Undefined; INLINE_ARGS];
                buf[..len].copy_from_slice(&self.regs[start..start + len]);
                k(self, &buf[..len])
            }
            Args::Regs { start, len } => {
                let v = self.regs[start..start + len].to_vec();
                k(self, &v)
            }
        }
    }

    /// `Construct`: like `call_from_frame`, for `new`.
    pub(crate) fn construct_from_frame(
        &mut self,
        f: Value,
        args: Args,
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
        let v = self.with_arg_slice(args, |vm, a| vm.construct_value(f, a, new_target))?;
        self.frames.last_mut().expect("construct with no frame").acc = v;
        Ok(Flow::Next)
    }

    /// Push a frame for a bytecode function: the one frame-push path for
    /// calls (ADR 0019 J8). `this` is coerced here for plain calls. The
    /// arguments are copied straight into the new frame's parameter
    /// registers; they are kept as a list only when the code needs it (an
    /// arguments object or rest parameters), so a call allocates nothing.
    pub(crate) fn push_function_frame(
        &mut self,
        fo: ObjId,
        this: Value,
        args: Args,
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
                    // The wrapper comes from the callee's realm, which the
                    // native that used to sit in between (call/apply) set.
                    let caller = self.current_realm;
                    self.current_realm = realm;
                    let o = self.to_object(this);
                    self.current_realm = caller;
                    Value::Object(o?)
                }
            }
            _ => this,
        };
        let argc = args.len();
        let param_base = code.param_base as usize;
        let param_regs = code.param_regs as usize;
        let kept = if code.keeps_arguments {
            match args {
                Args::Regs { start, len } => self.regs[start..start + len].to_vec(),
                Args::Slice(s) => s.to_vec(),
            }
        } else {
            Vec::new()
        };
        let frame = Frame {
            code,
            pc: 0,
            base: 0,
            acc: Value::Undefined,
            env,
            base_env: env,
            env_depth: 0,
            this,
            callee: Some(fo),
            new_target,
            args: kept,
            argc,
            realm,
            exit,
            discard_result: false,
            construct,
            kind: FrameKind::Function,
            generator: None,
            module,
        };
        self.push_frame(frame)?;
        // The caller's registers sit below the new window, so a register
        // run of arguments is still in place after the file grew.
        let pb = self.frames.last().unwrap().base + param_base;
        let n = argc.min(param_regs);
        match args {
            Args::Regs { start, .. } => self.regs.copy_within(start..start + n, pb),
            Args::Slice(s) => self.regs[pb..pb + n].copy_from_slice(&s[..n]),
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
                    FunctionKind::Normal
                    | FunctionKind::Generator
                    | FunctionKind::AsyncGenerator => self.run_function(
                        fo,
                        this,
                        args,
                        Value::Undefined,
                        ConstructKind::None,
                    ),
                    FunctionKind::Async => {
                        crate::interp::generator::call_async_function(self, fo, this, args)
                    }
                }
            }
            Class::Native(_) => self.call_native(fo, this, args, Value::Undefined),
            Class::Bound(b) => {
                let target = b.target;
                let bthis = b.this;
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                self.check_stack()?;
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
                self.run_function(fo, this, args, new_target, kind)
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
                self.check_stack()?;
                self.construct_value(Value::Object(target), &all, nt)
            }
            Class::Proxy(_) => crate::builtins::proxy::proxy_construct(self, fo, args, new_target),
            _ => unreachable!(),
        }
    }

    /// Run a bytecode function to completion from Rust, through the one
    /// entry into JavaScript ([`Vm::run_entry`]).
    pub(crate) fn run_function(
        &mut self,
        fo: ObjId,
        this: Value,
        args: &[Value],
        new_target: Value,
        construct: ConstructKind,
    ) -> JsResult<Value> {
        let outcome = self.run_entry(true, |vm| {
            vm.push_function_frame(fo, this, Args::Slice(args), new_target, construct, true)
        })?;
        match outcome {
            RunOutcome::Return(v) => Ok(v),
            other => panic!("function run ended with {other:?}"),
        }
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
        let name = self.function_name(&code);
        let fd = match self.heap.fn_pool.pop() {
            Some(mut b) => {
                *b = fd;
                b
            }
            None => Box::new(fd),
        };
        let f = self.alloc(ObjectData::new(Some(proto), Class::Function(fd)));
        // The eager properties, in order: length, name, then (sloppy
        // functions, a web-compatible extension as in every major engine)
        // own undefined caller and arguments instead of inheriting
        // %Function.prototype%'s poison pills, then prototype.
        let length = PropSlot::Data(Value::Number(code.param_count as f64));
        let name = PropSlot::Data(name);
        let sloppy = !code.strict
            && code.kind == CodeKind::Function
            && code.function_kind == FunctionKind::Normal;
        let prototype = match code.function_kind {
            FunctionKind::Normal if is_ctor && code.kind != CodeKind::ClassConstructor => {
                let p = self.new_object();
                let shape = self.fn_shape(FnShape::ProtoObject);
                self.heap
                    .init_named(p, shape, &[PropSlot::Data(Value::Object(f))]);
                Some(p)
            }
            FunctionKind::Generator | FunctionKind::AsyncGenerator => {
                let pp = if code.function_kind == FunctionKind::Generator {
                    Intrinsic::GeneratorPrototype
                } else {
                    Intrinsic::AsyncGeneratorPrototype
                };
                let pp = self.intrinsic(pp);
                Some(self.new_object_with_proto(Some(pp)))
            }
            _ => None,
        };
        match prototype {
            None => {
                let shape = self.fn_shape(FnShape::Plain);
                self.heap.init_named(f, shape, &[length, name]);
            }
            Some(p) if sloppy => {
                let shape = self.fn_shape(FnShape::Sloppy);
                let u = PropSlot::UNDEFINED;
                let p = PropSlot::Data(Value::Object(p));
                self.heap.init_named(f, shape, &[length, name, u, u, p]);
            }
            Some(p) => {
                let shape = self.fn_shape(FnShape::WithPrototype);
                let p = PropSlot::Data(Value::Object(p));
                self.heap.init_named(f, shape, &[length, name, p]);
            }
        }
        f
    }

    /// A function's initial `name` value: one string per block, kept by
    /// the block's feedback like a string constant.
    fn function_name(&mut self, code: &CodeBlock) -> Value {
        let epoch = self.gc_count;
        let cell = code.feedback(self.feedback_owner).map(|f| &f.name);
        if let Some(c) = cell {
            let e = c.get();
            if e.epoch == epoch {
                return Value::String(StrId(e.id));
            }
        }
        let id = self.heap.atom(&code.name);
        if let Some(c) = cell {
            c.set(ConstString { id: id.0, epoch });
        }
        Value::String(id)
    }

    /// The shape a new function or prototype object starts with.
    fn fn_shape(&mut self, which: FnShape) -> ShapeId {
        let s = self.fn_shapes[which as usize];
        if s != ShapeId(0) {
            return s;
        }
        let c = Attrs::CONFIGURABLE_ONLY;
        let n = &self.names;
        let (length, name) = (PropKey::Str(n.length), PropKey::Str(n.name));
        let prototype = (PropKey::Str(n.prototype), Attrs(Attrs::WRITABLE));
        let keys: Vec<(PropKey, Attrs)> = match which {
            FnShape::Plain => vec![(length, c), (name, c)],
            FnShape::WithPrototype => vec![(length, c), (name, c), prototype],
            FnShape::Sloppy => vec![
                (length, c),
                (name, c),
                (PropKey::Str(n.caller), c),
                (PropKey::Str(n.arguments), c),
                prototype,
            ],
            FnShape::ProtoObject => vec![(PropKey::Str(n.constructor), Attrs::METHOD)],
        };
        let s = self.heap.shape_with(&keys);
        self.fn_shapes[which as usize] = s;
        s
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
        self.heap.set_proto(f, Some(ctor_parent));
        if let Some(fd) = self.heap.obj_mut(f).function_mut() {
            fd.home_object = Some(proto);
        }
        if !name.is_empty() {
            self.set_function_name(f, name, None)?;
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
    pub fn set_function_name(
        &mut self,
        f: ObjId,
        name: Value,
        prefix: Option<&str>,
    ) -> JsResult<()> {
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
                Value::String(self.concat(p, b)?)
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
        Ok(())
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
            Value::Number(n) => agnijs_js_parser::number::to_js_string(n),
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
            Fault::OutOfFuel => self.make_error(crate::vm::ErrorType::Error, "out of fuel"),
            Fault::OutOfMemory => {
                self.make_error(crate::vm::ErrorType::RangeError, "out of memory")
            }
        }
    }
}
