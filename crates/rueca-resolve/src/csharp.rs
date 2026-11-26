//! C# resolver rules (RFC 0003 §10.4d): namespaces bound by name, partial
//! types and extension methods.
//!
//! The C# pack is package-scoped like Java (files of one namespace are one
//! module) and opts into [`LanguagePack::namespace_imports`]. On top of the
//! RFC 0003 hooks, this module adds what C# name lookup needs:
//!
//! - **Usings by name.** `using A.B;` binds the namespace whose module
//!   name is `A.B`, wherever its files are (`IMPORTS` File → the directory
//!   holding most of them); its exported types are bound like a star
//!   import. `using X = A.B.T;` binds `X` to type `T` of `A.B`, or to the
//!   namespace `A.B.T` when one exists; `using X = A;` binds `A`.
//!   `using static A.B.T;` binds the bare calls and reads of the file the
//!   pack lists to `T`'s members (a project member wins over a library
//!   one). `global using …` (recorded as re-exports) applies to every C#
//!   file of the same project — the nearest directory above with a
//!   `*.csproj`, else the repository.
//! - **Name lookup order** for a bare name, after local scope and implicit
//!   `this`: the file's namespace (`same_module` 0.90), the enclosing
//!   namespaces innermost first (`import_resolved` 0.90), the usings
//!   (0.95), the global namespace (0.90). Receivers (`Type.M()`,
//!   `A.B.Type.M()`, `Outer.Inner.M()`) take their head through the same
//!   lookup, else as a namespace path — absolute or relative to an
//!   enclosing namespace — then walk the remaining segments as types.
//!   A head that is none of these but a member of the enclosing type
//!   (a property declared in another partial part or in a base class)
//!   gives its declared type (as `$member:` receivers do). Library
//!   namespace heads (`System`, `Microsoft`) stop as external only when no
//!   project namespace matches.
//! - **`base`** is the `super` receiver.
//! - **Partial types**: the parts of a type (one global qualified name,
//!   several declarations, usually several files) share their members and
//!   resolved bases, so a member declared in one part is found from
//!   another and through either node.
//! - **Extension methods**: a call on a `$type:T` receiver whose type has
//!   no such member (or is a library type) binds the extension method of
//!   that name whose `this` parameter's type is named like `T` or one of
//!   its resolved bases, when all such methods are in one static class
//!   (`type_resolved` 0.85).
//! - **Arity** of a C# callable comes from its signature: parameters with
//!   default values are optional, a `params` array is variadic.

use super::*;
use crate::hooks::TypeLookup;

/// Confidence of a member found through a namespace other than the file's.
const OUTER_CONFIDENCE: f32 = 0.9;

/// Lazily built C# tables.
#[derive(Default)]
pub(crate) struct CSharpTables {
    /// Namespace → package (C# files only).
    namespaces: OnceCell<HashMap<String, u32>>,
    /// Extension methods by name: (method, simple name of the `this` type).
    extensions: OnceCell<HashMap<String, Vec<(NodeId, String)>>>,
    /// Per file: the directory of its project (nearest `*.csproj`), for
    /// `global using`.
    projects: OnceCell<Vec<Option<String>>>,
    /// Nesting of `$ret:` evaluations (bounded by [`MAX_RET_NESTING`]).
    ret_depth: std::cell::Cell<usize>,
}

/// Nested `$ret:` receivers evaluated at most.
const MAX_RET_NESTING: usize = 4;

/// Modifiers that may precede a method's return type in a signature.
const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "virtual",
    "override",
    "abstract",
    "sealed",
    "async",
    "new",
    "extern",
    "unsafe",
    "partial",
    "readonly",
    "file",
    "required",
];

/// The return type a C# method signature declares (`public async
/// Task<Order> GetAsync(int id)` → `Task<Order>`), `None` when it cannot be
/// read (constructors, operators).
fn return_text<'s>(signature: &'s str, name: &str) -> Option<&'s str> {
    let bytes = signature.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut from = 0;
    let at = loop {
        let k = from + signature.get(from..)?.find(name)?;
        let end = k + name.len();
        let before_ok = k == 0 || !word(bytes[k - 1]);
        if before_ok && matches!(bytes.get(end), Some(b'(' | b'<')) {
            break k;
        }
        from = end;
    };
    let mut head = signature[..at].trim_end();
    loop {
        let trimmed = head.trim_start();
        let Some((first, rest)) = trimmed.split_once(' ') else { break };
        if !MODIFIERS.contains(&first) {
            break;
        }
        head = rest;
    }
    let t = head.trim();
    (!t.is_empty() && !MODIFIERS.contains(&t)).then_some(t)
}

