//! Error, the NativeError constructors, AggregateError and %ThrowTypeError%.

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, Value};
use crate::vm::{ErrorType, JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let error_proto = vm.new_object_with_proto(Some(object_proto));
    let error_ctor = super::install_ctor(
        vm,
        realm,
        "Error",
        1,
        error_constructor,
        error_proto,
        Intrinsic::Error,
        Intrinsic::ErrorPrototype,
    );
    let name = vm.str("Error");
    vm.define_raw_str(error_proto, "name", name, Attrs::METHOD);
    let empty = Value::String(vm.names.empty);
    vm.define_raw_str(error_proto, "message", empty, Attrs::METHOD);
    vm.method(error_proto, "toString", 0, error_proto_to_string);
    vm.method(error_ctor, "isError", 1, error_is_error);
    crate::stack_trace::install(vm, error_ctor, error_proto);

    for ty in [
        ErrorType::TypeError,
        ErrorType::RangeError,
        ErrorType::ReferenceError,
        ErrorType::SyntaxError,
        ErrorType::EvalError,
        ErrorType::URIError,
        ErrorType::AggregateError,
        ErrorType::SuppressedError,
    ] {
        let proto = vm.new_object_with_proto(Some(error_proto));
        let (f, len): (crate::object::NativeFn, u32) = match ty {
            ErrorType::SuppressedError => (suppressed_error_constructor, 3),
            ErrorType::TypeError => (type_error_constructor, 1),
            ErrorType::RangeError => (range_error_constructor, 1),
            ErrorType::ReferenceError => (reference_error_constructor, 1),
            ErrorType::SyntaxError => (syntax_error_constructor, 1),
            ErrorType::EvalError => (eval_error_constructor, 1),
            ErrorType::URIError => (uri_error_constructor, 1),
            _ => (aggregate_error_constructor, 2),
        };
        let ctor = super::install_ctor(
            vm,
            realm,
            ty.name(),
            len,
            f,
            proto,
            ty.constructor(),
            ty.prototype(),
        );
        vm.heap.set_proto(ctor, Some(error_ctor));
        let name = vm.str(ty.name());
        vm.define_raw_str(proto, "name", name, Attrs::METHOD);
        vm.define_raw_str(proto, "message", empty, Attrs::METHOD);
    }

    // %ThrowTypeError%: frozen, anonymous, length 0.
    let thrower = vm.new_native("", 0, throw_type_error);
    let len_key = PropKey::Str(vm.names.length);
    let name_key = PropKey::Str(vm.names.name);
    vm.heap.replace_own_property(
        thrower,
        len_key,
        Property::data(Value::Number(0.0), Attrs::NONE),
    );
    vm.heap
        .replace_own_property(thrower, name_key, Property::data(empty, Attrs::NONE));
    vm.heap.obj_mut(thrower).extensible = false;
    vm.realm_mut()
        .set_intrinsic(Intrinsic::ThrowTypeError, thrower);
    super::function::install_poison_pills(vm);
}

/// Error.isError(arg) (ES2026): whether `arg` has an [[ErrorData]] slot.
/// A proxy has none, whatever its target.
fn error_is_error(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let is = match a.arg(0) {
        Value::Object(o) => matches!(vm.heap.obj(o).class, Class::Error(_)),
        _ => false,
    };
    Ok(Value::Bool(is))
}

fn throw_type_error(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    vm.type_error("'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them")
}

/// Shared body of the Error and NativeError constructors.
fn make_error_object(
    vm: &mut Vm,
    a: &CallArgs,
    ty: ErrorType,
    message: Value,
    options: Value,
) -> JsResult<ObjId> {
    let new_target = if a.new_target.is_undefined() {
        Value::Object(a.callee)
    } else {
        a.new_target
    };
    let o = vm.ordinary_create_from_constructor(new_target, ty.prototype(), Class::Error(None))?;
    vm.attach_error_stack(o);
    if !message.is_undefined() {
        let msg = vm.to_string(message)?;
        let key = PropKey::Str(vm.names.message);
        vm.define_raw(o, key, Value::String(msg), Attrs::METHOD);
    }
    install_error_cause(vm, o, options)?;
    Ok(o)
}

