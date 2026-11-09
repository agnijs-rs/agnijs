//! The string, regex and JSON built-ins that TASK-100.29 made allocate
//! less give the same results as before, edge cases included. Each case
//! is a script whose value is a string describing what it computed; it
//! must be the same on the AgniJS engine, on the AgniJS engine collecting
//! at every safe point (so a missing root shows up as a wrong or dead
//! value), and on QuickJS-NG, the reference engine.
//!
//! Cases whose answer QuickJS-NG gets wrong or does not support are in
//! `expected` with the spec's answer written out.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_engine_quickjs::QuickJsEngine;
use agnijs_js_vm::Vm;

/// `show(v)`: a description of `v` that uses none of the built-ins under
/// test beyond indexing and `charCodeAt`.
const PRELUDE: &str = r#"
function q(s) {
  var out = '"';
  for (var i = 0; i < s.length; i++) {
    var c = s.charCodeAt(i);
    if (c === 34 || c === 92) out += '\\' + s[i];
    else if (c < 32 || c > 126) out += '\\u{' + c.toString(16) + '}';
    else out += s[i];
  }
  return out + '"';
}
function show(v) {
  if (v === undefined) return 'undefined';
  if (v === null) return 'null';
  if (typeof v === 'string') return q(v);
  if (typeof v === 'number') return Object.is(v, -0) ? '-0' : String(v);
  if (typeof v !== 'object') return String(v);
  var keys = Object.getOwnPropertyNames(v), parts = [];
  for (var i = 0; i < keys.length; i++) {
    var d = Object.getOwnPropertyDescriptor(v, keys[i]);
    if (Array.isArray(v) && keys[i] === 'length') continue;
    parts.push(keys[i] + (d.enumerable ? '' : '~') + (d.writable === false ? '!' : '') + ':' + show(d.value));
  }
  var p = Object.getPrototypeOf(v);
  var tag = p === null ? 'null' : p === Array.prototype ? 'A' : p === Object.prototype ? 'O' : '?';
  return tag + '{' + parts.join(',') + '}';
}
function t(f) { try { return show(f()); } catch (e) { return 'throws ' + e.name; } }
"#;

fn agnijs(stress: bool, src: &str) -> String {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    vm.heap.stress_every = 1;
    let realm = vm.main_realm();
    vm.eval_script(realm, "prelude.js", PRELUDE).unwrap();
    match vm.eval_script(realm, "case.js", src) {
        Ok(Value::String(s)) => s.to_string_lossy(),
        Ok(other) => panic!("{src}: not a string: {other:?}"),
        Err(e) => panic!("{src}: {e}"),
    }
}

fn quickjs(src: &str) -> String {
    let mut e = QuickJsEngine::new(TestHost::new()).unwrap();
    let realm = e.main_realm();
    e.eval_script(realm, "prelude.js", PRELUDE).unwrap();
    match e.eval_script(realm, "case.js", src) {
        Ok(Value::String(s)) => s.to_string_lossy(),
        Ok(other) => panic!("{src}: not a string: {other:?}"),
        Err(e) => panic!("{src}: {e}"),
    }
}