/// The type of a call's value: `Task<T>` / `ValueTask<T>` are `T` when
/// awaited, nullable marks dropped; `None` for `void`, a non-generic task,
/// arrays, tuples and keyword types (no project members).
fn value_type(ret: &str, awaited: bool) -> Option<String> {
    let mut t = ret.trim().trim_start_matches("ref ").trim_end_matches('?').trim().to_string();
    if awaited {
        let inner = ["Task<", "ValueTask<", "System.Threading.Tasks.Task<", "System.Threading.Tasks.ValueTask<"]
            .iter()
            .find_map(|p| t.strip_prefix(p).and_then(|r| r.strip_suffix('>')).map(str::to_string));
        t = inner?.trim_end_matches('?').trim().to_string();
    }
    let keyword = matches!(
        t.as_str(),
        "void"
            | "bool"
            | "byte"
            | "sbyte"
            | "char"
            | "decimal"
            | "double"
            | "float"
            | "int"
            | "uint"
            | "long"
            | "ulong"
            | "short"
            | "ushort"
            | "nint"
            | "nuint"
            | "object"
            | "string"
            | "dynamic"
            | "Task"
            | "ValueTask"
    );
    (!keyword && !t.ends_with(']') && !t.starts_with('(')).then_some(t)
}

/// The simple type name of a `this` parameter in a signature
/// (`AddX(this IServiceCollection services)` → `IServiceCollection`).
fn this_parameter_type(signature: &str) -> Option<String> {
    let rest = signature.split_once("(this ")?.1;
    let mut words = rest.split_whitespace().skip_while(|w| matches!(*w, "ref" | "in" | "scoped" | "readonly"));
    let ty = words.next()?;
    let ty = hooks::normalize_type_text(ty);
    let simple = ty.rsplit('.').next().unwrap_or(ty);
    (!simple.is_empty()).then(|| simple.to_string())
}

/// Arity range of a C# callable from its signature: parameters with a
/// default value are optional, a `params` array makes it variadic.
pub(crate) fn signature_arity(sig: &str, name: &str) -> Option<(u32, Option<u32>)> {
    let params = hooks::signature_param_list(sig, name)?;
    let (mut min, mut max, mut variadic) = (0u32, 0u32, false);
    for p in hooks::split_param_list(params) {
        if p.starts_with("params ") || p.contains(" params ") {
            variadic = true;
            continue;
        }
        max += 1;
        if !hooks::has_default(p) {
            min += 1;
        }
    }
    Some((min, if variadic { None } else { Some(max) }))
}

/// A parameter type named like a type parameter (`T`, `T0`, `TKey`): it
/// accepts any argument (the method's or type's generic parameters are
/// not known from the suffix).
fn generic_like(t: &str) -> bool {
    let b = t.as_bytes();
    b.first() == Some(&b'T') && (b.len() == 1 || b[1].is_ascii_uppercase() || b[1].is_ascii_digit())
}

/// Numeric keyword types by implicit-conversion rank (C# §10.2.3: a lower
/// rank converts to a higher one; `decimal` takes the integral types only).
fn numeric_rank(t: &str) -> Option<u8> {
    Some(match t {
        "sbyte" | "byte" => 0,
        "short" | "ushort" | "char" => 1,
        "int" | "uint" => 2,
        "long" | "ulong" => 3,
        "float" => 4,
        "double" => 5,
        "decimal" => 6,
        _ => return None,
    })
}

/// Types a `string` argument converts to without a user conversion.
const STRING_TARGETS: &[&str] = &[
    "string",
    "object",
    "IEnumerable",
    "IComparable",
    "IConvertible",
    "ICloneable",
    "IEquatable",
    "ReadOnlySpan",
    "FormattableString",
    "IFormattable",
];

