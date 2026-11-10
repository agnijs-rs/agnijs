//! The `intl` cargo feature (docs/binary-size.md). With it the engine has
//! ECMA-402 and the locale-sensitive methods go through it; without it
//! there is no `Intl` global, ICU4X is not linked, and those methods take
//! the plain ECMA-262 behaviour the language spec allows.
//!
//! `cargo test -p agnijs-js-vm --test intl_feature` checks the first;
//! `cargo test -p agnijs-js-vm --no-default-features --features linear-regex --test intl_feature`
//! the second.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn eval(src: &str) -> Value {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    vm.eval_script(realm, "intl-feature.js", src)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn check(src: &str, expected: &str) {
    assert_eq!(eval(src), Value::string(expected), "{src}");
}

#[cfg(feature = "intl")]
#[test]
fn with_intl_the_locale_methods_are_locale_sensitive() {
    check("typeof Intl", "object");
    check(
        "new Date(0).toLocaleDateString('en-US', { timeZone: 'UTC' })",
        "1/1/1970",
    );
    // Case mapping is not locale data: Turkish dotless i in every build.
    check("'I'.toLocaleLowerCase('tr')", "\u{131}");
    // German needs German data, which a trimmed build may not have.
    if configured_locales().is_none_or(|l| l.iter().any(|t| t == "de" || t == "recommended")) {
        check("(1234.5).toLocaleString('de')", "1.234,5");
        check("(12345n).toLocaleString('de')", "12.345");
    } else {
        check("(1234.5).toLocaleString('de')", "1,234.5");
    }
}

/// The locale list a custom `ICU4X_DATA_DIR` was generated for
/// (`crates/agnijs-js-intl/tools/gen-icu4x-data.js` writes it beside the
/// data), or `None` for the crate-default data.
#[cfg(feature = "intl")]
fn configured_locales() -> Option<Vec<String>> {
    let dir = option_env!("ICU4X_DATA_DIR")?;
    let file = std::path::Path::new(dir).join("agnijs-locales.txt");
    let list = std::fs::read_to_string(&file).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; generate the data with gen-icu4x-data.js",
            file.display()
        )
    });
    Some(
        list.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

#[cfg(feature = "intl")]
#[test]
fn the_build_supports_exactly_the_locales_its_data_was_generated_for() {
    const PROBE: [&str; 8] = ["en", "de", "fr", "es", "ja", "zh", "ar", "sw"];
    let src = format!("Intl.NumberFormat.supportedLocalesOf({PROBE:?}).join()");
    let expected: Vec<&str> = match configured_locales() {
        Some(list) if !["recommended", "modern", "full"].contains(&list[0].as_str()) => PROBE
            .iter()
            .copied()
            .filter(|p| list.iter().any(|l| l == p))
            .collect(),
        // The crate-default data (ICU4X's recommended set) or a named set
        // at least as wide: every probe.
        _ => PROBE.to_vec(),
    };
    check(&src, &expected.join(","));
    // A locale the data lacks falls back to the default; it never fails.
    assert_eq!(
        eval("new Intl.NumberFormat('sw').format(1) === '1'"),
        Value::Bool(true)
    );
}

/// How many arguments Array and %TypedArray% toLocaleString pass to each
/// element's toLocaleString: ECMA-402 forwards `locales` and `options`,
/// ECMA-262 alone passes none.
const ELEMENT_ARGUMENT_COUNTS: &str = "var counts = [];
     Number.prototype.toLocaleString = function () { counts.push(arguments.length); return 'n'; };
     [1].toLocaleString(); [2].toLocaleString('de', {}); new Uint8Array(1).toLocaleString();
     counts.join()";

#[cfg(feature = "intl")]
#[test]
fn with_intl_array_to_locale_string_forwards_locales_and_options() {
    check(ELEMENT_ARGUMENT_COUNTS, "2,2,2");
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_array_to_locale_string_passes_no_arguments() {
    check(ELEMENT_ARGUMENT_COUNTS, "0,0,0");
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_there_is_no_intl_global() {
    check("typeof Intl", "undefined");
    assert_eq!(eval("'Intl' in globalThis"), Value::Bool(false));
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_number_and_bigint_to_locale_string_are_to_string() {
    check("(1234.5).toLocaleString('de')", "1234.5");
    check("(-0.000001).toLocaleString()", "-0.000001");
    check(
        "(12345n).toLocaleString('de', { style: 'currency' })",
        "12345",
    );
    check("[1234.5, 2n, null, 'x'].toLocaleString()", "1234.5,2,,x");
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_date_to_locale_strings_are_the_ecma262_forms() {
    check(
        "var d = new Date(2020, 1, 29, 13, 5, 7);
         [d.toLocaleString() === d.toString(),
          d.toLocaleDateString('ja') === d.toDateString(),
          d.toLocaleTimeString(undefined, { hour: 'numeric' }) === d.toTimeString()].join()",
        "true,true,true",
    );
    check("new Date(NaN).toLocaleString()", "Invalid Date");
    check("new Date(NaN).toLocaleDateString()", "Invalid Date");
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_locale_compare_orders_canonically_equivalent_strings_equal() {
    check(
        "['a'.localeCompare('b'), 'b'.localeCompare('a'), 'a'.localeCompare('a'),
          'A\\u030A'.localeCompare('\\u00C5'), 'o\\u0308'.localeCompare('\\u00F6', 'de')].join()",
        "-1,1,0,0,0",
    );
}

#[cfg(not(feature = "intl"))]
#[test]
fn without_intl_locale_case_mapping_is_the_root_mapping() {
    check("'I'.toLocaleLowerCase('tr')", "i");
    check("'i'.toLocaleUpperCase('tr')", "I");
    check("'\\u00DF'.toLocaleUpperCase()", "SS");
    // No locale list is read, so an invalid tag does not throw.
    check("'A'.toLocaleLowerCase('not a tag!')", "a");
}
