//! Dockerfile, a line-oriented parser built here (RFC 0003 §8) against the
//! [Dockerfile reference](https://docs.docker.com/reference/dockerfile/):
//! parser directives (`# escape=`), comments, line continuations, JSON-form
//! arguments and heredocs (`<<EOF`, `<<-EOF`) are understood.
//!
//! | Instruction | Definition | Key path |
//! |---|---|---|
//! | `FROM image [AS name]` | `Image` (a stage) | `name`, unnamed stages `stage[N]` |
//! | `ENV K=v …`, `ENV K v` | `EnvVar` provider, child of the stage | `stage.K` |
//! | `ARG K[=v]` | `Variable` | `stage.K` (`K` before the first `FROM`) |
//!
//! `FROM <earlier stage>` → base of the stage (`INHERITS`);
//! `COPY --from=<stage>` and `RUN --mount=…,from=<stage>` → `DataRef` to the
//! stage (by name or index). ENV values never reach signatures.

use std::collections::HashMap;
use std::ops::Range;

use rueca_model::{Label, LocalDefId, RefKind};

use crate::emit::{Def, Out};
use crate::keypath;
use crate::text::{self, MASK};

/// Instructions read per file.
const MAX_INSTRUCTIONS: usize = 10_000;

struct Instruction {
    keyword: String,
    /// Arguments with continuations joined (one space).
    args: String,
    range: Range<usize>,
}

/// Logical instructions of a Dockerfile.
fn instructions(src: &str) -> Vec<Instruction> {
    let mut lexer = Lexer { escape: '\\', directives: true, current: None, heredocs: Vec::new(), out: Vec::new() };
    let mut pos = 0;
    for raw in src.split_inclusive('\n') {
        let start = pos;
        pos += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if lexer.heredoc_line(line, start) {
            continue;
        }
        let t = line.trim_start();
        if lexer.directives && lexer.directive(t) {
            continue;
        }
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        if lexer.instruction_line(line, t, start) {
            break;
        }
    }
    lexer.finish(src.len())
}

/// Line-by-line state of [`instructions`].
struct Lexer {
    /// The escape (line continuation) character.
    escape: char,
    /// Still in the parser-directive header.
    directives: bool,
    /// Start and text of an instruction continued on the next line.
    current: Option<(usize, String)>,
    /// Pending heredoc terminators (and whether tabs are stripped).
    heredocs: Vec<(String, bool)>,
    out: Vec<Instruction>,
}

impl Lexer {
    /// A line inside a heredoc: extends the last instruction, ends the
    /// heredoc at its terminator. False when no heredoc is open.
    fn heredoc_line(&mut self, line: &str, start: usize) -> bool {
        let Some((term, strip_tabs)) = self.heredocs.first() else { return false };
        let l = if *strip_tabs { line.trim_start_matches('\t') } else { line };
        if l == term {
            self.heredocs.remove(0);
        }
        if let Some(last) = self.out.last_mut() {
            last.range.end = start + line.len();
        }
        true
    }

    /// A `# key=value` parser directive (`escape` is honoured); the first
    /// other line ends the directive header.
    fn directive(&mut self, t: &str) -> bool {
        let d = t.strip_prefix('#').map(str::trim);
        match d.and_then(|d| d.split_once('=')) {
            Some((k, v)) if !k.trim().contains(' ') => {
                if k.trim().eq_ignore_ascii_case("escape") && v.trim() == "`" {
                    self.escape = '`';
                }
                true
            }
            _ => {
                self.directives = false;
                false
            }
        }
    }

    /// A non-empty, non-comment line (`t` is it without leading blanks):
    /// continues or completes an instruction. True once the instruction
    /// limit is reached.
    fn instruction_line(&mut self, line: &str, t: &str, start: usize) -> bool {
        let (s, mut text) = self.current.take().unwrap_or((start + (line.len() - t.len()), String::new()));
        let body = t.trim_end();
        if let Some(stripped) = body.strip_suffix(self.escape) {
            text.push_str(stripped);
            text.push(' ');
            self.current = Some((s, text));
            return false;
        }
        text.push_str(body);
        let end = start + line.trim_end().len();
        let (keyword, args) = split_keyword(&text);
        self.heredocs.extend(heredoc_terminators(&args));
        self.out.push(Instruction { keyword, args, range: s..end });
        self.out.len() >= MAX_INSTRUCTIONS
    }

    /// The instructions, with a trailing continued one ending the file.
    fn finish(mut self, len: usize) -> Vec<Instruction> {
        if let Some((s, text)) = self.current.take() {
            let (keyword, args) = split_keyword(&text);
            self.out.push(Instruction { keyword, args, range: s..len });
        }
        self.out
    }
}

