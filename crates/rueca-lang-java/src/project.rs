//! Java project layout: source roots, Maven / Gradle modules, and import
//! targets (RFC 0003 §10.4).
//!
//! **Source roots** are, in this order of trust:
//!
//! 1. Maven / Gradle conventions: every directory `…/src/<set>/java`
//!    (`src/main/java`, `src/test/java`, `src/integrationTest/java`,
//!    Android `app/src/main/java`, …) that holds Java files, at any depth, so
//!    every module of a multi-module build is covered.
//! 2. Declared roots: `<sourceDirectory>` / `<testSourceDirectory>` of every
//!    `pom.xml` and literal `srcDir` / `srcDirs` strings of every
//!    `build.gradle(.kts)`, relative to the build file.
//!
//! **Modules** (for tie-breaks between equally named classes) are the
//! directories of every build file plus the `<module>`s of a `pom.xml` and
//! the `include`s of a `settings.gradle(.kts)`.
//!
//! **Import targets.** A class import `a.b.C` (or `a.b.C.m`, `a.b.C.Inner`)
//! resolves to the file `C.java` of package directory `a/b` under a source
//! root, else under any directory whose path ends with `a/b` (non-standard
//! layouts); an on-demand import `a.b.*` resolves to such a package
//! directory. When several files match, the importer's own source root
//! wins, then its module, then roots over plain suffix matches, then main
//! code over tests, then the path order. Classes of the JDK and of
//! dependencies are not in the snapshot and resolve to nothing (external).
//!
//! The layout is computed once per [`ProjectFiles`] and cached (keyed by
//! its address, size and the identity of its build files), like the
//! Composer cache of the PHP pack.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use rueca_extract::ProjectFiles;
use rueca_model::Import;

/// Build files read for source roots and modules.
fn is_build_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(name, "pom.xml" | "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts")
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Joins `base` and a relative path, resolving `.` and `..`.
fn join(base: &str, rel: &str) -> Option<String> {
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

/// Text between `<tag>` and `</tag>`, for every occurrence.
fn xml_values<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        let Some(j) = rest.find(&close) else { break };
        out.push(rest[..j].trim());
        rest = &rest[j + close.len()..];
    }
    out
}

/// Removes XML comments (`<!-- … -->`).
fn strip_xml_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Quoted string literals (`'x'`, `"x"`) on a line.
fn quoted(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find(['\'', '"']) {
        let q = rest.as_bytes()[i] as char;
        let after = &rest[i + 1..];
        let Some(j) = after.find(q) else { break };
        out.push(&after[..j]);
        rest = &after[j + 1..];
    }
    out
}

/// Maven `${project.basedir}` / `${basedir}` prefixes.
fn strip_basedir(p: &str) -> &str {
    for pre in ["${project.basedir}/", "${basedir}/", "${project.basedir}", "${basedir}"] {
        if let Some(r) = p.strip_prefix(pre) {
            return r;
        }
    }
    p
}

/// The project layout of one snapshot.
#[derive(Debug, Default)]
pub(crate) struct Layout {
    /// Source roots, sorted.
    pub(crate) roots: BTreeSet<String>,
    /// Module directories (`""` = the repository root), sorted.
    pub(crate) modules: BTreeSet<String>,
    /// Java files by file name (`Foo.java` → paths).
    by_file_name: HashMap<String, Vec<String>>,
    /// Directories that directly contain Java files, by last segment.
    dirs_by_name: HashMap<String, Vec<String>>,
}

