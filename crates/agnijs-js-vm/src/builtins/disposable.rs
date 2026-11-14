//! Explicit resource management (ES2026): DisposableStack,
//! AsyncDisposableStack, %IteratorPrototype%[@@dispose],
//! %AsyncIteratorPrototype%[@@asyncDispose], and the dispose capability
//! behind `using` and `await using` scopes.
//!
//! A dispose capability is a `Class::DisposableStack` object. DisposeResources
//! is split into steps so the same state machine serves the synchronous
//! `dispose()`, the promise-returning `disposeAsync()` and the bytecode of a
//! scope's exit (which awaits in its own frame): [`step`] runs disposal
//! methods until one needs an Await and hands back the value to await; an
//! Await that rejects is recorded with [`record_error`].

use crate::builtins::promise;
use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;

/// How a resource's dispose method is called.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `method.call(value)` (`using`, `use()`).
    This,
    /// `method(value)` (`adopt()`).
    Arg,
    /// `method()` (`defer()`).
    Bare,
    /// An `await using` resource with only a `Symbol.dispose` method: it
    /// is called with the value as `this`, its result is dropped and the
    /// disposal awaits undefined (GetDisposeMethod's wrapper closure).
    SyncFallback,
}

pub struct Resource {
    pub value: Value,
    /// Undefined for a null or undefined `await using` value.
    pub method: Value,
    pub is_async: bool,
    pub call: CallKind,
}

pub struct DisposeData {
    /// An AsyncDisposableStack (only meaningful for the script-visible
    /// stacks; a scope's internal stack mixes hints freely).
    pub is_async: bool,
    pub disposed: bool,
    pub resources: Vec<Resource>,
    // DisposeResources in progress.
    pub needs_await: bool,
    pub has_awaited: bool,
    /// The completion so far is a throw of `error`.
    pub has_error: bool,
    pub error: Value,
    /// A disposal method threw (the completion is no longer the original).
    pub changed: bool,
}

impl DisposeData {
    pub fn new(is_async: bool) -> DisposeData {
        DisposeData {
            is_async,
            disposed: false,
            resources: Vec::new(),
            needs_await: false,
            has_awaited: false,
            has_error: false,
            error: Value::Undefined,
            changed: false,
        }
    }

    pub fn trace(&self, t: &mut Tracer) {
        for r in &self.resources {
            t.value(r.value);
            t.value(r.method);
        }
        t.value(self.error);
    }
}

fn data(vm: &mut Vm, o: ObjId) -> &mut DisposeData {
    match &mut vm.heap.obj_mut(o).class {
        Class::DisposableStack(d) => d,
        _ => unreachable!("a dispose capability is a DisposableStack object"),
    }
}

// ---- the dispose capability -------------------------------------------------------------

/// The dispose capability `v` holds, if it is one (bytecode registers are
/// checked rather than trusted).
pub fn as_capability(vm: &Vm, v: Value) -> Option<ObjId> {
    match v {
        Value::Object(o) if matches!(vm.heap.obj(o).class, Class::DisposableStack(_)) => Some(o),
        _ => None,
    }
}

/// A new, empty internal dispose capability (a `using` scope's).
pub fn new_capability(vm: &mut Vm) -> ObjId {
    vm.alloc(ObjectData::new(None, Class::DisposableStack(Box::new(DisposeData::new(false)))))
}

/// AddDisposableResource(capability, value, hint) for a `using` (or
/// `use()`) value: null and undefined are skipped for sync-dispose and
/// recorded with no method for async-dispose; anything else must be an
/// object with a callable dispose method.
pub fn add_value(vm: &mut Vm, stack: ObjId, v: Value, is_async: bool) -> JsResult<()> {
    let resource = match v {
        Value::Undefined | Value::Null => {
            if !is_async {
                return Ok(());
            }
            Resource { value: Value::Undefined, method: Value::Undefined, is_async, call: CallKind::This }
        }
        Value::Object(_) => {
            let mut call = CallKind::This;
            let mut method = Value::Undefined;
            if is_async {
                method = vm.get_method(v, PropKey::Sym(vm.well_known.async_dispose))?.unwrap_or(Value::Undefined);
            }
            if method.is_undefined() {
                method = vm.get_method(v, PropKey::Sym(vm.well_known.dispose))?.unwrap_or(Value::Undefined);
                if is_async && !method.is_undefined() {
                    call = CallKind::SyncFallback;
                }
            }
            if method.is_undefined() {
                let which = if is_async { "Symbol.asyncDispose or Symbol.dispose" } else { "Symbol.dispose" };
                return vm.type_error(&format!("the value has no {which} method"));
            }
            Resource { value: v, method, is_async, call }
        }
        _ => return vm.type_error("a disposable resource must be an object, null or undefined"),
    };
    data(vm, stack).resources.push(resource);
    vm.heap.note_growth(std::mem::size_of::<Resource>());
    Ok(())
}

