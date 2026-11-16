//! The inputs each target's mutation starts from. `agnijs-fuzz --seeds`
//! writes them out as a libFuzzer corpus.

use crate::targets::{snapshot_body, CDP_PAUSE_MARKER, MODULE_MARKER};

/// JavaScript covering most of the grammar, scripts and modules.
pub const JS: &[&str] = &[
    "var log = []; function sloppy(a, b) { arguments[0] = 9; b = 3; return a + arguments[1] + arguments.length; } log.push(sloppy(1, 2));",
    "function counter() { let n = 0; return { inc: () => ++n, get: function () { return n; } }; } var c = counter(); c.inc(); c.get();",
    "var fns = []; for (let i = 0; i < 3; i++) { fns.push(() => i); } fns.map(f => f()).join();",
    "try { with ({ w: 1 }) { w; } } catch (e) {} label: for (var k in { a: 1, b: 2 }) { for (const v of [1, 2]) { if (v == 2) continue label; if (k == 'b') break label; } }",
    "function* gen(x) { try { const y = yield x; yield* [y, 2]; return 3; } finally { 0; } } var g = gen(1); g.next(); g.next(7); g.return(0); [...gen(2)];",
    "async function af(p) { const v = await p; try { await Promise.reject(1); } catch (e) { return v + e; } } af(Promise.resolve(1)).then(v => v);",
    "async function* ag() { yield 1; yield await 2; } (async () => { for await (const v of ag()) v; })();",
    "class Base { #p = 1; static s = 2; static { this.t = 3; } get p() { return this.#p; } #m() { return 4; } m() { return this.#m(); } static #sp() { return 5; } static sp() { return Base.#sp(); } }\nclass Derived extends Base { constructor(...a) { super(...a); this.q = super.m(); } accessor x = 1; }\nnew Derived().p + Base.sp();",
    "function destr({ a, b: [c, ...d] = [1, 2, 3], ...rest }, ...more) { return a + c + d.length + Object.keys(rest).length + more.length; } destr({ a: 1, x: 2 }, 3, 4);",
    "function tag(s, ...v) { return s.raw.join('|') + v.join(','); } tag`a${1}b${2}\\u{41}`; `t${`n${1}`}`;",
    "function evals(x) { var y = 1; eval('var z = x + y'); return z; } evals(2); (0, eval)('1'); new Function('a', 'b', 'return a + b')(1, 2);",
    "function disposer() { using r = { [Symbol.dispose]() {} }; return 1; } disposer();",
    "var o = { __proto__: null, ['k' + 1]: 1, get g() { return 2; }, set g(v) {}, m() { return super.toString; }, async *am() {} }; o.k1 + o?.g + o.nope?.x; delete o.k1; 'k1' in o;",
    "var s = 0; for (var j = 0; j < 200; j++) { s += j * 2 > 10 ? j : -j; s |= 0; } switch (s) { case 1: break; default: s = -s; } s;",
    "/a(b)?/.exec('ab')[1]; 'aBc'.replace(/(?<x>b)/gi, '$<x>$1$&'); 'a,b'.split(/(,)/); /\\p{Script=Greek}+/v.test('αβ'); /(?<=a)b/.exec('ab'); 'x'.matchAll(/./g).next();",
    "2n ** 64n; BigInt.asIntN(8, 255n); (-7n) / 2n; 0x1fn << 3n; typeof 1n; BigInt('0b101');",
    "var p = new Proxy({}, { get(t, k) { return k; }, has() { return true; }, ownKeys() { return ['a']; }, getOwnPropertyDescriptor() { return { value: 1, configurable: true, enumerable: true }; } }); p.x; 'y' in p; Object.keys(p); Reflect.ownKeys(p);",
    "var m = new Map([[1, 'a']]); var st = new Set([1, 2]); var w = new WeakMap(); w.set(m, 1); st.union(new Set([3])); [...m.entries()]; Object.groupBy([1, 2, 3], x => x % 2);",
    "var ab = new ArrayBuffer(8, { maxByteLength: 16 }); var ta = new Uint8Array(ab); ta.set([1, 2, 3]); ab.resize(4); ta.copyWithin(0, 1); new DataView(ab).getInt16(0); Atomics.add(new Int32Array(new SharedArrayBuffer(8)), 0, 1);",
    "JSON.stringify({ a: [1, { b: 2 }], c: 'x\\u2028' }, null, 2); JSON.parse('[1, {\"a\": null}]', (k, v) => v); JSON.rawJSON && JSON.rawJSON('1');",
    "new Date(0).toISOString(); Date.UTC(2020, 1); new Date('2020-02-30T25:00Z').getTime(); Date.parse('Tue, 01 Jan 2030 00:00:00 GMT');",
    "Array.from({ length: 5 }, (_, i) => i * i).toSorted((a, b) => b - a).with(0, 9).flatMap(x => [x, [x]]).flat(Infinity).findLast(x => x > 1);",
    "var wr = new WeakRef({}); var fr = new FinalizationRegistry(() => {}); fr.register({}, 1); wr.deref();",
    "Object.defineProperty(globalThis, 'q', { get() { return 1; }, configurable: true }); q; Object.freeze([]); Object.fromEntries([['a', 1]]); structuredClone === undefined;",
    "'abc'.at(-1) + 'x'.padStart(4, 'ab') + 'é'.normalize('NFD') + 'İ'.toLowerCase() + String.raw`\\n` + 'a'.localeCompare('b') + 'abc'.isWellFormed();",
    "Iterator.from([1, 2, 3]).map(x => x * 2).filter(Boolean).take(2).toArray(); Promise.withResolvers(); Array.fromAsync([1]);",
    "a: { b: { break a; } } do { continue; } while (false); if (1) ; else ; for (;;) { break; } while (0) {} x => ({}); (a = 1, [b] = [2], { c } = {}) => a;",
    "function f() { 'use strict'; return this; } f(); (function () { return typeof arguments; })(); new.target; (class { static m() { return new.target; } }).m();",
    "Math.max(...[1, 2, 3]); Number('0x10') + parseInt('08') + parseFloat('1e3x') + (0.1 + 0.2).toFixed(20) + (123.456).toPrecision(2) + (255).toString(36);",
    "Symbol.for('a').description; var it = { [Symbol.iterator]() { return { next() { return { done: true }; }, return() { return {}; } }; } }; for (const x of it) {} [...it];",
    "Error.captureStackTrace && Error.captureStackTrace({}); new AggregateError([1], 'm', { cause: 2 }).errors; try { null.x; } catch ({ message }) { message; }",
    "<!-- html comment\nvar a = 1; --> also a comment\n0x1F; 017; 08; '\\08'; /=/; a\n++\nb;",
    "export default 1;",
    "//@module\nimport { dep } from 'dep.mjs'; import * as ns from 'cycle.mjs'; import f from 'dep.mjs'; export { dep }; export let total = dep + ns.c + f(); export * from 'dep.mjs'; const { v } = await import('tla.mjs'); import.meta;",
    "//@module\nexport default class { static #x = 1; static get x() { return this.#x; } }; export const a = await Promise.resolve(1); export function* g() {} export { a as 'string name' };",
];

