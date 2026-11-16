//! Byte-for-byte fixtures: every `tests/fixtures/<name>.{ts,mts,cts,tsx}`
//! is transpiled twice and must give exactly `<name>.out` both times, on
//! every platform (ADR 0029, determinism). `AGNIJS_BLESS=1` rewrites the
//! `.out` files instead of comparing.
//!
//! A fixture's `// @options:` lines set options:
//! `// @options: strip-only script decorators metadata verbatim automatic factory=h fragment=Fragment`.
//! `.out` holds the code and the map, or the error as `Display` prints it.

use agnijs_js_typescript::{transpile, JsxRuntime, Mode, Options, SourceKind};
use std::path::Path;

fn options_for(name: &str, source: &str) -> Options {
    let mut o = Options::for_path(name).expect("a TypeScript extension");
    for flags in source
        .lines()
        .filter_map(|l| l.strip_prefix("// @options:"))
    {
        for flag in flags.split_whitespace() {
            match flag.split_once('=') {
                Some(("factory", v)) => o.jsx_factory = v.into(),
                Some(("fragment", v)) => o.jsx_fragment_factory = v.into(),
                Some(("source", v)) => o.jsx_import_source = v.into(),
                _ => match flag {
                    "strip-only" => o.mode = Mode::StripOnly,
                    "script" => o.kind = SourceKind::Script,
                    "decorators" => o.experimental_decorators = true,
                    "metadata" => o.emit_decorator_metadata = true,
                    "verbatim" => o.verbatim_module_syntax = true,
                    "automatic" => o.jsx_runtime = JsxRuntime::Automatic,
                    other => panic!("{name}: unknown option {other}"),
                },
            }
        }
    }
    o
}

fn render(name: &str, source: &str) -> String {
    match transpile(name, source, &options_for(name, source)) {
        Ok(out) => format!("// code\n{}// map\n{}\n", out.code, out.source_map),
        Err(e) => format!("// error\n{e}\n"),
    }
}

#[test]
fn fixtures_match_byte_for_byte() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bless = std::env::var_os("AGNIJS_BLESS").is_some();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| Options::for_path(n).is_some())
        .collect();
    names.sort();
    assert!(names.len() >= 10, "fixtures missing from {}", dir.display());
    let mut failures = Vec::new();
    for name in &names {
        let source = std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(
            !source.contains('\r'),
            "{name} has CR line ends; tests/.gitattributes keeps them out"
        );
        let first = render(name, &source);
        assert_eq!(first, render(name, &source), "{name}: two runs differ");
        let out = dir.join(format!("{}.out", name.rsplit_once('.').unwrap().0));
        if bless {
            std::fs::write(&out, &first).unwrap();
        } else if std::fs::read_to_string(&out).ok().as_deref() != Some(first.as_str()) {
            failures.push(format!(
                "{name}: output differs from {}:\n{first}",
                out.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn crlf_and_cr_line_ends_map_like_lf() {
    // The map counts lines with the ECMAScript terminators, as the engine does.
    let lf = transpile(
        "a.ts",
        "let a: number = 1;\nlet b: string = x;\n",
        &Options::default(),
    )
    .unwrap();
    let crlf = transpile(
        "a.ts",
        "let a: number = 1;\r\nlet b: string = x;\r\n",
        &Options::default(),
    )
    .unwrap();
    assert_eq!(lf.code, crlf.code);
    let mappings = |m: &str| {
        m.split("\"mappings\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(mappings(&lf.source_map), mappings(&crlf.source_map));
}
