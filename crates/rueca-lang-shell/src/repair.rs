//! Same-length repairs before parsing: byte offsets, lines and columns stay
//! exact, so every span points at the original text. Each rule works around
//! a construct the grammar rejects (or misreads) although the shell accepts
//! it; the replacement is a construct the grammar reads the same way for
//! extraction. Errors that remain are counted as parse errors.
//!
//! Rules for tree-sitter-bash 0.25 (sh, bash, ksh and bats files):
//!
//! | Written | Read as | Why |
//! |---|---|---|
//! | `\. "$NVM_DIR/nvm.sh"`, `\command ls` at the start of a line (an alias bypass) | ` . …`, ` command …` | the grammar joins the backslash to the previous line's last word |
//! | `${s:$i:1}` (substring offset from a variable) | `${s: i:1}` | the grammar rejects `:$` there (`${s:i:1}` is the same offset) |
//! | `&`, `<`, `>`, `;` inside `${…}` (`${x//&/\&amp;}`, `${x//</\&lt;}`) | `_` | the grammar rejects them in an expansion's pattern |
//! | zsh parameter flags `${(j:,:)xs}`, `${(%):-%x}` | `${______xs}`, `${___:-%x}` | not bash syntax; the variable and the operator stay readable |
//! | a here-string after an output redirection, `cat > f <<<"x"` | `cat > f   <"x"` | the grammar rejects `<<<` there; an input redirection holds the same words |
//!
//! Rules for tree-sitter-zsh 0.64 (zsh files):
//!
//! | Written | Read as | Why |
//! |---|---|---|
//! | `$$` right before more text (`$f.$$.tmp`, `pid=$$;`) | `$!` | the grammar reads `$$` only before a blank; another special parameter, no reference either way |
//! | arguments of an anonymous function, a line `} "$1" …` | `};"$1" …` | the grammar has no arguments there; a quoted or expanded command word gives no reference |
//! | a commented-out assignment, a line `#NAME=value …` or `#NAME+=value` | `#NAME value …`, `#NAME =value` | the grammar reads an assignment to `#NAME` (and the words after it as a command) |
//! | an unquoted prompt escape `%{` (`COLOR=%{$fg[red]%}`) | `%_` | the grammar ends the value at `%` and reads `{…}` as a command |
//!
//! Rules for both:
//!
//! | Written | Read as | Why |
//! |---|---|---|
//! | `for arg do` (no `in`, on one line) | `for arg;do` | the grammars want `;` or a newline before `do` |
//! | a hexadecimal prefix before an expansion in arithmetic, `$((0x${h} * 2))` | `$((  ${h} * 2))` | `0x` and the expansion are not one number for the grammars |
//! | `} 0</dev/null` (an explicit descriptor 0 after a block) | `}  </dev/null` | the grammars reject a numbered redirection after `}`; `<` is descriptor 0 |

use std::borrow::Cow;

use crate::dialect::Grammar;

/// Applies the rules of the module docs for `grammar`; borrows when
/// nothing changes.
pub(crate) fn repair(source: &[u8], grammar: Grammar) -> Cow<'_, [u8]> {
    let mut r = Repair {
        src: source,
        out: None,
        bash: grammar == Grammar::Bash,
        depth: 0,
        redirected: false,
        arithmetic: false,
    };
    let mut line_start = true;
    let mut i = 0;
    while i < source.len() {
        let b = source[i];
        i = r.byte(i, line_start);
        line_start = b == b'\n' || (line_start && matches!(b, b' ' | b'\t'));
    }
    r.out.map_or(Cow::Borrowed(source), Cow::Owned)
}

/// The scan: the text, the copy once something changes, the grammar's rule
/// set, the nesting of `${…}` at the current byte, whether the current
/// command already has an output redirection and whether the current line
/// opened an arithmetic `((`.
struct Repair<'s> {
    src: &'s [u8],
    out: Option<Vec<u8>>,
    bash: bool,
    depth: usize,
    redirected: bool,
    arithmetic: bool,
}

