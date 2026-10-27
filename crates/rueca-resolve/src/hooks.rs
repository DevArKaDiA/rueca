//! Resolver support for the RFC 0003 §9.2 / §10.1 contract hooks: package-
//! and directory-scoped modules, directory imports, typed (`$type:T`) and
//! normalized (`$self`) receivers, implicit self, star imports of packages
//! and C/C++ includes, declarations vs bodies, overloads and `OVERRIDES`.
//!
//! Every rule here is keyed on a hook a pack opts into or on facts a pack
//! chooses to emit (`$type:` receivers, argument types in names): Python,
//! ECMAScript and PHP use typed receivers too, the other hooks are for the
//! RFC 0003 languages.

use super::*;

/// Confidence multiplier when a reference links several overloads.
const OVERLOAD_FANOUT: f32 = 0.8;

/// The candidates of an ambiguous overload choice without `caller`, unless
/// it is the only one: see [`Resolver::expand_overloads`].
fn without_caller(cands: &[NodeId], caller: NodeId) -> Vec<NodeId> {
    let others: Vec<NodeId> = cands.iter().copied().filter(|&n| n != caller).collect();
    if others.is_empty() { cands.to_vec() } else { others }
}

/// Splits an overload suffix off a qualified name: `Parser.parse(String)` →
/// (`Parser.parse`, `String`). `None` when the name has no suffix. A
/// trailing comment that tells apart callables with the same parameter
/// types (C/C++ `get(/*const*/)`, `write(OutputIt, T/*#2*/)`) is not part
/// of the parameters.
pub(crate) fn overload_suffix(qn: &str) -> Option<(&str, &str)> {
    let (base, params) = overload_suffix_raw(qn)?;
    let params = match params.find("/*") {
        Some(i) if params.ends_with("*/") => params[..i].trim_end(),
        _ => params,
    };
    Some((base, params))
}

