//! Swift additions: imports bound by module name
//! (`LanguagePack::namespace_imports`), extension members across modules,
//! overloads chosen by argument labels, and call-result receivers.
//!
//! - **Imports by module.** `import Foo` binds the package (SwiftPM target)
//!   named `Foo` of the Swift family, represented by its first file; the
//!   `IMPORTS` edge goes to the directory holding most of its files. The
//!   import names `*`: every exported top-level name of the target is bound.
//! - **Extension members.** A top-level `T.m` (a member of `extension T`)
//!   is a member of type `T` of its own module (the package hook). When
//!   the module does not declare `T`, the type is looked up in the modules
//!   the file imports, then as the only Swift type of that name in the
//!   snapshot; a nested `A.B.m` walks `A` then `B`. A call on a receiver
//!   whose type is outside the snapshot (`"s".trimmed()`, `$type:String`)
//!   or misses the member finds the extension member `m` of a type with
//!   that name (`type_resolved` 0.85, unique only).
//! - **Arity** of a Swift callable comes from its signature: parameters with
//!   a default value are optional, a variadic (`T...`) makes it open-ended.
//! - **Labels.** A call's name carries its argument labels
//!   (`draw$args:at,_`; `_` unlabeled, `?` a trailing closure). Among the
//!   overloads its arity admits, those whose parameter list accepts the
//!   labels in order (a defaulted parameter may be skipped, a variadic one
//!   takes following unlabeled arguments, a trailing closure fills any
//!   parameter) remain; when none does, the arity choice stands.
//! - **Call results** (`$ret:<receiver>#<name>`, the Rust pack's receiver
//!   form): the callee is resolved like a call and the return type its
//!   Swift signature declares (`-> T`, optionals and `throws` seen
//!   through; `Self` is the callee's type) is resolved in the callee's
//!   file; an initializer returns its type.

use std::cell::OnceCell;
use std::collections::HashMap;

use rueca_model::{Definition, FileId, Label, Language, LanguageFamily, NodeId};

use crate::hooks::TypeLookup;
use crate::{Outcome, RefItem, Resolver, Want};
use rueca_extract::TYPE_RECEIVER_PREFIX;
use rueca_model::Strategy;

/// Tables built on first use.
#[derive(Default)]
pub(crate) struct SwiftTables {
    /// Extension members by (extended type's last segment, member name).
    extensions: OnceCell<HashMap<(String, String), Vec<NodeId>>>,
}

/// One parameter of a Swift signature: external label, defaulted, variadic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Param {
    label: String,
    default: bool,
    variadic: bool,
    /// May take a closure: a function type, or a named type that is not a
    /// standard value type (a typealias of a closure, a generic parameter).
    closure: bool,
}

/// Standard value types a trailing closure can never fill.
const VALUE_TYPES: &[&str] = &[
    "Bool",
    "Character",
    "Data",
    "Date",
    "Double",
    "Float",
    "Int",
    "Int64",
    "String",
    "Substring",
    "UInt",
    "URL",
    "UUID",
];

/// Whether a parameter type may take a closure.
fn takes_closure(ty: &str) -> bool {
    let t = ty.trim().trim_start_matches("inout ").trim_start_matches('@');
    if t.contains("->") {
        return true;
    }
    let bare = t.trim_end_matches(['?', '!']);
    !(bare.is_empty() || bare.starts_with('[') || bare.starts_with('(') || VALUE_TYPES.contains(&bare))
}

