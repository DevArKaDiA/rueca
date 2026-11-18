//! Cargo module model: crates, crate roots, module paths and the module
//! files `use` / `mod` paths name.
//!
//! - Every `Cargo.toml` with a `[package]` is a package (workspace members
//!   or not: a fixture crate inside another workspace is still a crate). Its
//!   targets are crates: the library (`[lib] path`, default `src/lib.rs`,
//!   named by `[lib] name`, else the package name with `-` → `_`), binaries
//!   (`src/main.rs` named like the package, `src/bin/<x>.rs` and
//!   `src/bin/<x>/main.rs`, `[[bin]]` entries), and the test, example and
//!   bench crates (`tests/<x>.rs`, `examples/<x>.rs`, `benches/<x>.rs`, or
//!   `<dir>/<x>/main.rs`); `build.rs` is its own crate.
//! - A file belongs to the crate whose root it is, else to the binary,
//!   test, example or bench crate whose directory holds it, else to the
//!   library (the binary when the package has no library). Without any
//!   `Cargo.toml`, the nearest directory holding `lib.rs` or `main.rs` is a
//!   crate named after it (`src` → its parent directory).
//! - A file's module path is the crate name plus the directories and file
//!   stem below the crate root's directory (`rueca_index::link` for
//!   `crates/rueca-index/src/link.rs`; `mod.rs` names its directory).
//! - `mod x;` in a file whose module directory is `d` (its own directory
//!   for crate roots and `mod.rs`, else `<dir>/<stem>`) is `d/x.rs` or
//!   `d/x/mod.rs`; `#[path = "p"]` is relative to the declaring file's
//!   directory.
//! - A path's crate is `crate` (the file's crate root), `super` / `self`
//!   (relative to the file's module), a dependency of the file's package
//!   (`[dependencies]` key, renames and `path` / `workspace = true` entries
//!   followed), or any crate of the snapshot with that name (the one in the
//!   same workspace, then the nearest by directory). `std`, `core`, `alloc`
//!   and third-party crates are not in the snapshot: they resolve to
//!   nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use rueca_extract::ProjectFiles;
use rueca_model::Import;
use toml_edit::{Document, Item};

/// One dependency entry of a manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dependency {
    /// The key as written (`serde`, `rueca-model`): the crate name code uses,
    /// with `-` → `_`.
    pub key: String,
    /// `package = "…"`: the package it renames.
    pub package: Option<String>,
    /// `path = "…"`, relative to the manifest's directory.
    pub path: Option<String>,
    /// `workspace = true`: the entry of `[workspace.dependencies]`.
    pub workspace: bool,
}

impl Dependency {
    /// The name of the package the entry depends on.
    pub fn package_name(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.key)
    }
}

/// The parts of a `Cargo.toml` the module model and the organization layer
/// read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// `[package] name`.
    pub package: Option<String>,
    /// `[lib] name`.
    pub lib_name: Option<String>,
    /// `[lib] path`.
    pub lib_path: Option<String>,
    /// `[[bin]]` entries: (name, path).
    pub bins: Vec<(Option<String>, Option<String>)>,
    /// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` and
    /// their `[target.….*]` forms.
    pub dependencies: Vec<Dependency>,
    /// Whether the manifest has a `[workspace]` table.
    pub workspace: bool,
    /// `[workspace] members` and `exclude` patterns.
    pub members: Vec<String>,
    pub exclude: Vec<String>,
    /// `[workspace.dependencies]`.
    pub workspace_dependencies: Vec<Dependency>,
}

impl Manifest {
    /// The library crate name: `[lib] name`, else the package name with
    /// `-` → `_`.
    pub fn lib_crate_name(&self) -> Option<String> {
        self.lib_name.clone().or_else(|| self.package.as_deref().map(crate_name))
    }
}

/// A crate name from a package or target name (`rueca-index` → `rueca_index`).
pub fn crate_name(name: &str) -> String {
    name.replace('-', "_")
}

