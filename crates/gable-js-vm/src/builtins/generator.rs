//! GeneratorFunction, AsyncGeneratorFunction, AsyncFunction and the
//! Generator / AsyncGenerator prototypes.

use crate::interp::generator::ResumeMode;
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};
use gable_engine_api::RealmId;
use gable_js_compiler::DynamicFunctionKind;

pub fn install(vm: &mut Vm, _realm: RealmId) {
    let function_proto = vm.intrinsic(Intrinsic::FunctionPrototype);
    let function_ctor = vm.intrinsic(Intrinsic::Function);
    let iterator_proto = vm.intrinsic(Intrinsic::IteratorPrototype);
    let async_iterator_proto = vm.intrinsic(Intrinsic::AsyncIteratorPrototype);

    // %GeneratorFunction% and %GeneratorFunction.prototype%.
    let gen_proto = vm.new_object_with_proto(Some(iterator_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::GeneratorPrototype, gen_proto);
    vm.method(gen_proto, "next", 1, generator_next);
    vm.method(gen_proto, "return", 1, generator_return);
    vm.method(gen_proto, "throw", 1, generator_throw);
    super::to_string_tag(vm, gen_proto, "Generator");
    install_function_kind(
        vm,
        "GeneratorFunction",
        generator_function_constructor,
        function_ctor,
        function_proto,
        Some(gen_proto),
        Intrinsic::GeneratorFunction,
        Intrinsic::GeneratorFunctionPrototype,
    );

    // %AsyncGeneratorFunction% and friends.
    let agen_proto = vm.new_object_with_proto(Some(async_iterator_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::AsyncGeneratorPrototype, agen_proto);
    vm.method(agen_proto, "next", 1, async_generator_next);
    vm.method(agen_proto, "return", 1, async_generator_return);
    vm.method(agen_proto, "throw", 1, async_generator_throw);
    super::to_string_tag(vm, agen_proto, "AsyncGenerator");
    install_function_kind(
        vm,
        "AsyncGeneratorFunction",
        async_generator_function_constructor,
        function_ctor,
        function_proto,
        Some(agen_proto),
        Intrinsic::AsyncGeneratorFunction,
        Intrinsic::AsyncGeneratorFunctionPrototype,
    );

    // %AsyncFunction%.
    install_function_kind(
        vm,
        "AsyncFunction",
        async_function_constructor,
        function_ctor,
        function_proto,
        None,
        Intrinsic::AsyncFunction,
        Intrinsic::AsyncFunctionPrototype,
    );
}

#[allow(clippy::too_many_arguments)]
fn install_function_kind(
    vm: &mut Vm,
    name: &str,
    f: crate::object::NativeFn,
    function_ctor: ObjId,
    function_proto: ObjId,
    instance_proto: Option<ObjId>,
    ctor_intrinsic: Intrinsic,
    proto_intrinsic: Intrinsic,
) {
    let ctor = vm.new_native_ctor(name, 1, f);
    vm.heap.obj_mut(ctor).proto = Some(function_ctor);
    let proto = vm.new_object_with_proto(Some(function_proto));
    vm.realm_mut().set_intrinsic(ctor_intrinsic, ctor);
    vm.realm_mut().set_intrinsic(proto_intrinsic, proto);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), Attrs::NONE);
    vm.define_raw_str(
        proto,
        "constructor",
        Value::Object(ctor),
        Attrs::CONFIGURABLE_ONLY,
    );
    if let Some(ip) = instance_proto {
        vm.define_raw_str(
            proto,
            "prototype",
            Value::Object(ip),
            Attrs::CONFIGURABLE_ONLY,
        );
        vm.define_raw_str(
            ip,
            "constructor",
            Value::Object(proto),
            Attrs::CONFIGURABLE_ONLY,
        );
    }
    super::to_string_tag(vm, proto, name);
}

fn generator_function_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    super::function::create_dynamic_function(vm, a, DynamicFunctionKind::Generator)
}

fn async_generator_function_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    super::function::create_dynamic_function(vm, a, DynamicFunctionKind::AsyncGenerator)
}

fn async_function_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    super::function::create_dynamic_function(vm, a, DynamicFunctionKind::Async)
}

fn generator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.generator_resume(a.this, ResumeMode::Next, a.arg(0), "next")
}

fn generator_return(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.generator_resume(a.this, ResumeMode::Return, a.arg(0), "return")
}

fn generator_throw(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.generator_resume(a.this, ResumeMode::Throw, a.arg(0), "throw")
}

fn async_generator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.async_generator_enqueue(a.this, ResumeMode::Next, a.arg(0))
}

fn async_generator_return(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.async_generator_enqueue(a.this, ResumeMode::Return, a.arg(0))
}

fn async_generator_throw(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.async_generator_enqueue(a.this, ResumeMode::Throw, a.arg(0))
}
