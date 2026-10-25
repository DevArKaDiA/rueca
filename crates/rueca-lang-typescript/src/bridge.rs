//! Env var and config key reads for the code ↔ config bridge (RFC 0003
//! §7.2): `process.env.X`, `process.env["X"]`, `const { X } = process.env`,
//! `import.meta.env.X` (also destructured) and `Deno.env.get("X")` →
//! `EnvVar`; `config.get("a.b")` / `config.has("a.b")` with `config`
//! imported from the `config` package → `ConfigKey` (namespace
//! `node-config`). Names must be literals; `process` / `Deno` must not be
//! local bindings; assignment targets are writes, not reads.

use std::collections::HashSet;

use rueca_extract::text_of;
use rueca_model::RefKind;
use tree_sitter::Node;

/// Bridge facts of one node: (kind, name, receiver).
pub(crate) type Fact = (RefKind, String, Option<String>);

/// Local names bound to the `config` package (node-config).
#[derive(Debug, Default)]
pub(crate) struct EnvApi {
    config: HashSet<String>,
}

fn field<'t>(n: Node<'t>, name: &str) -> Option<Node<'t>> {
    n.child_by_field_name(name)
}

fn compact(n: Node<'_>, src: &[u8]) -> String {
    text_of(n, src).split_whitespace().collect()
}

/// Value of a plain string literal (no escapes, no substitutions).
fn string_literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    if n.kind() != "string" {
        return None;
    }
    let mut c = n.walk();
    let parts: Vec<Node<'_>> = n.named_children(&mut c).collect();
    match parts.as_slice() {
        [frag] if frag.kind() == "string_fragment" => {
            let v = text_of(*frag, src).into_owned();
            (!v.is_empty() && v.len() <= 200).then_some(v)
        }
        _ => None,
    }
}

fn first_arg<'t>(call: Node<'t>) -> Option<Node<'t>> {
    let args = field(call, "arguments")?;
    let mut c = args.walk();
    args.named_children(&mut c).next()
}