/// Every case agrees on both AgniJS modes and on QuickJS-NG.
fn same(cases: &[&str]) {
    let mut failures = Vec::new();
    for case in cases {
        let src = format!("[{case}].map(function (f) {{ return t(f); }}).join(' | ')");
        let reference = quickjs(&src);
        for stress in [false, true] {
            let got = agnijs(stress, &src);
            if got != reference {
                failures.push(format!("{case}\n  stress={stress}\n  agnijs:  {got}\n  quickjs: {reference}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Cases with the answer written out (both AgniJS modes).
fn expected(cases: &[(&str, &str)]) {
    for (case, want) in cases {
        let src = format!("t({case})");
        for stress in [false, true] {
            assert_eq!(agnijs(stress, &src), *want, "{case} stress={stress}");
        }
    }
}

#[test]
fn json_parse() {
    same(&[
        r#"() => JSON.parse('{"a":1,"b":[true,false,null],"c":{"d":"x"},"e":-0,"f":1.5e3,"g":"\\u00e9\\n"}')"#,
        r#"() => JSON.parse('[1, 2.5, -3, 0, -0, 1e400, 123456789012345678901, 1E-2, 9007199254740993]')"#,
        r#"() => JSON.parse('{"0":1,"1":2,"10":3,"01":4,"4294967294":5,"4294967295":6,"-1":7}')"#,
        r#"() => JSON.parse('{"a":1,"a":2,"b":3,"a":4}')"#,
        r#"() => JSON.parse('"\\ud83d\\ude00 \\ud800 \\u0041\\t\\"\\\\\\/"')"#,
        r#"() => JSON.parse('"café 中文"')"#,
        r#"() => JSON.parse('{"中":"文","ascii":"ÿ"}')"#,
        r#"() => JSON.parse('{"__proto__":1}')"#,
        r#"() => JSON.parse(' \t\n\r[ ] ')"#,
        r#"() => JSON.parse('[[],[[]],{},[{}]]')"#,
        r#"() => JSON.parse('"' + 'x'.repeat(300) + '"').length"#,
        r#"() => JSON.parse('{"a":[1,{"b":[2,[3,{"c":4}]]}],"d":5}')"#,
        r#"() => JSON.parse(String(12345678901234567))"#,
        r#"() => JSON.parse('-0.0e-0')"#,
        r#"() => JSON.parse('01')"#,
        r#"() => JSON.parse('1.')"#,
        r#"() => JSON.parse('-')"#,
        r#"() => JSON.parse('"abc')"#,
        r#"() => JSON.parse('"a\u0001b"')"#,
        r#"() => JSON.parse('"\\x"')"#,
        r#"() => JSON.parse('"\\u12"')"#,
        r#"() => JSON.parse('{"a" 1}')"#,
        r#"() => JSON.parse('[1,]')"#,
        r#"() => JSON.parse('{"a":1,}')"#,
        r#"() => JSON.parse('')"#,
        r#"() => JSON.parse('[1] x')"#,
        r#"() => JSON.parse({ toString() { return '[7]'; } })"#,
        r#"() => JSON.parse('{"a":[1,2],"b":{"c":3}}', function (k, v) { return typeof v === 'number' ? v * 10 : v; })"#,
        r#"() => JSON.parse('{"a":1,"b":2,"c":[1,2]}', function (k, v) { return k === 'b' || k === '0' ? undefined : v; })"#,
        r#"() => { var seen = []; JSON.parse('{"x":{"y":1},"z":[2]}', function (k, v) { seen.push(k); return v; }); return seen; }"#,
        r#"() => { var o = JSON.parse('[{"a":1},{"a":2},{"a":3}]'); return o[0].a + o[1].a + o[2].a; }"#,
    ]);
}

#[test]
fn json_stringify() {
    same(&[
        r#"() => JSON.stringify({ a: 1, b: [true, false, null], c: { d: 'x' }, e: -0, f: 1.5e3, g: 'é\n' })"#,
        r#"() => JSON.stringify([1, 2.5, -3, 0.1 + 0.2, 1e21, 1e-7, -1e-7, 5e-324, 1.7976931348623157e308, 123456789012345680000, NaN, Infinity, -Infinity])"#,
        r#"() => JSON.stringify([2 ** 53, 2 ** 53 + 2, 2 ** 60, -(2 ** 31), 4294967295, 0.000001, 1 / 3])"#,
        r#"() => JSON.stringify({ 1: 'one', 0: 'zero', b: 'bee', a: 'ay', 4294967295: 'big' })"#,
        r#"() => JSON.stringify({ a: undefined, b: function () {}, c: Symbol('s'), d: 1 })"#,
        r#"() => JSON.stringify({ a: undefined })"#,
        r#"() => JSON.stringify([undefined, function () {}, Symbol('s')])"#,
        r#"() => JSON.stringify('\u0000\u0001\u001f\u007f\b\t\n\f\r"\\/   ')"#,
        r#"() => JSON.stringify('😀 \ud800 \udc00 x\ud800 􏿿')"#,
        r#"() => JSON.stringify('café ÿ Ā 中')"#,
        r#"() => JSON.stringify({ '中': 1, 'q"k': 2, 'n\nk': 3 })"#,
        r#"() => JSON.stringify({ a: [1, { b: 2 }], c: 'x' }, null, 2)"#,
        r#"() => JSON.stringify({ a: [1, { b: 2 }], c: 'x', d: [], e: {} }, null, '--')"#,
        r#"() => JSON.stringify({ a: 1, b: undefined, c: 3 }, null, 4)"#,
        r#"() => JSON.stringify([1, [2, [3]]], null, '中')"#,
        r#"() => JSON.stringify({ a: 1, b: 2, c: { a: 3, d: 4 } }, ['a', 'c', 1, 'a'])"#,
        r#"() => JSON.stringify({ a: 1, b: 'x', c: [1, 2] }, function (k, v) { return typeof v === 'number' ? v + 1 : v; })"#,
        r#"() => JSON.stringify({ a: 1, b: 2 }, function (k, v) { return k === 'a' ? undefined : v; })"#,
        r#"() => JSON.stringify({ toJSON(k) { return 'key:' + k; } })"#,
        r#"() => JSON.stringify({ x: { toJSON(k) { return [k, 1]; } }, d: new Date(0) })"#,
        r#"() => JSON.stringify([new Number(3), new String('s'), new Boolean(false), Object(1n) === 1])"#,
        r#"() => JSON.stringify(1n)"#,
        r#"() => { var o = {}; o.self = o; return JSON.stringify(o); }"#,
        r#"() => { var a = []; a[0] = a; return JSON.stringify(a); }"#,
        r#"() => JSON.stringify(undefined)"#,
        r#"() => JSON.stringify('x'.repeat(70) + 'Ā' + 'y'.repeat(70)).length"#,
        r#"() => { var o = {}; for (var i = 0; i < 40; i++) o['k' + i] = i % 3 ? 'v' + i : i / 8; return JSON.stringify(o); }"#,
        r#"() => { var log = []; var o = { get a() { log.push('a'); return 1; }, get b() { log.push('b'); return undefined; }, get c() { log.push('c'); return 3; } }; return JSON.stringify(o) + log.join(''); }"#,
        r#"() => JSON.stringify(Object.create({ inherited: 1 }, { own: { value: 2, enumerable: true }, hidden: { value: 3 } }))"#,
        r#"() => JSON.stringify(new Proxy({ a: 1, b: 2 }, { ownKeys() { return ['b', 'a']; } }))"#,
        r#"() => JSON.stringify([1, 2, 3].map(String))"#,
    ]);
}

#[test]
fn string_search_and_slice() {
    same(&[
        r#"() => ['abcabc'.indexOf('c'), 'abcabc'.indexOf('c', 3), 'abc'.indexOf(''), 'abc'.indexOf('', 9), 'abc'.indexOf('d')]"#,
        r#"() => ['café中'.indexOf('中'), 'café'.indexOf('中'), 'x中y'.indexOf('y'), 'café'.indexOf('é')]"#,
        r#"() => ['abcabc'.lastIndexOf('b'), 'abcabc'.lastIndexOf('b', 3), 'abc'.lastIndexOf(''), '中中'.lastIndexOf('中')]"#,
        r#"() => ['abc'.includes('bc'), 'abc'.includes('bc', 2), 'aĀ'.includes('Ā'), 'abc'.includes('Ā')]"#,
        r#"() => ['abc'.startsWith('ab'), 'abc'.startsWith('bc', 1), 'abc'.endsWith('bc'), 'abc'.endsWith('ab', 2), '中x'.startsWith('中'), 'ab'.endsWith('abc')]"#,
        r#"() => { var s = 'x'.repeat(40) + 'y'.repeat(40); return [s.indexOf('xy'), s.slice(39, 41), s.substring(78), s.substr(-3, 2)]; }"#,
        r#"() => ['hello'.slice(1, -1), 'hello'.slice(-3), 'hello'.slice(3, 1), 'hello'.slice(), 'h中llo'.slice(1, 2), 'h中llo'.slice(2)]"#,
        r#"() => ['hello'.substring(4, 1), 'hello'.substring(-1, 2), 'hello'.substr(1, 3), 'hello'.substr(-2), 'hello'.substr(1, 0)]"#,
        r#"() => ['hello'.at(-1), 'h中'.at(1), 'hello'.at(9), 'hello'.charAt(1)]"#,
        r#"() => ['  a b  '.trim(), ' ﻿x '.trim(), '  a '.trimStart(), ' a  '.trimEnd(), '   '.trim(), 'abc'.trim(), ' 中 '.trim()]"#,
        r#"() => { var s = 'ab'; for (var i = 0; i < 6; i++) s += s; return [s.length, s.slice(10, 14), s.indexOf('ba', 7), s.trim().length]; }"#,
        r#"() => ({ valueOf() { return 1; }, toString() { return 'abc'; } }).toString().indexOf('b')"#,
        r#"() => String.prototype.indexOf.call(12345, 34)"#,
    ]);
}

#[test]
fn string_split_and_replace() {
    same(&[
        r#"() => 'a b  c'.split(' ')"#,
        r#"() => ['a,b,c'.split(',', 2), 'abc'.split(''), 'abc'.split('', 2), ''.split(''), ''.split(','), 'abc'.split(), 'abc'.split(undefined, 0)]"#,
        r#"() => ['a中b中c'.split('中'), 'café-x'.split('-'), 'aXbXc'.split('X', 0), ',a,'.split(',')]"#,
        r#"() => 'x中é'.split('')"#,
        r#"() => { var s = 'w '.repeat(50); return s.split(' ').length; }"#,
        r#"() => ['aaa'.replace('a', 'b'), 'abc'.replace('d', 'x'), 'abc'.replace('', '-'), 'abc'.replace('b', '[$&|$`|$\'|$$|$1|$<n>|$]')]"#,
        r#"() => ['abc'.replace('b', function (m, p, s) { return m + p + s; }), 'a中c'.replace('中', 'é'), 'café'.replace('f', '中')]"#,
        r#"() => ['abab'.replaceAll('b', 'x'), 'abab'.replaceAll('', '-'), 'aaaa'.replaceAll('aa', 'b'), 'abab'.replaceAll('b', '$&$&'), 'abc'.replaceAll('z', 'y')]"#,
        r#"() => { var seen = []; var r = 'abcb'.replaceAll('b', function (m, p) { seen.push(p); return '<' + p + '>'; }); return [r, seen]; }"#,
        r#"() => 'a.b.c'.replaceAll('.', function () { return '中'; })"#,
        r#"() => 'x'.replace('x', { toString() { return '$&$&'; } })"#,
        r#"() => { var order = []; 'ab'.replace({ toString() { order.push('search'); return 'a'; } }, { toString() { order.push('replace'); return 'z'; } }); return order; }"#,
        r#"() => 'aaa'.replaceAll(/a/g, 'b')"#,
        r#"() => 'aaa'.replaceAll(/a/, 'b')"#,
    ]);
}

#[test]
fn string_case_mapping() {
    same(&[
        r#"() => ['Hello World 123'.toLowerCase(), 'Hello World 123'.toUpperCase(), 'abc'.toLowerCase(), 'ABC'.toUpperCase(), ''.toUpperCase()]"#,
        r#"() => ['À×Þßà÷þÿµ'.toUpperCase(), 'À×Þßà÷þÿµ'.toLowerCase()]"#,
        r#"() => ['straße'.toUpperCase(), 'café'.toUpperCase(), 'CAFÉ'.toLowerCase(), 'ÿ'.toUpperCase().charCodeAt(0)]"#,
        r#"() => ['İ'.toLowerCase(), 'ΣAΣ'.toLowerCase(), 'AΣ B'.toLowerCase(), 'ŉ'.toUpperCase(), 'ﬀ'.toUpperCase()]"#,
        r#"() => ['𐐀'.toLowerCase(), '𐐨'.toUpperCase(), '\ud800x'.toUpperCase(), 'x\udc00'.toLowerCase()]"#,
        r#"() => ['中Abc'.toUpperCase(), '中Abc'.toLowerCase(), 'aĀb'.toUpperCase()]"#,
        r#"() => String.prototype.toUpperCase.call(true)"#,
        r#"() => { var s = 'Ab'; for (var i = 0; i < 6; i++) s += s; return [s.toUpperCase().slice(0, 4), s.toLowerCase().length]; }"#,
    ]);
}

#[test]
fn regexp_exec_and_test() {
    same(&[
        r#"() => /(\d+)-(\d+)/.exec('ab 12-34 cd')"#,
        r#"() => /(a)|(b)/.exec('b')"#,
        r#"() => /x/.exec('abc')"#,
        r#"() => /(?<year>\d{4})-(?<m>\d\d)/.exec('on 2024-05')"#,
        r#"() => { var m = /(?<a>x)|(?<a>y)/.exec('y'); return [m.groups.a, m.length]; }"#,
        r#"() => { var m = /(a)(?<n>b)?/d.exec('xa'); return [m.indices, m.indices.groups]; }"#,
        r#"() => { var r = /a/g; var out = []; var m; while ((m = r.exec('aXaXa'))) out.push(m.index, r.lastIndex); return out; }"#,
        r#"() => { var r = /a/y; return [r.test('ba'), r.lastIndex, (r.lastIndex = 1, r.test('ba')), r.lastIndex]; }"#,
        r#"() => [/café/.test('un café'), /中/.test('x中'), /^\d+$/.test('123'), /^\d+$/.test('12a')]"#,
        r#"() => { var r = /a/g; r.lastIndex = 10; return [r.test('aaa'), r.lastIndex]; }"#,
        r#"() => { var r = /a/g; var n = 0; r.lastIndex = { valueOf() { n++; return 1; } }; return [r.test('aa'), r.lastIndex, n]; }"#,
        r#"() => { var r = /a/; r.exec = function (s) { return { 0: 'zz', index: 0, length: 1 }; }; return r.test('nothing'); }"#,
        r#"() => { var r = /a/; r.exec = function () { return null; }; return r.test('a'); }"#,
        r#"() => { var r = /a/; r.exec = function () { return 3; }; return r.test('a'); }"#,
        r#"() => { var calls = 0; var r = /b/; Object.defineProperty(r, 'exec', { get() { calls++; return RegExp.prototype.exec; } }); return [r.test('abc'), calls]; }"#,
        r#"() => RegExp.prototype.test.call({ exec() { return {}; } }, 'x')"#,
        r#"() => RegExp.prototype.exec.call({}, 'x')"#,
        r#"() => { var long = 'x'.repeat(300) + '42'; var r = /\d+/g; return [r.exec(long)[0], r.lastIndex, r.exec(long), r.lastIndex]; }"#,
        r#"() => { var long = '中'.repeat(300) + '42'; return /\d+/.exec(long).index; }"#,
        r#"() => { var out = []; for (var i = 0; i < 5; i++) out.push(/id=\d+$/.test('id=' + i)); return out; }"#,
        r#"() => { var r = new RegExp('a', 'gimsuy'); return [r.flags, r.global, r.sticky, new RegExp('b', 'dv').flags]; }"#,
        r#"() => new RegExp('a', 'gg')"#,
        r#"() => new RegExp('a', 'z')"#,
        r#"() => new RegExp('a', 'gimsuyabcdefghijk')"#,
        r#"() => new RegExp('(', 'g')"#,
        r#"() => Object.getOwnPropertyDescriptor(RegExp.prototype, 'flags').get.call({ global: 1, sticky: 'yes', hasIndices: 0 })"#,
        r#"() => Object.getOwnPropertyDescriptor(RegExp.prototype, 'global').get.call(3)"#,
        r#"() => Object.getOwnPropertyNames(/(a)/.exec('a'))"#,
    ]);
}

/// A unicode-mode lastIndex inside a surrogate pair: the match is
/// recorded at lastIndex itself (the engine's reading of
/// RegExpBuiltinExec, which QuickJS-NG does not share).
#[test]
fn regexp_last_index_inside_a_pair() {
    expected(&[
        (
            r#"() => { var r = /\u{1F600}/gu; r.lastIndex = 1; var m = r.exec('\u{1F600}'); return [m && m.index, r.lastIndex]; }"#,
            "A{0:1,1:2}",
        ),
        (
            r#"() => { var r = /\u{1F600}/gu; r.lastIndex = 1; return [r.test('\u{1F600}'), r.lastIndex]; }"#,
            "A{0:true,1:2}",
        ),
        (
            r#"() => { var r = /(?:)/gu; r.lastIndex = 1; var m = r.exec('\u{1F600}'); return m === null ? 'null' : [m.index, m[0], r.lastIndex]; }"#,
            // Was a panic (slicing backwards); now an empty match there.
            r#"A{0:1,1:"",2:0}"#,
        ),
    ]);
}

#[test]
fn regexp_replace_match_split() {
    same(&[
        r#"() => 'a1b22c333'.replace(/\d+/g, '#')"#,
        r#"() => 'a1b22c333'.replace(/\d+/, '#')"#,
        r#"() => 'john smith'.replace(/(\w+)\s(\w+)/, '$2, $1 [$&] [$`] [$\'] $$ $3 $0 $10 $01')"#,
        r#"() => '2024-05-06'.replace(/(?<y>\d+)-(?<m>\d+)-(?<d>\d+)/, '$<d>/$<m>/$<y> $<nope> $<')"#,
        r#"() => 'abc'.replace(/(?<x>b)/, '[$<x]')"#,
        r#"() => 'aaa'.replace(/a*?/g, '-')"#,
        r#"() => 'a\u{1F600}b'.replace(/(?:)/gu, '-')"#,
        r#"() => 'a\u{1F600}b'.replace(/(?:)/g, '-').length"#,
        r#"() => 'x-y-z'.replace(/-/g, function (m, p, s) { return '(' + m + p + s.length + ')'; })"#,
        r#"() => 'x1y2'.replace(/(\d)/g, function (m, c, p) { return '[' + c + p + ']'; })"#,
        r#"() => '2024-05'.replace(/(?<y>\d+)-(?<m>\d+)/g, function () { var g = arguments[arguments.length - 1]; return g.m + '/' + g.y; })"#,
        r#"() => 'café café'.replace(/é/g, '中')"#,
        r#"() => '中1中2'.replace(/\d/g, 'x')"#,
        r#"() => { var long = 'ab1 '.repeat(100); return long.replace(/\d/g, '').length; }"#,
        r#"() => { var r = /a/g; r.exec = function (s) { var m = RegExp.prototype.exec.call(this, s); if (m) m.index = 0; return m; }; return 'xaxa'.replace(r, 'Y'); }"#,
        r#"() => { var log = []; var r = /./g; r.exec = function () { if (log.length > 2) return null; log.push('e'); return { length: 2, 0: { toString() { log.push('m'); return 'q'; } }, 1: 'c', index: log.length, get groups() { log.push('g'); return undefined; } }; }; var out = 'abcdef'.replace(r, '<$1>'); return [out, log]; }"#,
        r#"() => { var r = /b/; r.exec = function () { return { length: 1, 0: 'b', index: 1, groups: null }; }; return 'abc'.replace(r, 'X'); }"#,
        r#"() => { var r = /b/; r.exec = function () { return { length: 1, 0: 'b', index: 1, groups: { n: 'N' } }; }; return 'abc'.replace(r, '[$<n>]'); }"#,
        r#"() => ['a1b2'.match(/\d/g), 'ab'.match(/\d/g), 'a1'.match(/(\d)/), 'aaa'.match(/a*?/g), '中1'.match(/\d/g)]"#,
        r#"() => [...'a1b2'.matchAll(/(\d)/g)].map(function (m) { return m[0] + m[1] + m.index; })"#,
        r#"() => 'a1b2c3'.search(/\d/)"#,
        r#"() => ['abc'.search(/z/), 'a中'.search(/中/)]"#,
        r#"() => { var r = /b/g; r.lastIndex = 2; var i = 'abc'.search(r); return [i, r.lastIndex]; }"#,
        r#"() => ['a, b,c'.split(/\s*,\s*/), 'a1b2c'.split(/(\d)/), 'abc'.split(/(?:)/), ''.split(/x/), ''.split(/(?:)/), 'abc'.split(/b/, 1)]"#,
        r#"() => ['a1b2c'.split(/(\d)/, 2), 'a1b2c'.split(/(\d)(x)?/), '中,中'.split(/,/), 'a\u{1F600}b'.split(/(?:)/u).length]"#,
        r#"() => { var s = 'w1 w2  w3   w4'; return s.split(/\s+/); }"#,
        r#"() => { var r = /,/; r.constructor = function (rx, f) { return /;/y; }; r.constructor[Symbol.species] = r.constructor; return 'a;b,c'.split(r); }"#,
        r#"() => { class R extends RegExp { exec(s) { return super.exec(s); } } return ['a-b'.split(new R('-')), 'a-b'.replace(new R('-', 'g'), '+'), new R('b').test('abc')]; }"#,
    ]);
}

#[test]
fn regexp_in_place_of_exec_unmodified_after_mutation() {
    // Replacing RegExp.prototype.exec is seen by test/replace/match/split.
    same(&[
        r#"() => { var orig = RegExp.prototype.exec; var n = 0; RegExp.prototype.exec = function (s) { n++; return orig.call(this, s); }; try { return [/a/.test('a'), 'aa'.replace(/a/g, 'b'), 'a,b'.split(/,/), 'aa'.match(/a/g), n]; } finally { RegExp.prototype.exec = orig; } }"#,
        r#"() => { var orig = RegExp.prototype.exec; RegExp.prototype.exec = function () { return null; }; try { return [/a/.test('a'), 'aa'.replace(/a/g, 'b'), 'aa'.match(/a/g)]; } finally { RegExp.prototype.exec = orig; } }"#,
        r#"() => { var d = Object.getOwnPropertyDescriptor(RegExp.prototype, 'flags'); Object.defineProperty(RegExp.prototype, 'flags', { get() { return 'y'; }, configurable: true }); try { return ['aXa'.replace(/a/g, 'b'), 'aXa'.match(/a/g)]; } finally { Object.defineProperty(RegExp.prototype, 'flags', d); } }"#,
    ]);
}