fn str_at<'a>(item: &'a Item, keys: &[&str]) -> Option<&'a str> {
    let mut cur = item;
    for k in keys {
        cur = cur.get(*k)?;
    }
    cur.as_str()
}

fn strings_at(item: &Item, keys: &[&str]) -> Vec<String> {
    let mut cur = item;
    for k in keys {
        match cur.get(*k) {
            Some(next) => cur = next,
            None => return Vec::new(),
        }
    }
    cur.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// The entries of a dependency table (`serde = "1"`, `x = { path = "…" }`).
fn dependency_table(item: Option<&Item>) -> Vec<Dependency> {
    let Some(table) = item.and_then(Item::as_table_like) else { return Vec::new() };
    table
        .iter()
        .map(|(key, v)| Dependency {
            key: key.to_string(),
            package: v.get("package").and_then(Item::as_str).map(str::to_string),
            path: v.get("path").and_then(Item::as_str).map(str::to_string),
            workspace: v.get("workspace").and_then(Item::as_bool).unwrap_or(false),
        })
        .collect()
}

/// Every dependency table of a manifest, `[target.'cfg(…)'.*]` included.
fn all_dependencies(root: &Item) -> Vec<Dependency> {
    const TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut out = Vec::new();
    for t in TABLES {
        out.extend(dependency_table(root.get(t)));
    }
    if let Some(targets) = root.get("target").and_then(Item::as_table_like) {
        for (_, target) in targets.iter() {
            for t in TABLES {
                out.extend(dependency_table(target.get(t)));
            }
        }
    }
    out
}

fn bins(root: &Item) -> Vec<(Option<String>, Option<String>)> {
    let Some(list) = root.get("bin").and_then(Item::as_array_of_tables) else { return Vec::new() };
    list.iter()
        .map(|t| {
            let field = |k: &str| t.get(k).and_then(Item::as_str).map(str::to_string);
            (field("name"), field("path"))
        })
        .collect()
}

/// Parses a `Cargo.toml`; `None` when it is not valid TOML.
pub fn parse_manifest(text: &str) -> Option<Manifest> {
    let doc = Document::parse(text).ok()?;
    let root = doc.as_item();
    Some(Manifest {
        package: str_at(root, &["package", "name"]).map(str::to_string),
        lib_name: str_at(root, &["lib", "name"]).map(str::to_string),
        lib_path: str_at(root, &["lib", "path"]).map(str::to_string),
        bins: bins(root),
        dependencies: all_dependencies(root),
        workspace: root.get("workspace").is_some(),
        members: strings_at(root, &["workspace", "members"]),
        exclude: strings_at(root, &["workspace", "exclude"]),
        workspace_dependencies: dependency_table(root.get("workspace").and_then(|w| w.get("dependencies"))),
    })
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub(crate) fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `dir/rel` (`rel` when `dir` is the repository root).
/// A `mod x;` declared inside an inline module (`module` `m::x`, see the
/// crate docs, *Imports*).
fn is_nested_mod_declaration(import: &Import, segs: &[&str]) -> bool {
    import.relative_level == 1
        && segs.len() > 1
        && import.names.is_empty()
        && !import.type_only
        && import.module_alias.as_deref() == segs.last().copied()
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() { rel.to_string() } else { format!("{dir}/{rel}") }
}

/// Joins `base` and a relative path, normalizing `.` and `..`; `None` when
/// the path escapes the repository or is absolute.
pub(crate) fn join_normalized(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// The path below `dir` (`None` when `path` is not under it).
fn below<'p>(dir: &str, path: &'p str) -> Option<&'p str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// Whether a workspace `members` / `exclude` pattern (`crates/*`) matches a
/// directory relative to the workspace root.
fn glob_matches(pattern: &str, dir: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let (p_parts, d_parts): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), dir.split('/').collect());
    p_parts.len() == d_parts.len() && p_parts.iter().zip(&d_parts).all(|(p, d)| segment_matches(p, d))
}

