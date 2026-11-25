//! Dialect selection (crate docs, *Dialects*): which shell a file is
//! written for. Deterministic: the same path and bytes always give the same
//! dialect; nothing is read from the store or the environment.
//!
//! The first rule that answers wins:
//!
//! 1. the `.bats` extension (bats preprocesses the file, whatever its `#!`);
//! 2. a ShellCheck directive in the leading comment block
//!    (`# shellcheck shell=bash`), which ShellCheck itself puts above the
//!    `#!` line;
//! 3. the `#!` line (`Language::shebang_interpreter`: `env`, `env -S`,
//!    `busybox sh`, version suffixes);
//! 4. zsh's own first lines, `#compdef …` and `#autoload`;
//! 5. the file name: `.zsh`, `.zsh-theme`, `.zshrc` and the other zsh
//!    start-up files; `.bash`, `.bashrc`, …; `.ksh`, `.kshrc`, `.mkshrc`;
//!    `.profile` (read by every Bourne shell: `sh`);
//! 6. an editor modeline in the first or last five lines (`vim: ft=zsh`,
//!    `-*- sh-shell: bash -*-`);
//! 7. a directory of the path named after a shell (`zsh/`, `bash/`,
//!    `ksh/`, also `zsh-functions`, `bash_completion.d`, …);
//! 8. content signals, for the `.sh` files left: builtins and syntax only
//!    one shell has, weighted (see [`signals`]);
//! 9. `sh`.

use rueca_model::Language;

/// A shell dialect the pack tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dialect {
    /// POSIX `sh` (also `dash`, `ash`, busybox `sh`, `posh`, `yash`).
    Sh,
    Bash,
    Zsh,
    /// ksh88 / ksh93 and the pdksh family (`mksh`, `oksh`).
    Ksh,
    /// A bats test file: bash with `@test` blocks.
    Bats,
}

/// The rule that chose the dialect (for the coverage table and the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Evidence {
    Extension,
    Directive,
    Shebang,
    ZshHeader,
    FileName,
    Modeline,
    Directory,
    Content,
    Default,
}

/// The grammar a file is parsed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grammar {
    /// tree-sitter-bash: sh, bash, ksh and bats files.
    Bash,
    /// tree-sitter-zsh: zsh files.
    Zsh,
}

impl Grammar {
    /// The grammar tried when this one leaves parse errors.
    pub fn other(self) -> Grammar {
        match self {
            Grammar::Bash => Grammar::Zsh,
            Grammar::Zsh => Grammar::Bash,
        }
    }
}

impl Dialect {
    /// Every dialect, in declaration order.
    pub const ALL: [Dialect; 5] = [Dialect::Sh, Dialect::Bash, Dialect::Zsh, Dialect::Ksh, Dialect::Bats];

    /// Lower-case name, as written in signatures and directives.
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Sh => "sh",
            Dialect::Bash => "bash",
            Dialect::Zsh => "zsh",
            Dialect::Ksh => "ksh",
            Dialect::Bats => "bats",
        }
    }

    /// The dialect of a shell name as written in a `#!` line, a directive
    /// or a modeline (`dash`, `ksh93`, `mksh`, …).
    pub fn parse(name: &str) -> Option<Dialect> {
        let name = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        Some(match name.to_ascii_lowercase().as_str() {
            "sh" | "dash" | "ash" | "posix" | "posh" | "yash" | "busybox" => Dialect::Sh,
            "bash" => Dialect::Bash,
            "zsh" => Dialect::Zsh,
            "ksh" | "mksh" | "pdksh" | "oksh" | "loksh" => Dialect::Ksh,
            "bats" => Dialect::Bats,
            _ => return None,
        })
    }

    /// The grammar this dialect is read with first.
    pub fn grammar(self) -> Grammar {
        match self {
            Dialect::Zsh => Grammar::Zsh,
            Dialect::Sh | Dialect::Bash | Dialect::Ksh | Dialect::Bats => Grammar::Bash,
        }
    }
}

/// How many leading bytes the header rules read.
const HEAD_BYTES: usize = 2048;
/// How many bytes the content signals read.
const CONTENT_BYTES: usize = 256 * 1024;
/// Lines at each end of the file searched for a modeline.
const MODELINE_LINES: usize = 5;

