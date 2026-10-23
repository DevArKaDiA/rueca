//! Module names and import resolution (RFC 0002 §4.1: `oxc_resolver` is a
//! dependency for TS/JS imports).
//!
//! `LanguagePack::resolve_import` must not touch the disk: everything Telar
//! knows about the snapshot is in [`ProjectFiles`]. `oxc_resolver` accepts a
//! custom [`FileSystem`], so this module implements one over a **virtual
//! root** ([`ROOT`], `/__telar__`):
//!
//! - every `ProjectFiles::paths` entry is a file at `ROOT/<path>`; every
//!   prefix of a path is a directory; nothing else exists (no symlinks);
//! - `read` / `read_to_string` serve `ProjectFiles::configs` (tsconfig*,
//!   jsconfig, package.json); other files are not readable (the resolver
//!   never needs source contents);
//! - a directory with a `jsconfig.json` and no `tsconfig.json` also has a
//!   synthesized `tsconfig.json` that `extends` the jsconfig and sets
//!   `allowJs`, because oxc_resolver's automatic discovery only looks for
//!   `tsconfig.json` (TypeScript treats jsconfig as a tsconfig with
//!   `allowJs`);
//! - **workspace packages**: every `package.json` with a `"name"` (outside
//!   `node_modules/`) is mounted at `ROOT/node_modules/<name>`, mirroring
//!   the symlinks npm / yarn / pnpm workspaces create. Bare specifiers of
//!   workspace packages therefore go through oxc_resolver's full package
//!   resolution (`exports` with conditions, `types` / `module` / `main`).
//!
//! Results are mapped back to repository-relative paths; anything under a
//! real `node_modules/` directory (vendored third-party code) or absent from
//! `ProjectFiles::paths` is `None`.
//!
//! Unbuilt workspace packages: `exports` / `types` / `main` usually point at
//! a build directory that is not committed. Inside a workspace package, a
//! missing `dist|lib|build|out/[types|cjs|esm|mjs/]x.{d.ts,js,mjs,cjs}` is
//! served as the existing `src/x.{ts,tsx,mts,cts,js,jsx}`. If oxc_resolver
//! still finds nothing, a last fallback resolves the subpath against the
//! package's `src/` directory and then its root (`@acme/ui/button` →
//! `packages/ui/src/button`, `packages/ui/button`).
//!
//! One resolver (with its path cache) is built per `ProjectFiles` and kept in
//! a small process-wide cache keyed by the `ProjectFiles` address plus a
//! cheap fingerprint, so per-import calls only pay for the lookup.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use oxc_resolver::{
    EnforceExtension, FileMetadata, FileSystem, ResolveError, ResolveOptions, ResolverGeneric, TsconfigDiscovery,
};
use telar_extract::ProjectFiles;

/// Absolute root of the virtual file system.
#[cfg(not(windows))]
pub(crate) const ROOT: &str = "/__telar__";
#[cfg(windows)]
pub(crate) const ROOT: &str = "C:\\__telar__";

/// Build-output directories mapped back to `src/` (see
/// [`FsData::build_output_source`]).
const BUILD_DIRS: &[&str] = &["build", "dist", "lib", "out"];
const BUILD_FLAVORS: &[&str] = &["cjs", "esm", "mjs", "types"];
const OUTPUT_EXTENSIONS: &[&str] = &[".d.ts", ".d.mts", ".d.cts", ".js", ".mjs", ".cjs", ".jsx"];
const SOURCE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx"];

/// Resolver cache size (distinct `ProjectFiles` alive at once).
const CACHE_ENTRIES: usize = 4;

// ---------------------------------------------------------------------------
// Module names
// ---------------------------------------------------------------------------

