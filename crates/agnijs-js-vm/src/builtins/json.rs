//! JSON.parse and JSON.stringify.

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::string::{StrBuilder, Units};
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;
use agnijs_js_parser::number::{format_js_number, NUMBER_BUF_LEN};

pub fn install(vm: &mut Vm, realm: RealmId) {
    let json = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::JSON, json);
    vm.method(json, "isRawJSON", 1, json_is_raw_json);
    vm.method(json, "parse", 2, json_parse);
    vm.method(json, "rawJSON", 1, json_raw_json);
    vm.method(json, "stringify", 3, json_stringify);
    super::to_string_tag(vm, json, "JSON");
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "JSON", Value::Object(json), Attrs::METHOD);
}

// ---- parse ---------------------------------------------------------------------------

/// Parse JSON text given as a Rust string (JSON modules).
pub fn parse_json_text(vm: &mut Vm, text: &str) -> JsResult<Value> {
    if text.is_ascii() {
        return parse_units(vm, text.as_bytes());
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    parse_units(vm, &units)
}

/// A code unit of JSON text. The parser runs over a string's own one-byte
/// or two-byte storage, so one-byte text is never widened.
trait Unit: Copy {
    fn u(self) -> u16;
    fn units(s: &[Self]) -> Units<'_>;
}

impl Unit for u8 {
    #[inline]
    fn u(self) -> u16 {
        self as u16
    }
    fn units(s: &[u8]) -> Units<'_> {
        Units::One(s)
    }
}

impl Unit for u16 {
    #[inline]
    fn u(self) -> u16 {
        self
    }
    fn units(s: &[u16]) -> Units<'_> {
        Units::Two(s)
    }
}

fn parse_units<T: Unit>(vm: &mut Vm, units: &[T]) -> JsResult<Value> {
    Ok(parse_units_tracked(vm, units, false)?.0)
}

/// Parse, also building the JSON Parse Record tree (the source text of
/// each primitive) when `track` is set, for a reviver's `context`.
fn parse_units_tracked<T: Unit>(vm: &mut Vm, units: &[T], track: bool) -> JsResult<(Value, Option<Rec>)> {
    let mut p = Parser {
        units,
        pos: 0,
        depth: 0,
        items: Vec::new(),
        scratch: Vec::new(),
        track,
        recs: Vec::new(),
        values: 0,
    };
    p.skip_ws();
    let v = p.parse_value(vm)?;
    p.skip_ws();
    if p.pos != units.len() {
        return p.fail(vm, "Unexpected token after JSON value");
    }
    Ok((v, p.recs.pop()))
}

/// A JSON Parse Record: the value a node parsed to and, for a primitive,
/// its source text; for an array or object, the records of its elements
/// or (last definition of each) entries.
enum Rec {
    Prim(Value, Value),
    Array(Value, Vec<Rec>),
    Object(Value, Vec<(PropKey, Rec)>),
}

impl Rec {
    fn value(&self) -> Value {
        match self {
            Rec::Prim(v, _) | Rec::Array(v, _) | Rec::Object(v, _) => *v,
        }
    }
}

struct Parser<'a, T: Unit> {
    units: &'a [T],
    pos: usize,
    depth: u32,
    /// The elements of the arrays being parsed, innermost last (one
    /// buffer for the whole parse; each array takes its run at the end).
    items: Vec<Value>,
    /// The content of the current string when it has escapes.
    scratch: Vec<u16>,
    /// Whether to build parse records (JSON.parse with a reviver).
    track: bool,
    /// The records of the values parsed so far, innermost last.
    recs: Vec<Rec>,
    /// Array elements and object members parsed so far; the host's
    /// interrupt hook is asked once per 4096 (TASK-103.28).
    values: u32,
}

const MAX_JSON_DEPTH: u32 = 5000;

/// Where a parsed string's content is.
enum Content {
    /// `units[start..end]` of the text, which had no escapes.
    Text(usize, usize),
    /// The parser's scratch buffer.
    Scratch,
}

