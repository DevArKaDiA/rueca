//! JVM family additions for Kotlin: imports bound by package name
//! (`LanguagePack::namespace_imports`) and companion objects.
//!
//! - **Imports by package.** A Kotlin file may declare any package wherever
//!   it lives, so `import a.b.C` binds the package `a.b` of the JVM family
//!   (Kotlin and Java files alike), represented by its first file; the
//!   `IMPORTS` edge goes to the directory holding most of its files.
//! - **Extension functions.** `x.f()` whose receiver has no member `f`
//!   (or whose receiver type is unknown or outside the project) calls a
//!   top-level extension function `fun T.f()` in scope: imported by name or
//!   on demand, or declared in the file's package. A candidate whose
//!   receiver type matches the call's (`$type:T`, a base of `T`, or a type
//!   parameter) is `type_resolved` 0.85; otherwise, when no project
//!   method has the name, a single imported candidate is `import_resolved`
//!   0.8 and a single one of the package `heuristic` 0.6. Several
//!   candidates that the receiver does not tell apart resolve nothing.
//! - **Implicit receivers of lambdas** (`$with:<T>`, a call without a
//!   receiver inside `x.apply { … }`, `x.run { … }` or `with(x) { … }`
//!   where `x` holds a `T`): a member of `T` (or an extension of it) first,
//!   else the call resolves as written, without a receiver.
//! - **Enum entries.** `Color.RED.hex()`: when `Color.RED` names no type,
//!   the member is looked up in `Color` (an entry's members are its enum's).
//! - **Arity.** A Kotlin callable's parameter range comes from its
//!   signature: parameters with a default value are optional and a
//!   `vararg` makes it open-ended, so overloads are chosen by arity like
//!   C++'s.
//! - **Overloads.** Among the overloads a Kotlin call's arity admits, the
//!   argument types the extractor knows choose: a value type (`String`,
//!   `Int`, `KClass`, a lambda's `Function`, …) rules out a parameter of
//!   another value type or of a project class, a spread argument (`*xs`)
//!   needs a `vararg`, and a call result (`^f`) takes the return type every
//!   project callable named `f` declares. The candidates with the most
//!   exact parameter matches remain.
//! - **Call results** (`$ret:<receiver>#<name>`, the Rust pack's
//!   receiver form): the callee is resolved like a call and the return
//!   type its Kotlin signature declares (`fun f(): T`) is resolved in the
//!   callee's file; a constructor returns its class.
//! - **Companion objects.** `Order.create()` calls a member of `Order`'s
//!   companion object: a member lookup in a Kotlin class that finds
//!   nothing in the class and its bases continues in its companion (the
//!   child class whose signature declares a `companion object`).

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};

use rueca_model::{FileId, Label, Language, LanguageFamily, NodeId};

use rueca_extract::TYPE_RECEIVER_PREFIX;
use rueca_model::Strategy;

use crate::{LocalDefId, Outcome, RefItem, RefKind, Resolver, Target, Want, parent_dir};

/// Tables built on first use.
#[derive(Default)]
pub(crate) struct JvmTables {
    /// (family, module name) → package.
    packages: OnceCell<HashMap<(LanguageFamily, String), u32>>,
    /// Kotlin class → its companion object.
    companions: OnceCell<HashMap<NodeId, NodeId>>,
}