/// InstallErrorCause(O, options).
pub fn install_error_cause(vm: &mut Vm, o: ObjId, options: Value) -> JsResult<()> {
    if let Value::Object(opts) = options {
        let key = PropKey::Str(vm.names.cause);
        if vm.has_property(opts, key)? {
            let cause = vm.get(opts, key)?;
            vm.define_raw(o, key, cause, Attrs::METHOD);
        }
    }
    Ok(())
}

fn error_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Object(make_error_object(
        vm,
        a,
        ErrorType::Error,
        a.arg(0),
        a.arg(1),
    )?))
}

macro_rules! native_error {
    ($fn_name:ident, $ty:expr) => {
        fn $fn_name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
            Ok(Value::Object(make_error_object(
                vm,
                a,
                $ty,
                a.arg(0),
                a.arg(1),
            )?))
        }
    };
}

native_error!(type_error_constructor, ErrorType::TypeError);
native_error!(range_error_constructor, ErrorType::RangeError);
native_error!(reference_error_constructor, ErrorType::ReferenceError);
native_error!(syntax_error_constructor, ErrorType::SyntaxError);
native_error!(eval_error_constructor, ErrorType::EvalError);
native_error!(uri_error_constructor, ErrorType::URIError);

fn aggregate_error_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = make_error_object(vm, a, ErrorType::AggregateError, a.arg(1), a.arg(2))?;
    let mark = vm.root_mark();
    vm.root(Value::Object(o));
    let list = vm.iterable_to_list(a.arg(0));
    vm.pop_roots(mark);
    let list = list?;
    let arr = vm.new_array(list);
    let key = PropKey::Str(vm.names.errors);
    vm.define_raw(o, key, Value::Object(arr), Attrs::METHOD);
    Ok(Value::Object(o))
}

/// SuppressedError(error, suppressed, message).
fn suppressed_error_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = make_error_object(vm, a, ErrorType::SuppressedError, a.arg(2), Value::Undefined)?;
    set_suppressed(vm, o, a.arg(0), a.arg(1));
    Ok(Value::Object(o))
}

fn set_suppressed(vm: &mut Vm, o: ObjId, error: Value, suppressed: Value) {
    let key = PropKey::Str(vm.atom("error"));
    vm.define_raw(o, key, error, Attrs::METHOD);
    let key = PropKey::Str(vm.atom("suppressed"));
    vm.define_raw(o, key, suppressed, Attrs::METHOD);
}

/// A new SuppressedError of the current realm with no message, as
/// DisposeResources creates when a disposal throws over an earlier error.
pub fn new_suppressed_error(vm: &mut Vm, error: Value, suppressed: Value) -> Value {
    let proto = vm.intrinsic(Intrinsic::SuppressedErrorPrototype);
    let o = vm.alloc(crate::object::ObjectData::new(
        Some(proto),
        Class::Error(None),
    ));
    vm.attach_error_stack(o);
    set_suppressed(vm, o, error, suppressed);
    Value::Object(o)
}

/// Error.prototype.toString.
pub fn error_proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("Error.prototype.toString called on non-object");
    };
    let name_key = PropKey::Str(vm.names.name);
    let name = vm.get(o, name_key)?;
    let name = if name.is_undefined() {
        vm.atom("Error")
    } else {
        vm.to_string(name)?
    };
    let msg_key = PropKey::Str(vm.names.message);
    let msg = vm.get(o, msg_key)?;
    let msg = if msg.is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(msg)?
    };
    if vm.heap.str_len(name) == 0 {
        return Ok(Value::String(msg));
    }
    if vm.heap.str_len(msg) == 0 {
        return Ok(Value::String(name));
    }
    let sep = vm.heap.alloc_str(": ");
    let s = vm.heap.concat(name, sep);
    let s = vm.heap.concat(s, msg);
    Ok(Value::String(s))
}
