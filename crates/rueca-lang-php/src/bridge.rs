//! The code ↔ config bridge for PHP (RFC 0003 §7.2, §7.3):
//!
//! - `EnvVar` reads: the builtin `getenv('X')`, the superglobals
//!   `$_ENV['X']` and `$_SERVER['X']`, and Laravel's `env('X')` helper.
//! - `ConfigKey` reads (namespace `laravel`): `config('app.name')` (a string
//!   first argument; `config([...])` sets values and is skipped) and
//!   `Config::get('app.name')` through the `Config` facade.
//! - Laravel config files are PHP, not data: for `config/<file>.php` the
//!   array the file returns becomes `Key` definitions by key path (`name`,
//!   `providers.cache.driver`), so `config('app.name')` resolves to key
//!   `name` of `config/app.php`; `env('X')` calls inside an entry are reads
//!   by that key (`Key` → `READS` → `$env:X`).
//!
//! Names must be string literals. Function names are matched unqualified
//! (`getenv`, `\getenv`): PHP falls back to the global function, and a
//! project that defines its own `env()` / `config()` in a namespace would be
//! misread (documented limitation).

use rueca_extract::{Budget, ExtractError, span_of, text_of};
use rueca_model::{Definition, FileFacts, Label, LocalDefId, RefKind};
use tree_sitter::Node;

/// A bridge fact: (kind, name, receiver).
pub(crate) type Fact = (RefKind, String, Option<String>);

fn named<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

/// Value of a literal string without interpolation.
fn string_literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    if !matches!(n.kind(), "string" | "encapsed_string") {
        return None;
    }
    match named(n).as_slice() {
        [c] if c.kind() == "string_content" => {
            let v = text_of(*c, src).into_owned();
            (!v.is_empty() && v.len() <= 200).then_some(v)
        }
        _ => None,
    }
}

/// First argument's value of a call (skipping a named-argument label).
fn first_arg<'t>(call: Node<'t>) -> Option<Node<'t>> {
    let args = call.child_by_field_name("arguments")?;
    let arg = named(args).into_iter().find(|a| a.kind() == "argument")?;
    let label = arg.child_by_field_name("name");
    named(arg).into_iter().find(|c| Some(*c) != label && c.kind() != "comment")
}

/// `getenv('X')`, `env('X')`, `config('a.b')` for a function call whose
/// callee is written `written`.
pub(crate) fn call(written: &str, call: Node<'_>, src: &[u8]) -> Option<Fact> {
    let bare = written.strip_prefix('\\').unwrap_or(written);
    let kind = match bare {
        "getenv" | "env" => RefKind::EnvVar,
        "config" => RefKind::ConfigKey,
        _ => return None,
    };
    let name = string_literal(first_arg(call)?, src)?;
    Some(match kind {
        RefKind::EnvVar => (kind, name, None),
        _ => (kind, name, Some("laravel".into())),
    })
}

/// `Config::get('a.b')` (the Laravel facade, imported or global alias).
pub(crate) fn scoped_call(node: Node<'_>, src: &[u8]) -> Option<Fact> {
    let scope = node.child_by_field_name("scope")?;
    if scope.byte_range().len() > 200 {
        return None;
    }
    let scope = text_of(scope, src);
    let class = scope.rsplit('\\').next().unwrap_or(&scope);
    let method = text_of(node.child_by_field_name("name")?, src);
    if class != "Config" || method != "get" {
        return None;
    }
    let name = string_literal(first_arg(node)?, src)?;
    Some((RefKind::ConfigKey, name, Some("laravel".into())))
}

/// `$_ENV['X']` / `$_SERVER['X']`.
pub(crate) fn subscript(node: Node<'_>, src: &[u8]) -> Option<String> {
    let kids = named(node);
    let (base, index) = (kids.first()?, kids.get(1)?);
    if base.kind() != "variable_name" || !matches!(text_of(*base, src).as_ref(), "$_ENV" | "$_SERVER") {
        return None;
    }
    let target = node.parent().is_some_and(|p| {
        matches!(p.kind(), "assignment_expression" | "reference_assignment_expression")
            && p.child_by_field_name("left").is_some_and(|l| l.id() == node.id())
    });
    if target {
        return None;
    }
    string_literal(*index, src)
}

