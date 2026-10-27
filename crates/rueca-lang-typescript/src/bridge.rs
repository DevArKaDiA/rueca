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
                "import_statement" => api.config_import(stmt, src),
                "lexical_declaration" | "variable_declaration" => {
                    let mut cc = stmt.walk();
                    api.config.extend(stmt.named_children(&mut cc).filter_map(|d| config_require(d, src)));
                }
                _ => {}
            }
        }
        api
    }

    /// The local names an `import ... from "config"` binds.
    fn config_import(&mut self, stmt: Node<'_>, src: &[u8]) {
        let from_config = field(stmt, "source").and_then(|s| string_literal(s, src)).as_deref() == Some("config");
        if !from_config {
            return;
        }
        let mut stack = vec![stmt];
        while let Some(n) = stack.pop() {
            let mut cc = n.walk();
            match n.kind() {
                "import_clause" => {
                    for ch in n.named_children(&mut cc) {
                        if ch.kind() == "identifier" {
                            self.config.insert(text_of(ch, src).into_owned());
                        } else {
                            stack.push(ch);
                        }
                    }
                }
                "namespace_import" => {
                    let names = n.named_children(&mut cc).filter(|ch| ch.kind() == "identifier");
                    self.config.extend(names.map(|ch| text_of(ch, src).into_owned()));
                }
                _ => stack.extend(n.named_children(&mut cc)),
            }
        }
    }

    /// Bridge facts for a `member_expression`, `subscript_expression` or
    /// `call_expression`. `is_local(name)` tells whether a root identifier
    /// is a local binding.
    pub(crate) fn facts(&self, n: Node<'_>, src: &[u8], is_local: impl Fn(&str) -> bool) -> Vec<Fact> {
        match n.kind() {
            "member_expression" => member_facts(n, src, &is_local),
            "subscript_expression" => {
                let (Some(obj), Some(index)) = (field(n, "object"), field(n, "index")) else { return Vec::new() };
                match (env_object(obj, src), string_literal(index, src)) {
                    (Some(root), Some(name)) if root_ok(root, &is_local) && !is_write_target(n) => env_var(name),
                    _ => Vec::new(),
                }
            }
            "call_expression" => self.call_facts(n, src, &is_local),
            _ => Vec::new(),
        }
    }

    /// `Deno.env.get("X")` and `config.get("a.b")` / `config.has("a.b")`.
    fn call_facts(&self, n: Node<'_>, src: &[u8], is_local: &impl Fn(&str) -> bool) -> Vec<Fact> {
        let Some(callee) = field(n, "function") else { return Vec::new() };
        let Some(arg) = first_arg(n).and_then(|a| string_literal(a, src)) else { return Vec::new() };
        if callee.byte_range().len() <= 40 && compact(callee, src) == "Deno.env.get" && !is_local("Deno") {
            return env_var(arg);
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
}

/// `x` of a declarator `x = require("config")`.
fn config_require(d: Node<'_>, src: &[u8]) -> Option<String> {
    let (name, value) = (field(d, "name")?, field(d, "value")?);
    let is_require = value.kind() == "call_expression"
        && field(value, "function").is_some_and(|f| text_of(f, src) == "require")
        && first_arg(value).and_then(|a| string_literal(a, src)).as_deref() == Some("config");
    (is_require && name.kind() == "identifier").then(|| text_of(name, src).into_owned())
}

fn env_var(name: String) -> Vec<Fact> {
    vec![(RefKind::EnvVar, name, None)]
}

/// Whether an env object root is usable: `import.meta` always, `process`
/// unless it is a local binding.
fn root_ok(root: &str, is_local: &impl Fn(&str) -> bool) -> bool {
    root == "import" || !is_local(root)
}

/// `process.env.X` (a read), or `process.env` itself when destructured.
fn member_facts(n: Node<'_>, src: &[u8], is_local: &impl Fn(&str) -> bool) -> Vec<Fact> {
    let (Some(obj), Some(prop)) = (field(n, "object"), field(n, "property")) else { return Vec::new() };
    if let Some(root) = env_object(obj, src) {
        if prop.kind() != "property_identifier" || !root_ok(root, is_local) || is_write_target(n) {
            return Vec::new();
        }
        return env_var(text_of(prop, src).into_owned());
    }
    // `const { A, B: b } = process.env`
    let Some(pattern) = env_destructuring(n, src, is_local) else { return Vec::new() };
    let mut c = pattern.walk();
    pattern
        .named_children(&mut c)
        .filter_map(destructured_key)
        .map(|k| (RefKind::EnvVar, text_of(k, src).into_owned(), None))
        .collect()
}

/// The object pattern of `const { ... } = process.env` when `n` is the env
/// object being destructured.
fn env_destructuring<'t>(n: Node<'t>, src: &[u8], is_local: &impl Fn(&str) -> bool) -> Option<Node<'t>> {
    let root = env_object(n, src)?;
    if !root_ok(root, is_local) {
        return None;
    }
    let d = n.parent().filter(|p| p.kind() == "variable_declarator")?;
    if !field(d, "value").is_some_and(|v| v.id() == n.id()) {
        return None;
    }
    field(d, "name").filter(|p| p.kind() == "object_pattern")
}

/// The env var key of one property of an object pattern (`A`, `B: b`,
/// `C = "x"`).
fn destructured_key(p: Node<'_>) -> Option<Node<'_>> {
    match p.kind() {
        "shorthand_property_identifier_pattern" => Some(p),
        "pair_pattern" => field(p, "key").filter(|k| k.kind() == "property_identifier"),
        "object_assignment_pattern" => field(p, "left").filter(|k| k.kind() == "shorthand_property_identifier_pattern"),
        _ => None,
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
