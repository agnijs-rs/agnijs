//! The workload registry.
//!
//! Every workload uses only the ECMAScript standard library (no host
//! globals), so it runs unchanged on every engine. A script workload's `run`
//! expression returns a checksum; the runner checks that every engine
//! computes the same one, so a workload that silently does less work on one
//! engine shows up as a mismatch rather than as a fast time.

use serde::{Deserialize, Serialize};

/// What area of the engine a workload exercises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Loops,
    PropertyAccess,
    Strings,
    Collections,
    Calls,
    Regex,
    Json,
    Promises,
    Bytecode,
    Startup,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::Loops,
        Category::PropertyAccess,
        Category::Strings,
        Category::Collections,
        Category::Calls,
        Category::Regex,
        Category::Json,
        Category::Promises,
        Category::Bytecode,
        Category::Startup,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Loops => "loops",
            Category::PropertyAccess => "property-access",
            Category::Strings => "strings",
            Category::Collections => "collections",
            Category::Calls => "calls",
            Category::Regex => "regex",
            Category::Json => "json",
            Category::Promises => "promises",
            Category::Bytecode => "bytecode",
            Category::Startup => "startup",
        }
    }
}

/// How a workload is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `setup` is evaluated once in a fresh engine (after the scale
    /// prelude); each iteration evaluates `run` and then drains the job
    /// queue. The checksum is the string value of `check` evaluated after
    /// the drain when given (for work done in jobs), else of `run`'s result.
    Script { setup: &'static str, run: &'static str, check: Option<&'static str> },
    /// Each iteration creates an engine with its main realm and evaluates a
    /// trivial script.
    Startup,
    /// Each iteration compiles [`generated_source`] from source and runs it.
    CompileSource,
    /// [`generated_source`] is compiled to bytecode once; each iteration
    /// loads that bytecode and runs it.
    BytecodeLoad,
}

#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub name: &'static str,
    pub category: Category,
    pub description: &'static str,
    pub kind: Kind,
}

/// Evaluated before a script workload's setup. `scaled(n)` sizes the work so
/// the smoke test can run every workload in a fraction of its full size.
pub fn prelude(scale: f64) -> String {
    format!(
        "var SCALE = {scale:?};\n\
         function scaled(base) {{ return Math.max(1, Math.floor(base * SCALE)); }}\n"
    )
}

/// Select workloads by filter: no filters selects everything; otherwise a
/// workload is selected when any filter is a substring of its name or equals
/// its category name.
pub fn select<'a>(all: &'a [Workload], filters: &[String]) -> Vec<&'a Workload> {
    all.iter()
        .filter(|w| {
            filters.is_empty()
                || filters.iter().any(|f| w.name.contains(f.as_str()) || w.category.name() == f)
        })
        .collect()
}

const fn script(
    name: &'static str,
    category: Category,
    description: &'static str,
    setup: &'static str,
    run: &'static str,
) -> Workload {
    Workload { name, category, description, kind: Kind::Script { setup, run, check: None } }
}

const fn job_script(
    name: &'static str,
    category: Category,
    description: &'static str,
    setup: &'static str,
    run: &'static str,
    check: &'static str,
) -> Workload {
    Workload { name, category, description, kind: Kind::Script { setup, run, check: Some(check) } }
}

