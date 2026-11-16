//! The legacy decorator helpers. Oxc's transform calls
//! `babelHelpers.decorate`, `babelHelpers.decorateParam` and
//! `babelHelpers.decorateMetadata` and leaves defining them to the host
//! (its inline mode is not implemented, and its runtime mode imports a
//! package). These are the crate's own implementations, with the semantics
//! of `tslib`'s `__decorate`, `__param` and `__metadata`, on one line that
//! goes after the hashbang and the directive prologue, so that
//! `"use strict"` still comes first. The map gets an empty line there.

/// One line, no line terminator inside.
pub(crate) const PRELUDE: &str = "var babelHelpers = { \
decorate: function (decorators, target, key, desc) { var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d; \
if (typeof Reflect === \"object\" && typeof Reflect.decorate === \"function\") r = Reflect.decorate(decorators, target, key, desc); \
else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r; \
return c > 3 && r && Object.defineProperty(target, key, r), r; }, \
decorateParam: function (paramIndex, decorator) { return function (target, key) { decorator(target, key, paramIndex); }; }, \
decorateMetadata: function (k, v) { if (typeof Reflect === \"object\" && typeof Reflect.metadata === \"function\") return Reflect.metadata(k, v); } };";

/// Inserts [`PRELUDE`] as a new line before line `at` (0-based) of `code`,
/// and an empty generated line at the same place in the map's `mappings`.
pub(crate) fn insert_prelude(code: &mut String, map: &mut String, at: usize) {
    let offset = nth_line_start(code, at);
    let mut line = String::with_capacity(PRELUDE.len() + 1);
    line.push_str(PRELUDE);
    line.push('\n');
    if offset == code.len() && !code.is_empty() && !code.ends_with('\n') {
        line.insert(0, '\n');
    }
    code.insert_str(offset, &line);
    shift_mappings(map, at);
}

/// The byte offset of the start of line `n` (0-based), or the end of the
/// code. Oxc prints only `\n` between lines; a U+2028 inside a string is
/// escaped, so it never ends a printed line.
fn nth_line_start(code: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    code.match_indices('\n')
        .nth(n - 1)
        .map_or(code.len(), |(i, _)| i + 1)
}

/// Adds an empty generated line before line `at` in the `mappings` of a
/// Source Map v3 JSON document. Lines are separated by `;`; every field but
/// the generated column is relative across lines, so an empty line changes
/// nothing else.
fn shift_mappings(map: &mut String, at: usize) {
    const KEY: &str = "\"mappings\":\"";
    let Some(key) = map.find(KEY) else { return };
    let start = key + KEY.len();
    let len = map[start..].find('"').unwrap_or(map.len() - start);
    let mappings = &map[start..start + len];
    let insert = if at == 0 {
        0
    } else {
        match mappings.match_indices(';').nth(at - 1) {
            Some((i, _)) => i + 1,
            // Fewer mapped lines than `at`: the new line is past them all.
            None => {
                let missing = at - mappings.matches(';').count();
                map.insert_str(start + len, &";".repeat(missing));
                return;
            }
        }
    };
    map.insert(start + insert, ';');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_is_one_line() {
        assert!(!PRELUDE.contains('\n') && !PRELUDE.contains('\r'));
    }

    #[test]
    fn inserts_after_the_prologue() {
        let mut code = "\"use strict\";\nfoo();\n".to_string();
        let mut map = "{\"version\":3,\"mappings\":\"AAAA;AACA\"}".to_string();
        insert_prelude(&mut code, &mut map, 1);
        assert_eq!(code, format!("\"use strict\";\n{PRELUDE}\nfoo();\n"));
        assert_eq!(map, "{\"version\":3,\"mappings\":\"AAAA;;AACA\"}");

        let mut code = "foo();\n".to_string();
        let mut map = "{\"mappings\":\"AAAA\"}".to_string();
        insert_prelude(&mut code, &mut map, 0);
        assert!(code.starts_with("var babelHelpers"));
        assert_eq!(map, "{\"mappings\":\";AAAA\"}");

        let mut code = "\"use strict\"".to_string();
        let mut map = "{\"mappings\":\"AAAA\"}".to_string();
        insert_prelude(&mut code, &mut map, 1);
        assert_eq!(code, format!("\"use strict\"\n{PRELUDE}\n"));
        assert_eq!(map, "{\"mappings\":\"AAAA;\"}");
    }
}
