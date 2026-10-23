//! Pure text helpers: line splitting and snippet windows.

/// Splits file content into lines. Invalid UTF-8 is replaced lossily, `\r\n`
/// and `\n` both end a line, and a trailing newline does not start a new line.
pub fn split_lines(content: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(content);
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(&text);
    body.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

/// A resolved window of lines (1-based, inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: u32,
    pub end: u32,
    pub truncated: bool,
}

/// Computes the window for the requested 1-based inclusive range `[start, end]`
/// in a file of `line_count` lines, widened by `context` lines on both sides
/// (clamped to the file) and cut to at most `max_lines` lines (setting
/// `truncated`). An empty file yields `1..=0`.
pub fn window(start: u32, end: u32, context: u32, max_lines: u32, line_count: u32) -> Window {
    if line_count == 0 {
        return Window { start: 1, end: 0, truncated: false };
    }
    let start = start.clamp(1, line_count);
    let end = end.clamp(start, line_count);
    let ws = start.saturating_sub(context).max(1);
    let we = end.saturating_add(context).min(line_count);
    let max_lines = max_lines.max(1);
    if we - ws + 1 > max_lines {
        Window { start: ws, end: ws + max_lines - 1, truncated: true }
    } else {
        Window { start: ws, end: we, truncated: false }
    }
}

/// Joins lines `[w.start, w.end]` (1-based) with `\n`.
pub fn join_window(lines: &[String], w: Window) -> String {
    if w.end < w.start {
        return String::new();
    }
    lines[(w.start - 1) as usize..w.end as usize].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_crlf_and_lossy() {
        assert_eq!(split_lines(b"a\r\nb\nc"), vec!["a", "b", "c"]);
        assert_eq!(split_lines(b"a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines(b"a\n\n"), vec!["a", ""]);
        assert!(split_lines(b"").is_empty());
        assert_eq!(split_lines(b"x\xff\n"), vec!["x\u{fffd}"]);
    }

    #[test]
    fn windows() {
        assert_eq!(window(3, 4, 0, 300, 10), Window { start: 3, end: 4, truncated: false });
        assert_eq!(window(3, 4, 5, 300, 10), Window { start: 1, end: 9, truncated: false });
        assert_eq!(window(1, 10, 0, 4, 10), Window { start: 1, end: 4, truncated: true });
        assert_eq!(window(9, 99, 0, 300, 10), Window { start: 9, end: 10, truncated: false });
        assert_eq!(window(5, 2, 0, 300, 10), Window { start: 5, end: 5, truncated: false });
        let lines: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(join_window(&lines, window(2, 3, 0, 300, 3)), "b\nc");
    }
}
