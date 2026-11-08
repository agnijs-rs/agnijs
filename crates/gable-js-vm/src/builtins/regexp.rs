//! RegExp objects (ES2025 §22.2): the constructor, `%RegExp.prototype%`,
//! the `@@match` / `@@matchAll` / `@@replace` / `@@search` / `@@split`
//! protocol, `%RegExpStringIteratorPrototype%`, `RegExp.escape` and the
//! Annex B `compile`. Matching itself goes through the `gable-js-regex`
//! seam (ADR 0013); this module owns the ECMAScript algorithms around it.

use std::rc::Rc;

use gable_engine_api::RealmId;
use gable_js_regex::{Flags, Regex};

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};

/// The internal slots of a RegExp instance: `[[OriginalSource]]`,
/// `[[OriginalFlags]]` and `[[RegExpMatcher]]`.
pub struct RegExpData {
    pub source: StrId,
    pub flags: StrId,
    pub matcher: Rc<Regex>,
    pub parsed: Flags,
}

impl RegExpData {
    pub fn trace(&self, t: &mut Tracer) {
        t.str(self.source);
        t.str(self.flags);
    }
}

/// `%RegExpStringIteratorPrototype%` instance state.
pub struct RegExpIterState {
    pub regexp: Value,
    pub string: Value,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

const CACHE_LIMIT: usize = 1024;

// ---- creation ----------------------------------------------------------------------------

/// Compile `pattern` under `flags`, sharing compiled matchers between
/// RegExp objects with the same source and flags (regex literals in a loop
/// body create a fresh object per evaluation; the matcher is immutable, so
/// one compilation serves them all).
fn compile(vm: &mut Vm, pattern: StrId, flags: StrId) -> JsResult<(Rc<Regex>, Flags)> {
    let f_units = vm.heap.units(flags).to_vec();
    let parsed = match Flags::parse(&f_units) {
        Ok(f) => f,
        Err(msg) => return vm.syntax_error(&msg),
    };
    let p_units = vm.heap.units(pattern).to_vec();
    let key = (p_units, parsed.to_string());
    if let Some(r) = vm.regex_cache.get(&key) {
        return Ok((r.clone(), parsed));
    }
    let re = match Regex::compile(&key.0, &parsed) {
        Ok(r) => Rc::new(r),
        Err(msg) => return vm.syntax_error(&msg),
    };
    if vm.regex_cache.len() >= CACHE_LIMIT {
        vm.regex_cache.clear();
    }
    vm.regex_cache.insert(key, re.clone());
    Ok((re, parsed))
}

/// RegExpAlloc(newTarget): an object with the RegExp slots unset and a
/// `lastIndex` own property (writable, non-enumerable, non-configurable).
fn regexp_alloc(vm: &mut Vm, new_target: Value) -> JsResult<ObjId> {
    let proto = vm.get_prototype_from_constructor(new_target, Intrinsic::RegExpPrototype)?;
    let placeholder = RegExpData {
        source: vm.names.empty,
        flags: vm.names.empty,
        matcher: empty_matcher(vm),
        parsed: Flags::default(),
    };
    let o = vm.alloc(ObjectData::new(
        Some(proto),
        Class::RegExp(Box::new(placeholder)),
    ));
    let key = PropKey::Str(vm.names.lastIndex);
    vm.define_raw(o, key, Value::Number(0.0), Attrs::WRITABLE_ONLY);
    Ok(o)
}

fn empty_matcher(vm: &mut Vm) -> Rc<Regex> {
    let key = (Vec::new(), String::new());
    if let Some(r) = vm.regex_cache.get(&key) {
        return r.clone();
    }
    let re = Rc::new(Regex::compile(&[], &Flags::default()).expect("empty pattern compiles"));
    vm.regex_cache.insert(key, re.clone());
    re
}

/// RegExpInitialize(obj, pattern, flags).
fn regexp_initialize(vm: &mut Vm, obj: ObjId, pattern: Value, flags: Value) -> JsResult<Value> {
    let p = if pattern.is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(pattern)?
    };
    let f = if flags.is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(flags)?
    };
    let (matcher, parsed) = compile(vm, p, f)?;
    match &mut vm.heap.obj_mut(obj).class {
        Class::RegExp(d) => {
            d.source = p;
            d.flags = f;
            d.matcher = matcher;
            d.parsed = parsed;
        }
        _ => unreachable!("RegExpInitialize on a non-RegExp"),
    }
    let key = PropKey::Str(vm.names.lastIndex);
    vm.set(obj, key, Value::Number(0.0), true)?;
    Ok(Value::Object(obj))
}

