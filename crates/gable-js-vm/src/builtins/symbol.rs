//! Symbol constructor and Symbol.prototype.

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::symbol::{SymbolData, SymbolKind};
use crate::value::{Attrs, PropKey, SymId, Value};
use crate::vm::{JsResult, Vm};
use gable_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let proto = vm.new_object_with_proto(Some(object_proto));
    let ctor = super::install_ctor(
        vm,
        realm,
        "Symbol",
        0,
        symbol_constructor,
        proto,
        Intrinsic::Symbol,
        Intrinsic::SymbolPrototype,
    );

    vm.method(ctor, "for", 1, symbol_for);
    vm.method(ctor, "keyFor", 1, symbol_key_for);
    let wk = [
        ("asyncIterator", vm.well_known.async_iterator),
        ("hasInstance", vm.well_known.has_instance),
        ("isConcatSpreadable", vm.well_known.is_concat_spreadable),
        ("iterator", vm.well_known.iterator),
        ("match", vm.well_known.match_),
        ("matchAll", vm.well_known.match_all),
        ("replace", vm.well_known.replace),
        ("search", vm.well_known.search),
        ("species", vm.well_known.species),
        ("split", vm.well_known.split),
        ("toPrimitive", vm.well_known.to_primitive),
        ("toStringTag", vm.well_known.to_string_tag),
        ("unscopables", vm.well_known.unscopables),
    ];
    for (name, sym) in wk {
        vm.constant(ctor, name, Value::Symbol(sym));
    }

    vm.accessor(proto, "description", proto_description, None);
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "valueOf", 0, proto_value_of);
    let tp = vm.well_known.to_primitive;
    let f = vm.new_native("[Symbol.toPrimitive]", 1, proto_to_primitive);
    vm.define_raw(
        proto,
        PropKey::Sym(tp),
        Value::Object(f),
        Attrs::CONFIGURABLE_ONLY,
    );
    super::to_string_tag(vm, proto, "Symbol");
}

fn symbol_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.is_construct() {
        return vm.type_error("Symbol is not a constructor");
    }
    let desc = a.arg(0);
    let description = if desc.is_undefined() {
        None
    } else {
        Some(vm.to_string(desc)?)
    };
    let sym = vm
        .heap
        .alloc_symbol(SymbolData::new(description, SymbolKind::Normal));
    Ok(Value::Symbol(sym))
}

fn symbol_for(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = vm.to_string(a.arg(0))?;
    let key = vm.heap.intern(key);
    if let Some(s) = vm.symbol_registry.get(&key) {
        return Ok(Value::Symbol(*s));
    }
    let sym = vm
        .heap
        .alloc_symbol(SymbolData::new(Some(key), SymbolKind::Registered));
    vm.symbol_registry.insert(key, sym);
    Ok(Value::Symbol(sym))
}

fn symbol_key_for(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Symbol(s) = a.arg(0) else {
        return vm.type_error("Symbol.keyFor requires a symbol");
    };
    let data = vm.heap.sym(s);
    if data.kind == SymbolKind::Registered {
        return Ok(data
            .description
            .map(Value::String)
            .unwrap_or(Value::Undefined));
    }
    Ok(Value::Undefined)
}

/// thisSymbolValue(value).
fn this_symbol_value(vm: &mut Vm, v: Value) -> JsResult<SymId> {
    match v {
        Value::Symbol(s) => Ok(s),
        Value::Object(o) => match vm.heap.obj(o).class {
            Class::Symbol(s) => Ok(s),
            _ => vm.type_error("Symbol.prototype method called on incompatible receiver"),
        },
        _ => vm.type_error("Symbol.prototype method called on incompatible receiver"),
    }
}

/// SymbolDescriptiveString(sym).
pub fn symbol_descriptive_string(vm: &mut Vm, s: SymId) -> Value {
    let desc = match vm.heap.sym(s).description {
        Some(d) => vm.heap.str_to_string(d),
        None => String::new(),
    };
    vm.str(&format!("Symbol({desc})"))
}

fn proto_description(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_symbol_value(vm, a.this)?;
    Ok(vm
        .heap
        .sym(s)
        .description
        .map(Value::String)
        .unwrap_or(Value::Undefined))
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_symbol_value(vm, a.this)?;
    Ok(symbol_descriptive_string(vm, s))
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Symbol(this_symbol_value(vm, a.this)?))
}

fn proto_to_primitive(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Symbol(this_symbol_value(vm, a.this)?))
}