/// `process.env` / `import.meta.env` (the env object).
fn env_object(n: Node<'_>, src: &[u8]) -> Option<&'static str> {
    // Cheap reject first: receivers of long call chains are large texts.
    if n.byte_range().len() > 40 {
        return None;
    }
    match compact(n, src).as_str() {
        "process.env" => Some("process"),
        "import.meta.env" => Some("import"),
        _ => None,
    }
}

impl EnvApi {
    /// `import config from "config"`, `import * as config from "config"`,
    /// `const config = require("config")` at the top level.
    pub(crate) fn scan(root: Node<'_>, src: &[u8]) -> EnvApi {
        let mut api = EnvApi::default();
        let mut c = root.walk();
        for stmt in root.named_children(&mut c) {
            match stmt.kind() {
                "import_statement" => {
                    let from_config =
                        field(stmt, "source").and_then(|s| string_literal(s, src)).as_deref() == Some("config");
                    if !from_config {
                        continue;
                    }
                    let mut stack = vec![stmt];
                    while let Some(n) = stack.pop() {
                        match n.kind() {
                            "import_clause" => {
                                let mut cc = n.walk();
                                for ch in n.named_children(&mut cc) {
                                    if ch.kind() == "identifier" {
                                        api.config.insert(text_of(ch, src).into_owned());
                                    } else {
                                        stack.push(ch);
                                    }
                                }
                            }
                            "namespace_import" => {
                                let mut cc = n.walk();
                                for ch in n.named_children(&mut cc) {
                                    if ch.kind() == "identifier" {
                                        api.config.insert(text_of(ch, src).into_owned());
                                    }
                                }
                            }
                            _ => {
                                let mut cc = n.walk();
                                stack.extend(n.named_children(&mut cc));
                            }
                        }
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    let mut cc = stmt.walk();
                    for d in stmt.named_children(&mut cc) {
                        let (Some(name), Some(value)) = (field(d, "name"), field(d, "value")) else { continue };
                        let is_require = value.kind() == "call_expression"
                            && field(value, "function").is_some_and(|f| text_of(f, src) == "require")
                            && first_arg(value).and_then(|a| string_literal(a, src)).as_deref() == Some("config");
                        if is_require && name.kind() == "identifier" {
                            api.config.insert(text_of(name, src).into_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        api
    }

    /// Bridge facts for a `member_expression`, `subscript_expression` or
    /// `call_expression`. `is_local(name)` tells whether a root identifier
    /// is a local binding.
    pub(crate) fn facts(&self, n: Node<'_>, src: &[u8], is_local: impl Fn(&str) -> bool) -> Vec<Fact> {
        let env = |kind: RefKind, name: String| -> Vec<Fact> { vec![(kind, name, None)] };
        let root_ok = |root: &str| root == "import" || !is_local(root);
        match n.kind() {
            "member_expression" => {
                let (Some(obj), Some(prop)) = (field(n, "object"), field(n, "property")) else { return Vec::new() };
                if let Some(root) = env_object(obj, src) {
                    if prop.kind() != "property_identifier" || !root_ok(root) || is_write_target(n) {
                        return Vec::new();
                    }
                    return env(RefKind::EnvVar, text_of(prop, src).into_owned());
                }
                // `const { A, B: b } = process.env`
                if let Some(root) = env_object(n, src)
                    && root_ok(root)
                    && let Some(d) = n.parent().filter(|p| p.kind() == "variable_declarator")
                    && field(d, "value").is_some_and(|v| v.id() == n.id())
                    && let Some(pattern) = field(d, "name").filter(|p| p.kind() == "object_pattern")
                {
                    let mut out = Vec::new();
                    let mut c = pattern.walk();
                    for p in pattern.named_children(&mut c) {
                        let key = match p.kind() {
                            "shorthand_property_identifier_pattern" => Some(p),
                            "pair_pattern" => field(p, "key").filter(|k| k.kind() == "property_identifier"),
                            "object_assignment_pattern" => {
                                field(p, "left").filter(|k| k.kind() == "shorthand_property_identifier_pattern")
                            }
                            _ => None,
                        };
                        if let Some(k) = key {
                            out.push((RefKind::EnvVar, text_of(k, src).into_owned(), None));
                        }
                    }
                    return out;
                }
                Vec::new()
            }
            "subscript_expression" => {
                let (Some(obj), Some(index)) = (field(n, "object"), field(n, "index")) else { return Vec::new() };
                match (env_object(obj, src), string_literal(index, src)) {
                    (Some(root), Some(name)) if root_ok(root) && !is_write_target(n) => env(RefKind::EnvVar, name),
                    _ => Vec::new(),
                }
            }
            "call_expression" => {
                let Some(callee) = field(n, "function") else { return Vec::new() };
                let Some(arg) = first_arg(n).and_then(|a| string_literal(a, src)) else { return Vec::new() };
                if callee.byte_range().len() <= 40 && compact(callee, src) == "Deno.env.get" && !is_local("Deno") {
                    return env(RefKind::EnvVar, arg);
                }
                if callee.kind() == "member_expression"
                    && let (Some(obj), Some(prop)) = (field(callee, "object"), field(callee, "property"))
                    && obj.kind() == "identifier"
                    && self.config.contains(text_of(obj, src).as_ref())
                    && matches!(text_of(prop, src).as_ref(), "get" | "has")
                {
                    return vec![(RefKind::ConfigKey, arg, Some("node-config".into()))];
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

/// Left side of an assignment (`process.env.X = …`) or `delete` operand.
fn is_write_target(n: Node<'_>) -> bool {
    n.parent().is_some_and(|p| {
        (matches!(p.kind(), "assignment_expression" | "augmented_assignment_expression")
            && field(p, "left").is_some_and(|l| l.id() == n.id()))
            || (p.kind() == "unary_expression" && field(p, "operator").is_some_and(|o| o.kind() == "delete"))
    })
}
