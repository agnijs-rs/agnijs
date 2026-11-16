//! The optional TypeScript front end (ADR 0029), with the `typescript`
//! feature: the transpiler takes source an embedder may not trust.

use super::{accepted, assert_usable, drain_jobs, engine, text, FuzzHost};
use agnijs_js_parser::stack::{StackBudget, DEFAULT_BUDGET, EXHAUSTED_MESSAGE};
use agnijs_js_typescript::{transpile, Error, JsxRuntime, Mode, Options, SourceKind, MAX_DIAGNOSTICS};

/// Options for an input, from its `// @options:` lines, the syntax of the
/// crate's fixtures plus `jsx`. Unknown flags are ignored, so mutants keep
/// running.
fn options(src: &str) -> Options {
    let mut o = Options::default();
    for flags in src.lines().filter_map(|l| l.strip_prefix("// @options:")) {
        for flag in flags.split_whitespace() {
            match flag.split_once('=') {
                Some(("factory", v)) => o.jsx_factory = v.into(),
                Some(("fragment", v)) => o.jsx_fragment_factory = v.into(),
                Some(("source", v)) => o.jsx_import_source = v.into(),
                _ => match flag {
                    "jsx" => o.jsx = true,
                    "strip-only" => o.mode = Mode::StripOnly,
                    "script" => o.kind = SourceKind::Script,
                    "decorators" => o.experimental_decorators = true,
                    "metadata" => o.emit_decorator_metadata = true,
                    "verbatim" => o.verbatim_module_syntax = true,
                    "automatic" => o.jsx_runtime = JsxRuntime::Automatic,
                    _ => {}
                },
            }
        }
    }
    o
}

/// The starts of the engine's messages for early errors Oxc does not
/// check, so that TypeScript with them transpiles and the engine refuses
/// the output: regular expression bodies (Oxc's `regular_expression`
/// parsing is off), exports of names the module never declares or
/// exports twice, and declarations of one name that TypeScript would not
/// merge.
const ENGINE_EARLY_ERRORS: &[(&str, &str)] = &[
    ("invalid regular expression", ""),
    ("export '", "' is not defined in module"),
    ("identifier '", "' has already been declared"),
    ("duplicate export name", ""),
];

/// Lines of `src` at every ECMAScript line terminator.
fn line_count(src: &str) -> u32 {
    let mut n = 1u32;
    let mut cr = false;
    for c in src.chars() {
        match c {
            '\n' if cr => {}
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => n += 1,
            _ => {}
        }
        cr = c == '\r';
    }
    n
}

/// Transpile the input under the options its `// @options:` lines give.
///
/// - Diagnostics: never empty, at most [`MAX_DIAGNOSTICS`], in source
///   order, with byte ranges and lines inside the source.
/// - Output: the same on a second run (ADR 0029: transpiling is
///   deterministic); a source map whose source is the file and its text,
///   and whose lookups land on lines of it; JavaScript our parser accepts
///   as the same kind; and an engine that runs it with the map installed
///   stays usable.
/// - Oxc panicking (`Error::Internal`) is a finding.
pub fn typescript(data: &[u8]) {
    let src = text(data);
    let o = options(&src);
    let module = o.kind == SourceKind::Module;
    let name = match (module, o.jsx) {
        (true, false) => "fuzz.ts",
        (true, true) => "fuzz.tsx",
        (false, _) => "fuzz.cts",
    };
    let lines = line_count(&src);
    let out = match transpile(name, &src, &o) {
        Ok(out) => out,
        Err(Error::Syntax(diags)) => {
            assert!(!diags.is_empty(), "a syntax error with no diagnostics");
            assert!(diags.len() <= MAX_DIAGNOSTICS, "{} diagnostics", diags.len());
            for (i, d) in diags.iter().enumerate() {
                assert_eq!(d.file, name);
                assert!(d.start <= d.end && d.end as usize <= src.len(), "a diagnostic outside the source: {d:?}");
                assert!(d.line >= 1 && d.line <= lines && d.column >= 1, "a diagnostic off the source's lines: {d:?}");
                if i > 0 {
                    assert!(diags[i - 1].start <= d.start, "diagnostics out of order: {diags:?}");
                }
            }
            return;
        }
        Err(Error::InvalidOptions(_) | Error::Resource(_)) => return,
        Err(e @ Error::TooLarge { .. }) => panic!("a fuzz input is over the size limit: {e}"),
        Err(e) => panic!("{e}"),
    };
    accepted();
    assert!(transpile(name, &src, &o) == Ok(out.clone()), "transpiling twice gives different output");

    let map = agnijs_js_debugger::SourceMap::parse(&out.source_map)
        .unwrap_or_else(|e| panic!("the source map does not parse: {e:?}"));
    assert_eq!(map.sources, [name]);
    assert_eq!(map.sources_content.first().and_then(|s| s.as_deref()), Some(src.as_str()));
    for (g, text) in (0u32..).zip(out.code.lines()).take(256) {
        let width = text.encode_utf16().count() as u32;
        for column in [0, 1, 7, width / 2, width.saturating_sub(1), width] {
            if let Some(at) = map.lookup(g, column) {
                assert!(at.line < lines, "{g}:{column} maps to line {} of {lines}", at.line);
            }
        }
    }

    {
        let _budget = StackBudget::enter(DEFAULT_BUDGET);
        let parsed = if module {
            agnijs_js_parser::parse_module(&out.code)
        } else {
            agnijs_js_parser::parse_script(&out.code)
        };
        if let Err(e) = parsed {
            // Nesting deeper than the stack budget is a refusal, and the
            // early errors Oxc leaves to the engine are the engine's to
            // report when it loads the output (ADR 0029); anything else is
            // TypeScript turned into JavaScript that is not.
            assert!(
                e.message == EXHAUSTED_MESSAGE || ENGINE_EARLY_ERRORS.iter().any(|(p, s)| e.message.starts_with(p) && e.message.ends_with(s)),
                "the output does not parse: {}\n{}",
                e.message,
                out.code
            );
            return;
        }
    }

    let host = FuzzHost::new();
    let mut e = engine(&host);
    e.set_source_map(name, Some(&out.source_map)).expect("source maps are supported");
    let realm = e.main_realm();
    if module {
        host.add_module(name, &out.code);
        let _ = e.load_module(realm, name);
    } else {
        let _ = e.eval_script(realm, name, &out.code);
    }
    drain_jobs(&mut *e);
    assert_usable(&mut *e, &host);
}