fn add_method(vm: &mut Vm, stack: ObjId, value: Value, method: Value, call: CallKind) {
    let d = data(vm, stack);
    let is_async = d.is_async;
    d.resources.push(Resource { value, method, is_async, call });
    vm.heap.note_growth(std::mem::size_of::<Resource>());
}

/// Start DisposeResources with the scope's completion (`error` when it is
/// a throw).
pub fn begin(vm: &mut Vm, stack: ObjId, has_error: bool, error: Value) {
    let d = data(vm, stack);
    d.needs_await = false;
    d.has_awaited = false;
    d.has_error = has_error;
    d.error = if has_error { error } else { Value::Undefined };
    d.changed = false;
}

/// Fold a disposal error into the completion: the first error replaces a
/// normal completion; a later one wraps the current error in a
/// SuppressedError.
pub fn record_error(vm: &mut Vm, stack: ObjId, e: Value) {
    let (has_error, prev) = {
        let d = data(vm, stack);
        (d.has_error, d.error)
    };
    let error = if has_error { super::error::new_suppressed_error(vm, e, prev) } else { e };
    let d = data(vm, stack);
    d.has_error = true;
    d.error = error;
    d.changed = true;
}

/// Run disposal methods, newest first, until one needs an Await: returns
/// the value to await (its rejection goes to [`record_error`]), or `None`
/// when every resource is disposed.
pub fn step(vm: &mut Vm, stack: ObjId) -> JsResult<Option<Value>> {
    loop {
        let Some(r) = data(vm, stack).resources.pop() else {
            let d = data(vm, stack);
            if d.needs_await && !d.has_awaited {
                d.needs_await = false;
                return Ok(Some(Value::Undefined));
            }
            return Ok(None);
        };
        {
            let d = data(vm, stack);
            if !r.is_async && d.needs_await && !d.has_awaited {
                // Await(undefined) before the sync resource, then dispose it.
                d.needs_await = false;
                d.resources.push(r);
                return Ok(Some(Value::Undefined));
            }
        }
        if r.method.is_undefined() {
            data(vm, stack).needs_await = true;
            continue;
        }
        let result = match r.call {
            CallKind::This | CallKind::SyncFallback => vm.call_value(r.method, r.value, &[]),
            CallKind::Arg => vm.call_value(r.method, Value::Undefined, &[r.value]),
            CallKind::Bare => vm.call_value(r.method, Value::Undefined, &[]),
        };
        match result {
            Ok(v) if r.is_async => {
                data(vm, stack).has_awaited = true;
                return Ok(Some(if r.call == CallKind::SyncFallback { Value::Undefined } else { v }));
            }
            Ok(_) => {}
            Err(Fault::Throw(e)) if r.call == CallKind::SyncFallback => {
                // The wrapper returns a promise rejected with the error.
                data(vm, stack).has_awaited = true;
                let cap = promise::new_internal_capability_intrinsic(vm);
                promise::reject_capability(vm, &cap, e)?;
                return Ok(Some(Value::Object(cap.promise)));
            }
            Err(Fault::Throw(e)) => record_error(vm, stack, e),
            Err(f) => return Err(f),
        }
    }
}