impl Resolver<'_> {
    fn package_index(&self) -> &HashMap<(LanguageFamily, String), u32> {
        self.jvm.packages.get_or_init(|| {
            let mut m = HashMap::new();
            for (p, members) in self.packages.iter().enumerate() {
                let first = members[0];
                if let Some(family) = self.families[first as usize] {
                    m.entry((family, self.module_names[first as usize].clone())).or_insert(p as u32);
                }
            }
            m
        })
    }

    /// The `Folder` node of the directory holding most files of package `p`
    /// (the first such directory in path order on ties).
    fn main_folder(&self, p: u32) -> Option<NodeId> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for &m in &self.packages[p as usize] {
            *counts.entry(parent_dir(&self.files[m as usize].path).unwrap_or("")).or_default() += 1;
        }
        let best = counts.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))?;
        self.folder_node(best.0)
    }

    /// Import targets of a `LanguagePack::namespace_imports` file: the
    /// package each import names, as its first file and its main folder.
    pub(crate) fn jvm_namespace_import_targets(&self, fid: FileId) -> (Vec<Option<FileId>>, Vec<Option<NodeId>>) {
        let imports = self.imports(fid);
        let (mut t, mut d) = (vec![None; imports.len()], vec![None; imports.len()]);
        let Some(family) = self.families[fid as usize] else { return (t, d) };
        for (k, imp) in imports.iter().enumerate() {
            if imp.names.is_empty() {
                continue;
            }
            if let Some(&p) = self.package_index().get(&(family, imp.module.clone())) {
                t[k] = Some(self.packages[p as usize][0]);
                d[k] = self.main_folder(p);
            }
        }
        (t, d)
    }

    fn companions(&self) -> &HashMap<NodeId, NodeId> {
        self.jvm.companions.get_or_init(|| {
            let mut m = HashMap::new();
            for (i, f) in self.files.iter().enumerate() {
                if f.language != Some(Language::Kotlin) {
                    continue;
                }
                let defs = self.defs(i as FileId);
                for (j, d) in defs.iter().enumerate() {
                    let companion = d.label == Label::Class
                        && d.signature.as_deref().is_some_and(|s| s.contains("companion object"));
                    if let (true, Some(parent)) = (companion, self.parents[i][j]) {
                        m.insert(self.node_of(i as FileId, parent), self.node_of(i as FileId, j as u32));
                    }
                }
            }
            m
        })
    }

    /// Member `name` of the companion object of Kotlin class `class`.
    pub(crate) fn companion_member(&self, class: NodeId, name: &str, want: Want) -> Option<(NodeId, bool)> {
        let &companion = self.companions().get(&class)?;
        (companion != class).then(|| self.find_member(companion, name, want)).flatten()
    }
}

/// Splits a parameter list on its top-level commas.
fn split_top(params: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, c) in params.char_indices() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '>' if !params[..i].ends_with('-') => depth -= 1,
            ',' if depth == 0 => {
                out.push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out.into_iter().map(str::trim).filter(|p| !p.is_empty()).collect()
}

/// Whether a Kotlin parameter has a default value (`= …` outside
/// parentheses and generic arguments).
fn has_default(p: &str) -> bool {
    let mut depth = 0i32;
    let b = p.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'<' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i == 0 || b[i - 1] != b'-' => depth -= 1,
            b'=' if depth == 0 && b.get(i + 1) != Some(&b'=') && (i == 0 || !b"=!<>".contains(&b[i - 1])) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The byte range of the parameter list of a Kotlin signature: after
/// `name(` (functions; also backquoted), else the first `(` of a
/// constructor.
fn param_range(sig: &str, name: &str) -> Option<(usize, usize)> {
    let open = if sig.contains("constructor(") && !sig.contains("fun ") {
        sig.find("constructor(")? + "constructor".len()
    } else {
        let bare = format!("{name}(");
        let quoted = format!("{name}`(");
        sig.find(&bare).map(|i| i + name.len()).or_else(|| sig.find(&quoted).map(|i| i + name.len() + 1))?
    };
    let mut depth = 0i32;
    for (i, c) in sig[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, open + i));
                }
            }
            _ => {}
        }
    }
    None
}

/// Arity range of a Kotlin callable from its signature: parameters with a
/// default value are optional, a `vararg` makes the range open-ended.
pub(crate) fn kotlin_signature_arity(sig: &str, name: &str) -> Option<(u32, Option<u32>)> {
    let (a, b) = param_range(sig, name)?;
    let (mut min, mut max, mut variadic) = (0u32, 0u32, false);
    for p in split_top(&sig[a..b]) {
        if p.split_whitespace().any(|w| w == "vararg") {
            variadic = true;
            continue;
        }
        max += 1;
        if !has_default(p) {
            min += 1;
        }
    }
    Some((min, if variadic { None } else { Some(max) }))
}