/// JSON texts, valid and not.
pub const JSON: &[&str] = &[
    "{\"a\":[1,2.5e10,-0,true,false,null],\"b\":{\"c\":\"\\u00e9\\ud83d\\ude00\\n\"},\"__proto__\":1}",
    "[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]",
    "\"\\ud800\" ",
    "1e400",
    "-1.5E-7",
    "{\"a\":1,\"a\":2}",
    "[1,]",
    "{\"x\":\"\u{2028}\"}",
    " \t\n\r123 ",
    "{\"9007199254740993\":9007199254740993,\"0\":[],\"1\":{}}",
    "\"\\x41\"",
    "[\"a\",\"b\",{\"c\":[null,{\"d\":\"\"}]}]",
];

/// Intl inputs: locale, two option values, text.
pub const TEMPORAL: &[&str] = &[
    "P1Y2M3W4DT5H6M7.008009010S\nday\nhalfExpand\n2024-02-01\n3",
    "-PT36H\nhours\nfloor\n2024-03-10T00:00[America/New_York]\n15",
    "PT18014398509.481981S\nmicrosecond\ntrunc\n+275760-09-13\n9007199254740991",
    "P4294967295Y\nyear\nceil\n-271821-04-19T00:00Z[UTC]\n1e9",
    "pt1,5h\nweeks\nhalfEven\n2020-02-29T12:00+05:30[+05:30]\n-0",
    "P1M\nauto\nexpand\n1970-01-01[u-ca=iso8601]\nNaN",
    "Europe/Paris\nsecond\nhalfEven\n2020-01-01T00:00:00.123456789Z\n1700000000123",
    "-12:59\nminutes\nhalfTrunc\n+275760-09-13T00:00Z\n-8.64e15",
    "M02L\nmonths\nconstrain\n2024-02-29[u-ca=chinese]\n30",
    "hebrew\neraYear\nreject\n2024-01-31[u-ca=japanese]\n5784",
    "nanosecond\nhour\nreject\nT23:59:59.999999999\n24",
    "\n\n\n\n",
];