/// Upper-cased keyword and trimmed arguments of an instruction's text.
fn split_keyword(text: &str) -> (String, String) {
    let (kw, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    (kw.to_ascii_uppercase(), args.trim().to_string())
}

/// `<<EOF`, `<<-EOF`, `<<"EOF"` markers in an instruction.
fn heredoc_terminators(args: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = args;
    while let Some(i) = rest.find("<<") {
        let mut r = &rest[i + 2..];
        let strip = r.starts_with('-');
        if strip {
            r = &r[1..];
        }
        let r = r.trim_start_matches(['"', '\'']);
        let word: String = r.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !word.is_empty() {
            out.push((word.clone(), strip));
        }
        rest = &rest[i + 2..];
    }
    out
}

/// Splits `K=v K2="a b"` pairs (quotes and backslash escapes understood).
fn pairs(args: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = args.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let word = shell_word(&mut chars);
        match word.split_once('=') {
            Some((k, v)) => out.push((k.to_string(), v.to_string())),
            None => out.push((word, String::new())),
        }
    }
    out
}

/// One whitespace-delimited word, quotes removed and escapes applied.
fn shell_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    while let Some(&c) = chars.peek() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => break,
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (_, '\\') => {
                chars.next();
                if let Some(n) = chars.next() {
                    word.push(n);
                }
                continue;
            }
            _ => word.push(c),
        }
        chars.next();
    }
    word
}

/// Value of `--name=value` flags at the start of `args`, and the rest.
fn flags(args: &str) -> (Vec<(String, String)>, &str) {
    let mut fl = Vec::new();
    let mut rest = args.trim_start();
    while let Some(r) = rest.strip_prefix("--") {
        let end = r.find(char::is_whitespace).unwrap_or(r.len());
        let (k, v) = r[..end].split_once('=').unwrap_or((&r[..end], ""));
        fl.push((k.to_ascii_lowercase(), v.to_string()));
        rest = r[end..].trim_start();
    }
    (fl, rest)
}

fn is_directive(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(k, _)| matches!(k.trim().to_ascii_lowercase().as_str(), "syntax" | "escape" | "check"))
}

struct Stage {
    id: LocalDefId,
    qn: String,
    start: usize,
    end: usize,
}

impl Instruction {
    /// Source range of `needle` after the keyword, else of the keyword.
    fn locate(&self, src: &str, needle: &str) -> Range<usize> {
        let hay = src.get(self.range.clone()).unwrap_or("");
        match hay.get(self.keyword.len()..).and_then(|h| h.find(needle)) {
            Some(i) if !needle.is_empty() => {
                let s = self.range.start + self.keyword.len() + i;
                s..s + needle.len()
            }
            _ => self.range.start..self.range.start + self.keyword.len().min(self.range.len()),
        }
    }
}

/// The build stages seen so far and their names (lower-cased).
#[derive(Default)]
struct Stages {
    list: Vec<Stage>,
    by_name: HashMap<String, usize>,
}

impl Stages {
    /// Index of the stage named `name` (or numbered `name`).
    fn find(&self, name: &str) -> Option<usize> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .copied()
            .or_else(|| name.parse::<usize>().ok().filter(|&i| i < self.list.len()))
    }
}

pub(crate) fn extract(out: &mut Out<'_>) {
    let src = out.src;
    let mut stages = Stages::default();
    for ins in instructions(src) {
        // Parser directives (`# syntax=…`) are not docs.
        let doc = out
            .doc(ins.range.start, None)
            .map(|d| d.lines().filter(|l| !is_directive(l)).collect::<Vec<_>>().join("\n"))
            .filter(|d| !d.trim().is_empty());
        if ins.keyword != "FROM"
            && let Some(st) = stages.list.last_mut()
        {
            st.end = ins.range.end;
        }
        match ins.keyword.as_str() {
            "FROM" => {
                if !from(out, &mut stages, &ins, doc) {
                    return;
                }
            }
            "ENV" | "ARG" => variables(out, stages.list.last(), &ins, doc),
            "COPY" | "ADD" | "RUN" => stage_refs(out, &stages, &ins),
            _ => {}
        }
    }
    for s in &stages.list {
        let span = out.span(&(s.start..s.end));
        out.get(s.id).span = span;
    }
}