/// The receiver type of an extension function's signature, generic
/// arguments and nullability removed (`fun <T> KClass<T>.asClassName()`
/// → `KClass`); `None` when the function is not an extension.
pub(crate) fn extension_receiver(sig: &str, name: &str) -> Option<String> {
    let at = sig.find(&format!(".{name}(")).or_else(|| sig.find(&format!(".`{name}`(")))?;
    let head = &sig[..at];
    let fun = head.find("fun ")? + 4;
    let mut rest = head[fun..].trim_start();
    if rest.starts_with('<') {
        let mut depth = 0i32;
        let end = rest.char_indices().find(|&(_, c)| {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
        rest = rest[end.0 + 1..].trim_start();
    }
    let ty: String = rest.split('<').next()?.chars().filter(|c| !c.is_whitespace() && *c != '?').collect();
    (!ty.is_empty()).then(|| ty.rsplit('.').next().unwrap_or(&ty).to_string())
}

/// Type parameter names of a signature (`fun <T, R : Any> …` → `T`, `R`).
fn type_params(sig: &str) -> Vec<String> {
    let Some(i) = sig.find("fun <") else { return Vec::new() };
    let rest = &sig[i + 5..];
    let end = rest.find('>').unwrap_or(rest.len());
    split_top(&rest[..end])
        .iter()
        .filter_map(|p| p.split([':', ' ']).find(|w| !w.is_empty() && *w != "reified" && *w != "in" && *w != "out"))
        .map(str::to_string)
        .collect()
}

/// Prefix of an implicit lambda receiver (`$with:<T>`, `rueca-lang-kotlin`).
pub(crate) const WITH_RECEIVER_PREFIX: &str = "$with:";

/// Kotlin value types: an argument of one never converts to another
/// (an integer literal still fits `Long`, `Short`, `Byte`).
const VALUE_TYPES: &[&str] =
    &["Boolean", "Byte", "Char", "Class", "Double", "Float", "Function", "Int", "KClass", "Long", "Short", "String"];

/// The parameter types of a Kotlin signature (simple names, generic
/// arguments and nullability removed; `T...` for a `vararg`).
fn kotlin_param_types(sig: &str, name: &str) -> Option<Vec<String>> {
    let (a, b) = param_range(sig, name)?;
    let types = split_top(&sig[a..b])
        .into_iter()
        .map(|p| {
            let vararg = p.split_whitespace().any(|w| w == "vararg");
            let ty = p.split_once(':').map_or("?", |(_, t)| t);
            let ty = ty.split('=').next().unwrap_or(ty);
            let simple = simple_type(ty);
            if vararg { format!("{simple}...") } else { simple }
        })
        .collect();
    Some(types)
}

/// `kotlin.collections.List<Foo>?` → `List`; function types → `Function`.
fn simple_type(ty: &str) -> String {
    let ty = ty.trim();
    if ty.contains("->") {
        return "Function".to_string();
    }
    let head: String = ty.split('<').next().unwrap_or(ty).chars().filter(|c| !c.is_whitespace() && *c != '?').collect();
    head.rsplit('.').next().unwrap_or(&head).trim_matches(['(', ')']).to_string()
}

/// The declared return type after a Kotlin signature's parameter list.
fn kotlin_return_type(sig: &str, name: &str) -> Option<String> {
    let (_, b) = param_range(sig, name)?;
    let rest = sig[b + 1..].trim_start().strip_prefix(':')?;
    let ty = simple_type(rest.split(" where ").next().unwrap_or(rest));
    (!ty.is_empty()).then_some(ty)
}

/// The declared return type of a Kotlin signature as written, generic
/// arguments and nullability removed (`fun f(): a.Foo<T>?` → `a.Foo`).
fn kotlin_return_text(sig: &str, name: &str) -> Option<String> {
    let (_, b) = param_range(sig, name)?;
    let rest = sig[b + 1..].trim_start().strip_prefix(':')?;
    let rest = rest.split(" where ").next().unwrap_or(rest).trim();
    if rest.contains("->") {
        return None;
    }
    let head: String =
        rest.split('<').next().unwrap_or(rest).chars().filter(|c| !c.is_whitespace() && *c != '?').collect();
    (!head.is_empty()).then_some(head)
}

/// How well argument `a` fits parameter `p`: `None` rules the candidate
/// out, else 2 for an exact match, 1 for a spread into a `vararg`, 0 when
/// unknown.
fn arg_fit(a: &str, p: &str, vararg_tail: bool) -> Option<u32> {
    let p = p.trim_end_matches("...");
    if a == "?" || p == "?" || p == "Any" || (p.len() == 1 && p.chars().all(char::is_uppercase)) {
        return Some(0);
    }
    if a == "*" {
        return vararg_tail.then_some(1);
    }
    if a == p {
        return Some(2);
    }
    let (av, pv) = (VALUE_TYPES.contains(&a), VALUE_TYPES.contains(&p));
    let literal_widening = a == "Int" && matches!(p, "Long" | "Short" | "Byte");
    if literal_widening || (av && !pv) {
        return Some(0);
    }
    if pv { None } else { Some(0) }
}

/// Score of a candidate's parameters against the arguments (`None` = ruled
/// out), argument by argument with `fit` (see [`arg_fit`]).
fn overload_score_with(
    params: &[String],
    args: &[String],
    fit: impl Fn(&str, &str, bool) -> Option<u32>,
) -> Option<u32> {
    let mut score = 0;
    for (i, a) in args.iter().enumerate() {
        let last = params.len().checked_sub(1);
        let (p, tail) = match params.get(i) {
            Some(p) => (p.as_str(), Some(i) == last && p.ends_with("...")),
            None => (params.last().filter(|p| p.ends_with("..."))?.as_str(), true),
        };
        score += fit(a, p, tail)?;
    }
    Some(score)
}

#[cfg(test)]
fn overload_score(params: &[String], args: &[String]) -> Option<u32> {
    overload_score_with(params, args, arg_fit)
}

impl Resolver<'_> {
    pub(crate) fn is_kotlin(&self, f: FileId) -> bool {
        self.files[f as usize].language == Some(Language::Kotlin)
    }

    /// `$with:<T>` receivers (module docs); `None` for any other receiver.
    pub(crate) fn kotlin_implicit_receiver(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Option<Outcome> {
        let ty = receiver.strip_prefix(WITH_RECEIVER_PREFIX)?;
        let typed = format!("{TYPE_RECEIVER_PREFIX}{ty}");
        let out = self.resolve_with_receiver(f, item, scope, &typed);
        let out = self.kotlin_extension(f, item, scope, &typed, out);
        Some(match out {
            Outcome::Hit(_, s, _) if s != Strategy::Heuristic => out,
            _ => self.resolve_bare(f, item, scope),
        })
    }

    /// `Color.RED.hex()`: the member of the enum `Color` (module docs).
    fn kotlin_enum_entry(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Option<Outcome> {
        let (owner, entry) = receiver.rsplit_once('.')?;
        let entry_like = entry.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
        if !entry_like || !owner.split('.').all(crate::is_ident) {
            return None;
        }
        let class = match self.resolve_type_text(f, scope, owner) {
            crate::hooks::TypeLookup::Found(c) | crate::hooks::TypeLookup::ByName(c) => c,
            _ => return None,
        };
        if self.nodes[class as usize].label != Label::Enum {
            return None;
        }
        let (n, inherited) = self.find_member(class, item.name, Want::of(item.kind))?;
        Some(Outcome::Hit(n, Strategy::TypeResolved, if inherited { 0.85 } else { 0.9 }))
    }

    /// A call with a receiver in a Kotlin file that its receiver does not
    /// explain: an enum entry's member, else an extension function in scope
    /// (module docs).
    pub(crate) fn kotlin_extension(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
        out: Outcome,
    ) -> Outcome {
        let explained = match out {
            Outcome::Hit(_, s, _) => s != Strategy::Heuristic,
            _ => false,
        };
        if explained || !self.is_kotlin(f) || item.kind != RefKind::Call {
            return out;
        }
        if let Some(hit) = self.kotlin_enum_entry(f, item, scope, receiver) {
            return hit;
        }
        let ty = receiver.strip_prefix(TYPE_RECEIVER_PREFIX).map(|t| t.split('<').next().unwrap_or(t));
        let class = ty.and_then(|t| match self.resolve_type_text(f, scope, t) {
            crate::hooks::TypeLookup::Found(c) | crate::hooks::TypeLookup::ByName(c) => Some(c),
            _ => None,
        });
        let simple = ty.map(|t| t.rsplit('.').next().unwrap_or(t));
        self.pick_extension(f, item.name, simple, class).unwrap_or(out)
    }

    /// The return type every project callable named `name` declares
    /// (Kotlin `fun f(…): T`), when they agree.
    fn call_result_type(&self, name: &str) -> Option<String> {
        let mut found: Option<String> = None;
        for &n in self.by_name.get(name)? {
            let node = &self.nodes[n as usize];
            if node.language != Some(Language::Kotlin) || !node.label.is_callable() {
                continue;
            }
            let ty = kotlin_return_type(node.signature.as_deref()?, name)?;
            match &found {
                Some(t) if *t != ty => return None,
                _ => found = Some(ty),
            }
        }
        found
    }

    /// The simple name of the type a call argument written `^spec`
    /// evaluates to: `<receiver>#<name>` resolves the callee and reads its
    /// return type; a bare `<name>` takes the return type every project
    /// callable of that name declares.
    fn call_spec_type(&self, f: FileId, scope: Option<LocalDefId>, spec: &str) -> Option<String> {
        let Some((recv, name)) = spec.rsplit_once('#') else { return self.call_result_type(spec) };
        let call = RefItem {
            kind: RefKind::Call,
            name,
            receiver: (!recv.is_empty()).then_some(recv),
            enclosing: scope,
            span: rueca_model::Span::default(),
            decorated: None,
            arity: None,
            arg_types: None,
        };
        let Outcome::Hit(callee, _, _) = self.resolve_item(f, &call, scope) else { return None };
        match self.kotlin_return_lookup(callee) {
            crate::hooks::TypeLookup::Found(t) | crate::hooks::TypeLookup::ByName(t) => {
                Some(self.nodes[t as usize].name.clone())
            }
            _ => None,
        }
    }

    /// The overloads of `chosen` (already fitting the arity) that the
    /// argument types of a Kotlin call favour (module docs); unchanged when a
    /// candidate's parameters are unknown.
    pub(crate) fn kotlin_narrow(
        &self,
        from: FileId,
        scope: Option<LocalDefId>,
        chosen: Vec<NodeId>,
        args: Option<&[&str]>,
    ) -> Vec<NodeId> {
        let Some(args) = args else { return chosen };
        let args: Vec<String> = args
            .iter()
            .map(|a| match a.strip_prefix('^') {
                Some(spec) => self.call_spec_type(from, scope, spec).unwrap_or_else(|| "?".to_string()),
                None => a.to_string(),
            })
            .collect();
        let mut scored: Vec<(NodeId, u32)> = Vec::new();
        for &n in &chosen {
            let node = &self.nodes[n as usize];
            let params = match node.language {
                Some(Language::Kotlin) => node.signature.as_deref().and_then(|s| kotlin_param_types(s, &node.name)),
                _ => self.param_types(n),
            };
            let Some(params) = params else { return chosen };
            if let Some(s) = overload_score_with(&params, &args, |a, p, tail| self.fit(a, p, tail)) {
                scored.push((n, s));
            }
        }
        let Some(best) = scored.iter().map(|s| s.1).max() else { return chosen };
        scored.into_iter().filter(|s| s.1 == best).map(|s| s.0).collect()
    }

    /// The type a Kotlin callable returns, for `$ret:` receivers: its
    /// declared return type resolved in its file; a constructor's class.
    pub(crate) fn kotlin_return_lookup(&self, callee: NodeId) -> crate::hooks::TypeLookup {
        use crate::hooks::TypeLookup;
        let node = &self.nodes[callee as usize];
        let (Some(file), Some(sig)) = (node.file, node.signature.as_deref()) else { return TypeLookup::Unknown };
        if node.label.is_type() {
            return TypeLookup::Found(callee);
        }
        if sig.starts_with("constructor") {
            let owner = node.qualified_name.rsplit_once('.').map(|(o, _)| o.to_string());
            let class = owner.and_then(|o| {
                self.nodes_by_qn().get(&o)?.iter().copied().find(|&t| self.nodes[t as usize].label.is_type())
            });
            return class.map_or(TypeLookup::Unknown, TypeLookup::Found);
        }
        let Some(ret) = kotlin_return_text(sig, &node.name) else { return TypeLookup::Unknown };
        let j = self.def_node[file as usize].iter().position(|&x| x == callee);
        let scope = j.and_then(|j| self.parents[file as usize][j]);
        self.resolve_type_text(file, scope, &ret)
    }

    /// [`arg_fit`], refined by the project's class hierarchy: a project
    /// class argument passes to a parameter of its own class (2) or of one
    /// of its bases (1), and to no other when its hierarchy is entirely in
    /// the project.
    fn fit(&self, a: &str, p: &str, tail: bool) -> Option<u32> {
        let base = arg_fit(a, p, tail)?;
        let p = p.trim_end_matches("...");
        if base != 0 || a == "?" || a == "*" || VALUE_TYPES.contains(&a) || p == "Any" || p == "?" {
            return Some(base);
        }
        let Some(class) = self.unique_type_named(a) else { return Some(0) };
        if self.has_base_named(class, p) {
            return Some(1);
        }
        let generic = p.len() == 1 && p.chars().all(char::is_uppercase);
        (generic || self.type_is_open(class)).then_some(0)
    }

    /// The only JVM-family type named `name`.
    fn unique_type_named(&self, name: &str) -> Option<NodeId> {
        let mut types = self.by_name.get(name)?.iter().copied().filter(|&n| {
            let node = &self.nodes[n as usize];
            node.label.is_type() && node.language.map(Language::family) == Some(LanguageFamily::Java)
        });
        let first = types.next()?;
        types.next().is_none().then_some(first)
    }

    /// Whether a (transitive) base of `class` is named `name`.
    fn has_base_named(&self, class: NodeId, name: &str) -> bool {
        let mut stack = vec![(class, 0usize)];
        while let Some((c, depth)) = stack.pop() {
            if self.nodes[c as usize].name == name && c != class {
                return true;
            }
            if depth < 8 {
                stack.extend(self.bases.get(&c).into_iter().flatten().map(|&b| (b, depth + 1)));
            }
        }
        false
    }

    /// Extension functions named `name` visible from `f`: (node, imported).
    fn extension_candidates(&self, f: FileId, name: &str) -> Vec<(NodeId, bool)> {
        let mut out: Vec<(NodeId, bool)> = Vec::new();
        if let Some(Some(Target::Symbol(n))) = self.envs[f as usize].get(name) {
            out.push((*n, true));
        }
        for g in self.module_files(f) {
            for &j in &self.top_level[g as usize] {
                let d = &self.defs(g)[j as usize];
                if d.name == name && d.label == Label::Function {
                    out.push((self.node_of(g, j), false));
                }
            }
        }
        out.retain(|&(n, _)| self.is_extension(n, name).is_some());
        out.sort_unstable();
        out.dedup_by_key(|c| c.0);
        out
    }

    /// The receiver type of extension function `n` (see [`extension_receiver`]).
    fn is_extension(&self, n: NodeId, name: &str) -> Option<String> {
        let node = &self.nodes[n as usize];
        if node.language != Some(Language::Kotlin) || !node.label.is_callable() {
            return None;
        }
        extension_receiver(node.signature.as_deref()?, name)
    }

    /// Whether extension `n` (receiver `on`) applies to a receiver of
    /// simple type `ty` (class node `class`).
    fn extension_fits(&self, n: NodeId, on: &str, ty: &str, class: Option<NodeId>) -> bool {
        if on == ty || type_params(self.nodes[n as usize].signature.as_deref().unwrap_or("")).iter().any(|p| p == on) {
            return true;
        }
        let Some(c) = class else { return false };
        let mut stack = vec![(c, 0usize)];
        while let Some((x, depth)) = stack.pop() {
            if self.nodes[x as usize].name == on {
                return true;
            }
            if depth < 8 {
                stack.extend(self.bases.get(&x).into_iter().flatten().map(|&b| (b, depth + 1)));
            }
        }
        false
    }

    fn pick_extension(&self, f: FileId, name: &str, ty: Option<&str>, class: Option<NodeId>) -> Option<Outcome> {
        let cands = self.extension_candidates(f, name);
        if let Some(t) = ty {
            let fit: Vec<(NodeId, bool)> = cands
                .iter()
                .copied()
                .filter(|&(n, _)| self.is_extension(n, name).is_some_and(|on| self.extension_fits(n, &on, t, class)))
                .collect();
            if let [(n, _)] = fit.as_slice() {
                return Some(Outcome::Hit(*n, Strategy::TypeResolved, 0.85));
            }
            if !fit.is_empty() {
                return None;
            }
        }
        // A receiver the call does not type: only when no project method
        // shares the name (else the member is as likely).
        if self.method_named(name) {
            return None;
        }
        match cands.as_slice() {
            [(n, true)] => Some(Outcome::Hit(*n, Strategy::ImportResolved, 0.8)),
            [(n, false)] => Some(Outcome::Hit(*n, Strategy::Heuristic, 0.6)),
            _ => None,
        }
    }

    /// Whether a JVM-family method (a member of a type) is named `name`.
    fn method_named(&self, name: &str) -> bool {
        self.by_name.get(name).is_some_and(|ns| {
            ns.iter().any(|&n| {
                let node = &self.nodes[n as usize];
                node.label == Label::Method && node.language.map(Language::family) == Some(LanguageFamily::Java)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_from_signatures() {
        assert_eq!(kotlin_signature_arity("fun f(a: Int, b: String = \"x\")", "f"), Some((1, Some(2))));
        assert_eq!(kotlin_signature_arity("fun f(vararg xs: String)", "f"), Some((0, None)));
        assert_eq!(
            kotlin_signature_arity("constructor(val id: Long, var total: Int = 0)", "Order"),
            Some((1, Some(2)))
        );
        assert_eq!(kotlin_signature_arity("constructor()", "Order"), Some((0, Some(0))));
        assert_eq!(kotlin_signature_arity("fun g(cb: (Int) -> Unit = {}, m: Map<K, V>)", "g"), Some((1, Some(2))));
        assert_eq!(kotlin_signature_arity("fun `is`(x: Int)", "is"), Some((1, Some(1))));
    }

    #[test]
    fn overload_types() {
        let p = kotlin_param_types(
            "fun addProperty(name: String, type: KClass<*>, vararg modifiers: KModifier): T",
            "addProperty",
        );
        assert_eq!(p, Some(vec!["String".to_string(), "KClass".to_string(), "KModifier...".to_string()]));
        assert_eq!(
            kotlin_return_type("fun <T> KClass<T>.asTypeName(): TypeName", "asTypeName").as_deref(),
            Some("TypeName")
        );
        assert_eq!(kotlin_return_type("fun f(a: Int)", "f"), None);
        let params = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            overload_score(&params(&["String", "TypeName", "KModifier..."]), &params(&["String", "TypeName", "*"])),
            Some(5)
        );
        assert_eq!(
            overload_score(&params(&["String", "TypeName", "Iterable"]), &params(&["String", "TypeName", "*"])),
            None
        );
        assert_eq!(overload_score(&params(&["KClass"]), &params(&["TypeName"])), None);
        assert_eq!(overload_score(&params(&["Long"]), &params(&["Int"])), Some(0));
    }

    #[test]
    fn extension_receivers() {
        assert_eq!(extension_receiver("fun String.shout(): String", "shout").as_deref(), Some("String"));
        assert_eq!(
            extension_receiver("fun <T : Any> KClass<T>.asClassName(): ClassName", "asClassName").as_deref(),
            Some("KClass")
        );
        assert_eq!(
            extension_receiver("fun Taggable.Builder<*>.buildTagMap(): TagMap", "buildTagMap").as_deref(),
            Some("Builder")
        );
        assert_eq!(extension_receiver("fun shout(s: String)", "shout"), None);
        assert_eq!(type_params("fun <T, reified R : Any> T.f()"), vec!["T", "R"]);
    }
}
