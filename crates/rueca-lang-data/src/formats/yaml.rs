//! YAML → data tree from `saphyr-parser` **events** (MIT OR Apache-2.0).
//!
//! The tree is built with an explicit stack, never by recursion, and
//! aliases are kept as [`Value::Alias`]: nothing is ever expanded, so a
//! "billion laughs" document costs one node per alias (RFC 0003 §4.7).
//! Subtrees below the build depth are consumed without being stored.
//! Multi-document streams give one tree document per `---` (at most
//! [`MAX_DOCS`]). On a syntax error the tree built so far is kept and the
//! error is counted.
//!
//! saphyr's markers count characters; they are mapped back to byte offsets.
//! Block collections end where their last child ends (the parser's end
//! marker sits at the next token, past comments and blank lines); flow
//! collections end at their closing bracket.

use saphyr_parser::{Event, Parser, ScalarStyle};

use crate::text::CharOffsets;
use crate::tree::{BUILD_DEPTH, Entry, Node, Tree, Value};

/// Documents read per stream.
pub const MAX_DOCS: usize = 1_000;
/// Events read per file (bounds time on pathological input).
const MAX_EVENTS: usize = 2_000_000;

enum Frame {
    Map {
        entries: Vec<Entry>,
        key: Option<(String, std::ops::Range<usize>)>,
        start: usize,
        anchor: usize,
        flow: bool,
    },
    Seq {
        items: Vec<Node>,
        start: usize,
        anchor: usize,
        flow: bool,
    },
    /// A collection below the build depth (or a complex key), skipped.
    Skip {
        depth: usize,
        start: usize,
    },
}

pub(crate) fn parse(src: &str) -> Tree {
    let offsets = CharOffsets::new(src);
    let mut docs: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Node> = None;
    let mut errors = 0;
    let mut parser = Parser::new_from_str(src);
    let mut events = 0usize;
    let bytes = src.as_bytes();
    let is_flow = |at: usize, open: u8| bytes.get(at) == Some(&open);

    loop {
        events += 1;
        if events > MAX_EVENTS {
            errors += 1;
            break;
        }
        let (ev, span) = match parser.next_event() {
            None => break,
            Some(Ok(e)) => e,
            Some(Err(_)) => {
                errors += 1;
                break;
            }
        };
        let start = offsets.byte(span.start.index());
        let end = offsets.byte(span.end.index()).max(start);
        // A finished node to attach to the frame on top of the stack.
        let node: Option<Node> = match ev {
            Event::StreamStart | Event::Nothing => None,
            Event::StreamEnd => break,
            Event::DocumentStart(_) => {
                root = None;
                None
            }
            Event::DocumentEnd => {
                close_all(src, &mut stack, &mut root);
                docs.push(root.take().unwrap_or_else(|| Node::new(Value::Scalar(String::new()), start..start)));
                if docs.len() >= MAX_DOCS {
                    errors += 1;
                    break;
                }
                None
            }
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _)
                if matches!(stack.last(), Some(Frame::Skip { .. })) =>
            {
                let _ = anchor;
                if let Some(Frame::Skip { depth, .. }) = stack.last_mut() {
                    *depth += 1;
                }
                None
            }
            Event::MappingEnd | Event::SequenceEnd if matches!(stack.last(), Some(Frame::Skip { .. })) => {
                let Some(Frame::Skip { depth, start: s }) = stack.last_mut() else { unreachable!() };
                if *depth > 0 {
                    *depth -= 1;
                    None
                } else {
                    let s = *s;
                    stack.pop();
                    Some(Node::new(Value::Folded, s..end))
                }
            }
            Event::Scalar(..) | Event::Alias(_) if matches!(stack.last(), Some(Frame::Skip { .. })) => None,
            Event::MappingStart(anchor, _) => {
                let complex_key = matches!(stack.last(), Some(Frame::Map { key: None, .. }));
                if stack.len() >= BUILD_DEPTH || complex_key {
                    stack.push(Frame::Skip { depth: 0, start });
                } else {
                    stack.push(Frame::Map {
                        entries: Vec::new(),
                        key: None,
                        start,
                        anchor,
                        flow: is_flow(start, b'{'),
                    });
                }
                None
            }
            Event::SequenceStart(anchor, _) => {
                let complex_key = matches!(stack.last(), Some(Frame::Map { key: None, .. }));
                if stack.len() >= BUILD_DEPTH || complex_key {
                    stack.push(Frame::Skip { depth: 0, start });
                } else {
                    stack.push(Frame::Seq { items: Vec::new(), start, anchor, flow: is_flow(start, b'[') });
                }
                None
            }
            Event::MappingEnd | Event::SequenceEnd => stack.pop().map(|f| close(f, end)),
            Event::Scalar(text, style, anchor, _) => {
                let (s, e) = if style == ScalarStyle::Plain && text.is_empty() { (start, start) } else { (start, end) };
                let e = trim_end(src, s, e);
                let mut n = Node::new(Value::Scalar(text.into_owned()), s..e);
                n.anchor = (anchor != 0).then_some(anchor);
                Some(n)
            }
            Event::Alias(id) => Some(Node::new(Value::Alias(id), start..trim_end(src, start, end))),
        };
        if let Some(n) = node {
            attach(src, &mut stack, &mut root, n);
        }
    }
    // Unterminated input (error or event cap): keep what was built.
    close_all(src, &mut stack, &mut root);
    if let Some(r) = root.take() {
        docs.push(r);
    }
    Tree { docs, errors }
}