/// The completion after disposal: `Err` with the new error when a
/// disposal method threw, else `Ok` (the scope's own completion stands).
pub fn end(vm: &mut Vm, stack: ObjId) -> JsResult<()> {
    let d = data(vm, stack);
    d.resources.clear();
    if d.changed {
        let e = d.error;
        d.error = Value::Undefined;
        return Err(Fault::Throw(e));
    }
    d.error = Value::Undefined;
    Ok(())
}

/// The whole of a synchronous DisposeResources with a normal completion.
fn dispose_sync(vm: &mut Vm, stack: ObjId) -> JsResult<()> {
    begin(vm, stack, false, Value::Undefined);
    while step(vm, stack)?.is_some() {}
    end(vm, stack)
}

// ---- install ----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);

    let proto = vm.new_object_with_proto(Some(object_proto));
    super::install_ctor(
        vm,
        realm,
        "DisposableStack",
        0,
        disposable_stack_ctor,
        proto,
        Intrinsic::DisposableStack,
        Intrinsic::DisposableStackPrototype,
    );
    vm.method(proto, "adopt", 2, ds_adopt);
    vm.method(proto, "defer", 1, ds_defer);
    let dispose = vm.method(proto, "dispose", 0, ds_dispose);
    vm.accessor(proto, "disposed", ds_disposed, None);
    vm.method(proto, "move", 0, ds_move);
    vm.method(proto, "use", 1, ds_use);
    let key = PropKey::Sym(vm.well_known.dispose);
    vm.define_raw(proto, key, Value::Object(dispose), Attrs::METHOD);
    super::to_string_tag(vm, proto, "DisposableStack");

    let proto = vm.new_object_with_proto(Some(object_proto));
    super::install_ctor(
        vm,
        realm,
        "AsyncDisposableStack",
        0,
        async_disposable_stack_ctor,
        proto,
        Intrinsic::AsyncDisposableStack,
        Intrinsic::AsyncDisposableStackPrototype,
    );
    vm.method(proto, "adopt", 2, ads_adopt);
    vm.method(proto, "defer", 1, ads_defer);
    let dispose_async = vm.method(proto, "disposeAsync", 0, ads_dispose_async);
    vm.accessor(proto, "disposed", ads_disposed, None);
    vm.method(proto, "move", 0, ads_move);
    vm.method(proto, "use", 1, ads_use);
    let key = PropKey::Sym(vm.well_known.async_dispose);
    vm.define_raw(proto, key, Value::Object(dispose_async), Attrs::METHOD);
    super::to_string_tag(vm, proto, "AsyncDisposableStack");

    let iter_proto = vm.intrinsic(Intrinsic::IteratorPrototype);
    let sym = vm.well_known.dispose;
    vm.method_sym(iter_proto, sym, "Symbol.dispose", 0, iterator_dispose);
    let async_iter_proto = vm.intrinsic(Intrinsic::AsyncIteratorPrototype);
    let sym = vm.well_known.async_dispose;
    vm.method_sym(async_iter_proto, sym, "Symbol.asyncDispose", 0, async_iterator_async_dispose);
}

// ---- DisposableStack and AsyncDisposableStack ----------------------------------------------

fn construct(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let name = if is_async { "AsyncDisposableStack" } else { "DisposableStack" };
    if a.new_target.is_undefined() {
        return vm.type_error(&format!("Constructor {name} requires 'new'"));
    }
    let default = if is_async { Intrinsic::AsyncDisposableStackPrototype } else { Intrinsic::DisposableStackPrototype };
    let o = vm.ordinary_create_from_constructor(
        a.new_target,
        default,
        Class::DisposableStack(Box::new(DisposeData::new(is_async))),
    )?;
    Ok(Value::Object(o))
}

fn disposable_stack_ctor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct(vm, a, false)
}

fn async_disposable_stack_ctor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct(vm, a, true)
}

fn brand(vm: &mut Vm, this: Value, is_async: bool) -> Option<ObjId> {
    match this {
        Value::Object(o) => match &vm.heap.obj(o).class {
            Class::DisposableStack(d) if d.is_async == is_async => Some(o),
            _ => None,
        },
        _ => None,
    }
}