/// RegExpCreate for a literal `/p/f` (the compiler validated the syntax, so
/// a failure here is a matcher limitation reported as a SyntaxError).
pub fn regexp_create(vm: &mut Vm, pattern: StrId, flags: StrId) -> JsResult<Value> {
    let ctor = vm.intrinsic(Intrinsic::RegExp);
    let o = regexp_alloc(vm, Value::Object(ctor))?;
    regexp_initialize(vm, o, Value::String(pattern), Value::String(flags))
}

/// RegExpCreate(P, F) with Value arguments (String.prototype.match etc.).
pub fn regexp_create_from_value(vm: &mut Vm, pattern: Value, flags: Value) -> JsResult<Value> {
    let ctor = vm.intrinsic(Intrinsic::RegExp);
    let o = regexp_alloc(vm, Value::Object(ctor))?;
    regexp_initialize(vm, o, pattern, flags)
}

/// IsRegExp(argument).
pub fn is_regexp(vm: &mut Vm, v: Value) -> JsResult<bool> {
    let Value::Object(o) = v else {
        return Ok(false);
    };
    let key = PropKey::Sym(vm.well_known.match_);
    let m = vm.get(o, key)?;
    if !m.is_undefined() {
        return Ok(vm.to_boolean(m));
    }
    Ok(matches!(vm.heap.obj(o).class, Class::RegExp(_)))
}

/// The code units of `s`, shared across the repeated execs a global
/// operation performs on one string.
fn input_units(vm: &mut Vm, s: StrId) -> Rc<Vec<u16>> {
    if let Some((id, u)) = &vm.regex_input_cache {
        if *id == s {
            return u.clone();
        }
    }
    let u = Rc::new(vm.heap.units(s).to_vec());
    vm.regex_input_cache = Some((s, u.clone()));
    u
}

fn regexp_data(vm: &Vm, v: Value) -> Option<(StrId, StrId, Rc<Regex>, Flags)> {
    let Value::Object(o) = v else { return None };
    match &vm.heap.obj(o).class {
        Class::RegExp(d) => Some((d.source, d.flags, d.matcher.clone(), d.parsed)),
        _ => None,
    }
}

fn this_regexp_object(vm: &mut Vm, this: Value, what: &str) -> JsResult<ObjId> {
    match this {
        Value::Object(o) => Ok(o),
        _ => vm.type_error(&format!("{what} called on a non-object")),
    }
}

// ---- install -----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    let proto = vm.new_object();
    let ctor = super::install_ctor(
        vm,
        realm,
        "RegExp",
        2,
        regexp_constructor,
        proto,
        Intrinsic::RegExp,
        Intrinsic::RegExpPrototype,
    );
    super::species_accessor(vm, ctor);
    vm.method(ctor, "escape", 1, regexp_escape);

    vm.method(proto, "exec", 1, proto_exec);
    vm.method(proto, "test", 1, proto_test);
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "compile", 2, proto_compile);
    vm.accessor(proto, "flags", flags_getter, None);
    vm.accessor(proto, "source", source_getter, None);
    vm.accessor(proto, "dotAll", get_dot_all, None);
    vm.accessor(proto, "global", get_global, None);
    vm.accessor(proto, "hasIndices", get_has_indices, None);
    vm.accessor(proto, "ignoreCase", get_ignore_case, None);
    vm.accessor(proto, "multiline", get_multiline, None);
    vm.accessor(proto, "sticky", get_sticky, None);
    vm.accessor(proto, "unicode", get_unicode, None);
    vm.accessor(proto, "unicodeSets", get_unicode_sets, None);
    let wk = &vm.well_known;
    let (m, ma, r, s, sp) = (wk.match_, wk.match_all, wk.replace, wk.search, wk.split);
    vm.method_sym(proto, m, "Symbol.match", 1, symbol_match);
    vm.method_sym(proto, ma, "Symbol.matchAll", 1, symbol_match_all);
    vm.method_sym(proto, r, "Symbol.replace", 2, symbol_replace);
    vm.method_sym(proto, s, "Symbol.search", 1, symbol_search);
    vm.method_sym(proto, sp, "Symbol.split", 2, symbol_split);

    let iter_proto = vm.intrinsic(Intrinsic::IteratorPrototype);
    let rsi = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::RegExpStringIteratorPrototype, rsi);
    vm.method(rsi, "next", 0, regexp_string_iterator_next);
    super::to_string_tag(vm, rsi, "RegExp String Iterator");
}

// ---- constructor -------------------------------------------------------------------------