pub const INTL: &[&str] = &[
    "en-US\nunit\nkilometer-per-hour\n1234.5",
    "de-DE-u-co-phonebk\ncurrency\nEUR\n-0.005",
    "ar-EG-u-nu-arab\npercent\ncompact\n12345678901234567890",
    "ja-JP-u-ca-japanese\nfull\nAsia/Tokyo\n1700000000000",
    "zh-Hant-TW\nlong\nlanguage\n漢字かなカナ😀 text",
    "x-private\ndecimal\nscientific\nNaN",
    "und-Latn-t-ja-m0-hepburn\nshort\nregion\nhello world. second sentence?",
    "tr\nword\ngrapheme\nİstanbul ıi",
    "en-u-hc-h23-nu-thai\ndateTime\nEtc/GMT+14\n8.64e15",
    "\n\n\n",
];

/// Regex inputs: flags, pattern, subject.
pub const REGEX: &[&str] = &[
    "g\n(a|ab)(c|bcd)(d*)\nabcd",
    "i\n(?<year>\\d{4})-(?<month>\\d{2})\nDate: 2024-05 and 1999-12",
    "u\n\\p{Script=Greek}+|\\P{L}\nαβγ 123",
    "v\n[\\p{L}--[a-z]]+\nABCdef",
    "v\n[[a-z]&&[^aeiou]]\\q{abc|d}?\nbcd",
    "\n(a*)*b\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaac",
    "s\n(?<=\\$)\\d+(?!\\.)\ncost $42 and $5.00",
    "m\n^\\w+$\nfirst\nsecond line\nthird",
    "y\n(?:a|b)+?\nabab",
    "d\n(a)|(b)\nb",
    "\n(\\w+)\\s\\1\nthe the end",
    "\n(?<x>a)|(?<x>b)\\k<x>\nbb",
    "\n(?i:a)B\nAB aB",
    "u\n\\u{1F600}.\\uD83D\n😀😀x",
    "\n[\\b\\cJ\\0\\x41-\\x5A]{2,4}?\nABC\n",
    "\n((((((((((a))))))))))\\10\naa",
    "\n\\b\\B^$.\n",
    "gim\na{2,}\naaaa",
];