/// End of a value trimmed back over trailing whitespace.
fn trim_end(src: &str, start: usize, end: usize) -> usize {
    let b = src.as_bytes();
    let mut e = end.min(b.len());
    while e > start && b[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    e
}

fn close(frame: Frame, end: usize) -> Node {
    match frame {
        Frame::Map { entries, start, anchor, flow, .. } => {
            let last = entries.last().map(|e| e.value.span.end.max(e.key_span.end));
            let e = if flow { end } else { last.unwrap_or(start) };
            let mut n = Node::new(Value::Map(entries), start..e.max(start));
            n.anchor = (anchor != 0).then_some(anchor);
            n
        }
        Frame::Seq { items, start, anchor, flow } => {
            let last = items.last().map(|i| i.span.end);
            let e = if flow { end } else { last.unwrap_or(start) };
            let mut n = Node::new(Value::Seq(items), start..e.max(start));
            n.anchor = (anchor != 0).then_some(anchor);
            n
        }
        Frame::Skip { start, .. } => Node::new(Value::Folded, start..end),
    }
}

fn close_all(src: &str, stack: &mut Vec<Frame>, root: &mut Option<Node>) {
    while let Some(f) = stack.pop() {
        let end = match &f {
            Frame::Map { entries, start, .. } => entries.last().map_or(*start, |e| e.value.span.end),
            Frame::Seq { items, start, .. } => items.last().map_or(*start, |i| i.span.end),
            Frame::Skip { start, .. } => *start,
        };
        // A dangling key without a value is dropped.
        let n = close(f, end);
        attach(src, stack, root, n);
    }
}

fn attach(src: &str, stack: &mut [Frame], root: &mut Option<Node>, n: Node) {
    match stack.last_mut() {
        None => {
            if root.is_none() {
                *root = Some(n);
            }
        }
        Some(Frame::Map { entries, key, .. }) => match key.take() {
            None => {
                // Complex keys (`? [a, b]`) are named by their first line.
                let k = match &n.value {
                    Value::Scalar(s) => s.clone(),
                    _ => crate::text::preview(src.get(n.span.clone()).unwrap_or("?")),
                };
                *key = Some((k, n.span.clone()));
            }
            Some((k, ks)) => entries.push(Entry { key: k, key_span: ks, value: n, split: false }),
        },
        Some(Frame::Seq { items, .. }) => items.push(n),
        Some(Frame::Skip { .. }) => {}
    }
}
