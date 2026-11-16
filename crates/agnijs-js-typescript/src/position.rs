//! Byte offsets to 1-based lines and UTF-16 columns, with the ECMAScript
//! line terminators (LF, CR LF, CR, U+2028, U+2029), as the engine counts
//! positions.

pub(crate) struct LineIndex<'a> {
    source: &'a str,
    /// The byte offset where each line starts.
    starts: Vec<u32>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(source: &'a str) -> LineIndex<'a> {
        let mut starts = vec![0u32];
        let bytes = source.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => starts.push((i + 1) as u32),
                b'\r' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                    starts.push((i + 1) as u32);
                }
                // U+2028 and U+2029 are E2 80 A8 and E2 80 A9.
                0xE2 if bytes.get(i + 1) == Some(&0x80)
                    && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) =>
                {
                    i += 2;
                    starts.push((i + 1) as u32);
                }
                _ => {}
            }
            i += 1;
        }
        LineIndex { source, starts }
    }

    /// The 1-based line and UTF-16 column of a byte offset. An offset inside
    /// a character or past the end is clamped.
    pub(crate) fn position(&self, offset: u32) -> (u32, u32) {
        let mut offset = (offset as usize).min(self.source.len());
        while !self.source.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s as usize <= offset) - 1;
        let start = self.starts[line] as usize;
        let column = self.source[start..offset].encode_utf16().count();
        (line as u32 + 1, column as u32 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn lines_and_utf16_columns() {
        let src = "a\nb\r\nc\rd\u{2028}e\u{2029}f\n\u{1F600}x";
        let ix = LineIndex::new(src);
        assert_eq!(ix.position(0), (1, 1));
        assert_eq!(ix.position(2), (2, 1));
        assert_eq!(ix.position(5), (3, 1));
        assert_eq!(ix.position(7), (4, 1));
        let e = src.find('e').unwrap() as u32;
        assert_eq!(ix.position(e), (5, 1));
        let f = src.find('f').unwrap() as u32;
        assert_eq!(ix.position(f), (6, 1));
        let x = src.find('x').unwrap() as u32;
        // The emoji is two UTF-16 units.
        assert_eq!(ix.position(x), (7, 3));
        assert_eq!(ix.position(x + 100), (7, 4));
        assert_eq!(ix.position(x - 1), (7, 1));
    }
}
