//! Source positions.
//!
//! Every AST node carries a [`Span`]: byte offsets into the UTF-8 source
//! text. A [`LineTable`] converts offsets to line and column on demand,
//! which is what the debugger, source maps and error messages need. Columns
//! are counted in UTF-16 code units, as the Chrome DevTools Protocol and
//! source maps expect.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }
    pub const fn empty(at: u32) -> Span {
        Span { start: at, end: at }
    }
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start, end: other.end }
    }
    pub fn len(self) -> u32 {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A line and column, both zero-based; column in UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

/// Byte offsets of line starts, for offset → line/column lookup.
#[derive(Clone, Debug)]
pub struct LineTable {
    line_starts: Vec<u32>,
    source_len: u32,
}

impl LineTable {
    pub fn new(source: &str) -> LineTable {
        let bytes = source.as_bytes();
        let mut line_starts = vec![0u32];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => line_starts.push(i as u32 + 1),
                b'\r' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                    line_starts.push(i as u32 + 1);
                }
                // U+2028 and U+2029 are E2 80 A8 / E2 80 A9.
                0xE2 if bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xA8) | Some(0xA9)) => {
                    i += 2;
                    line_starts.push(i as u32 + 1);
                }
                _ => {}
            }
            i += 1;
        }
        LineTable { line_starts, source_len: bytes.len() as u32 }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Line and column of a byte offset. `source` must be the text the
    /// table was built from (needed to count UTF-16 units in the column).
    pub fn line_col(&self, source: &str, offset: u32) -> LineCol {
        let offset = offset.min(self.source_len);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line] as usize;
        let column = source[start..offset as usize].encode_utf16().count() as u32;
        LineCol { line: line as u32, column }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_utf16_columns() {
        let src = "ab\ncd\r\n€x\u{2028}y";
        let t = LineTable::new(src);
        assert_eq!(t.line_count(), 4);
        assert_eq!(t.line_col(src, 0), LineCol { line: 0, column: 0 });
        assert_eq!(t.line_col(src, 3), LineCol { line: 1, column: 0 });
        assert_eq!(t.line_col(src, 7), LineCol { line: 2, column: 0 });
        // "€" is 3 bytes, 1 UTF-16 unit.
        assert_eq!(t.line_col(src, 10), LineCol { line: 2, column: 1 });
        assert_eq!(t.line_col(src, src.len() as u32), LineCol { line: 3, column: 1 });
    }
}
