//! `Package.swift` read with the Swift grammar (never evaluated): the
//! package name, its library products, its targets (name, custom `path:`,
//! test or not) and its package dependencies (by URL, path or registry
//! identity). Best effort and deterministic: only string literals count,
//! so a name built at run time (`name: prefix + "Core"`, a loop over
//! targets) is not seen.
//!
//! - `Package(name: "X", …)` → [`Manifest::name`].
//! - `.library(name: "L", targets: ["A", "B"])` → a product.
//! - `.target(…)`, `.executableTarget(…)`, `.testTarget(…)`, `.macro(…)`,
//!   `.plugin(…)`, `.systemLibrary(…)`, `.binaryTarget(…)` (also written
//!   `Target.target(…)`) → a target with its `name:` and `path:`.
//! - `.package(url: "https://github.com/apple/swift-log.git", from: …)`,
//!   `.package(path: "../Kit")`, `.package(id: "scope.name", …)`,
//!   `.package(name: "N", url: …)` → a dependency whose identity is
//!   SwiftPM's: the last path component of the URL or path, lower-cased,
//!   without `.git`; a registry identifier as written, lower-cased.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rueca_extract::tree_sitter::{Node, Parser};

use crate::types::{child_of_kind, named_children, text};

/// Target declarations a manifest may hold.
const TARGET_KINDS: &[&str] =
    &["binaryTarget", "executableTarget", "macro", "plugin", "systemLibrary", "target", "testTarget"];

/// Distinct manifests kept parsed (module names are asked once per file).
const CACHE_CAP: usize = 256;

/// One target of a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    /// Custom `path:` relative to the package directory.
    pub path: Option<String>,
    /// `.testTarget(…)`.
    pub test: bool,
}

/// What a `Package.swift` declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// `Package(name:)`.
    pub name: Option<String>,
    /// Library products: (name, targets).
    pub libraries: Vec<(String, Vec<String>)>,
    pub targets: Vec<Target>,
    /// SwiftPM identities of the package dependencies.
    pub dependencies: Vec<String>,
}

/// The SwiftPM identity of a dependency location: the last path
/// component, lower-cased, without `.git` (`https://github.com/apple/swift-log.git`
/// → `swift-log`).
pub fn package_identity(location: &str) -> String {
    let last = location.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or(location);
    last.strip_suffix(".git").unwrap_or(last).to_lowercase()
}

/// The labeled arguments of a call: (label, value node).
fn labeled_args<'t>(call: Node<'t>, source: &[u8]) -> Vec<(String, Node<'t>)> {
    let Some(args) = child_of_kind(call, "call_suffix").and_then(|s| child_of_kind(s, "value_arguments")) else {
        return Vec::new();
    };
    named_children(args)
        .into_iter()
        .filter(|a| a.kind() == "value_argument")
        .filter_map(|a| {
            Some((text(a.child_by_field_name("name")?, source).to_string(), a.child_by_field_name("value")?))
        })
        .collect()
}

/// The string of a plain literal (`"X"`).
fn literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "line_string_literal" {
        return None;
    }
    match named_children(node).as_slice() {
        [t] if t.kind() == "line_str_text" => Some(text(*t, source).to_string()),
        [] => Some(String::new()),
        _ => None,
    }
}

/// The string literals of an array literal (`["A", "B"]`).
fn literals(node: Node<'_>, source: &[u8]) -> Vec<String> {
    if node.kind() != "array_literal" {
        return Vec::new();
    }
    named_children(node).into_iter().filter_map(|e| literal(e, source)).collect()
}

/// The callee name of a manifest call: `Package`, `.target` → `target`,
/// `Target.target` → `target`.
fn callee_name<'s>(call: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let callee = call.named_child(0)?;
    let id = match callee.kind() {
        "simple_identifier" => callee,
        "prefix_expression" => callee.child_by_field_name("target")?,
        "navigation_expression" => callee.child_by_field_name("suffix")?.child_by_field_name("suffix")?,
        _ => return None,
    };
    (id.kind() == "simple_identifier").then(|| text(id, source))
}