/// One glob segment: `*` and a single `*` inside a segment (`rueca-*`).
fn segment_matches(pattern: &str, seg: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == seg,
        Some((pre, post)) => seg.len() >= pre.len() + post.len() && seg.starts_with(pre) && seg.ends_with(post),
    }
}

// ---------------------------------------------------------------------------
// Crates of a snapshot
// ---------------------------------------------------------------------------

/// A crate root: the root file (when there is one) and its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Root {
    pub(crate) file: Option<String>,
    pub(crate) dir: String,
    pub(crate) name: String,
}

/// A target of a package: crate name and root file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    name: String,
    root: String,
}

#[derive(Debug, Clone)]
struct Package {
    dir: String,
    name: String,
    lib: Option<Target>,
    /// Binaries (implicit and `[[bin]]`).
    bins: Vec<Target>,
    dependencies: Vec<Dependency>,
    /// Directory of the workspace root that lists this package, if any.
    workspace: Option<String>,
}

#[derive(Debug, Clone)]
struct Workspace {
    dir: String,
    members: Vec<String>,
    exclude: Vec<String>,
    dependencies: Vec<Dependency>,
}

impl Workspace {
    fn contains(&self, pkg_dir: &str) -> bool {
        if pkg_dir == self.dir {
            return true;
        }
        let Some(rel) = below(&self.dir, pkg_dir) else { return false };
        self.members.iter().any(|m| glob_matches(m, rel)) && !self.exclude.iter().any(|e| glob_matches(e, rel))
    }
}

/// The crates of a snapshot (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct Crates {
    /// Packages sorted by directory.
    packages: Vec<Package>,
    paths: BTreeSet<String>,
}

/// Directories of crates other than the library: (directory under the
/// package, kind).
const TARGET_DIRS: [&str; 4] = ["tests", "examples", "benches", "src/bin"];

impl Crates {
    pub(crate) fn build(project: &ProjectFiles) -> Crates {
        let mut workspaces = Vec::new();
        let mut manifests = Vec::new();
        for (path, text) in &project.configs {
            if file_name(path) != "Cargo.toml" {
                continue;
            }
            let Some(m) = parse_manifest(text) else { continue };
            let dir = parent_dir(path).to_string();
            if m.workspace {
                workspaces.push(Workspace {
                    dir: dir.clone(),
                    members: m.members.clone(),
                    exclude: m.exclude.clone(),
                    dependencies: m.workspace_dependencies.clone(),
                });
            }
            manifests.push((dir, m));
        }
        let mut crates = Crates { packages: Vec::new(), paths: project.paths.clone() };
        for (dir, m) in manifests {
            if let Some(name) = m.package.clone() {
                let workspace = Self::workspace_of(&workspaces, &dir);
                let pkg = crates.package(dir, name, &m, workspace);
                crates.packages.push(pkg);
            }
        }
        crates.packages.sort_by(|a, b| a.dir.cmp(&b.dir));
        crates.workspaces_deps(&workspaces);
        crates
    }

    /// The deepest workspace root listing `dir` as a member.
    fn workspace_of(workspaces: &[Workspace], dir: &str) -> Option<String> {
        workspaces.iter().filter(|w| w.contains(dir)).max_by_key(|w| w.dir.len()).map(|w| w.dir.clone())
    }

    /// Resolves `workspace = true` dependencies to the workspace's entries.
    fn workspaces_deps(&mut self, workspaces: &[Workspace]) {
        for pkg in &mut self.packages {
            let Some(ws) = pkg.workspace.as_ref().and_then(|d| workspaces.iter().find(|w| &w.dir == d)) else {
                continue;
            };
            for dep in &mut pkg.dependencies {
                if !dep.workspace {
                    continue;
                }
                if let Some(w) = ws.dependencies.iter().find(|w| w.key == dep.key) {
                    dep.path = w.path.as_ref().and_then(|p| join_normalized(&ws.dir, p)).map(|abs| format!("/{abs}"));
                    dep.package = dep.package.clone().or_else(|| w.package.clone());
                }
            }
        }
    }