/// Whether an argument of static type `arg` may bind parameter `param`
/// (both normalized; `false` only when it certainly cannot).
fn csharp_accepts(param: &str, arg: &str) -> bool {
    let param = param.strip_suffix("...").unwrap_or(param);
    if param == arg || generic_like(param) || matches!(param, "object" | "dynamic") {
        return true;
    }
    if param == "bool" || arg == "bool" {
        return false;
    }
    match (numeric_rank(param), numeric_rank(arg)) {
        (Some(p), Some(a)) => p >= a && arg != "decimal" && !(param == "decimal" && matches!(arg, "float" | "double")),
        // A class or string argument never converts to a numeric parameter.
        (Some(_), None) => false,
        // Boxing: a number to `object`, an interface, `ValueType`, …
        (None, Some(_)) => param != "string",
        (None, None) if arg == "string" => STRING_TARGETS.contains(&param),
        (None, None) => param != "string",
    }
}

/// Whether `arg` is exactly the parameter type (an array for a `params`
/// parameter included).
fn csharp_exact(param: &str, arg: &str) -> bool {
    param == arg || param.strip_suffix("...").is_some_and(|e| arg.strip_suffix("[]") == Some(e))
}

/// The parameter argument `i` binds (the `params` element past the end).
fn param_for(params: &[String], i: usize) -> Option<&str> {
    params.get(i).or_else(|| params.last().filter(|p| p.ends_with("..."))).map(String::as_str)
}

/// How well argument `i` (static type `arg`, if known) fits `params`: an
/// identical type 3, a type parameter 2 (an identity conversion for any
/// argument), another accepted type 1; for an unknown argument a type
/// parameter 2, `object` or a `params` element 0 (boxing, an expanded
/// form), another type 1.
fn fit(params: &[String], i: usize, arg: Option<&str>) -> u32 {
    let Some(q) = param_for(params, i) else { return 0 };
    match arg {
        Some(a) if csharp_exact(q, a) => 3,
        _ if generic_like(q.trim_end_matches("...")) => 2,
        Some(_) => 1,
        None if q.ends_with("...") || q == "object" => 0,
        None => 1,
    }
}

/// C# overload choice among candidates that fit the call's arity (C#
/// §12.6.4, approximated): those that accept every known argument type,
/// then those whose parameters fit the arguments best ([`fit`], summed
/// over the arguments): an exact type over a type parameter over a
/// conversion, and for an argument of unknown type a type parameter over
/// a specific type over `object` or a `params` expansion. Every
/// candidate when none accepts the arguments.
pub(crate) fn choose_overloads(
    typed: &[(NodeId, Vec<String>)],
    arity: Option<u32>,
    args: Option<&[&str]>,
) -> Vec<NodeId> {
    let args = args.unwrap_or(&[]);
    let known = |i: usize| args.get(i).copied().filter(|a| *a != "?");
    let accepts = |p: &[String]| {
        (0..args.len()).all(|i| match (known(i), param_for(p, i)) {
            (Some(a), Some(q)) => csharp_accepts(q, a),
            _ => true,
        })
    };
    let cands: Vec<&(NodeId, Vec<String>)> = typed.iter().filter(|(_, p)| accepts(p)).collect();
    if cands.is_empty() {
        return typed.iter().map(|(n, _)| *n).collect();
    }
    let n = arity.map_or(args.len(), |a| a as usize).max(args.len());
    let score = |p: &[String]| (0..n).map(|i| fit(p, i, known(i))).sum::<u32>();
    let best = cands.iter().map(|(_, p)| score(p)).max().unwrap_or(0);
    cands.into_iter().filter(|(_, p)| score(p) == best).map(|(n, _)| *n).collect()
}

/// The enclosing namespaces of `ns`, innermost first, without `ns` itself
/// and without the global namespace (`A.B.C` → `A.B`, `A`).
fn outer_namespaces(ns: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur = ns;
    while let Some((head, _)) = cur.rsplit_once('.') {
        out.push(head);
        cur = head;
    }
    out
}