/// The dialect of the file at `path` with content `source`, and the rule
/// that chose it (module docs).
pub fn detect(path: &str, source: &[u8]) -> (Dialect, Evidence) {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".bats") {
        return (Dialect::Bats, Evidence::Extension);
    }
    let head = String::from_utf8_lossy(&source[..source.len().min(HEAD_BYTES)]);
    if let Some(d) = directive(&head) {
        return (d, Evidence::Directive);
    }
    if let Some(d) = Language::shebang_interpreter(head.as_bytes()).and_then(Dialect::parse) {
        return (d, Evidence::Shebang);
    }
    let first = head.lines().next().unwrap_or("");
    if first.starts_with("#compdef") || first.starts_with("#autoload") {
        return (Dialect::Zsh, Evidence::ZshHeader);
    }
    if let Some(d) = by_file_name(name) {
        return (d, Evidence::FileName);
    }
    if let Some(d) = modeline(source) {
        return (d, Evidence::Modeline);
    }
    if let Some(d) = by_directory(path) {
        return (d, Evidence::Directory);
    }
    let text = String::from_utf8_lossy(&source[..source.len().min(CONTENT_BYTES)]);
    match signals(&text).verdict() {
        Some(d) => (d, Evidence::Content),
        None => (Dialect::Sh, Evidence::Default),
    }
}

/// `# shellcheck shell=NAME` in the comment lines that open the file.
fn directive(head: &str) -> Option<Dialect> {
    for line in head.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let comment = line.strip_prefix('#')?;
        let Some(rest) = comment.trim_start().strip_prefix("shellcheck") else { continue };
        if let Some(d) = rest.split_ascii_whitespace().find_map(|w| w.strip_prefix("shell=")).and_then(Dialect::parse) {
            return Some(d);
        }
    }
    None
}

/// The dialect a file name states: its extension or a start-up file name
/// (with or without the leading dot: `zshrc`, `dot.bashrc`, `bashrc.sh`
/// are not matched, only the names shells read).
fn by_file_name(name: &str) -> Option<Dialect> {
    let bare = name.trim_start_matches('.');
    let ext = bare.rsplit_once('.').map_or("", |(_, e)| e);
    Some(match ext {
        "zsh" | "zsh-theme" => Dialect::Zsh,
        "bash" => Dialect::Bash,
        "ksh" => Dialect::Ksh,
        "sh" => return None,
        _ => match bare {
            "zshrc" | "zshenv" | "zprofile" | "zlogin" | "zlogout" => Dialect::Zsh,
            "bashrc" | "bash_profile" | "bash_login" | "bash_logout" | "bash_aliases" => Dialect::Bash,
            "kshrc" | "mkshrc" => Dialect::Ksh,
            "profile" => Dialect::Sh,
            _ => return None,
        },
    })
}

/// `vim: set ft=zsh:` / `vi: filetype=bash` / `-*- mode: sh; sh-shell: zsh -*-`
/// in the first or last lines of the file.
fn modeline(source: &[u8]) -> Option<Dialect> {
    let head = String::from_utf8_lossy(&source[..source.len().min(HEAD_BYTES)]);
    let tail = String::from_utf8_lossy(&source[source.len().saturating_sub(HEAD_BYTES)..]);
    let tail_lines: Vec<&str> = tail.lines().collect();
    let last = tail_lines.iter().rev().take(MODELINE_LINES).rev().copied();
    head.lines().take(MODELINE_LINES).chain(last).find_map(modeline_of)
}