impl Layout {
    pub(crate) fn build(project: &ProjectFiles) -> Layout {
        let mut layout = Layout::default();
        let mut java_dirs = BTreeSet::new();
        for p in project.paths.iter().filter(|p| p.ends_with(".java")) {
            let name = p.rsplit('/').next().unwrap_or(p);
            layout.by_file_name.entry(name.to_string()).or_default().push(p.clone());
            java_dirs.insert(parent_dir(p).to_string());
        }
        for d in &java_dirs {
            let last = d.rsplit('/').next().unwrap_or(d);
            layout.dirs_by_name.entry(last.to_string()).or_default().push(d.clone());
            // Convention: the leftmost `src/<set>/java`.
            let segs: Vec<&str> = d.split('/').collect();
            if let Some(i) = (0..segs.len().saturating_sub(2)).find(|&i| segs[i] == "src" && segs[i + 2] == "java") {
                layout.roots.insert(segs[..=i + 2].join("/"));
            }
        }
        for (path, text) in project.configs.iter().filter(|(p, _)| is_build_file(p)) {
            let dir = parent_dir(path);
            let name = path.rsplit('/').next().unwrap_or(path);
            if !name.starts_with("settings.") {
                layout.modules.insert(dir.to_string());
            }
            let add_root = |rel: &str, layout: &mut Layout| {
                if let Some(r) = join(dir, strip_basedir(rel.trim()))
                    && project.is_dir(&r)
                {
                    layout.roots.insert(r);
                }
            };
            if name == "pom.xml" {
                let xml = strip_xml_comments(text);
                for tag in ["sourceDirectory", "testSourceDirectory"] {
                    for v in xml_values(&xml, tag) {
                        add_root(v, &mut layout);
                    }
                }
                for m in xml_values(&xml, "module") {
                    if let Some(d) = join(dir, m) {
                        layout.modules.insert(d);
                    }
                }
            } else if name.starts_with("build.gradle") {
                for line in text.lines().filter(|l| l.contains("srcDir")) {
                    for q in quoted(line) {
                        add_root(q, &mut layout);
                    }
                }
            } else {
                // settings.gradle(.kts): include ':a', ':b:c' / include("a")
                for line in text.lines().map(str::trim).filter(|l| l.starts_with("include")) {
                    for q in quoted(line) {
                        let m = q.trim_start_matches(':').replace(':', "/");
                        if let Some(d) = join(dir, &m) {
                            layout.modules.insert(d);
                        }
                    }
                }
            }
        }
        layout
    }

    /// Longest source root containing `path`.
    fn root_of(&self, path: &str) -> Option<&str> {
        self.roots.iter().filter(|r| path.starts_with(&format!("{r}/"))).max_by_key(|r| r.len()).map(String::as_str)
    }

    /// Longest module directory containing `path`.
    fn module_of(&self, path: &str) -> Option<&str> {
        self.modules
            .iter()
            .filter(|m| m.is_empty() || path.starts_with(&format!("{m}/")))
            .max_by_key(|m| m.len())
            .map(String::as_str)
    }

    /// Package implied by the path of a file under a source root
    /// (`src/main/java/com/x/A.java` → `com.x`); empty outside roots and in
    /// the default package.
    pub(crate) fn package_of_path(&self, path: &str) -> String {
        let Some(root) = self.root_of(path) else { return String::new() };
        parent_dir(&path[root.len() + 1..]).replace('/', ".")
    }

    /// Ranks a candidate target for an importer (lower is better).
    fn rank(&self, candidate: &str, prefix: &str, from: &str) -> (bool, bool, bool, bool, usize, String) {
        let from_root = self.root_of(from);
        let same_root = from_root.is_some() && from_root == Some(prefix);
        let from_module = self.module_of(from);
        let same_module = from_module.is_some() && self.module_of(candidate) == from_module;
        let is_root = self.roots.contains(prefix) || prefix.is_empty();
        let test = candidate.split('/').any(|s| s == "test" || s == "tests");
        let shared = candidate.split('/').zip(from.split('/')).take_while(|(a, b)| a == b).count();
        (!same_root, !same_module, !is_root, test, usize::MAX - shared, candidate.to_string())
    }