/// The string argument `label:` of a call.
fn string_arg(args: &[(String, Node<'_>)], label: &str, source: &[u8]) -> Option<String> {
    args.iter().find(|(l, _)| l == label).and_then(|(_, v)| literal(*v, source))
}

struct Reader<'s> {
    source: &'s [u8],
    out: Manifest,
}

impl Reader<'_> {
    fn call(&mut self, call: Node<'_>) {
        let Some(name) = callee_name(call, self.source) else { return };
        let args = labeled_args(call, self.source);
        let s = self.source;
        match name {
            "Package" => self.out.name = self.out.name.take().or_else(|| string_arg(&args, "name", s)),
            "library" => {
                if let Some(n) = string_arg(&args, "name", s) {
                    let targets =
                        args.iter().find(|(l, _)| l == "targets").map(|(_, v)| literals(*v, s)).unwrap_or_default();
                    self.out.libraries.push((n, targets));
                }
            }
            "package" => self.dependency(&args),
            k if TARGET_KINDS.contains(&k) => {
                if let Some(n) = string_arg(&args, "name", s) {
                    self.out.targets.push(Target {
                        name: n,
                        path: string_arg(&args, "path", s),
                        test: k == "testTarget",
                    });
                }
            }
            _ => {}
        }
    }

    fn dependency(&mut self, args: &[(String, Node<'_>)]) {
        let s = self.source;
        let id = string_arg(args, "url", s)
            .or_else(|| string_arg(args, "path", s))
            .map(|l| package_identity(&l))
            .or_else(|| string_arg(args, "id", s).map(|i| i.to_lowercase()));
        if let Some(id) = id.filter(|i| !i.is_empty())
            && !self.out.dependencies.contains(&id)
        {
            self.out.dependencies.push(id);
        }
    }

    fn walk(&mut self, node: Node<'_>, depth: usize) {
        if depth > 256 {
            return;
        }
        if node.kind() == "call_expression" {
            self.call(node);
        }
        for c in named_children(node) {
            self.walk(c, depth + 1);
        }
    }
}

/// Reads a `Package.swift`.
pub fn parse_manifest(text: &str) -> Manifest {
    let mut parser = Parser::new();
    if parser.set_language(&crate::locals::language()).is_err() {
        return Manifest::default();
    }
    let Some(tree) = parser.parse(text, None) else { return Manifest::default() };
    let mut r = Reader { source: text.as_bytes(), out: Manifest::default() };
    r.walk(tree.root_node(), 0);
    r.out
}

/// The targets of a manifest text, parsed once per distinct text in the
/// process (module names are asked for every file).
pub fn manifest_targets(text: &str) -> Arc<Vec<Target>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<Target>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(t) = cache.lock().ok().and_then(|c| c.get(text).cloned()) {
        return t;
    }
    let targets = Arc::new(parse_manifest(text).targets);
    if let Ok(mut c) = cache.lock() {
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(text.to_string(), targets.clone());
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Alamofire",
    products: [.library(name: "Alamofire", targets: ["Alamofire"])],
    dependencies: [
        .package(url: "https://github.com/apple/swift-log.git", from: "1.0.0"),
        .package(path: "../LocalKit"),
        .package(id: "mona.LinkedList", from: "1.0.0"),
    ],
    targets: [
        .target(name: "Alamofire", path: "Source", exclude: ["Info.plist"]),
        .target(name: "Helpers", dependencies: [.product(name: "Logging", package: "swift-log")]),
        .testTarget(name: "AlamofireTests", dependencies: ["Alamofire"], path: "Tests"),
    ]
)
"#;

    #[test]
    fn reads_the_manifest() {
        let m = parse_manifest(MANIFEST);
        assert_eq!(m.name.as_deref(), Some("Alamofire"));
        assert_eq!(m.libraries, [("Alamofire".to_string(), vec!["Alamofire".to_string()])]);
        let names: Vec<&str> = m.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Alamofire", "Helpers", "AlamofireTests"]);
        assert_eq!(m.targets[0].path.as_deref(), Some("Source"));
        assert!(m.targets[2].test);
        assert_eq!(m.dependencies, ["swift-log", "localkit", "mona.linkedlist"]);
    }

    #[test]
    fn identities() {
        assert_eq!(package_identity("https://github.com/apple/swift-argument-parser.git"), "swift-argument-parser");
        assert_eq!(package_identity("git@github.com:Alamofire/Alamofire.git"), "alamofire");
        assert_eq!(package_identity("../Kit/"), "kit");
    }
}