fn modeline_of(line: &str) -> Option<Dialect> {
    let line = line.trim();
    if !line.starts_with('#') {
        return None;
    }
    let value_after = |key: &str| {
        let at = line.find(key)? + key.len();
        let value: String =
            line[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
        Dialect::parse(&value)
    };
    if line.contains("vim:") || line.contains("vi:") || line.contains("ex:") {
        return ["filetype=", "ft=", "syntax=", "syn="].iter().find_map(|k| value_after(k));
    }
    if line.contains("-*-") {
        return value_after("sh-shell: ").or_else(|| value_after("sh-shell:"));
    }
    None
}

/// A directory of the path named after one shell: `zsh`, `.zsh`,
/// `zsh-functions`, `bash_completion.d`, `ksh93`, … (the name split at
/// `-`, `_` and `.` must contain the shell's name as a word, and only one
/// shell's).
fn by_directory(path: &str) -> Option<Dialect> {
    let mut found = None;
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    for dir in dirs.into_iter().rev() {
        let lower = dir.to_ascii_lowercase();
        let mut here = None;
        for word in lower.split(['-', '_', '.']) {
            let d = match word {
                "zsh" | "ohmyzsh" | "zshrc" | "zfunctions" => Dialect::Zsh,
                "bash" | "bashrc" => Dialect::Bash,
                "ksh" | "ksh93" | "mksh" => Dialect::Ksh,
                _ => continue,
            };
            if here.is_some_and(|h| h != d) {
                return found;
            }
            here = Some(d);
        }
        if here.is_some() {
            found = here;
            break;
        }
    }
    found
}

/// Weighted evidence in a file's code (comment lines skipped).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Signals {
    /// zsh-only builtins and syntax (`setopt`, `autoload`, `zstyle`, `${(f)x}`, …).
    pub zsh: u32,
    /// bash-only builtins and variables (`shopt`, `BASH_SOURCE`, `mapfile`, …).
    pub bash: u32,
    /// ksh-only forms (`set -A`, `${.sh.version}`, `print -u2`).
    pub ksh: u32,
    /// Syntax POSIX sh lacks and bash, zsh and ksh share (`[[ … ]]`,
    /// `function f`, arrays, `<<<`).
    pub extended: u32,
    /// The file tests which shell runs it (`$ZSH_VERSION`, `$BASH_VERSION`):
    /// a script written for several shells.
    pub portable: bool,
}

/// Commands only zsh has.
const ZSH_COMMANDS: &[&str] = &[
    "setopt",
    "unsetopt",
    "autoload",
    "zstyle",
    "zmodload",
    "compdef",
    "emulate",
    "zle",
    "bindkey",
    "zparseopts",
    "add-zsh-hook",
    "compinit",
    "zcompile",
];
/// Commands only bash has.
const BASH_COMMANDS: &[&str] = &["shopt", "mapfile", "readarray", "complete", "compgen", "compopt"];
/// Text only zsh code contains.
const ZSH_TEXT: &[&str] = &["${(", "$+commands", "${+", "$+functions"];
/// Text only bash code contains.
const BASH_TEXT: &[&str] = &["BASH_SOURCE", "BASH_REMATCH", "COMPREPLY", "PROMPT_COMMAND", "declare -", "local -n "];
/// Text only ksh code contains.
const KSH_TEXT: &[&str] = &["set -A ", "${.sh.", "print -u", "KSH_VERSION"];
/// Syntax beyond POSIX sh that the three larger shells share.
const EXTENDED_TEXT: &[&str] = &["[[ ", "<<<", "=(", "<(", "&>", "$'"];

/// Counts the content signals of `text` (each distinct signal once).
pub fn signals(text: &str) -> Signals {
    let mut seen: Vec<&'static str> = Vec::new();
    let mut s = Signals::default();
    let mut once = |key: &'static str| {
        let new = !seen.contains(&key);
        if new {
            seen.push(key);
        }
        new
    };
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains("ZSH_VERSION") || line.contains("BASH_VERSION") {
            s.portable = true;
        }
        let first = line.split(|c: char| c.is_ascii_whitespace() || c == ';').next().unwrap_or("");
        if let Some(k) = ZSH_COMMANDS.iter().find(|k| **k == first).filter(|k| once(k)) {
            s.zsh += if *k == "autoload" || *k == "bindkey" { 2 } else { 3 };
        }
        if let Some(k) = BASH_COMMANDS.iter().find(|k| **k == first).filter(|k| once(k)) {
            s.bash += if *k == "complete" { 2 } else { 3 };
        }
        if first == "function" && once("function") {
            s.extended += 1;
        }
        for k in ZSH_TEXT.iter().filter(|k| line.contains(**k)) {
            s.zsh += u32::from(once(k)) * 3;
        }
        for k in BASH_TEXT.iter().filter(|k| line.contains(**k)) {
            s.bash += u32::from(once(k)) * 3;
        }
        for k in KSH_TEXT.iter().filter(|k| line.contains(**k)) {
            s.ksh += u32::from(once(k)) * 3;
        }
        for k in EXTENDED_TEXT.iter().filter(|k| line.contains(**k)) {
            s.extended += u32::from(once(k));
        }
    }
    s
}

