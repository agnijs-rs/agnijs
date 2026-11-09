//! `Intl.Segmenter` (ECMA-402 §18), `%Segments%` and `%SegmentIterator%`.

use gable_js_intl::segmenter::{segment as seam_segment, Granularity, Segment};

use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{create_intl_object, intl_data, put_str, put_value, IntlData};

pub struct SegmenterRecord {
    pub locale: String,
    pub granularity: Granularity,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "Segmenter",
        0,
        constructor,
        Intrinsic::IntlSegmenter,
        Intrinsic::IntlSegmenterPrototype,
        supported_locales_of,
    );
    vm.method(proto, "segment", 1, segment);
    vm.method(proto, "resolvedOptions", 0, resolved_options);

    // %SegmentsPrototype%
    let segments_proto = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::IntlSegmentsPrototype, segments_proto);
    vm.method(segments_proto, "containing", 1, containing);
    let iter_sym = vm.well_known.iterator;
    vm.method_sym(segments_proto, iter_sym, "Symbol.iterator", 0, segments_iterator);

    // %SegmentIteratorPrototype%
    let iter_base = vm.intrinsic(Intrinsic::IteratorPrototype);
    let iter_proto = vm.new_object_with_proto(Some(iter_base));
    vm.realm_mut().set_intrinsic(Intrinsic::IntlSegmentIteratorPrototype, iter_proto);
    vm.method(iter_proto, "next", 0, iterator_next);
    super::super::to_string_tag(vm, iter_proto, "Segmenter String Iterator");
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.Segmenter must be called with new");
    }
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = get_options_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let r = resolve_locale(&requested, &[], &[], &|_, _| Vec::new(), vm);
    let granularity = get_string_option(vm, options, "granularity", &["grapheme", "word", "sentence"])?.unwrap_or_else(|| "grapheme".to_string());
    let granularity = match granularity.as_str() {
        "word" => Granularity::Word,
        "sentence" => Granularity::Sentence,
        _ => Granularity::Grapheme,
    };
    let record = SegmenterRecord { locale: r.locale, granularity };
    let obj = create_intl_object(vm, a, Intrinsic::IntlSegmenterPrototype, IntlData::Segmenter(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn this_segmenter(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::Segmenter(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.Segmenter.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &SegmenterRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::Segmenter(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn segment(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let seg = this_segmenter(vm, a.this, "segment")?;
    let string = vm.to_string(a.arg(0))?;
    let proto = vm.intrinsic(Intrinsic::IntlSegmentsPrototype);
    let data = IntlData::Segments { segmenter: seg, string };
    let obj = vm.alloc(ObjectData::new(Some(proto), Class::Intl(Box::new(data))));
    Ok(Value::Object(obj))
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let seg = this_segmenter(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, g) = {
        let r = record(vm, seg);
        (r.locale.clone(), r.granularity)
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "granularity", g.name())?;
    Ok(Value::Object(o))
}

fn this_segments(vm: &mut Vm, this: Value, method: &str) -> JsResult<(ObjId, StrId)> {
    match intl_data(vm, this) {
        Some(IntlData::Segments { segmenter, string }) => Ok((*segmenter, *string)),
        _ => vm.type_error(&format!("%Segments.prototype%.{method} called on an incompatible receiver")),
    }
}

/// CreateSegmentDataObject(segmenter, string, startIndex, endIndex)
fn segment_data_object(vm: &mut Vm, units: &[u16], string: StrId, s: &Segment) -> JsResult<Value> {
    let o = vm.new_object();
    let seg = vm.str_from_units(units[s.start..s.end].to_vec());
    put_value(vm, o, "segment", seg)?;
    put_value(vm, o, "index", Value::Number(s.start as f64))?;
    put_value(vm, o, "input", Value::String(string))?;
    if let Some(w) = s.is_word_like {
        put_value(vm, o, "isWordLike", Value::Bool(w))?;
    }
    Ok(Value::Object(o))
}

fn containing(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (seg, string) = this_segments(vm, a.this, "containing")?;
    let units = vm.heap.units(string).to_vec();
    let n = vm.to_integer_or_infinity(a.arg(0))?;
    if n < 0.0 || n >= units.len() as f64 {
        return Ok(Value::Undefined);
    }
    let n = n as usize;
    let g = record(vm, seg).granularity;
    let segments = seam_segment(&units, g);
    let Some(s) = segments.iter().find(|s| s.start <= n && n < s.end) else { return Ok(Value::Undefined) };
    segment_data_object(vm, &units, string, s)
}

fn segments_iterator(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (seg, string) = this_segments(vm, a.this, "[Symbol.iterator]")?;
    let proto = vm.intrinsic(Intrinsic::IntlSegmentIteratorPrototype);
    let data = IntlData::SegmentIterator { segmenter: seg, string, index: 0 };
    let obj = vm.alloc(ObjectData::new(Some(proto), Class::Intl(Box::new(data))));
    Ok(Value::Object(obj))
}

fn iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (seg, string, index) = match intl_data(vm, a.this) {
        Some(IntlData::SegmentIterator { segmenter, string, index }) => (*segmenter, *string, *index),
        _ => return vm.type_error("%SegmentIterator.prototype%.next called on an incompatible receiver"),
    };
    let units = vm.heap.units(string).to_vec();
    if index >= units.len() {
        return Ok(iter_result(vm, Value::Undefined, true));
    }
    let g = record(vm, seg).granularity;
    // FindBoundary from the current index: segment the remainder so the
    // boundary search never looks behind the iterator.
    let rest = seam_segment(&units[index..], g);
    let Some(first) = rest.first() else { return Ok(iter_result(vm, Value::Undefined, true)) };
    let s = Segment { start: index, end: index + first.end, is_word_like: first.is_word_like };
    let value = segment_data_object(vm, &units, string, &s)?;
    if let Some(o) = a.this.as_object() {
        if let Class::Intl(d) = &mut vm.heap.obj_mut(o).class {
            if let IntlData::SegmentIterator { index, .. } = &mut **d {
                *index = s.end;
            }
        }
    }
    Ok(iter_result(vm, value, false))
}

fn iter_result(vm: &mut Vm, value: Value, done: bool) -> Value {
    let o = vm.new_object();
    let vk = PropKey::Str(vm.names.value);
    vm.define_raw(o, vk, value, Attrs::ALL);
    let dk = PropKey::Str(vm.names.done);
    vm.define_raw(o, dk, Value::Bool(done), Attrs::ALL);
    Value::Object(o)
}
