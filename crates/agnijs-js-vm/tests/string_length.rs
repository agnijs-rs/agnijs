//! The maximum string length (TASK-103.26): no operation makes a string
//! longer than `MAX_STRING_LENGTH` (2^30 code units). Each one throws a
//! catchable `RangeError: Invalid string length` instead, and no length is
//! ever truncated or wraps.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, Value};
use agnijs_js_vm::{MAX_STRING_LENGTH, Vm};

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    // Some cases loop; under AGNIJS_GC_STRESS each step would collect.
    vm.heap.stress = false;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> Result<Value, Error> {
    let realm = vm.main_realm();
    vm.eval_script(realm, "t.js", src)
}

/// `src` throws `RangeError: Invalid string length`, catchably, and the
/// engine runs ordinary code afterwards.
fn too_long(vm: &mut Vm, src: &str) {
    let caught = format!(
        "{{ let r; try {{ r = (() => {{ {src} }})(); }} catch (e) {{ r = e }}
         r instanceof RangeError ? r.message : 'no error: ' + typeof r + ' ' + (r && r.length) }}"
    );
    match eval(vm, &caught) {
        Ok(v) => assert_eq!(v, Value::string("Invalid string length"), "{src}"),
        Err(e) => panic!("{src}: {e:?}"),
    }
    let v = eval(vm, "'ab' + 'cd'").unwrap();
    assert_eq!(v, Value::string("abcd"), "after {src}");
}

/// `src` returns a string of exactly `len` code units.
fn fits(vm: &mut Vm, src: &str, len: usize) {
    let v = eval(vm, &format!("(() => {{ {src} }})().length"))
        .unwrap_or_else(|e| panic!("{src}: {e:?}"));
    assert_eq!(v, Value::Number(len as f64), "{src}");
}

#[test]
fn the_maximum_is_2_to_the_30() {
    assert_eq!(MAX_STRING_LENGTH, 1 << 30);
    // Two maximal lengths add up without wrapping a u32.
    assert!(MAX_STRING_LENGTH * 2 <= u32::MAX as usize);
}

/// The probe from the review: doubling a 1 Mi-unit string twelve times
/// gave `length` 0. Ropes keep this cheap: nothing is flattened.
#[test]
fn doubling_past_the_maximum_throws_instead_of_wrapping() {
    let mut vm = vm();
    too_long(
        &mut vm,
        "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 12; i++) s = s + s; return s",
    );
    // The last doubling that fits keeps its full length.
    fits(
        &mut vm,
        "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 10; i++) s = s + s; return s",
        MAX_STRING_LENGTH,
    );
    too_long(
        &mut vm,
        "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 11; i++) s += s; return s",
    );
    too_long(
        &mut vm,
        "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 11; i++) s = `${s}${s}`; return s",
    );
}

#[test]
fn one_unit_past_the_maximum_throws() {
    let mut vm = vm();
    let max = "let s = 'x'.repeat(1 << 20); for (let i = 0; i < 10; i++) s = s + s;";
    fits(&mut vm, &format!("{max} return s"), MAX_STRING_LENGTH);
    too_long(&mut vm, &format!("{max} return s + 'y'"));
    too_long(&mut vm, &format!("{max} return 'y' + s"));
    too_long(&mut vm, &format!("{max} return s.concat('y')"));
    too_long(&mut vm, &format!("{max} return `${{s}}y`"));
    // A rope one unit short of the maximum (2^0 + ... + 2^29), never
    // flattened: one more unit fits, two do not.
    let short = "let p = 'x', s = ''; for (let k = 0; k < 30; k++) { s = s + p; p = p + p; }";
    fits(&mut vm, &format!("{short} return s"), MAX_STRING_LENGTH - 1);
    fits(
        &mut vm,
        &format!("{short} return s + 'y'"),
        MAX_STRING_LENGTH,
    );
    too_long(&mut vm, &format!("{short} return s + 'yy'"));
}

/// Results short enough to be copied rather than roped are checked too.
#[test]
fn short_concatenations_check_the_maximum() {
    let mut vm = vm();
    vm.set_max_string_length(40);
    fits(&mut vm, "return 'a'.repeat(20) + 'b'.repeat(20)", 40);
    too_long(&mut vm, "return 'a'.repeat(20) + 'b'.repeat(21)");
    too_long(&mut vm, "return `${'a'.repeat(20)}${'b'.repeat(21)}`");
}

/// `join` with a long separator over a huge sparse array would build a
/// terabyte-sized buffer; it is refused before anything is built.
#[test]
fn join_is_refused_before_it_builds_anything() {
    let mut vm = vm();
    too_long(&mut vm, "return Array(1 << 20).join('x'.repeat(1 << 20))");
    too_long(&mut vm, "return new Array(2 ** 31).join()");
    too_long(&mut vm, "return 'x'.repeat(2 ** 30 + 1)");
    too_long(&mut vm, "return 'x'.padEnd(2 ** 30 + 1)");
}