/// Whether `path` is a Laravel config file (`config/<name>.php`).
pub(crate) fn is_config_file(path: &str) -> bool {
    let mut parts = path.rsplit('/');
    let name = parts.next().unwrap_or("");
    name.ends_with(".php") && parts.next() == Some("config")
}

const MAX_KEYS: usize = 5_000;
const MAX_DEPTH: usize = 32;

/// Adds `Key` definitions for the array returned by a Laravel config file,
/// and makes the bridge reads inside an entry belong to that key.
pub(crate) fn config_keys(source: &[u8], budget: &Budget, facts: &mut FileFacts) -> Result<(), ExtractError> {
    let language: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let Some(array) = named(root)
        .into_iter()
        .filter(|s| s.kind() == "return_statement")
        .find_map(|r| named(r).into_iter().find(|e| e.kind() == "array_creation_expression"))
    else {
        return Ok(());
    };
    let first = facts.definitions.len();
    entries(array, None, "", 0, source, facts);
    // Bridge reads inside an entry are reads by the innermost key.
    let keys: Vec<(LocalDefId, rueca_model::Span)> =
        (first..facts.definitions.len()).map(|i| (i as LocalDefId, facts.definitions[i].span)).collect();
    for r in &mut facts.references {
        if r.enclosing.is_none() && matches!(r.kind, RefKind::EnvVar | RefKind::ConfigKey) {
            r.enclosing = keys
                .iter()
                .filter(|(_, s)| s.contains(&r.span))
                .min_by_key(|(_, s)| s.end_byte - s.start_byte)
                .map(|(i, _)| *i);
        }
    }
    Ok(())
}

fn comment_doc(element: Node<'_>, src: &[u8]) -> Option<String> {
    let prev = element.prev_named_sibling().filter(|p| p.kind() == "comment")?;
    // Only a comment on the line directly above.
    if prev.end_position().row + 1 != element.start_position().row {
        return None;
    }
    let t = text_of(prev, src);
    let t = t.trim().trim_start_matches("/**").trim_start_matches("/*").trim_end_matches("*/");
    let t = t.trim_start_matches("//").trim_start_matches('#').trim();
    (!t.is_empty()).then(|| t.chars().take(500).collect())
}

/// The value shown in a key's signature: `[…]` for nested arrays, `•••` for
/// secrets, else the first line (cut at 80 characters).
fn value_preview(value: Node<'_>, key: &str, src: &[u8]) -> String {
    if value.kind() == "array_creation_expression" {
        return "[…]".to_string();
    }
    if is_secret(key) {
        return "•••".to_string();
    }
    let value_text = text_of(value, src);
    let line = value_text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(80).collect::<String>())
    } else {
        line.to_string()
    }
}

fn entries(array: Node<'_>, parent: Option<LocalDefId>, prefix: &str, depth: usize, src: &[u8], facts: &mut FileFacts) {
    if depth >= MAX_DEPTH {
        return;
    }
    for el in named(array).into_iter().filter(|e| e.kind() == "array_element_initializer") {
        if facts.definitions.len() >= MAX_KEYS {
            return;
        }
        let parts: Vec<Node<'_>> = named(el).into_iter().filter(|c| c.kind() != "comment").collect();
        let [key, value] = parts.as_slice() else { continue };
        let Some(k) = string_literal(*key, src) else { continue };
        let qn = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        let preview = value_preview(*value, &k, src);
        let id = facts.definitions.len() as LocalDefId;
        facts.definitions.push(Definition {
            label: Label::Key,
            name: k.clone(),
            qualified_name: qn.clone(),
            parent,
            span: span_of(el),
            name_span: span_of(*key),
            signature: Some(format!("{k}: {preview}")),
            doc: comment_doc(el, src),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        if value.kind() == "array_creation_expression" {
            entries(*value, Some(id), &qn, depth + 1, src, facts);
        }
    }
}

/// RFC 0003 §4.6 secret-looking key names.
fn is_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "pass",
        "secret",
        "token",
        "credential",
        "apikey",
        "api_key",
        "api-key",
        "privatekey",
        "private_key",
        "private-key",
    ]
    .iter()
    .any(|p| n.contains(p))
}
