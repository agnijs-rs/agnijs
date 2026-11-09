//! Lone surrogates (TASK-100.15). JavaScript strings are sequences of
//! UTF-16 code units and may hold unpaired surrogates; source text built
//! from such strings (eval, `Function`) must reach the lexer unchanged, and
//! every round trip through literals, string built-ins, JSON, RegExp,
//! bytecode and the host boundary must keep the code units.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, RealmId, Value};

fn engine() -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine(TestHost::new());
    let realm = e.main_realm();
    (e, realm)
}

/// `hex(s)`: the code units of `s` in hex, for readable comparisons.
const HEX: &str = "function hex(s) { var o = []; for (var i = 0; i < s.length; i++) \
                   o.push(s.charCodeAt(i).toString(16)); return o.join(' '); }\n";

fn run(src: &str) -> Value {
    let (mut e, realm) = engine();
    let src = format!("{HEX}{src}");
    let v = e
        .eval_script(realm, "lone-surrogates.js", &src)
        .unwrap_or_else(|err| panic!("{err}: {src}"));
    e.run_jobs(realm).unwrap();
    v
}

/// Run `src` and expect the string `expected` (a probe's verdict).
fn check(src: &str, expected: &str) {
    assert_eq!(run(src), Value::string(expected), "{src}");
}

#[test]
fn string_literals_in_eval_and_function_source_keep_lone_surrogates() {
    check(
        r#"var hi = String.fromCharCode(0xD800), lo = String.fromCharCode(0xDC00);
           var a = eval("'" + hi + "'");
           var b = eval('"x' + lo + 'y"');
           var c = Function('return "' + lo + hi + '";')();
           var d = (0, eval)("'\\" + hi + "'");
           [hex(a), hex(b), hex(c), hex(d)].join('|')"#,
        "d800|78 dc00 79|dc00 d800|d800",
    );
}

#[test]
fn template_literals_keep_lone_surrogates_cooked_and_raw() {
    check(
        r#"var hi = String.fromCharCode(0xDBFF);
           var cooked = eval('`a' + hi + '`');
           var raw = eval('String.raw`' + hi + '\\' + hi + '`');
           var tagged = eval('(function (s) { return s.raw[0] + s[1]; })`' + hi + '${1}' + hi + '`');
           [hex(cooked), hex(raw), hex(tagged)].join('|')"#,
        "61 dbff|dbff 5c dbff|dbff dbff",
    );
}

#[test]
fn regexp_literals_keep_lone_surrogates_in_source_and_matching() {
    check(
        r#"var lo = String.fromCharCode(0xDC01), hi = String.fromCharCode(0xD801);
           var a = eval('/' + lo + '/');
           var b = eval('/\\' + hi + '/');
           var c = eval('/[' + hi + ']/u');
           var d = eval('/x' + hi + '/g');
           [hex(a.source), a.test(lo), a.test('\uFFFD'), hex(b.source), b.test(hi),
            c.test(hi), c.test(hi + lo), hex(d.source), d.flags].join('|')"#,
        "dc01|true|false|5c d801|true|true|false|78 d801|g",
    );
}

#[test]
fn lone_surrogates_are_not_identifier_characters_but_may_sit_in_comments() {
    check(
        r#"var hi = String.fromCharCode(0xD800), out = [];
           for (var src of ['var a' + hi + ' = 1', 'var ' + hi + ' = 1', hi, 'var \\u' + 'd800 = 1']) {
             try { eval(src); out.push('ok'); } catch (e) { out.push(e.name); }
           }
           out.push(eval('1 // ' + hi), eval('/* ' + hi + ' */ 2'));
           out.join()"#,
        "SyntaxError,SyntaxError,SyntaxError,SyntaxError,1,2",
    );
}

#[test]
fn replacement_characters_written_in_source_stay_replacement_characters() {
    check(
        r#"var hi = String.fromCharCode(0xD800), r = '\uFFFD';
           hex(eval("'" + r + hi + r + "'")) + '|' + hex(eval('/' + r + hi + '/').source)"#,
        "fffd d800 fffd|fffd d800",
    );
}

#[test]
fn function_source_text_keeps_lone_surrogates() {
    check(
        r#"var lo = String.fromCharCode(0xDC00);
           var f = eval("(function () { return '" + lo + "'; })");
           var g = Function('a', "return '" + lo + "' + a;");
           var t = f.toString(), u = g.toString();
           [hex(f()), t.indexOf(lo) > 0, t.indexOf('\uFFFD'), u.indexOf(lo) > 0,
            u.indexOf('\uFFFD'), g('!') === lo + '!'].join('|')"#,
        "dc00|true|-1|true|-1|true",
    );
}