impl<'a, T: Unit> Parser<'a, T> {
    /// Count one array element or object member, and ask the host whether
    /// to stop once per 4096. A parse is linear in its text, so this only
    /// bounds how long a huge text holds the thread.
    #[inline(always)]
    fn poll(&mut self, vm: &mut Vm) -> JsResult<()> {
        self.values = self.values.wrapping_add(1);
        if self.values & 0xfff == 0 {
            return vm.ask_host_interrupt();
        }
        Ok(())
    }

    #[inline]
    fn peek(&self) -> Option<u16> {
        self.units.get(self.pos).map(|c| c.u())
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

    fn fail<R>(&self, vm: &mut Vm, msg: &str) -> JsResult<R> {
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
        vm.check_stack()?;
        if !self.track {
            return self.parse_value_inner(vm);
        }
        let start = self.pos;
        let is_composite = matches!(self.peek(), Some(0x7B) | Some(0x5B));
        let v = self.parse_value_inner(vm)?;
        if !is_composite {
            let src = vm.heap.alloc_str_units(T::units(&self.units[start..self.pos]));
            self.recs.push(Rec::Prim(v, Value::String(src)));
        }
        Ok(v)
    }

    fn parse_value_inner(&mut self, vm: &mut Vm) -> JsResult<Value> {
        match self.peek() {
            None => self.fail(vm, "Unexpected end of JSON input"),
            Some(0x7B) => self.parse_object(vm),
            Some(0x5B) => self.parse_array(vm),
            Some(0x22) => {
                let s = match self.parse_string(vm)? {
                    Content::Text(a, b) => vm.heap.alloc_str_units(T::units(&self.units[a..b])),
                    Content::Scratch => vm.heap.alloc_str_u16(&self.scratch),
                };
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

    fn skip_digits(&mut self) {
        while matches!(self.peek(), Some(c) if (0x30..=0x39).contains(&c)) {
            self.pos += 1;
        }
    }

    fn parse_number(&mut self, vm: &mut Vm) -> JsResult<Value> {
        let start = self.pos;
        let negative = self.peek() == Some(0x2D);
        if negative {
            self.pos += 1;
        }
        let int_start = self.pos;
        match self.peek() {
            Some(0x30) => {
                self.pos += 1;
            }
            Some(c) if (0x31..=0x39).contains(&c) => self.skip_digits(),
            _ => return self.fail(vm, "No number after minus sign"),
        }
        let int_end = self.pos;
        let mut plain = true;
        if self.peek() == Some(0x2E) {
            plain = false;
            self.pos += 1;
            let ds = self.pos;
            self.skip_digits();
            if self.pos == ds {
                return self.fail(vm, "Unterminated fractional number");
            }
        }
        if matches!(self.peek(), Some(0x65) | Some(0x45)) {
            plain = false;
            self.pos += 1;
            if matches!(self.peek(), Some(0x2B) | Some(0x2D)) {
                self.pos += 1;
            }
            let ds = self.pos;
            self.skip_digits();
            if self.pos == ds {
                return self.fail(vm, "Exponent part is missing a number");
            }
        }
        // An integer of up to 15 digits is exact in an f64.
        if plain && int_end - int_start <= 15 {
            let mut v: u64 = 0;
            for c in &self.units[int_start..int_end] {
                v = v * 10 + (c.u() - 0x30) as u64;
            }
            let n = v as f64;
            return Ok(Value::Number(if negative { -n } else { n }));
        }
        // The text is ASCII digits, signs, '.' and 'e'.
        let text = &self.units[start..self.pos];
        let mut buf = [0u8; 64];
        let n: f64 = if text.len() <= buf.len() {
            for (b, c) in buf.iter_mut().zip(text) {
                *b = c.u() as u8;
            }
            let s = std::str::from_utf8(&buf[..text.len()]).expect("ASCII");
            s.parse().unwrap_or(f64::NAN)
        } else {
            let s: String = text.iter().map(|c| c.u() as u8 as char).collect();
            s.parse().unwrap_or(f64::NAN)
        };
        Ok(Value::Number(n))
    }

    /// Read a string literal (at its opening quote).
    fn parse_string(&mut self, vm: &mut Vm) -> JsResult<Content> {
        // Opening quote.
        self.pos += 1;
        let start = self.pos;
        // The common case: no escapes, so the content is the text itself.
        loop {
            let Some(c) = self.peek() else {
                return self.fail(vm, "Unterminated string");
            };
            if c == 0x22 {
                self.pos += 1;
                return Ok(Content::Text(start, self.pos - 1));
            }
            if c == 0x5C {
                break;
            }
            if c < 0x20 {
                self.pos += 1;
                return self.fail(vm, "Bad control character in string literal");
            }
            self.pos += 1;
        }
        self.scratch.clear();
        self.scratch.extend(self.units[start..self.pos].iter().map(|c| c.u()));
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
                    let u = match e {
                        0x22 => 0x22,
                        0x5C => 0x5C,
                        0x2F => 0x2F,
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => 0x0A,
                        0x72 => 0x0D,
                        0x74 => 0x09,
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
                            v as u16
                        }
                        _ => return self.fail(vm, "Bad escaped character"),
                    };
                    self.scratch.push(u);
                }
                c if c < 0x20 => return self.fail(vm, "Bad control character in string literal"),
                c => self.scratch.push(c),
            }
        }
        Ok(Content::Scratch)
    }