    fn best<'a>(&self, cands: impl Iterator<Item = (&'a str, &'a str)>, from: &str) -> Option<String> {
        cands.map(|(c, prefix)| (self.rank(c, prefix, from), c)).min().map(|(_, c)| c.to_string())
    }

    /// File of class `head` in package directory `pkg_path`.
    fn class_file(&self, pkg_path: &str, head: &str, from: &str) -> Option<String> {
        let file = format!("{head}.java");
        let tail = if pkg_path.is_empty() { file.clone() } else { format!("{pkg_path}/{file}") };
        let cands = self.by_file_name.get(&file)?;
        self.best(
            cands.iter().filter_map(|c| {
                if c == &tail {
                    Some((c.as_str(), ""))
                } else {
                    c.strip_suffix(&tail).and_then(|p| p.strip_suffix('/')).map(|p| (c.as_str(), p))
                }
            }),
            from,
        )
    }

    /// Directory of package `pkg_path` (directly holding Java files).
    fn package_dir(&self, pkg_path: &str, from: &str) -> Option<String> {
        let last = pkg_path.rsplit('/').next().unwrap_or(pkg_path);
        let cands = self.dirs_by_name.get(last)?;
        self.best(
            cands.iter().filter_map(|d| {
                if d == pkg_path {
                    Some((d.as_str(), ""))
                } else {
                    d.strip_suffix(pkg_path).and_then(|p| p.strip_suffix('/')).map(|p| (d.as_str(), p))
                }
            }),
            from,
        )
    }

    /// [`rueca_extract::LanguagePack::resolve_import`] for Java (module docs).
    pub(crate) fn resolve_import(&self, from: &str, import: &Import) -> Option<String> {
        // The package clause (an import without names) is not an import.
        let name = &import.names.first()?.name;
        if import.module.is_empty() {
            // Classes of the default package cannot be imported.
            return None;
        }
        let pkg_path = import.module.replace('.', "/");
        if name == "*" {
            return self.package_dir(&pkg_path, from);
        }
        let head = name.split('.').next().filter(|h| !h.is_empty() && *h != "*")?;
        self.class_file(&pkg_path, head, from)
    }
}

/// Identity of the inputs a [`Layout`] was built from.
type CacheKey = (usize, usize, Option<String>, Option<String>, Vec<(String, usize, usize)>);

fn cache_key(project: &ProjectFiles) -> CacheKey {
    let builds = project
        .configs
        .iter()
        .filter(|(p, _)| is_build_file(p))
        .map(|(p, t)| (p.clone(), t.as_ptr() as usize, t.len()))
        .collect();
    (
        project as *const ProjectFiles as usize,
        project.paths.len(),
        project.paths.first().cloned(),
        project.paths.last().cloned(),
        builds,
    )
}

/// Process-wide cache of the most recent snapshots' layouts.
#[derive(Default)]
pub(crate) struct LayoutCache {
    entries: Mutex<BTreeMap<u64, (CacheKey, Arc<Layout>)>>,
    counter: Mutex<u64>,
}

/// Snapshots kept in the cache (an MCP server may index several roots).
const CACHE_ENTRIES: usize = 4;