fn regexp_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let pattern = a.arg(0);
    let flags = a.arg(1);
    let pattern_is_regexp = is_regexp(vm, pattern)?;
    let new_target = if a.new_target.is_undefined() {
        let nt = Value::Object(a.callee);
        if pattern_is_regexp && flags.is_undefined() {
            let Value::Object(po) = pattern else { unreachable!() };
            let key = PropKey::Str(vm.names.constructor);
            let pattern_ctor = vm.get(po, key)?;
            if vm.same_value(nt, pattern_ctor) {
                return Ok(pattern);
            }
        }
        nt
    } else {
        a.new_target
    };
    let (p, f) = if let Some((src, fl, _, _)) = regexp_data(vm, pattern) {
        let f = if flags.is_undefined() {
            Value::String(fl)
        } else {
            flags
        };
        (Value::String(src), f)
    } else if pattern_is_regexp {
        let Value::Object(po) = pattern else { unreachable!() };
        let sk = PropKey::Str(vm.names.source);
        let p = vm.get(po, sk)?;
        let f = if flags.is_undefined() {
            let fk = PropKey::Str(vm.names.flags);
            vm.get(po, fk)?
        } else {
            flags
        };
        (p, f)
    } else {
        (pattern, flags)
    };
    let o = regexp_alloc(vm, new_target)?;
    regexp_initialize(vm, o, p, f)
}

// ---- RegExp.escape -----------------------------------------------------------------------

fn regexp_escape(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::String(s) = a.arg(0) else {
        return vm.type_error("RegExp.escape requires a string");
    };
    let units = vm.heap.units(s).to_vec();
    let mut out: Vec<u16> = Vec::with_capacity(units.len() + 8);
    let mut i = 0;
    while i < units.len() {
        let (cp, n) = code_point_at(&units, i);
        if out.is_empty() && (is_ascii_digit(cp) || is_ascii_letter(cp)) {
            out.extend("\\x".encode_utf16());
            out.extend(format!("{cp:02x}").encode_utf16());
        } else {
            encode_for_regexp_escape(cp, &units[i..i + n], &mut out);
        }
        i += n;
    }
    Ok(vm.str_from_units(out))
}

fn is_ascii_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}

fn is_ascii_letter(c: u32) -> bool {
    (0x41..=0x5A).contains(&c) || (0x61..=0x7A).contains(&c)
}

fn is_white_space_or_line_terminator(c: u32) -> bool {
    matches!(
        c,
        0x09 | 0x0B | 0x0C | 0x20 | 0xA0 | 0xFEFF | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F
            | 0x3000 | 0x0A | 0x0D | 0x2028 | 0x2029
    )
}

/// EncodeForRegExpEscape(c).
fn encode_for_regexp_escape(c: u32, raw: &[u16], out: &mut Vec<u16>) {
    const SYNTAX: &str = "^$\\.*+?()[]{}|/";
    const OTHER_PUNCT: &str = ",-=<>#&!%:;@~'`\"";
    if c < 0x80 && SYNTAX.contains(c as u8 as char) {
        out.push(b'\\' as u16);
        out.push(c as u16);
        return;
    }
    let control = match c {
        0x09 => Some('t'),
        0x0A => Some('n'),
        0x0B => Some('v'),
        0x0C => Some('f'),
        0x0D => Some('r'),
        _ => None,
    };
    if let Some(l) = control {
        out.push(b'\\' as u16);
        out.push(l as u16);
        return;
    }
    let is_surrogate = (0xD800..0xE000).contains(&c);
    if (c < 0x80 && OTHER_PUNCT.contains(c as u8 as char))
        || is_white_space_or_line_terminator(c)
        || is_surrogate
    {
        if c <= 0xFF {
            out.extend("\\x".encode_utf16());
            out.extend(format!("{c:02x}").encode_utf16());
        } else {
            for &u in raw {
                out.extend("\\u".encode_utf16());
                out.extend(format!("{u:04x}").encode_utf16());
            }
        }
        return;
    }
    out.extend_from_slice(raw);
}

/// CodePointAt(string, position): the code point and how many units it
/// occupies (a lone surrogate is its own code point).
fn code_point_at(units: &[u16], i: usize) -> (u32, usize) {
    let u = units[i] as u32;
    if (0xD800..0xDC00).contains(&u) && i + 1 < units.len() {
        let l = units[i + 1] as u32;
        if (0xDC00..0xE000).contains(&l) {
            return (0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00), 2);
        }
    }
    (u, 1)
}

/// AdvanceStringIndex(S, index, unicode).
pub fn advance_string_index(units: &[u16], index: usize, unicode: bool) -> usize {
    if !unicode || index + 1 >= units.len() {
        return index + 1;
    }
    index + code_point_at(units, index).1
}

// ---- exec --------------------------------------------------------------------------------

fn proto_exec(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some(o) = a.this.as_object().filter(|o| matches!(vm.heap.obj(*o).class, Class::RegExp(_))) else {
        return vm.type_error("RegExp.prototype.exec called on incompatible receiver");
    };
    let s = vm.to_string(a.arg(0))?;
    regexp_builtin_exec(vm, o, s)
}

fn proto_test(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_regexp_object(vm, a.this, "RegExp.prototype.test")?;
    let s = vm.to_string(a.arg(0))?;
    let m = regexp_exec(vm, r, s)?;
    Ok(Value::Bool(!m.is_null()))
}