    /// A property name: an index key, or the atom with the name's content
    /// (allocated only the first time a name is seen).
    fn parse_key(&mut self, vm: &mut Vm) -> JsResult<PropKey> {
        let units = match self.parse_string(vm)? {
            Content::Text(a, b) => T::units(&self.units[a..b]),
            Content::Scratch => Units::Two(&self.scratch),
        };
        let index = match units {
            Units::One(b) => PropKey::index_from_bytes(b),
            Units::Two(w) => PropKey::index_from_units(w),
        };
        Ok(match index {
            Some(i) => PropKey::Index(i),
            None => PropKey::Str(vm.heap.atom_units(units)),
        })
    }

    fn parse_array(&mut self, vm: &mut Vm) -> JsResult<Value> {
        self.pos += 1;
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(vm.throw_range("Maximum call stack size exceeded"));
        }
        let mark = vm.root_mark();
        let first = self.items.len();
        self.skip_ws();
        if self.peek() == Some(0x5D) {
            self.pos += 1;
        } else {
            loop {
                self.poll(vm)?;
                self.skip_ws();
                let v = self.parse_value(vm)?;
                vm.root(v);
                self.items.push(v);
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
        let items: Vec<Value> = self.items.drain(first..).collect();
        let n = items.len();
        vm.pop_roots(mark);
        self.depth -= 1;
        let v = Value::Object(vm.new_array(items));
        if self.track {
            let at = self.recs.len() - n;
            let elements = self.recs.split_off(at);
            self.recs.push(Rec::Array(v, elements));
        }
        Ok(v)
    }

    fn parse_object(&mut self, vm: &mut Vm) -> JsResult<Value> {
        self.pos += 1;
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(vm.throw_range("Maximum call stack size exceeded"));
        }
        let obj = vm.new_object();
        let mut entries: Vec<(PropKey, Rec)> = Vec::new();
        let mark = vm.root_mark();
        vm.root(Value::Object(obj));
        self.skip_ws();
        if self.peek() == Some(0x7D) {
            self.pos += 1;
        } else {
            loop {
                self.poll(vm)?;
                self.skip_ws();
                if self.peek() != Some(0x22) {
                    return self.fail(vm, "Expected property name");
                }
                let key = self.parse_key(vm)?;
                self.skip_ws();
                if self.peek() != Some(0x3A) {
                    return self.fail(vm, "Expected ':' after property name");
                }
                self.pos += 1;
                self.skip_ws();
                let v = self.parse_value(vm)?;
                vm.create_data_property(obj, key, v)?;
                if self.track {
                    let r = self.recs.pop().expect("a parsed value has a record");
                    match entries.iter_mut().find(|(k, _)| *k == key) {
                        Some(e) => e.1 = r,
                        None => entries.push((key, r)),
                    }
                }
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
        if self.track {
            self.recs.push(Rec::Object(Value::Object(obj), entries));
        }
        Ok(Value::Object(obj))
    }
}

fn json_parse(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let text = vm.to_string(a.arg(0))?;
    let reviver = a.arg(1);
    let track = vm.is_callable(reviver);
    // A copy of the text in its own storage width: the parser allocates
    // into the heap while it reads.
    let (unfiltered, rec) = match vm.heap.units(text) {
        Units::One(b) => {
            let b = b.to_vec();
            parse_units_tracked(vm, &b, track)?
        }
        Units::Two(u) => {
            let u = u.to_vec();
            parse_units_tracked(vm, &u, track)?
        }
    };
    if track {
        let root = vm.new_object();
        let key = PropKey::Str(vm.names.empty);
        vm.create_data_property_or_throw(root, key, unfiltered)?;
        return internalize(vm, root, key, reviver, rec.as_ref());
    }
    Ok(unfiltered)
}

/// InternalizeJSONProperty(holder, name, reviver, parseRecord). The
/// record describes the value as parsed; once the value at `name` is no
/// longer that value (a reviver replaced it), it and its children get no
/// source text.
fn internalize(vm: &mut Vm, holder: ObjId, name: PropKey, reviver: Value, rec: Option<&Rec>) -> JsResult<Value> {
    vm.check_stack()?;
    let val = vm.get(holder, name)?;
    let context = vm.new_object();
    let rec = match rec {
        Some(r) if vm.same_value(r.value(), val) => Some(r),
        _ => None,
    };
    if let Some(Rec::Prim(_, src)) = rec {
        let key = PropKey::Str(vm.atom("source"));
        vm.create_data_property_or_throw(context, key, *src)?;
    }
    if let Value::Object(o) = val {
        if vm.is_array(val)? {
            let elements: &[Rec] = match rec {
                Some(Rec::Array(_, e)) => e,
                _ => &[],
            };
            let len = vm.length_of_array_like(o)?;
            let mut i = 0.0;
            while i < len {
                vm.poll()?;
                let k = vm.number_key(i);
                let new_el = internalize(vm, o, k, reviver, elements.get(i as usize))?;
                if new_el.is_undefined() {
                    vm.delete_property(o, k)?;
                } else {
                    vm.create_data_property(o, k, new_el)?;
                }
                i += 1.0;
            }
        } else {
            let entries: &[(PropKey, Rec)] = match rec {
                Some(Rec::Object(_, e)) => e,
                _ => &[],
            };
            let keys = vm.enumerable_own_keys(o)?;
            for k in keys {
                vm.poll()?;
                let entry = entries.iter().find(|(ek, _)| *ek == k).map(|(_, r)| r);
                let new_el = internalize(vm, o, k, reviver, entry)?;
                if new_el.is_undefined() {
                    vm.delete_property(o, k)?;
                } else {
                    vm.create_data_property(o, k, new_el)?;
                }
            }
        }
    }
    let name_v = vm.key_to_value(name);
    vm.call_value(reviver, Value::Object(holder), &[name_v, val, Value::Object(context)])
}

/// JSON.rawJSON(text): a frozen null-prototype object whose `rawJSON` is
/// the text of a JSON primitive, which JSON.stringify emits verbatim.
fn json_raw_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let units: Vec<u16> = vm.heap.units(s).iter().collect();
    let edge_ws = |c: u16| matches!(c, 0x09 | 0x0A | 0x0D | 0x20);
    match (units.first(), units.last()) {
        (None, _) => return vm.syntax_error("JSON.rawJSON: empty text"),
        (Some(&f), Some(&l)) if edge_ws(f) || edge_ws(l) => {
            return vm.syntax_error("JSON.rawJSON: the text starts or ends with whitespace")
        }
        (Some(0x7B) | Some(0x5B), _) => {
            return vm.syntax_error("JSON.rawJSON: the text must be a JSON primitive, not an object or array")
        }
        _ => {}
    }
    parse_units(vm, &units)?;
    let o = vm.new_object_with_proto(None);
    vm.heap.obj_mut(o).class = Class::RawJSON;
    let key = PropKey::Str(vm.atom("rawJSON"));
    vm.create_data_property_or_throw(o, key, Value::String(s))?;
    vm.set_integrity_level(o, true)?;
    Ok(Value::Object(o))
}

/// JSON.isRawJSON(O).
fn json_is_raw_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let is = match a.arg(0) {
        Value::Object(o) => matches!(vm.heap.obj(o).class, Class::RawJSON),
        _ => false,
    };
    Ok(Value::Bool(is))
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
                vm.poll()?;
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
        true => vm.string_from_builder(out),
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
    vm.check_stack()?;
    vm.check_string_len(out.len())?;
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
        if matches!(vm.heap.obj(o).class, Class::RawJSON) {
            let key = PropKey::Str(vm.atom("rawJSON"));
            if let Value::String(s) = vm.get(o, key)? {
                out.push_units(&vm.heap.units(s));
            }
            return Ok(true);
        }
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
            let u = vm.heap.units(s);
            quote_json_string(u, out);
            Ok(true)
        }
        Value::Number(n) => {
            if n.is_finite() {
                let mut buf = [0u8; NUMBER_BUF_LEN];
                out.push_bytes(format_js_number(n, &mut buf).as_bytes());
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
pub fn quote_json_string(units: Units<'_>, out: &mut StrBuilder) {
    out.push(0x22);
    match units {
        Units::One(b) => {
            // Runs of characters that need no escape are copied whole.
            let mut run = 0;
            for (i, &c) in b.iter().enumerate() {
                if c < 0x20 || c == 0x22 || c == 0x5C {
                    out.push_bytes(&b[run..i]);
                    push_escape(c as u16, out);
                    run = i + 1;
                }
            }
            out.push_bytes(&b[run..]);
        }
        Units::Two(u) => {
            let mut run = 0;
            let mut i = 0;
            while i < u.len() {
                let c = u[i];
                let plain = match c {
                    0x22 | 0x5C => false,
                    c if c < 0x20 => false,
                    0xD800..=0xDBFF if i + 1 < u.len() && (0xDC00..=0xDFFF).contains(&u[i + 1]) => {
                        // A surrogate pair stays as it is.
                        i += 2;
                        continue;
                    }
                    0xD800..=0xDFFF => false,
                    _ => true,
                };
                if !plain {
                    out.push_slice(&u[run..i]);
                    push_escape(c, out);
                    run = i + 1;
                }
                i += 1;
            }
            out.push_slice(&u[run..]);
        }
    }
    out.push(0x22);
}

/// The escape QuoteJSONString writes for `c`: a short form, or `\uXXXX`
/// for other control characters and lone surrogates.
fn push_escape(c: u16, out: &mut StrBuilder) {
    let short: &[u8] = match c {
        0x08 => b"\\b",
        0x09 => b"\\t",
        0x0A => b"\\n",
        0x0C => b"\\f",
        0x0D => b"\\r",
        0x22 => b"\\\"",
        0x5C => b"\\\\",
        _ => b"",
    };
    if !short.is_empty() {
        out.push_bytes(short);
        return;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push_bytes(&[
        b'\\',
        b'u',
        HEX[(c >> 12) as usize & 0xF],
        HEX[(c >> 8) as usize & 0xF],
        HEX[(c >> 4) as usize & 0xF],
        HEX[c as usize & 0xF],
    ]);
}

/// A property name as QuoteJSONString writes it.
fn quote_key(vm: &mut Vm, k: PropKey, out: &mut StrBuilder) {
    match k {
        PropKey::Index(i) => {
            let mut buf = [0u8; NUMBER_BUF_LEN];
            out.push(0x22);
            out.push_bytes(format_js_number(i as f64, &mut buf).as_bytes());
            out.push(0x22);
        }
        PropKey::Str(s) => quote_json_string(vm.heap.units(s), out),
        PropKey::Sym(_) => unreachable!("JSON keys are strings"),
    }
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
        None => vm.enumerable_own_keys(o)?,
    };
    out.push(0x7B);
    let mut any = false;
    for k in keys {
        vm.poll()?;
        // The member is written in place and taken back if the value turns
        // out to be undefined; writing to `out` is not observable, so this
        // is the same as serializing the value first.
        let mark = out.len();
        if any {
            out.push(0x2C);
        }
        if !st.gap.is_empty() {
            out.push(0x0A);
            out.push_slice(&st.indent);
        }
        quote_key(vm, k, out);
        out.push(0x3A);
        if !st.gap.is_empty() {
            out.push(0x20);
        }
        if serialize_property(vm, st, k, o, out)? {
            any = true;
        } else {
            out.truncate(mark);
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
        vm.poll()?;
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
