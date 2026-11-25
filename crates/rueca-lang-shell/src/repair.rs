//! Same-length repairs before parsing: byte offsets, lines and columns stay
//! exact, so every span points at the original text. Each rule works around
//! a construct tree-sitter-bash 0.25 rejects (or misreads) although the
//! shell accepts it; the replacement is a construct the grammar reads the
//! same way for extraction. Errors that remain are counted as parse errors.
//!
//! | Written | Read as | Why |
//! |---|---|---|
//! | `\. "$NVM_DIR/nvm.sh"`, `\command ls` at the start of a line (an alias bypass) | ` . …`, ` command …` | the grammar joins the backslash to the previous line's last word |
//! | `${s:$i:1}` (substring offset from a variable) | `${s: i:1}` | the grammar rejects `:$` there (`${s:i:1}` is the same offset) |
//! | `&`, `<`, `>` inside `${…}` (`${x//&/\&amp;}`, `${x//</\&lt;}`) | `_` | the grammar rejects them in an expansion's pattern |
//! | zsh parameter flags `${(j:,:)xs}`, `${(%):-%x}` | `${______xs}`, `${___:-%x}` | not bash syntax; the variable and the operator stay readable |

use std::borrow::Cow;

/// Applies the rules of the module docs; borrows when nothing changes.
pub(crate) fn repair(source: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut set = |i: usize, b: u8| out.get_or_insert_with(|| source.to_vec())[i] = b;
    let mut line_start = true;
    let mut depth = 0usize;
    let mut i = 0;
    while i < source.len() {
        let b = source[i];
        let next = source.get(i + 1).copied().unwrap_or(b' ');
        match b {
            b'\\' if line_start && is_command_start(next) => set(i, b' '),
            b'\\' => {
                if depth > 0 && matches!(next, b'&' | b'<' | b'>') {
                    set(i + 1, b'_');
                }
                i += 1;
            }
            b'$' if next == b'{' => {
                depth += 1;
                if source.get(i + 2) == Some(&b'(') {
                    if let Some(close) = flags_end(source, i + 2) {
                        (i + 2..=close).for_each(|k| set(k, b'_'));
                        i = close;
                    }
                } else {
                    i += 1;
                }
            }
            b'}' if depth > 0 => depth -= 1,
            b':' if depth > 0
                && next == b'$'
                && source.get(i + 2).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') =>
            {
                set(i + 1, b' ');
            }
            b'&' | b'<' | b'>' if depth > 0 => set(i, b'_'),
            _ => {}
        }
        line_start = b == b'\n' || (line_start && (b == b' ' || b == b'\t'));
        i += 1;
    }
    out.map_or(Cow::Borrowed(source), Cow::Owned)
}

fn is_command_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'.' || b == b'_'
}

/// The `)` closing zsh parameter flags opened at `open` (`(j:,:)`), when the
/// group is short and on one line.
fn flags_end(source: &[u8], open: usize) -> Option<usize> {
    let rest = &source[open + 1..source.len().min(open + 33)];
    let close = rest.iter().position(|&c| c == b')')?;
    (!rest[..close].iter().any(|&c| matches!(c, b'\n' | b'(' | b'$' | b'{' | b'}'))).then_some(open + 1 + close)
}

#[cfg(test)]
mod tests {
    use super::repair;

    fn r(s: &str) -> String {
        String::from_utf8(repair(s.as_bytes()).into_owned()).unwrap()
    }

    #[test]
    fn rules() {
        assert_eq!(r("x\n\\. \"$D/a.sh\"\n  \\command ls\n"), "x\n . \"$D/a.sh\"\n   command ls\n");
        assert_eq!(r("echo \\$HOME a\\.b"), "echo \\$HOME a\\.b");
        assert_eq!(r("c=${name:$i:1}"), "c=${name: i:1}");
        assert_eq!(r("c=${x:-$y} ${x:$((i+1))}"), "c=${x:-$y} ${x:$((i+1))}");
        assert_eq!(r("o=${1//&/\\&amp;}; a && b"), "o=${1//_/\\_amp;}; a && b");
        assert_eq!(r("o=${o//</\\&lt;} > f"), "o=${o//_/\\_lt;} > f");
        assert_eq!(r("j=${(j:,:)parts} z=${${(%):-%x}:a:h}"), "j=${______parts} z=${${___:-%x}:a:h}");
        assert_eq!(r("f() { :; }"), "f() { :; }");
        for s in ["", "${", "${(", "x=${(a", "\\"] {
            assert_eq!(repair(s.as_bytes()).len(), s.len());
        }
        assert!(matches!(repair(b"plain"), std::borrow::Cow::Borrowed(_)));
    }
}
