//! Paths written in scripts: what `source`, `.` and script invocations
//! name, evaluated as far as the file shows, and how the resolver maps them
//! to files of the snapshot.
//!
//! A path is evaluated from its syntax into a [`PathPattern`]: an anchor and
//! a literal rest.
//!
//! | Written | Anchor | Rest |
//! |---|---|---|
//! | `lib.sh`, `./lib/x.sh`, `'../a b.sh'` | literal | as written |
//! | `"$(dirname "$0")/lib.sh"`, `${BASH_SOURCE%/*}/lib.sh`, `"$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/x.sh"`, zsh `${0:A:h}/x.zsh` | the script's directory | `/lib.sh` |
//! | `"$DIR/x.sh"` where the file assigns `DIR` one of the forms above (`DIR=$(dirname "$0")`, `ROOT="$(cd "$(dirname "$0")/.." && pwd)"`) | that variable's anchor | its rest + `/x.sh` |
//! | `"$NVM_DIR/nvm.sh"` (a variable the file does not assign) | unknown | `/nvm.sh` |
//! | `"$dir/$name.sh"`, `$(find …)` | — (not evaluated: no import, no edge) | |
//!
//! Patterns travel in `Import::module` (and in the name of a `$script`
//! call reference) as text: the rest itself for a literal path, `$dir` +
//! rest for the script's directory, `*` + rest for an unknown prefix
//! (neither can occur in a path a shell would have expanded).
//!
//! [`resolve`] maps a pattern written in `from` to a file of the snapshot:
//! the script's directory is `from`'s directory; a literal relative path is
//! tried against `from`'s directory, then the repository root (the shell
//! resolves it against the working directory, which the file does not
//! show); an unknown prefix matches the only file whose path ends with the
//! rest, which must be a whole path component (`/nvm.sh`), and nothing when
//! several files do. Absolute paths and paths leaving the repository
//! resolve to nothing.

use std::collections::HashMap;

use rueca_extract::ProjectFiles;
use rueca_extract::tree_sitter::Node;

use crate::locals::text;

/// Text of a script-directory anchor in an encoded pattern.
pub(crate) const DIR_ANCHOR: &str = "$dir";
/// Text of an unknown prefix in an encoded pattern.
pub(crate) const UNKNOWN_ANCHOR: &str = "*";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Relative to the working directory (or absolute), as written.
    Literal,
    /// The directory of the script that writes the path.
    ScriptDir,
    /// A prefix the file does not determine.
    Unknown,
}

/// A path as far as the file determines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathPattern {
    pub(crate) anchor: Anchor,
    pub(crate) rest: String,
}

impl PathPattern {
    fn literal(s: &str) -> Self {
        PathPattern { anchor: Anchor::Literal, rest: s.to_string() }
    }

    fn script_dir() -> Self {
        PathPattern { anchor: Anchor::ScriptDir, rest: String::new() }
    }

    fn unknown() -> Self {
        PathPattern { anchor: Anchor::Unknown, rest: String::new() }
    }

    /// The text carried in `Import::module` (see the module docs).
    pub(crate) fn encode(&self) -> String {
        match self.anchor {
            Anchor::Literal => self.rest.clone(),
            Anchor::ScriptDir => format!("{DIR_ANCHOR}{}", self.rest),
            Anchor::Unknown => format!("{UNKNOWN_ANCHOR}{}", self.rest),
        }
    }

    /// Whether the pattern can name a file at all.
    pub(crate) fn is_useful(&self) -> bool {
        match self.anchor {
            Anchor::Literal => !self.rest.is_empty(),
            Anchor::ScriptDir => self.rest.len() > 1,
            Anchor::Unknown => unknown_tail(&self.rest).is_some(),
        }
    }

    /// The pattern of the parent directory (`dirname` of this path).
    fn parent(self) -> Self {
        PathPattern { rest: format!("{}/..", self.rest), ..self }
    }

    fn push(&mut self, part: Part) {
        match part {
            Part::Lit(s) => self.rest.push_str(&s),
            // A dynamic part after literal text: only what follows it is known.
            Part::Pattern(p) if self.rest.is_empty() && self.anchor == Anchor::Literal => *self = p,
            Part::Pattern(_) | Part::ScriptFile | Part::Dynamic => *self = PathPattern::unknown(),
        }
    }
}

/// The rest of an unknown-prefix pattern when it is a usable suffix: a
/// whole path component with a file name (`/lib/x.sh`, not `/` or `.sh`).
fn unknown_tail(rest: &str) -> Option<&str> {
    let tail = rest.strip_prefix('/')?;
    let name = tail.rsplit('/').next()?;
    (!name.is_empty() && name != "." && name != ".." && !tail.contains('*')).then_some(tail)
}

/// Decodes `Import::module` text into an anchor and the rest.
pub(crate) fn decode(module: &str) -> (Anchor, &str) {
    if let Some(rest) = module.strip_prefix(DIR_ANCHOR) {
        (Anchor::ScriptDir, rest)
    } else if let Some(rest) = module.strip_prefix(UNKNOWN_ANCHOR) {
        (Anchor::Unknown, rest)
    } else {
        (Anchor::Literal, module)
    }
}

