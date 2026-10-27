//! Env var reads for the code ↔ config bridge (RFC 0003 §7.2): `EnvVar`
//! references for `os.environ["X"]`, `os.environ.get("X")`,
//! `os.getenv("X")`, and `environ[...]` / `environ.get(...)` / `getenv(...)`
//! after `from os import environ, getenv` (aliases followed). The name must
//! be a plain string literal; the API is recognized only with the file's
//! own `import os` / `from os import …` as evidence, never by name alone.
//! Writes (`os.environ["X"] = v`) are not reads.

use std::collections::HashSet;

use rueca_extract::text_of;
use tree_sitter::Node;

/// Names bound to `os`, `os.environ` and `os.getenv` by the file's imports.
#[derive(Debug, Default)]
pub(crate) struct EnvApi {
    os: HashSet<String>,
    environ: HashSet<String>,
    getenv: HashSet<String>,
}

impl EnvApi {
    /// Scans every import statement of the file (any depth up to 64).
    pub(crate) fn scan(root: Node<'_>, src: &[u8]) -> EnvApi {
        let mut api = EnvApi::default();
        let mut stack = vec![(root, 0usize)];
        while let Some((n, depth)) = stack.pop() {
            match n.kind() {
                "import_statement" => api.scan_import(n, src),
                "import_from_statement" => api.scan_import_from(n, src),
                _ if depth < 64 => {
                    let mut c = n.walk();
                    stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
                }
                _ => {}
            }
        }
        api
    }

    /// `import os` / `import os as o`.
    fn scan_import(&mut self, n: Node<'_>, src: &[u8]) {
        let mut c = n.walk();
        for name in n.children_by_field_name("name", &mut c) {
            match name.kind() {
                "dotted_name" if text_of(name, src) == "os" => {
                    self.os.insert("os".into());
                }
                "aliased_import" => {
                    if let Some((module, alias)) = aliased_import(name, src)
                        && module == "os"
                    {
                        self.os.insert(alias);
                    }
                }
                _ => {}
            }
        }
    }

    /// `from os import environ, getenv as g`.
    fn scan_import_from(&mut self, n: Node<'_>, src: &[u8]) {
        let module = n.child_by_field_name("module_name").map(|m| text_of(m, src));
        if module.as_deref() != Some("os") {
            return;
        }
        let mut c = n.walk();
        for name in n.children_by_field_name("name", &mut c) {
            let Some((orig, local)) = imported_name(name, src) else { continue };
            match orig.as_str() {
                "environ" => self.environ.insert(local),
                "getenv" => self.getenv.insert(local),
                _ => false,
            };
        }
    }

    fn is_empty(&self) -> bool {
        self.os.is_empty() && self.environ.is_empty() && self.getenv.is_empty()
    }

    /// Whether `n` is `os.environ` (or a `from os import environ` alias).
    fn is_environ(&self, n: Node<'_>, src: &[u8]) -> bool {
        match n.kind() {
            "identifier" => self.environ.contains(text_of(n, src).as_ref()),
            "attribute" => {
                let obj = n.child_by_field_name("object");
                let attr = n.child_by_field_name("attribute");
                matches!((obj, attr), (Some(o), Some(a))
                    if o.kind() == "identifier"
                        && self.os.contains(text_of(o, src).as_ref())
                        && text_of(a, src) == "environ")
            }
            _ => false,
        }
    }

    /// The variable read by a `call` node (`os.getenv("X")`,
    /// `os.environ.get("X")`, `getenv("X")`).
    pub(crate) fn call(&self, call: Node<'_>, src: &[u8]) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let func = call.child_by_field_name("function")?;
        let matched = match func.kind() {
            "identifier" => self.getenv.contains(text_of(func, src).as_ref()),
            "attribute" => {
                let obj = func.child_by_field_name("object")?;
                let attr = text_of(func.child_by_field_name("attribute")?, src);
                (attr == "getenv" && obj.kind() == "identifier" && self.os.contains(text_of(obj, src).as_ref()))
                    || (attr == "get" && self.is_environ(obj, src))
            }
            _ => false,
        };
        if !matched {
            return None;
        }
        let args = call.child_by_field_name("arguments")?;
        let mut c = args.walk();
        let first = args.named_children(&mut c).next()?;
        string_literal(first, src)
    }

    /// The variable read by a `subscript` node (`os.environ["X"]`), unless
    /// it is an assignment or `del` target.
    pub(crate) fn subscript(&self, node: Node<'_>, src: &[u8]) -> Option<String> {
        if self.is_empty() || !self.is_environ(node.child_by_field_name("value")?, src) {
            return None;
        }
        if let Some(p) = node.parent() {
            let target = matches!(p.kind(), "assignment" | "augmented_assignment")
                && p.child_by_field_name("left").is_some_and(|l| l.id() == node.id());
            if target || p.kind() == "delete_statement" {
                return None;
            }
        }
        string_literal(node.child_by_field_name("subscript")?, src)
    }
}

/// `(name, alias)` of an `aliased_import` node, when both are present.
fn aliased_import(node: Node<'_>, src: &[u8]) -> Option<(String, String)> {
    let name = node.child_by_field_name("name")?;
    let alias = node.child_by_field_name("alias")?;
    Some((text_of(name, src).into_owned(), text_of(alias, src).into_owned()))
}

/// `(imported name, local name)` of a `from … import` name node.
fn imported_name(node: Node<'_>, src: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "dotted_name" => Some((text_of(node, src).into_owned(), text_of(node, src).into_owned())),
        "aliased_import" => aliased_import(node, src),
        _ => None,
    }
}

/// Value of a plain string literal (no f-string, bytes or interpolation).
fn string_literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    if n.kind() != "string" {
        return None;
    }
    let mut c = n.walk();
    let mut value = String::new();
    for ch in n.named_children(&mut c) {
        match ch.kind() {
            "string_start" => {
                let start = text_of(ch, src);
                if start.chars().any(|c| matches!(c, 'f' | 'F' | 'b' | 'B')) {
                    return None;
                }
            }
            "string_content" => value.push_str(&text_of(ch, src)),
            "string_end" => {}
            _ => return None,
        }
    }
    let ok = !value.is_empty()
        && value.len() <= 200
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    ok.then_some(value)
}
