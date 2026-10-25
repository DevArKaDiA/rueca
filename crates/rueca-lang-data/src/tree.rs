//! The spanned data tree every mapping-shaped format is converted to
//! (JSON, YAML, TOML, `.env`, INI, `.properties`): maps, sequences, scalars
//! and YAML aliases, with byte ranges into the (UTF-8) source. Values are
//! kept only as source ranges and decoded text; nothing is expanded.

use std::ops::Range;

/// Maximum key depth emitted (RFC 0003 §4.5); deeper subtrees are folded
/// into their ancestor's preview. Front ends stop building the tree a few
/// levels below this.
pub const MAX_DEPTH: usize = 32;

/// Depth at which front ends stop building nodes (deeper values become
/// [`Value::Folded`]).
pub(crate) const BUILD_DEPTH: usize = MAX_DEPTH + 2;

#[derive(Debug)]
pub(crate) struct Node {
    pub value: Value,
    /// Byte range of the value as written (quotes included), trimmed.
    pub span: Range<usize>,
    /// YAML anchor id (`&name`), if any.
    pub anchor: Option<usize>,
}

#[derive(Debug)]
pub(crate) enum Value {
    Map(Vec<Entry>),
    Seq(Vec<Node>),
    Scalar(String),
    /// YAML alias (`*name`) to an anchor id.
    Alias(usize),
    /// A subtree below the build depth, or a value the format cannot
    /// represent (YAML complex keys).
    Folded,
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub key: String,
    pub key_span: Range<usize>,
    pub value: Node,
    /// The key is a dotted path to split into several segments (`.properties`
    /// keys, Spring YAML keys): `server.port` → `server` / `port`.
    pub split: bool,
}

#[derive(Debug)]
pub(crate) struct Tree {
    pub docs: Vec<Node>,
    /// Recoverable errors (YAML stops at the first one and keeps the tree
    /// built so far).
    pub errors: u32,
}

impl Node {
    pub(crate) fn new(value: Value, span: Range<usize>) -> Node {
        Node { value, span, anchor: None }
    }

    pub(crate) fn as_map(&self) -> Option<&[Entry]> {
        match &self.value {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn as_seq(&self) -> Option<&[Node]> {
        match &self.value {
            Value::Seq(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// Value of entry `key` in a map.
    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?.iter().find(|e| e.key == key).map(|e| &e.value)
    }

    pub(crate) fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// Whether this is an empty or null scalar (`key:` in YAML).
    pub(crate) fn is_null(&self) -> bool {
        matches!(&self.value, Value::Scalar(s) if s.is_empty() || s == "null" || s == "~")
    }

    /// Stable address, used to map tree nodes to emitted definitions.
    pub(crate) fn addr(&self) -> usize {
        self as *const Node as usize
    }
}

/// Visits every node of a subtree (pre-order) with its depth; bounded by
/// the build depth, iterative.
pub(crate) fn walk<'t>(root: &'t Node, mut f: impl FnMut(&'t Node)) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        f(n);
        match &n.value {
            Value::Map(m) => stack.extend(m.iter().rev().map(|e| &e.value)),
            Value::Seq(s) => stack.extend(s.iter().rev()),
            _ => {}
        }
    }
}