    fn package(&self, dir: String, name: String, m: &Manifest, workspace: Option<String>) -> Package {
        let lib_root = match &m.lib_path {
            Some(p) => join_normalized(&dir, p),
            None => Some(join(&dir, "src/lib.rs")),
        }
        .filter(|p| self.paths.contains(p));
        let lib_name = m.lib_crate_name().unwrap_or_else(|| crate_name(&name));
        let lib = lib_root.map(|root| Target { name: lib_name, root });
        let mut bins: Vec<Target> = m
            .bins
            .iter()
            .filter_map(|(n, p)| {
                let root = join_normalized(&dir, p.as_deref()?)?;
                Some(Target { name: crate_name(n.as_deref().unwrap_or(&name)), root })
            })
            .collect();
        let main = join(&dir, "src/main.rs");
        if self.paths.contains(&main) && !bins.iter().any(|b| b.root == main) {
            // The package's default binary first: other files of `src/` are
            // its modules when there is no library.
            bins.insert(0, Target { name: crate_name(&name), root: main });
        }
        Package { dir, name, lib, bins, dependencies: m.dependencies.clone(), workspace }
    }

    /// The package whose directory is the deepest ancestor of `path`.
    fn package_of(&self, path: &str) -> Option<&Package> {
        self.packages.iter().filter(|p| below(&p.dir, path).is_some()).max_by_key(|p| p.dir.len())
    }

    /// The crate `path` belongs to (see the module docs).
    pub(crate) fn root_of(&self, path: &str) -> Root {
        match self.package_of(path) {
            Some(pkg) => self.package_root(pkg, path),
            None => self.loose_root(path),
        }
    }

    fn package_root(&self, pkg: &Package, path: &str) -> Root {
        let target = |t: &Target| Root {
            file: Some(t.root.clone()),
            dir: parent_dir(&t.root).to_string(),
            name: t.name.clone(),
        };
        if let Some(t) = pkg.lib.iter().chain(&pkg.bins).find(|t| t.root == path) {
            return target(t);
        }
        let rel = below(&pkg.dir, path).unwrap_or(path);
        if rel == "build.rs" {
            return Root { file: Some(path.to_string()), dir: pkg.dir.clone(), name: "build_script_build".into() };
        }
        if let Some(root) = self.target_dir_root(pkg, rel) {
            return root;
        }
        match pkg.lib.as_ref().or(pkg.bins.first()) {
            Some(t) => target(t),
            None => Root { file: None, dir: join(&pkg.dir, "src"), name: crate_name(&pkg.name) },
        }
    }

    /// The test, example, bench or `src/bin` crate holding `rel` (a path
    /// below the package): `<dir>/<x>.rs`, `<dir>/<x>/main.rs` and the files
    /// under `<dir>/<x>/`.
    fn target_dir_root(&self, pkg: &Package, rel: &str) -> Option<Root> {
        let (kind, rest) = TARGET_DIRS.iter().find_map(|d| below(d, rel).map(|r| (*d, r)))?;
        let base = join(&pkg.dir, kind);
        match rest.split_once('/') {
            None => {
                let name = crate_name(rest.strip_suffix(".rs")?);
                Some(Root { file: Some(join(&base, rest)), dir: base, name })
            }
            Some((sub, _)) => {
                let main = join(&base, &format!("{sub}/main.rs"));
                let dir = join(&base, sub);
                let file = self.paths.contains(&main).then_some(main);
                Some(Root { file, dir, name: crate_name(sub) })
            }
        }
    }

    /// Without a package: the nearest directory holding `lib.rs` or
    /// `main.rs`, named after it (`src` → its parent).
    fn loose_root(&self, path: &str) -> Root {
        let mut dir = parent_dir(path);
        loop {
            for root in ["lib.rs", "main.rs"] {
                let f = join(dir, root);
                if self.paths.contains(&f) {
                    return Root { file: Some(f), dir: dir.to_string(), name: loose_name(dir) };
                }
            }
            if dir.is_empty() {
                break;
            }
            dir = parent_dir(dir);
        }
        let dir = parent_dir(path).to_string();
        Root { file: None, name: loose_name(&dir), dir }
    }

