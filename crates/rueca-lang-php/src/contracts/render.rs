//! The URL an expression builds: literal pieces and holes (interpolated
//! variables, concatenated unknowns), and the HTTP key of it.

use std::collections::HashMap;

use rueca_extract::text_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{Part, http_key_from_parts};

use super::{Ctx, positional};

/// Owned [`Part`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    Lit(String),
    Hole,
}

/// How deep [`Ctx::render`] follows names and nested expressions.
const MAX_RENDER_DEPTH: usize = 4;

/// Function-like nodes (their variables scope a render).
const FUNCTIONS: &[&str] = &["method_declaration", "function_definition", "anonymous_function", "arrow_function"];

impl<'t> Ctx<'t, '_> {
    /// The pieces `node` builds: string literals, interpolated strings,
    /// `.` concatenation (an unknown side is a hole), `sprintf('/x/%s', …)`,
    /// a variable assigned once in its function.
    pub(super) fn render(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        if depth > MAX_RENDER_DEPTH {
            return None;
        }
        match node.kind() {
            "string" | "encapsed_string" => Some(string_pieces(node, self.src)),
            "parenthesized_expression" => self.render(node.named_child(0)?, depth + 1),
            "binary_expression" => self.render_concat(node, depth),
            "function_call_expression" => {
                let func = node.child_by_field_name("function")?;
                if text_of(func, self.src).trim_start_matches('\\') != "sprintf" {
                    return None;
                }
                let first = *positional(node, self.src).first()?;
                self.render(first, depth + 1)
            }
            "variable_name" => {
                let value = self.single_assignment(node)?;
                self.render(value, depth + 1)
            }
            _ => None,
        }
    }

    fn render_concat(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let op = node.child_by_field_name("operator")?;
        if text_of(op, self.src) != "." {
            return None;
        }
        let l = self.render(node.child_by_field_name("left")?, depth + 1);
        let r = self.render(node.child_by_field_name("right")?, depth + 1);
        if l.is_none() && r.is_none() {
            return None;
        }
        let mut out = l.unwrap_or_else(|| vec![Piece::Hole]);
        out.extend(r.unwrap_or_else(|| vec![Piece::Hole]));
        Some(out)
    }

    /// The value of `$name` when its function assigns it exactly once.
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

/// The nearest function around `node`.
fn enclosing_function(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if FUNCTIONS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// `$name = value` assignments of a function body: the value when the
/// variable is assigned once, `None` when more often.
fn assignments<'t>(func: Node<'t>, src: &[u8], max_depth: usize) -> HashMap<String, Option<Node<'t>>> {
    let mut out: HashMap<String, Option<Node<'t>>> = HashMap::new();
    let Some(body) = func.child_by_field_name("body") else { return out };
    let mut stack = vec![(body, 0usize)];
    while let Some((n, depth)) = stack.pop() {
        if matches!(n.kind(), "assignment_expression" | "augmented_assignment_expression")
            && let (Some(l), Some(r)) = (n.child_by_field_name("left"), n.child_by_field_name("right"))
            && l.kind() == "variable_name"
        {
            let once = n.kind() == "assignment_expression";
            out.entry(text_of(l, src).into_owned()).and_modify(|v| *v = None).or_insert(once.then_some(r));
        }
        if depth < max_depth {
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
        }
    }
    out
}

/// Pieces of a string literal: text, and a hole per interpolation.
fn string_pieces(node: Node<'_>, src: &[u8]) -> Vec<Piece> {
    if node.kind() == "string" {
        let t = text_of(node, src);
        let inner = t.get(1..t.len().saturating_sub(1)).unwrap_or_default();
        return vec![Piece::Lit(inner.to_string())];
    }
    let mut out = Vec::new();
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        match ch.kind() {
            "string_content" | "escape_sequence" => out.push(Piece::Lit(text_of(ch, src).into_owned())),
            _ => out.push(Piece::Hole),
        }
    }
    out
}

/// Value of a string literal without interpolation.
pub(super) fn plain_string(node: Node<'_>, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut out = String::new();
    for p in string_pieces(node, src) {
        match p {
            Piece::Lit(s) => out.push_str(&s),
            Piece::Hole => return None,
        }
    }
    Some(out)
}

/// Plain strings of an array literal (`['GET', 'POST']`), or of a single
/// string.
pub(super) fn string_list(node: Node<'_>, src: &[u8]) -> Vec<String> {
    if let Some(s) = plain_string(node, src) {
        return vec![s];
    }
    if node.kind() != "array_creation_expression" {
        return Vec::new();
    }
    let mut c = node.walk();
    node.named_children(&mut c)
        .filter(|e| e.kind() == "array_element_initializer" && e.named_child_count() == 1)
        .filter_map(|e| plain_string(e.named_child(0)?, src))
        .collect()
}

/// The value of key `key` of an array literal (`['prefix' => 'api']`).
pub(super) fn array_value<'t>(node: Node<'t>, key: &str, src: &[u8]) -> Option<Node<'t>> {
    if node.kind() != "array_creation_expression" {
        return None;
    }
    let mut c = node.walk();
    node.named_children(&mut c)
        .filter(|e| e.kind() == "array_element_initializer" && e.named_child_count() == 2)
        .find_map(|e| {
            let k = e.named_child(0)?;
            (plain_string(k, src).as_deref() == Some(key)).then(|| e.named_child(1)).flatten()
        })
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
