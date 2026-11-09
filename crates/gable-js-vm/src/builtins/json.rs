//! JSON.parse and JSON.stringify.

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::string::StrBuilder;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};
use gable_engine_api::RealmId;
use gable_js_parser::number::to_js_string;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let json = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::JSON, json);
    vm.method(json, "parse", 2, json_parse);
    vm.method(json, "stringify", 3, json_stringify);
    super::to_string_tag(vm, json, "JSON");
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "JSON", Value::Object(json), Attrs::METHOD);
}

// ---- parse ---------------------------------------------------------------------------

/// Parse JSON text given as a Rust string (JSON modules).
pub fn parse_json_text(vm: &mut Vm, text: &str) -> JsResult<Value> {
    let units: Vec<u16> = text.encode_utf16().collect();
    parse_units(vm, &units)
}

fn parse_units(vm: &mut Vm, units: &[u16]) -> JsResult<Value> {
    let mut p = Parser {
        units,
        pos: 0,
        depth: 0,
    };
    p.skip_ws();
    let v = p.parse_value(vm)?;
    p.skip_ws();
    if p.pos != units.len() {
        return p.fail(vm, "Unexpected token after JSON value");
    }
    Ok(v)
}

struct Parser<'a> {
    units: &'a [u16],
    pos: usize,
    depth: u32,
}

