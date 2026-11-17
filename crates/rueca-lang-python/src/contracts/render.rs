//! The URL an expression builds: literal pieces and holes (f-string
//! interpolations, concatenated unknowns), and the HTTP key of it.

use std::collections::HashMap;

use rueca_extract::text_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{Part, http_key_from_parts};

use super::Ctx;

/// Owned [`Part`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    Lit(String),
    Hole,
}

/// How deep [`Ctx::render`] follows names and nested expressions.
const MAX_RENDER_DEPTH: usize = 4;

impl<'t> Ctx<'t, '_> {
    /// The pieces `node` builds: string literals, f-strings, `+`
    /// concatenation (an unknown side is a hole), `"…" % x`,
    /// `"…".format(…)`, a wrapper call whose first argument is a literal
    /// template (`path_template("/files/{id}", id=x)`), or a local name
    /// assigned once in its function to one of these.
    pub(super) fn render(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        if depth > MAX_RENDER_DEPTH {
            return None;
        }
        match node.kind() {
            "string" => string_pieces(node, self.src),
            "concatenated_string" => self.render_all(node, depth),
            "parenthesized_expression" => self.render(node.named_child(0)?, depth + 1),
            "binary_operator" => self.render_binary(node, depth),
            "call" => self.render_call(node, depth),
            "identifier" => {
                let value = self.single_assignment(node)?;
                self.render(value, depth + 1)
            }
            _ => None,
        }
    }

    fn render_all(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let mut out = Vec::new();
        let mut c = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut c).collect();
        for ch in children {
            out.extend(self.render(ch, depth + 1)?);
        }
        Some(out)
    }

    fn render_binary(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let op = node.child_by_field_name("operator").map(|o| text_of(o, self.src).into_owned())?;
        let left = node.child_by_field_name("left")?;
        let right = node.child_by_field_name("right")?;
        match op.as_str() {
            "%" => self.render(left, depth + 1),
            "+" => {
                let l = self.render(left, depth + 1);
                let r = self.render(right, depth + 1);
                if l.is_none() && r.is_none() {
                    return None;
                }
                let mut out = l.unwrap_or_else(|| vec![Piece::Hole]);
                out.extend(r.unwrap_or_else(|| vec![Piece::Hole]));
                Some(out)
            }
            _ => None,
        }
    }

    fn render_call(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let func = node.child_by_field_name("function")?;
        if func.kind() == "attribute"
            && func.child_by_field_name("attribute").is_some_and(|a| text_of(a, self.src) == "format")
        {
            return self.render(func.child_by_field_name("object")?, depth + 1);
        }
        let first = *super::positional(node).first()?;
        if matches!(first.kind(), "string" | "concatenated_string") { self.render(first, depth + 1) } else { None }
    }

    /// The value of `name` when its function assigns it exactly once.
    fn single_assignment(&mut self, name: Node<'t>) -> Option<Node<'t>> {
        let func = enclosing_function(name)?;
        if !self.assigned.contains_key(&func.id()) {
            let table = assignments(func, self.src, self.max_depth);
            self.assigned.insert(func.id(), table);
        }
        let key = text_of(name, self.src);
        self.assigned.get(&func.id())?.get(key.as_ref()).copied().flatten()
    }
}

/// The nearest `def` around `node`.
fn enclosing_function(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_definition" {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// `name = value` assignments of a function body: the value when the name
/// is assigned once, `None` when more often.
fn assignments<'t>(func: Node<'t>, src: &[u8], max_depth: usize) -> HashMap<String, Option<Node<'t>>> {
    let mut out: HashMap<String, Option<Node<'t>>> = HashMap::new();
    let Some(body) = func.child_by_field_name("body") else { return out };
    let mut stack = vec![(body, 0usize)];
    while let Some((n, depth)) = stack.pop() {
        if n.kind() == "assignment"
            && let (Some(l), Some(r)) = (n.child_by_field_name("left"), n.child_by_field_name("right"))
            && l.kind() == "identifier"
        {
            out.entry(text_of(l, src).into_owned()).and_modify(|v| *v = None).or_insert(Some(r));
        }
        if depth < max_depth {
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
        }
    }
    out
}

/// Pieces of a string literal: text, and a hole per f-string
/// interpolation. `None` for bytes.
fn string_pieces(node: Node<'_>, src: &[u8]) -> Option<Vec<Piece>> {
    let mut out = Vec::new();
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        match ch.kind() {
            "string_start" if text_of(ch, src).contains(['b', 'B']) => return None,
            "string_content" | "escape_sequence" => out.push(Piece::Lit(text_of(ch, src).into_owned())),
            "interpolation" => out.push(Piece::Hole),
            _ => {}
        }
    }
    Some(out)
}

/// Value of a plain (not f-, not bytes) string literal.
pub(super) fn plain_string(node: Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let pieces = string_pieces(node, src)?;
    let mut out = String::new();
    for p in pieces {
        match p {
            Piece::Lit(s) => out.push_str(&s),
            Piece::Hole => return None,
        }
    }
    Some(out)
}

/// Plain strings of a literal list / tuple / set (`["a", "b"]`), or of a
/// single string.
pub(super) fn string_list(node: Node<'_>, src: &[u8]) -> Vec<String> {
    if node.kind() == "string" {
        return plain_string(node, src).into_iter().collect();
    }
    if !matches!(node.kind(), "list" | "tuple" | "set") {
        return Vec::new();
    }
    let mut c = node.walk();
    node.named_children(&mut c).filter_map(|n| plain_string(n, src)).collect()
}

/// Whether the pieces render something an HTTP client could send: `/…`,
/// `http(s)://…`, or an interpolated base URL followed by `/…`.
pub(super) fn looks_like_path(pieces: &[Piece]) -> bool {
    let joined: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => s.as_str(),
            Piece::Hole => "{}",
        })
        .collect();
    !joined.contains(char::is_whitespace)
        && joined.len() <= 300
        && ["/", "http://", "https://", "{}/"].iter().any(|p| joined.starts_with(p))
}

/// HTTP key of `method` (`*` = unknown) and the rendered URL.
pub(super) fn http_key(method: &str, pieces: &[Piece]) -> Option<String> {
    let parts: Vec<Part<'_>> = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => Part::Lit(s),
            Piece::Hole => Part::Hole,
        })
        .collect();
    http_key_from_parts((method != "*").then_some(method), &parts)
}