    /// Whether `path` is the root file of its crate.
    pub(crate) fn is_root(&self, path: &str) -> bool {
        self.root_of(path).file.as_deref() == Some(path)
    }

    /// The directory holding the modules `path` declares with `mod x;`.
    pub(crate) fn module_dir(&self, path: &str) -> String {
        let name = file_name(path);
        if matches!(name, "mod.rs" | "lib.rs" | "main.rs") || self.is_root(path) {
            return parent_dir(path).to_string();
        }
        join(parent_dir(path), name.strip_suffix(".rs").unwrap_or(name))
    }

    /// The module path of `path` (`rueca_index::link`).
    pub(crate) fn module_path(&self, path: &str) -> String {
        let root = self.root_of(path);
        if root.file.as_deref() == Some(path) {
            return root.name;
        }
        let rel = below(&root.dir, path).unwrap_or_else(|| file_name(path));
        let mut segs: Vec<&str> = rel.strip_suffix(".rs").unwrap_or(rel).split('/').collect();
        if segs.last() == Some(&"mod") {
            segs.pop();
        }
        let mut out = root.name;
        for s in segs.into_iter().filter(|s| !s.is_empty()) {
            out.push_str("::");
            out.push_str(s);
        }
        out
    }

    /// The module file for module directory `dir` of crate `root`.
    fn module_file(&self, root: &Root, dir: &str) -> Option<String> {
        if dir == root.dir {
            return root.file.clone();
        }
        [format!("{dir}.rs"), format!("{dir}/mod.rs")].into_iter().find(|p| self.paths.contains(p))
    }

    /// Walks module names below (`file`, `dir`): each segment is `<dir>/<s>.rs`
    /// or `<dir>/<s>/mod.rs`. Stops at the first segment that is not a file
    /// (an inline module, an item) and returns the deepest module file.
    fn walk(&self, file: Option<String>, dir: String, segs: &[&str]) -> Option<String> {
        let (mut file, mut dir) = (file, dir);
        for s in segs {
            let next = join(&dir, s);
            match [format!("{next}.rs"), format!("{next}/mod.rs")].into_iter().find(|p| self.paths.contains(p)) {
                Some(f) => {
                    file = Some(f);
                    dir = next;
                }
                None => break,
            }
        }
        file
    }

    /// Resolves a module path written in `from` (encoding in the crate docs,
    /// *Imports*) to a module file of the snapshot.
    pub(crate) fn resolve(&self, from: &str, import: &Import) -> Option<String> {
        let module = import.module.as_str();
        if let Some(lit) = module.strip_prefix('"').and_then(|m| m.strip_suffix('"')) {
            return join_normalized(parent_dir(from), lit).filter(|p| self.paths.contains(p));
        }
        let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
        if import.relative_level > 0 {
            if is_nested_mod_declaration(import, &segs)
                && let Some(f) = self.nested_module_file(from, &segs)
            {
                return Some(f);
            }
            return self.resolve_relative(from, import.relative_level, &segs);
        }
        match segs.split_first() {
            Some((&"crate", rest)) => {
                let root = self.root_of(from);
                self.walk(root.file, root.dir, rest)
            }
            Some((first, rest)) => self.resolve_crate_path(from, module.starts_with("::"), first, rest),
            None => None,
        }
    }

    /// The file of `mod x;` written inside inline modules (`mod net { mod
    /// tcp; }` → `<dir>/net/tcp.rs`): the inline modules are directories
    /// even when no file holds them.
    fn nested_module_file(&self, from: &str, segs: &[&str]) -> Option<String> {
        let dir = segs.iter().fold(self.module_dir(from), |d, s| join(&d, s));
        [format!("{dir}.rs"), format!("{dir}/mod.rs")].into_iter().find(|p| self.paths.contains(p))
    }