const MAX_JSON_DEPTH: u32 = 5000;

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u16> {
        self.units.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == 0x20 || c == 0x09 || c == 0x0A || c == 0x0D {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn fail<T>(&self, vm: &mut Vm, msg: &str) -> JsResult<T> {
        vm.syntax_error(&format!("JSON.parse: {msg} at position {}", self.pos))
    }

    fn expect_literal(&mut self, vm: &mut Vm, lit: &str) -> JsResult<()> {
        for b in lit.bytes() {
            if self.peek() != Some(b as u16) {
                return self.fail(vm, "Unexpected token");
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn parse_value(&mut self, vm: &mut Vm) -> JsResult<Value> {
        match self.peek() {
            None => self.fail(vm, "Unexpected end of JSON input"),
            Some(0x7B) => self.parse_object(vm),
            Some(0x5B) => self.parse_array(vm),
            Some(0x22) => {
                let s = self.parse_string(vm)?;
                Ok(Value::String(s))
            }
            Some(0x74) => {
                self.expect_literal(vm, "true")?;
                Ok(Value::Bool(true))
            }
            Some(0x66) => {
                self.expect_literal(vm, "false")?;
                Ok(Value::Bool(false))
            }
            Some(0x6E) => {
                self.expect_literal(vm, "null")?;
                Ok(Value::Null)
            }
            Some(c) if c == 0x2D || (0x30..=0x39).contains(&c) => self.parse_number(vm),
            Some(_) => self.fail(vm, "Unexpected token"),
        }
    }

    fn parse_number(&mut self, vm: &mut Vm) -> JsResult<Value> {
        let start = self.pos;
        if self.peek() == Some(0x2D) {
            self.pos += 1;
        }
        match self.peek() {
            Some(0x30) => {
                self.pos += 1;
            }
            Some(c) if (0x31..=0x39).contains(&c) => {
                while matches!(self.peek(), Some(c) if (0x30..=0x39).contains(&c)) {
                    self.pos += 1;
                }
            }
            _ => return self.fail(vm, "No number after minus sign"),
        }
        if self.peek() == Some(0x2E) {
            self.pos += 1;
            let ds = self.pos;
            while matches!(self.peek(), Some(c) if (0x30..=0x39).contains(&c)) {
                self.pos += 1;
            }
            if self.pos == ds {
                return self.fail(vm, "Unterminated fractional number");
            }
        }
        if matches!(self.peek(), Some(0x65) | Some(0x45)) {
            self.pos += 1;
            if matches!(self.peek(), Some(0x2B) | Some(0x2D)) {
                self.pos += 1;
            }
            let ds = self.pos;
            while matches!(self.peek(), Some(c) if (0x30..=0x39).contains(&c)) {
                self.pos += 1;
            }
            if self.pos == ds {
                return self.fail(vm, "Exponent part is missing a number");
            }
        }
        let text: String = self.units[start..self.pos]
            .iter()
            .map(|&u| u as u8 as char)
            .collect();
        let n: f64 = text.parse().unwrap_or(f64::NAN);
        Ok(Value::Number(n))
    }

    fn parse_string(&mut self, vm: &mut Vm) -> JsResult<StrId> {
        // Opening quote.
        self.pos += 1;
        let mut b = StrBuilder::new();
        loop {
            let Some(c) = self.peek() else {
                return self.fail(vm, "Unterminated string");
            };
            self.pos += 1;
            match c {
                0x22 => break,
                0x5C => {
                    let Some(e) = self.peek() else {
                        return self.fail(vm, "Unterminated string");
                    };
                    self.pos += 1;
                    match e {
                        0x22 => b.push(0x22),
                        0x5C => b.push(0x5C),
                        0x2F => b.push(0x2F),
                        0x62 => b.push(0x08),
                        0x66 => b.push(0x0C),
                        0x6E => b.push(0x0A),
                        0x72 => b.push(0x0D),
                        0x74 => b.push(0x09),
                        0x75 => {
                            let mut v: u32 = 0;
                            for _ in 0..4 {
                                let Some(h) = self.peek() else {
                                    return self.fail(vm, "Bad Unicode escape");
                                };
                                let d = match h {
                                    0x30..=0x39 => h - 0x30,
                                    0x61..=0x66 => h - 0x61 + 10,
                                    0x41..=0x46 => h - 0x41 + 10,
                                    _ => return self.fail(vm, "Bad Unicode escape"),
                                };
                                v = (v << 4) | d as u32;
                                self.pos += 1;
                            }
                            b.push(v as u16);
                        }
                        _ => return self.fail(vm, "Bad escaped character"),
                    }
                }
                c if c < 0x20 => return self.fail(vm, "Bad control character in string literal"),
                c => b.push(c),
            }
        }
        Ok(vm.heap.alloc_str_utf16(b.finish()))
    }

    fn parse_array(&mut self, vm: &mut Vm) -> JsResult<Value> {
        self.pos += 1;
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(vm.throw_range("Maximum call stack size exceeded"));
        }
        let mark = vm.root_mark();
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(0x5D) {
            self.pos += 1;
        } else {
            loop {
                self.skip_ws();
                let v = self.parse_value(vm)?;
                vm.root(v);
                items.push(v);
                self.skip_ws();
                match self.peek() {
                    Some(0x2C) => {
                        self.pos += 1;
                    }
                    Some(0x5D) => {
                        self.pos += 1;
                        break;
                    }
                    _ => return self.fail(vm, "Expected ',' or ']' after array element"),
                }
            }
        }
        vm.pop_roots(mark);
        self.depth -= 1;
        Ok(Value::Object(vm.new_array(items)))
    }

    fn parse_object(&mut self, vm: &mut Vm) -> JsResult<Value> {
        self.pos += 1;
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(vm.throw_range("Maximum call stack size exceeded"));
        }
        let obj = vm.new_object();
        let mark = vm.root_mark();
        vm.root(Value::Object(obj));
        self.skip_ws();
        if self.peek() == Some(0x7D) {
            self.pos += 1;
        } else {
            loop {
                self.skip_ws();
                if self.peek() != Some(0x22) {
                    return self.fail(vm, "Expected property name");
                }
                let key = self.parse_string(vm)?;
                let key = vm.key_from_str(key);
                self.skip_ws();
                if self.peek() != Some(0x3A) {
                    return self.fail(vm, "Expected ':' after property name");
                }
                self.pos += 1;
                self.skip_ws();
                let v = self.parse_value(vm)?;
                vm.create_data_property(obj, key, v)?;
                self.skip_ws();
                match self.peek() {
                    Some(0x2C) => {
                        self.pos += 1;
                    }
                    Some(0x7D) => {
                        self.pos += 1;
                        break;
                    }
                    _ => return self.fail(vm, "Expected ',' or '}' after property value"),
                }
            }
        }
        vm.pop_roots(mark);
        self.depth -= 1;
        Ok(Value::Object(obj))
    }
}

fn json_parse(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let text = vm.to_string(a.arg(0))?;
    let units = vm.heap.units(text).to_vec();
    let unfiltered = parse_units(vm, &units)?;
    let reviver = a.arg(1);
    if vm.is_callable(reviver) {
        let root = vm.new_object();
        let key = PropKey::Str(vm.names.empty);
        vm.create_data_property_or_throw(root, key, unfiltered)?;
        return internalize(vm, root, key, reviver);
    }
    Ok(unfiltered)
}

/// InternalizeJSONProperty(holder, name, reviver).
fn internalize(vm: &mut Vm, holder: ObjId, name: PropKey, reviver: Value) -> JsResult<Value> {
    let val = vm.get(holder, name)?;
    if let Value::Object(o) = val {
        if vm.is_array(val)? {
            let len = vm.length_of_array_like(o)?;
            let mut i = 0.0;
            while i < len {
                let k = vm.number_key(i);
                let new_el = internalize(vm, o, k, reviver)?;
                if new_el.is_undefined() {
                    vm.delete_property(o, k)?;
                } else {
                    vm.create_data_property(o, k, new_el)?;
                }
                i += 1.0;
            }
        } else {
            let keys = vm.enumerable_own_properties(o, 0)?;
            for k in keys {
                let k = vm.to_property_key(k)?;
                let new_el = internalize(vm, o, k, reviver)?;
                if new_el.is_undefined() {
                    vm.delete_property(o, k)?;
                } else {
                    vm.create_data_property(o, k, new_el)?;
                }
            }
        }
    }
    let name_v = vm.key_to_value(name);
    vm.call_value(reviver, Value::Object(holder), &[name_v, val])
}

// ---- stringify -----------------------------------------------------------------------

struct Stringify {
    replacer: Option<Value>,
    property_list: Option<Vec<PropKey>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<ObjId>,
}

fn json_stringify(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut st = Stringify {
        replacer: None,
        property_list: None,
        gap: Vec::new(),
        indent: Vec::new(),
        stack: Vec::new(),
    };
    let replacer = a.arg(1);
    if let Value::Object(r) = replacer {
        if vm.is_callable(replacer) {
            st.replacer = Some(replacer);
        } else if vm.is_array(replacer)? {
            let len = vm.length_of_array_like(r)?;
            let mut list: Vec<PropKey> = Vec::new();
            let mut i = 0.0;
            while i < len {
                let k = vm.number_key(i);
                let v = vm.get(r, k)?;
                let item: Option<StrId> = match v {
                    Value::String(s) => Some(s),
                    Value::Number(_) => Some(vm.to_string(v)?),
                    Value::Object(o) => match vm.heap.obj(o).class {
                        Class::String(_) | Class::Number(_) => Some(vm.to_string(v)?),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(s) = item {
                    let key = vm.key_from_str(s);
                    if !list.contains(&key) {
                        list.push(key);
                    }
                }
                i += 1.0;
            }
            st.property_list = Some(list);
        }
    }
    let mut space = a.arg(2);
    if let Value::Object(o) = space {
        match vm.heap.obj(o).class {
            Class::Number(_) => space = Value::Number(vm.to_number(space)?),
            Class::String(_) => space = Value::String(vm.to_string(space)?),
            _ => {}
        }
    }
    match space {
        Value::Number(n) => {
            let n = crate::convert::to_integer_or_infinity_f64(n).min(10.0);
            if n >= 1.0 {
                st.gap = vec![0x20; n as usize];
            }
        }
        Value::String(s) => {
            let u = vm.heap.units(s).to_vec();
            st.gap = u[..u.len().min(10)].to_vec();
        }
        _ => {}
    }
    let wrapper = vm.new_object();
    let key = PropKey::Str(vm.names.empty);
    vm.create_data_property_or_throw(wrapper, key, a.arg(0))?;
    let mut out = StrBuilder::new();
    let mark = vm.root_mark();
    vm.root(Value::Object(wrapper));
    let r = serialize_property(vm, &mut st, key, wrapper, &mut out);
    vm.pop_roots(mark);
    match r? {
        true => Ok(vm.string_from_builder(out)),
        false => Ok(Value::Undefined),
    }
}

/// SerializeJSONProperty: appends to `out`; returns false for `undefined`.
fn serialize_property(
    vm: &mut Vm,
    st: &mut Stringify,
    key: PropKey,
    holder: ObjId,
    out: &mut StrBuilder,
) -> JsResult<bool> {
    let mut value = vm.get(holder, key)?;
    if matches!(value, Value::Object(_) | Value::BigInt(_)) {
        let tojson_key = PropKey::Str(vm.names.toJSON);
        let to_json = vm.get_value(value, tojson_key)?;
        if vm.is_callable(to_json) {
            let k = vm.key_to_value(key);
            value = vm.call_value(to_json, value, &[k])?;
        }
    }
    if let Some(rep) = st.replacer {
        let k = vm.key_to_value(key);
        value = vm.call_value(rep, Value::Object(holder), &[k, value])?;
    }
    if let Value::Object(o) = value {
        let unwrapped = match vm.heap.obj(o).class {
            Class::Number(_) => Some(Value::Number(vm.to_number(value)?)),
            Class::String(_) => Some(Value::String(vm.to_string(value)?)),
            Class::Boolean(b) => Some(Value::Bool(b)),
            Class::BigInt(b) => Some(Value::BigInt(b)),
            _ => None,
        };
        if let Some(u) = unwrapped {
            value = u;
        }
    }
    match value {
        Value::Null => {
            out.push_str("null");
            Ok(true)
        }
        Value::Bool(true) => {
            out.push_str("true");
            Ok(true)
        }
        Value::Bool(false) => {
            out.push_str("false");
            Ok(true)
        }
        Value::String(s) => {
            let u = vm.heap.units(s).to_vec();
            quote_json_string(&u, out);
            Ok(true)
        }
        Value::Number(n) => {
            if n.is_finite() {
                out.push_str(&to_js_string(n));
            } else {
                out.push_str("null");
            }
            Ok(true)
        }
        Value::BigInt(_) => vm.type_error("Do not know how to serialize a BigInt"),
        Value::Object(o) if !vm.is_callable(value) => {
            if vm.is_array(value)? {
                serialize_array(vm, st, o, out)?;
            } else {
                serialize_object(vm, st, o, out)?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// QuoteJSONString.
pub fn quote_json_string(units: &[u16], out: &mut StrBuilder) {
    out.push(0x22);
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        match c {
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0C => out.push_str("\\f"),
            0x0D => out.push_str("\\r"),
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            c if c < 0x20 => out.push_str(&format!("\\u{c:04x}")),
            0xD800..=0xDBFF => {
                if i + 1 < units.len() && (0xDC00..=0xDFFF).contains(&units[i + 1]) {
                    out.push(c);
                    out.push(units[i + 1]);
                    i += 1;
                } else {
                    out.push_str(&format!("\\u{c:04x}"));
                }
            }
            0xDC00..=0xDFFF => out.push_str(&format!("\\u{c:04x}")),
            c => out.push(c),
        }
        i += 1;
    }
    out.push(0x22);
}

fn enter(vm: &mut Vm, st: &mut Stringify, o: ObjId) -> JsResult<Vec<u16>> {
    if st.stack.contains(&o) {
        return vm.type_error("Converting circular structure to JSON");
    }
    st.stack.push(o);
    let stepback = st.indent.clone();
    let gap = st.gap.clone();
    st.indent.extend_from_slice(&gap);
    Ok(stepback)
}

fn leave(st: &mut Stringify, stepback: Vec<u16>) {
    st.stack.pop();
    st.indent = stepback;
}

fn serialize_object(
    vm: &mut Vm,
    st: &mut Stringify,
    o: ObjId,
    out: &mut StrBuilder,
) -> JsResult<()> {
    let stepback = enter(vm, st, o)?;
    let keys: Vec<PropKey> = match &st.property_list {
        Some(list) => list.clone(),
        None => {
            let vals = vm.enumerable_own_properties(o, 0)?;
            let mut ks = Vec::with_capacity(vals.len());
            for v in vals {
                ks.push(vm.to_property_key(v)?);
            }
            ks
        }
    };
    out.push(0x7B);
    let mut any = false;
    for k in keys {
        let mut part = StrBuilder::new();
        if serialize_property(vm, st, k, o, &mut part)? {
            if any {
                out.push(0x2C);
            }
            if !st.gap.is_empty() {
                out.push(0x0A);
                out.push_slice(&st.indent);
            }
            let ks = vm.key_to_string(k);
            let ku = vm.heap.units(ks).to_vec();
            quote_json_string(&ku, out);
            out.push(0x3A);
            if !st.gap.is_empty() {
                out.push(0x20);
            }
            out.push_slice(&part.finish());
            any = true;
        }
    }
    if any && !st.gap.is_empty() {
        out.push(0x0A);
        out.push_slice(&stepback);
    }
    out.push(0x7D);
    leave(st, stepback);
    Ok(())
}

fn serialize_array(
    vm: &mut Vm,
    st: &mut Stringify,
    o: ObjId,
    out: &mut StrBuilder,
) -> JsResult<()> {
    let stepback = enter(vm, st, o)?;
    let len = vm.length_of_array_like(o)?;
    out.push(0x5B);
    let mut i = 0.0;
    while i < len {
        if i > 0.0 {
            out.push(0x2C);
        }
        if !st.gap.is_empty() {
            out.push(0x0A);
            out.push_slice(&st.indent);
        }
        let k = vm.number_key(i);
        if !serialize_property(vm, st, k, o, out)? {
            out.push_str("null");
        }
        i += 1.0;
    }
    if len > 0.0 && !st.gap.is_empty() {
        out.push(0x0A);
        out.push_slice(&stepback);
    }
    out.push(0x5D);
    leave(st, stepback);
    Ok(())
}