/// Every string-creating path, under a test-only smaller maximum: one
/// unit past it throws, and exactly the maximum fits.
#[test]
fn every_path_that_makes_strings_checks_the_maximum() {
    const MAX: usize = 1000;
    let mut vm = vm();
    vm.set_max_string_length(MAX);
    let a = "const a = 'a'.repeat(500), b = 'b'.repeat(501), c = a + 'a';";
    let cases: &[(&str, &str)] = &[
        // Concatenation, short and rope.
        ("concat +", "return a + b"),
        ("concat +=", "let s = a; s += b; return s"),
        ("template", "return `${a}${b}`"),
        ("String.prototype.concat", "return a.concat(b)"),
        (
            "String.prototype.concat many",
            "return ''.concat(a, 'x', a)",
        ),
        // Builders.
        ("join", "return [a, b].join('')"),
        ("join separator only", "return Array(1002).join('x')"),
        ("Array toString", "return [a, b].toString()"),
        ("typed array join", "return new Uint8Array(501).join()"),
        ("replace string", "return a.replace('a', 'xx' + b)"),
        ("replace regexp", "return c.replace(/a/g, 'xx')"),
        ("replaceAll", "return c.replaceAll('a', 'xx')"),
        ("replace function", "return c.replace(/a/g, () => 'xx')"),
        ("repeat", "return 'ab'.repeat(501)"),
        ("padStart", "return 'a'.padStart(1001, 'xy')"),
        ("padEnd", "return 'a'.padEnd(1001)"),
        ("JSON.stringify", "return JSON.stringify([a, b])"),
        (
            "JSON.stringify indent",
            "return JSON.stringify(Array(300).fill(0), null, 4)",
        ),
        (
            "JSON.stringify quote",
            "return JSON.stringify('\\n'.repeat(500))",
        ),
        (
            "String.fromCharCode",
            "return String.fromCharCode(...Array(1001).fill(65))",
        ),
        (
            "String.fromCodePoint",
            "return String.fromCodePoint(...Array(501).fill(0x1F600))",
        ),
        ("String.raw", "return String.raw({ raw: [a, b] })"),
        // Mappings that grow their input.
        ("toUpperCase", "return '\\u00df'.repeat(501).toUpperCase()"),
        ("normalize", "return '\\u00e9'.repeat(501).normalize('NFD')"),
        ("encodeURI", "return encodeURI('\\u00e9'.repeat(167))"),
        (
            "encodeURIComponent",
            "return encodeURIComponent(' '.repeat(334))",
        ),
        ("escape", "return escape('\\u0100'.repeat(167))"),
        ("RegExp source", "return new RegExp('/'.repeat(501)).source"),
        (
            "Error toString",
            "return String(new Error('a'.repeat(994)))",
        ),
        (
            "Symbol toString",
            "return Symbol('a'.repeat(993)).toString()",
        ),
        (
            "Function toString",
            "return String(Function('a'.repeat(1000), ''))",
        ),
    ];
    for (what, src) in cases {
        too_long(&mut vm, &format!("/* {what} */ {a} {src}"));
    }
    // Exactly the maximum is allowed, and lengths are exact.
    fits(&mut vm, &format!("{a} return a + b.slice(1)"), MAX);
    fits(&mut vm, "return Array(1001).join('x')", MAX);
    fits(&mut vm, "return 'ab'.repeat(500)", MAX);
    fits(
        &mut vm,
        "return String.fromCharCode(...Array(1000).fill(65))",
        MAX,
    );
    fits(&mut vm, "return JSON.stringify('a'.repeat(998))", MAX);
}

/// The host sees the same RangeError when a script's result is too long.
#[test]
fn the_host_gets_a_range_error() {
    let mut vm = vm();
    vm.set_max_string_length(100);
    match eval(&mut vm, "'a'.repeat(60) + 'b'.repeat(60)") {
        Err(Error::Exception(x)) => {
            assert_eq!(
                (x.name.as_str(), x.message.as_str()),
                ("RangeError", "Invalid string length")
            )
        }
        other => panic!("{other:?}"),
    }
    // A string the host passes in is not silently cut either.
    let realm = vm.main_realm();
    let long = Value::string(&"z".repeat(101));
    let global = vm.global_object(realm).unwrap();
    let key = agnijs_engine_api::PropertyKey::from("long");
    let r = Engine::set(&mut vm, realm, &global, &key, &long);
    assert!(r.is_err() || eval(&mut vm, "long.length").is_err(), "{r:?}");
}
