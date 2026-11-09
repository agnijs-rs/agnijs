//! Function constructor and Function.prototype.

use crate::object::{CallArgs, Class, NativeTag};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;
use agnijs_js_compiler::DynamicFunctionKind;

/// `Function.prototype` itself: callable, accepts anything, returns undefined.
pub fn function_prototype_call(_vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Undefined)
}

pub fn install(vm: &mut Vm, realm: RealmId) {
    let proto = vm.intrinsic(Intrinsic::FunctionPrototype);
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    vm.heap.set_proto(proto, Some(object_proto));
    let ctor = super::install_ctor(
        vm,
        realm,
        "Function",
        1,
        function_constructor,
        proto,
        Intrinsic::Function,
        Intrinsic::FunctionPrototype,
    );
    let _ = ctor;

    let apply = vm.method(proto, "apply", 2, proto_apply);
    vm.method(proto, "bind", 1, proto_bind);
    let call = vm.method(proto, "call", 1, proto_call);
    // The interpreter pushes a bytecode target's frame itself for these two
    // (TASK-100.19); the natives below remain the generic path.
    for (f, tag) in [(apply, NativeTag::FunctionApply), (call, NativeTag::FunctionCall)] {
        if let Some(n) = vm.heap.obj_mut(f).native_mut() {
            n.tag = tag;
        }
    }
    vm.method(proto, "toString", 0, proto_to_string);
    let hi = vm.well_known.has_instance;
    let f = vm.new_native("[Symbol.hasInstance]", 1, proto_has_instance);
    vm.define_raw(proto, PropKey::Sym(hi), Value::Object(f), Attrs::NONE);

    // Annex B / web-compat poison pills: `caller` and `arguments` accessors
    // throwing %ThrowTypeError% are installed by error.rs once that
    // intrinsic exists.
}

fn function_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_dynamic_function(vm, a, DynamicFunctionKind::Normal)
}

/// CreateDynamicFunction(constructor, newTarget, kind, parameterArgs, bodyArg).
pub fn create_dynamic_function(
    vm: &mut Vm,
    a: &CallArgs,
    kind: DynamicFunctionKind,
) -> JsResult<Value> {
    let realm = vm.current_realm;
    let host = vm.host.clone();
    if !host.can_compile_strings(realm) {
        return Err(vm.throw(
            crate::vm::ErrorType::EvalError,
            "Code generation from strings disallowed for this context",
        ));
    }
    let new_target = if a.new_target.is_undefined() {
        Value::Object(a.callee)
    } else {
        a.new_target
    };
    let n = a.len();
    // Code units, so lone surrogates reach the lexer (TASK-100.15).
    let mut params: Vec<u16> = Vec::new();
    let mut body: Vec<u16> = Vec::new();
    if n > 0 {
        for (i, p) in a.args[..n - 1].iter().enumerate() {
            if i > 0 {
                params.push(u16::from(b','));
            }
            let s = vm.to_string(*p)?;
            params.extend(vm.heap.units(s).iter());
        }
        let s = vm.to_string(a.args[n - 1])?;
        body.extend(vm.heap.units(s).iter());
    }
    let code = match agnijs_js_compiler::compile_dynamic_function_utf16(&params, &body, kind) {
        Ok(r) => r,
        Err(e) => return vm.syntax_error(&e.message),
    };
    vm.debug_script_compiled(&code, agnijs_js_debugger::ScriptKind::Function);
    let fallback = match kind {
        DynamicFunctionKind::Normal => Intrinsic::FunctionPrototype,
        DynamicFunctionKind::Generator => Intrinsic::GeneratorFunctionPrototype,
        DynamicFunctionKind::Async => Intrinsic::AsyncFunctionPrototype,
        DynamicFunctionKind::AsyncGenerator => Intrinsic::AsyncGeneratorFunctionPrototype,
    };
    let proto = vm.get_prototype_from_constructor(new_target, fallback)?;
    let global_env = vm.realm().global_env;
    let f = vm.create_function(code, Some(global_env), None);
    vm.heap.set_proto(f, Some(proto));
    Ok(Value::Object(f))
}

