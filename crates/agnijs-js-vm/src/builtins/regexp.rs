//! RegExp objects (ES2025 §22.2): the constructor, `%RegExp.prototype%`,
//! the `@@match` / `@@matchAll` / `@@replace` / `@@search` / `@@split`
//! protocol, `%RegExpStringIteratorPrototype%`, `RegExp.escape` and the
//! Annex B `compile`. Matching itself goes through the `agnijs-js-regex`
//! seam (ADR 0013); this module owns the ECMAScript algorithms around it.

use std::rc::Rc;

use agnijs_engine_api::RealmId;
use agnijs_js_regex::{Captures, Flags, Regex};

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, NativeTag, ObjectData};
use crate::realm::Intrinsic;
use crate::string::{StrBuilder, Units};
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

/// Compiled matchers by (pattern, flags). A lookup hashes the pattern's
/// code units in place, so a hit allocates nothing whatever the string's
/// storage width.
#[derive(Default)]
pub struct RegexCache {
    table: hashbrown::HashTable<CacheEntry>,
}

struct CacheEntry {
    pattern: Vec<u16>,
    flags: u8,
    hash: u64,
    re: Rc<Regex>,
}

impl RegexCache {
    fn hash(pattern: &Units<'_>, flags: u8) -> u64 {
        pattern.hash() ^ (flags as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    fn get(&self, pattern: &Units<'_>, flags: u8) -> Option<&Rc<Regex>> {
        let h = Self::hash(pattern, flags);
        self.table
            .find(h, |e| e.flags == flags && e.pattern.len() == pattern.len() && pattern.eq_units(&e.pattern))
            .map(|e| &e.re)
    }

    fn insert(&mut self, pattern: Vec<u16>, flags: u8, re: Rc<Regex>) {
        if self.table.len() >= CACHE_LIMIT {
            self.table.clear();
        }
        let hash = Self::hash(&Units::Two(&pattern), flags);
        self.table.insert_unique(hash, CacheEntry { pattern, flags, hash, re }, |e| e.hash);
    }

    /// Add a matcher compiled ahead of time (a regex literal's program
    /// from a bytecode container, ADR 0010 addendum), unless the pattern
    /// is already cached.
    pub(crate) fn seed(&mut self, pattern: Vec<u16>, flags: u8, re: Rc<Regex>) {
        if self.get(&Units::Two(&pattern), flags).is_none() {
            self.insert(pattern, flags, re);
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

/// The flags as eight bits, in `dgimsuvy` order.
pub(crate) fn flag_bits(f: &Flags) -> u8 {
    [f.has_indices, f.global, f.ignore_case, f.multiline, f.dot_all, f.unicode, f.unicode_sets, f.sticky]
        .iter()
        .enumerate()
        .fold(0, |bits, (i, &on)| bits | ((on as u8) << i))
}

// ---- creation ----------------------------------------------------------------------------

/// Compile `pattern` under `flags`, sharing compiled matchers between
/// RegExp objects with the same source and flags (regex literals in a loop
/// body create a fresh object per evaluation; the matcher is immutable, so
/// one compilation serves them all).
pub(crate) fn compile(vm: &mut Vm, pattern: StrId, flags: StrId) -> JsResult<(Rc<Regex>, Flags)> {
    // A valid flags string has at most eight units; a longer one is parsed
    // from a copy, for the same error.
    let parsed = {
        let fu = vm.heap.units(flags);
        let mut buf = [0u16; 16];
        let parsed = if fu.len() <= buf.len() {
            for (i, c) in fu.iter().enumerate() {
                buf[i] = c;
            }
            Flags::parse(&buf[..fu.len()])
        } else {
            Flags::parse(&fu.to_vec())
        };
        match parsed {
            Ok(f) => f,
            Err(msg) => return vm.syntax_error(&msg),
        }
    };
    let bits = flag_bits(&parsed);
    if let Some(r) = vm.regex_cache.get(&vm.heap.units(pattern), bits) {
        return Ok((r.clone(), parsed));
    }
    let p_units = vm.heap.units(pattern).to_vec();
    let re = match Regex::compile_with(&p_units, &parsed, vm.regex_mode) {
        Ok(r) => Rc::new(r),
        Err(msg) => return vm.syntax_error(&msg),
    };
    vm.regex_cache.insert(p_units, bits, re.clone());
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
    if let Some(r) = vm.regex_cache.get(&Units::One(&[]), 0) {
        return r.clone();
    }
    let re = Rc::new(Regex::compile(&[], &Flags::default()).expect("empty pattern compiles"));
    vm.regex_cache.insert(Vec::new(), 0, re.clone());
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

/// Run `f` over the UTF-16 code units of `s`, the form the matcher reads.
///
/// Two-byte text is read in place. One-byte text is widened: a short
/// string into a buffer reused between execs, a long one into a small
/// most-recent-first cache shared across the repeated execs a global
/// operation or a sticky tokenizer performs on it (so those stay linear).
/// Short strings never enter the cache, so matching a word between two
/// execs on a long line does not evict the line (that made a tokenizer
/// quadratic).
fn with_input<R>(vm: &mut Vm, s: StrId, f: impl FnOnce(&[u16]) -> R) -> R {
    const SHORT: usize = 256;
    const ENTRIES: usize = 4;
    vm.heap.flatten(s);
    let len = match vm.heap.units_flat(s) {
        Units::Two(u) => return f(u),
        Units::One(b) => b.len(),
    };
    if len < SHORT {
        let mut buf = std::mem::take(&mut vm.regex_scratch);
        buf.clear();
        vm.heap.units_flat(s).extend_into(&mut buf);
        let r = f(&buf);
        vm.regex_scratch = buf;
        return r;
    }
    let u = if let Some(i) = vm.regex_input_cache.iter().position(|(id, _)| *id == s) {
        let hit = vm.regex_input_cache.remove(i);
        let u = hit.1.clone();
        vm.regex_input_cache.insert(0, hit);
        u
    } else {
        let u = Rc::new(vm.heap.units_flat(s).to_vec());
        vm.regex_input_cache.truncate(ENTRIES - 1);
        vm.regex_input_cache.insert(0, (s, u.clone()));
        u
    };
    f(&u)
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

    let exec = vm.method(proto, "exec", 1, proto_exec);
    if let Some(n) = vm.heap.obj_mut(exec).native_mut() {
        n.tag = NativeTag::RegExpExec;
    }
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
    vm.str_from_units(out)
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
pub fn advance_string_index(units: &Units<'_>, index: usize, unicode: bool) -> usize {
    if !unicode || index + 1 >= units.len() {
        return index + 1;
    }
    index + units.code_point_at(index).1
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
    let m = regexp_exec_match(vm, r, s)?;
    Ok(Value::Bool(!matches!(m, Exec::Null)))
}

/// RegExpExec(R, S).
pub fn regexp_exec(vm: &mut Vm, r: ObjId, s: StrId) -> JsResult<Value> {
    let key = PropKey::Str(vm.names.exec);
    let exec = vm.get(r, key)?;
    call_exec(vm, r, s, exec)
}

/// RegExpExec(R, S) after its Get of `exec`.
fn call_exec(vm: &mut Vm, r: ObjId, s: StrId, exec: Value) -> JsResult<Value> {
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

/// What RegExpExec(R, S) gave a caller that only reads the match.
enum Exec {
    /// No match.
    Null,
    /// The built-in exec matched: the `index` and the ranges its result
    /// array would hold (the match, then each capture), the array not
    /// built.
    Raw(usize, Captures),
    /// A result object: another exec's, or the built-in one's for a
    /// pattern with named groups or the d flag.
    Object(ObjId),
}

/// Whether `f` is a built-in `RegExp.prototype.exec` (of any realm).
fn is_builtin_exec(vm: &Vm, f: Value) -> bool {
    let Value::Object(fo) = f else { return false };
    matches!(&vm.heap.obj(fo).class, Class::Native(n) if n.tag == NativeTag::RegExpExec)
}

/// RegExpExec(R, S) for a caller that reads the result's match, index and
/// captures and then drops it. With the built-in exec on a RegExp the
/// result array is not built: nothing but the caller could reach it, and
/// reading its own data properties runs no code, so leaving it out is not
/// observable. lastIndex is read and written as the spec says.
fn regexp_exec_match(vm: &mut Vm, r: ObjId, s: StrId) -> JsResult<Exec> {
    let key = PropKey::Str(vm.names.exec);
    let exec = vm.get(r, key)?;
    if is_builtin_exec(vm, exec) && matches!(vm.heap.obj(r).class, Class::RegExp(_)) {
        let Some((index, caps, matcher, flags)) = builtin_exec_raw(vm, r, s)? else {
            return Ok(Exec::Null);
        };
        if flags.has_indices || matcher.has_named_groups() {
            return Ok(Exec::Object(build_match_array(vm, s, index, caps, &matcher, flags.has_indices)?));
        }
        return Ok(Exec::Raw(index, caps));
    }
    Ok(match call_exec(vm, r, s, exec)? {
        Value::Object(o) => Exec::Object(o),
        _ => Exec::Null,
    })
}

/// RegExpBuiltinExec(R, S).
fn regexp_builtin_exec(vm: &mut Vm, r: ObjId, s: StrId) -> JsResult<Value> {
    match builtin_exec_raw(vm, r, s)? {
        None => Ok(Value::Null),
        Some((index, caps, matcher, flags)) => Ok(Value::Object(build_match_array(
            vm,
            s,
            index,
            caps,
            &matcher,
            flags.has_indices,
        )?)),
    }
}

/// RegExpBuiltinExec(R, S) up to building the result array: the match's
/// `index` (lastIndex itself when that pointed into the surrogate pair the
/// match starts at), its ranges, and the matcher and flags it ran under.
#[allow(clippy::type_complexity)]
fn builtin_exec_raw(
    vm: &mut Vm,
    r: ObjId,
    s: StrId,
) -> JsResult<Option<(usize, Captures, Rc<Regex>, Flags)>> {
    let length = vm.heap.str_len(s);
    let li_key = PropKey::Str(vm.names.lastIndex);
    let li_val = vm.get(r, li_key)?;
    let mut last_index = vm.to_length(li_val)?;
    let (matcher, flags) = match &vm.heap.obj(r).class {
        Class::RegExp(d) => (d.matcher.clone(), d.parsed),
        _ => unreachable!(),
    };
    let global = flags.global;
    let sticky = flags.sticky;
    let full_unicode = flags.unicode_mode();
    if !global && !sticky {
        last_index = 0.0;
    }
    if last_index > length as f64 {
        if global || sticky {
            vm.set(r, li_key, Value::Number(0.0), true)?;
        }
        return Ok(None);
    }
    let last_index = last_index as usize;
    let (attempt, found) = with_input(vm, s, |input| {
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
            matcher.exec_sticky(input, attempt)
        } else {
            matcher.exec(input, attempt)
        };
        (attempt, found)
    });
    let Some(caps) = found else {
        if global || sticky {
            vm.set(r, li_key, Value::Number(0.0), true)?;
        }
        return Ok(None);
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
    let mut caps = caps;
    if e < m_start {
        // An empty match at the start of the pair lastIndex pointed into
        // ends before lastIndex: the matched string is empty (it was a
        // panic slicing backwards).
        caps.ranges[0] = Some((found_start, m_start));
    }
    Ok(Some((m_start, caps, matcher, flags)))
}

/// The result array of RegExpBuiltinExec. Its properties are made in a
/// different order from the spec's (the elements first), which no code
/// can see: the array is new, and the order of its keys is the same
/// (integer keys first, then `index`, `input`, `groups`).
fn build_match_array(
    vm: &mut Vm,
    s: StrId,
    m_start: usize,
    caps: Captures,
    matcher: &Regex,
    has_indices: bool,
) -> JsResult<ObjId> {
    let (_, e) = caps.ranges[0].expect("whole match");
    let n = matcher.group_count();
    let mut items = Vec::with_capacity(n + 1);
    items.push(Value::String(vm.heap.substring(s, m_start, e)));
    for i in 1..=n {
        items.push(match caps.ranges.get(i).copied().flatten() {
            Some((cs, ce)) => Value::String(vm.heap.substring(s, cs, ce)),
            None => Value::Undefined,
        });
    }
    let names = matcher.group_names();
    let has_groups = matcher.has_named_groups();
    // The group names MakeMatchIndicesIndexPairArray uses: a name repeated
    // after a group of that name matched is left out.
    let mut index_names: Vec<Option<String>> = Vec::new();
    let groups = if has_groups {
        let go = vm.new_object_with_proto(None);
        let mut matched_group_names: Vec<&str> = Vec::new();
        for i in 1..=n {
            let Some(name) = names[i - 1].as_deref() else {
                index_names.push(None);
                continue;
            };
            if matched_group_names.contains(&name) {
                index_names.push(None);
                continue;
            }
            let captured = items[i];
            if !captured.is_undefined() {
                matched_group_names.push(name);
            }
            let nk = vm.key(name);
            vm.create_data_property_or_throw(go, nk, captured)?;
            if has_indices {
                index_names.push(Some(name.to_string()));
            }
        }
        Value::Object(go)
    } else {
        index_names.resize(n, None);
        Value::Undefined
    };
    let a = vm.new_array(items);
    let idx_key = PropKey::Str(vm.names.index);
    vm.create_data_property_or_throw(a, idx_key, Value::Number(m_start as f64))?;
    let in_key = PropKey::Str(vm.names.input);
    vm.create_data_property_or_throw(a, in_key, Value::String(s))?;
    let g_key = PropKey::Str(vm.names.groups);
    vm.create_data_property_or_throw(a, g_key, groups)?;
    if has_indices {
        let indices = make_match_indices_index_pair_array(vm, &caps.ranges, &index_names, has_groups, m_start, e)?;
        let ik = PropKey::Str(vm.names.indices);
        vm.create_data_property_or_throw(a, ik, indices)?;
    }
    Ok(a)
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
    let mut out = [0u8; 8];
    let mut n = 0;
    for (p, c) in props {
        let v = vm.get(r, PropKey::Str(p))?;
        if vm.to_boolean(v) {
            out[n] = c as u8;
            n += 1;
        }
    }
    // One of at most 256 short strings: the interned one, not a new one.
    Ok(Value::String(vm.heap.atom_units(Units::One(&out[..n]))))
}

fn flag_getter(vm: &mut Vm, this: Value, name: &str, pick: fn(&Flags) -> bool) -> JsResult<Value> {
    let Value::Object(r) = this else {
        return vm.type_error(&format!("RegExp.prototype.{name} getter called on a non-object"));
    };
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
    vm.str_from_units(escape_regexp_pattern(&units))
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
    vm.str_from_units(out)
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

/// ToString(Get(rx, "flags")).
fn flags_of(vm: &mut Vm, rx: ObjId) -> JsResult<StrId> {
    let fk = PropKey::Str(vm.names.flags);
    let f = vm.get(rx, fk)?;
    vm.to_string(f)
}

fn has_flag(vm: &mut Vm, flags: StrId, c: u8) -> bool {
    vm.heap.units(flags).iter().any(|u| u == c as u16)
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

/// The end of a raw match (its `index` plus the matched string's length).
fn raw_end(caps: &Captures) -> usize {
    caps.ranges[0].expect("whole match").1
}

/// The capture strings of a raw match (undefined for a group that did not
/// participate), as the result array would hold them.
fn raw_captures(vm: &mut Vm, s: StrId, caps: &Captures) -> Vec<Value> {
    caps.ranges[1..]
        .iter()
        .map(|r| match r {
            Some((cs, ce)) => Value::String(vm.heap.substring(s, *cs, *ce)),
            None => Value::Undefined,
        })
        .collect()
}

/// After an empty match of a global or sticky search: lastIndex =
/// AdvanceStringIndex(S, ToLength(Get(rx, "lastIndex")), fullUnicode).
fn step_past_empty(vm: &mut Vm, rx: ObjId, s: StrId, full_unicode: bool) -> JsResult<()> {
    let this_index = get_last_index(vm, rx)?;
    let next = advance_index_f64(&vm.heap.units(s), this_index, full_unicode);
    set_last_index(vm, rx, next)
}

fn symbol_match(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.match]")?;
    let s = vm.to_string(a.arg(0))?;
    let flags = flags_of(vm, rx)?;
    if !has_flag(vm, flags, b'g') {
        return regexp_exec(vm, rx, s);
    }
    let full_unicode = has_flag(vm, flags, b'u') || has_flag(vm, flags, b'v');
    set_last_index(vm, rx, 0.0)?;
    let arr = vm.array_create(0.0, None)?;
    let mut n: u32 = 0;
    loop {
        let match_str = match regexp_exec_match(vm, rx, s)? {
            Exec::Null => return Ok(if n == 0 { Value::Null } else { Value::Object(arr) }),
            Exec::Raw(index, caps) => vm.heap.substring(s, index, raw_end(&caps)),
            Exec::Object(ro) => get_match_string(vm, ro)?,
        };
        vm.create_data_property_or_throw(arr, PropKey::Index(n), Value::String(match_str))?;
        if vm.heap.str_len(match_str) == 0 {
            step_past_empty(vm, rx, s, full_unicode)?;
        }
        n += 1;
    }
}

fn advance_index_f64(units: &Units<'_>, index: f64, unicode: bool) -> f64 {
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
    let matcher = vm.construct_value(c, &[Value::Object(r), Value::String(flags)], c)?;
    let Value::Object(mo) = matcher else { unreachable!("Construct returns an object") };
    let last_index = get_last_index(vm, r)?;
    set_last_index(vm, mo, last_index)?;
    let global = has_flag(vm, flags, b'g');
    let full_unicode = has_flag(vm, flags, b'u') || has_flag(vm, flags, b'v');
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
        step_past_empty(vm, r, s, unicode)?;
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
    let length_s = vm.heap.str_len(s);
    let mut replace_value = a.arg(1);
    let functional = vm.is_callable(replace_value);
    if !functional {
        let rs = vm.to_string(replace_value)?;
        replace_value = Value::String(rs);
    }
    let flags = flags_of(vm, rx)?;
    let global = has_flag(vm, flags, b'g');
    let mut full_unicode = false;
    if global {
        full_unicode = has_flag(vm, flags, b'u') || has_flag(vm, flags, b'v');
        set_last_index(vm, rx, 0.0)?;
    }
    let mut results: Vec<Exec> = Vec::new();
    loop {
        let result = regexp_exec_match(vm, rx, s)?;
        if matches!(result, Exec::Null) {
            break;
        }
        results.push(result);
        if !global {
            break;
        }
        let empty = match results.last() {
            Some(Exec::Raw(index, caps)) => raw_end(caps) == *index,
            Some(&Exec::Object(ro)) => {
                let match_str = get_match_string(vm, ro)?;
                vm.heap.str_len(match_str) == 0
            }
            _ => unreachable!(),
        };
        if empty {
            step_past_empty(vm, rx, s, full_unicode)?;
        }
    }
    // A template without `$` is its own expansion.
    let plain_template = match replace_value {
        Value::String(t) if !functional => !vm.heap.units(t).iter().any(|c| c == 0x24),
        _ => false,
    };
    let mut accumulated = StrBuilder::with_capacity(length_s);
    let mut next_source_position = 0usize;
    for result in results {
        // The match, its position and length, and (unless the template is
        // plain) its captures and named captures, read as the spec reads
        // them from the result object.
        let (matched, position, match_length, captures, named_captures) = match result {
            Exec::Null => unreachable!(),
            Exec::Raw(index, caps) => {
                let end = raw_end(&caps);
                let (matched, captures) = if plain_template {
                    (None, Vec::new())
                } else {
                    (Some(vm.heap.substring(s, index, end)), raw_captures(vm, s, &caps))
                };
                (matched, index, end - index, captures, Value::Undefined)
            }
            Exec::Object(result) => {
                let result_length = vm.length_of_array_like(result)?;
                let n_captures = (result_length - 1.0).max(0.0) as usize;
                let matched = get_match_string(vm, result)?;
                let match_length = vm.heap.str_len(matched);
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
                (Some(matched), position, match_length, captures, named_captures)
            }
        };
        // The text before the match goes in first and the replacement
        // after it; a replacement for a match behind nextSourcePosition is
        // computed (a callback still runs) and then taken back.
        let append = position >= next_source_position;
        if append {
            accumulated.push_units(&vm.heap.units(s).slice(next_source_position, position));
        }
        let mark = accumulated.len();
        if functional {
            let mut args: Vec<Value> = Vec::with_capacity(captures.len() + 4);
            args.push(Value::String(matched.expect("a callback sees the match")));
            args.extend(captures.iter().copied());
            args.push(Value::Number(position as f64));
            args.push(Value::String(s));
            if !named_captures.is_undefined() {
                args.push(named_captures);
            }
            let repl = vm.call_value(replace_value, Value::Undefined, &args)?;
            let rs = vm.to_string(repl)?;
            accumulated.push_units(&vm.heap.units(rs));
        } else {
            let Value::String(tmpl) = replace_value else { unreachable!() };
            if plain_template && named_captures.is_undefined() {
                accumulated.push_units(&vm.heap.units(tmpl));
            } else {
                let named = if named_captures.is_undefined() {
                    Value::Undefined
                } else {
                    Value::Object(vm.to_object(named_captures)?)
                };
                let matched = match matched {
                    Some(m) => m,
                    None => vm.heap.substring(s, position, position + match_length),
                };
                super::string::get_substitution(
                    vm,
                    matched,
                    s,
                    position,
                    &captures,
                    named,
                    tmpl,
                    &mut accumulated,
                )?;
            }
        }
        vm.check_string_len(accumulated.len())?;
        if append {
            next_source_position = position + match_length;
        } else {
            accumulated.truncate(mark);
        }
    }
    if next_source_position < length_s {
        accumulated.push_units(&vm.heap.units(s).slice(next_source_position, length_s));
    }
    vm.string_from_builder(accumulated)
}

fn symbol_search(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.search]")?;
    let s = vm.to_string(a.arg(0))?;
    let key = PropKey::Str(vm.names.lastIndex);
    let previous = vm.get(rx, key)?;
    if !vm.same_value(previous, Value::Number(0.0)) {
        vm.set(rx, key, Value::Number(0.0), true)?;
    }
    let result = regexp_exec_match(vm, rx, s)?;
    let current = vm.get(rx, key)?;
    if !vm.same_value(current, previous) {
        vm.set(rx, key, previous, true)?;
    }
    match result {
        Exec::Raw(index, _) => Ok(Value::Number(index as f64)),
        Exec::Object(ro) => {
            let ik = PropKey::Str(vm.names.index);
            vm.get(ro, ik)
        }
        Exec::Null => Ok(Value::Number(-1.0)),
    }
}

fn symbol_split(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rx = this_regexp_object(vm, a.this, "RegExp.prototype[Symbol.split]")?;
    let s = vm.to_string(a.arg(0))?;
    let size = vm.heap.str_len(s);
    let c = vm.species_constructor(rx, Intrinsic::RegExp)?;
    let flags = flags_of(vm, rx)?;
    let unicode_matching = has_flag(vm, flags, b'u') || has_flag(vm, flags, b'v');
    let new_flags = if has_flag(vm, flags, b'y') {
        flags
    } else {
        let y = vm.heap.char_str(b'y' as u16);
        vm.concat(flags, y)?
    };
    let splitter = vm.construct_value(c, &[Value::Object(rx), Value::String(new_flags)], c)?;
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
        let z = regexp_exec_match(vm, sp, s)?;
        if !matches!(z, Exec::Null) {
            return Ok(Value::Object(arr));
        }
        vm.create_data_property_or_throw(arr, PropKey::Index(0), Value::String(s))?;
        return Ok(Value::Object(arr));
    }
    let mut p = 0usize;
    let mut q = p;
    while q < size {
        set_last_index(vm, sp, q as f64)?;
        let z = regexp_exec_match(vm, sp, s)?;
        if matches!(z, Exec::Null) {
            q = advance_string_index(&vm.heap.units(s), q, unicode_matching);
            continue;
        }
        let e = get_last_index(vm, sp)?;
        let e = (e.min(size as f64)) as usize;
        if e == p {
            q = advance_string_index(&vm.heap.units(s), q, unicode_matching);
            continue;
        }
        let t = Value::String(vm.heap.substring(s, p, q));
        vm.create_data_property_or_throw(arr, PropKey::Index(length_a), t)?;
        length_a += 1;
        if length_a == lim {
            return Ok(Value::Object(arr));
        }
        p = e;
        match z {
            Exec::Raw(_, caps) => {
                for next_capture in raw_captures(vm, s, &caps) {
                    vm.create_data_property_or_throw(arr, PropKey::Index(length_a), next_capture)?;
                    length_a += 1;
                    if length_a == lim {
                        return Ok(Value::Object(arr));
                    }
                }
            }
            Exec::Object(zo) => {
                // Each capture is read just before it is stored, and none
                // after the limit is reached.
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
            }
            Exec::Null => unreachable!(),
        }
        q = p;
    }
    let t = Value::String(vm.heap.substring(s, p, size));
    vm.create_data_property_or_throw(arr, PropKey::Index(length_a), t)?;
    Ok(Value::Object(arr))
}
