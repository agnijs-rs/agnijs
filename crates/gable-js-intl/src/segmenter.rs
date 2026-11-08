//! `Intl.Segmenter` on ICU4X's grapheme, word and sentence segmenters.

use icu_segmenter::options::{SentenceBreakInvariantOptions, WordBreakInvariantOptions};
use icu_segmenter::{GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Grapheme,
    Word,
    Sentence,
}

impl Granularity {
    pub fn name(self) -> &'static str {
        match self {
            Granularity::Grapheme => "grapheme",
            Granularity::Word => "word",
            Granularity::Sentence => "sentence",
        }
    }
}

/// One segment: UTF-16 code-unit range plus, for words, whether it is
/// word-like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub is_word_like: Option<bool>,
}

/// Segments `input` fully. The segments are contiguous and cover the input.
pub fn segment(input: &[u16], granularity: Granularity) -> Vec<Segment> {
    let mut out = Vec::new();
    if input.is_empty() {
        return out;
    }
    let mut prev = 0;
    let mut push = |out: &mut Vec<Segment>, b: usize, word_like: Option<bool>| {
        if b == 0 || b <= prev {
            return;
        }
        out.push(Segment { start: prev, end: b, is_word_like: word_like });
        prev = b;
    };
    match granularity {
        Granularity::Grapheme => {
            let seg = GraphemeClusterSegmenter::new();
            for b in seg.segment_utf16(input) {
                push(&mut out, b, None);
            }
        }
        Granularity::Word => {
            let seg = WordSegmenter::new_auto(WordBreakInvariantOptions::default());
            for (b, ty) in seg.segment_utf16(input).iter_with_word_type() {
                push(&mut out, b, Some(ty.is_word_like()));
            }
        }
        Granularity::Sentence => {
            let seg = SentenceSegmenter::new(SentenceBreakInvariantOptions::default());
            for b in seg.segment_utf16(input) {
                push(&mut out, b, None);
            }
        }
    }
    let covered = out.last().map(|s| s.end).unwrap_or(0);
    if covered < input.len() {
        let word_like = if granularity == Granularity::Word { Some(false) } else { None };
        out.push(Segment { start: covered, end: input.len(), is_word_like: word_like });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn segments() {
        let g = segment(&u("a👨‍👩‍👧b"), Granularity::Grapheme);
        assert_eq!(g.len(), 3);
        let w = segment(&u("Hello, world!"), Granularity::Word);
        assert_eq!(w[0], Segment { start: 0, end: 5, is_word_like: Some(true) });
        assert_eq!(w[1], Segment { start: 5, end: 6, is_word_like: Some(false) });
        assert_eq!(w[2], Segment { start: 6, end: 7, is_word_like: Some(false) });
        assert_eq!(w[3], Segment { start: 7, end: 12, is_word_like: Some(true) });
        let s = segment(&u("One. Two! Three"), Granularity::Sentence);
        assert_eq!(s.len(), 3);
        assert_eq!(s[2], Segment { start: 10, end: 15, is_word_like: None });
        let ja = segment(&u("吾輩は猫である。"), Granularity::Word);
        assert!(ja.len() > 2);
        assert!(segment(&[], Granularity::Grapheme).is_empty());
    }
}