#[test]
fn string_built_ins_keep_lone_surrogates() {
    // One-byte strings widen when a surrogate joins them; slices share
    // their parent's buffer; searches run in place (TASK-100.29).
    check(
        r#"var hi = '\uD800', lo = '\uDC00', latin = 'caf\xE9';
           var s = latin + hi + 'x' + lo;
           var out = [hex(s), s.length, hex(s.slice(4, 6)), hex(s.substring(6)),
             s.indexOf(hi), s.lastIndexOf(lo), s.includes(lo + 'x'), hex(s.at(-1)),
             s.codePointAt(4).toString(16), s.isWellFormed(), hex(s.toWellFormed()),
             hex((hi + lo).split('')[1]), hex(s.toUpperCase()), hex(s.normalize('NFC')),
             hex(s.replace('x', lo)), hex(s.padEnd(8, hi)), hex(s.charAt(4)),
             hex(String.fromCodePoint(0xDBFF)), hex([...(hi + 'a' + hi + lo)][2]),
             hex(hi.concat(lo)), (hi + lo).length, (hi + lo).isWellFormed()];
           var o = {}; o[hi] = 1; o[lo] = 2;
           out.push(Object.keys(o).map(hex).join('/'), o['\uD800'], hi < lo, hi === '\uD800');
           var m = new Map([[hi, 'H']]); out.push(m.get(String.fromCharCode(0xD800)));
           out.join('|')"#,
        "63 61 66 e9 d800 78 dc00|7|d800 78|dc00|4|6|false|dc00|d800|false|\
         63 61 66 e9 fffd 78 fffd|dc00|43 41 46 c9 d800 58 dc00|63 61 66 e9 d800 78 dc00|\
         63 61 66 e9 d800 dc00 dc00|63 61 66 e9 d800 78 dc00 d800|d800|dbff|d800 dc00|\
         d800 dc00|2|true|d800/dc00|1|true|true|H",
    );
}

#[test]
fn json_escapes_lone_surrogates_and_parses_them_back() {
    check(
        r#"var hi = '\uD800', lo = '\uDFFF';
           var j = JSON.stringify({ k: hi + 'a' + lo, p: '\uD83D\uDE00' });
           var back = JSON.parse(j);
           var raw = JSON.parse('"' + hi + '"');
           var key = JSON.stringify({ [lo]: 1 });
           [j, hex(back.k), hex(back.p), hex(raw), key, JSON.stringify(lo + hi),
            hex(JSON.parse('"\\udc00\\ud800"'))].join('|')"#,
        "{\"k\":\"\\ud800a\\udfff\",\"p\":\"\u{1F600}\"}|d800 61 dfff|d83d de00|d800|\
         {\"\\udfff\":1}|\"\\udfff\\ud800\"|dc00 d800",
    );
}

#[test]
fn regexp_built_ins_keep_lone_surrogates() {
    check(
        r#"var hi = '\uD800', lo = '\uDC00';
           var r = new RegExp(hi + '+');
           var s = 'a' + hi + hi + lo + 'b';
           var out = [hex(r.source), hex(r.exec(s)[0]), hex(s.match(/\uD800/g).join('')),
             hex(s.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/g, '?')),
             hex(s.split(new RegExp(hi))[2]), /^.$/u.test(hi), /^.$/u.test(hi + lo),
             /^..$/.test(hi + lo), hex(String(new RegExp(lo, 'g'))),
             s.search(/\uDC00/), /\p{Cs}/u.test(lo), hex(RegExp(hi + lo, 'u').exec(s)[0])];
           out.join('|')"#,
        "d800 2b|d800 d800|d800 d800|61 3f d800 dc00 62|dc00 62|true|true|true|\
         2f dc00 2f 67|3|true|d800 dc00",
    );
}

#[test]
fn the_host_boundary_keeps_lone_surrogates_in_values_and_replaces_them_in_rust_strings() {
    use agnijs_engine_api::{JsString, PropertyKey};
    let (mut e, realm) = engine();
    let global = e.global_object(realm).unwrap();
    // Host to script: a JsString is UTF-16, so the surrogate arrives as is.
    let lone = JsString::from_utf16(vec![0x61, 0xD800]);
    e.set(
        realm,
        &global,
        &PropertyKey::from("fromHost"),
        &Value::String(lone.clone()),
    )
    .unwrap();
    let seen = e
        .eval_script(
            realm,
            "h.js",
            "fromHost.length + ':' + fromHost.charCodeAt(1).toString(16)",
        )
        .unwrap();
    assert_eq!(seen, Value::string("2:d800"));
    // Script to host: values keep their code units.
    let back = e
        .eval_script(realm, "h.js", "fromHost + '\\uDC01'")
        .unwrap();
    let Value::String(back) = back else {
        panic!("not a string: {back:?}")
    };
    assert_eq!(back.units(), [0x61, 0xD800, 0xDC01]);
    // Property keys too.
    e.eval_script(realm, "h.js", "globalThis['k\\uDFFF'] = 1")
        .unwrap();
    let key = PropertyKey::String(JsString::from_utf16(vec![0x6B, 0xDFFF]));
    assert_eq!(e.get(realm, &global, &key).unwrap(), Value::Number(1.0));
    // Rust strings cannot hold a lone surrogate: `to_string` replaces it
    // with U+FFFD, as JsString::to_string_lossy does.
    let text = e.to_string(realm, &Value::String(lone.clone())).unwrap();
    assert_eq!(text, "a\u{FFFD}");
    assert_eq!(lone.to_string_lossy(), "a\u{FFFD}");
}
