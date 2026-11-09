//! Source text that came from a JavaScript string (eval, `Function`).
//!
//! JavaScript source is a sequence of UTF-16 code units, so it can hold lone
//! surrogates, which no Rust `str` can. The lexer reads UTF-8 and works in
//! byte offsets, so a UTF-16 source is converted once: each lone surrogate
//! becomes U+FFFD in the text (three bytes, the length WTF-8 would give it,
//! so offsets match a WTF-8 encoding) and is listed with its byte offset in
//! a side table, sorted by offset. A U+FFFD written in the source itself is
//! not in the table and stays U+FFFD.
//!
//! The lexer consults the table only where a lone surrogate can matter (the
//! values of string, template and regular expression literals); everywhere
//! else U+FFFD and a lone surrogate behave alike (neither may start or
//! continue an identifier, neither is whitespace or a line terminator), and
//! source text from a Rust `str` has an empty table, so ordinary source
//! pays nothing.

/// A lone surrogate of a UTF-16 source, standing as U+FFFD at byte
/// `offset` of the converted text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LoneSurrogate {
    pub offset: u32,
    pub unit: u16,
}

/// Convert UTF-16 source to the lexer's text and lone-surrogate table.
pub fn from_utf16(units: &[u16]) -> (String, Vec<LoneSurrogate>) {
    let mut text = String::with_capacity(units.len());
    let mut lone = Vec::new();
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok(c) => text.push(c),
            Err(e) => {
                lone.push(LoneSurrogate {
                    offset: text.len() as u32,
                    unit: e.unpaired_surrogate(),
                });
                text.push('\u{FFFD}');
            }
        }
    }
    (text, lone)
}

/// The lone surrogate standing at byte `offset`, if any.
pub fn lone_at(lone: &[LoneSurrogate], offset: usize) -> Option<u16> {
    if lone.is_empty() {
        return None;
    }
    lone.binary_search_by_key(&(offset as u32), |l| l.offset)
        .ok()
        .map(|i| lone[i].unit)
}

/// Append the UTF-16 code units of `text`, which starts at byte `base` of
/// the source, putting back the lone surrogates the table lists there.
pub fn push_units(out: &mut Vec<u16>, text: &str, base: usize, lone: &[LoneSurrogate]) {
    let from = lone.partition_point(|l| (l.offset as usize) < base);
    let to = lone.partition_point(|l| (l.offset as usize) < base + text.len());
    if from == to {
        out.extend(text.encode_utf16());
        return;
    }
    let mut next = from;
    let mut buf = [0u16; 2];
    for (i, c) in text.char_indices() {
        if next < to && lone[next].offset as usize == base + i {
            out.push(lone[next].unit);
            next += 1;
        } else {
            out.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
}

/// The UTF-16 code units of `text[start..end]` with its lone surrogates.
pub fn units_of(text: &str, start: usize, end: usize, lone: &[LoneSurrogate]) -> Vec<u16> {
    let mut out = Vec::with_capacity(end - start);
    push_units(&mut out, &text[start..end], start, lone);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_surrogates_round_trip_and_real_replacement_characters_stay() {
        let units = [0x61, 0xD800, 0xFFFD, 0xD83D, 0xDE00, 0xDC00];
        let (text, lone) = from_utf16(&units);
        assert_eq!(text, "a\u{FFFD}\u{FFFD}\u{1F600}\u{FFFD}");
        assert_eq!(
            lone,
            [
                LoneSurrogate {
                    offset: 1,
                    unit: 0xD800
                },
                LoneSurrogate {
                    offset: 11,
                    unit: 0xDC00
                }
            ]
        );
        assert_eq!(units_of(&text, 0, text.len(), &lone), units);
        assert_eq!(units_of(&text, 4, text.len(), &lone), units[2..]);
        assert_eq!(lone_at(&lone, 4), None);
        assert_eq!(lone_at(&lone, 11), Some(0xDC00));
    }
}
