//! `use`, `extern crate` and `mod` declarations → [`Import`] facts (the
//! encoding is in the crate docs, *Imports*).
//!
//! A `use` tree is flattened into one import per leaf. The path before the
//! leaf is the module the leaf is looked up in, normalized at the
//! declaration site: `self::` and `super::` that stay inside the file
//! (inline modules) become file-relative paths; `super::` beyond the file
//! counts levels above the file's module.

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::{Import, ImportedName};

use crate::extract::{Extractor, mod_is_cfg_test, mod_path_attr, named_children};
use crate::locals::text;

/// One leaf of a `use` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Leaf {
    /// `a::b` (binds `b`, or the alias).
    Name(Option<String>),
    /// `a::b::{self}` (binds `b`, or the alias).
    SelfModule(Option<String>),
    /// `a::b::*`.
    Glob,
}

/// A path relative to a site: the module of the file (`relative_level` ≥ 1)
/// or a crate / uniform path (`relative_level` 0), as the crate docs say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModulePath {
    pub(crate) level: u32,
    pub(crate) module: String,
}

/// Normalizes path segments written inside the inline modules `mods` of a
/// file (see the module docs). `None` for an empty path.
pub(crate) fn normalize(segs: &[String], mods: &[String]) -> Option<ModulePath> {
    let first = segs.first()?;
    let join = |s: &[String]| s.join("::");
    let path = match first.as_str() {
        "crate" | "$crate" => ModulePath { level: 0, module: format!("crate{}", tail(&segs[1..])) },
        "" => ModulePath { level: 0, module: format!("::{}", join(&segs[1..])) },
        "self" => {
            let mut s: Vec<String> = mods.to_vec();
            s.extend_from_slice(&segs[1..]);
            ModulePath { level: 1, module: join(&s) }
        }
        "super" => {
            let k = segs.iter().take_while(|s| *s == "super").count();
            let rest = &segs[k..];
            if k <= mods.len() {
                let mut s: Vec<String> = mods[..mods.len() - k].to_vec();
                s.extend_from_slice(rest);
                ModulePath { level: 1, module: join(&s) }
            } else {
                ModulePath { level: 1 + (k - mods.len()) as u32, module: join(rest) }
            }
        }
        _ => ModulePath { level: 0, module: join(segs) },
    };
    Some(path)
}

fn tail(rest: &[String]) -> String {
    if rest.is_empty() { String::new() } else { format!("::{}", rest.join("::")) }
}

/// `module_alias` of the marker import that follows a `#[cfg(test)] mod x;`
/// (crate docs, *Imports*).
const TEST_MARKER: &str = "$test";

/// The marker of a `use` declared inside inline modules `mods` (`$in:a.b`),
/// kept in the import's `module_alias` (unused by `use` imports otherwise).
fn inside(mods: &[String]) -> Option<String> {
    (!mods.is_empty()).then(|| format!("$in:{}", mods.join(".")))
}

fn import(path: ModulePath, names: Vec<ImportedName>, alias: Option<String>, reexport: bool, node: Node<'_>) -> Import {
    Import {
        module: path.module,
        relative_level: path.level,
        names,
        module_alias: alias,
        type_only: false,
        reexport,
        span: span_of(node),
    }
}

/// Path segments of a path node in a `use` tree (`crate`, `self`, `super`,
/// identifiers; `""` first for `::a`).
fn segments(node: Node<'_>, src: &[u8]) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "scoped_identifier" => {
                if let Some(name) = n.child_by_field_name("name") {
                    segs.push(text(name, src).to_string());
                }
                cur = n.child_by_field_name("path");
                if cur.is_none() {
                    segs.push(String::new());
                }
            }
            _ => {
                segs.push(text(n, src).to_string());
                cur = None;
            }
        }
    }
    segs.reverse();
    segs
}

/// Flattens a `use` tree into (path, leaf) pairs under `prefix`.
fn flatten(node: Node<'_>, src: &[u8], prefix: &[String], out: &mut Vec<(Vec<String>, Leaf)>) {
    let with = |segs: Vec<String>| {
        let mut p = prefix.to_vec();
        p.extend(segs);
        p
    };
    match node.kind() {
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (node.child_by_field_name("path"), node.child_by_field_name("alias"))
            else {
                return;
            };
            out.push(leaf(with(segments(path, src)), Some(text(alias, src).to_string())));
        }
        "use_wildcard" => {
            let segs = named_children(node).first().map(|p| segments(*p, src)).unwrap_or_default();
            out.push((with(segs), Leaf::Glob));
        }
        "scoped_use_list" => {
            let base =
                node.child_by_field_name("path").map(|p| segments(p, src)).unwrap_or_else(|| vec![String::new()]);
            let p = with(base);
            if let Some(list) = node.child_by_field_name("list") {
                flatten(list, src, &p, out);
            }
        }
        "use_list" => {
            for c in named_children(node) {
                flatten(c, src, prefix, out);
            }
        }
        _ => out.push(leaf(with(segments(node, src)), None)),
    }
}

/// A `Name` leaf, or `SelfModule` for a path ending in `self`.
fn leaf(mut segs: Vec<String>, alias: Option<String>) -> (Vec<String>, Leaf) {
    if segs.len() > 1 && segs.last().is_some_and(|s| s == "self") {
        segs.pop();
        return (segs, Leaf::SelfModule(alias));
    }
    (segs, Leaf::Name(alias))
}