fn this_stack(vm: &mut Vm, a: &CallArgs, is_async: bool, method: &str) -> JsResult<ObjId> {
    match brand(vm, a.this, is_async) {
        Some(o) => Ok(o),
        None => {
            let name = if is_async { "AsyncDisposableStack" } else { "DisposableStack" };
            vm.type_error(&format!("{name}.prototype.{method} called on an incompatible receiver"))
        }
    }
}

/// The stack, which must not be disposed (a ReferenceError otherwise).
fn pending_stack(vm: &mut Vm, a: &CallArgs, is_async: bool, method: &str) -> JsResult<ObjId> {
    let o = this_stack(vm, a, is_async, method)?;
    if data(vm, o).disposed {
        let name = if is_async { "AsyncDisposableStack" } else { "DisposableStack" };
        return Err(vm.throw(ErrorType::ReferenceError, &format!("{name}.prototype.{method}: the stack is already disposed")));
    }
    Ok(o)
}

fn use_(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, a, is_async, "use")?;
    let v = a.arg(0);
    add_value(vm, o, v, is_async)?;
    Ok(v)
}

fn adopt(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, a, is_async, "adopt")?;
    let on_dispose = a.arg(1);
    if !vm.is_callable(on_dispose) {
        return vm.type_error("adopt: onDispose is not callable");
    }
    let v = a.arg(0);
    add_method(vm, o, v, on_dispose, CallKind::Arg);
    Ok(v)
}

fn defer(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, a, is_async, "defer")?;
    let on_dispose = a.arg(0);
    if !vm.is_callable(on_dispose) {
        return vm.type_error("defer: onDispose is not callable");
    }
    add_method(vm, o, Value::Undefined, on_dispose, CallKind::Bare);
    Ok(Value::Undefined)
}

fn move_(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, a, is_async, "move")?;
    let proto = vm.intrinsic(if is_async {
        Intrinsic::AsyncDisposableStackPrototype
    } else {
        Intrinsic::DisposableStackPrototype
    });
    let resources = std::mem::take(&mut data(vm, o).resources);
    data(vm, o).disposed = true;
    let mut d = DisposeData::new(is_async);
    d.resources = resources;
    let n = vm.alloc(ObjectData::new(Some(proto), Class::DisposableStack(Box::new(d))));
    Ok(Value::Object(n))
}

fn disposed(vm: &mut Vm, a: &CallArgs, is_async: bool) -> JsResult<Value> {
    let o = this_stack(vm, a, is_async, "disposed")?;
    Ok(Value::Bool(data(vm, o).disposed))
}

fn ds_use(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    use_(vm, a, false)
}
fn ds_adopt(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    adopt(vm, a, false)
}
fn ds_defer(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    defer(vm, a, false)
}
fn ds_move(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    move_(vm, a, false)
}
fn ds_disposed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    disposed(vm, a, false)
}
fn ads_use(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    use_(vm, a, true)
}
fn ads_adopt(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    adopt(vm, a, true)
}
fn ads_defer(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    defer(vm, a, true)
}
fn ads_move(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    move_(vm, a, true)
}
fn ads_disposed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    disposed(vm, a, true)
}

/// DisposableStack.prototype.dispose (also its @@dispose).
fn ds_dispose(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_stack(vm, a, false, "dispose")?;
    if data(vm, o).disposed {
        return Ok(Value::Undefined);
    }
    data(vm, o).disposed = true;
    dispose_sync(vm, o)?;
    Ok(Value::Undefined)
}

// ---- disposeAsync: a native async closure -------------------------------------------------

const D_STACK: usize = 0;
const D_PROMISE: usize = 1;

/// AsyncDisposableStack.prototype.disposeAsync (also its @@asyncDispose).
fn ads_dispose_async(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cap = promise::new_internal_capability_intrinsic(vm);
    let Some(o) = brand(vm, a.this, true) else {
        let e = vm.make_error(
            ErrorType::TypeError,
            "AsyncDisposableStack.prototype.disposeAsync called on an incompatible receiver",
        );
        promise::reject_capability(vm, &cap, e)?;
        return Ok(Value::Object(cap.promise));
    };
    if data(vm, o).disposed {
        promise::resolve_capability(vm, &cap, Value::Undefined)?;
        return Ok(Value::Object(cap.promise));
    }
    data(vm, o).disposed = true;
    begin(vm, o, false, Value::Undefined);
    let driver = vm.new_native_closure(
        "",
        0,
        driver_never_called,
        vec![Value::Object(o), Value::Object(cap.promise)],
    );
    run_async(vm, driver)?;
    Ok(Value::Object(cap.promise))
}

