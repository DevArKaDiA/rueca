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
    let mut escape = '\\';
    let mut out = Vec::new();
    let mut directives = true;
    let mut pos = 0;
    let mut current: Option<(usize, String)> = None;
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    for raw in src.split_inclusive('\n') {
        let start = pos;
        pos += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if let Some((term, strip_tabs)) = heredocs.first() {
            let l = if *strip_tabs { line.trim_start_matches('\t') } else { line };
            if l == term {
                heredocs.remove(0);
            }
            if let Some(last) = out.last_mut() {
                let last: &mut Instruction = last;
                last.range.end = start + line.len();
            }
            continue;
        }
        let t = line.trim_start();
        if directives {
            let d = t.strip_prefix('#').map(str::trim);
            match d.and_then(|d| d.split_once('=')) {
                Some((k, v)) if !k.trim().contains(' ') => {
                    if k.trim().eq_ignore_ascii_case("escape") && v.trim() == "`" {
                        escape = '`';
                    }
                    continue;
                }
                _ => directives = false,
            }
        }
        if t.starts_with('#') || (t.is_empty() && current.is_none()) {
            continue;
        }
        if t.is_empty() {
            continue;
        }
        let (s, mut text) = current.take().unwrap_or((start + (line.len() - t.len()), String::new()));
        let body = t.trim_end();
        if let Some(stripped) = body.strip_suffix(escape) {
            text.push_str(stripped);
            text.push(' ');
            current = Some((s, text));
            continue;
        }
        text.push_str(body);
        let end = start + line.trim_end().len();
        let (kw, args) = match text.split_once(char::is_whitespace) {
            Some((k, a)) => (k.to_ascii_uppercase(), a.trim().to_string()),
            None => (text.to_ascii_uppercase(), String::new()),
        };
        heredocs.extend(heredoc_terminators(&args));
        out.push(Instruction { keyword: kw, args, range: s..end });
        if out.len() >= MAX_INSTRUCTIONS {
            break;
        }
    }
    if let Some((s, text)) = current {
        let (kw, args) = text.split_once(char::is_whitespace).unwrap_or((text.as_str(), ""));
        out.push(Instruction { keyword: kw.to_ascii_uppercase(), args: args.trim().to_string(), range: s..src.len() });
    }
    out
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
        match word.split_once('=') {
            Some((k, v)) => out.push((k.to_string(), v.to_string())),
            None => out.push((word, String::new())),
        }
    }
    out
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

pub(crate) fn extract(out: &mut Out<'_>) {
    let src = out.src;
    let mut stages: Vec<Stage> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for ins in instructions(src) {
        let in_src = |needle: &str| -> Range<usize> {
            let hay = src.get(ins.range.clone()).unwrap_or("");
            match hay.get(ins.keyword.len()..).and_then(|h| h.find(needle)) {
                Some(i) if !needle.is_empty() => {
                    let s = ins.range.start + ins.keyword.len() + i;
                    s..s + needle.len()
                }
                _ => ins.range.start..ins.range.start + ins.keyword.len().min(ins.range.len()),
            }
        };
        // Parser directives (`# syntax=…`) are not docs.
        let doc = out
            .doc(ins.range.start, None)
            .map(|d| d.lines().filter(|l| !is_directive(l)).collect::<Vec<_>>().join("\n"))
            .filter(|d| !d.trim().is_empty());
        if ins.keyword != "FROM"
            && let Some(st) = stages.last_mut()
        {
            st.end = ins.range.end;
        }
        match ins.keyword.as_str() {
            "FROM" => {
                let (_, rest) = flags(&ins.args);
                let words: Vec<&str> = rest.split_whitespace().collect();
                let Some(image) = words.first().copied() else { continue };
                let alias = match words.as_slice() {
                    [_, as_kw, name, ..] if as_kw.eq_ignore_ascii_case("as") => Some(*name),
                    _ => None,
                };
                let index = stages.len();
                let (qn, name, name_span) = match alias {
                    Some(a) => (keypath::key_segment(a), a.to_string(), in_src(a)),
                    None => {
                        let q = format!("stage{}", keypath::index_segment(index));
                        (q.clone(), q, in_src(image))
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
                    return;
                };
                if let Some(&base) = by_name.get(&image.to_ascii_lowercase()) {
                    let b = stages[base].qn.clone();
                    out.get(id).bases.push(b);
                }
                if let Some(a) = alias {
                    by_name.entry(a.to_ascii_lowercase()).or_insert(index);
                }
                stages.push(Stage { id, qn, start: ins.range.start, end: ins.range.end });
            }
            "ENV" | "ARG" => {
                let is_env = ins.keyword == "ENV";
                let list: Vec<(String, String, bool)> =
                    if is_env && !ins.args.split_whitespace().next().unwrap_or("").contains('=') {
                        // Legacy `ENV KEY value with spaces`.
                        match ins.args.split_once(char::is_whitespace) {
                            Some((k, v)) => vec![(k.to_string(), v.trim().to_string(), true)],
                            None => vec![(ins.args.clone(), String::new(), false)],
                        }
                    } else {
                        pairs(&ins.args)
                            .into_iter()
                            .map(|(k, v)| {
                                let has = ins.args.contains(&format!("{k}="));
                                (k, v, has)
                            })
                            .collect()
                    };
                let stage = stages.last();
                for (k, v, has_value) in list {
                    if !text::is_env_name(&k) {
                        continue;
                    }
                    let qn = match stage {
                        Some(s) => keypath::join(&s.qn, &keypath::key_segment(&k)),
                        None => keypath::key_segment(&k),
                    };
                    let (label, sig) = if is_env {
                        (Label::EnvVar, if has_value { format!("ENV {k}={MASK}") } else { format!("ENV {k}") })
                    } else if !has_value {
                        (Label::Variable, format!("ARG {k}"))
                    } else if text::is_secret_name(&k) {
                        (Label::Variable, format!("ARG {k}={MASK}"))
                    } else {
                        (Label::Variable, text::preview(&format!("ARG {k}={v}")))
                    };
                    out.def(Def {
                        label,
                        name: k.clone(),
                        qn,
                        parent: stage.map(|s| s.id),
                        span: ins.range.clone(),
                        name_span: in_src(&k),
                        signature: Some(sig),
                        doc: doc.clone(),
                    });
                }
            }
            "COPY" | "ADD" | "RUN" => {
                let (fl, _) = flags(&ins.args);
                let mut froms: Vec<String> = Vec::new();
                for (k, v) in fl {
                    match k.as_str() {
                        "from" => froms.push(v),
                        "mount" => {
                            froms.extend(v.split(',').filter_map(|p| p.strip_prefix("from=")).map(str::to_string))
                        }
                        _ => {}
                    }
                }
                let enclosing = stages.last().map(|s| s.id);
                for f in froms {
                    let target = by_name
                        .get(&f.to_ascii_lowercase())
                        .copied()
                        .or_else(|| f.parse::<usize>().ok().filter(|&i| i < stages.len()));
                    if let Some(t) = target {
                        let qn = stages[t].qn.clone();
                        let span = in_src(&f);
                        out.reference(RefKind::DataRef, &qn, None, enclosing, &span);
                    }
                }
            }
            _ => {}
        }
    }
    for s in &stages {
        let span = out.span(&(s.start..s.end));
        out.get(s.id).span = span;
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
