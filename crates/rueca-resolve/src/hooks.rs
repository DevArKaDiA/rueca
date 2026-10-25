//! Resolver support for the RFC 0003 §9.2 / §10.1 contract hooks: package-
//! and directory-scoped modules, directory imports, typed (`$type:T`) and
//! normalized (`$self`) receivers, implicit self, star imports of packages
//! and C/C++ includes, declarations vs bodies, overloads and `OVERRIDES`.
//!
//! Every rule here is keyed on a hook a pack opts into (or on facts only the
//! new packs emit), so Python, ECMAScript, PHP and Markdown resolve exactly
//! as before.

use super::*;

/// Confidence multiplier when a reference links several overloads.
const OVERLOAD_FANOUT: f32 = 0.8;

/// Splits an overload suffix off a qualified name: `Parser.parse(String)` →
/// (`Parser.parse`, `String`). `None` when the name has no suffix.
pub(crate) fn overload_suffix(qn: &str) -> Option<(&str, &str)> {
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

impl<'a> Resolver<'a> {
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
        let mut decls = Vec::new();
        for i in 0..self.files.len() {
            for (j, d) in self.defs(i as FileId).iter().enumerate() {
                if d.is_declaration {
                    decls.push(self.def_node[i][j]);
                }
            }
        }
        if decls.is_empty() {
            return;
        }
        let decl_set: HashSet<NodeId> = decls.iter().copied().collect();
        let mut body_of = HashMap::new();
        for &d in &decls {
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
        for v in self.by_name.values_mut() {
            v.retain(|n| !body_of.contains_key(n));
        }
        self.body_of = body_of;
    }

    /// The body of a declaration, else the node itself.
    pub(crate) fn prefer_body(&self, n: NodeId) -> NodeId {
        self.body_of.get(&n).copied().unwrap_or(n)
    }

    /// Overloads of packs with `overloads()`: definitions whose local name
    /// ends with a parameter suffix, grouped by (family, global qualified
    /// name without the suffix).
    fn build_overload_sets(&mut self) {
        let mut groups: BTreeMap<(String, String), Vec<NodeId>> = BTreeMap::new();
        for i in 0..self.files.len() {
            if !self.packs[i].is_some_and(|p| p.overloads()) {
                continue;
            }
            for j in 0..self.def_node[i].len() {
                let n = self.def_node[i][j];
                if self.body_of.contains_key(&n) {
                    continue;
                }
                let node = &self.nodes[n as usize];
                let Some((base, _)) = overload_suffix(&node.qualified_name) else { continue };
                let family = format!("{:?}", self.families[i]);
                let v = groups.entry((family, base.to_string())).or_default();
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
        for (_, set) in groups {
            if set.len() < 2 {
                continue;
            }
            let id = self.overload_sets.len();
            for &n in &set {
                self.overload_of.insert(n, id);
            }
            self.overload_sets.push(set);
        }
    }

    /// Arity range of a definition from its overload suffix.
    fn def_arity(&self, n: NodeId) -> Option<(u32, Option<u32>)> {
        overload_suffix(&self.nodes[n as usize].qualified_name).map(|(_, p)| params_arity(p))
    }

    /// Expands a resolved target to its overloads (RFC 0003 §10.1): those
    /// whose parameter count fits `arity`, narrowed by the static argument
    /// types when the pack reports them (`args`, see [`overload_by_types`]);
    /// one candidate keeps the confidence, several are all linked × 0.8,
    /// and no fitting one links the whole set × 0.8.
    pub(crate) fn expand_overloads(
        &self,
        target: NodeId,
        arity: Option<u32>,
        args: Option<&[&str]>,
        strategy: Strategy,
        confidence: f32,
    ) -> Vec<(NodeId, Strategy, f32)> {
        let Some(&set) = self.overload_of.get(&target) else { return vec![(target, strategy, confidence)] };
        let set = &self.overload_sets[set];
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
        match chosen.len() {
            1 => vec![(chosen[0], strategy, confidence)],
            0 => set.iter().map(|&n| (n, strategy, confidence * OVERLOAD_FANOUT)).collect(),
            _ => chosen.into_iter().map(|n| (n, strategy, confidence * OVERLOAD_FANOUT)).collect(),
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
                for m in self.module_files(t) {
                    let defs = self.defs(m);
                    for &j in &self.top_level[m as usize] {
                        let d = &defs[j as usize];
                        if !d.exported || (self.package_of[m as usize].is_some() && d.qualified_name != d.name) {
                            continue;
                        }
                        if seen.insert(d.name.as_str()) {
                            out.push((d.name.clone(), self.node_of(m, j)));
                        }
                    }
                }
                if self.families[t as usize] != Some(LanguageFamily::C) || depth == MAX_DEPTH {
                    continue;
                }
                for (k, imp) in self.imports(t).iter().enumerate() {
                    let star = imp.names.iter().any(|n| n.name == "*" && n.alias.is_none());
                    if let (true, Some(i)) = (star, self.import_targets[t as usize][k])
                        && visited.insert(i)
                    {
                        next.push(i);
                    }
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
            TypeLookup::Unknown => self.unique_name(f, item.kind, item.name, true),
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
    pub(crate) fn override_edges(&mut self) {
        let mut out = Vec::new();
        for i in 0..self.files.len() {
            if !self.packs[i].is_some_and(|p| p.overrides()) {
                continue;
            }
            let fid = i as FileId;
            let defs = self.defs(fid);
            for (j, d) in defs.iter().enumerate() {
                if !d.label.is_callable() || d.is_test {
                    continue;
                }
                let Some(p) = self.parents[i][j] else { continue };
                if !defs[p as usize].label.is_type() {
                    continue;
                }
                let class = self.node_of(fid, p);
                let method = self.node_of(fid, j as LocalDefId);
                // Constructors are not overridden.
                if d.name == defs[p as usize].name {
                    continue;
                }
                let own = self.param_types(method);
                for &b in self.bases.get(&class).map_or(&[][..], |v| v.as_slice()) {
                    let Some((found, _)) = self.find_member(b, &d.name, Want::Call) else { continue };
                    let cands: Vec<NodeId> = match self.overload_of.get(&found) {
                        Some(&s) => self.overload_sets[s].clone(),
                        None => vec![found],
                    };
                    for c in self.override_targets(method, own.as_deref(), &cands) {
                        out.push((method, c, fid, d.name_span));
                    }
                }
            }
        }
        for (src, dst, f, span) in out {
            let meta = EdgeMeta { confidence: 0.9, strategy: Strategy::TypeResolved, evidence: Some((f, span)) };
            self.add_edge(src, dst, EdgeKind::Overrides, meta);
        }
    }

    /// The candidates (same-named methods of a base) that `method`, with
    /// parameter types `own`, overrides (see [`Self::override_edges`]).
    fn override_targets(&self, method: NodeId, own: Option<&[String]>, cands: &[NodeId]) -> Vec<NodeId> {
        let arity = |types: &[String]| match types.last() {
            Some(t) if t.ends_with("...") => (types.len() as u32 - 1, None),
            _ => (types.len() as u32, Some(types.len() as u32)),
        };
        let mut same_arity = Vec::new();
        let mut same_types = Vec::new();
        for &c in cands {
            if c == method || !self.nodes[c as usize].label.is_callable() {
                continue;
            }
            let theirs = self.param_types(c);
            match (own, theirs.as_deref()) {
                (Some(a), Some(b)) => {
                    if arity(a) != arity(b) {
                        continue;
                    }
                    if a == b {
                        same_types.push(c);
                    }
                }
                _ if cands.len() > 1 => {
                    // Several overloads and no way to pair: keep the
                    // overload-suffix arity rule (all when unknown).
                    let (x, y) = (self.def_arity(method), self.def_arity(c));
                    if x.is_some() && y.is_some() && x != y {
                        continue;
                    }
                }
                _ => {}
            }
            same_arity.push(c);
        }
        if same_types.len() == 1 { same_types } else { same_arity }
    }
}

/// Primitive types of Java and C-family languages, in widening order
/// (`byte` → `short` → `int` → `long` → `float` → `double`; `char` → `int`).
const PRIMITIVES: &[&str] = &["byte", "short", "char", "int", "long", "float", "double", "boolean"];

/// Whether an argument of static type `arg` can be passed to a parameter
/// of type `param` (both normalized). `false` only when it certainly
/// cannot: a primitive that does not widen to the parameter, a primitive
/// for `String` or the reverse, or a class other than `String` for
/// `String` (which is final). Class to class is always possible: the
/// subtype relation is not known.
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
        (true, false) => arg != "String",
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
    let known = |i: usize| args.get(i).copied().filter(|a| *a != "?");
    let param = |params: &[String], i: usize| -> Option<String> {
        match params.get(i) {
            Some(p) => Some(p.clone()),
            // Variadic tail: the element type.
            None => params.last().filter(|p| p.ends_with("...")).cloned(),
        }
    };
    let accepts = |params: &[String]| {
        (0..args.len()).all(|i| match (known(i), param(params, i)) {
            (Some(a), Some(p)) => {
                may_accept(&p, a) || may_accept(p.strip_suffix("...").unwrap_or(&p), a.trim_end_matches("[]"))
            }
            _ => true,
        })
    };
    let exact = |params: &[String]| {
        (0..args.len()).all(|i| match (known(i), params.get(i)) {
            (Some(a), Some(p)) => normalize_param_type(a, false) == *p,
            (None, Some(_)) => true,
            _ => false,
        }) && params.len() == args.len()
    };
    let compatible: Vec<&(NodeId, Vec<String>)> = typed.iter().filter(|(_, p)| accepts(p)).collect();
    if compatible.is_empty() {
        return typed.iter().map(|(n, _)| *n).collect();
    }
    let exact: Vec<NodeId> = compatible.iter().filter(|(_, p)| exact(p)).map(|(n, _)| *n).collect();
    if exact.len() == 1 && (0..args.len()).any(|i| known(i).is_some()) {
        return exact;
    }
    compatible.into_iter().map(|(n, _)| *n).collect()
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
    let p = param;
    // Annotations (`@Nullable`, `@Size(max = 3)`) and modifiers.
    let mut text = String::new();
    let mut depth = 0i32;
    let mut skipping = false;
    for c in p.chars() {
        if skipping {
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
            continue;
        }
        if c == '@' {
            skipping = true;
            continue;
        }
        text.push(c);
    }
    // A default value (C++ `int n = 0`).
    let text = text.split('=').next().unwrap_or(&text);
    let mut tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace())
        .filter(|t| !t.is_empty() && !matches!(*t, "final" | "const" | "volatile" | "struct" | "class" | "enum"))
        .collect();
    if named && tokens.len() > 1 {
        let last = tokens[tokens.len() - 1];
        if last.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            tokens.pop();
        }
    }
    let joined: String = tokens.concat();
    // Generic / template arguments.
    let mut out = String::new();
    let mut depth = 0i32;
    for c in joined.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overload_suffixes_and_arity() {
        assert_eq!(overload_suffix("a.Parser.parse(String, int)"), Some(("a.Parser.parse", "String, int")));
        assert_eq!(overload_suffix("f(std::function<void(int)>)"), Some(("f", "std::function<void(int)>")));
        assert_eq!(overload_suffix("Parser.parse"), None);
        assert_eq!(overload_suffix("(x)"), None);
        assert_eq!(params_arity(""), (0, Some(0)));
        assert_eq!(params_arity("void"), (0, Some(0)));
        assert_eq!(params_arity("String"), (1, Some(1)));
        assert_eq!(params_arity("Map<K, V>, int"), (2, Some(2)));
        assert_eq!(params_arity("String, Object..."), (1, None));
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
        // `int` widens to long / double and boxes to Number: no exact match.
        assert_eq!(overload_by_types(&value, &["int"]), vec![2, 3, 4]);
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