/// RegExpExec(R, S).
pub fn regexp_exec(vm: &mut Vm, r: ObjId, s: StrId) -> JsResult<Value> {
    let key = PropKey::Str(vm.names.exec);
    let exec = vm.get(r, key)?;
    if vm.is_callable(exec) {
        let result = vm.call_value(exec, Value::Object(r), &[Value::String(s)])?;
        if !result.is_object() && !result.is_null() {
            return vm.type_error("RegExp exec method returned something other than an Object or null");
        }
        return Ok(result);
    }
    if !matches!(vm.heap.obj(r).class, Class::RegExp(_)) {
        return vm.type_error("RegExp.prototype.exec called on incompatible receiver");
    }
    regexp_builtin_exec(vm, r, s)
}

/// RegExpBuiltinExec(R, S).
fn regexp_builtin_exec(vm: &mut Vm, r: ObjId, s: StrId) -> JsResult<Value> {
    let input = input_units(vm, s);
    let length = input.len();
    let li_key = PropKey::Str(vm.names.lastIndex);
    let li_val = vm.get(r, li_key)?;
    let mut last_index = vm.to_length(li_val)?;
    let (matcher, flags) = match &vm.heap.obj(r).class {
        Class::RegExp(d) => (d.matcher.clone(), d.parsed),
        _ => unreachable!(),
    };
    let global = flags.global;
    let sticky = flags.sticky;
    let has_indices = flags.has_indices;
    let full_unicode = flags.unicode_mode();
    if !global && !sticky {
        last_index = 0.0;
    }
    if last_index > length as f64 {
        if global || sticky {
            vm.set(r, li_key, Value::Number(0.0), true)?;
        }
        return Ok(Value::Null);
    }
    let last_index = last_index as usize;
    // In unicode mode an index inside a surrogate pair denotes the
    // character that pair encodes, so the attempt starts at the pair.
    let mut attempt = last_index;
    if full_unicode
        && attempt > 0
        && attempt < length
        && (0xDC00..0xE000).contains(&input[attempt])
        && (0xD800..0xDC00).contains(&input[attempt - 1])
    {
        attempt -= 1;
    }
    let found = if sticky {
        matcher.exec_sticky(&input, attempt)
    } else {
        matcher.exec(&input, attempt)
    };
    let Some(caps) = found else {
        if global || sticky {
            vm.set(r, li_key, Value::Number(0.0), true)?;
        }
        return Ok(Value::Null);
    };
    let (found_start, e) = caps.ranges[0].expect("whole match");
    // The spec records the match as starting at lastIndex itself, even when
    // that index pointed into a surrogate pair.
    let m_start = if attempt != last_index && found_start == attempt {
        last_index
    } else {
        found_start
    };
    if global || sticky {
        vm.set(r, li_key, Value::Number(e as f64), true)?;
    }
    let n = matcher.group_count();
    let a = vm.array_create((n + 1) as f64, None)?;
    let idx_key = PropKey::Str(vm.names.index);
    vm.create_data_property_or_throw(a, idx_key, Value::Number(m_start as f64))?;
    let in_key = PropKey::Str(vm.names.input);
    vm.create_data_property_or_throw(a, in_key, Value::String(s))?;
    let matched = vm.str_from_units(input[m_start..e].to_vec());
    vm.create_data_property_or_throw(a, PropKey::Index(0), matched)?;
    let names = matcher.group_names().to_vec();
    let has_groups = names.iter().any(|n| n.is_some());
    let groups = if has_groups {
        Value::Object(vm.new_object_with_proto(None))
    } else {
        Value::Undefined
    };
    let g_key = PropKey::Str(vm.names.groups);
    vm.create_data_property_or_throw(a, g_key, groups)?;
    let mut matched_group_names: Vec<&str> = Vec::new();
    let mut group_names: Vec<Option<String>> = Vec::with_capacity(n);
    for i in 1..=n {
        let captured = match caps.ranges.get(i).copied().flatten() {
            Some((cs, ce)) => vm.str_from_units(input[cs..ce].to_vec()),
            None => Value::Undefined,
        };
        vm.create_data_property_or_throw(a, PropKey::Index(i as u32), captured)?;
        if let Some(name) = names[i - 1].as_deref() {
            if matched_group_names.contains(&name) {
                group_names.push(None);
            } else {
                if !captured.is_undefined() {
                    matched_group_names.push(name);
                }
                let Value::Object(go) = groups else { unreachable!() };
                let nk = vm.key(name);
                vm.create_data_property_or_throw(go, nk, captured)?;
                group_names.push(Some(name.to_string()));
            }
        } else {
            group_names.push(None);
        }
    }
    if has_indices {
        let indices = make_match_indices_index_pair_array(vm, &caps.ranges, &group_names, has_groups, m_start, e)?;
        let ik = PropKey::Str(vm.names.indices);
        vm.create_data_property_or_throw(a, ik, indices)?;
    }
    Ok(Value::Object(a))
}

