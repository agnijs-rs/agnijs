//! TypeScript files, with the `typescript` feature (ADR 0029): `.ts`,
//! `.mts`, `.cts` and `.tsx` are transpiled through agnijs-js-typescript
//! before the engine sees them, with the source map registered so stack
//! traces, code frames and the debugger show TypeScript positions. Without
//! the feature no file is TypeScript and the command is unchanged.

/// Transpiled code and its Source Map v3.
pub struct Transpiled {
    pub code: String,
    pub source_map: String,
}

/// The extensions the resolver also tries, after the JavaScript ones.
pub const EXTENSIONS: &[&str] = if cfg!(feature = "typescript") {
    &[".ts", ".tsx", ".mts", ".cts"]
} else {
    &[]
};

/// Whether `path` is a TypeScript file this build runs. Declaration files
/// (`.d.ts`) hold no code and are not.
pub fn is_typescript(path: &str) -> bool {
    cfg!(feature = "typescript")
        && EXTENSIONS.iter().any(|e| path.ends_with(e))
        && ![".d.ts", ".d.mts", ".d.cts"]
            .iter()
            .any(|e| path.ends_with(e))
}

/// Transpile the file `path` as a script or a module. The error is the
/// text to print: each diagnostic with its code frame.
#[cfg(feature = "typescript")]
pub fn transpile(path: &str, source: &str, script: bool) -> Result<Transpiled, String> {
    use agnijs_js_typescript::{Error, Options, SourceKind};
    let mut options = Options::for_path(path).unwrap_or_default();
    options.kind = if script {
        SourceKind::Script
    } else {
        SourceKind::Module
    };
    match agnijs_js_typescript::transpile(path, source, &options) {
        Ok(out) => Ok(Transpiled {
            code: out.code,
            source_map: out.source_map,
        }),
        Err(Error::Syntax(diagnostics)) => {
            let lines = ecma_lines(source);
            let mut text = String::new();
            for d in &diagnostics {
                if let Some(line) = lines.get(d.line as usize - 1) {
                    text.push_str(&format!(
                        "{}:{}\n{line}\n{}^\n",
                        d.file,
                        d.line,
                        pad(line, d.column)
                    ));
                }
                text.push_str(&format!("{d}\n"));
            }
            Err(text)
        }
        Err(e) => Err(format!("{path}: {e}\n")),
    }
}

#[cfg(not(feature = "typescript"))]
pub fn transpile(path: &str, _source: &str, _script: bool) -> Result<Transpiled, String> {
    Err(format!(
        "{path}: this agnijs was built without TypeScript support\n"
    ))
}

/// The lines of `source` split at ECMAScript line terminators, as the
/// diagnostics count them.
#[cfg_attr(not(feature = "typescript"), allow(dead_code))]
fn ecma_lines(source: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = source.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            lines.push(&source[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    lines.push(&source[start..]);
    lines
}

/// Spaces (tabs kept) up to the 1-based UTF-16 `column` of `line`.
#[cfg_attr(not(feature = "typescript"), allow(dead_code))]
fn pad(line: &str, column: u32) -> String {
    let mut width = 0;
    line.chars()
        .take_while(|c| {
            width += c.len_utf16() as u32;
            width < column
        })
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ecma_lines, pad};

    #[test]
    fn lines_split_at_every_terminator() {
        assert_eq!(
            ecma_lines("a\r\nb\rc\u{2028}d\ne"),
            ["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn the_caret_counts_utf16_units() {
        assert_eq!(pad("\t\u{1F600}x", 4), "\t ");
        assert_eq!(pad("abc", 1), "");
    }
}