    /// `self` / `super` paths: `level` 1 is the file's module, each further
    /// level its parent.
    fn resolve_relative(&self, from: &str, level: u32, segs: &[&str]) -> Option<String> {
        let root = self.root_of(from);
        let mut file = Some(from.to_string());
        let mut dir = self.module_dir(from);
        for _ in 1..level {
            if dir == root.dir || below(&root.dir, &dir).is_none() {
                return None;
            }
            dir = parent_dir(&dir).to_string();
            file = self.module_file(&root, &dir);
        }
        self.walk(file, dir, segs)
    }

    /// A path whose first segment is a crate name (or, unless `extern_only`,
    /// a module declared by `from`: 2018 uniform paths).
    fn resolve_crate_path(&self, from: &str, extern_only: bool, first: &str, rest: &[&str]) -> Option<String> {
        if !extern_only {
            let dir = self.module_dir(from);
            let child = join(&dir, first);
            if [format!("{child}.rs"), format!("{child}/mod.rs")].iter().any(|p| self.paths.contains(p)) {
                let mut segs = vec![first];
                segs.extend_from_slice(rest);
                return self.walk(Some(from.to_string()), dir, &segs);
            }
        }
        let root = self.crate_root(from, first)?;
        self.walk(root.file, root.dir, rest)
    }

    /// The library crate `name` as seen from `from`: a dependency of its
    /// package (renames, `path` and workspace entries followed), its own
    /// library, else any library of the snapshot with that name (same
    /// workspace first, then the nearest).
    pub(crate) fn crate_root(&self, from: &str, name: &str) -> Option<Root> {
        let pkg = self.package_of(from);
        if let Some(root) = pkg.and_then(|p| self.dependency_root(p, name)) {
            return Some(root);
        }
        let workspace = pkg.and_then(|p| p.workspace.clone());
        self.packages
            .iter()
            .filter(|p| p.lib.as_ref().is_some_and(|l| l.name == name))
            .max_by_key(|p| {
                let same_pkg = pkg.is_some_and(|own| own.dir == p.dir);
                let same_ws = workspace.is_some() && p.workspace == workspace;
                (same_pkg, same_ws, common_prefix(&p.dir, from), std::cmp::Reverse(p.dir.clone()))
            })
            .and_then(|p| p.lib.as_ref())
            .map(|l| Root { file: Some(l.root.clone()), dir: parent_dir(&l.root).to_string(), name: l.name.clone() })
    }

    /// A `path` dependency of `pkg` used under crate name `name`.
    fn dependency_root(&self, pkg: &Package, name: &str) -> Option<Root> {
        let dep = pkg.dependencies.iter().find(|d| crate_name(&d.key) == name)?;
        let path = dep.path.as_deref()?;
        let dir = match path.strip_prefix('/') {
            Some(abs) => abs.to_string(),
            None => join_normalized(&pkg.dir, path)?,
        };
        let target = self.packages.iter().find(|p| p.dir == dir)?.lib.as_ref()?;
        Some(Root {
            file: Some(target.root.clone()),
            dir: parent_dir(&target.root).to_string(),
            name: name.to_string(),
        })
    }
}

/// Number of leading path segments two paths share.
fn common_prefix(a: &str, b: &str) -> usize {
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).count()
}

