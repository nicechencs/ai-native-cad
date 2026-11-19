use serde::{Deserialize, Serialize};

/// 1-based line and column span matching `schemas/diagnostics.schema.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Span {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    #[serde(skip_serializing, skip_deserializing)]
    pub start_byte: usize,
    #[serde(skip_serializing, skip_deserializing)]
    pub end_byte: usize,
}

impl Span {
    pub fn new(
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
        start_byte: usize,
        end_byte: usize,
    ) -> Self {
        Self {
            start_line,
            start_col,
            end_line,
            end_col,
            start_byte,
            end_byte,
        }
    }

    pub fn merge(self, other: Span) -> Span {
        if self.start_byte == 0 && self.end_byte == 0 && self.start_line == 0 {
            return other;
        }
        if other.start_byte == 0 && other.end_byte == 0 && other.start_line == 0 {
            return self;
        }
        let (start_line, start_col, start_byte) = if self.start_byte <= other.start_byte {
            (self.start_line, self.start_col, self.start_byte)
        } else {
            (other.start_line, other.start_col, other.start_byte)
        };
        let (end_line, end_col, end_byte) = if self.end_byte >= other.end_byte {
            (self.end_line, self.end_col, self.end_byte)
        } else {
            (other.end_line, other.end_col, other.end_byte)
        };
        Span {
            start_line,
            start_col,
            end_line,
            end_col,
            start_byte,
            end_byte,
        }
    }

    pub fn contains_line_col(&self, line_1based: usize, col_1based: usize) -> bool {
        if line_1based < self.start_line || line_1based > self.end_line {
            return false;
        }
        if line_1based == self.start_line && col_1based < self.start_col {
            return false;
        }
        if line_1based == self.end_line && col_1based > self.end_col {
            return false;
        }
        true
    }
}

/// Maps byte offsets in UTF-8 source text to 1-based line and character column positions.
/// Uses an O(1) ASCII fast-path and a prebuilt cumulative character-offset table for UTF-8
/// so column lookup never rescans from the start of long lines (R21).
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Vec<usize>,
    source_len: usize,
    /// `None` when source is pure ASCII (`col = byte - line_start + 1` in O(1)).
    /// When non-ASCII, maps each byte offset `0..=source_len` to cumulative Unicode scalar count.
    utf8_char_prefix: Option<Vec<usize>>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        let mut is_ascii = true;
        for (idx, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(idx + 1);
            }
            if b >= 0x80 {
                is_ascii = false;
            }
        }

        let utf8_char_prefix = if is_ascii {
            None
        } else {
            let mut prefix = vec![0usize; source.len() + 1];
            let mut count = 0usize;
            for (byte_idx, ch) in source.char_indices() {
                let len = ch.len_utf8();
                for k in 0..len {
                    prefix[byte_idx + k] = count;
                }
                count += 1;
            }
            prefix[source.len()] = count;
            Some(prefix)
        };

        Self {
            line_starts,
            source_len: source.len(),
            utf8_char_prefix,
        }
    }

    pub fn line_col(&self, _source: &str, byte_offset: usize) -> (usize, usize) {
        let clamped = byte_offset.min(self.source_len);
        let line_idx = match self.line_starts.binary_search(&clamped) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[line_idx];
        let col = match &self.utf8_char_prefix {
            None => (clamped - line_start) + 1,
            Some(prefix) => (prefix[clamped] - prefix[line_start]) + 1,
        };
        (line_idx + 1, col)
    }

    pub fn span(&self, source: &str, start_byte: usize, end_byte: usize) -> Span {
        let (sl, sc) = self.line_col(source, start_byte);
        let (el, ec) = self.line_col(source, end_byte);
        Span::new(sl, sc, el, ec, start_byte, end_byte)
    }
}