/// MakeMatchIndicesIndexPairArray(S, indices, groupNames, hasGroups).
fn make_match_indices_index_pair_array(
    vm: &mut Vm,
    ranges: &[Option<(usize, usize)>],
    group_names: &[Option<String>],
    has_groups: bool,
    m_start: usize,
    m_end: usize,
) -> JsResult<Value> {
    let n = ranges.len();
    let a = vm.array_create(n as f64, None)?;
    let groups = if has_groups {
        Value::Object(vm.new_object_with_proto(None))
    } else {
        Value::Undefined
    };
    let g_key = PropKey::Str(vm.names.groups);
    vm.create_data_property_or_throw(a, g_key, groups)?;
    for i in 0..n {
        let r = if i == 0 { Some((m_start, m_end)) } else { ranges[i] };
        let pair = match r {
            Some((s, e)) => {
                let arr = vm.new_array(vec![Value::Number(s as f64), Value::Number(e as f64)]);
                Value::Object(arr)
            }
            None => Value::Undefined,
        };
        vm.create_data_property_or_throw(a, PropKey::Index(i as u32), pair)?;
        if i > 0 {
            if let Some(name) = group_names[i - 1].as_deref() {
                let Value::Object(go) = groups else { unreachable!() };
                let nk = vm.key(name);
                vm.create_data_property_or_throw(go, nk, pair)?;
            }
        }
    }
    Ok(Value::Object(a))
}

// ---- accessors ---------------------------------------------------------------------------

fn flags_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_regexp_object(vm, a.this, "RegExp.prototype.flags getter")?;
    let props: [(StrId, u16); 8] = [
        (vm.names.hasIndices, b'd' as u16),
        (vm.names.global, b'g' as u16),
        (vm.names.ignoreCase, b'i' as u16),
        (vm.names.multiline, b'm' as u16),
        (vm.names.dotAll, b's' as u16),
        (vm.names.unicode, b'u' as u16),
        (vm.names.unicodeSets, b'v' as u16),
        (vm.names.sticky, b'y' as u16),
    ];
    let mut out = Vec::with_capacity(8);
    for (p, c) in props {
        let v = vm.get(r, PropKey::Str(p))?;
        if vm.to_boolean(v) {
            out.push(c);
        }
    }
    Ok(vm.str_from_units(out))
}

fn flag_getter(vm: &mut Vm, this: Value, name: &str, pick: fn(&Flags) -> bool) -> JsResult<Value> {
    let r = this_regexp_object(vm, this, &format!("RegExp.prototype.{name} getter"))?;
    match &vm.heap.obj(r).class {
        Class::RegExp(d) => Ok(Value::Bool(pick(&d.parsed))),
        _ => {
            if r == vm.intrinsic(Intrinsic::RegExpPrototype) {
                Ok(Value::Undefined)
            } else {
                vm.type_error(&format!("RegExp.prototype.{name} getter called on incompatible receiver"))
            }
        }
    }
}

fn get_dot_all(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "dotAll", |f| f.dot_all)
}
fn get_global(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "global", |f| f.global)
}
fn get_has_indices(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "hasIndices", |f| f.has_indices)
}
fn get_ignore_case(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "ignoreCase", |f| f.ignore_case)
}
fn get_multiline(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "multiline", |f| f.multiline)
}
fn get_sticky(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "sticky", |f| f.sticky)
}
fn get_unicode(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "unicode", |f| f.unicode)
}
fn get_unicode_sets(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    flag_getter(vm, a.this, "unicodeSets", |f| f.unicode_sets)
}

fn source_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_regexp_object(vm, a.this, "RegExp.prototype.source getter")?;
    let src = match &vm.heap.obj(r).class {
        Class::RegExp(d) => d.source,
        _ => {
            if r == vm.intrinsic(Intrinsic::RegExpPrototype) {
                return Ok(vm.str("(?:)"));
            }
            return vm.type_error("RegExp.prototype.source getter called on incompatible receiver");
        }
    };
    let units = vm.heap.units(src).to_vec();
    Ok(vm.str_from_units(escape_regexp_pattern(&units)))
}