/// CDP sessions: messages before the marker, the marker, then messages
/// sent while paused.
pub fn cdp() -> Vec<Vec<u8>> {
    let pre = [
        r#"{"id":1,"method":"Runtime.enable"}"#,
        r#"{"id":2,"method":"Debugger.enable"}"#,
        r#"{"id":3,"method":"Debugger.setPauseOnExceptions","params":{"state":"all"}}"#,
        r#"{"id":4,"method":"Debugger.setBreakpointByUrl","params":{"url":"app.js","lineNumber":2,"columnNumber":0,"condition":"a > 1"}}"#,
        r#"{"id":5,"method":"Debugger.setAsyncCallStackDepth","params":{"maxDepth":8}}"#,
        r#"{"id":6,"method":"Runtime.evaluate","params":{"expression":"1 + 1","returnByValue":true}}"#,
        r#"{"id":7,"method":"Runtime.evaluate","params":{"expression":"({a: [1, {b: 2}], s: new Set([1])})","generatePreview":true}}"#,
        r#"{"id":8,"method":"Runtime.getProperties","params":{"objectId":"1","ownProperties":true}}"#,
        r#"{"id":9,"method":"Runtime.callFunctionOn","params":{"functionDeclaration":"function (x) { return this.a + x; }","objectId":"1","arguments":[{"value":2}]}}"#,
        r#"{"id":10,"method":"Runtime.releaseObjectGroup","params":{"objectGroup":"console"}}"#,
        r#"{"id":11,"method":"Runtime.runIfWaitingForDebugger"}"#,
    ];
    let paused = [
        r#"{"id":20,"method":"Debugger.evaluateOnCallFrame","params":{"callFrameId":"0","expression":"a + b"}}"#,
        r#"{"id":21,"method":"Debugger.getPossibleBreakpoints","params":{"start":{"scriptId":"1","lineNumber":0},"end":{"scriptId":"1","lineNumber":9}}}"#,
        r#"{"id":22,"method":"Debugger.getScriptSource","params":{"scriptId":"1"}}"#,
        r#"{"id":23,"method":"Runtime.getProperties","params":{"objectId":"2"}}"#,
        r#"{"id":24,"method":"Debugger.stepOver"}"#,
        r#"{"id":25,"method":"Debugger.stepInto"}"#,
        r#"{"id":26,"method":"Debugger.stepOut"}"#,
        r#"{"id":27,"method":"Debugger.removeBreakpoint","params":{"breakpointId":"1"}}"#,
        r#"{"id":28,"method":"Debugger.setBreakpointsActive","params":{"active":false}}"#,
        r#"{"id":29,"method":"Debugger.resume"}"#,
    ];
    let mut out = Vec::new();
    let mut all: Vec<&str> = pre.to_vec();
    all.push(CDP_PAUSE_MARKER);
    all.extend(paused);
    out.push(all.join("\n").into_bytes());
    out.push(pre.join("\n").into_bytes());
    out.push([r#"{"id":1,"method":"Debugger.enable"}"#, r#"{"id":2,"method":"Debugger.pause"}"#, CDP_PAUSE_MARKER, paused[0], paused[3], paused[9]].join("\n").into_bytes());
    out.push(b"not json\n{\"id\":\"x\",\"method\":7}\n{\"method\":\"Nope.nope\",\"params\":[]}".to_vec());
    out
}

/// Source maps, also as `data:` URLs and with junk.
pub const SOURCE_MAPS: &[&str] = &[
    r#"{"version":3,"sources":["src/app.ts"],"names":["fail"],"mappings":"AAEA;EAEI;;AAKJA","file":"app.js"}"#,
    r#"{"version":3,"sources":["a.js","b.js"],"sourcesContent":["x",null],"names":[],"mappings":"AAAA,CAAC,EAAE;AACF,IAAI;;;ACAJ,gBAAgB","sourceRoot":"/root/"}"#,
    r#"{"version":3,"sections":[{"offset":{"line":0,"column":0},"map":{"version":3,"sources":["x"],"mappings":"AAAA"}}]}"#,
    "data:application/json;charset=utf-8;base64,eyJ2ZXJzaW9uIjozLCJzb3VyY2VzIjpbInNyYy9hcHAudHMiXSwibmFtZXMiOlsiZmFpbCJdLCJtYXBwaW5ncyI6IkFBRUE7RUFFSTs7QUFLSkEiLCJmaWxlIjoiYXBwLmpzIn0=",
    "data:application/json,%7B%22version%22%3A3%2C%22sources%22%3A%5B%22a%22%5D%2C%22mappings%22%3A%22AAAA%22%7D",
    "var x = 1;\n//# sourceMappingURL=app.js.map",
    r#"{"version":3,"sources":["a"],"mappings":"gggggggggggggg,/////////+;;;;,,,,AAAAAAAAAAAA"}"#,
];

/// Import maps: the map on the first line, then specifiers.
pub const IMPORT_MAPS: &[&str] = &[
    "{\"imports\":{\"lodash\":\"/node_modules/lodash-es/lodash.js\",\"lodash/\":\"/node_modules/lodash-es/\",\"app/\":\"./src/\"},\"scopes\":{\"/scoped/\":{\"lodash\":\"/other/lodash.js\"}},\"integrity\":{\"/node_modules/lodash-es/lodash.js\":\"sha384-abc\"}}\nlodash\nlodash/fp.js\napp/main.js\n./rel.js\n../up.js\n/abs.js\nhttps://cdn.example/x.js\nunknown",
    "{\"imports\":{\"a\":null,\"b/\":\"c\",\"\":\"/empty\",\"%2e%2e/\":\"/dots/\"}}\na\nb/x\n%2e%2e/y\n\u{0}\nb/../../etc",
    "{\"imports\":{\"x\":\"https://example.com/x.js\"},\"scopes\":{\"https://example.com/\":{\"x\":\"./y.js\"}}}\nx\nhttps://example.com/x.js",
    "not json\nx",
];

/// WebSocket frames: masked text, ping, close, fragmented, a 16-bit and a
/// 64-bit length.
pub fn ws_frames() -> Vec<Vec<u8>> {
    let masked = |op: u8, fin: bool, payload: &[u8]| {
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut f = vec![(u8::from(fin) << 7) | op];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 0xffff => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        f
    };
    let msg = br#"{"id":1,"method":"Runtime.evaluate","params":{"expression":"1"}}"#;
    let mut fragmented = masked(1, false, &msg[..10]);
    fragmented.extend(masked(0, true, &msg[10..]));
    let mut long64 = vec![0x81, 0x80 | 127];
    long64.extend_from_slice(&(1u64 << 40).to_be_bytes());
    long64.extend_from_slice(&[0, 0, 0, 0, 1, 2, 3]);
    vec![
        masked(1, true, msg),
        masked(9, true, b"ping"),
        masked(8, true, &[0x03, 0xe8]),
        fragmented,
        masked(2, true, &vec![0xAB; 300]),
        long64,
    ]
}

pub fn js() -> Vec<Vec<u8>> {
    JS.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn json() -> Vec<Vec<u8>> {
    JSON.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn intl() -> Vec<Vec<u8>> {
    INTL.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn temporal() -> Vec<Vec<u8>> {
    TEMPORAL.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn regex() -> Vec<Vec<u8>> {
    REGEX.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn source_maps() -> Vec<Vec<u8>> {
    SOURCE_MAPS.iter().map(|s| s.as_bytes().to_vec()).collect()
}

pub fn import_maps() -> Vec<Vec<u8>> {
    IMPORT_MAPS.iter().map(|s| s.as_bytes().to_vec()).collect()
}

/// The compiled programs of the regex seeds, under each flag set.
pub fn regex_programs() -> Vec<Vec<u8>> {
    use agnijs_js_regex::{Flags, Regex};
    let mut out = Vec::new();
    for s in REGEX {
        let mut lines = s.splitn(3, '\n');
        let flags: Vec<u16> = lines.next().unwrap_or("").encode_utf16().collect();
        let pattern: Vec<u16> = lines.next().unwrap_or("").encode_utf16().collect();
        let Ok(flags) = Flags::parse(&flags) else { continue };
        if let Ok(re) = Regex::compile(&pattern, &flags) {
            out.push(re.program_bytes());
        }
    }
    out
}

/// The code trees of the JavaScript seeds that compile, each behind the
/// byte that picks a script (0) or module (1) container.
pub fn bytecode_trees() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for s in JS {
        let (module, src) = match s.strip_prefix(MODULE_MARKER) {
            Some(m) => (true, m),
            None => (false, *s),
        };
        let code = if module {
            agnijs_js_compiler::compile_module(src, "seed.mjs")
        } else {
            agnijs_js_compiler::compile_script(src, "seed.js")
        };
        if let Ok(code) = code {
            let mut v = vec![u8::from(module)];
            v.extend(agnijs_js_compiler::serialize::encode(&code));
            out.push(v);
        }
    }
    out
}

pub fn snapshot_bodies() -> Vec<Vec<u8>> {
    vec![snapshot_body()]
}

/// The TypeScript crate's fixtures, with the option lines their
/// extensions imply, and some inputs of their own.
#[cfg(feature = "typescript")]
pub fn typescript() -> Vec<Vec<u8>> {
    const FIXTURES: &[(&str, &str)] = &[
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/decorators-prologue.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/decorators.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/enum.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/imports.ts")),
        ("// @options: jsx\n", include_str!("../../agnijs-js-typescript/tests/fixtures/jsx-automatic.tsx")),
        ("// @options: jsx\n", include_str!("../../agnijs-js-typescript/tests/fixtures/jsx-classic.tsx")),
        ("// @options: jsx\n", include_str!("../../agnijs-js-typescript/tests/fixtures/jsx-factory.tsx")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/modern.mts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/namespace-merged.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/namespace.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/param-props.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/refused.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/decorators-expression.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/escapes.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/wrapped.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/nested-modules.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/nameless.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/html-comments.ts")),
        ("// @options: script\n", include_str!("../../agnijs-js-typescript/tests/fixtures/script-modules.cts")),
        ("// @options: script\n", include_str!("../../agnijs-js-typescript/tests/fixtures/script.cts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/strip-only-refused.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/strip-only.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/syntax-error.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/types.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/unicode.ts")),
        ("", include_str!("../../agnijs-js-typescript/tests/fixtures/verbatim.ts")),
    ];
    const MORE: &[&str] = &[
        "let x = f<T>(1) as unknown as number satisfies number;\nlet y = a < b > (c);\nexport {};",
        "abstract class A<T extends object = {}> { abstract m(): void; declare d: T; static #p?: number; override toString() { return ''; } }",
        "declare module 'm' { export const v: number; }\ndeclare global { var g: string }\nexport = 1;",
        "function f(this: Window, ...rest: [a: number, b?: string]): asserts rest is never { throw 0; }\nf.call(globalThis as any);",
        "// @options: script\nenum E { A = 'a'.length, B = A << 2, C }\nnamespace N.M { export let v = E.C; }\nthrow new Error(String(N.M.v));",
        "// @options: jsx automatic source=preact\nexport const v = <><a b={1} {...{c: 2}}>t{'x'}</a></>;",
        "type T<in out U> = { [K in keyof U as `get${string & K}`]-?: () => U[K] };\nlet u: unique symbol;\nexport type { T };",
        "let a = <any>(<number>1);\nlet b = x!.y!;\nlet c = (d?: number): d is 1 => true;\nfor (const k of [] as const) {}",
        "\u{FEFF}let s: string = '\\u2028';\r\nlet t = `${s as string}`;\u{2029}export default t;",
    ];
    FIXTURES
        .iter()
        .map(|(h, f)| format!("{h}{f}").into_bytes())
        .chain(MORE.iter().map(|s| s.as_bytes().to_vec()))
        .collect()
}