impl Extractor<'_> {
    /// The imports of a `use` declaration written inside inline modules `mods`.
    pub(crate) fn use_declaration(&mut self, node: Node<'_>, mods: &[String]) {
        let Some(arg) = node.child_by_field_name("argument") else { return };
        let reexport = named_children(node).iter().any(|c| c.kind() == "visibility_modifier");
        let mut leaves = Vec::new();
        flatten(arg, self.src, &[], &mut leaves);
        for (segs, leaf) in leaves {
            self.use_leaf(node, mods, segs, leaf, reexport);
        }
    }

    fn use_leaf(&mut self, node: Node<'_>, mods: &[String], segs: Vec<String>, leaf: Leaf, reexport: bool) {
        self.note_env_evidence(&segs, &leaf);
        if leaf == Leaf::Glob {
            let Some(path) = normalize(&segs, mods) else { return };
            let names = vec![ImportedName { name: "*".into(), alias: None }];
            self.imports.push(import(path, names, inside(mods), reexport, node));
            return;
        }
        let Some((name, module)) = segs.split_last() else { return };
        let alias = match leaf {
            Leaf::Name(a) | Leaf::SelfModule(a) => a,
            Leaf::Glob => None,
        };
        if name == "self" || alias.as_deref() == Some("_") {
            return;
        }
        let bound = alias.clone().unwrap_or_else(|| name.clone());
        self.file.bound.insert(bound);
        let path = if module.is_empty() {
            ModulePath { level: 0, module: String::new() }
        } else {
            match normalize(module, mods) {
                Some(p) => p,
                None => return,
            }
        };
        let names = vec![ImportedName { name: name.clone(), alias }];
        self.imports.push(import(path, names, inside(mods), reexport, node));
    }

    /// Names bound to `std::env` and to `std::env::var` / `var_os`.
    fn note_env_evidence(&mut self, segs: &[String], leaf: &Leaf) {
        let s: Vec<&str> = segs.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
        let alias = match leaf {
            Leaf::Name(a) | Leaf::SelfModule(a) => a.clone(),
            Leaf::Glob => {
                if matches!(s.as_slice(), ["std" | "core", "env"]) {
                    self.file.env_fns.extend(["var".to_string(), "var_os".to_string()]);
                }
                return;
            }
        };
        match s.as_slice() {
            ["std" | "core", "env"] => {
                self.file.env_mods.insert(alias.unwrap_or_else(|| "env".into()));
            }
            ["std" | "core", "env", f @ ("var" | "var_os")] => {
                self.file.env_fns.insert(alias.unwrap_or_else(|| (*f).to_string()));
            }
            _ => {}
        }
    }

    /// `extern crate foo [as bar];` binds the crate.
    pub(crate) fn extern_crate(&mut self, node: Node<'_>) {
        let Some(name) = node.child_by_field_name("name") else { return };
        let name = text(name, self.src).to_string();
        if name == "self" {
            return;
        }
        let alias = node.child_by_field_name("alias").map(|a| text(a, self.src).to_string()).unwrap_or(name.clone());
        self.file.bound.insert(alias.clone());
        let path = ModulePath { level: 0, module: format!("::{name}") };
        self.imports.push(import(path, Vec::new(), Some(alias), false, node));
    }

    /// `mod x;` declared inside inline modules `mods`: an import of the
    /// module file binding `x` (`#[path = "…"]` honoured).
    pub(crate) fn mod_declaration(&mut self, node: Node<'_>, mods: &[String]) {
        let Some(name) = node.child_by_field_name("name") else { return };
        let name = text(name, self.src).to_string();
        self.file.bound.insert(name.clone());
        let module = match mod_path_attr(node, self.src) {
            Some(p) => format!("\"{p}\""),
            None => {
                let mut s = mods.to_vec();
                s.push(name.clone());
                s.join("::")
            }
        };
        let path = ModulePath { level: 1, module };
        if mod_is_cfg_test(node, self.src) {
            // The resolver marks the module file as test code.
            let mut marker = import(path.clone(), Vec::new(), Some(TEST_MARKER.to_string()), false, node);
            marker.type_only = true;
            self.imports.push(import(path, Vec::new(), Some(name), false, node));
            self.imports.push(marker);
            return;
        }
        self.imports.push(import(path, Vec::new(), Some(name), false, node));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(p: &str) -> Vec<String> {
        p.split("::").map(str::to_string).collect()
    }

    #[test]
    fn normalized_paths() {
        let mods = vec!["tests".to_string()];
        let n = |p: &str, m: &[String]| normalize(&segs(p), m).map(|x| (x.level, x.module));
        assert_eq!(n("crate::a::b", &[]), Some((0, "crate::a::b".into())));
        assert_eq!(n("crate", &[]), Some((0, "crate".into())));
        assert_eq!(n("self::a", &[]), Some((1, "a".into())));
        assert_eq!(n("self::a", &mods), Some((1, "tests::a".into())));
        assert_eq!(n("super::a", &[]), Some((2, "a".into())));
        assert_eq!(n("super", &mods), Some((1, String::new())));
        assert_eq!(n("super::super::x", &mods), Some((2, "x".into())));
        assert_eq!(n("serde::de", &[]), Some((0, "serde::de".into())));
        assert_eq!(normalize(&["".into(), "serde".into()], &[]).map(|x| x.module), Some("::serde".into()));
    }
}