/// EscapeRegExpPattern: a source text that re-parses to the same pattern
/// when placed between slashes. `/` outside a class is escaped, line
/// terminators become escapes, and the empty pattern reads `(?:)`.
fn escape_regexp_pattern(src: &[u16]) -> Vec<u16> {
    if src.is_empty() {
        return "(?:)".encode_utf16().collect();
    }
    let mut out = Vec::with_capacity(src.len() + 4);
    let mut in_class = false;
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        match c {
            0x5C => {
                // Backslash: copy the escape (and a line terminator after
                // it as an escape too, which keeps `\` + newline parseable).
                out.push(c);
                if let Some(&n) = src.get(i + 1) {
                    match n {
                        0x0A => out.extend("\\n".encode_utf16()),
                        0x0D => out.extend("\\r".encode_utf16()),
                        0x2028 => out.extend("\\u2028".encode_utf16()),
                        0x2029 => out.extend("\\u2029".encode_utf16()),
                        _ => out.push(n),
                    }
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            0x5B => in_class = true,
            0x5D => in_class = false,
            0x2F if !in_class => {
                out.extend("\\/".encode_utf16());
                i += 1;
                continue;
            }
            0x0A => {
                out.extend("\\n".encode_utf16());
                i += 1;
                continue;
            }
            0x0D => {
                out.extend("\\r".encode_utf16());
                i += 1;
                continue;
            }
            0x2028 => {
                out.extend("\\u2028".encode_utf16());
                i += 1;
                continue;
            }
            0x2029 => {
                out.extend("\\u2029".encode_utf16());
                i += 1;
                continue;
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    out
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_regexp_object(vm, a.this, "RegExp.prototype.toString")?;
    let sk = PropKey::Str(vm.names.source);
    let p = vm.get(r, sk)?;
    let p = vm.to_string(p)?;
    let fk = PropKey::Str(vm.names.flags);
    let f = vm.get(r, fk)?;
    let f = vm.to_string(f)?;
    let mut out = vec![b'/' as u16];
    out.extend(vm.heap.units(p).iter());
    out.push(b'/' as u16);
    out.extend(vm.heap.units(f).iter());
    Ok(vm.str_from_units(out))
}

/// Annex B `RegExp.prototype.compile(pattern, flags)`.
fn proto_compile(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some(o) = a.this.as_object().filter(|o| matches!(vm.heap.obj(*o).class, Class::RegExp(_))) else {
        return vm.type_error("RegExp.prototype.compile called on incompatible receiver");
    };
    let pattern = a.arg(0);
    let flags = a.arg(1);
    let (p, f) = if let Some((src, fl, _, _)) = regexp_data(vm, pattern) {
        if !flags.is_undefined() {
            return vm.type_error("RegExp.prototype.compile: flags must be undefined when pattern is a RegExp");
        }
        (Value::String(src), Value::String(fl))
    } else {
        (pattern, flags)
    };
    regexp_initialize(vm, o, p, f)
}

// ---- symbol protocol ---------------------------------------------------------------------

fn flags_of(vm: &mut Vm, rx: ObjId) -> JsResult<Vec<u16>> {
    let fk = PropKey::Str(vm.names.flags);
    let f = vm.get(rx, fk)?;
    let f = vm.to_string(f)?;
    Ok(vm.heap.units(f).to_vec())
}

fn has_flag(flags: &[u16], c: u8) -> bool {
    flags.contains(&(c as u16))
}

fn get_match_string(vm: &mut Vm, result: ObjId) -> JsResult<StrId> {
    let v = vm.get(result, PropKey::Index(0))?;
    vm.to_string(v)
}

fn get_last_index(vm: &mut Vm, rx: ObjId) -> JsResult<f64> {
    let key = PropKey::Str(vm.names.lastIndex);
    let v = vm.get(rx, key)?;
    vm.to_length(v)
}

fn set_last_index(vm: &mut Vm, rx: ObjId, v: f64) -> JsResult<()> {
    let key = PropKey::Str(vm.names.lastIndex);
    vm.set(rx, key, Value::Number(v), true)
}

fn symbol_match(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.match]")?;
    let s = vm.to_string(a.arg(0))?;
    let flags = flags_of(vm, rx)?;
    if !has_flag(&flags, b'g') {
        return regexp_exec(vm, rx, s);
    }
    let full_unicode = has_flag(&flags, b'u') || has_flag(&flags, b'v');
    set_last_index(vm, rx, 0.0)?;
    let arr = vm.array_create(0.0, None)?;
    let mut n: u32 = 0;
    loop {
        let result = regexp_exec(vm, rx, s)?;
        let Value::Object(ro) = result else {
            return Ok(if n == 0 { Value::Null } else { Value::Object(arr) });
        };
        let match_str = get_match_string(vm, ro)?;
        vm.create_data_property_or_throw(arr, PropKey::Index(n), Value::String(match_str))?;
        if vm.heap.str_len(match_str) == 0 {
            let this_index = get_last_index(vm, rx)?;
            let units = input_units(vm, s);
            let next = advance_index_f64(&units, this_index, full_unicode);
            set_last_index(vm, rx, next)?;
        }
        n += 1;
    }
}

fn advance_index_f64(units: &[u16], index: f64, unicode: bool) -> f64 {
    if index >= units.len() as f64 {
        return index + 1.0;
    }
    advance_string_index(units, index as usize, unicode) as f64
}

fn symbol_match_all(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.matchAll]")?;
    let s = vm.to_string(a.arg(0))?;
    let c = vm.species_constructor(r, Intrinsic::RegExp)?;
    let flags = flags_of(vm, r)?;
    let flags_v = vm.str_from_units(flags.clone());
    let matcher = vm.construct_value(c, &[Value::Object(r), flags_v], c)?;
    let Value::Object(mo) = matcher else { unreachable!("Construct returns an object") };
    let last_index = get_last_index(vm, r)?;
    set_last_index(vm, mo, last_index)?;
    let global = has_flag(&flags, b'g');
    let full_unicode = has_flag(&flags, b'u') || has_flag(&flags, b'v');
    let proto = vm.intrinsic(Intrinsic::RegExpStringIteratorPrototype);
    let state = RegExpIterState {
        regexp: matcher,
        string: Value::String(s),
        global,
        unicode: full_unicode,
        done: false,
    };
    Ok(Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::RegExpStringIterator(Box::new(state)),
    ))))
}