impl<'a> Resolver<'a> {
    pub(crate) fn is_csharp(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::CSharp)
    }

    fn namespace_index(&self) -> &HashMap<String, u32> {
        self.csharp.namespaces.get_or_init(|| {
            let mut m = HashMap::new();
            for (p, members) in self.packages.iter().enumerate() {
                let first = members[0];
                if self.is_csharp(first) && self.packs[first as usize].is_some_and(|k| k.namespace_imports()) {
                    m.insert(self.module_names[first as usize].clone(), p as u32);
                }
            }
            m
        })
    }

    /// The representative file of namespace `ns` (its first file).
    fn namespace_file(&self, ns: &str) -> Option<FileId> {
        self.namespace_index().get(ns).map(|&p| self.packages[p as usize][0])
    }

    /// The `Folder` node of the directory holding most files of package `p`
    /// (the first such directory in path order on ties).
    fn package_folder(&self, p: u32) -> Option<NodeId> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for &m in &self.packages[p as usize] {
            *counts.entry(parent_dir(&self.files[m as usize].path).unwrap_or("")).or_default() += 1;
        }
        let best = counts.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))?;
        self.folder_node(best.0)
    }

    /// The namespace a using names: its module, or for an alias of a
    /// namespace (`using X = A.B;` recorded as `A` + `B`) the full path.
    fn using_namespace(&self, imp: &Import) -> Option<String> {
        let first = imp.names.first();
        match first {
            Some(n) if n.alias.is_some() => {
                let full = format!("{}.{}", imp.module, n.name);
                Some(if self.namespace_index().contains_key(&full) { full } else { imp.module.clone() })
            }
            Some(_) => Some(imp.module.clone()),
            None if imp.module_alias.is_some() => Some(imp.module.clone()),
            None => None,
        }
    }

    /// Import targets of a [`LanguagePack::namespace_imports`] file: the
    /// namespace each using names, as its first file and its main folder.
    pub(crate) fn csharp_namespace_import_targets(&self, fid: FileId) -> (Vec<Option<FileId>>, Vec<Option<NodeId>>) {
        let imports = self.imports(fid);
        let (mut t, mut d) = (vec![None; imports.len()], vec![None; imports.len()]);
        for (k, imp) in imports.iter().enumerate() {
            let Some(ns) = self.using_namespace(imp) else { continue };
            if let Some(&p) = self.namespace_index().get(&ns) {
                t[k] = Some(self.packages[p as usize][0]);
                d[k] = self.package_folder(p);
            }
        }
        (t, d)
    }

    /// The project directory of each file: the nearest directory above it
    /// with a `*.csproj` (`None` = no project: the repository).
    fn project_dirs(&self) -> &Vec<Option<String>> {
        self.csharp.projects.get_or_init(|| {
            let dirs: HashSet<&str> = self
                .project
                .paths
                .iter()
                .filter(|p| p.ends_with(".csproj"))
                .map(|p| parent_dir(p).unwrap_or(""))
                .collect();
            self.files
                .iter()
                .map(|f| {
                    let mut cur = parent_dir(&f.path);
                    while let Some(d) = cur {
                        if dirs.contains(d) {
                            return Some(d.to_string());
                        }
                        cur = if d.is_empty() { None } else { Some(parent_dir(d).unwrap_or("")) };
                    }
                    None
                })
                .collect()
        })
    }

    /// The names a C# file's usings bind (see the module docs): explicit
    /// aliases and `using static` members override namespace usings; the
    /// project's `global using`s come last.
    pub(crate) fn csharp_env(&self, fid: FileId) -> Env {
        let mut stars = Env::new();
        let mut explicit = Env::new();
        for (k, imp) in self.imports(fid).iter().enumerate() {
            if !imp.reexport {
                self.bind_using(fid, fid, k, &mut stars, &mut explicit);
            }
        }
        let mut globals = Env::new();
        let mut global_explicit = Env::new();
        let project = &self.project_dirs()[fid as usize];
        for g in 0..self.files.len() as FileId {
            if !self.is_csharp(g) || self.project_dirs()[g as usize] != *project {
                continue;
            }
            for (k, imp) in self.imports(g).iter().enumerate() {
                if imp.reexport {
                    self.bind_using(fid, g, k, &mut globals, &mut global_explicit);
                }
            }
        }
        let mut env = explicit;
        for (n, t) in stars.into_iter().chain(global_explicit).chain(globals) {
            env.entry(n).or_insert(t);
        }
        env
    }

    /// Binds the `k`-th using of file `g` (seen from file `f`).
    fn bind_using(&self, f: FileId, g: FileId, k: usize, stars: &mut Env, explicit: &mut Env) {
        let imp = &self.imports(g)[k];
        let target = self.import_targets[g as usize][k];
        if let Some(a) = &imp.module_alias {
            explicit.insert(a.clone(), Some(target.map_or(Target::External, Target::Module)));
            return;
        }
        let Some(n) = imp.names.first() else { return };
        if n.name == "*" {
            if let Some(t) = target {
                for (name, b) in self.star_exports(t, None) {
                    stars.entry(name).or_insert(b);
                }
            }
            return;
        }
        if let Some(alias) = &n.alias {
            let full = format!("{}.{}", imp.module, n.name);
            let bound = match target {
                Some(t) if self.module_names[t as usize] == full => Target::Module(t),
                Some(t) => self.lookup_in_module(t, &n.name, &mut Vec::new()).unwrap_or(Target::External),
                None => Target::External,
            };
            explicit.insert(alias.clone(), Some(bound));
            return;
        }
        self.bind_static_member(f, n, target, explicit);
    }

    /// `T.name` from `using static`: a project member wins over a library one.
    fn bind_static_member(&self, f: FileId, n: &ImportedName, target: Option<FileId>, explicit: &mut Env) {
        let Some((_, member)) = n.name.rsplit_once('.') else { return };
        if member == "*" {
            return;
        }
        let bound = target.and_then(|t| self.imported_member(f, t, &n.name, false, &mut Vec::new()));
        match bound {
            Some(Target::Symbol(s)) => {
                explicit.insert(member.to_string(), Some(Target::Symbol(s)));
            }
            _ => {
                explicit.entry(member.to_string()).or_insert(Some(Target::External));
            }
        }
    }

    /// Top-level definition `name` of namespace `ns` (all its files).
    fn namespace_def(&self, ns: &str, name: &str) -> Option<NodeId> {
        self.namespace_file(ns).and_then(|rep| self.module_def(rep, name))
    }

    /// A name in scope at file level: the file's namespace, the enclosing
    /// namespaces, the usings, the global namespace.
    fn csharp_name(&self, f: FileId, name: &str) -> Option<(Target, Strategy, f32)> {
        if let Some(n) = self.module_def(f, name) {
            return Some((Target::Symbol(n), Strategy::SameModule, 0.9));
        }
        let ns = self.module_names[f as usize].as_str();
        for outer in outer_namespaces(ns) {
            if let Some(n) = self.namespace_def(outer, name) {
                return Some((Target::Symbol(n), Strategy::ImportResolved, OUTER_CONFIDENCE));
            }
        }
        if let Some(Some(t)) = self.envs[f as usize].get(name) {
            return Some((t.clone(), Strategy::ImportResolved, 0.95));
        }
        if !ns.is_empty()
            && let Some(n) = self.namespace_def("", name)
        {
            return Some((Target::Symbol(n), Strategy::ImportResolved, OUTER_CONFIDENCE));
        }
        None
    }

    /// Strategies 3 / 4 for a bare C# name (see [`Self::csharp_name`]).
    pub(crate) fn csharp_scope_name(&self, f: FileId, item: &RefItem<'_>) -> Option<Outcome> {
        let (t, s, c) = self.csharp_name(f, item.name)?;
        Some(match t {
            Target::Symbol(n) => Outcome::Hit(n, s, c),
            _ => Outcome::External,
        })
    }

    /// A C# reference with a receiver (after `this`): `$type:` and `$ret:`
    /// receivers, `base`, dotted paths, else the unique-method rung.
    pub(crate) fn csharp_with_receiver(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Outcome {
        if let Some(ty) = receiver.strip_prefix(TYPE_RECEIVER_PREFIX) {
            return self.csharp_typed(f, item, scope, ty);
        }
        if let Some(spec) = receiver.strip_prefix(rust::RET_RECEIVER_PREFIX) {
            return self.csharp_resolve_ret(f, item, scope, spec);
        }
        if receiver == "base" {
            return self.super_member(f, scope, item.name, Want::of(item.kind));
        }
        self.csharp_dotted(f, item, scope, receiver).unwrap_or_else(|| self.unique_name(f, item.kind, item.name, true))
    }

    /// A dotted C# receiver: its head as a name in scope, else a namespace
    /// path (absolute or relative to an enclosing namespace), then the
    /// remaining segments as types and the member. Library namespace heads
    /// are external. `None` falls through to the unique-method rung.
    pub(crate) fn csharp_dotted(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Option<Outcome> {
        let segs: Vec<&str> = receiver.split('.').collect();
        if !segs.iter().all(|s| is_ident(s)) {
            return None;
        }
        if let Some((start, _, _)) = self.csharp_name(f, segs[0])
            && let Some(hit) = self.member_of_import(f, item, &start, &segs[1..])
        {
            return Some(hit);
        }
        if let Some(hit) = self.namespace_path(f, item, &segs) {
            return Some(hit);
        }
        // A member of the enclosing type declared in another file (a
        // partial part, a base class): its declared type, then the rest.
        if self.enclosing_type(f, scope).is_some_and(|t| self.find_member(t, segs[0], Want::Value).is_some()) {
            return Some(self.resolve_member_chain(f, item, scope, receiver));
        }
        self.packs[f as usize].is_some_and(|p| p.is_builtin(segs[0])).then_some(Outcome::External)
    }

    /// The longest prefix of `segs` that is a project namespace (relative to
    /// the file's namespace and its enclosing ones, then absolute), with the
    /// rest walked from it.
    fn namespace_path(&self, f: FileId, item: &RefItem<'_>, segs: &[&str]) -> Option<Outcome> {
        let ns = self.module_names[f as usize].as_str();
        let mut bases: Vec<&str> = Vec::new();
        if !ns.is_empty() {
            bases.push(ns);
            bases.extend(outer_namespaces(ns));
        }
        bases.push("");
        for i in (1..=segs.len()).rev() {
            let prefix = segs[..i].join(".");
            for base in &bases {
                let full = if base.is_empty() { prefix.clone() } else { format!("{base}.{prefix}") };
                if let Some(rep) = self.namespace_file(&full) {
                    return self.member_of_import(f, item, &Target::Module(rep), &segs[i..]);
                }
            }
        }
        None
    }

    /// A `$type:T` receiver in C#: the member of `T`, else an extension
    /// method for `T` (see the module docs).
    pub(crate) fn csharp_typed(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>, ty: &str) -> Outcome {
        let lookup = self.resolve_type_text(f, scope, ty);
        self.csharp_member(f, item, lookup, ty)
    }

    /// A `$ret:<receiver>#<name>` receiver (`?` = awaited): the type the
    /// callee's signature declares it returns, then the member as for a
    /// `$type:` receiver. A callee found by name alone makes the type a
    /// guess (`heuristic`).
    pub(crate) fn csharp_resolve_ret(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        spec: &str,
    ) -> Outcome {
        let depth = self.csharp.ret_depth.get();
        if depth >= MAX_RET_NESTING {
            return self.unique_name(f, item.kind, item.name, true);
        }
        self.csharp.ret_depth.set(depth + 1);
        let typed = self.ret_type(f, scope, spec);
        self.csharp.ret_depth.set(depth);
        let (lookup, text) = typed.unwrap_or((TypeLookup::Unknown, String::new()));
        self.csharp_member(f, item, lookup, &text)
    }

    /// The type a `$ret:` spec evaluates to, with its text.
    fn ret_type(&self, f: FileId, scope: Option<LocalDefId>, spec: &str) -> Option<(TypeLookup, String)> {
        let awaited = spec.ends_with('?');
        let (recv, name) = spec.trim_end_matches('?').rsplit_once('#')?;
        let call = RefItem {
            kind: RefKind::Call,
            name,
            receiver: (!recv.is_empty()).then_some(recv),
            enclosing: scope,
            span: Span::default(),
            decorated: None,
            arity: None,
            arg_types: None,
        };
        match self.resolve_item(f, &call, scope) {
            Outcome::Hit(callee, s, _) => {
                let (lookup, text) = self.return_type(callee, awaited)?;
                let guess = matches!(s, Strategy::Heuristic | Strategy::UniqueName);
                Some((if guess { lookup.guessed() } else { lookup }, text))
            }
            Outcome::External => Some((TypeLookup::External, String::new())),
            Outcome::Miss => None,
        }
    }

    /// The declared return type of C# callable `callee`, resolved in its file.
    fn return_type(&self, callee: NodeId, awaited: bool) -> Option<(TypeLookup, String)> {
        let node = &self.nodes[callee as usize];
        if node.language != Some(Language::CSharp) {
            return None;
        }
        let file = node.file?;
        let Some(ty) = value_type(return_text(node.signature.as_deref()?, &node.name)?, awaited) else {
            return Some((TypeLookup::External, String::new()));
        };
        let j = self.def_node[file as usize].iter().position(|&x| x == callee)?;
        let scope = self.parents[file as usize][j];
        Some((self.resolve_type_text(file, scope, &ty), ty))
    }

    /// Member `item` of a receiver typed `lookup` (written `ty`): the
    /// member, else an extension method; a missing member of an open type
    /// or a library type is external; an unknown type falls back to the
    /// unique-method rung.
    fn csharp_member(&self, f: FileId, item: &RefItem<'_>, lookup: TypeLookup, ty: &str) -> Outcome {
        let found = match lookup {
            TypeLookup::Found(t) | TypeLookup::ByName(t) => Some(t),
            _ => None,
        };
        if let Some(t) = found
            && let Some((n, _)) = self.find_member(t, item.name, Want::of(item.kind))
        {
            return lookup.member_hit(n);
        }
        if item.kind == RefKind::Call
            && !ty.is_empty()
            && let Some(hit) = self.extension_method(item.name, ty, found)
        {
            return hit;
        }
        match lookup {
            TypeLookup::Found(t) | TypeLookup::ByName(t) if self.type_is_open(t) => Outcome::External,
            TypeLookup::Found(_) | TypeLookup::ByName(_) => Outcome::Miss,
            TypeLookup::External => Outcome::External,
            TypeLookup::Unknown => self.unique_name(f, item.kind, item.name, true),
        }
    }

    fn extension_index(&self) -> &HashMap<String, Vec<(NodeId, String)>> {
        self.csharp.extensions.get_or_init(|| {
            let mut m: HashMap<String, Vec<(NodeId, String)>> = HashMap::new();
            for (i, n) in self.nodes.iter().enumerate() {
                if n.label != Label::Method || n.language != Some(Language::CSharp) || n.is_test {
                    continue;
                }
                if let Some(ty) = n
                    .signature
                    .as_deref()
                    .filter(|s| s.contains(" static ") || s.starts_with("static "))
                    .and_then(this_parameter_type)
                {
                    m.entry(n.name.clone()).or_default().push((i as NodeId, ty));
                }
            }
            m
        })
    }

    /// The simple names a receiver of type `ty` (resolved to `found`, if a
    /// project type) answers to: its own and its resolved bases'.
    fn receiver_type_names(&self, ty: &str, found: Option<NodeId>) -> HashSet<String> {
        let t = hooks::normalize_type_text(ty);
        let mut names: HashSet<String> = HashSet::from([t.rsplit('.').next().unwrap_or(t).to_string()]);
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<(NodeId, usize)> = found.into_iter().map(|t| (t, 0)).collect();
        while let Some((c, depth)) = stack.pop() {
            if depth > MAX_DEPTH || !seen.insert(c) {
                continue;
            }
            names.insert(self.nodes[c as usize].name.clone());
            for &b in self.bases.get(&c).map_or(&[][..], |v| v.as_slice()) {
                stack.push((b, depth + 1));
            }
        }
        names
    }

    /// The extension method `name` for a receiver of type `ty`, when every
    /// candidate is in one static class.
    fn extension_method(&self, name: &str, ty: &str, found: Option<NodeId>) -> Option<Outcome> {
        let cands = self.extension_index().get(name)?;
        let names = self.receiver_type_names(ty, found);
        let fitting: Vec<NodeId> = cands.iter().filter(|(_, t)| names.contains(t)).map(|(n, _)| *n).collect();
        let first = *fitting.first()?;
        let owner = |n: NodeId| self.nodes[n as usize].qualified_name.rsplit_once('.').map(|(o, _)| o.to_string());
        let one_class = fitting.iter().all(|&n| owner(n) == owner(first));
        one_class.then_some(Outcome::Hit(first, Strategy::TypeResolved, 0.85))
    }

    /// Partial types: the parts of a C# type share members and bases.
    pub(crate) fn csharp_partial_types(&mut self) {
        let mut groups: BTreeMap<&str, Vec<NodeId>> = BTreeMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.language == Some(Language::CSharp) && n.label.is_type() {
                groups.entry(n.qualified_name.as_str()).or_default().push(i as NodeId);
            }
        }
        let parts: Vec<Vec<NodeId>> = groups.into_values().filter(|g| g.len() > 1).collect();
        for group in parts {
            self.merge_parts(&group);
        }
    }

    /// The members of the parts of a partial type: (name, member, label).
    fn part_members(&self, group: &[NodeId]) -> Vec<(String, NodeId, Label)> {
        let mut out = Vec::new();
        for (name, slots) in group.iter().flat_map(|n| self.members.get(n).into_iter().flatten()) {
            let found = [slots.callable, slots.ty, slots.value].into_iter().flatten();
            out.extend(found.map(|m| (name.clone(), m, self.nodes[m as usize].label)));
        }
        out
    }

    /// The resolved bases of the parts of a partial type (the parts left out).
    fn part_bases(&self, group: &[NodeId]) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::new();
        for &b in group.iter().flat_map(|n| self.bases.get(n).map_or(&[][..], |v| v.as_slice())) {
            if !out.contains(&b) && !group.contains(&b) {
                out.push(b);
            }
        }
        out
    }

    fn merge_parts(&mut self, group: &[NodeId]) {
        let members = self.part_members(group);
        let bases = self.part_bases(group);
        let open = group.iter().any(|n| self.open_types.contains(n));
        for &n in group {
            for (name, m, label) in &members {
                self.add_member(n, name, *m, *label);
            }
            let v = self.bases.entry(n).or_default();
            v.extend(bases.iter().copied().filter(|b| !v.contains(b)).collect::<Vec<_>>());
            if open {
                self.open_types.insert(n);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        assert_eq!(
            this_parameter_type("public static IServiceCollection AddX(this IServiceCollection s, int n)").as_deref(),
            Some("IServiceCollection")
        );
        assert_eq!(this_parameter_type("static void F(this ref Span<int> s)").as_deref(), Some("Span"));
        assert_eq!(this_parameter_type("static void F(System.IO.Stream s)"), None);
        assert_eq!(signature_arity("public void Add(T item, int n = 0, params object[] rest)", "Add"), Some((1, None)));
        assert_eq!(signature_arity("void M()", "M"), Some((0, Some(0))));
        assert_eq!(signature_arity("void M(int a, string b = \"x\")", "M"), Some((1, Some(2))));
        assert_eq!(outer_namespaces("A.B.C"), ["A.B", "A"]);
        assert!(outer_namespaces("A").is_empty());
        assert_eq!(return_text("public async Task<Order> GetAsync(int id)", "GetAsync"), Some("Task<Order>"));
        assert_eq!(return_text("public static Query From(string t)", "From"), Some("Query"));
        assert_eq!(return_text("T Map<T>(object o)", "Map"), Some("T"));
        assert_eq!(return_text("public Order(int id)", "Order"), None);
        assert_eq!(value_type("Task<Order?>", true).as_deref(), Some("Order"));
        assert_eq!(value_type("Task<Order>", false).as_deref(), Some("Task<Order>"));
        assert_eq!(value_type("Task", true), None);
        assert_eq!(value_type("int", false), None);
        assert_eq!(value_type("Item[]", false), None);
    }

    #[test]
    fn overloads_by_csharp_conversions() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let set = vec![
            (1, t(&["LogEventLevel", "string", "T0", "T1"])),
            (2, t(&["LogEventLevel", "Exception", "string", "T"])),
            (3, t(&["LogEventLevel", "string", "object..."])),
        ];
        assert_eq!(choose_overloads(&set, Some(4), Some(&["?", "string", "?", "?"])), [1]);
        let set = vec![(1, t(&["Exception", "string", "T"])), (2, t(&["Exception", "string", "object..."]))];
        assert_eq!(choose_overloads(&set, Some(3), Some(&["Exception", "string", "object[]"])), [2]);
        let set = vec![(1, t(&["int"])), (2, t(&["string"]))];
        assert_eq!(choose_overloads(&set, Some(1), Some(&["int"])), [1]);
        assert_eq!(choose_overloads(&set, Some(1), Some(&["string"])), [2]);
        assert_eq!(choose_overloads(&set, Some(1), Some(&["long"])), [1, 2]);
        assert!(csharp_accepts("long", "int") && !csharp_accepts("int", "long") && !csharp_accepts("string", "Foo"));
        assert!(csharp_accepts("object...", "int") && csharp_exact("object...", "object[]"));
    }
}