/// A crate name for a directory without a package (`src` → its parent).
fn loose_name(dir: &str) -> String {
    let name = file_name(dir);
    let name = if name == "src" { file_name(parent_dir(dir)) } else { name };
    if name.is_empty() { "crate".to_string() } else { crate_name(name) }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

type CacheKey = (usize, usize, Vec<(String, usize, usize)>);

fn cache_key(project: &ProjectFiles) -> CacheKey {
    let manifests = project
        .configs
        .iter()
        .filter(|(p, _)| file_name(p) == "Cargo.toml")
        .map(|(p, t)| (p.clone(), t.as_ptr() as usize, t.len()))
        .collect();
    (project as *const ProjectFiles as usize, project.paths.len(), manifests)
}

/// Snapshots kept in the cache (an MCP server may index several roots).
const CACHE_ENTRIES: usize = 4;

/// Process-wide cache of the most recent snapshots' crate tables.
#[derive(Default)]
pub(crate) struct CratesCache {
    entries: Mutex<BTreeMap<u64, (CacheKey, Arc<Crates>)>>,
    counter: Mutex<u64>,
}

impl CratesCache {
    pub(crate) fn get(&self, project: &ProjectFiles) -> Arc<Crates> {
        let key = cache_key(project);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (_, c))) = entries.iter().find(|(_, (k, _))| *k == key) {
            return c.clone();
        }
        let built = Arc::new(Crates::build(project));
        let mut counter = self.counter.lock().unwrap_or_else(|e| e.into_inner());
        *counter += 1;
        entries.insert(*counter, (key, built.clone()));
        while let Some(oldest) = entries.keys().next().copied().filter(|_| entries.len() > CACHE_ENTRIES) {
            entries.remove(&oldest);
        }
        built
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: configs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        }
    }

    fn imp(module: &str, level: u32) -> Import {
        Import {
            module: module.into(),
            relative_level: level,
            names: vec![],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Default::default(),
        }
    }

    const WS: &str = "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n\n[workspace.dependencies]\ncore-x = { path = \"crates/core\", package = \"core\" }\n";
    const CORE: &str = "[package]\nname = \"core\"\n";
    const APP: &str = "[package]\nname = \"my-app\"\n\n[dependencies]\ncore = { workspace = true }\nlib2 = { path = \"../core\", package = \"core\" }\nserde = \"1\"\n\n[[bin]]\nname = \"tool\"\npath = \"src/tool.rs\"\n";

    fn snapshot() -> Crates {
        Crates::build(&project(
            &[
                "Cargo.toml",
                "crates/core/Cargo.toml",
                "crates/core/src/lib.rs",
                "crates/core/src/a.rs",
                "crates/core/src/a/b.rs",
                "crates/core/src/c/mod.rs",
                "crates/core/src/c/d.rs",
                "crates/core/tests/it.rs",
                "crates/core/tests/common/mod.rs",
                "crates/core/build.rs",
                "crates/app/Cargo.toml",
                "crates/app/src/main.rs",
                "crates/app/src/tool.rs",
                "crates/app/src/cli.rs",
                "crates/app/src/bin/extra.rs",
                "crates/app/examples/demo/main.rs",
                "crates/app/examples/demo/util.rs",
                "loose/src/lib.rs",
                "loose/src/x.rs",
            ],
            &[("Cargo.toml", WS), ("crates/core/Cargo.toml", CORE), ("crates/app/Cargo.toml", APP)],
        ))
    }

    #[test]
    fn manifests() {
        let m = parse_manifest(APP).unwrap();
        assert_eq!(m.package.as_deref(), Some("my-app"));
        assert_eq!(m.lib_crate_name().as_deref(), Some("my_app"));
        assert_eq!(m.bins, vec![(Some("tool".to_string()), Some("src/tool.rs".to_string()))]);
        let keys: Vec<&str> = m.dependencies.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["core", "lib2", "serde"]);
        assert_eq!(m.dependencies[1].package_name(), "core");
        let ws = parse_manifest(WS).unwrap();
        assert!(ws.workspace && ws.package.is_none());
        assert_eq!(ws.members, ["crates/*"]);
        assert_eq!(ws.workspace_dependencies[0].package.as_deref(), Some("core"));
        assert!(parse_manifest("[package\nname=").is_none());
        assert!(glob_matches("crates/*", "crates/core") && !glob_matches("crates/*", "crates/a/b"));
        assert!(segment_matches("rueca-*", "rueca-model") && !segment_matches("rueca-*", "other"));
    }

    #[test]
    fn module_paths() {
        let c = snapshot();
        let cases = [
            ("crates/core/src/lib.rs", "core"),
            ("crates/core/src/a.rs", "core::a"),
            ("crates/core/src/a/b.rs", "core::a::b"),
            ("crates/core/src/c/mod.rs", "core::c"),
            ("crates/core/src/c/d.rs", "core::c::d"),
            ("crates/core/tests/it.rs", "it"),
            ("crates/core/tests/common/mod.rs", "common"),
            ("crates/core/build.rs", "build_script_build"),
            ("crates/app/src/main.rs", "my_app"),
            ("crates/app/src/tool.rs", "tool"),
            ("crates/app/src/cli.rs", "my_app::cli"),
            ("crates/app/src/bin/extra.rs", "extra"),
            ("crates/app/examples/demo/main.rs", "demo"),
            ("crates/app/examples/demo/util.rs", "demo::util"),
            ("loose/src/lib.rs", "loose"),
            ("loose/src/x.rs", "loose::x"),
        ];
        for (path, want) in cases {
            assert_eq!(c.module_path(path), want, "{path}");
        }
        assert_eq!(c.module_dir("crates/core/src/a.rs"), "crates/core/src/a");
        assert_eq!(c.module_dir("crates/core/src/c/mod.rs"), "crates/core/src/c");
        assert_eq!(c.module_dir("crates/core/tests/it.rs"), "crates/core/tests");
    }

    #[test]
    fn module_resolution() {
        let c = snapshot();
        let r = |from: &str, m: &str, level: u32| c.resolve(from, &imp(m, level));
        // `mod b;` in a.rs, `mod d;` in c/mod.rs.
        assert_eq!(r("crates/core/src/a.rs", "b", 1).as_deref(), Some("crates/core/src/a/b.rs"));
        assert_eq!(r("crates/core/src/c/mod.rs", "d", 1).as_deref(), Some("crates/core/src/c/d.rs"));
        // `self` and `super`.
        assert_eq!(r("crates/core/src/a/b.rs", "", 1).as_deref(), Some("crates/core/src/a/b.rs"));
        assert_eq!(r("crates/core/src/a/b.rs", "", 2).as_deref(), Some("crates/core/src/a.rs"));
        assert_eq!(r("crates/core/src/a/b.rs", "c::d", 3).as_deref(), Some("crates/core/src/c/d.rs"));
        assert_eq!(r("crates/core/src/a.rs", "", 3), None);
        // `crate::…`, deepest module file.
        assert_eq!(r("crates/core/src/c/d.rs", "crate::a::b", 0).as_deref(), Some("crates/core/src/a/b.rs"));
        assert_eq!(r("crates/core/src/c/d.rs", "crate::a::Inline::X", 0).as_deref(), Some("crates/core/src/a.rs"));
        assert_eq!(r("crates/core/src/c/d.rs", "crate", 0).as_deref(), Some("crates/core/src/lib.rs"));
        // Crates by name: dependency key, rename, workspace entry, own library.
        assert_eq!(r("crates/app/src/cli.rs", "core::a", 0).as_deref(), Some("crates/core/src/a.rs"));
        assert_eq!(r("crates/app/src/cli.rs", "lib2::c", 0).as_deref(), Some("crates/core/src/c/mod.rs"));
        assert_eq!(r("crates/core/tests/it.rs", "::core", 0).as_deref(), Some("crates/core/src/lib.rs"));
        assert_eq!(r("crates/app/src/cli.rs", "serde::de", 0), None);
        assert_eq!(r("crates/app/src/cli.rs", "std::fmt", 0), None);
        // Uniform path through a child module, and `#[path]`.
        assert_eq!(r("crates/core/src/a.rs", "b", 0).as_deref(), Some("crates/core/src/a/b.rs"));
        assert_eq!(r("crates/core/src/lib.rs", "\"c/d.rs\"", 1).as_deref(), Some("crates/core/src/c/d.rs"));
    }
}
