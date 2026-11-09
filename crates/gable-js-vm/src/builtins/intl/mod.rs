//! ECMA-402 (the `Intl` object and its service constructors) on the
//! `gable-js-intl` seam. This module owns the JavaScript-facing parts:
//! option resolution, object shapes, the abstract operations of ECMA-402
//! §9, and the `toLocale*` methods of the language built-ins. Everything
//! locale-sensitive is a call into the seam crate.

pub mod abstract_ops;
pub mod collator;
pub mod datetimeformat;
pub mod displaynames;
pub mod listformat;
pub mod locale;
pub mod numberformat;
pub mod pluralrules;
pub mod relativetimeformat;
pub mod segmenter;

use gable_engine_api::RealmId;

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};

pub use abstract_ops::*;

/// The internal slots of an `Intl` service object.
pub enum IntlData {
    Locale(Box<locale::LocaleRecord>),
    NumberFormat(Box<numberformat::NumberFormatRecord>),
    PluralRules(Box<pluralrules::PluralRulesRecord>),
    DateTimeFormat(Box<datetimeformat::DateTimeFormatRecord>),
    Collator(Box<collator::CollatorRecord>),
    ListFormat(Box<listformat::ListFormatRecord>),
    RelativeTimeFormat(Box<relativetimeformat::RelativeTimeFormatRecord>),
    Segmenter(Box<segmenter::SegmenterRecord>),
    Segments { segmenter: ObjId, string: StrId },
    SegmentIterator { segmenter: ObjId, string: StrId, index: usize },
    DisplayNames(Box<displaynames::DisplayNamesRecord>),
}

impl IntlData {
    pub fn trace(&self, t: &mut Tracer) {
        match self {
            IntlData::NumberFormat(r) => t.opt_obj(r.bound_format),
            IntlData::DateTimeFormat(r) => t.opt_obj(r.bound_format),
            IntlData::Collator(r) => t.opt_obj(r.bound_compare),
            IntlData::Segments { segmenter, string } | IntlData::SegmentIterator { segmenter, string, .. } => {
                t.obj(*segmenter);
                t.str(*string);
            }
            _ => {}
        }
    }
}

/// The `IntlData` of `v` when it is an Intl object.
pub fn intl_data(vm: &Vm, v: Value) -> Option<&IntlData> {
    let o = v.as_object()?;
    match &vm.heap.obj(o).class {
        Class::Intl(d) => Some(d),
        _ => None,
    }
}

/// OrdinaryCreateFromConstructor for an Intl service object.
pub fn create_intl_object(vm: &mut Vm, a: &CallArgs, proto: Intrinsic, data: IntlData) -> JsResult<ObjId> {
    let proto = vm.get_prototype_from_constructor(a.new_target, proto)?;
    Ok(vm.alloc(ObjectData::new(Some(proto), Class::Intl(Box::new(data)))))
}

pub fn install(vm: &mut Vm, realm: RealmId) {
    let intl = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Intl, intl);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Intl", Value::Object(intl), Attrs::METHOD);
    super::to_string_tag(vm, intl, "Intl");
    vm.method(intl, "getCanonicalLocales", 1, get_canonical_locales);
    vm.method(intl, "supportedValuesOf", 1, supported_values_of);

    locale::install(vm, intl);
    numberformat::install(vm, intl);
    pluralrules::install(vm, intl);
    datetimeformat::install(vm, intl);
    collator::install(vm, intl);
    listformat::install(vm, intl);
    relativetimeformat::install(vm, intl);
    segmenter::install(vm, intl);
    displaynames::install(vm, intl);
}

/// Create a service constructor on `Intl` with its prototype wired both
/// ways. Returns `(ctor, proto)`.
pub fn install_service(
    vm: &mut Vm,
    intl: ObjId,
    name: &str,
    length: u32,
    f: crate::object::NativeFn,
    ctor_intrinsic: Intrinsic,
    proto_intrinsic: Intrinsic,
    supported_locales_of: crate::object::NativeFn,
) -> (ObjId, ObjId) {
    let proto = vm.new_object();
    let ctor = vm.new_native_ctor(name, length, f);
    vm.realm_mut().set_intrinsic(ctor_intrinsic, ctor);
    vm.realm_mut().set_intrinsic(proto_intrinsic, proto);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), Attrs::NONE);
    vm.define_raw_str(proto, "constructor", Value::Object(ctor), Attrs::METHOD);
    vm.define_raw_str(intl, name, Value::Object(ctor), Attrs::METHOD);
    vm.method(ctor, "supportedLocalesOf", 1, supported_locales_of);
    super::to_string_tag(vm, proto, &format!("Intl.{name}"));
    (ctor, proto)
}

fn get_canonical_locales(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let list = canonicalize_locale_list(vm, a.arg(0))?;
    Ok(string_list_to_array(vm, &list))
}

/// Intl.supportedValuesOf(key)
fn supported_values_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = vm.to_rust_string(a.arg(0))?;
    let list: Vec<String> = match key.as_str() {
        "calendar" => gable_js_intl::data::CALENDARS.iter().map(|s| s.to_string()).collect(),
        "collation" => gable_js_intl::data::COLLATIONS.iter().map(|s| s.to_string()).collect(),
        "currency" => gable_js_intl::data::currencies().to_vec(),
        "numberingSystem" => gable_js_intl::data::numbering_systems(),
        "timeZone" => gable_js_intl::timezone::available().to_vec(),
        "unit" => gable_js_intl::data::units(),
        _ => return vm.range_error(&format!("Invalid key : {key}")),
    };
    Ok(string_list_to_array(vm, &list))
}

/// CreateArrayFromList over Rust strings.
pub fn string_list_to_array(vm: &mut Vm, list: &[String]) -> Value {
    let items: Vec<Value> = list.iter().map(|s| vm.str(s)).collect();
    Value::Object(vm.new_array(items))
}

/// CreateDataPropertyOrThrow(obj, name, string).
pub fn put_str(vm: &mut Vm, obj: ObjId, name: &str, s: &str) -> JsResult<()> {
    let v = vm.str(s);
    let k = vm.key(name);
    vm.create_data_property_or_throw(obj, k, v)
}

pub fn put_value(vm: &mut Vm, obj: ObjId, name: &str, v: Value) -> JsResult<()> {
    let k = vm.key(name);
    vm.create_data_property_or_throw(obj, k, v)
}

/// A property key from a Rust string (interned).
pub fn key(vm: &mut Vm, name: &str) -> PropKey {
    vm.key(name)
}