/// Every workload, in report order.
pub fn registry() -> Vec<Workload> {
    vec![
        script(
            "loop-arith",
            Category::Loops,
            "Tight counting loop with integer arithmetic and modulo",
            r#"
function bench() {
  var s = 0, n = scaled(3000000);
  for (var i = 0; i < n; i++) { s = (s + i * 7) % 1000003; }
  return s;
}"#,
            "bench()",
        ),
        script(
            "loop-array-index",
            Category::Loops,
            "Nested loops reading a dense numeric array",
            r#"
var data = []; for (var i = 0; i < 1000; i++) data.push(i);
function bench() {
  var s = 0, rounds = scaled(1500);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < data.length; i++) { s = (s + data[i] * r) | 0; }
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "prop-monomorphic",
            Category::PropertyAccess,
            "Property reads on objects of one shape (inline-cache hits)",
            r#"
function Point(x, y) { this.x = x; this.y = y; }
var pts = []; for (var i = 0; i < 1000; i++) pts.push(new Point(i, i * 2));
function bench() {
  var s = 0, rounds = scaled(1000);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < pts.length; i++) { var p = pts[i]; s = (s + p.x + p.y) | 0; }
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "prop-polymorphic",
            Category::PropertyAccess,
            "Property reads at one site over four object shapes",
            r#"
var objs = [];
for (var i = 0; i < 1000; i++) {
  switch (i % 4) {
    case 0: objs.push({ a: i, b: 1 }); break;
    case 1: objs.push({ b: 1, a: i }); break;
    case 2: objs.push({ a: i, b: 1, c: 2 }); break;
    default: objs.push({ c: 2, b: 1, a: i });
  }
}
function bench() {
  var s = 0, rounds = scaled(1000);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < objs.length; i++) { s = (s + objs[i].a + objs[i].b) | 0; }
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "prop-write-alloc",
            Category::PropertyAccess,
            "Allocate small objects, add, write and read properties (GC pressure)",
            r#"
function bench() {
  var s = 0, n = scaled(300000);
  for (var i = 0; i < n; i++) {
    var o = { id: i, name: 'n', next: null };
    o.value = i * 2; o.next = { id: i + 1 };
    s = (s + o.value + o.next.id) | 0;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "string-concat",
            Category::Strings,
            "Build long strings with += and template literals",
            r#"
function bench() {
  var total = 0, rounds = scaled(200);
  for (var r = 0; r < rounds; r++) {
    var s = '';
    for (var i = 0; i < 1000; i++) { s += `item-${i};`; }
    total += s.length + s.charCodeAt(s.length - 2);
  }
  return total;
}"#,
            "bench()",
        ),
        script(
            "string-methods",
            Category::Strings,
            "split, join, indexOf, slice, replace and case conversion",
            r#"
var text = []; for (var i = 0; i < 200; i++) text.push('Word' + i); text = text.join(' ');
function bench() {
  var s = 0, rounds = scaled(1000);
  for (var r = 0; r < rounds; r++) {
    var parts = text.split(' ');
    s += parts.length + text.indexOf('Word1' + (r % 100)) + text.slice(10, 40).toUpperCase().length;
    s += parts.join('-').toLowerCase().length + text.replace('Word5', 'X').length;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "array-methods",
            Category::Collections,
            "Array push, map, filter, reduce and numeric sort",
            r#"
function bench() {
  var s = 0, rounds = scaled(300);
  for (var r = 0; r < rounds; r++) {
    var a = [];
    for (var i = 0; i < 1000; i++) a.push((i * 7919 + r) % 1000);
    var b = a.map(function (x) { return x * 2; }).filter(function (x) { return x % 3 !== 0; });
    b.sort(function (x, y) { return x - y; });
    s = (s + b.reduce(function (acc, x) { return acc + x; }, 0) + b[0] + b[b.length - 1]) | 0;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "map-set",
            Category::Collections,
            "Map set/get with number and string keys, Set add/has/delete",
            r#"
function bench() {
  var s = 0, rounds = scaled(100);
  for (var r = 0; r < rounds; r++) {
    var m = new Map(), st = new Set();
    for (var i = 0; i < 1000; i++) { m.set(i, i * 2); m.set('k' + i, i); st.add(i * 3); }
    for (var i = 0; i < 1000; i++) {
      s = (s + m.get(i) + m.get('k' + i)) | 0;
      if (st.has(i)) { st.delete(i); s++; }
    }
    s = (s + m.size + st.size) | 0;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "closures",
            Category::Calls,
            "Create closures over mutable captured variables and call them",
            r#"
function makeCounter(start) { var c = start; return function (d) { c += d; return c; }; }
function bench() {
  var s = 0, n = scaled(100000);
  for (var i = 0; i < n; i++) {
    var f = makeCounter(i);
    f(1); s = (s + f(2)) | 0;
    var g = (x) => x + i; s = (s + g(3)) | 0;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "calls-recursive",
            Category::Calls,
            "Recursive function calls (fib)",
            r#"
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
function bench() {
  var s = 0, rounds = scaled(30);
  for (var r = 0; r < rounds; r++) s += fib(20);
  return s;
}"#,
            "bench()",
        ),
        script(
            "method-calls",
            Category::Calls,
            "Class instances: method calls, getters and super calls",
            r#"
class Shape { constructor(n) { this.n = n; } area() { return this.n; } get double() { return this.n * 2; } }
class Square extends Shape { area() { return super.area() * this.n; } }
var shapes = []; for (var i = 0; i < 100; i++) shapes.push(i % 2 ? new Shape(i) : new Square(i));
function bench() {
  var s = 0, rounds = scaled(3000);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < shapes.length; i++) { s = (s + shapes[i].area() + shapes[i].double) | 0; }
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "regex",
            Category::Regex,
            "Regex test, exec with captures and global replace",
            r#"
var lines = [];
for (var i = 0; i < 200; i++) lines.push('user' + i + '@example' + (i % 7) + '.com 2026-11-' + (10 + i % 20) + ' id=' + i * 13);
var email = /^([a-z]+\d+)@([a-z]+\d)\.com/, date = /(\d{4})-(\d{2})-(\d{2})/, digits = /\d+/g;
function bench() {
  var s = 0, rounds = scaled(100);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < lines.length; i++) {
      var l = lines[i], m = email.exec(l), d = date.exec(l);
      if (m) s += m[1].length + m[2].length;
      if (d) s += +d[3];
      if (/id=\d+$/.test(l)) s++;
      s += l.replace(digits, '#').length;
    }
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "regex-tokenize",
            Category::Regex,
            "Sticky-regex tokenizer over source text (ordinary patterns)",
            r#"
var src = '';
for (var i = 0; i < 300; i++) src += 'var x' + i + ' = foo(' + i + ', "s' + i + '") + 3.5e' + (i % 9) + '; // note\n';
var rules = [/\s+/y, /\/\/[^\n]*/y, /[A-Za-z_$][\w$]*/y, /\d+(?:\.\d+)?(?:e[+-]?\d+)?/y, /"(?:[^"\\]|\\.)*"/y, /[=+();,.]/y];
function bench() {
  var n = 0, rounds = scaled(4);
  for (var r = 0; r < rounds; r++) {
    var pos = 0;
    while (pos < src.length) {
      for (var k = 0; k < rules.length; k++) {
        var re = rules[k]; re.lastIndex = pos;
        var m = re.exec(src);
        if (m) { pos += m[0].length; n += k; break; }
      }
      if (k === rules.length) pos++;
    }
  }
  return n;
}"#,
            "bench()",
        ),
        script(
            "regex-long-input",
            Category::Regex,
            "Searches over a 40k-character text, including a trailing-space trim",
            r#"
var words = [];
for (var i = 0; i < 6000; i++) words.push(i % 97 === 0 ? 'mail' + i + '@host.com' : 'w' + (i % 13));
var text = words.join(' ') + '   x';
var mail = /\b[a-z]+\d+@[a-z]+\.com\b/g, trail = /\s+$/, num = /\d{3,}/g;
function bench() {
  var s = 0, rounds = scaled(3);
  for (var r = 0; r < rounds; r++) {
    s += text.match(mail).length;
    s += trail.test(text) ? 1 : 0;
    s += text.replace(num, '#').length;
  }
  return s;
}"#,
            "bench()",
        ),
        script(
            "regex-redos",
            Category::Regex,
            "Nested quantifier on a failing input (/(a+)+$/, ADR 0013)",
            r#"
var input = 'a'.repeat(20) + '!';
function bench() {
  var s = 0, rounds = scaled(2);
  for (var r = 0; r < rounds; r++) s += /(a+)+$/.test(input) ? 1 : 0;
  return s + rounds;
}"#,
            "bench()",
        ),
        script(
            "json-stringify",
            Category::Json,
            "JSON.stringify of a nested object tree",
            r#"
function tree(depth, i) {
  if (depth === 0) return { id: i, name: 'leaf' + i, tags: ['a', 'b'], ok: i % 2 === 0, v: i / 4 };
  var kids = []; for (var k = 0; k < 4; k++) kids.push(tree(depth - 1, i * 4 + k));
  return { id: i, kids: kids, meta: { depth: depth, label: 'node' + i } };
}
var doc = tree(5, 1);
function bench() {
  var s = 0, rounds = scaled(50);
  for (var r = 0; r < rounds; r++) s += JSON.stringify(doc).length;
  return s;
}"#,
            "bench()",
        ),
        script(
            "json-parse",
            Category::Json,
            "JSON.parse of the same nested document",
            r#"
function tree(depth, i) {
  if (depth === 0) return { id: i, name: 'leaf' + i, tags: ['a', 'b'], ok: i % 2 === 0, v: i / 4 };
  var kids = []; for (var k = 0; k < 4; k++) kids.push(tree(depth - 1, i * 4 + k));
  return { id: i, kids: kids, meta: { depth: depth, label: 'node' + i } };
}
var text = JSON.stringify(tree(5, 1));
function bench() {
  var s = 0, rounds = scaled(50);
  for (var r = 0; r < rounds; r++) { var o = JSON.parse(text); s += o.kids.length + o.kids[3].kids[2].meta.depth; }
  return s + text.length;
}"#,
            "bench()",
        ),
        job_script(
            "promise-chain",
            Category::Promises,
            "Long .then chains and Promise.all, drained by the host job queue",
            r#"
var result = 0;
function bench() {
  result = 0;
  var p = Promise.resolve(0), n = scaled(20000);
  for (var i = 0; i < n; i++) p = p.then(function (v) { return v + 1; });
  var all = []; for (var i = 0; i < 1000; i++) all.push(Promise.resolve(i));
  Promise.all(all).then(function (vs) { result += vs.length; });
  p.then(function (v) { result += v; });
}"#,
            "bench()",
            "result",
        ),
        job_script(
            "async-await",
            Category::Promises,
            "async functions awaiting in a loop, drained by the host job queue",
            r#"
var result = 0;
async function step(i) { await null; return i; }
async function loop(n) { var s = 0; for (var i = 0; i < n; i++) s += await step(i); result = s; }
function bench() { result = 0; loop(scaled(20000)); }"#,
            "bench()",
            "result",
        ),
        Workload {
            name: "compile-source",
            category: Category::Bytecode,
            description: "Parse, compile and run a generated ~100 KB script from source",
            kind: Kind::CompileSource,
        },
        Workload {
            name: "bytecode-load",
            category: Category::Bytecode,
            description: "Load and run the same script from precompiled bytecode",
            kind: Kind::BytecodeLoad,
        },
        Workload {
            name: "engine-startup",
            category: Category::Startup,
            description: "Create an engine with its main realm and run a trivial script",
            kind: Kind::Startup,
        },
    ]
}

/// A medium-sized, deterministic script (a few hundred small functions and
/// a short top-level driver) for the compile-from-source versus
/// bytecode-load comparison. Its completion value is a checksum.
pub fn generated_source() -> String {
    let mut out = String::with_capacity(128 * 1024);
    for i in 0..300 {
        out.push_str(&format!(
            "function f_{i}(a, b) {{\n  var x = a * {i} + b, list = [a, b, {i}];\n  if (x % 3 === 0) {{ return x + {i}; }}\n  for (var k = 0; k < 3; k++) {{ x = (x ^ k) + list[k % 3]; }}\n  var o = {{ p: x, q: list, r: 'name{i}', s: function (y) {{ return y * {i}; }} }};\n  var arrow = (z) => z + o.p;\n  switch (x & 3) {{ case 0: x += 1; break; case 1: x -= 1; break; default: x = arrow(x); }}\n  try {{ if (x < 0) throw new Error('negative ' + x); }} catch (e) {{ x = -x; }}\n  return (o.p + o.q.length + o.r.length + o.s(2) + x) | 0;\n}}\n"
        ));
    }
    out.push_str("var acc = 0;\n");
    for i in 0..300 {
        out.push_str(&format!("acc = (acc + f_{i}({i}, 2)) | 0;\n"));
    }
    out.push_str("acc;\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn workload_names_are_unique() {
        let all = registry();
        let names: HashSet<_> = all.iter().map(|w| w.name).collect();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn every_category_has_a_workload() {
        let all = registry();
        for c in Category::ALL {
            assert!(all.iter().any(|w| w.category == c), "no workload for {}", c.name());
        }
    }

    #[test]
    fn no_filter_selects_everything() {
        let all = registry();
        assert_eq!(select(&all, &[]).len(), all.len());
    }

    #[test]
    fn a_filter_selects_by_name_substring_or_category() {
        let all = vec![
            Workload { name: "json-parse", category: Category::Json, description: "", kind: Kind::Startup },
            Workload { name: "json-stringify", category: Category::Json, description: "", kind: Kind::Startup },
            Workload { name: "regex-match", category: Category::Regex, description: "", kind: Kind::Startup },
            Workload { name: "engine-startup", category: Category::Startup, description: "", kind: Kind::Startup },
        ];
        let names = |f: &[&str]| -> Vec<&str> {
            let f: Vec<String> = f.iter().map(|s| s.to_string()).collect();
            select(&all, &f).iter().map(|w| w.name).collect()
        };
        assert_eq!(names(&["parse"]), ["json-parse"]);
        assert_eq!(names(&["json"]), ["json-parse", "json-stringify"]);
        assert_eq!(names(&["startup", "match"]), ["regex-match", "engine-startup"]);
        assert!(names(&["nothing"]).is_empty());
    }

    #[test]
    fn the_generated_source_is_deterministic_and_nontrivial() {
        let a = generated_source();
        assert_eq!(a, generated_source());
        assert!(a.len() > 20_000, "only {} bytes", a.len());
    }
}