fn regexp_string_iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some(o) = a.this.as_object() else {
        return vm.type_error("%RegExpStringIteratorPrototype%.next called on incompatible receiver");
    };
    let (regexp, string, global, unicode, done) = match &vm.heap.obj(o).class {
        Class::RegExpStringIterator(st) => (st.regexp, st.string, st.global, st.unicode, st.done),
        _ => {
            return vm.type_error("%RegExpStringIteratorPrototype%.next called on incompatible receiver")
        }
    };
    if done {
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
    let Value::Object(r) = regexp else { unreachable!() };
    let Value::String(s) = string else { unreachable!() };
    let m = regexp_exec(vm, r, s)?;
    let Value::Object(mo) = m else {
        mark_done(vm, o);
        return Ok(vm.create_iter_result(Value::Undefined, true));
    };
    if !global {
        mark_done(vm, o);
        return Ok(vm.create_iter_result(m, false));
    }
    let match_str = get_match_string(vm, mo)?;
    if vm.heap.str_len(match_str) == 0 {
        let this_index = get_last_index(vm, r)?;
        let units = input_units(vm, s);
        let next = advance_index_f64(&units, this_index, unicode);
        set_last_index(vm, r, next)?;
    }
    Ok(vm.create_iter_result(m, false))
}

fn mark_done(vm: &mut Vm, o: ObjId) {
    if let Class::RegExpStringIterator(st) = &mut vm.heap.obj_mut(o).class {
        st.done = true;
    }
}

fn symbol_replace(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.replace]")?;
    let s = vm.to_string(a.arg(0))?;
    let su = input_units(vm, s);
    let length_s = su.len();
    let mut replace_value = a.arg(1);
    let functional = vm.is_callable(replace_value);
    if !functional {
        let rs = vm.to_string(replace_value)?;
        replace_value = Value::String(rs);
    }
    let flags = flags_of(vm, rx)?;
    let global = has_flag(&flags, b'g');
    let mut full_unicode = false;
    if global {
        full_unicode = has_flag(&flags, b'u') || has_flag(&flags, b'v');
        set_last_index(vm, rx, 0.0)?;
    }
    let mut results: Vec<ObjId> = Vec::new();
    loop {
        let result = regexp_exec(vm, rx, s)?;
        let Value::Object(ro) = result else { break };
        results.push(ro);
        if !global {
            break;
        }
        let match_str = get_match_string(vm, ro)?;
        if vm.heap.str_len(match_str) == 0 {
            let this_index = get_last_index(vm, rx)?;
            let next = advance_index_f64(&su, this_index, full_unicode);
            set_last_index(vm, rx, next)?;
        }
    }
    let mut accumulated: Vec<u16> = Vec::with_capacity(length_s);
    let mut next_source_position = 0usize;
    for result in results {
        let result_length = vm.length_of_array_like(result)?;
        let n_captures = (result_length - 1.0).max(0.0) as usize;
        let matched = get_match_string(vm, result)?;
        let matched_u = vm.heap.units(matched).to_vec();
        let match_length = matched_u.len();
        let idx_key = PropKey::Str(vm.names.index);
        let pos_v = vm.get(result, idx_key)?;
        let position = vm.to_integer_or_infinity(pos_v)?;
        let position = position.max(0.0).min(length_s as f64) as usize;
        let mut captures: Vec<Value> = Vec::with_capacity(n_captures);
        for n in 1..=n_captures {
            let mut cap = vm.get(result, PropKey::Index(n as u32))?;
            if !cap.is_undefined() {
                cap = Value::String(vm.to_string(cap)?);
            }
            captures.push(cap);
        }
        let g_key = PropKey::Str(vm.names.groups);
        let named_captures = vm.get(result, g_key)?;
        let replacement: Vec<u16> = if functional {
            let mut args: Vec<Value> = Vec::with_capacity(n_captures + 4);
            args.push(Value::String(matched));
            args.extend(captures.iter().copied());
            args.push(Value::Number(position as f64));
            args.push(Value::String(s));
            if !named_captures.is_undefined() {
                args.push(named_captures);
            }
            let repl = vm.call_value(replace_value, Value::Undefined, &args)?;
            let rs = vm.to_string(repl)?;
            vm.heap.units(rs).to_vec()
        } else {
            let named = if named_captures.is_undefined() {
                Value::Undefined
            } else {
                Value::Object(vm.to_object(named_captures)?)
            };
            let Value::String(tmpl) = replace_value else { unreachable!() };
            let tu = vm.heap.units(tmpl).to_vec();
            super::string::get_substitution(vm, &matched_u, &su, position, &captures, named, &tu)?
        };
        if position >= next_source_position {
            accumulated.extend_from_slice(&su[next_source_position..position]);
            accumulated.extend_from_slice(&replacement);
            next_source_position = position + match_length;
        }
    }
    if next_source_position < length_s {
        accumulated.extend_from_slice(&su[next_source_position..]);
    }
    Ok(vm.str_from_units(accumulated))
}

