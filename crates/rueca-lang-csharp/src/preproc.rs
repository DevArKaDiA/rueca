//! Conditional compilation: one configuration of a file whose `#if` blocks
//! break the syntax.
//!
//! tree-sitter-c-sharp parses `#if` / `#elif` / `#else` / `#endif` where a
//! declaration or a statement may stand, and walks every branch. A
//! directive in the middle of a declaration (`void F() #if X => G() #endif
//! ;`, a different signature per target framework) leaves the tree full of
//! `ERROR` nodes and the code inside them unseen. For such a file the pack
//! parses again a copy in which only the **first branch** of every
//! conditional is kept: the directive lines and the other branches are
//! replaced with spaces (newlines kept), so every byte offset, line and
//! column stays where it was. The copy is used when it has fewer parse
//! errors than the original (see `extract.rs`).

/// The directive keyword of a line (`if`, `else`, …), when it is one.
fn directive(line: &[u8]) -> Option<&[u8]> {
    let start = line.iter().position(|b| !b.is_ascii_whitespace())?;
    let rest = line[start..].strip_prefix(b"#")?;
    let rest = &rest[rest.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(rest.len())..];
    let end = rest.iter().position(|b| !b.is_ascii_alphabetic()).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Replaces every byte of `line` but newlines with a space.
fn blank(line: &mut [u8]) {
    for b in line.iter_mut() {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

/// The source with only the first branch of each `#if` kept (see the
/// module docs); `None` when it has no conditional.
pub(crate) fn first_branch(source: &[u8]) -> Option<Vec<u8>> {
    if !source.windows(3).any(|w| w == b"#if") {
        return None;
    }
    let mut out = source.to_vec();
    // Per open conditional: whether the lines of its current branch are kept.
    let mut stack: Vec<bool> = Vec::new();
    let mut start = 0;
    while start < out.len() {
        let end = out[start..].iter().position(|&b| b == b'\n').map_or(out.len(), |i| start + i + 1);
        let active = stack.last().is_none_or(|&keep| keep);
        match directive(&out[start..end]) {
            Some(b"if") => {
                stack.push(active);
                blank(&mut out[start..end]);
            }
            Some(b"elif" | b"else") => {
                if let Some(keep) = stack.last_mut() {
                    *keep = false;
                }
                blank(&mut out[start..end]);
            }
            Some(b"endif") => {
                stack.pop();
                blank(&mut out[start..end]);
            }
            _ if !active => blank(&mut out[start..end]),
            _ => {}
        }
        start = end;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_first_branch_in_place() {
        let src = b"class A {\n  void F()\n#if X\n    => G()\n#else\n    => H()\n#endif\n    ;\n}\n";
        let out = first_branch(src).unwrap();
        assert_eq!(out.len(), src.len());
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("=> G()") && !text.contains("H()") && !text.contains('#'));
        assert_eq!(text.lines().count(), 9);
        let nested = b"#if A\nx\n#if B\ny\n#else\nz\n#endif\n#elif C\nw\n#endif\nv\n";
        let text = String::from_utf8(first_branch(nested).unwrap()).unwrap();
        let kept: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(kept, ["x", "y", "v"]);
        assert!(first_branch(b"class A { }").is_none());
    }
}
