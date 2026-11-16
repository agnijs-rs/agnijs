//! Hostile input: the deepest nesting each kind of syntax allows, at the
//! size limit, must come back as output or an error, never as a stack
//! overflow (which would abort the process). ADR 0029 has the measurements
//! the worker stack is sized from.

use agnijs_js_typescript::{transpile, Error, Options, MAX_DIAGNOSTICS};

/// Source of at most `max` bytes: `open` repeated, a core, `close` repeated.
fn nest(max: usize, prefix: &str, open: &str, core: &str, close: &str, suffix: &str) -> String {
    let fixed = prefix.len() + core.len() + suffix.len();
    let n = (max - fixed) / (open.len() + close.len());
    let mut s = String::with_capacity(max);
    s.push_str(prefix);
    s.push_str(&open.repeat(n));
    s.push_str(core);
    s.push_str(&close.repeat(n));
    s.push_str(suffix);
    assert!(s.len() <= max);
    s
}

/// The patterns measured for ADR 0029, plus the TypeScript-only ones.
fn patterns(max: usize) -> Vec<(&'static str, String)> {
    vec![
        ("paren", nest(max, "", "(", "1", ")", "")),
        ("not", nest(max, "let x = ", "!", "1", "", ";")),
        ("neg", nest(max, "let x = ", "- ", "1", "", ";")),
        ("generic", nest(max, "let x: ", "A<", "number", ">", ";")),
        ("arrow", nest(max, "let f = ", "a => ", "1", "", ";")),
        ("plus", nest(max, "let x = 1", "+1", "", "", ";")),
        ("member", nest(max, "let x = a", ".b", "", "", ";")),
        ("call", nest(max, "let x = f", "()", "", "", ";")),
        ("cond", nest(max, "let x = ", "a ? b : ", "0", "", ";")),
        ("brace", nest(max, "", "{", "", "}", "")),
        ("array", nest(max, "let x = ", "[", "", "]", ";")),
        ("object", nest(max, "let x = ", "{a:", "1", "}", ";")),
        ("union", nest(max, "type T = ", "1 | ", "0", "", ";")),
        ("tuple", nest(max, "type T = ", "[", "", "]", ";")),
        ("namespace", nest(max, "", "namespace A {", "", "}", "")),
        ("as", nest(max, "let x = 1", " as any", "", "", ";")),
        ("assertion", nest(max, "let x = ", "<any>", "1", "", ";")),
        ("template", nest(max, "let x = ", "`${", "1", "}`", ";")),
        ("jsx", nest(max, "let x = ", "<a>", "", "</a>", ";")),
        // Statement nesting, which the printer indents.
        ("if", nest(max, "", "if (a) ", "b;", "", "")),
        ("function", nest(max, "", "function f(){", "", "}", "")),
        ("class", nest(max, "", "class A{m(){", "", "}}", "")),
        ("switch", nest(max, "", "switch(a){case 1:", "", "}", "")),
        ("label", nest(max, "", "a: ", ";", "", "")),
    ]
}

fn assert_survives(max: usize) {
    for (name, src) in patterns(max) {
        // `AGNIJS_TS_PATTERN=switch` runs one pattern; `--nocapture` shows
        // the time each takes.
        if std::env::var("AGNIJS_TS_PATTERN").is_ok_and(|p| p != name) {
            continue;
        }
        let t = std::time::Instant::now();
        let mut options = Options::default();
        options.jsx = name == "jsx";
        options.max_source_len = max;
        match transpile("deep.ts", &src, &options) {
            // The output stays proportional to the input: the printer's
            // indentation is dropped for nesting this deep.
            Ok(out) => assert!(
                out.code.len() <= 8 * src.len()
                    && out.source_map.len() <= 16 * src.len() + (1 << 20),
                "{name}: {} bytes of code and {} of map for {} of source",
                out.code.len(),
                out.source_map.len(),
                src.len()
            ),
            Err(Error::Syntax(_)) => {}
            Err(e) => panic!("{name}: {e}"),
        }
        eprintln!("{name}: {:?}", t.elapsed());
    }
}

#[test]
fn the_deepest_nesting_at_the_default_limit_does_not_overflow() {
    assert_survives(Options::default().max_source_len);
}

#[test]
fn a_source_over_the_limit_is_refused_before_parsing() {
    let mut options = Options::default();
    options.max_source_len = 8;
    assert_eq!(
        transpile("a.ts", "let x = 1;", &options),
        Err(Error::TooLarge { len: 10, max: 8 })
    );
}

#[test]
fn an_error_on_every_few_bytes_reports_the_first_ones_only() {
    // One duplicate-label error per three bytes, all on one line.
    let src = "a: ".repeat(300_000) + ";";
    match transpile("labels.ts", &src, &Options::default()) {
        Err(Error::Syntax(d)) => {
            assert_eq!(d.len(), MAX_DIAGNOSTICS);
            assert!(d.windows(2).all(|w| w[0].start <= w[1].start));
            assert_eq!(d[0].line, 1);
        }
        other => panic!("{other:?}"),
    }
}
