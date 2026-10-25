//! JSON and JSONC → data tree, with `jsonc-parser` (MIT, byte ranges on
//! every node, depth limit 512). Every `.json` is parsed leniently:
//! comments, trailing commas, single-quoted strings and bare property names
//! are accepted (`tsconfig.json`, VS Code settings). JSON5 files are not
//! detected.

use jsonc_parser::ast::{ObjectPropName, Value as JValue};
use jsonc_parser::{CollectOptions, CommentCollectionStrategy, ParseOptions, parse_to_ast};

use crate::tree::{BUILD_DEPTH, Entry, Node, Tree, Value};

pub(crate) fn parse(src: &str) -> Result<Tree, String> {
    let collect = CollectOptions { comments: CommentCollectionStrategy::Off, tokens: false };
    let parsed = parse_to_ast(src, &collect, &ParseOptions::default()).map_err(|e| e.to_string())?;
    let docs = parsed.value.as_ref().map(|v| convert(v, 0)).into_iter().collect();
    Ok(Tree { docs, errors: 0 })
}

fn convert(v: &JValue<'_>, depth: usize) -> Node {
    let r = |r: jsonc_parser::common::Range| r.start..r.end;
    if depth >= BUILD_DEPTH {
        let range = match v {
            JValue::Object(o) => r(o.range),
            JValue::Array(a) => r(a.range),
            JValue::StringLit(s) => r(s.range),
            JValue::NumberLit(n) => r(n.range),
            JValue::BooleanLit(b) => r(b.range),
            JValue::NullKeyword(n) => r(n.range),
        };
        return Node::new(Value::Folded, range);
    }
    match v {
        JValue::Object(o) => {
            let entries = o
                .properties
                .iter()
                .map(|p| {
                    let (key, key_span) = match &p.name {
                        ObjectPropName::String(s) => (s.value.to_string(), r(s.range)),
                        ObjectPropName::Word(w) => (w.value.to_string(), r(w.range)),
                    };
                    Entry { key, key_span, value: convert(&p.value, depth + 1), split: false }
                })
                .collect();
            Node::new(Value::Map(entries), r(o.range))
        }
        JValue::Array(a) => {
            Node::new(Value::Seq(a.elements.iter().map(|e| convert(e, depth + 1)).collect()), r(a.range))
        }
        JValue::StringLit(s) => Node::new(Value::Scalar(s.value.to_string()), r(s.range)),
        JValue::NumberLit(n) => Node::new(Value::Scalar(n.value.to_string()), r(n.range)),
        JValue::BooleanLit(b) => Node::new(Value::Scalar(b.value.to_string()), r(b.range)),
        JValue::NullKeyword(n) => Node::new(Value::Scalar("null".into()), r(n.range)),
    }
}