/// One piece of a written path.
enum Part {
    Lit(String),
    /// A value the file determines (a script-directory form, or a variable
    /// it assigns one).
    Pattern(PathPattern),
    /// The script's own path (`$0`, `${BASH_SOURCE[0]}`): its `dirname` is
    /// the script's directory.
    ScriptFile,
    Dynamic,
}

/// Evaluates path syntax with the file's directory variables.
pub(crate) struct PathEval<'a> {
    pub(crate) source: &'a [u8],
    pub(crate) vars: &'a HashMap<String, PathPattern>,
}

/// Variable names whose value is the script's own path.
const SCRIPT_FILE_VARS: &[&str] = &["0", "BASH_SOURCE", "ZSH_ARGZERO"];

impl PathEval<'_> {
    /// The pattern of a path-valued node, `None` when the file does not
    /// determine a usable one.
    pub(crate) fn eval(&self, node: Node<'_>) -> Option<PathPattern> {
        self.value(node).filter(PathPattern::is_useful)
    }

    /// The pattern of a variable's value (a directory: no rest needed);
    /// `None` when nothing of it is known.
    pub(crate) fn value(&self, node: Node<'_>) -> Option<PathPattern> {
        let mut parts = Vec::new();
        self.parts(node, &mut parts, 0);
        let mut out = PathPattern::literal("");
        for part in parts {
            out.push(part);
        }
        let known = out.anchor != Anchor::Unknown || unknown_tail(&out.rest).is_some();
        (known && !(out.anchor == Anchor::Literal && out.rest.is_empty())).then_some(out)
    }

    fn parts(&self, node: Node<'_>, out: &mut Vec<Part>, depth: usize) {
        if depth > 16 {
            out.push(Part::Dynamic);
            return;
        }
        match node.kind() {
            "word" | "number" => out.push(literal_word(text(node, self.source))),
            "raw_string" => out.push(Part::Lit(text(node, self.source).trim_matches('\'').to_string())),
            "string_content" => out.push(Part::Lit(text(node, self.source).to_string())),
            "string" | "concatenation" => {
                let mut cursor = node.walk();
                for c in node.named_children(&mut cursor) {
                    self.parts(c, out, depth + 1);
                }
            }
            "simple_expansion" | "expansion" => out.push(self.expansion(node)),
            "command_substitution" => out.push(self.substitution(node, depth)),
            _ => out.push(Part::Dynamic),
        }
    }

    /// `$X`, `${X}`, `${BASH_SOURCE%/*}`, `${BASH_SOURCE[0]}`, zsh `${0:A:h}`.
    fn expansion(&self, node: Node<'_>) -> Part {
        let raw = text(node, self.source);
        let inner = raw.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}');
        let (name, op) = split_expansion(inner);
        if SCRIPT_FILE_VARS.contains(&name) {
            return match op {
                "" | "[0]" | ":A" | ":a" => Part::ScriptFile,
                "%/*" | "[0]%/*" | ":h" | ":A:h" | ":a:h" => Part::Pattern(PathPattern::script_dir()),
                _ => Part::Dynamic,
            };
        }
        match (op, self.vars.get(name)) {
            ("", Some(p)) => Part::Pattern(p.clone()),
            _ => Part::Dynamic,
        }
    }

    /// `$(dirname "$0")`, `$(cd "$(dirname "$0")/.." && pwd)`,
    /// `$(realpath "$0")`, `$(readlink -f …)`.
    fn substitution(&self, node: Node<'_>, depth: usize) -> Part {
        let Some(stmt) = node.named_child(0).filter(|_| node.named_child_count() == 1) else { return Part::Dynamic };
        match stmt.kind() {
            "command" => self.path_command(stmt, depth),
            "list" => self.cd_and_pwd(stmt, depth),
            _ => Part::Dynamic,
        }
    }

    /// The value of a path command: `dirname P`, `realpath P`, `readlink
    /// -f P`, `cd P && pwd` (as a `list`).
    fn path_command(&self, cmd: Node<'_>, depth: usize) -> Part {
        let name = cmd.child_by_field_name("name").map_or("", |n| text(n, self.source));
        let Some(arg) = last_argument(cmd, self.source) else { return Part::Dynamic };
        let value = self.single(arg, depth);
        match (name, value) {
            ("dirname", Part::ScriptFile) => Part::Pattern(PathPattern::script_dir()),
            ("dirname", Part::Pattern(p)) => Part::Pattern(p.parent()),
            ("realpath" | "readlink", v @ (Part::ScriptFile | Part::Pattern(_))) => v,
            _ => Part::Dynamic,
        }
    }

    /// `cd P && pwd [-P]` (also `cd P; pwd`): the value of `P`.
    fn cd_and_pwd(&self, list: Node<'_>, depth: usize) -> Part {
        let (Some(first), Some(second)) = (list.named_child(0), list.named_child(1)) else { return Part::Dynamic };
        let first_name = first.child_by_field_name("name").map_or("", |n| text(n, self.source));
        let second_name = second.child_by_field_name("name").map_or("", |n| text(n, self.source));
        if first_name != "cd" || second_name != "pwd" {
            return Part::Dynamic;
        }
        match last_argument(first, self.source).map(|a| self.single(a, depth)) {
            Some(Part::Pattern(p)) => Part::Pattern(p),
            _ => Part::Dynamic,
        }
    }

    /// One argument evaluated as a whole value.
    fn single(&self, node: Node<'_>, depth: usize) -> Part {
        let mut parts = Vec::new();
        self.parts(node, &mut parts, depth + 1);
        if let [Part::ScriptFile] = parts.as_slice() {
            return Part::ScriptFile;
        }
        let mut out = PathPattern::literal("");
        for p in parts {
            out.push(p);
        }
        match out.anchor {
            Anchor::Unknown => Part::Dynamic,
            _ => Part::Pattern(out),
        }
    }
}

