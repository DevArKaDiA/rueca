//! TOML → data tree with `toml_edit` (MIT OR Apache-2.0; `Document::parse`
//! keeps byte spans on keys and values; its recursion guard stays on).
//!
//! Tables, array-of-tables entries (`[[bin]]` → `bin[0]`), dotted keys and
//! key/value pairs are entries by key path, even when a table is reopened
//! later in the file; a table's span starts at its first header and ends
//! with its last key. Entries are ordered by position.

use std::ops::Range;

use toml_edit::{Document, InlineTable, Item, Key, Table, Value as TValue};

use crate::tree::{BUILD_DEPTH, Entry, Node, Tree, Value};

pub(crate) fn parse(src: &str) -> Result<Tree, String> {
    let doc = Document::parse(src).map_err(|e| e.to_string())?;
    let root = table(doc.as_table(), 0..0, 0);
    let mut root = root;
    root.span = 0..src.trim_end().len();
    Ok(Tree { docs: vec![root], errors: 0 })
}

fn key_span(k: Option<&Key>) -> Range<usize> {
    k.and_then(Key::span).unwrap_or(0..0)
}

fn finish(mut entries: Vec<Entry>, own: Option<Range<usize>>, fallback: Range<usize>) -> Node {
    entries.sort_by_key(|e| e.key_span.start);
    let start = own.as_ref().map(|r| r.start).or_else(|| entries.first().map(|e| e.key_span.start));
    let start = start.unwrap_or(fallback.start);
    let end = entries.iter().map(|e| e.value.span.end).chain(own.map(|r| r.end)).max().unwrap_or(fallback.end);
    Node::new(Value::Map(entries), start..end.max(start))
}

fn table(t: &Table, fallback: Range<usize>, depth: usize) -> Node {
    if depth >= BUILD_DEPTH {
        return Node::new(Value::Folded, t.span().unwrap_or(fallback));
    }
    let entries = t
        .iter()
        .map(|(k, item)| {
            let ks = key_span(t.key(k));
            Entry { key: k.to_string(), value: self::item(item, ks.clone(), depth + 1), key_span: ks, split: false }
        })
        .collect();
    finish(entries, t.span(), fallback)
}

fn inline(t: &InlineTable, depth: usize) -> Node {
    let fallback = t.span().unwrap_or(0..0);
    if depth >= BUILD_DEPTH {
        return Node::new(Value::Folded, fallback);
    }
    let entries = t
        .iter()
        .map(|(k, v)| {
            let ks = key_span(t.key(k));
            Entry { key: k.to_string(), value: value(v, depth + 1), key_span: ks, split: false }
        })
        .collect();
    let own = if t.is_dotted() { None } else { t.span() };
    finish(entries, own, fallback)
}

fn item(i: &Item, key: Range<usize>, depth: usize) -> Node {
    match i {
        Item::None => Node::new(Value::Folded, key),
        Item::Value(v) => value(v, depth),
        Item::Table(t) => table(t, key, depth),
        Item::ArrayOfTables(a) => {
            let items: Vec<Node> = if depth >= BUILD_DEPTH {
                Vec::new()
            } else {
                a.iter().map(|t| table(t, key.clone(), depth + 1)).collect()
            };
            let start = items.first().map_or(key.start, |n| n.span.start);
            let end = items.iter().map(|n| n.span.end).max().unwrap_or(key.end);
            Node::new(Value::Seq(items), start..end.max(start))
        }
    }
}

fn value(v: &TValue, depth: usize) -> Node {
    let span = v.span().unwrap_or(0..0);
    match v {
        TValue::Array(a) => {
            let items = if depth >= BUILD_DEPTH { Vec::new() } else { a.iter().map(|x| value(x, depth + 1)).collect() };
            Node::new(Value::Seq(items), span)
        }
        TValue::InlineTable(t) => inline(t, depth),
        TValue::String(s) => Node::new(Value::Scalar(s.value().clone()), span),
        TValue::Integer(i) => Node::new(Value::Scalar(i.value().to_string()), span),
        TValue::Float(f) => Node::new(Value::Scalar(f.value().to_string()), span),
        TValue::Boolean(b) => Node::new(Value::Scalar(b.value().to_string()), span),
        TValue::Datetime(d) => Node::new(Value::Scalar(d.value().to_string()), span),
    }
}