impl Repair<'_> {
    fn set(&mut self, i: usize, b: u8) {
        self.out.get_or_insert_with(|| self.src.to_vec())[i] = b;
    }

    fn at(&self, i: usize) -> u8 {
        self.src.get(i).copied().unwrap_or(b' ')
    }

    fn before(&self, i: usize) -> u8 {
        if i == 0 { b' ' } else { self.src[i - 1] }
    }

    fn starts(&self, i: usize, text: &[u8]) -> bool {
        self.src[i..].starts_with(text)
    }

    /// Handles the byte at `i`; the index of the next byte to look at.
    fn byte(&mut self, i: usize, line_start: bool) -> usize {
        let next = self.at(i + 1);
        match self.src[i] {
            b'\\' => self.backslash(i, line_start, next),
            b'$' if next == b'{' => self.expansion_start(i),
            b'$' if next == b'$' => self.pid(i),
            b'}' if self.depth > 0 => {
                self.depth -= 1;
                i + 1
            }
            b'#' if !self.bash && line_start => self.commented_assignment(i),
            b'%' if !self.bash && next == b'{' && !is_name_byte(self.before(i)) => {
                self.set(i + 1, b'_');
                i + 2
            }
            b'}' if line_start => self.block_end(i),
            b'}' => self.descriptor_zero(i),
            b':' if self.bash && self.depth > 0 && next == b'$' && is_name_start(self.at(i + 2)) => {
                self.set(i + 1, b' ');
                i + 1
            }
            b'&' | b'<' | b'>' | b';' if self.bash && self.depth > 0 => {
                self.set(i, b'_');
                i + 1
            }
            b'<' if self.bash && self.redirected && self.starts(i, b"<<<") => {
                self.set(i, b' ');
                self.set(i + 1, b' ');
                i + 3
            }
            b'>' => {
                self.redirected = true;
                i + 1
            }
            b'\n' => {
                self.redirected = false;
                self.arithmetic = false;
                i + 1
            }
            b';' | b'|' => {
                self.redirected = false;
                i + 1
            }
            b'(' if next == b'(' => {
                self.arithmetic = true;
                i + 2
            }
            b'0' if self.arithmetic && self.starts(i, b"0x$") && !is_name_byte(self.before(i)) => {
                self.set(i, b' ');
                self.set(i + 1, b' ');
                i + 2
            }
            b'f' if self.depth == 0 && !is_name_byte(self.before(i)) && self.starts(i, b"for ") => self.for_do(i),
            _ => i + 1,
        }
    }

    /// `\cmd` at the start of a line loses its backslash (bash grammar);
    /// `\&` (`\<`, `\>`, `\;`) inside an expansion becomes `\_`; any other
    /// escape is skipped.
    fn backslash(&mut self, i: usize, line_start: bool, next: u8) -> usize {
        if !self.bash {
            return i + 2;
        }
        if line_start && is_command_start(next) {
            self.set(i, b' ');
            return i + 1;
        }
        if self.depth > 0 && matches!(next, b'&' | b'<' | b'>' | b';') {
            self.set(i + 1, b'_');
        }
        i + 2
    }

    /// `${`: one level deeper (bash grammar: zsh flags right after it are
    /// masked). The zsh grammar reads expansions as written.
    fn expansion_start(&mut self, i: usize) -> usize {
        if !self.bash {
            return i + 2;
        }
        self.depth += 1;
        if self.at(i + 2) != b'(' {
            return i + 2;
        }
        match flags_end(self.src, i + 2) {
            Some(close) => {
                (i + 2..=close).for_each(|k| self.set(k, b'_'));
                close + 1
            }
            None => i + 2,
        }
    }

    /// zsh grammar: `$$` followed by more text becomes `$!`.
    fn pid(&mut self, i: usize) -> usize {
        let after = self.at(i + 2);
        if !self.bash && !after.is_ascii_whitespace() && !matches!(after, b'"' | b'\'' | b'-' | b':' | b'(' | b'{') {
            self.set(i + 1, b'!');
        }
        i + 2
    }

    /// zsh grammar: a line `} "$1" …` or `} $x …` closes an anonymous
    /// function called with arguments; the arguments become a command of
    /// their own. Then the rule for `} 0<`.
    fn block_end(&mut self, i: usize) -> usize {
        if !self.bash && self.at(i + 1) == b' ' && matches!(self.at(i + 2), b'"' | b'$') {
            self.set(i + 1, b';');
            return i + 2;
        }
        self.descriptor_zero(i)
    }

    /// zsh grammar: `#NAME=…` / `#NAME+=…` at the start of a line stays a
    /// comment once the `=` (or the `+`) is a blank.
    fn commented_assignment(&mut self, i: usize) -> usize {
        let mut k = i + 1;
        if !is_name_start(self.at(k)) {
            return i + 1;
        }
        while is_name_byte(self.at(k)) && k < self.src.len() {
            k += 1;
        }
        if self.at(k) == b'=' || self.starts(k, b"+=") {
            self.set(k, b' ');
        }
        k
    }

    /// `} 0<file` → `}  <file`.
    fn descriptor_zero(&mut self, i: usize) -> usize {
        if self.starts(i, b"} 0<") && !matches!(self.at(i + 4), b'<' | b'(') {
            self.set(i + 2, b' ');
        }
        i + 1
    }

    /// `for NAME do` → `for NAME;do`.
    fn for_do(&mut self, i: usize) -> usize {
        let name_start = i + 4;
        let mut k = name_start;
        while is_name_byte(self.at(k)) && k < self.src.len() {
            k += 1;
        }
        let after_do = self.at(k + 3);
        if k > name_start && self.starts(k, b" do") && (after_do.is_ascii_whitespace() || after_do == b';') {
            self.set(k, b';');
            return k + 3;
        }
        i + 1
    }
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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
    use super::Grammar;

    fn repair(s: &[u8]) -> std::borrow::Cow<'_, [u8]> {
        super::repair(s, Grammar::Bash)
    }

    fn r(s: &str) -> String {
        String::from_utf8(repair(s.as_bytes()).into_owned()).unwrap()
    }

    fn z(s: &str) -> String {
        String::from_utf8(super::repair(s.as_bytes(), Grammar::Zsh).into_owned()).unwrap()
    }

    #[test]
    fn bash_rules() {
        assert_eq!(r("x\n\\. \"$D/a.sh\"\n  \\command ls\n"), "x\n . \"$D/a.sh\"\n   command ls\n");
        assert_eq!(r("echo \\$HOME a\\.b"), "echo \\$HOME a\\.b");
        assert_eq!(r("c=${name:$i:1}"), "c=${name: i:1}");
        assert_eq!(r("c=${x:-$y} ${x:$((i+1))}"), "c=${x:-$y} ${x:$((i+1))}");
        assert_eq!(r("o=${1//&/\\&amp;}; a && b"), "o=${1//_/\\_amp_}; a && b");
        assert_eq!(r("o=${o//</\\&lt;} > f"), "o=${o//_/\\_lt_} > f");
        assert_eq!(r("j=${(j:,:)parts} z=${${(%):-%x}:a:h}"), "j=${______parts} z=${${___:-%x}:a:h}");
        assert_eq!(r("f() { :; }"), "f() { :; }");
        for s in
            ["", "${", "${(", "x=${(a", "\\", "for ", "for x", "0x$", "} 0<", "$$", "<<<", "}", "#", "#a", "#a+", "%"]
        {
            assert_eq!(repair(s.as_bytes()).len(), s.len());
            assert_eq!(super::repair(s.as_bytes(), Grammar::Zsh).len(), s.len());
        }
        assert!(matches!(repair(b"plain"), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn here_string_after_a_redirection() {
        assert_eq!(r("cat > f <<<\"x\"\n"), "cat > f   <\"x\"\n");
        assert_eq!(r("cat >\"$f\" <<< $'a\\r'"), "cat >\"$f\"   < $'a\\r'");
        // without a redirection before it the grammar reads a here-string
        assert_eq!(r("cat <<<\"x\" > f\nread a <<<\"$b\""), "cat <<<\"x\" > f\nread a <<<\"$b\"");
        assert_eq!(r("a > f; b <<<x\na > f | b <<<x"), "a > f; b <<<x\na > f | b <<<x");
        // the zsh grammar reads both
        assert_eq!(z("cat > f <<<\"x\""), "cat > f <<<\"x\"");
    }

    #[test]
    fn rules_for_both_grammars() {
        for f in [r, z] {
            assert_eq!(f("for arg do echo; done"), "for arg;do echo; done");
            assert_eq!(f("  for a do\n"), "  for a;do\n");
            assert_eq!(f("for a in x; do :; done; for a\ndo :; done"), "for a in x; do :; done; for a\ndo :; done");
            assert_eq!(f("for a dog"), "for a dog");
            assert_eq!(f("wait_for x do"), "wait_for x do");
            assert_eq!(f("n=$((0x${h} * 2)) m=$(( 0x$a ))"), "n=$((  ${h} * 2)) m=$((   $a ))");
            assert_eq!(f("echo 0x$a; n=$((0x10)); (( n = a0x$b ))"), "echo 0x$a; n=$((0x10)); (( n = a0x$b ))");
            assert_eq!(f("{ a; } 0</dev/null"), "{ a; }  </dev/null");
            assert_eq!(f("}\n} 0<&3\n"), "}\n}  <&3\n");
            assert_eq!(f("{ a; } 0<<EOF"), "{ a; } 0<<EOF");
        }
    }

    #[test]
    fn zsh_rules() {
        assert_eq!(z("t=$f.tmp.$$.zwc; pid=$$; echo $$ \"$$\" $$"), "t=$f.tmp.$!.zwc; pid=$!; echo $$ \"$$\" $$");
        assert_eq!(r("t=$f.tmp.$$.zwc"), "t=$f.tmp.$$.zwc");
        assert_eq!(z("() {\n  a\n} \"$1\" &|\n} $x\n"), "() {\n  a\n};\"$1\" &|\n};$x\n");
        assert_eq!(z("f() {\n  a\n} > f\n  } \"$1\""), "f() {\n  a\n} > f\n  };\"$1\"");
        assert_eq!(r("} \"$1\""), "} \"$1\"");
        assert_eq!(
            z("#FOO=\"x\" bar\n  #B+=1\n#foo\n# A=1\nA=1 #B=2"),
            "#FOO \"x\" bar\n  #B =1\n#foo\n# A=1\nA=1 #B=2"
        );
        assert_eq!(r("#FOO=1"), "#FOO=1");
        assert_eq!(z("C=%{$fg[red]%}\"::%{$x%}\" ${x%{*}"), "C=%_$fg[red]%}\"::%_$x%}\" ${x%{*}");
        assert_eq!(r("C=%{$fg[red]%}"), "C=%{$fg[red]%}");
        // what the bash rules rewrite, the zsh grammar reads as written
        let s = "\\. x\nc=${s:$i:1} o=${1//&/x} j=${(j:,:)parts}";
        assert_eq!(z(s), s);
    }
}