/// A word in a path: `~` and globs are not literal.
fn literal_word(w: &str) -> Part {
    if w.starts_with('~') || w.contains(['*', '?', '[']) { Part::Dynamic } else { Part::Lit(w.replace('\\', "")) }
}

/// The last argument of a command that is not an option (`dirname --
/// "$0"`, `readlink -f "$0"`).
fn last_argument<'t>(cmd: Node<'t>, source: &[u8]) -> Option<Node<'t>> {
    let mut cursor = cmd.walk();
    let args: Vec<Node<'t>> = cmd.children_by_field_name("argument", &mut cursor).collect();
    args.into_iter().rev().find(|a| !text(*a, source).starts_with('-'))
}

/// Splits the inside of `${…}` into the name and the operator text
/// (`BASH_SOURCE[0]%/*` → `BASH_SOURCE`, `[0]%/*`).
fn split_expansion(inner: &str) -> (&str, &str) {
    let end = inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(inner.len());
    if end == 0 {
        return ("", inner);
    }
    // `$0` is a one-character special name; `${0:h}` too.
    if inner.as_bytes()[0].is_ascii_digit() {
        return (&inner[..1], &inner[1..]);
    }
    inner.split_at(end)
}

/// Normalizes `a/./b/../c` to `a/c`; `None` when it leaves the root.
pub(crate) fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

/// The directory of a repository path (`""` at the root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, rest: &str) -> Option<String> {
    normalize(&format!("{dir}/{rest}"))
}

/// The file of the snapshot a pattern written in `from` names (see the
/// module docs).
pub(crate) fn resolve(from: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    let (anchor, rest) = decode(module);
    let found = |p: Option<String>| p.filter(|p| !p.is_empty() && project.contains(p));
    match anchor {
        Anchor::ScriptDir => found(join(dir_of(from), rest)),
        Anchor::Literal if rest.starts_with('/') => None,
        Anchor::Literal => found(join(dir_of(from), rest)).or_else(|| found(normalize(rest))),
        Anchor::Unknown => {
            let tail = unknown_tail(rest)?;
            let suffix = format!("/{tail}");
            let mut hits = project.paths.iter().filter(|p| p.as_str() == tail || p.ends_with(&suffix));
            match (hits.next(), hits.next()) {
                (Some(p), None) => Some(p.clone()),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(paths: &[&str]) -> ProjectFiles {
        ProjectFiles { paths: paths.iter().map(|p| p.to_string()).collect(), configs: Default::default() }
    }

    #[test]
    fn resolution() {
        let p = project(&["bin/run.sh", "lib/util.sh", "lib/net.sh", "nvm.sh", "a/x.sh", "b/x.sh", "run.sh"]);
        assert_eq!(resolve("bin/run.sh", "$dir/../lib/util.sh", &p).as_deref(), Some("lib/util.sh"));
        assert_eq!(resolve("lib/net.sh", "./util.sh", &p).as_deref(), Some("lib/util.sh"));
        assert_eq!(resolve("bin/run.sh", "lib/util.sh", &p).as_deref(), Some("lib/util.sh"));
        assert_eq!(resolve("bin/run.sh", "*/nvm.sh", &p).as_deref(), Some("nvm.sh"));
        assert_eq!(resolve("bin/run.sh", "*/x.sh", &p), None);
        assert_eq!(resolve("bin/run.sh", "*/a/x.sh", &p).as_deref(), Some("a/x.sh"));
        assert_eq!(resolve("bin/run.sh", "*.sh", &p), None);
        assert_eq!(resolve("bin/run.sh", "/etc/profile", &p), None);
        assert_eq!(resolve("run.sh", "$dir/../../x.sh", &p), None);
        assert_eq!(resolve("bin/run.sh", "$dir/missing.sh", &p), None);
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(decode("$dir/x"), (Anchor::ScriptDir, "/x"));
        assert_eq!(split_expansion("BASH_SOURCE[0]%/*"), ("BASH_SOURCE", "[0]%/*"));
        assert_eq!(split_expansion("0:A:h"), ("0", ":A:h"));
    }
}