fn driver_never_called(_vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Undefined)
}

fn driver_parts(vm: &Vm, driver: ObjId) -> (ObjId, promise::Capability) {
    let (Value::Object(stack), Value::Object(p)) = (vm.native_slot(driver, D_STACK), vm.native_slot(driver, D_PROMISE))
    else {
        unreachable!("the disposeAsync driver holds its stack and promise")
    };
    let realm = vm.heap.obj(driver).native().map(|n| n.realm).unwrap_or(vm.current_realm);
    (stack, promise::Capability { promise: p, realm, resolve: Value::Undefined, reject: Value::Undefined })
}

/// Step the disposal until it awaits or finishes, then settle the promise.
fn run_async(vm: &mut Vm, driver: ObjId) -> JsResult<()> {
    let (stack, cap) = driver_parts(vm, driver);
    match step(vm, stack) {
        Ok(Some(v)) => {
            let p = match promise::promise_resolve_intrinsic(vm, v) {
                Ok(p) => p,
                Err(Fault::Throw(e)) => {
                    record_error(vm, stack, e);
                    return run_async(vm, driver);
                }
                Err(f) => return Err(f),
            };
            let on_ok = vm.new_native_closure("", 1, resume_fulfilled, vec![Value::Object(driver)]);
            let on_err = vm.new_native_closure("", 1, resume_rejected, vec![Value::Object(driver)]);
            promise::perform_then(vm, p, Value::Object(on_ok), Value::Object(on_err), None)?;
            Ok(())
        }
        Ok(None) => match end(vm, stack) {
            Ok(()) => promise::resolve_capability(vm, &cap, Value::Undefined),
            Err(Fault::Throw(e)) => promise::reject_capability(vm, &cap, e),
            Err(f) => Err(f),
        },
        Err(Fault::Throw(e)) => {
            data(vm, stack).resources.clear();
            promise::reject_capability(vm, &cap, e)
        }
        Err(f) => Err(f),
    }
}

fn resume_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if let Value::Object(d) = vm.native_slot(a.callee, 0) {
        run_async(vm, d)?;
    }
    Ok(Value::Undefined)
}

fn resume_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if let Value::Object(d) = vm.native_slot(a.callee, 0) {
        let (stack, _) = driver_parts(vm, d);
        record_error(vm, stack, a.arg(0));
        run_async(vm, d)?;
    }
    Ok(Value::Undefined)
}

// ---- iterator prototypes ------------------------------------------------------------------

/// %IteratorPrototype%[@@dispose]().
fn iterator_dispose(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let ret = vm.get_method(a.this, PropKey::Str(vm.names.return_))?;
    if let Some(r) = ret {
        vm.call_value(r, a.this, &[])?;
    }
    Ok(Value::Undefined)
}

/// %AsyncIteratorPrototype%[@@asyncDispose]().
fn async_iterator_async_dispose(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cap = promise::new_internal_capability_intrinsic(vm);
    let r = (|| -> JsResult<Option<ObjId>> {
        let Some(ret) = vm.get_method(a.this, PropKey::Str(vm.names.return_))? else {
            return Ok(None);
        };
        let result = vm.call_value(ret, a.this, &[])?;
        Ok(Some(promise::promise_resolve_intrinsic(vm, result)?))
    })();
    match r {
        Ok(None) => promise::resolve_capability(vm, &cap, Value::Undefined)?,
        Ok(Some(p)) => {
            let on_ok = vm.new_native("", 1, return_undefined);
            promise::perform_then(vm, p, Value::Object(on_ok), Value::Undefined, Some(cap.clone()))?;
        }
        Err(Fault::Throw(e)) => promise::reject_capability(vm, &cap, e)?,
        Err(f) => return Err(f),
    }
    Ok(Value::Object(cap.promise))
}

fn return_undefined(_vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Undefined)
}