fn proto_apply(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let func = a.this;
    if !vm.is_callable(func) {
        return vm.type_error("Function.prototype.apply was called on a non-function");
    }
    let arg_array = a.arg(1);
    if arg_array.is_nullish() {
        return vm.call_value(func, a.arg(0), &[]);
    }
    let args = vm.create_list_from_array_like(arg_array)?;
    vm.call_value(func, a.arg(0), &args)
}

fn proto_call(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let func = a.this;
    if !vm.is_callable(func) {
        return vm.type_error("Function.prototype.call was called on a non-function");
    }
    let rest = if a.len() > 1 { &a.args[1..] } else { &[] };
    vm.call_value(func, a.arg(0), rest)
}

fn proto_bind(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(target) = a.this else {
        return vm.type_error("Bind must be called on a function");
    };
    if !vm.is_callable(a.this) {
        return vm.type_error("Bind must be called on a function");
    }
    let bound_args: Vec<Value> = if a.len() > 1 {
        a.args[1..].to_vec()
    } else {
        Vec::new()
    };
    let arg_count = bound_args.len() as f64;
    let f = vm.bound_function_create(target, a.arg(0), bound_args)?;
    let mut length = 0.0;
    let len_key = PropKey::Str(vm.names.length);
    if vm.has_own_property(target, len_key)? {
        let l = vm.get(target, len_key)?;
        if let Value::Number(n) = l {
            if n == f64::INFINITY {
                length = f64::INFINITY;
            } else if n == f64::NEG_INFINITY {
                length = 0.0;
            } else {
                let li = crate::convert::to_integer_or_infinity_f64(n);
                length = (li - arg_count).max(0.0);
            }
        }
    }
    vm.define_raw(f, len_key, Value::Number(length), Attrs::CONFIGURABLE_ONLY);
    let name_key = PropKey::Str(vm.names.name);
    let name = vm.get(target, name_key)?;
    let name = if name.is_string() {
        name
    } else {
        Value::String(vm.names.empty)
    };
    vm.set_function_name(f, name, Some("bound"));
    Ok(Value::Object(f))
}

/// Function.prototype.toString.
pub fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("Function.prototype.toString requires that 'this' be a Function");
    };
    let text: String = match &vm.heap.obj(o).class {
        Class::Function(fd) => {
            let span = fd.code.span;
            let source = &fd.code.source;
            if !source.lone_surrogates.is_empty() {
                // Source from a JavaScript string: put its lone surrogates
                // back (TASK-100.15).
                let units = source.units(span.start as usize, span.end as usize);
                return Ok(vm.str_from_units(units));
            }
            let text = &source.text;
            let start = (span.start as usize).min(text.len());
            let end = (span.end as usize).clamp(start, text.len());
            text[start..end].to_string()
        }
        Class::Native(_) | Class::Bound(_) => {
            let name = native_name(vm, o);
            format!("function {name}() {{ [native code] }}")
        }
        Class::Proxy(p) if p.callable => "function () { [native code] }".into(),
        _ => {
            return vm.type_error("Function.prototype.toString requires that 'this' be a Function")
        }
    };
    Ok(vm.str(&text))
}

fn native_name(vm: &mut Vm, o: ObjId) -> String {
    let key = PropKey::Str(vm.names.name);
    match vm.heap.get_own_property(o, key).and_then(|p| p.value()) {
        Some(Value::String(s)) => vm.heap.str_to_string(s),
        _ => String::new(),
    }
}

fn proto_has_instance(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(vm.ordinary_has_instance(a.this, a.arg(0))?))
}

/// Define the `caller`/`arguments` poison-pill accessors on %Function.prototype%.
pub fn install_poison_pills(vm: &mut Vm) {
    let proto = vm.intrinsic(Intrinsic::FunctionPrototype);
    let thrower = Value::Object(vm.intrinsic(Intrinsic::ThrowTypeError));
    for name in ["caller", "arguments"] {
        let key = vm.key(name);
        vm.heap.add_own_property(
            proto,
            key,
            Property::accessor(thrower, thrower, Attrs::CONFIGURABLE_ONLY),
        );
    }
}