fn symbol_search(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.search]")?;
    let s = vm.to_string(a.arg(0))?;
    let key = PropKey::Str(vm.names.lastIndex);
    let previous = vm.get(rx, key)?;
    if !vm.same_value(previous, Value::Number(0.0)) {
        vm.set(rx, key, Value::Number(0.0), true)?;
    }
    let result = regexp_exec(vm, rx, s)?;
    let current = vm.get(rx, key)?;
    if !vm.same_value(current, previous) {
        vm.set(rx, key, previous, true)?;
    }
    match result {
        Value::Object(ro) => {
            let ik = PropKey::Str(vm.names.index);
            vm.get(ro, ik)
        }
        _ => Ok(Value::Number(-1.0)),
    }
}

fn symbol_split(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.split]")?;
    let s = vm.to_string(a.arg(0))?;
    let su = input_units(vm, s);
    let size = su.len();
    let c = vm.species_constructor(rx, Intrinsic::RegExp)?;
    let flags = flags_of(vm, rx)?;
    let unicode_matching = has_flag(&flags, b'u') || has_flag(&flags, b'v');
    let mut new_flags = flags.clone();
    if !has_flag(&flags, b'y') {
        new_flags.push(b'y' as u16);
    }
    let nf = vm.str_from_units(new_flags);
    let splitter = vm.construct_value(c, &[Value::Object(rx), nf], c)?;
    let Value::Object(sp) = splitter else { unreachable!() };
    let arr = vm.array_create(0.0, None)?;
    let mut length_a: u32 = 0;
    let limit = a.arg(1);
    let lim: u32 = if limit.is_undefined() {
        u32::MAX
    } else {
        vm.to_uint32(limit)?
    };
    if lim == 0 {
        return Ok(Value::Object(arr));
    }
    if size == 0 {
        let z = regexp_exec(vm, sp, s)?;
        if !z.is_null() {
            return Ok(Value::Object(arr));
        }
        vm.create_data_property_or_throw(arr, PropKey::Index(0), Value::String(s))?;
        return Ok(Value::Object(arr));
    }
    let mut p = 0usize;
    let mut q = p;
    while q < size {
        set_last_index(vm, sp, q as f64)?;
        let z = regexp_exec(vm, sp, s)?;
        let Value::Object(zo) = z else {
            q = advance_string_index(&su, q, unicode_matching);
            continue;
        };
        let e = get_last_index(vm, sp)?;
        let e = (e.min(size as f64)) as usize;
        if e == p {
            q = advance_string_index(&su, q, unicode_matching);
            continue;
        }
        let t = vm.str_from_units(su[p..q].to_vec());
        vm.create_data_property_or_throw(arr, PropKey::Index(length_a), t)?;
        length_a += 1;
        if length_a == lim {
            return Ok(Value::Object(arr));
        }
        p = e;
        let number_of_captures = vm.length_of_array_like(zo)?;
        let number_of_captures = (number_of_captures - 1.0).max(0.0) as usize;
        for i in 1..=number_of_captures {
            let next_capture = vm.get(zo, PropKey::Index(i as u32))?;
            vm.create_data_property_or_throw(arr, PropKey::Index(length_a), next_capture)?;
            length_a += 1;
            if length_a == lim {
                return Ok(Value::Object(arr));
            }
        }
        q = p;
    }
    let t = vm.str_from_units(su[p..size].to_vec());
    vm.create_data_property_or_throw(arr, PropKey::Index(length_a), t)?;
    Ok(Value::Object(arr))
}