impl Signals {
    /// The dialect the signals name, `None` when the code is plain POSIX
    /// sh as far as they show. One shell's own signals must outweigh the
    /// others'; a file that tests `$ZSH_VERSION` / `$BASH_VERSION` is
    /// written for several shells and is never zsh or ksh by content alone
    /// (nvm.sh, completion scripts shared by bash and zsh): it is `bash`
    /// when it uses anything beyond POSIX, else `sh`.
    pub fn verdict(self) -> Option<Dialect> {
        let beyond_posix = self.bash + self.zsh + self.ksh + self.extended > 0;
        if self.portable {
            return beyond_posix.then_some(Dialect::Bash);
        }
        if self.zsh >= 3 && self.zsh > self.bash && self.zsh > self.ksh {
            return Some(Dialect::Zsh);
        }
        if self.ksh >= 3 && self.ksh > self.bash && self.ksh > self.zsh {
            return Some(Dialect::Ksh);
        }
        beyond_posix.then_some(Dialect::Bash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(path: &str, src: &str) -> (Dialect, Evidence) {
        detect(path, src.as_bytes())
    }

    #[test]
    fn extension_directive_shebang() {
        assert_eq!(d("test/x.bats", "#!/usr/bin/env zsh\n"), (Dialect::Bats, Evidence::Extension));
        assert_eq!(d("x.sh", "#!/bin/sh\n# shellcheck shell=bash\n"), (Dialect::Bash, Evidence::Directive));
        assert_eq!(d("x.sh", "# shellcheck shell=ksh disable=SC2034\nx=1\n"), (Dialect::Ksh, Evidence::Directive));
        // only in the comment block that opens the file
        assert_eq!(d("x.sh", "x=1\n# shellcheck shell=zsh\n"), (Dialect::Sh, Evidence::Default));
        for (head, want) in [
            ("#!/bin/sh", Dialect::Sh),
            ("#!/bin/dash", Dialect::Sh),
            ("#!/bin/busybox sh", Dialect::Sh),
            ("#!/usr/bin/env bash", Dialect::Bash),
            ("#!/usr/bin/env -S bash -eu", Dialect::Bash),
            ("#! /bin/bash -e", Dialect::Bash),
            ("#!/usr/local/bin/zsh -f", Dialect::Zsh),
            ("#!/bin/ksh93", Dialect::Ksh),
            ("#!/bin/mksh", Dialect::Ksh),
            ("#!/usr/bin/env bats", Dialect::Bats),
        ] {
            assert_eq!(d("bin/tool", &format!("{head}\necho\n")), (want, Evidence::Shebang), "{head}");
            // the `#!` line wins over the extension
            assert_eq!(d("bin/tool.zsh", &format!("{head}\necho\n")).0, want, "{head}");
        }
        // an interpreter that is not a shell says nothing
        assert_eq!(d("x.zsh", "#!/usr/bin/env python3\n"), (Dialect::Zsh, Evidence::FileName));
    }

    #[test]
    fn names_headers_modelines_directories() {
        assert_eq!(d("completions/_git", "#compdef git\n_git() { :; }\n"), (Dialect::Zsh, Evidence::ZshHeader));
        assert_eq!(d("functions/x.sh", "#autoload\n"), (Dialect::Zsh, Evidence::ZshHeader));
        for (path, want) in [
            ("plugins/git/git.plugin.zsh", Dialect::Zsh),
            ("themes/robbyrussell.zsh-theme", Dialect::Zsh),
            ("home/.zshrc", Dialect::Zsh),
            ("home/.zlogout", Dialect::Zsh),
            ("lib/x.bash", Dialect::Bash),
            ("home/.bashrc", Dialect::Bash),
            ("home/.bash_profile", Dialect::Bash),
            ("x.ksh", Dialect::Ksh),
            ("home/.kshrc", Dialect::Ksh),
            ("home/.mkshrc", Dialect::Ksh),
            ("home/.profile", Dialect::Sh),
        ] {
            assert_eq!(d(path, "x=1\n"), (want, Evidence::FileName), "{path}");
        }
        assert_eq!(d("x.sh", "x=1\n# vim: set ft=zsh sw=2:\n"), (Dialect::Zsh, Evidence::Modeline));
        assert_eq!(d("x.sh", "# -*- mode: sh; sh-shell: bash -*-\nx=1\n"), (Dialect::Bash, Evidence::Modeline));
        assert_eq!(d("x.sh", "# vi: filetype=ksh\n"), (Dialect::Ksh, Evidence::Modeline));
        assert_eq!(d("x.sh", "echo 'vim: ft=zsh'\n"), (Dialect::Sh, Evidence::Default));
        assert_eq!(d("shell/zsh/init.sh", "x=1\n"), (Dialect::Zsh, Evidence::Directory));
        assert_eq!(d("etc/bash_completion.d/tool.sh", "x=1\n"), (Dialect::Bash, Evidence::Directory));
        assert_eq!(d("contrib/ksh93/x.sh", "x=1\n"), (Dialect::Ksh, Evidence::Directory));
        // a directory naming two shells says nothing; the nearest directory wins
        assert_eq!(d("bash-zsh/x.sh", "x=1\n"), (Dialect::Sh, Evidence::Default));
        assert_eq!(d("zsh/bash/x.sh", "x=1\n"), (Dialect::Bash, Evidence::Directory));
        assert_eq!(d("bashful/x.sh", "x=1\n"), (Dialect::Sh, Evidence::Default));
    }

    #[test]
    fn content_signals() {
        assert_eq!(d("x.sh", "f() { echo hi; }\nf\n"), (Dialect::Sh, Evidence::Default));
        assert_eq!(d("x.sh", "if [[ -n $x ]]; then y=(a b); fi\n"), (Dialect::Bash, Evidence::Content));
        assert_eq!(d("x.sh", "shopt -s nullglob\n"), (Dialect::Bash, Evidence::Content));
        assert_eq!(d("x.sh", "dir=${BASH_SOURCE[0]%/*}\n"), (Dialect::Bash, Evidence::Content));
        assert_eq!(d("x.sh", "setopt extendedglob\nprint ${(j:,:)xs}\n"), (Dialect::Zsh, Evidence::Content));
        assert_eq!(d("x.sh", "autoload -U compinit\ncompinit\n"), (Dialect::Zsh, Evidence::Content));
        // one weak zsh signal is not enough
        assert_eq!(d("x.sh", "bindkey -e\n"), (Dialect::Bash, Evidence::Content));
        assert_eq!(d("x.sh", "set -A xs a b\nprint -u2 x\n"), (Dialect::Ksh, Evidence::Content));
        // comments are not code
        assert_eq!(d("x.sh", "# setopt extendedglob\necho\n"), (Dialect::Sh, Evidence::Default));
        // a script written for several shells is bash, whatever it calls under its checks
        let portable = "if [ -n \"$ZSH_VERSION\" ]; then\n  setopt local_options\n  autoload -U x\nfi\n";
        assert_eq!(d("nvm.sh", portable), (Dialect::Bash, Evidence::Content));
        assert_eq!(d("x.sh", "[ -n \"$BASH_VERSION\" ] && echo bash\n"), (Dialect::Sh, Evidence::Default));
        // bash and zsh signals: the heavier side
        assert_eq!(d("x.sh", "shopt -s extglob\nmapfile -t a\nsetopt x\n").0, Dialect::Bash);
    }

    #[test]
    fn names_round_trip_and_grammars() {
        for dialect in Dialect::ALL {
            assert_eq!(Dialect::parse(dialect.as_str()), Some(dialect));
        }
        assert_eq!(Dialect::parse("ksh93"), Some(Dialect::Ksh));
        assert_eq!(Dialect::parse("fish"), None);
        assert_eq!(Dialect::Zsh.grammar(), Grammar::Zsh);
        assert_eq!(Dialect::Bats.grammar(), Grammar::Bash);
        assert_eq!(Grammar::Bash.other(), Grammar::Zsh);
        assert_eq!(detect("x.sh", b""), (Dialect::Sh, Evidence::Default));
        assert_eq!(detect("x.sh", &[0xff, 0xfe, b'\n']), (Dialect::Sh, Evidence::Default));
    }
}
