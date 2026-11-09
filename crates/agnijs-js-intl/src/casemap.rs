//! Locale-sensitive case mapping for `String.prototype.toLocaleUpperCase`
//! and `toLocaleLowerCase`.

use icu_casemap::CaseMapper;
use icu_locale_core::LanguageIdentifier;

/// Case-maps a UTF-16 string. Lone surrogates survive unchanged, as the
/// spec requires; well-formed runs are mapped with ICU4X.
pub fn map_utf16(input: &[u16], tag: &str, upper: bool) -> Vec<u16> {
    let lang: LanguageIdentifier = tag
        .split('-')
        .next()
        .unwrap_or("und")
        .parse()
        .unwrap_or(LanguageIdentifier::UNKNOWN);
    let cm = CaseMapper::new();
    let mut out: Vec<u16> = Vec::with_capacity(input.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u16>| {
        if run.is_empty() {
            return;
        }
        let mapped = if upper { cm.uppercase_to_string(run, &lang) } else { cm.lowercase_to_string(run, &lang) };
        out.extend(mapped.encode_utf16());
        run.clear();
    };
    for r in std::char::decode_utf16(input.iter().copied()) {
        match r {
            Ok(c) => run.push(c),
            Err(e) => {
                flush(&mut run, &mut out);
                out.push(e.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn s(v: &[u16]) -> String {
        String::from_utf16_lossy(v)
    }

    #[test]
    fn turkish_and_surrogates() {
        assert_eq!(s(&map_utf16(&u("i"), "tr", true)), "İ");
        assert_eq!(s(&map_utf16(&u("I"), "tr", false)), "ı");
        assert_eq!(s(&map_utf16(&u("I"), "en", false)), "i");
        assert_eq!(s(&map_utf16(&u("ß"), "de", true)), "SS");
        assert_eq!(s(&map_utf16(&u("ΑΣ"), "el", false)), "ας");
        let lone = vec![0x61, 0xD800, 0x62];
        assert_eq!(map_utf16(&lone, "en", true), vec![0x41, 0xD800, 0x42]);
    }
}