/// The path without its final extension; declaration files also drop `.d`
/// (`src/util/index.ts` → `src/util/index`, `types/x.d.ts` → `types/x`).
pub(crate) fn module_name(path: &str) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    for ext in [".d.ts", ".d.mts", ".d.cts"] {
        if let Some(stem) = file.strip_suffix(ext)
            && !stem.is_empty()
        {
            return format!("{dir}{stem}");
        }
    }
    match file.rfind('.') {
        Some(i) if i > 0 => format!("{dir}{}", &file[..i]),
        _ => path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn abs(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(ROOT);
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

/// Repository-relative form of a virtual absolute path (`""` for the root).
fn rel(path: &Path) -> Option<String> {
    let rest = path.strip_prefix(ROOT).ok()?;
    let mut segs: Vec<String> = Vec::new();
    for c in rest.components() {
        match c {
            Component::Normal(s) => segs.push(s.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                segs.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(segs.join("/"))
}

fn join(dir: &str, sub: &str) -> String {
    match (dir.is_empty(), sub.is_empty()) {
        (true, _) => sub.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{sub}"),
    }
}

fn parent_dir(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn in_node_modules(path: &str) -> bool {
    path.split('/').any(|s| s == "node_modules")
}

/// Splits a bare specifier into package name and subpath
/// (`@a/b/c/d` → (`@a/b`, `c/d`), `lodash/fp` → (`lodash`, `fp`)).
pub(crate) fn split_package(spec: &str) -> Option<(&str, &str)> {
    let mut idx = spec.find('/');
    if spec.starts_with('@') {
        let first = idx?;
        idx = spec[first + 1..].find('/').map(|i| first + 1 + i);
    }
    match idx {
        Some(i) => Some((&spec[..i], &spec[i + 1..])),
        None => Some((spec, "")),
    }
}

// ---------------------------------------------------------------------------
// Virtual file system
// ---------------------------------------------------------------------------

/// Read-only file system over a [`ProjectFiles`]; see the module docs.
/// Cheap to clone (shared data).
#[derive(Debug, Default, Clone)]
pub(crate) struct VirtualFs(Arc<FsData>);

#[derive(Debug, Default)]
pub(crate) struct FsData {
    files: HashSet<String>,
    dirs: HashSet<String>,
    contents: HashMap<String, String>,
    /// `tsconfig.json` files that only exist as jsconfig aliases.
    synthesized: HashSet<String>,
    /// Workspace package name → package directory (`""` = repository root).
    packages: BTreeMap<String, String>,
}

impl std::ops::Deref for VirtualFs {
    type Target = FsData;
    fn deref(&self) -> &FsData {
        &self.0
    }
}

impl VirtualFs {
    pub(crate) fn new_for(project: &ProjectFiles) -> VirtualFs {
        VirtualFs(Arc::new(FsData::new_for(project)))
    }
}

impl FsData {
    fn new_for(project: &ProjectFiles) -> FsData {
        let mut fs = FsData::default();
        fs.dirs.insert(String::new());
        for p in project.paths.iter().chain(project.configs.keys()) {
            fs.add_file(p);
        }
        for (path, text) in &project.configs {
            fs.contents.insert(path.clone(), text.clone());
        }
        // jsconfig.json doubles as tsconfig.json when there is none: a
        // synthesized tsconfig extends it and turns on `allowJs` (implied by
        // jsconfig; without it oxc_resolver does not apply the config to
        // `.js` files).
        for path in project.configs.keys() {
            let name = path.rsplit('/').next().unwrap_or(path);
            if name == "jsconfig.json" {
                let ts = join(parent_dir(path), "tsconfig.json");
                if !fs.files.contains(&ts) {
                    let text = r#"{"extends":"./jsconfig.json","compilerOptions":{"allowJs":true}}"#;
                    fs.contents.insert(ts.clone(), text.to_string());
                    fs.add_file(&ts);
                    fs.synthesized.insert(ts);
                }
            }
        }
        // Workspace packages, first directory (in path order) wins a name.
        for (path, text) in &project.configs {
            let name = path.rsplit('/').next().unwrap_or(path);
            if name != "package.json" || in_node_modules(path) {
                continue;
            }
            let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else { continue };
            let Some(pkg) = json.get("name").and_then(|n| n.as_str()) else { continue };
            if pkg.is_empty() || split_package(pkg).is_none_or(|(n, sub)| n != pkg || !sub.is_empty()) {
                continue;
            }
            fs.packages.entry(pkg.to_string()).or_insert_with(|| parent_dir(path).to_string());
        }
        if !fs.packages.is_empty() {
            fs.dirs.insert("node_modules".into());
            let scopes: Vec<String> =
                fs.packages.keys().filter_map(|n| n.split_once('/').map(|(s, _)| format!("node_modules/{s}"))).collect();
            fs.dirs.extend(scopes);
        }
        fs
    }

    fn add_file(&mut self, path: &str) {
        self.files.insert(path.to_string());
        let mut dir = parent_dir(path);
        while !dir.is_empty() && self.dirs.insert(dir.to_string()) {
            dir = parent_dir(dir);
        }
    }

    /// Repository-relative path behind a virtual path, following the
    /// workspace-package mounts under `ROOT/node_modules` and mapping
    /// missing build outputs of workspace packages to their sources.
    pub(crate) fn real(&self, path: &Path) -> Option<String> {
        let mut r = rel(path)?;
        if let Some(rest) = r.strip_prefix("node_modules/")
            && let Some((name, sub)) = split_package(rest)
            && let Some(dir) = self.packages.get(name)
        {
            r = join(dir, sub);
        }
        if !self.files.contains(&r)
            && !self.dirs.contains(&r)
            && let Some(src) = self.build_output_source(&r)
        {
            return Some(src);
        }
        Some(r)
    }

    /// `<pkg>/dist/types/a/b.d.ts`, `<pkg>/dist/cjs/a/b.js`, `<pkg>/lib/a/b.mjs`
    /// … → `<pkg>/src/a/b.{ts,tsx,mts,cts,js,jsx}` when the build output is
    /// not in the repository (unbuilt monorepo package whose `exports` /
    /// `main` / `types` point at its build directory).
    fn build_output_source(&self, r: &str) -> Option<String> {
        let dir = self.packages.values().filter(|d| d.is_empty() || r.starts_with(&format!("{d}/"))).max_by_key(|d| d.len())?;
        let rest = if dir.is_empty() { r } else { &r[dir.len() + 1..] };
        let (out, rest) = rest.split_once('/')?;
        if !BUILD_DIRS.contains(&out) {
            return None;
        }
        let rest = match rest.split_once('/') {
            Some((flavor, inner)) if BUILD_FLAVORS.contains(&flavor) => inner,
            _ => rest,
        };
        let stem = OUTPUT_EXTENSIONS.iter().find_map(|e| rest.strip_suffix(e))?;
        SOURCE_EXTENSIONS.iter().map(|e| join(dir, &format!("src/{stem}{e}"))).find(|c| self.files.contains(c))
    }

    fn meta(&self, path: &Path) -> io::Result<FileMetadata> {
        let r = self.real(path).ok_or_else(not_found)?;
        if self.files.contains(&r) {
            Ok(FileMetadata::new(true, false, false))
        } else if self.dirs.contains(&r) {
            Ok(FileMetadata::new(false, true, false))
        } else {
            Err(not_found())
        }
    }
}

#[cold]
fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "not in the project")
}

impl FileSystem for VirtualFs {
    fn new() -> Self {
        VirtualFs::default()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_to_string(path).map(String::into_bytes)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let r = self.real(path).ok_or_else(not_found)?;
        self.contents.get(&r).cloned().ok_or_else(not_found)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        self.meta(path)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        self.meta(path)
    }

    fn read_link(&self, _path: &Path) -> Result<PathBuf, ResolveError> {
        Err(io::Error::new(io::ErrorKind::InvalidInput, "no symlinks in the virtual file system").into())
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.meta(path)?;
        Ok(abs(&self.real(path).ok_or_else(not_found)?))
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

fn options() -> ResolveOptions {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    ResolveOptions {
        cwd: Some(PathBuf::from(ROOT)),
        tsconfig: Some(TsconfigDiscovery::Auto),
        extensions: s(&[".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".json"]),
        extension_alias: vec![
            (".js".into(), s(&[".ts", ".tsx", ".js"])),
            (".jsx".into(), s(&[".tsx", ".jsx"])),
            (".mjs".into(), s(&[".mts", ".mjs"])),
            (".cjs".into(), s(&[".cts", ".cjs"])),
        ],
        main_fields: s(&["types", "module", "main"]),
        condition_names: s(&["types", "import", "require", "default", "node"]),
        enforce_extension: EnforceExtension::Auto,
        symlinks: false,
        node_path: false,
        builtin_modules: false,
        ..ResolveOptions::default()
    }
}

/// oxc_resolver over one [`ProjectFiles`].
pub(crate) struct ProjectResolver {
    resolver: ResolverGeneric<VirtualFs>,
    /// Same cache, no tsconfig: used when a tsconfig cannot be loaded (e.g.
    /// it `extends` a package that is not in the repository).
    plain: ResolverGeneric<VirtualFs>,
    /// Shared view of the resolver's file system, for path mapping.
    fs: VirtualFs,
}

impl ProjectResolver {
    pub(crate) fn new(project: &ProjectFiles) -> ProjectResolver {
        let fs = VirtualFs::new_for(project);
        let resolver = ResolverGeneric::new_with_file_system(fs.clone(), options());
        let plain = resolver.clone_with_options(ResolveOptions { tsconfig: None, ..options() });
        ProjectResolver { resolver, plain, fs }
    }

    /// Resolves `specifier` written in `from_path`.
    pub(crate) fn resolve(&self, from_path: &str, specifier: &str) -> Option<String> {
        if specifier.is_empty() || specifier.contains('\0') {
            return None;
        }
        let file = abs(from_path);
        let first = match self.resolver.resolve_file(&file, specifier) {
            Err(e) if tsconfig_problem(&e) => self.plain.resolve_file(&file, specifier),
            r => r,
        };
        if let Ok(res) = first
            && let Some(p) = self.project_path(res.path())
        {
            return Some(p);
        }
        self.workspace_fallback(specifier)
    }

    fn project_path(&self, path: &Path) -> Option<String> {
        let r = self.fs.real(path)?;
        (self.fs.files.contains(&r) && !self.fs.synthesized.contains(&r) && !in_node_modules(&r)).then_some(r)
    }

    fn workspace_fallback(&self, specifier: &str) -> Option<String> {
        if specifier.starts_with('.') || specifier.starts_with('/') {
            return None;
        }
        let (name, sub) = split_package(specifier)?;
        let dir = self.fs.packages.get(name)?;
        let candidates: Vec<String> = if sub.is_empty() {
            vec!["./src".into(), "./src/index".into(), "./index".into()]
        } else {
            vec![format!("./src/{sub}"), format!("./{sub}")]
        };
        let base = abs(dir);
        for c in candidates {
            if let Ok(res) = self.plain.resolve(&base, &c)
                && let Some(p) = self.project_path(res.path())
            {
                return Some(p);
            }
        }
        None
    }
}

fn tsconfig_problem(e: &ResolveError) -> bool {
    matches!(
        e,
        ResolveError::TsconfigNotFound(_)
            | ResolveError::TsconfigSelfReference(_)
            | ResolveError::TsconfigCircularExtend(_)
            | ResolveError::Json(_)
            | ResolveError::IOError(_)
    )
}

// ---------------------------------------------------------------------------
// Per-ProjectFiles cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    addr: usize,
    fingerprint: u64,
}

/// Cheap content fingerprint: sizes, every config path and length, the
/// first 4 KiB of config text, and the first / last 16 paths. Guards the
/// address key against a dropped `ProjectFiles` whose memory is reused.
fn fingerprint(project: &ProjectFiles) -> u64 {
    let mut h = DefaultHasher::new();
    project.paths.len().hash(&mut h);
    project.configs.len().hash(&mut h);
    let mut budget = 4096usize;
    for (k, v) in &project.configs {
        k.hash(&mut h);
        v.len().hash(&mut h);
        if budget > 0 {
            let n = v.len().min(budget);
            v.as_bytes()[..n].hash(&mut h);
            budget -= n;
        }
    }
    for p in project.paths.iter().take(16) {
        p.hash(&mut h);
    }
    for p in project.paths.iter().rev().take(16) {
        p.hash(&mut h);
    }
    h.finish()
}

type CacheList = Vec<(Key, Arc<ProjectResolver>)>;

/// The resolver for `project`, built on first use.
pub(crate) fn resolver_for(project: &ProjectFiles) -> Arc<ProjectResolver> {
    static CACHE: OnceLock<Mutex<CacheList>> = OnceLock::new();
    let key = Key { addr: project as *const ProjectFiles as usize, fingerprint: fingerprint(project) };
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    {
        let mut list = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = list.iter().position(|(k, _)| *k == key) {
            let entry = list.remove(i);
            let r = Arc::clone(&entry.1);
            list.insert(0, entry);
            return r;
        }
    }
    let built = Arc::new(ProjectResolver::new(project));
    let mut list = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, r)) = list.iter().find(|(k, _)| *k == key) {
        return Arc::clone(r);
    }
    list.insert(0, (key, Arc::clone(&built)));
    list.truncate(CACHE_ENTRIES);
    built
}