fn overload_suffix_raw(qn: &str) -> Option<(&str, &str)> {
    if !qn.ends_with(')') {
        return None;
    }
    let mut depth = 0i32;
    for (i, c) in qn.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    return (i > 0).then(|| (&qn[..i], &qn[i + 1..qn.len() - 1]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Arity range of an overload parameter list: (minimum, maximum; `None` =
/// variadic). `()` and `(void)` take no arguments.
pub(crate) fn params_arity(params: &str) -> (u32, Option<u32>) {
    let p = params.trim();
    if p.is_empty() || p == "void" {
        return (0, Some(0));
    }
    let mut depth = 0i32;
    let mut count = 1u32;
    for c in p.chars() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    if p.contains("...") { (count - 1, None) } else { (count, Some(count)) }
}

/// Arity range of a C/C++ callable from its signature: the parameter list
/// that follows `name` (after any `template <…>` head). Parameters with a
/// default value are optional; `...` (C varargs or a parameter pack) makes
/// it variadic; `()` and `(void)` take none. `None` when no list is found.
pub(crate) fn signature_arity(sig: &str, name: &str) -> Option<(u32, Option<u32>)> {
    let open = param_list_open(sig, name, template_head_end(sig))?;
    let close = matching_close(sig, open)?;
    let params = sig[open + 1..close].trim();
    if params.is_empty() || params == "void" {
        return Some((0, Some(0)));
    }
    let (mut min, mut max, mut variadic) = (0u32, 0u32, false);
    for p in split_params(params) {
        if p.contains("...") {
            variadic = true;
            continue;
        }
        max += 1;
        if !has_default_value(p) {
            min += 1;
        }
    }
    Some((min, if variadic { None } else { Some(max) }))
}

/// Byte offset just after a leading `template <…>` head (0 without one).
fn template_head_end(sig: &str) -> usize {
    if !sig.starts_with("template") {
        return 0;
    }
    let mut depth = 0i32;
    for (i, c) in sig.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    0
}

/// Byte offset of the `(` that follows `name` (as a word) at or after `from`.
fn param_list_open(sig: &str, name: &str, from: usize) -> Option<usize> {
    let bytes = sig.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut at = from;
    loop {
        let k = at + sig.get(at..)?.find(name)?;
        let end = k + name.len();
        let boundary = k == 0 || !word(bytes[k - 1]) || !word(name.as_bytes()[0]);
        let rest = sig[end..].trim_start();
        if boundary && rest.starts_with('(') {
            return Some(sig.len() - rest.len());
        }
        at = end;
    }
}

/// Byte offset of the bracket closing the one at `open` (any of `()<>[]{}`).
fn matching_close(sig: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in sig[open..].char_indices() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a parameter has a default value: a top-level `=` (not `==`,
/// `<=`, `>=`, `!=`).
fn has_default_value(p: &str) -> bool {
    let mut depth = 0i32;
    let pb = p.as_bytes();
    pb.iter().enumerate().any(|(i, &c)| {
        match c {
            b'(' | b'<' | b'[' | b'{' => depth += 1,
            b')' | b'>' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        c == b'='
            && depth == 0
            && pb.get(i + 1) != Some(&b'=')
            && !matches!(i.checked_sub(1).map(|j| pb[j]), Some(b'=' | b'<' | b'>' | b'!'))
    })
}

/// The type name written in a `$type:` receiver or a base list, without
/// pointers, references, qualifiers, array and generic arguments:
/// `*http.Server` → `http.Server`, `const std::vector<int>&` → `std::vector`.
pub(crate) fn normalize_type_text(text: &str) -> &str {
    let mut t = text.trim();
    loop {
        let before = t;
        for prefix in ["const ", "volatile ", "struct ", "class ", "enum ", "union ", "typename "] {
            if let Some(rest) = t.strip_prefix(prefix) {
                t = rest.trim_start();
            }
        }
        t = t.trim_start_matches(['*', '&', '[', ']', '.']).trim_start();
        if t == before {
            break;
        }
    }
    if let Some(i) = t.find(['<', '[', '(', ' ']) {
        t = &t[..i];
    }
    t.trim_end_matches(['*', '&', '?']).trim()
}

/// The namespace path a macro definition opens: `#define FMT_BEGIN_NAMESPACE
/// namespace fmt { inline namespace v12 {` → `fmt::v12` (and `fmt`, the
/// path without inline namespaces); `Some("")` when it opens no namespace
/// (`#define FMT_BEGIN_EXPORT`, `extern "C" {`).
fn namespace_expansion(signature: &str, name: &str) -> Option<(String, String)> {
    let rest = signature.strip_prefix('#')?.trim_start().strip_prefix("define")?.trim_start().strip_prefix(name)?;
    if rest.starts_with('(') {
        return None;
    }
    let spaced = rest.replace('{', " { ");
    let words: Vec<&str> = spaced.split_whitespace().collect();
    let (mut path, mut visible) = (Vec::new(), Vec::new());
    let mut inline = false;
    let mut i = 0;
    while i < words.len() {
        match words[i] {
            "inline" => {
                inline = true;
                i += 1;
            }
            "export" => i += 1,
            "namespace" if words.get(i + 2) == Some(&"{") => {
                let segs = words[i + 1].split("::").filter(|s| !s.is_empty());
                for seg in segs {
                    path.push(seg);
                    if !inline {
                        visible.push(seg);
                    }
                }
                inline = false;
                i += 3;
            }
            _ => return Some((String::new(), String::new())),
        }
    }
    Some((path.join("::"), visible.join("::")))
}

/// C/C++ namespace macros (before nodes are built): a pack writes the
/// definitions inside a masked `FMT_BEGIN_NAMESPACE` … `FMT_END_NAMESPACE`
/// region with the segment `${FMT_BEGIN_NAMESPACE}`, because the macro is
/// usually defined in another file. Every such segment, in qualified names
/// and receivers, becomes the namespaces the macro's definition opens
/// anywhere in the snapshot (`fmt::v12`), or goes when the definition is
/// unknown or opens none.
///
/// Returns the (path, path without inline namespaces) pairs that differ
/// (`fmt::v12`, `fmt`): exact qualified-name lookups see through inline
/// namespaces, as C++ does.
pub(crate) fn expand_namespace_macros(files: &mut [FileInput]) -> Vec<(String, String)> {
    let uses_macros = files
        .iter()
        .filter(|f| is_c_file(f))
        .filter_map(|f| f.facts.as_ref())
        .any(|x| x.definitions.iter().any(|d| d.qualified_name.contains("${")));
    if !uses_macros {
        return Vec::new();
    }
    let (map, aliases) = namespace_macro_paths(files);
    for facts in files.iter_mut().filter(|f| is_c_file(f)).filter_map(|f| f.facts.as_mut()) {
        for d in &mut facts.definitions {
            if d.qualified_name.contains("${") {
                d.qualified_name = rewrite_macro_segments(&d.qualified_name, &map);
            }
        }
        for r in &mut facts.references {
            if let Some(recv) = r.receiver.as_mut().filter(|r| r.contains("${")) {
                *recv = rewrite_macro_segments(recv, &map);
            }
        }
    }
    aliases
}

fn is_c_file(f: &FileInput) -> bool {
    f.language.map(Language::family) == Some(LanguageFamily::C)
}

/// The namespace path each namespace macro opens (first non-empty
/// definition wins), and the (path, visible path) pairs that differ.
fn namespace_macro_paths(files: &[FileInput]) -> (BTreeMap<String, String>, Vec<(String, String)>) {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    let constants = files
        .iter()
        .filter(|f| is_c_file(f))
        .flat_map(|f| f.facts.iter().flat_map(|x| x.definitions.iter()))
        .filter(|d| d.label == Label::Constant);
    for d in constants {
        let Some(sig) = d.signature.as_deref() else { continue };
        let Some((path, visible)) = namespace_expansion(sig, &d.name) else { continue };
        let e = map.entry(d.name.clone()).or_default();
        if !e.is_empty() || path.is_empty() {
            continue;
        }
        if visible != path && !aliases.iter().any(|(p, _)| *p == path) {
            aliases.push((path.clone(), visible));
        }
        *e = path;
    }
    (map, aliases)
}

/// Replaces every `${MACRO}` segment of a name by the path the macro
/// opens, or drops it (and its `::` separator) when it opens none.
fn rewrite_macro_segments(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else { break };
        let name = &rest[start + 2..start + len];
        let path = map.get(name).map_or("", String::as_str);
        out.push_str(&rest[..start]);
        let mut after = &rest[start + len + 1..];
        if path.is_empty() {
            // Drop the segment and its separator.
            after = after.strip_prefix("::").unwrap_or_else(|| {
                if out.ends_with("::") {
                    out.truncate(out.len() - 2);
                }
                after
            });
        } else {
            out.push_str(path);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Splits a qualified type text on its last `::` or `.` separator.
fn split_qualified(t: &str) -> Option<(&str, &str)> {
    let dot = t.rfind('.').map(|i| (i, 1));
    let colons = t.rfind("::").map(|i| (i, 2));
    let (i, len) = match (dot, colons) {
        (Some(a), Some(b)) => a.max(b),
        (a, b) => a.or(b)?,
    };
    let (q, n) = (&t[..i], &t[i + len..]);
    (!q.is_empty() && !n.is_empty()).then_some((q, n))
}

/// Names a star import of a file binds, with their definitions.
pub(crate) type StarNames = std::rc::Rc<Vec<(String, NodeId)>>;

/// Outcome of resolving a type name.
pub(crate) enum TypeLookup {
    Found(NodeId),
    External,
    Unknown,
}

/// Deepest macro-in-macro nesting followed by [`Resolver::expand_macro_calls`].
const MAX_MACRO_DEPTH: usize = 8;

/// Confidence multiplier of a call reached through a macro expansion.
const MACRO_EXPANSION: f32 = 0.95;

impl<'a> Resolver<'a> {
    /// Whether node `n` is a C/C++ macro definition (`#define`).
    fn is_macro(&self, n: NodeId) -> bool {
        let node = &self.nodes[n as usize];
        matches!(node.label, Label::Function | Label::Constant)
            && node.language.map(Language::family) == Some(LanguageFamily::C)
            && node.signature.as_deref().is_some_and(|s| s.trim_start().starts_with('#'))
    }

    /// C/C++ macro expansion: a pack reports the calls a macro's body makes
    /// as calls of the macro's definition (`#define lua_pop(L,n)
    /// lua_settop(L, -(n)-1)` calls `lua_settop`), and every use of the
    /// macro by another definition — a `CALLS` edge (a lower-case macro
    /// called like a function) or a `REFERENCES` edge (a macro invocation)
    /// — also calls those targets, through nested macros (depth ≤
    /// [`MAX_MACRO_DEPTH`]), as the code after preprocessing does. The edge
    /// keeps the use's strategy and call site; its confidence is the
    /// product of both edges' × [`MACRO_EXPANSION`].
    pub(crate) fn expand_macro_calls(&mut self) {
        let mut direct: BTreeMap<NodeId, Vec<(NodeId, f32)>> = BTreeMap::new();
        for (&(src, kind, dst), meta) in &self.edges {
            if kind == EdgeKind::Calls && src != dst && self.is_macro(src) {
                direct.entry(src).or_default().push((dst, meta.confidence));
            }
        }
        if direct.is_empty() {
            return;
        }
        let uses: Vec<(NodeId, NodeId, EdgeMeta)> = self
            .edges
            .iter()
            .filter(|((src, kind, dst), _)| {
                matches!(kind, EdgeKind::Calls | EdgeKind::References)
                    && direct.contains_key(dst)
                    && !self.is_macro(*src)
            })
            .map(|(&(src, _, dst), meta)| (src, dst, *meta))
            .collect();
        let mut closures: HashMap<NodeId, Vec<(NodeId, f32)>> = HashMap::new();
        for (src, m, meta) in uses {
            let targets = closures.entry(m).or_insert_with(|| self.macro_closure(m, &direct)).clone();
            for &(target, conf) in &targets {
                let confidence = meta.confidence * conf * MACRO_EXPANSION;
                self.add_edge(src, target, EdgeKind::Calls, EdgeMeta { confidence, ..meta });
            }
        }
    }

    /// Definitions that are not macros reached from macro `m` through the
    /// calls of its body and of the macros it uses, with the best product
    /// of confidences, sorted by node.
    fn macro_closure(&self, m: NodeId, direct: &BTreeMap<NodeId, Vec<(NodeId, f32)>>) -> Vec<(NodeId, f32)> {
        let mut best: BTreeMap<NodeId, f32> = BTreeMap::new();
        let mut seen: HashSet<NodeId> = HashSet::from([m]);
        let mut level: Vec<(NodeId, f32)> = vec![(m, 1.0)];
        for _ in 0..MAX_MACRO_DEPTH {
            level = self.macro_level(&level, direct, &mut seen, &mut best);
            if level.is_empty() {
                break;
            }
        }
        best.into_iter().collect()
    }

    /// One level of [`Self::macro_closure`]: the targets of the macros of
    /// `level` go to `best`, the macros not seen yet to the next level.
    fn macro_level(
        &self,
        level: &[(NodeId, f32)],
        direct: &BTreeMap<NodeId, Vec<(NodeId, f32)>>,
        seen: &mut HashSet<NodeId>,
        best: &mut BTreeMap<NodeId, f32>,
    ) -> Vec<(NodeId, f32)> {
        let mut next = Vec::new();
        for &(n, c) in level {
            for &(t, tc) in direct.get(&n).map(Vec::as_slice).unwrap_or_default() {
                let conf = c * tc;
                if !self.is_macro(t) {
                    let e = best.entry(t).or_insert(0.0);
                    *e = e.max(conf);
                } else if seen.insert(t) {
                    next.push((t, conf));
                }
            }
        }
        next
    }

    // -- packages ------------------------------------------------------------

    pub(crate) fn scope_of(&self, f: FileId) -> ModuleScope {
        match (self.packs[f as usize], self.files[f as usize].language) {
            (Some(p), Some(l)) => p.module_scope(l),
            _ => ModuleScope::File,
        }
    }

    /// Groups package- and directory-scoped files into packages.
    pub(crate) fn build_packages(&mut self) {
        let mut groups: BTreeMap<(u8, String, String), Vec<FileId>> = BTreeMap::new();
        for (i, f) in self.files.iter().enumerate() {
            let fid = i as FileId;
            let (Some(lang), Some(family)) = (f.language, self.families[i]) else { continue };
            let key = match self.scope_of(fid) {
                ModuleScope::File => continue,
                ModuleScope::Package => (0, format!("{family:?}"), self.module_names[i].clone()),
                ModuleScope::Directory => (1, lang.as_str().to_string(), parent_dir(&f.path).unwrap_or("").to_string()),
            };
            groups.entry(key).or_default().push(fid);
        }
        self.package_of = vec![None; self.files.len()];
        for (_, mut members) in groups {
            members.sort_by(|a, b| self.files[*a as usize].path.cmp(&self.files[*b as usize].path));
            let id = self.packages.len() as u32;
            for &m in &members {
                self.package_of[m as usize] = Some(id);
                let dir = parent_dir(&self.files[m as usize].path).unwrap_or("").to_string();
                let v = self.packages_by_dir.entry(dir).or_default();
                if !v.contains(&id) {
                    v.push(id);
                }
            }
            self.packages.push(members);
        }
    }

    /// Files sharing `f`'s module namespace: its package, else `f` alone.
    pub(crate) fn module_files(&self, f: FileId) -> Vec<FileId> {
        match self.package_of[f as usize] {
            Some(p) => self.packages[p as usize].clone(),
            None => vec![f],
        }
    }

    /// Top-level definition `name` of `f`'s module: `f` itself first, then
    /// the other files of its package in path order.
    pub(crate) fn module_def(&self, f: FileId, name: &str) -> Option<NodeId> {
        if let Some(n) = self.top_level_def(f, name) {
            return Some(n);
        }
        let p = self.package_of[f as usize]?;
        self.packages[p as usize].iter().filter(|&&o| o != f).find_map(|&o| self.top_level_def(o, name))
    }

    /// `Folder` node for a directory path returned by `resolve_import`
    /// (`""` / `.` = the repository).
    pub(crate) fn folder_node(&self, path: &str) -> Option<NodeId> {
        let p = path.trim_end_matches('/');
        if p.is_empty() || p == "." {
            return Some(self.repo_node);
        }
        self.folder_nodes.get(p).copied()
    }

    /// The package bound by importing directory `dir` from `importer`: a
    /// package of the importer's family whose files live directly in `dir`,
    /// preferring the one whose module name is the import specifier (Go
    /// `foo` over `foo_test`), else the largest. Represented by its first file.
    pub(crate) fn package_in_dir(&self, importer: FileId, dir: &str, module: &str) -> Option<FileId> {
        self.package_of[importer as usize]?;
        let family = self.families[importer as usize];
        let dir = dir.trim_end_matches('/');
        let dir = if dir == "." { "" } else { dir };
        let mut best: Option<(bool, usize, FileId)> = None;
        for &p in self.packages_by_dir.get(dir).map_or(&[][..], |v| v.as_slice()) {
            let members = &self.packages[p as usize];
            let first = members[0];
            if self.families[first as usize] != family {
                continue;
            }
            let in_dir = members.iter().any(|&m| parent_dir(&self.files[m as usize].path).unwrap_or("") == dir);
            if !in_dir {
                continue;
            }
            let rep = members
                .iter()
                .copied()
                .find(|&m| parent_dir(&self.files[m as usize].path).unwrap_or("") == dir)
                .unwrap_or(first);
            let cand = (self.module_names[rep as usize] == module, members.len(), rep);
            let better = match best {
                None => true,
                Some((exact, len, r)) => {
                    (cand.0, cand.1) > (exact, len)
                        || ((cand.0, cand.1) == (exact, len)
                            && self.files[rep as usize].path < self.files[r as usize].path)
                }
            };
            if better {
                best = Some(cand);
            }
        }
        best.map(|(_, _, rep)| rep)
    }

    // -- tables --------------------------------------------------------------

    /// Tables for the hooks, built once after the per-file tables.
    pub(crate) fn build_hook_tables(&mut self) {
        self.build_package_members();
        self.link_declarations();
        self.build_overload_sets();
        self.build_data_tables();
    }

    /// Package scope: a top-level `T.m` (a Go method declared outside its
    /// type, possibly in another file) is a member of type `T`.
    fn build_package_members(&mut self) {
        let mut found = Vec::new();
        for i in 0..self.files.len() {
            let fid = i as FileId;
            if self.package_of[i].is_none() {
                continue;
            }
            let defs = self.defs(fid);
            for &j in &self.top_level[i] {
                let d = &defs[j as usize];
                let Some((owner, member)) = d.qualified_name.rsplit_once('.') else { continue };
                if member != d.name || owner.contains('.') {
                    continue;
                }
                let Some(t) = self.module_def(fid, owner) else { continue };
                if !self.nodes[t as usize].label.is_type() {
                    continue;
                }
                found.push((t, d, self.def_node[i][j as usize]));
            }
        }
        for (t, d, node) in found {
            self.add_member(t, &d.name, node, d.label);
        }
    }

    /// Every node by global qualified name (built on first use).
    pub(crate) fn nodes_by_qn(&self) -> &HashMap<String, Vec<NodeId>> {
        self.by_qn.get_or_init(|| {
            let mut m: HashMap<String, Vec<NodeId>> = HashMap::new();
            for (i, n) in self.nodes.iter().enumerate() {
                m.entry(n.qualified_name.clone()).or_default().push(i as NodeId);
            }
            m
        })
    }

    /// C/C++: a declaration (`is_declaration`) whose global qualified name
    /// has exactly one body in the same family is replaced by that body as a
    /// reference target, and leaves the strategy-6 candidates.
    fn link_declarations(&mut self) {
        let decls: Vec<NodeId> = (0..self.files.len())
            .flat_map(|i| {
                let defs = self.defs(i as FileId);
                (0..defs.len()).filter(move |&j| defs[j].is_declaration).map(move |j| (i, j))
            })
            .map(|(i, j)| self.def_node[i][j])
            .collect();
        if decls.is_empty() {
            return;
        }
        let body_of = self.single_bodies(&decls);
        for v in self.by_name.values_mut() {
            v.retain(|n| !body_of.contains_key(n));
        }
        let mut declared_in: HashMap<NodeId, Vec<FileId>> = HashMap::new();
        let mut decls_of: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (&d, &b) in &body_of {
            if let Some(file) = self.nodes[d as usize].file {
                declared_in.entry(b).or_default().push(file);
            }
            decls_of.entry(b).or_default().push(d);
        }
        for v in decls_of.values_mut() {
            v.sort_unstable();
        }
        self.declared_in = declared_in;
        self.decls_of = decls_of;
        self.body_of = body_of;
    }

    /// Each declaration whose global qualified name has exactly one body
    /// (a non-declaration of the same family) → that body.
    fn single_bodies(&self, decls: &[NodeId]) -> HashMap<NodeId, NodeId> {
        let decl_set: HashSet<NodeId> = decls.iter().copied().collect();
        let mut body_of = HashMap::new();
        for &d in decls {
            let node = &self.nodes[d as usize];
            let family = node.language.map(Language::family);
            let bodies: Vec<NodeId> = self.nodes_by_qn()[&node.qualified_name]
                .iter()
                .copied()
                .filter(|&b| !decl_set.contains(&b) && self.nodes[b as usize].language.map(Language::family) == family)
                .collect();
            if let [b] = bodies[..] {
                body_of.insert(d, b);
            }
        }
        body_of
    }

    /// The body of a declaration, else the node itself.
    pub(crate) fn prefer_body(&self, n: NodeId) -> NodeId {
        self.body_of.get(&n).copied().unwrap_or(n)
    }

    /// Overloads of packs with `overloads()`: definitions whose local name
    /// ends with a parameter suffix, grouped by (family, global qualified
    /// name without the suffix). C++ callables without a suffix join the
    /// group of their name too: a pack suffixes overloads per file, so an
    /// overload set split across files (`vformat` in two headers) has bare
    /// members. Bare members alone form a set only when their arities
    /// differ (two same-name functions of different files with one
    /// signature are not overloads of each other).
    fn build_overload_sets(&mut self) {
        for (_, (set, any_suffixed)) in self.overload_groups() {
            if set.len() < 2 || (!any_suffixed && self.same_arity(&set)) {
                continue;
            }
            let id = self.overload_sets.len();
            for &n in &set {
                self.overload_of.insert(n, id);
            }
            self.overload_sets.push(set);
        }
    }

    /// Candidate overload groups by (family, name without suffix), with
    /// whether any member is suffixed.
    fn overload_groups(&self) -> BTreeMap<(String, String), (Vec<NodeId>, bool)> {
        let mut groups: BTreeMap<(String, String), (Vec<NodeId>, bool)> = BTreeMap::new();
        for i in 0..self.files.len() {
            if !self.packs[i].is_some_and(|p| p.overloads()) {
                continue;
            }
            let family = format!("{:?}", self.families[i]);
            for &n in &self.def_node[i] {
                let Some((base, suffixed)) = self.overload_base(n) else { continue };
                let (v, any_suffixed) = groups.entry((family.clone(), base.to_string())).or_default();
                *any_suffixed |= suffixed;
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
        groups
    }

    /// The name an overload set groups `n` under and whether `n` carries a
    /// suffix: suffixed definitions and bare C++ callables (not
    /// declarations with a body).
    fn overload_base(&self, n: NodeId) -> Option<(&str, bool)> {
        if self.body_of.contains_key(&n) {
            return None;
        }
        let node = &self.nodes[n as usize];
        match overload_suffix(&node.qualified_name) {
            Some((base, _)) => Some((base, true)),
            None if node.language == Some(Language::Cpp) && node.label.is_callable() => {
                Some((node.qualified_name.as_str(), false))
            }
            None => None,
        }
    }

    /// Whether every member of `set` has the same arity range.
    fn same_arity(&self, set: &[NodeId]) -> bool {
        let first = self.def_arity(set[0]);
        set.iter().all(|&n| self.def_arity(n) == first)
    }

    /// Arity range of a definition: from its C/C++ signature (default
    /// arguments are optional, `...` and parameter packs variadic), else
    /// from its overload suffix.
    fn def_arity(&self, n: NodeId) -> Option<(u32, Option<u32>)> {
        let own = self.own_arity(n)?;
        // C++ default arguments are written on the declaration, not on an
        // out-of-line body: the range covers both.
        let decls = self.decls_of.get(&n).map(Vec::as_slice).unwrap_or_default();
        Some(
            decls
                .iter()
                .filter_map(|&d| self.own_arity(d))
                .fold(own, |(lo, hi), (dlo, dhi)| (lo.min(dlo), hi.zip(dhi).map(|(a, b)| a.max(b)))),
        )
    }

    fn own_arity(&self, n: NodeId) -> Option<(u32, Option<u32>)> {
        let node = &self.nodes[n as usize];
        let from_signature = match node.language.map(Language::family) {
            Some(LanguageFamily::C) => node.signature.as_deref().and_then(|s| signature_arity(s, &node.name)),
            _ => None,
        };
        from_signature.or_else(|| overload_suffix(&node.qualified_name).map(|(_, p)| params_arity(p)))
    }

    /// Files whose declarations a C/C++ file `f` sees: itself and what it
    /// includes, transitively (depth ≤ 10). Memoized.
    fn visible_files(&self, f: FileId) -> std::rc::Rc<HashSet<FileId>> {
        if let Some(v) = self.visible_memo.borrow().get(&f) {
            return v.clone();
        }
        let mut seen: HashSet<FileId> = HashSet::from([f]);
        let mut level = vec![f];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for &t in &level {
                for &i in self.import_targets[t as usize].iter().flatten() {
                    if seen.insert(i) {
                        next.push(i);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        let v = std::rc::Rc::new(seen);
        self.visible_memo.borrow_mut().insert(f, v.clone());
        v
    }

    /// Expands a resolved target to its overloads (RFC 0003 §10.1): those
    /// whose parameter count fits the call's arity, narrowed by the static
    /// argument types when the pack reports them (see [`overload_by_types`]);
    /// one candidate keeps the confidence, several are all linked × 0.8,
    /// and no fitting one links the whole set × 0.8. A fan-out never links
    /// the calling overload itself (`caller`) when another candidate
    /// remains: an overload calling its own name with arguments of unknown
    /// type (`fromJson(String, Class)` → `fromJson(json,
    /// TypeToken.get(c))`) delegates to a sibling far more often than it
    /// recurses; a recursion chosen by arity or argument types keeps its
    /// self-loop.
    /// For a C/C++ reference of file `from`, only the overloads declared in
    /// files it sees (itself and its includes, transitively) are candidates,
    /// when there are any: C++ overload resolution only considers visible
    /// declarations.
    pub(crate) fn expand_overloads(
        &self,
        from: FileId,
        caller: NodeId,
        item: &RefItem<'_>,
        target: NodeId,
        strategy: Strategy,
        confidence: f32,
    ) -> Vec<(NodeId, Strategy, f32)> {
        let Some(&set) = self.overload_of.get(&target) else { return vec![(target, strategy, confidence)] };
        let (arity, args) = (item.arity, item.arg_types.as_deref());
        let all = self.overload_sets[set].as_slice();
        let visible_only: Vec<NodeId>;
        let set: &[NodeId] = if self.families[from as usize] == Some(LanguageFamily::C) {
            let visible = self.visible_files(from);
            let sees = |n: NodeId| {
                self.nodes[n as usize].file.is_some_and(|x| visible.contains(&x))
                    || self.declared_in.get(&n).is_some_and(|fs| fs.iter().any(|x| visible.contains(x)))
            };
            visible_only = all.iter().copied().filter(|&n| sees(n)).collect();
            if visible_only.is_empty() { all } else { &visible_only }
        } else {
            all
        };
        let fits = |n: NodeId, a: u32| match self.def_arity(n) {
            Some((min, max)) => a >= min && max.is_none_or(|m| a <= m),
            None => true,
        };
        let mut chosen: Vec<NodeId> = match arity {
            Some(a) => set.iter().copied().filter(|&n| fits(n, a)).collect(),
            None => Vec::new(),
        };
        if chosen.len() > 1
            && let Some(args) = args
        {
            let typed: Vec<(NodeId, Vec<String>)> =
                chosen.iter().filter_map(|&n| Some((n, self.param_types(n)?))).collect();
            if typed.len() == chosen.len() {
                chosen = overload_by_types(&typed, args);
            }
        }
        let fan_out = |cands: &[NodeId]| {
            without_caller(cands, caller).into_iter().map(|n| (n, strategy, confidence * OVERLOAD_FANOUT)).collect()
        };
        match chosen.len() {
            1 => vec![(chosen[0], strategy, confidence)],
            0 => fan_out(set),
            _ => fan_out(&chosen),
        }
    }

    // -- star imports --------------------------------------------------------

    /// Whether a star import (`*` without alias) from `f` binds names: Python
    /// (unchanged rule), imports of a package, and C/C++ includes.
    pub(crate) fn star_binds(&self, f: FileId, target: FileId) -> bool {
        self.package_of[target as usize].is_some() || self.families[f as usize] == Some(LanguageFamily::C)
    }

    /// Exported top-level definitions visible through a star import of
    /// `target` (its whole package; for C/C++ also, transitively, what
    /// `target` itself includes, breadth-first, each file once, depth ≤ 10),
    /// optionally only `want`. Memoized per target.
    pub(crate) fn star_exports(&self, target: FileId, want: Option<&str>) -> Vec<(String, Option<Target>)> {
        let all = self.star_closure(target);
        all.iter()
            .filter(|(name, _)| want.is_none_or(|w| w == name))
            .map(|(name, n)| (name.clone(), Some(Target::Symbol(*n))))
            .collect()
    }

    fn star_closure(&self, target: FileId) -> StarNames {
        if let Some(v) = self.star_memo.borrow().get(&target) {
            return v.clone();
        }
        let mut out = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut visited: HashSet<FileId> = HashSet::from([target]);
        let mut level = vec![target];
        for depth in 0..=MAX_DEPTH {
            let mut next = Vec::new();
            for &t in &level {
                self.exported_names(t, &mut seen, &mut out);
                if self.families[t as usize] == Some(LanguageFamily::C) && depth != MAX_DEPTH {
                    self.star_includes(t, &mut visited, &mut next);
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        let v = std::rc::Rc::new(out);
        self.star_memo.borrow_mut().insert(target, v.clone());
        v
    }

    /// Exported top-level names of `t`'s module not `seen` yet (package
    /// scope: not the `T.m` methods declared outside their type).
    fn exported_names(&self, t: FileId, seen: &mut HashSet<&'a str>, out: &mut Vec<(String, NodeId)>) {
        for m in self.module_files(t) {
            let defs = self.defs(m);
            let package_scoped = self.package_of[m as usize].is_some();
            for &j in &self.top_level[m as usize] {
                let d = &defs[j as usize];
                if !d.exported || (package_scoped && d.qualified_name != d.name) {
                    continue;
                }
                if seen.insert(d.name.as_str()) {
                    out.push((d.name.clone(), self.node_of(m, j)));
                }
            }
        }
    }

    /// The files a C/C++ file `t` star-includes, not `visited` yet.
    fn star_includes(&self, t: FileId, visited: &mut HashSet<FileId>, next: &mut Vec<FileId>) {
        for (k, imp) in self.imports(t).iter().enumerate() {
            let star = imp.names.iter().any(|n| n.name == "*" && n.alias.is_none());
            if let (true, Some(i)) = (star, self.import_targets[t as usize][k])
                && visited.insert(i)
            {
                next.push(i);
            }
        }
    }

    // -- C++ namespaces ------------------------------------------------------

    /// C/C++ nodes by global qualified name without overload suffix (built
    /// on first use).
    fn nodes_by_base_qn(&self) -> &HashMap<String, Vec<NodeId>> {
        self.by_base_qn.get_or_init(|| {
            let mut m: HashMap<String, Vec<NodeId>> = HashMap::new();
            for (i, n) in self.nodes.iter().enumerate() {
                if n.language.map(Language::family) != Some(LanguageFamily::C) || n.file.is_none() {
                    continue;
                }
                for key in self.base_qn_keys(n) {
                    m.entry(key).or_default().push(i as NodeId);
                }
            }
            m
        })
    }

    /// The global qualified name of a C/C++ node without overload suffix,
    /// also through inline namespaces: `fmt::v12::format` is also
    /// `fmt::format`.
    fn base_qn_keys(&self, n: &Node) -> Vec<String> {
        let base = overload_suffix(&n.qualified_name).map_or(n.qualified_name.as_str(), |(b, _)| b);
        let mut keys = vec![base.to_string()];
        for (path, visible) in &self.inline_aliases {
            if let Some(rest) = base.strip_prefix(path.as_str()).and_then(|r| r.strip_prefix("::")) {
                keys.push(if visible.is_empty() { rest.to_string() } else { format!("{visible}::{rest}") });
            }
        }
        keys
    }

    /// The scopes enclosing a C/C++ reference, innermost first: the
    /// qualified-name prefixes of its innermost enclosing definition
    /// (`a::b` and `a` for a reference inside `a::b::f`).
    fn enclosing_scopes(&self, f: FileId, scope: Option<LocalDefId>) -> Vec<String> {
        let Some(s) = scope else { return Vec::new() };
        let qn = &self.nodes[self.node_of(f, s) as usize].qualified_name;
        let qn = overload_suffix(qn).map_or(qn.as_str(), |(b, _)| b);
        let mut segs: Vec<&str> = Vec::new();
        let (mut depth, mut start) = (0i32, 0usize);
        let bytes = qn.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'<' | b'(' => depth += 1,
                b'>' | b')' => depth -= 1,
                b':' if depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                    segs.push(&qn[start..i]);
                    start = i + 2;
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }
        (1..=segs.len()).rev().map(|k| segs[..k].join("::")).collect()
    }

    /// The node with global qualified name `qn` (overload suffix optional),
    /// preferring one of file `f` (`same_module` 0.90, else
    /// `import_resolved` 0.95, like rungs 4 and 3).
    fn by_exact_name(&self, f: FileId, qn: &str) -> Option<Outcome> {
        let nodes = self.nodes_by_base_qn().get(qn)?;
        let n = nodes.iter().copied().find(|&n| self.nodes[n as usize].file == Some(f)).or(nodes.first().copied())?;
        Some(if self.nodes[n as usize].file == Some(f) {
            Outcome::Hit(n, Strategy::SameModule, 0.9)
        } else {
            Outcome::Hit(n, Strategy::ImportResolved, 0.95)
        })
    }

    /// Rung 1c (C family): a receiver-less name inside `a::b::f` is
    /// `a::b::name`, else `a::name` (C++ looks names up in the enclosing
    /// namespaces, innermost first).
    pub(crate) fn namespace_scope(&self, f: FileId, scope: Option<LocalDefId>, name: &str) -> Option<Outcome> {
        if self.families[f as usize] != Some(LanguageFamily::C) {
            return None;
        }
        self.enclosing_scopes(f, scope).iter().find_map(|p| self.by_exact_name(f, &format!("{p}::{name}")))
    }

    /// `$ns:path` receiver: `path::name` looked up from the innermost
    /// enclosing namespace outwards, then globally (`$ns:::a`, written
    /// `::a::name`, and `$ns:` for `::name` are global only); else the bare
    /// name through imports, the same file and unique names (never an
    /// implicit member).
    pub(crate) fn resolve_namespaced(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        path: &str,
    ) -> Outcome {
        let name = item.name;
        let (global, path) = match path.strip_prefix("::") {
            Some(p) => (true, p),
            None => (path.is_empty(), path),
        };
        if let Some(hit) = self.namespaced_exact(f, scope, global, path, name) {
            return hit;
        }
        match self.envs[f as usize].get(name) {
            Some(Some(Target::Symbol(n))) => return Outcome::Hit(*n, Strategy::ImportResolved, 0.95),
            Some(Some(_)) => return Outcome::External,
            Some(None) | None => {}
        }
        if let Some(n) = self.module_def(f, name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.9);
        }
        self.unique_name(f, item.kind, name, false)
    }

    /// `path::name` looked up exactly from the innermost enclosing namespace
    /// outwards (unless `global`), then from the global namespace.
    fn namespaced_exact(
        &self,
        f: FileId,
        scope: Option<LocalDefId>,
        global: bool,
        path: &str,
        name: &str,
    ) -> Option<Outcome> {
        let qualified = if path.is_empty() { name.to_string() } else { format!("{path}::{name}") };
        if !global {
            for p in self.enclosing_scopes(f, scope) {
                if let Some(hit) = self.by_exact_name(f, &format!("{p}::{qualified}")) {
                    return Some(hit);
                }
            }
        }
        self.by_exact_name(f, &qualified)
    }

    // -- member chains -------------------------------------------------------

    /// Declared type of a member (field) node: its first `Type` reference
    /// (in source order) resolved in the member's file.
    fn member_type(&self, member: NodeId) -> TypeLookup {
        let Some(g) = self.nodes[member as usize].file else { return TypeLookup::Unknown };
        let Some(j) = self.def_node[g as usize].iter().position(|&n| n == member) else {
            return TypeLookup::Unknown;
        };
        let j = j as LocalDefId;
        let refs = self.files[g as usize].facts.as_ref().map_or(&[][..], |x| x.references.as_slice());
        // C++ smart pointers stand for their pointee (`std::unique_ptr<T>
        // p_; p_->m()`).
        let c_family = self.families[g as usize] == Some(LanguageFamily::C);
        let pointer = |r: &Reference| {
            c_family && r.receiver.as_deref() == Some("std") && matches!(r.name.as_str(), "unique_ptr" | "shared_ptr")
        };
        let Some(r) = refs
            .iter()
            .filter(|r| r.kind == RefKind::Type && r.enclosing == Some(j) && !pointer(r))
            .min_by_key(|r| r.span.start_byte)
        else {
            return TypeLookup::Unknown;
        };
        let text = match r.receiver.as_deref() {
            Some(q) => format!("{q}::{}", r.name),
            None => r.name.clone(),
        };
        self.resolve_type_text(g, self.parents[g as usize][j as usize], &text)
    }

    /// `$member:a.b` receiver: member `a` of the enclosing type, member `b`
    /// of `a`'s declared type, then `name` in the last type (`type_resolved`
    /// 0.85). A first segment that is no member resolves like an unknown
    /// receiver (the unique-method rung).
    pub(crate) fn resolve_member_chain(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        chain: &str,
    ) -> Outcome {
        let fallback = || self.unique_name(f, item.kind, item.name, true);
        let Some(mut ty) = self.enclosing_type(f, scope).or_else(|| self.receiver_type_from_name(f, scope)) else {
            return fallback();
        };
        for (k, seg) in chain.split('.').enumerate() {
            let Some((member, _)) = self.find_member(ty, seg, Want::Value) else {
                return if k == 0 { fallback() } else { Outcome::Miss };
            };
            match self.member_type(self.prefer_body(member)) {
                TypeLookup::Found(t) => ty = t,
                TypeLookup::External => return Outcome::External,
                TypeLookup::Unknown => return fallback(),
            }
        }
        match self.find_member(ty, item.name, Want::of(item.kind)) {
            Some((n, _)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
            None if self.type_is_open(ty) => Outcome::External,
            None => Outcome::Miss,
        }
    }

    // -- receivers -----------------------------------------------------------

    /// Implicit self (`implicit_self` packs): member `name` of the enclosing
    /// types, innermost first, through their bases; for a method body
    /// declared outside its type (C++ `void Foo::bar() {}`), the type named
    /// by its qualified-name prefix.
    pub(crate) fn implicit_member(
        &self,
        f: FileId,
        scope: Option<LocalDefId>,
        name: &str,
        want: Want,
    ) -> Option<Outcome> {
        if !self.packs[f as usize].is_some_and(|p| p.implicit_self()) {
            return None;
        }
        let member = |class: NodeId| match self.find_member(class, name, want) {
            Some((n, false)) => Some(Outcome::Hit(n, Strategy::TypeResolved, 0.9)),
            Some((n, true)) => Some(Outcome::Hit(n, Strategy::TypeResolved, 0.85)),
            None => None,
        };
        let defs = self.defs(f);
        let mut cur = scope;
        let mut nested = false;
        while let Some(a) = cur {
            if defs[a as usize].label.is_type() {
                nested = true;
                if let Some(hit) = member(self.node_of(f, a)) {
                    return Some(hit);
                }
            }
            cur = self.parents[f as usize][a as usize];
        }
        if nested {
            return None;
        }
        self.receiver_type_from_name(f, scope).and_then(member)
    }

    /// Receiver type of a method that is not nested in its type (a Go
    /// method, a C++ out-of-line body): the type whose global qualified name
    /// is the enclosing callable's minus its last segment
    /// (`example.com/m/server.Server` for `….Server.Handle`, `ns::Foo` for
    /// `ns::Foo::bar`).
    pub(crate) fn receiver_type_from_name(&self, f: FileId, scope: Option<LocalDefId>) -> Option<NodeId> {
        let defs = self.defs(f);
        let mut cur = scope;
        while let Some(a) = cur {
            if defs[a as usize].label.is_callable() {
                let family = self.families[f as usize];
                let sep = if family == Some(LanguageFamily::C) { "::" } else { "." };
                let qn = &self.nodes[self.node_of(f, a) as usize].qualified_name;
                let (owner, _) = qn.rsplit_once(sep)?;
                let found = self.nodes_by_qn().get(owner)?.iter().copied().find(|&t| {
                    let n = &self.nodes[t as usize];
                    n.label.is_type() && n.language.map(Language::family) == family
                })?;
                return Some(self.prefer_body(found));
            }
            cur = self.parents[f as usize][a as usize];
        }
        None
    }

    /// Resolves a type written in `f` (a `$type:` receiver): exact global
    /// qualified name, else a `Type` reference through the strategy ladder.
    pub(crate) fn resolve_type_text(&self, f: FileId, scope: Option<LocalDefId>, text: &str) -> TypeLookup {
        let t = normalize_type_text(text);
        if t.is_empty() {
            return TypeLookup::Unknown;
        }
        let family = self.families[f as usize];
        let is_type = |n: NodeId| self.nodes[n as usize].label.is_type() || self.is_constructor_function(n);
        let item = |name, receiver| RefItem {
            kind: RefKind::Type,
            name,
            receiver,
            enclosing: scope,
            span: Span::default(),
            decorated: None,
            arity: None,
            arg_types: None,
        };
        let outcome = match split_qualified(t) {
            Some((qualifier, name)) => {
                if let Some(v) = self.nodes_by_qn().get(t) {
                    let hit = v
                        .iter()
                        .copied()
                        .find(|&n| is_type(n) && self.nodes[n as usize].language.map(Language::family) == family);
                    if let Some(n) = hit {
                        return TypeLookup::Found(self.prefer_body(n));
                    }
                }
                match self.resolve_item(f, &item(name, Some(qualifier)), scope) {
                    Outcome::Miss => self.unique_name(f, RefKind::Type, name, false),
                    other => other,
                }
            }
            None => self.resolve_item(f, &item(t, None), scope),
        };
        match outcome {
            Outcome::Hit(n, _, _) if is_type(n) => TypeLookup::Found(self.prefer_body(n)),
            Outcome::External => TypeLookup::External,
            _ if self.packs[f as usize].is_some_and(|p| p.is_builtin(t)) => TypeLookup::External,
            _ => TypeLookup::Unknown,
        }
    }

    /// `$type:T` receiver: member `name` of `T` (`type_resolved` 0.85). A
    /// member missing from an open type (a base outside the project) is
    /// external; an unknown `T` falls back to the unique-method rung.
    pub(crate) fn resolve_typed(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>, ty: &str) -> Outcome {
        match self.resolve_type_text(f, scope, ty) {
            TypeLookup::Found(t) => match self.find_member(t, item.name, Want::of(item.kind)) {
                Some((n, _)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
                None if self.type_is_open(t) => Outcome::External,
                None => Outcome::Miss,
            },
            TypeLookup::External => Outcome::External,
            TypeLookup::Unknown => {
                // C++: a qualifier the pack took for a type may be a
                // namespace declared in another file (`Exp::Break()`).
                if self.families[f as usize] == Some(LanguageFamily::C)
                    && let Some(hit) = self.namespaced_exact(f, scope, false, ty, item.name)
                {
                    return hit;
                }
                self.unique_name(f, item.kind, item.name, true)
            }
        }
    }

    /// Whether a type (or a project base of it) has a base outside the project.
    pub(crate) fn type_is_open(&self, class: NodeId) -> bool {
        let mut stack = vec![(class, 0usize)];
        let mut seen = Vec::new();
        while let Some((c, depth)) = stack.pop() {
            if self.open_types.contains(&c) {
                return true;
            }
            if depth >= MAX_DEPTH || seen.contains(&c) {
                continue;
            }
            seen.push(c);
            for &b in self.bases.get(&c).map_or(&[][..], |v| v.as_slice()) {
                stack.push((b, depth + 1));
            }
        }
        false
    }

    // -- OVERRIDES -----------------------------------------------------------

    /// Parameter types of a callable, normalized ([`normalize_param_type`]):
    /// from its overload suffix, else from the parameter list of its
    /// signature (`String fromJson(Reader json, Type typeOfT)`), when the
    /// pack wrote one. `None` when neither is known.
    pub(crate) fn param_types(&self, n: NodeId) -> Option<Vec<String>> {
        let node = &self.nodes[n as usize];
        if let Some((_, params)) = overload_suffix(&node.qualified_name) {
            return Some(split_params(params).into_iter().map(|p| normalize_param_type(p, false)).collect());
        }
        let params = signature_params(node.signature.as_deref()?, &node.name)?;
        Some(split_params(params).into_iter().map(|p| normalize_param_type(p, true)).collect())
    }

    /// `OVERRIDES` (packs with `overrides()`): a method of a type → the
    /// method with the same name found in each resolved base, paired by
    /// signature when the base has several overloads: the overloads whose
    /// parameter count matches (overload suffix, else the signature's
    /// parameter list), narrowed to the one with the same parameter types
    /// when exactly one has them; all of them when the counts are unknown.
    /// `type_resolved` 0.90.
    /// A C/C++ declaration stands for its body at either end.
    pub(crate) fn override_edges(&mut self) {
        let mut out = Vec::new();
        for i in 0..self.files.len() {
            if !self.packs[i].is_some_and(|p| p.overrides()) {
                continue;
            }
            let fid = i as FileId;
            for (j, d) in self.defs(fid).iter().enumerate() {
                for (src, dst) in self.overrides_of(fid, j as LocalDefId, d) {
                    out.push((src, dst, fid, d.name_span));
                }
            }
        }
        for (src, dst, f, span) in out {
            let meta = EdgeMeta { confidence: 0.9, strategy: Strategy::TypeResolved, evidence: Some((f, span)) };
            self.add_edge(src, dst, EdgeKind::Overrides, meta);
        }
    }

    /// The (method, overridden method) pairs of definition `j` of `fid`,
    /// a method of a type that is not a constructor; between bodies when
    /// they exist (C++ out-of-line definitions): what `trace` reaches from
    /// calls.
    fn overrides_of(&self, fid: FileId, j: LocalDefId, d: &Definition) -> Vec<(NodeId, NodeId)> {
        if !d.label.is_callable() || d.is_test {
            return Vec::new();
        }
        let Some(p) = self.parents[fid as usize][j as usize] else { return Vec::new() };
        let owner = &self.defs(fid)[p as usize];
        // Constructors are not overridden.
        if !owner.label.is_type() || d.name == owner.name {
            return Vec::new();
        }
        let class = self.node_of(fid, p);
        let method = self.node_of(fid, j);
        let own = self.param_types(method);
        let mut out = Vec::new();
        for &b in self.bases.get(&class).map_or(&[][..], |v| v.as_slice()) {
            let Some((found, _)) = self.find_member(b, &d.name, Want::Call) else { continue };
            let cands: Vec<NodeId> = match self.overload_of.get(&found) {
                Some(&s) => self.overload_sets[s].clone(),
                None => vec![found],
            };
            for c in self.override_targets(method, own.as_deref(), &cands) {
                out.push((self.prefer_body(method), self.prefer_body(c)));
            }
        }
        out
    }

    /// The candidates (same-named methods of a base) that `method`, with
    /// parameter types `own`, overrides (see [`Self::override_edges`]).
    fn override_targets(&self, method: NodeId, own: Option<&[String]>, cands: &[NodeId]) -> Vec<NodeId> {
        let mut same_arity = Vec::new();
        let mut same_types = Vec::new();
        for &c in cands {
            if c == method || !self.nodes[c as usize].label.is_callable() {
                continue;
            }
            let Some(types_equal) = self.override_fit(method, own, c, cands.len() > 1) else { continue };
            if types_equal {
                same_types.push(c);
            }
            same_arity.push(c);
        }
        if same_types.len() == 1 { same_types } else { same_arity }
    }

    /// Whether candidate `c` may be the method `method` (parameter types
    /// `own`) overrides: `None` when the parameter counts differ, else
    /// whether the parameter types are equal. `several`: `c` is one of
    /// several overloads.
    fn override_fit(&self, method: NodeId, own: Option<&[String]>, c: NodeId, several: bool) -> Option<bool> {
        match (own, self.param_types(c).as_deref()) {
            (Some(a), Some(b)) => (types_arity(a) == types_arity(b)).then_some(a == b),
            // Several overloads and no way to pair: keep the overload-suffix
            // arity rule (all when unknown).
            _ if several => {
                let (x, y) = (self.def_arity(method), self.def_arity(c));
                let differ = x.is_some() && y.is_some() && x != y;
                (!differ).then_some(false)
            }
            _ => Some(false),
        }
    }
}

/// Primitive types of Java and C-family languages, in widening order
/// (`byte` → `short` → `int` → `long` → `float` → `double`; `char` → `int`).
const PRIMITIVES: &[&str] = &["byte", "short", "char", "int", "long", "float", "double", "boolean"];

/// Whether an argument of static type `arg` can be passed to a parameter
/// of type `param` (both normalized). `false` only when it certainly
/// cannot: a primitive that does not widen to the parameter, a primitive
/// for `String` or the reverse, a class other than `String` for `String`
/// (which is final). Class to class is always possible: the subtype
/// relation is not known.
fn may_accept(param: &str, arg: &str) -> bool {
    let param = param.strip_suffix("...").unwrap_or(param);
    if param == arg {
        return true;
    }
    let prim = |t: &str| PRIMITIVES.contains(&t);
    match (prim(param), prim(arg)) {
        (true, true) => {
            let rank = |t: &str| match t {
                "byte" => 0,
                "short" | "char" => 1,
                "int" => 2,
                "long" => 3,
                "float" => 4,
                "double" => 5,
                _ => -1,
            };
            let (p, a) = (rank(param), rank(arg));
            a >= 0 && p >= a && !(param == "char" && arg != "char") && !(arg == "char" && param == "short")
        }
        // Boxing: `int` → `Integer`, `Number`, `Object`, …; never `String`.
        (false, true) => param != "String",
        // Never a C/C++ pointer for a primitive.
        (true, false) => arg != "String" && !arg.ends_with('*'),
        (false, false) => param != "String" || arg == "String",
    }
}

/// Overloads for a call with static argument types `args` (`?` unknown):
/// those whose parameters may accept every known argument
/// ([`may_accept`]), and of those the only one whose parameter types equal
/// the known argument types, when there is exactly one (an exact match is
/// the most specific applicable overload). All of `typed` when none
/// accepts the arguments (the types were wrong).
pub(crate) fn overload_by_types(typed: &[(NodeId, Vec<String>)], args: &[&str]) -> Vec<NodeId> {
    let compatible: Vec<&(NodeId, Vec<String>)> = typed.iter().filter(|(_, p)| accepts_args(p, args)).collect();
    if compatible.is_empty() {
        return typed.iter().map(|(n, _)| *n).collect();
    }
    let exact: Vec<NodeId> = compatible.iter().filter(|(_, p)| exact_args(p, args)).map(|(n, _)| *n).collect();
    if exact.len() == 1 && (0..args.len()).any(|i| known_arg(args, i).is_some()) {
        return exact;
    }
    // The most specific candidate: one whose parameters all pass to every
    // other's, and not the reverse (`twice(long)` over `twice(double)` for
    // an `int`). Class parameters always pass both ways (subtyping is not
    // known), so this only decides between primitives and `String`.
    let best: Vec<NodeId> = compatible
        .iter()
        .filter(|(n, p)| compatible.iter().all(|(m, q)| m == n || more_specific(p, q)))
        .map(|(n, _)| *n)
        .collect();
    if best.len() == 1 && compatible.len() > 1 {
        return best;
    }
    compatible.into_iter().map(|(n, _)| *n).collect()
}

/// The static type of argument `i`, when known (`?` is unknown).
fn known_arg<'s>(args: &[&'s str], i: usize) -> Option<&'s str> {
    args.get(i).copied().filter(|a| *a != "?")
}

/// The parameter that takes argument `i`; past the end, the element type
/// of a variadic tail.
fn param_at(params: &[String], i: usize) -> Option<String> {
    match params.get(i) {
        Some(p) => Some(p.clone()),
        None => params.last().filter(|p| p.ends_with("...")).cloned(),
    }
}

/// Only C/C++ types end in `*`: a known class argument never converts to
/// a pointer parameter (`std::stringstream` for `const char*`).
fn class_for_pointer(p: &str, a: &str) -> bool {
    p.trim_end_matches("...").ends_with('*') && !a.ends_with('*') && !a.ends_with("[]") && !PRIMITIVES.contains(&a)
}

/// Whether parameters `params` may accept every known argument type.
fn accepts_args(params: &[String], args: &[&str]) -> bool {
    (0..args.len()).all(|i| match (known_arg(args, i), param_at(params, i)) {
        (Some(a), Some(p)) => {
            !class_for_pointer(&p, a)
                && (may_accept(&p, a) || may_accept(p.strip_suffix("...").unwrap_or(&p), a.trim_end_matches("[]")))
        }
        _ => true,
    })
}

/// A type as an argument binds it: a C++ reference parameter binds an
/// argument of its type, and a string literal (`char*`) is best taken by a
/// string parameter.
fn value_type(t: &str) -> String {
    let t = t.trim_end_matches('&');
    match t {
        "string" | "string_view" | "basic_string" | "basic_string_view" => "char*".to_string(),
        _ => t.to_string(),
    }
}

/// Whether the parameter types equal the known argument types. Trailing
/// parameters beyond the arguments have defaults (C++): the arity filter
/// ran before.
fn exact_args(params: &[String], args: &[&str]) -> bool {
    (0..args.len()).all(|i| match (known_arg(args, i), params.get(i)) {
        (Some(a), Some(p)) => value_type(&normalize_param_type(a, false)) == value_type(p),
        (None, Some(_)) => true,
        _ => false,
    }) && params.len() >= args.len()
}

/// Whether parameters `a` all pass to `b`'s and not the reverse.
fn more_specific(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| may_accept(y, x))
        && !a.iter().zip(b).all(|(x, y)| may_accept(x, y))
}

/// Arity range of a list of parameter types (a `...` tail is variadic).
fn types_arity(types: &[String]) -> (u32, Option<u32>) {
    match types.last() {
        Some(t) if t.ends_with("...") => (types.len() as u32 - 1, None),
        _ => (types.len() as u32, Some(types.len() as u32)),
    }
}

/// The parameter list written after `name(` in a signature, without the
/// parentheses (`String fromJson(Reader json, Type t)` → `Reader json, Type t`).
fn signature_params<'s>(signature: &'s str, name: &str) -> Option<&'s str> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut from = 0;
    while let Some(i) = signature[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        let before_ok = !signature[..i].ends_with(ident);
        if !before_ok || !signature[from..].starts_with('(') {
            continue;
        }
        let open = from;
        let mut depth = 0i32;
        for (k, c) in signature[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&signature[open + 1..open + k]);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    None
}

/// Splits a parameter list on its top-level commas; `""` and `void` are
/// empty.
fn split_params(params: &str) -> Vec<&str> {
    let p = params.trim();
    if p.is_empty() || p == "void" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in p.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(p[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(p[start..].trim());
    out
}

/// A parameter's type in a comparable form: annotations, modifiers
/// (`final`, `const`), a default value, generic or template arguments,
/// qualifiers (`java.util.Map.Entry` → `Entry`, `std::string` → `string`)
/// and whitespace removed; arrays, varargs, pointers and references kept.
/// `named`: the text ends with the parameter name (a signature), which is
/// dropped when another token precedes it.
pub(crate) fn normalize_param_type(param: &str, named: bool) -> String {
    let text = strip_annotations(param);
    // A default value (C++ `int n = 0`).
    let text = text.split('=').next().unwrap_or(&text);
    let mut tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace())
        .filter(|t| !t.is_empty() && !matches!(*t, "final" | "const" | "volatile" | "struct" | "class" | "enum"))
        .collect();
    if named
        && tokens.len() > 1
        && tokens[tokens.len() - 1].chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
        tokens.pop();
    }
    let out = strip_generic_args(&tokens.concat());
    // Qualifiers: keep the last segment (`...` varargs is not a qualifier).
    let (core, varargs) = match out.strip_suffix("...") {
        Some(c) => (c.to_string(), true),
        None => (out, false),
    };
    let core = match core.rfind(['.', ':']) {
        Some(i) => core[i + 1..].to_string(),
        None => core,
    };
    if varargs { format!("{core}...") } else { core }
}

/// A parameter without its annotations (`@Nullable`, `@Size(max = 3)`).
fn strip_annotations(p: &str) -> String {
    let mut text = String::new();
    let mut depth = 0i32;
    let mut skipping = false;
    for c in p.chars() {
        if !skipping {
            if c == '@' {
                skipping = true;
            } else {
                text.push(c);
            }
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    skipping = false;
                }
            }
            c if depth == 0 && c.is_whitespace() => skipping = false,
            _ => {}
        }
    }
    text
}

/// A type without its generic / template arguments.
fn strip_generic_args(t: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overload_suffixes_and_arity() {
        assert_eq!(overload_suffix("a.Parser.parse(String, int)"), Some(("a.Parser.parse", "String, int")));
        assert_eq!(overload_suffix("f(std::function<void(int)>)"), Some(("f", "std::function<void(int)>")));
        assert_eq!(overload_suffix("Parser.parse"), None);
        assert_eq!(overload_suffix("(x)"), None);
        assert_eq!(overload_suffix("n::get(/*const*/)"), Some(("n::get", "")));
        assert_eq!(overload_suffix("write(OutputIt, T/*#2*/)"), Some(("write", "OutputIt, T")));
        assert_eq!(params_arity(""), (0, Some(0)));
        assert_eq!(params_arity("void"), (0, Some(0)));
        assert_eq!(params_arity("String"), (1, Some(1)));
        assert_eq!(params_arity("Map<K, V>, int"), (2, Some(2)));
        assert_eq!(params_arity("String, Object..."), (1, None));
    }

    #[test]
    fn signature_arities() {
        assert_eq!(signature_arity("int f(int a, int b = 0)", "f"), Some((1, Some(2))));
        assert_eq!(signature_arity("void f(void)", "f"), Some((0, Some(0))));
        assert_eq!(signature_arity("int printf(const char* fmt, ...)", "printf"), Some((1, None)));
        assert_eq!(
            signature_arity(
                "template <typename T, typename F = std::function<void(int)>> auto f(T&&... args) -> int",
                "f"
            ),
            Some((0, None))
        );
        assert_eq!(
            signature_arity(
                "auto write(OutputIt out, monostate, format_specs = {}, locale_ref = {}) -> OutputIt",
                "write"
            ),
            Some((2, Some(4)))
        );
        assert_eq!(signature_arity("bool operator==(const Money& o) const", "operator=="), Some((1, Some(1))));
        assert_eq!(signature_arity("#define MAX(a, b) ((a) > (b) ? (a) : (b))", "MAX"), Some((2, Some(2))));
        assert_eq!(signature_arity("void g(std::map<int, int> m, int x = a >= b)", "g"), Some((1, Some(2))));
        assert_eq!(signature_arity("struct prefix_f", "f"), None);
    }

    #[test]
    fn namespace_macro_expansions() {
        let e = |sig: &str, name: &str| namespace_expansion(sig, name).map(|(p, v)| format!("{p}|{v}"));
        let fmt = "#define FMT_BEGIN_NAMESPACE namespace fmt { inline namespace v12 {";
        assert_eq!(e(fmt, "FMT_BEGIN_NAMESPACE").as_deref(), Some("fmt::v12|fmt"));
        assert_eq!(e("# define NS_BEGIN namespace a::b {", "NS_BEGIN").as_deref(), Some("a::b|a::b"));
        assert_eq!(e("#define FMT_BEGIN_EXPORT", "FMT_BEGIN_EXPORT").as_deref(), Some("|"));
        assert_eq!(e("#define G_BEGIN_DECLS extern \"C\" {", "G_BEGIN_DECLS").as_deref(), Some("|"));
        assert_eq!(e("#define F(x) namespace x {", "F"), None);
    }

    #[test]
    fn type_texts() {
        assert_eq!(normalize_type_text("*http.Server"), "http.Server");
        assert_eq!(normalize_type_text("const std::vector<int>&"), "std::vector");
        assert_eq!(normalize_type_text("[]*Node"), "Node");
        assert_eq!(normalize_type_text("List<String>"), "List");
        assert_eq!(normalize_type_text("struct sockaddr *"), "sockaddr");
        assert_eq!(normalize_type_text("Integer?"), "Integer");
        assert_eq!(split_qualified("ns::Foo"), Some(("ns", "Foo")));
        assert_eq!(split_qualified("a.b.C"), Some(("a.b", "C")));
        assert_eq!(split_qualified("Foo"), None);
    }

    #[test]
    fn overloads_chosen_by_argument_types() {
        let set = |v: &[&[&str]]| -> Vec<(NodeId, Vec<String>)> {
            v.iter().enumerate().map(|(i, p)| (i as NodeId, p.iter().map(|s| s.to_string()).collect())).collect()
        };
        // value(String) / value(boolean) / value(long) / value(double) / value(Number)
        let value = set(&[&["String"], &["boolean"], &["long"], &["double"], &["Number"]]);
        assert_eq!(overload_by_types(&value, &["long"]), vec![2]);
        assert_eq!(overload_by_types(&value, &["boolean"]), vec![1]);
        // `int` widens to long / double and boxes to Number: no exact match,
        // and `Number` is not comparable with the primitives.
        assert_eq!(overload_by_types(&value, &["int"]), vec![2, 3, 4]);
        // twice(long) / twice(double) with an `int`: long is the most specific.
        assert_eq!(overload_by_types(&set(&[&["long"], &["double"]]), &["int"]), vec![0]);
        assert_eq!(overload_by_types(&value, &["?"]), vec![0, 1, 2, 3, 4]);
        // JsonSyntaxException(String) / (Throwable): a caught exception is not a String.
        let ctor = set(&[&["String"], &["Throwable"]]);
        assert_eq!(overload_by_types(&ctor, &["IOException"]), vec![1]);
        assert_eq!(overload_by_types(&ctor, &["String"]), vec![0]);
        // fromJson(String, Class) / (String, Type) / (Reader, Type).
        let from = set(&[&["String", "Class"], &["String", "Type"], &["Reader", "Type"]]);
        assert_eq!(overload_by_types(&from, &["String", "Class"]), vec![0]);
        assert_eq!(overload_by_types(&from, &["String", "?"]), vec![0, 1, 2]);
        // Varargs.
        let total = set(&[&["int..."], &["int", "int"]]);
        assert_eq!(overload_by_types(&total, &["int", "int"]), vec![1]);
        // Wrong types: keep every candidate.
        assert_eq!(overload_by_types(&set(&[&["int"], &["long"]]), &["String"]), vec![0, 1]);
        assert!(may_accept("Object", "int") && may_accept("long", "char") && !may_accept("char", "int"));
        assert!(!may_accept("String", "StringBuilder") && may_accept("CharSequence", "String"));
        // C++ Load(const std::string&) / (const char*) / (std::istream&) with
        // a `std::stringstream`: never the pointer; a pointer or an `int`
        // (`0`) may still be one.
        let load = set(&[&["string"], &["char*"], &["istream"]]);
        assert_eq!(overload_by_types(&load, &["stringstream"]), vec![0, 2]);
        assert_eq!(overload_by_types(&set(&[&["Node*"], &["Node"]]), &["int"]), vec![0, 1]);
    }

    #[test]
    fn parameter_types_for_overrides() {
        assert_eq!(
            signature_params("public Foo fromJson(Reader in, Type t) throws X", "fromJson"),
            Some("Reader in, Type t")
        );
        assert_eq!(signature_params("void toJson(Object src)", "Json"), None);
        assert_eq!(signature_params("int f()", "f"), Some(""));
        assert_eq!(split_params("Map<K, V> m, int n"), vec!["Map<K, V> m", "int n"]);
        assert!(split_params(" void ").is_empty());
        assert_eq!(normalize_param_type("final @Nullable java.util.Map.Entry<K, V> e", true), "Entry");
        assert_eq!(normalize_param_type("@Size(max = 3) String s", true), "String");
        assert_eq!(normalize_param_type("Object... args", true), "Object...");
        assert_eq!(normalize_param_type("byte[] b", true), "byte[]");
        assert_eq!(normalize_param_type("const std::string& s = \"\"", true), "string&");
        assert_eq!(normalize_param_type("String", false), "String");
        assert_eq!(normalize_param_type("int", true), "int");
    }
}
