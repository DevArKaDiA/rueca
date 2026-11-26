//! bats test blocks. `@test "name" { … }` is not shell syntax: the bash
//! grammar reads the head as a command `@test` whose last argument is `{`,
//! the body as statements of the file and the closing brace as a command
//! `}`. A block is the head and the statements up to that `}` (or up to the
//! next head, or the end of the file).

use rueca_extract::tree_sitter::Node;

use crate::locals::text;

/// One `@test` block: indexes into the program's named children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBlock {
    /// The `@test "name" {` command.
    pub(crate) head: usize,
    /// The last statement of the block (the closing `}` when there is one).
    pub(crate) last: usize,
    pub(crate) name: String,
}

fn command_name<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    (node.kind() == "command").then(|| node.child_by_field_name("name").map(|n| text(n, source))).flatten()
}

/// The test name of an `@test "name" {` head.
fn head_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if command_name(node, source)? != "@test" {
        return None;
    }
    let mut cursor = node.walk();
    let args: Vec<Node<'_>> = node.children_by_field_name("argument", &mut cursor).collect();
    let (first, last) = (args.first()?, args.last()?);
    if text(*last, source) != "{" || args.len() < 2 {
        return None;
    }
    let raw = text(*first, source);
    Some(raw.trim_matches(|c| c == '"' || c == '\'').to_string())
}

/// Whether `node` is a block's closing `}`.
pub(crate) fn is_block_end(node: Node<'_>, source: &[u8]) -> bool {
    command_name(node, source) == Some("}")
}

/// The `@test` blocks among the program's named children.
pub(crate) fn test_blocks(root: Node<'_>, source: &[u8]) -> Vec<TestBlock> {
    let mut cursor = root.walk();
    let children: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < children.len() {
        let Some(name) = head_name(children[i], source) else {
            i += 1;
            continue;
        };
        let mut last = i;
        for (j, c) in children.iter().enumerate().skip(i + 1) {
            if head_name(*c, source).is_some() {
                break;
            }
            last = j;
            if is_block_end(*c, source) {
                break;
            }
        }
        out.push(TestBlock { head: i, last, name });
        i = last + 1;
    }
    out
}
