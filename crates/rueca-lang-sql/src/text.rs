//! Positions: sqlparser reports 1-based lines and 1-based character
//! columns; Rueca stores byte offsets with 0-based lines and byte columns.

use rueca_model::Span;

/// Line index of one text.
pub struct Lines<'t> {
    text: &'t str,
    starts: Vec<usize>,
}

impl<'t> Lines<'t> {
    pub fn new(text: &'t str) -> Lines<'t> {
        let mut starts = vec![0];
        starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
        Lines { text, starts }
    }

    /// Byte offset of a 1-based (line, character column) location; past
    /// the end of its line it is the line's end, past the last line the
    /// text's end. Line 0 (sqlparser's "no location") is offset 0.
    pub fn byte_at(&self, line: u64, column: u64) -> usize {
        if line == 0 {
            return 0;
        }
        let Some(&start) = self.starts.get(line as usize - 1) else { return self.text.len() };
        let end = self.starts.get(line as usize).map_or(self.text.len(), |n| n - 1);
        let line_text = &self.text[start..end.max(start)];
        let col = column.saturating_sub(1) as usize;
        start + line_text.char_indices().nth(col).map_or(line_text.len(), |(i, _)| i)
    }

    /// 0-based line of a byte offset.
    pub fn line_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|s| *s <= byte) - 1
    }

    /// The [`Span`] of a byte range.
    pub fn span(&self, start: usize, end: usize) -> Span {
        let (sl, el) = (self.line_of(start), self.line_of(end));
        Span {
            start_byte: start as u32,
            end_byte: end as u32,
            start_line: sl as u32,
            start_col: (start - self.starts[sl]) as u32,
            end_line: el as u32,
            end_col: (end - self.starts[el]) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_to_bytes() {
        let l = Lines::new("ab\ncé d\n");
        assert_eq!(l.byte_at(1, 1), 0);
        assert_eq!(l.byte_at(2, 3), 6);
        assert_eq!(l.byte_at(2, 4), 7);
        assert_eq!(l.byte_at(2, 99), 8);
        assert_eq!(l.byte_at(9, 1), 9);
        let s = l.span(3, 8);
        assert_eq!((s.start_line, s.start_col, s.end_line, s.end_col), (1, 0, 1, 5));
    }
}