impl LayoutCache {
    pub(crate) fn get(&self, project: &ProjectFiles) -> Arc<Layout> {
        let key = cache_key(project);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (_, l))) = entries.iter().find(|(_, (k, _))| *k == key) {
            return l.clone();
        }
        let built = Arc::new(Layout::build(project));
        let mut counter = self.counter.lock().unwrap_or_else(|e| e.into_inner());
        *counter += 1;
        entries.insert(*counter, (key, built.clone()));
        while entries.len() > CACHE_ENTRIES {
            let oldest = *entries.keys().next().expect("non-empty");
            entries.remove(&oldest);
        }
        built
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_model::{ImportedName, Span};

    fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: configs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        }
    }

    fn import(module: &str, name: &str) -> Import {
        Import {
            module: module.into(),
            relative_level: 0,
            names: vec![ImportedName { name: name.into(), alias: None }],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Span::default(),
        }
    }

    #[test]
    fn maven_multi_module_roots_and_imports() {
        let p = project(
            &[
                "pom.xml",
                "core/pom.xml",
                "core/src/main/java/com/acme/core/Parser.java",
                "core/src/main/java/com/acme/core/Util.java",
                "core/src/test/java/com/acme/core/ParserTest.java",
                "app/pom.xml",
                "app/src/main/java/com/acme/app/App.java",
                "legacy/source/org/old/Thing.java",
                "legacy/pom.xml",
                "gen/src/com/acme/gen/Gen.java",
                "gen/build.gradle",
            ],
            &[
                (
                    "pom.xml",
                    "<project><modules><module>core</module><!-- <module>x</module> --><module>app</module></modules></project>",
                ),
                (
                    "legacy/pom.xml",
                    "<project><build><sourceDirectory>${project.basedir}/source</sourceDirectory></build></project>",
                ),
                ("gen/build.gradle", "sourceSets { main { java { srcDirs = ['src'] } } }"),
            ],
        );
        let l = Layout::build(&p);
        let roots: Vec<&str> = l.roots.iter().map(String::as_str).collect();
        assert_eq!(
            roots,
            ["app/src/main/java", "core/src/main/java", "core/src/test/java", "gen/src", "legacy/source"]
        );
        assert!(l.modules.contains("core") && l.modules.contains("app") && !l.modules.contains("x"));
        assert_eq!(l.package_of_path("core/src/test/java/com/acme/core/ParserTest.java"), "com.acme.core");
        assert_eq!(l.package_of_path("gen/src/com/acme/gen/Gen.java"), "com.acme.gen");
        assert_eq!(l.package_of_path("README.java"), "");

        let from = "app/src/main/java/com/acme/app/App.java";
        let r = |m: &str, n: &str| l.resolve_import(from, &import(m, n));
        assert_eq!(r("com.acme.core", "Parser").as_deref(), Some("core/src/main/java/com/acme/core/Parser.java"));
        assert_eq!(r("com.acme.core", "Util.max").as_deref(), Some("core/src/main/java/com/acme/core/Util.java"));
        assert_eq!(r("com.acme.core", "Parser.Inner").as_deref(), Some("core/src/main/java/com/acme/core/Parser.java"));
        // On demand: the main package directory wins over the test one.
        assert_eq!(r("com.acme.core", "*").as_deref(), Some("core/src/main/java/com/acme/core"));
        assert_eq!(r("org.old", "Thing").as_deref(), Some("legacy/source/org/old/Thing.java"));
        // JDK and third-party: nothing.
        assert_eq!(r("java.util", "List"), None);
        assert_eq!(r("org.junit", "*"), None);
        assert_eq!(r("com.acme.core", "Missing"), None);
        // The package clause and default-package names are never targets.
        let mut clause = import("com.acme.app", "x");
        clause.names.clear();
        assert_eq!(l.resolve_import(from, &clause), None);
        assert_eq!(r("", "Foo"), None);
        // From a test, the test root's package directory wins.
        let t = "core/src/test/java/com/acme/core/ParserTest.java";
        assert_eq!(
            l.resolve_import(t, &import("com.acme.core", "*")).as_deref(),
            Some("core/src/test/java/com/acme/core")
        );
    }

    #[test]
    fn suffix_match_without_build_files() {
        let p = project(&["x/y/com/foo/A.java", "x/y/com/foo/B.java", "other/com/foo/A.java"], &[]);
        let l = Layout::build(&p);
        assert!(l.roots.is_empty());
        // No roots or modules: the candidate closest to the importer, then path order.
        assert_eq!(
            l.resolve_import("x/y/com/foo/B.java", &import("com.foo", "A")).as_deref(),
            Some("x/y/com/foo/A.java")
        );
        assert_eq!(l.resolve_import("z.java", &import("com.foo", "*")).as_deref(), Some("other/com/foo"));
    }

    #[test]
    fn gradle_settings_modules() {
        let p = project(
            &[
                "settings.gradle.kts",
                "lib/build.gradle.kts",
                "lib/src/main/java/a/A.java",
                "tools/cli/src/main/java/b/B.java",
            ],
            &[
                ("settings.gradle.kts", "rootProject.name = \"x\"\ninclude(\"lib\", \":tools:cli\")\n"),
                ("lib/build.gradle.kts", ""),
            ],
        );
        let l = Layout::build(&p);
        assert!(l.modules.contains("lib") && l.modules.contains("tools/cli"));
        assert!(l.roots.contains("tools/cli/src/main/java"));
    }

    #[test]
    fn cache_reuses_layouts() {
        let p = project(&["src/main/java/a/A.java"], &[]);
        let cache = LayoutCache::default();
        let a = cache.get(&p);
        let b = cache.get(&p);
        assert!(Arc::ptr_eq(&a, &b));
    }
}