/// Splits a list on its top-level commas.
fn split_top(params: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    let mut in_str = false;
    for (i, c) in params.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '(' | '<' | '[' | '{' if !in_str => depth += 1,
            ')' | ']' | '}' if !in_str => depth -= 1,
            '>' if !in_str && !params[..i].ends_with('-') => depth -= 1,
            ',' if depth == 0 && !in_str => {
                out.push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out.into_iter().map(str::trim).filter(|p| !p.is_empty()).collect()
}

/// The byte index of the top-level `:` (or `=`) in a parameter.
fn top_level(p: &str, want: u8) -> Option<usize> {
    let mut depth = 0i32;
    let b = p.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'<' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i == 0 || b[i - 1] != b'-' => depth -= 1,
            c if c == want && depth == 0 => {
                let eq_op = want == b'=' && (b.get(i + 1) == Some(&b'=') || (i > 0 && b"=!<>".contains(&b[i - 1])));
                if !eq_op {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// One parameter text (`at x: Int`, `_ y: Int = 0`, `xs: Int...`).
fn parse_param(p: &str) -> Param {
    let colon = top_level(p, b':').unwrap_or(p.len());
    let names: Vec<&str> = p[..colon].split_whitespace().filter(|w| !w.starts_with('@')).collect();
    let label = names.first().map_or("_", |n| n.trim_matches('`')).to_string();
    let rest = &p[colon..];
    let eq = top_level(rest, b'=');
    let ty = eq.map_or(rest, |i| &rest[..i]).trim_end();
    let ty_text = ty.trim_start_matches(':').trim();
    Param { label, default: eq.is_some(), variadic: ty.ends_with("..."), closure: takes_closure(ty_text) }
}

/// The parameter list of a Swift signature: the first top-level `(…)`
/// after the name and its generic parameters.
fn param_list(sig: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut open = None;
    for (i, c) in sig.char_indices() {
        match c {
            '<' if open.is_none() => depth += 1,
            '>' if open.is_none() && !sig[..i].ends_with('-') => depth -= 1,
            '(' if depth == 0 && open.is_none() => {
                open = Some(i);
                depth = 1;
            }
            '(' if open.is_some() => depth += 1,
            ')' if open.is_some() => {
                depth -= 1;
                if depth == 0 {
                    return Some(&sig[open? + 1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The parameters of a Swift callable's signature (`None` without a list).
pub(crate) fn swift_params(sig: &str) -> Option<Vec<Param>> {
    // `subscript` / operators: parameters are unlabeled unless they name
    // an external label; the labels come from the selector instead.
    Some(split_top(param_list(sig)?).into_iter().map(parse_param).collect())
}

/// Arity range of a Swift callable from its signature: parameters with a
/// default value are optional, a variadic makes the range open-ended.
pub(crate) fn swift_signature_arity(sig: &str) -> Option<(u32, Option<u32>)> {
    let params = swift_params(sig)?;
    let min = params.iter().filter(|p| !p.default && !p.variadic).count() as u32;
    let variadic = params.iter().any(|p| p.variadic);
    Some((min, if variadic { None } else { Some(params.len() as u32) }))
}

/// The labels of a selector suffix (`(at:_:)` → `at`, `_`).
fn selector_labels(qn: &str) -> Option<Vec<&str>> {
    let (_, inner) = crate::hooks::overload_suffix(qn)?;
    let labels = inner.split('|').next().unwrap_or("");
    Some(labels.split(':').filter(|s| !s.is_empty()).collect())
}

/// Whether arguments `args[j..]` (labels) fit parameters `ps[i..]`.
fn fits_from(ps: &[Param], i: usize, args: &[&str], j: usize, budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if j == args.len() {
        return ps[i..].iter().all(|p| p.default || p.variadic);
    }
    let Some(p) = ps.get(i) else { return false };
    let a = args[j];
    let matches = (a == "?" && p.closure) || a == p.label;
    if matches {
        if p.variadic && fits_variadic(ps, i, args, j + 1, budget) {
            return true;
        }
        if fits_from(ps, i + 1, args, j + 1, budget) {
            return true;
        }
    }
    (p.default || p.variadic) && fits_from(ps, i + 1, args, j, budget)
}

/// A variadic parameter `ps[i]` takes the following unlabeled arguments.
fn fits_variadic(ps: &[Param], i: usize, args: &[&str], mut j: usize, budget: &mut u32) -> bool {
    while j < args.len() && args[j] == "_" {
        j += 1;
        if fits_from(ps, i + 1, args, j, budget) {
            return true;
        }
    }
    fits_from(ps, i + 1, args, j, budget)
}

/// Whether a parameter list accepts the argument labels of a call.
pub(crate) fn accepts_labels(ps: &[Param], args: &[&str]) -> bool {
    let mut budget = 4096;
    fits_from(ps, 0, args, 0, &mut budget)
}

/// The type text a Swift signature declares it returns: after the
/// top-level `->`, optionals, `throws` and `some` / `any` removed.
fn return_text(sig: &str) -> Option<String> {
    let close = sig.rfind(')')?;
    let after = &sig[close + 1..];
    let arrow = after.find("->")?;
    let t = after[arrow + 2..].split(" where ").next()?.trim();
    let t = t.trim_start_matches("some ").trim_start_matches("any ");
    let mut out = String::new();
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if depth == 0 && (c.is_alphanumeric() || c == '_' || c == '.') => out.push(c),
            '?' | '!' if depth == 0 => {}
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

impl<'a> Resolver<'a> {
    pub(crate) fn is_swift(&self, f: FileId) -> bool {
        self.files[f as usize].language == Some(Language::Swift)
    }

    // -- extension members ---------------------------------------------------

    /// Extension members `T.m` whose type `T` is not declared in their own
    /// module: attached to the type found through the file's imports, else
    /// to the only Swift type of that name (nested `A.B` walked).
    pub(crate) fn build_swift_extension_members(&mut self) {
        let mut found = Vec::new();
        for (fid, d, node, owner) in self.swift_extension_members() {
            let declared_here = !owner.contains('.') && self.module_def(fid, owner).is_some();
            if !declared_here && let Some(t) = self.swift_type_path(fid, owner) {
                found.push((t, d.name.clone(), node, d.label));
            }
        }
        for (t, name, node, label) in found {
            self.add_member(t, &name, node, label);
        }
    }

    /// Every top-level Swift definition named `T.m` (a member of an
    /// extension): (file, definition, node, owner path `T`).
    fn swift_extension_members(&self) -> Vec<(FileId, &'a Definition, NodeId, &'a str)> {
        let mut out = Vec::new();
        for i in (0..self.files.len()).filter(|&i| self.is_swift(i as FileId)) {
            let defs = self.defs(i as FileId);
            for &j in &self.top_level[i] {
                let d = &defs[j as usize];
                let local = d.qualified_name.split('(').next().unwrap_or("");
                if let Some((owner, member)) = local.rsplit_once('.')
                    && member == d.name
                {
                    out.push((i as FileId, d, self.def_node[i][j as usize], owner));
                }
            }
        }
        out
    }

    /// The Swift type a dotted path names from file `f`: its first segment
    /// in the module, an imported module or as the only Swift type of that
    /// name; then nested types.
    fn swift_type_path(&self, f: FileId, path: &str) -> Option<NodeId> {
        let mut segs = path.split('.');
        let head = segs.next()?;
        let mut t = self.module_def(f, head).or_else(|| self.swift_imported_type(f, head))?;
        for seg in segs {
            t = self.find_member(t, seg, Want::Type).map(|(n, _)| n)?;
        }
        self.nodes[t as usize].label.is_type().then_some(t)
    }

    /// A type `name` exported by a module `f` imports, else the only Swift
    /// type with that name.
    fn swift_imported_type(&self, f: FileId, name: &str) -> Option<NodeId> {
        if let Some(Some(crate::Target::Symbol(n))) = self.envs.get(f as usize).and_then(|e| e.get(name)) {
            return Some(*n);
        }
        let cands: Vec<NodeId> = self
            .by_name
            .get(name)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&n| {
                        let node = &self.nodes[n as usize];
                        node.label.is_type() && node.language == Some(Language::Swift)
                    })
                    .collect()
            })
            .unwrap_or_default();
        (cands.len() == 1).then(|| cands[0])
    }

    /// Extension members by (extended type's last segment, member name).
    fn swift_extension_index(&self) -> &HashMap<(String, String), Vec<NodeId>> {
        self.swift.extensions.get_or_init(|| {
            let mut m: HashMap<(String, String), Vec<NodeId>> = HashMap::new();
            for (_, d, node, owner) in self.swift_extension_members() {
                if d.label.is_callable() {
                    let last = owner.rsplit('.').next().unwrap_or(owner).to_string();
                    m.entry((last, d.name.clone())).or_default().push(node);
                }
            }
            m
        })
    }

    /// `Lib.f()` / `Lib.T`: a name qualified by a module of the snapshot
    /// (`import_resolved` 0.95), when the module declares no type of
    /// that name. `None` otherwise (the receiver is a value or a type).
    pub(crate) fn swift_module_member(&self, f: FileId, item: &RefItem<'_>, receiver: &str) -> Option<Outcome> {
        if !self.is_swift(f) || !crate::is_ident(receiver) || self.module_def(f, receiver).is_some() {
            return None;
        }
        let &p = self.package_index().get(&(LanguageFamily::Swift, receiver.to_string()))?;
        let rep = self.packages[p as usize][0];
        self.module_def(rep, item.name).map(|n| Outcome::Hit(n, Strategy::ImportResolved, 0.95))
    }

    /// A Swift call whose receiver resolution found nothing in the
    /// project: the extension member of a type with the receiver's type
    /// name (`$type:String` → `extension String { func m() }`), unique only.
    pub(crate) fn swift_extension(&self, f: FileId, item: &RefItem<'_>, receiver: &str, out: Outcome) -> Outcome {
        if !self.is_swift(f) || matches!(out, Outcome::Hit(..)) || !matches!(item.kind, rueca_model::RefKind::Call) {
            return out;
        }
        let Some(ty) = receiver.strip_prefix(TYPE_RECEIVER_PREFIX) else { return out };
        let last = ty.rsplit('.').next().unwrap_or(ty).to_string();
        match self.swift_extension_index().get(&(last, item.name.to_string())).map(Vec::as_slice) {
            Some([n]) => Outcome::Hit(*n, Strategy::TypeResolved, 0.85),
            _ => out,
        }
    }

    // -- overloads -----------------------------------------------------------

    /// The parameters of a Swift callable: labels from its selector,
    /// defaults and variadics from its signature.
    /// `None` when they are unknown (no selector, or a signature whose
    /// parameter list disagrees with it: truncated, or not Swift).
    fn swift_callable_params(&self, n: NodeId) -> Option<Vec<Param>> {
        let node = &self.nodes[n as usize];
        if node.language != Some(Language::Swift) || !node.label.is_callable() {
            return None;
        }
        let labels = selector_labels(&node.qualified_name)?;
        let mut params = swift_params(node.signature.as_deref()?)?;
        if params.len() != labels.len() {
            return None;
        }
        for (p, l) in params.iter_mut().zip(&labels) {
            p.label = l.to_string();
        }
        Some(params)
    }

    /// Whether Swift callable `n` accepts a call's argument labels (`true`
    /// when either is unknown): a call the compiler accepted names a
    /// callable whose parameters take its labels, so one that cannot is
    /// another callable (the memberwise initializer of a struct, a method
    /// of the same name elsewhere).
    pub(crate) fn swift_accepts(&self, n: NodeId, item: &RefItem<'_>) -> bool {
        let args: &[&str] = match (item.arg_types.as_deref(), item.arity) {
            (Some(a), _) => a,
            (None, Some(0)) => &[],
            _ => return true,
        };
        self.swift_callable_params(n).is_none_or(|ps| accepts_labels(&ps, args))
    }

    /// What a Swift reference that resolved to `target` reaches: `target`
    /// itself when it is no overload and accepts the call's labels (see
    /// [`Resolver::swift_accepts`]), else the overloads of its set that
    /// accept them; one keeps the confidence, several are linked × 0.8
    /// (without the caller when another remains), none links nothing.
    pub(crate) fn swift_expand(
        &self,
        target: NodeId,
        caller: NodeId,
        item: &RefItem<'_>,
        strategy: Strategy,
        confidence: f32,
    ) -> Vec<(NodeId, Strategy, f32)> {
        let single = [target];
        let set = self.overload_of.get(&target).map_or(&single[..], |&s| self.overload_sets[s].as_slice());
        let fit: Vec<NodeId> = set.iter().copied().filter(|&n| self.swift_accepts(n, item)).collect();
        if let [one] = fit.as_slice() {
            return vec![(*one, strategy, confidence)];
        }
        let others: Vec<NodeId> = fit.iter().copied().filter(|&n| n != caller).collect();
        let cands = if others.is_empty() { fit } else { others };
        cands.into_iter().map(|n| (n, strategy, confidence * 0.8)).collect()
    }

    // -- call results --------------------------------------------------------

    /// The type a Swift callable returns: its declared return type resolved
    /// in its file (`Self` is its type), an initializer's own type.
    pub(crate) fn swift_return_lookup(&self, callee: NodeId) -> TypeLookup {
        let node = &self.nodes[callee as usize];
        let (Some(file), Some(sig)) = (node.file, node.signature.as_deref()) else { return TypeLookup::Unknown };
        let owner = || {
            let local = node.qualified_name.split('(').next().unwrap_or("");
            let (o, _) = local.rsplit_once('.')?;
            self.nodes_by_qn().get(o)?.iter().copied().find(|&t| self.nodes[t as usize].label.is_type())
        };
        if node.name == "init" {
            return owner().map_or(TypeLookup::Unknown, TypeLookup::Found);
        }
        if node.label == Label::Class || node.label == Label::Struct || node.label == Label::Enum {
            return TypeLookup::Found(callee);
        }
        let Some(ret) = return_text(sig) else { return TypeLookup::Unknown };
        if ret == "Self" {
            return owner().map_or(TypeLookup::Unknown, TypeLookup::Found);
        }
        let j = self.def_node[file as usize].iter().position(|&x| x == callee);
        let scope = j.and_then(|j| self.parents[file as usize][j]);
        self.resolve_type_text(file, scope, &ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(s: &str) -> Vec<&str> {
        s.split(',').collect()
    }

    #[test]
    fn signature_parameters_and_arity() {
        let ps = swift_params("init?(x: Int, label: String = \"a, b\")").unwrap();
        assert_eq!(ps.len(), 2);
        assert!(ps[1].default);
        assert_eq!(swift_signature_arity("func draw(at x: Int, _ y: Int) -> Bool"), Some((2, Some(2))));
        assert_eq!(swift_signature_arity("func top(_ a: Int...) throws -> Int"), Some((0, None)));
        assert_eq!(swift_signature_arity("func f<T: P>(x: T, body: (Int) -> Void = { _ in })"), Some((1, Some(2))));
        assert_eq!(swift_signature_arity("func g()"), Some((0, Some(0))));
    }

    #[test]
    fn labels_choose_overloads() {
        let p = |l: &str, d: bool, v: bool| Param { label: l.into(), default: d, variadic: v, closure: true };
        let x = vec![p("x", false, false)];
        let y = vec![p("y", false, false)];
        assert!(accepts_labels(&x, &labels("x")));
        assert!(!accepts_labels(&y, &labels("x")));
        let defaulted = vec![p("a", true, false), p("body", false, false)];
        assert!(accepts_labels(&defaulted, &labels("?")));
        assert!(accepts_labels(&defaulted, &labels("a,body")));
        let var = vec![p("_", false, true), p("sep", true, false)];
        assert!(accepts_labels(&var, &labels("_,_,_,sep")));
        assert!(accepts_labels(&var, &[]));
        assert!(!accepts_labels(&x, &labels("x,_")));
        let text = swift_params("init(stringLiteral value: String)").unwrap();
        assert!(!accepts_labels(&text, &labels("?")));
        let body = swift_params("init(_ f: @escaping (Int) -> Void)").unwrap();
        assert!(accepts_labels(&body, &labels("?")));
    }

    #[test]
    fn return_types() {
        assert_eq!(return_text("func make() -> Point?").as_deref(), Some("Point"));
        assert_eq!(return_text("func f() async throws -> Box<Int>").as_deref(), Some("Box"));
        assert_eq!(return_text("func g(x: (Int) -> Int) -> some View").as_deref(), Some("View"));
        assert_eq!(return_text("func h() -> (Int, Int)"), None);
        assert_eq!(return_text("func v()"), None);
        assert_eq!(selector_labels("A.draw(at:_:)"), Some(vec!["at", "_"]));
        assert_eq!(selector_labels("A.run()"), Some(vec![]));
        assert_eq!(selector_labels("A.f(_:n:|String,(Int)->Void)"), Some(vec!["_", "n"]));
    }
}