/// `FROM image [AS name]`: a new stage, based on an earlier one when
/// `image` names it. False when the definition limit is reached.
fn from(out: &mut Out<'_>, stages: &mut Stages, ins: &Instruction, doc: Option<String>) -> bool {
    let (_, rest) = flags(&ins.args);
    let words: Vec<&str> = rest.split_whitespace().collect();
    let Some(image) = words.first().copied() else { return true };
    let alias = match words.as_slice() {
        [_, as_kw, name, ..] if as_kw.eq_ignore_ascii_case("as") => Some(*name),
        _ => None,
    };
    let index = stages.list.len();
    let (qn, name, name_span) = match alias {
        Some(a) => (keypath::key_segment(a), a.to_string(), ins.locate(out.src, a)),
        None => {
            let q = format!("stage{}", keypath::index_segment(index));
            (q.clone(), q, ins.locate(out.src, image))
        }
    };
    let line = text::preview(&format!("FROM {}", ins.args));
    let Some(id) = out.def(Def {
        label: Label::Image,
        name,
        qn: qn.clone(),
        parent: None,
        span: ins.range.clone(),
        name_span,
        signature: Some(line),
        doc,
    }) else {
        return false;
    };
    if let Some(&base) = stages.by_name.get(&image.to_ascii_lowercase()) {
        let b = stages.list[base].qn.clone();
        out.get(id).bases.push(b);
    }
    if let Some(a) = alias {
        stages.by_name.entry(a.to_ascii_lowercase()).or_insert(index);
    }
    stages.list.push(Stage { id, qn, start: ins.range.start, end: ins.range.end });
    true
}

/// `(name, value, has a value)` of the variables an `ENV` / `ARG` sets.
fn assignments(ins: &Instruction) -> Vec<(String, String, bool)> {
    let is_env = ins.keyword == "ENV";
    if is_env && !ins.args.split_whitespace().next().unwrap_or("").contains('=') {
        // Legacy `ENV KEY value with spaces`.
        return match ins.args.split_once(char::is_whitespace) {
            Some((k, v)) => vec![(k.to_string(), v.trim().to_string(), true)],
            None => vec![(ins.args.clone(), String::new(), false)],
        };
    }
    pairs(&ins.args)
        .into_iter()
        .map(|(k, v)| {
            let has = ins.args.contains(&format!("{k}="));
            (k, v, has)
        })
        .collect()
}

/// Label and signature of a variable: ENV values and secret-named ARG
/// values are masked.
fn variable_signature(is_env: bool, k: &str, v: &str, has_value: bool) -> (Label, String) {
    if is_env {
        (Label::EnvVar, if has_value { format!("ENV {k}={MASK}") } else { format!("ENV {k}") })
    } else if !has_value {
        (Label::Variable, format!("ARG {k}"))
    } else if text::is_secret_name(k) {
        (Label::Variable, format!("ARG {k}={MASK}"))
    } else {
        (Label::Variable, text::preview(&format!("ARG {k}={v}")))
    }
}

/// `ENV` (env var providers) and `ARG` (variables), children of the stage.
fn variables(out: &mut Out<'_>, stage: Option<&Stage>, ins: &Instruction, doc: Option<String>) {
    let is_env = ins.keyword == "ENV";
    for (k, v, has_value) in assignments(ins) {
        if !text::is_env_name(&k) {
            continue;
        }
        let qn = match stage {
            Some(s) => keypath::join(&s.qn, &keypath::key_segment(&k)),
            None => keypath::key_segment(&k),
        };
        let (label, sig) = variable_signature(is_env, &k, &v, has_value);
        out.def(Def {
            label,
            name: k.clone(),
            qn,
            parent: stage.map(|s| s.id),
            span: ins.range.clone(),
            name_span: ins.locate(out.src, &k),
            signature: Some(sig),
            doc: doc.clone(),
        });
    }
}

/// `--from=<stage>` and `--mount=…,from=<stage>` → references to the stage.
fn stage_refs(out: &mut Out<'_>, stages: &Stages, ins: &Instruction) {
    let (fl, _) = flags(&ins.args);
    let mut froms: Vec<String> = Vec::new();
    for (k, v) in fl {
        match k.as_str() {
            "from" => froms.push(v),
            "mount" => froms.extend(v.split(',').filter_map(|p| p.strip_prefix("from=")).map(str::to_string)),
            _ => {}
        }
    }
    let enclosing = stages.list.last().map(|s| s.id);
    for f in froms {
        if let Some(t) = stages.find(&f) {
            let qn = stages.list[t].qn.clone();
            let span = ins.locate(out.src, &f);
            out.reference(RefKind::DataRef, &qn, None, enclosing, &span);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_lines() {
        let src = "# syntax=docker/dockerfile:1\n# escape=\\\nFROM a AS b\n# c\nRUN x \\\n  # inner\n  y\nRUN <<EOF\necho hi\nEOF\nENV A=1\n";
        let ins = instructions(src);
        let kws: Vec<&str> = ins.iter().map(|i| i.keyword.as_str()).collect();
        assert_eq!(kws, ["FROM", "RUN", "RUN", "ENV"]);
        assert_eq!(ins[1].args, "x  y");
        assert_eq!(
            pairs("A=1 B=\"two words\" C"),
            vec![("A".into(), "1".into()), ("B".into(), "two words".into()), ("C".into(), "".into())]
        );
        let (f, rest) = flags("--from=build --chown=1:1 src dst");
        assert_eq!(f[0], ("from".into(), "build".into()));
        assert_eq!(rest, "src dst");
    }
}
