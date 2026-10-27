//! The organization link layer, packages (RFC 0006 §3.3.1): package
//! registries from manifests and cross-repository edges, the
//! [`OrgLinker`] of `rueca index --org`.
//!
//! Everything is computed from the store: a segment's file table names each
//! file's blob and language, its facts come from the facts cache (or are
//! extracted again from the content packs when the cache entry is gone), and
//! its manifests and module-resolution configs from the content packs.
//!
//! # Analysis ([`PackageLinker::analyze`])
//!
//! - **Packages** a repository publishes, with the import keys that name
//!   them: every `package.json` with a `name` (`npm:<name>`); a Python
//!   distribution (`pyproject.toml` `[project]` / `[tool.poetry]` name,
//!   `setup.cfg` `[metadata]` name, `setup.py` `name=`) with its top-level
//!   import packages (`py:<package>`, from the files' module names; test,
//!   documentation and example trees left out); every `go.mod` module
//!   (`go:<module path>`); `composer.json` name with its PSR-4 / PSR-0
//!   namespace prefixes (`php:<Namespace>`); Maven `groupId:artifactId`
//!   (`pom.xml`, the parent's group when the project has none) and Gradle
//!   (`settings.gradle` root project, `build.gradle` `group`) with the Java
//!   packages under the build file's directory (`java:<package>`); C / C++
//!   repositories publish the directories and headers directly under their
//!   `include/` (or `single_include/`) directories (`c:<dir>`).
//! - **Imports** the repository's own pack cannot resolve inside it (the
//!   ones a single-repository index reports as external), as keys:
//!   `npm:<package name>`, `py:<module>`, `go:<import path>`,
//!   `java:<package>`, `php:<name>`, `c:<include path>`.
//! - **Requires**: dependencies declared by manifests (`package.json`
//!   dependencies, `composer.json` require, `go.mod` require, PEP 621
//!   dependencies, Maven dependencies), for languages whose code can use a
//!   package without an import statement (PHP fully qualified names).
//!
//! # Links ([`PackageLinker::link`])
//!
//! The consumer and its dependencies are resolved **together** by the
//! normal resolver: the dependencies' files are mounted under
//! `@rueca-org/<k>/`, and every language pack is wrapped so that
//!
//! - a dependency's file keeps its own module name and resolves its own
//!   imports with its repository's project (configs, workspaces, source
//!   roots, `go.mod`, Composer autoload) — the paths it returns are mounted;
//! - an import the consumer cannot resolve inside itself, whose key names a
//!   package of a mounted dependency, is resolved by **that repository's
//!   pack against that repository's project** (`vue` → its workspace
//!   package entry, `click` → `src/click/__init__.py`,
//!   `github.com/gin-gonic/gin` → the root package, `Carbon\Carbon` →
//!   `src/Carbon/Carbon.php` by its PSR-4 map).
//!
//! The strategy ladder then binds names through those imports as it does
//! inside one repository, so the edges carry the strategy, confidence and
//! evidence they would have there. Kept: edges between a consumer node and
//! a dependency node (consumer → dependency, and dependency → consumer for
//! `DECORATES`: a library decorator on a consumer definition), of every
//! kind but `CONTAINS` / `DEFINES`, except the
//! name-matching rungs (`unique_name`, `heuristic`), which have no import
//! behind them across repositories, and edges into a dependency's tests or
//! into a dependency definition the consumer defines itself (same label and
//! qualified name: a stub or a vendored copy — the consumer's own wins). Nodes are mapped back to the
//! repositories' own segments by (label, qualified name, path); a folder
//! import of a dependency's root package maps to its `Repository` node.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles, Registry};
use rueca_model::{
    EdgeKind, FileFacts, FileId, Import, Label, Language, LanguageFamily, NodeId, Strategy, git_blob_sha,
};
use rueca_resolve::FileInput;
use rueca_segment::org::{LinkEdge, LinkInput, OrgLinker, Package, RepoDeps};
use rueca_segment::{Segment, SegmentError, Store};

use crate::default_registry;

/// Version of the analysis and link output (part of every profile and link
/// set; bumping it recomputes them).
pub const LINKER_VERSION: u32 = 1;

/// Path prefix of mounted dependencies in a link resolution.
const MOUNT: &str = "@rueca-org/";

/// Top-level Python packages never published (tests, docs, tooling).
const PY_NOT_PUBLISHED: &[&str] = &[
    "test",
    "tests",
    "testing",
    "doc",
    "docs",
    "example",
    "examples",
    "script",
    "scripts",
    "benchmark",
    "benchmarks",
    "bench",
    "tools",
    "tool",
    "ci",
    "setup",
    "conftest",
    "noxfile",
    "tasks",
    "build",
    "dist",
    "artwork",
];

/// The linker of `rueca index --org`.
pub struct PackageLinker {
    facts_dir: PathBuf,
    budget: Budget,
}

impl PackageLinker {
    pub fn new(store: &Path) -> PackageLinker {
        PackageLinker { facts_dir: store.join("facts"), budget: Budget::default() }
    }
}

fn other(e: impl std::fmt::Display) -> SegmentError {
    SegmentError::Other(e.to_string())
}

// ---------------------------------------------------------------------------
// Loading a segment's facts
// ---------------------------------------------------------------------------

/// `<sha>-<lang>-v<version>-f<FACTS_FORMAT>[-p<path hash>].bin` under
/// `facts_dir` (the indexer's facts cache).
pub(crate) fn facts_cache_path(
    facts_dir: &Path,
    sha: &str,
    lang: Language,
    version: u32,
    path: Option<&str>,
) -> PathBuf {
    let prefix = sha.get(..2).unwrap_or("00");
    let path_part = path.map(|p| format!("-p{}", &git_blob_sha(p.as_bytes())[..16])).unwrap_or_default();
    facts_dir.join(prefix).join(format!(
        "{sha}-{}-v{version}-f{}{path_part}.bin",
        lang.as_str(),
        rueca_model::FACTS_FORMAT
    ))
}

/// A segment's files as resolver inputs, and its project.
struct Loaded {
    files: Vec<FileInput>,
    project: ProjectFiles,
}

impl PackageLinker {
    fn load(&self, seg: &Segment, registry: &Registry) -> Loaded {
        let mut files = Vec::with_capacity(seg.files().len());
        let mut configs = BTreeMap::new();
        for (i, f) in seg.files().iter().enumerate() {
            if ProjectFiles::is_config_file(&f.path)
                && f.size <= ProjectFiles::max_config_bytes(&f.path)
                && let Ok(bytes) = seg.content(i as FileId)
                && let Ok(text) = String::from_utf8(bytes)
            {
                configs.insert(f.path.clone(), text);
            }
            let extracted = f.status == "ok" || f.status == "partial";
            let facts = match (f.language, extracted) {
                (Some(lang), true) => registry.for_language(lang).and_then(|pack| self.facts(seg, i, lang, pack)),
                _ => None,
            };
            files.push(FileInput {
                path: f.path.clone(),
                blob_sha: f.blob_sha.clone(),
                language: f.language,
                size: f.size,
                facts,
                status: f.status.clone(),
            });
        }
        let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs };
        Loaded { files, project }
    }

    /// Facts of file `i` from the cache, else extracted from the content packs.
    fn facts(&self, seg: &Segment, i: usize, lang: Language, pack: &dyn LanguagePack) -> Option<FileFacts> {
        let f = &seg.files()[i];
        let key = pack.facts_depend_on_path().then_some(f.path.as_str());
        let cache = facts_cache_path(&self.facts_dir, &f.blob_sha, lang, pack.extractor_version(), key);
        if let Ok(compressed) = fs::read(&cache)
            && let Ok(raw) = zstd::decode_all(compressed.as_slice())
            && let Ok(facts) = postcard::from_bytes::<FileFacts>(&raw)
        {
            return Some(facts);
        }
        let bytes = seg.content(i as FileId).ok()?;
        match catch_unwind(AssertUnwindSafe(|| pack.extract(&f.path, lang, &bytes, &self.budget))) {
            Ok(Ok(facts)) => Some(facts),
            Ok(Err(ExtractError::Skipped(_))) | Ok(Err(_)) | Err(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Manifests
// ---------------------------------------------------------------------------

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn under(dir: &str, path: &str) -> bool {
    dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Paths of vendored or third-party trees, whose manifests are not the
/// repository's.
fn foreign(path: &str) -> bool {
    path.split('/').any(|s| matches!(s, "node_modules" | "vendor" | "third_party" | "third-party" | "fixtures"))
}

/// The `"..."` value of a TOML / INI `key = "value"` line inside `[section]`
/// (any section when `section` is empty).
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            current = section_name(l);
            continue;
        }
        if !section.is_empty() && current != section {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        if k.trim() == key {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Quoted strings of a TOML array that starts on the line `key = [` of
/// `[section]` (PEP 621 `dependencies`).
fn toml_array(text: &str, section: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for line in text.lines() {
        let l = line.trim();
        if inside {
            collect_quoted(l, &mut out);
            inside = !l.contains(']');
        } else if l.starts_with('[') && !l.contains('=') {
            current = section_name(l);
        } else if current == section
            && let Some(v) = array_start(l, key)
        {
            collect_quoted(v, &mut out);
            inside = !v.contains(']');
        }
    }
    out
}

/// The name of a `[section]` header line.
fn section_name(line: &str) -> String {
    line.trim_matches(|c| c == '[' || c == ']').trim().to_string()
}

/// The value of a `key = [...` line that opens an array.
fn array_start<'l>(line: &'l str, key: &str) -> Option<&'l str> {
    let (k, v) = line.split_once('=')?;
    (k.trim() == key && v.trim().starts_with('[')).then_some(v)
}

fn collect_quoted(s: &str, out: &mut Vec<String>) {
    for (i, part) in s.split(['"', '\'']).enumerate() {
        if i % 2 == 1 && !part.is_empty() {
            out.push(part.to_string());
        }
    }
}

/// A PEP 508 requirement's distribution name.
fn requirement_name(req: &str) -> &str {
    let end = req.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))).unwrap_or(req.len());
    &req[..end]
}

/// Text of the first `<tag>...</tag>` of `xml`.
fn xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&close)?;
    Some(xml[start..start + len].trim())
}

/// `xml` without the `<tag>...</tag>` blocks.
fn xml_without(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        out.push_str(&rest[..i]);
        match rest[i..].find(&close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every `<dependency>` `groupId:artifactId` of a POM.
fn pom_dependencies(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<dependency>") {
        let Some(j) = rest[i..].find("</dependency>") else { break };
        let block = &rest[i..i + j];
        if let (Some(g), Some(a)) = (xml_tag(block, "groupId"), xml_tag(block, "artifactId")) {
            out.push(format!("{g}:{a}"));
        }
        rest = &rest[i + j..];
    }
    out
}

fn json_keys(v: &serde_json::Value, field: &str) -> Vec<String> {
    v.get(field).and_then(|d| d.as_object()).map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// Packages and required packages declared by the manifests of a project.
/// `modules` gives each file's module name (Python packages, Java packages).
fn manifests(project: &ProjectFiles, files: &[FileModule]) -> (Vec<Package>, Vec<String>) {
    let mut m = Manifests::default();
    for (path, text) in &project.configs {
        if !foreign(path) {
            m.read(path, text, files);
        }
    }
    let Manifests { mut packages, requires, python_name } = m;
    if let Some((dir, name)) = python_name {
        packages.push(python_package(&python_top_packages(files), dir, name));
    }
    packages.extend(c_package(files));
    for p in &mut packages {
        p.keys.sort();
        p.keys.dedup();
    }
    packages.sort_by(|a, b| (&a.eco, &a.dir, &a.name).cmp(&(&b.eco, &b.dir, &b.name)));
    (packages, requires.into_iter().collect())
}

/// A file of a project: (path, language, module name).
type FileModule = (String, Option<Language>, String);

/// What the manifests of a project declare, read one by one.
#[derive(Default)]
struct Manifests {
    packages: Vec<Package>,
    requires: BTreeSet<String>,
    /// The Python distribution: (directory, name) of the shallowest
    /// manifest that names one.
    python_name: Option<(String, String)>,
}

impl Manifests {
    fn read(&mut self, path: &str, text: &str, files: &[FileModule]) {
        let dir = dir_of(path).to_string();
        match file_name(path) {
            "package.json" => self.package_json(text, dir),
            "composer.json" => self.composer_json(text, dir),
            "go.mod" => self.go_mod(text, dir),
            "pyproject.toml" | "setup.cfg" | "setup.py" if !dir.contains('/') => self.python_manifest(path, text, dir),
            "pom.xml" => self.pom(text, &dir, files),
            "settings.gradle" | "settings.gradle.kts" => self.gradle_settings(text, &dir, files),
            _ => {}
        }
    }

    fn package_json(&mut self, text: &str, dir: String) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        if let Some(name) = json_name(&v) {
            self.packages.push(Package {
                eco: "npm".into(),
                name: name.to_string(),
                dir,
                keys: vec![format!("npm:{name}")],
            });
        }
        for field in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
            self.requires.extend(json_keys(&v, field).into_iter().map(|d| format!("npm:{}", d.to_lowercase())));
        }
    }

    fn composer_json(&mut self, text: &str, dir: String) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        if let Some(name) = json_name(&v) {
            let keys = composer_namespaces(&v);
            self.packages.push(Package { eco: "composer".into(), name: name.to_string(), dir, keys });
        }
        for field in ["require", "require-dev"] {
            self.requires.extend(json_keys(&v, field).into_iter().map(|d| format!("composer:{}", d.to_lowercase())));
        }
    }

    fn go_mod(&mut self, text: &str, dir: String) {
        for line in text.lines() {
            if let Some(m) = line.trim().strip_prefix("module ") {
                let m = m.trim().trim_matches('"');
                self.packages.push(Package {
                    eco: "go".into(),
                    name: m.to_string(),
                    dir: dir.clone(),
                    keys: vec![format!("go:{m}")],
                });
            }
        }
        self.requires.extend(go_requires(text));
    }

    /// `pyproject.toml` (PEP 621 / Poetry), `setup.cfg` or `setup.py` at
    /// the root or one directory down.
    fn python_manifest(&mut self, path: &str, text: &str, dir: String) {
        let file = file_name(path);
        let name = match file {
            "pyproject.toml" => ini_value(text, "project", "name").or_else(|| ini_value(text, "tool.poetry", "name")),
            "setup.cfg" => ini_value(text, "metadata", "name"),
            _ => setup_py_name(text),
        };
        if file == "pyproject.toml" {
            self.requires.extend(pep621_requires(text));
        }
        if let Some(n) = name
            && self.python_name.as_ref().is_none_or(|(d, _)| dir.len() < d.len())
        {
            self.python_name = Some((dir, n));
        }
    }

    fn pom(&mut self, text: &str, dir: &str, files: &[FileModule]) {
        self.requires.extend(pom_dependencies(text).into_iter().map(|d| format!("maven:{}", d.to_lowercase())));
        let parent_group = xml_tag(text, "parent").and_then(|p| xml_tag(p, "groupId")).map(str::to_string);
        let mut own = text.to_string();
        for tag in ["parent", "dependencies", "dependencyManagement", "build", "profiles", "reporting"] {
            own = xml_without(&own, tag);
        }
        let group = xml_tag(&own, "groupId").map(str::to_string).or(parent_group);
        if let (Some(g), Some(a)) = (group, xml_tag(&own, "artifactId")) {
            self.packages.push(java_package(format!("{g}:{a}"), dir, files));
        }
    }

    fn gradle_settings(&mut self, text: &str, dir: &str, files: &[FileModule]) {
        let name = text.lines().find_map(|l| {
            l.trim()
                .strip_prefix("rootProject.name")
                .map(|r| r.trim_start_matches([' ', '=']).trim_matches(['"', '\'', ' ']))
        });
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            self.packages.push(java_package(n.to_string(), dir, files));
        }
    }
}

/// A manifest's non-empty `name`.
fn json_name(v: &serde_json::Value) -> Option<&str> {
    v.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty())
}

/// The PSR-4 and PSR-0 namespace prefixes of a `composer.json`.
fn composer_namespaces(v: &serde_json::Value) -> Vec<String> {
    let autoload = v.get("autoload").unwrap_or(&serde_json::Value::Null);
    ["psr-4", "psr-0"]
        .into_iter()
        .flat_map(|kind| json_keys(autoload, kind))
        .map(|ns| ns.trim_end_matches('\\').to_string())
        .filter(|ns| !ns.is_empty())
        .map(|ns| format!("php:{ns}"))
        .collect()
}

/// The modules a `go.mod` requires (single lines and `require (…)` blocks).
fn go_requires(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("require (") {
            in_block = true;
            continue;
        }
        if in_block && l.starts_with(')') {
            in_block = false;
            continue;
        }
        let spec = if in_block { Some(l) } else { l.strip_prefix("require ") };
        if let Some(p) = spec.and_then(|s| s.split_whitespace().next()).filter(|p| !p.starts_with("//")) {
            out.push(format!("go:{}", p.to_lowercase()));
        }
    }
    out
}

/// The quoted `name=` argument of a `setup.py`.
fn setup_py_name(text: &str) -> Option<String> {
    let r = text.split("name=").nth(1).or_else(|| text.split("name =").nth(1))?.trim_start();
    let q = r.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    r[1..].split(q).next().map(str::to_string)
}

/// PEP 621 `[project] dependencies` as `pypi:` keys.
fn pep621_requires(text: &str) -> Vec<String> {
    toml_array(text, "project", "dependencies")
        .iter()
        .map(|req| requirement_name(req))
        .filter(|n| !n.is_empty())
        .map(|n| format!("pypi:{}", rueca_segment::org::link::normalize_package("pypi", n)))
        .collect()
}

/// Python top-level packages: the first segment of the modules inside a
/// package directory (not tests, documentation, tooling or private).
fn python_top_packages(files: &[FileModule]) -> BTreeSet<String> {
    let mut py_top = BTreeSet::new();
    for f in files.iter().filter(|f| is_source(f, LanguageFamily::Python)) {
        let is_pkg = f.2.contains('.') || file_name(&f.0).starts_with("__init__.");
        if let Some(top) = f.2.split('.').next()
            && is_pkg
            && !top.is_empty()
            && !PY_NOT_PUBLISHED.contains(&top)
            && !top.starts_with('_')
        {
            py_top.insert(top.to_string());
        }
    }
    py_top
}

/// A non-test, non-vendored source file of `family`.
fn is_source(f: &FileModule, family: LanguageFamily) -> bool {
    f.1.is_some_and(|l| l.family() == family) && !rueca_segment::is_test_path(&f.0) && !foreign(&f.0)
}

/// The Python distribution `name` (in `dir`) with its import keys: its
/// top-level packages, plus the normalized distribution name when it is
/// not one of them.
fn python_package(py_top: &BTreeSet<String>, dir: String, name: String) -> Package {
    let mut keys: Vec<String> = py_top.iter().map(|t| format!("py:{t}")).collect();
    let norm = name.to_lowercase().replace(['-', '.'], "_");
    if keys.is_empty() || !py_top.contains(&norm) {
        keys.push(format!("py:{norm}"));
    }
    keys.sort();
    keys.dedup();
    Package { eco: "pypi".into(), name, dir, keys }
}

/// C / C++: what `include/` (or `single_include/`) directories expose.
fn c_package(files: &[FileModule]) -> Option<Package> {
    let mut c_keys: BTreeSet<String> = BTreeSet::new();
    for f in files {
        if !f.1.is_some_and(|l| l.family() == LanguageFamily::C) || foreign(&f.0) {
            continue;
        }
        let parts: Vec<&str> = f.0.split('/').collect();
        if let Some(i) = parts.iter().position(|p| matches!(*p, "include" | "single_include"))
            && i <= 1
            && i + 1 < parts.len()
        {
            c_keys.insert(format!("c:{}", parts[i + 1]));
        }
    }
    (!c_keys.is_empty()).then(|| Package {
        eco: "c".into(),
        name: String::new(),
        dir: String::new(),
        keys: c_keys.into_iter().collect(),
    })
}

/// A Java build (Maven / Gradle) publishing the packages of the non-test
/// Java files under `dir`.
fn java_package(name: String, dir: &str, files: &[FileModule]) -> Package {
    let keys: BTreeSet<String> = files
        .iter()
        .filter(|f| {
            f.1 == Some(Language::Java) && under(dir, &f.0) && !rueca_segment::is_test_path(&f.0) && !f.2.is_empty()
        })
        .map(|f| format!("java:{}", f.2))
        .collect();
    Package { eco: "maven".into(), name, dir: dir.to_string(), keys: keys.into_iter().collect() }
}

/// The import key of an import a pack could not resolve inside its
/// repository, or `None` for relative imports and languages without
/// cross-repository packages.
pub(crate) fn import_key(family: LanguageFamily, imp: &Import) -> Option<String> {
    let m = imp.module.trim();
    if m.is_empty() {
        return None;
    }
    match family {
        LanguageFamily::EcmaScript => {
            if m.starts_with('.') || m.starts_with('/') || m.contains(':') {
                return None;
            }
            let mut parts = m.split('/');
            let first = parts.next()?;
            let name = if first.starts_with('@') { format!("{first}/{}", parts.next()?) } else { first.to_string() };
            Some(format!("npm:{name}"))
        }
        LanguageFamily::Python => (imp.relative_level == 0).then(|| format!("py:{m}")),
        LanguageFamily::Go => Some(format!("go:{m}")),
        LanguageFamily::Java => Some(format!("java:{m}")),
        LanguageFamily::Php => Some(format!("php:{}", m.trim_start_matches('\\'))),
        LanguageFamily::C => Some(format!("c:{}", m.trim_start_matches("./"))),
        LanguageFamily::Markdown | LanguageFamily::Data => None,
    }
}

// ---------------------------------------------------------------------------
// The linker
// ---------------------------------------------------------------------------

impl OrgLinker for PackageLinker {
    fn version(&self) -> u32 {
        LINKER_VERSION
    }

    fn analyze(&self, _store: &Store, seg: &Segment) -> rueca_segment::Result<RepoDeps> {
        let registry = default_registry();
        let loaded = self.load(seg, &registry);
        let modules: Vec<FileModule> = seg
            .files()
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), f.language, seg.module_of(i as FileId).unwrap_or_default().to_string()))
            .collect();
        let (packages, requires) = manifests(&loaded.project, &modules);
        let mut imports = BTreeSet::new();
        let resolved = catch_unwind(AssertUnwindSafe(|| {
            for f in &loaded.files {
                let (Some(lang), Some(facts)) = (f.language, f.facts.as_ref()) else { continue };
                let Some(pack) = registry.for_language(lang) else { continue };
                for imp in &facts.imports {
                    let Some(key) = import_key(lang.family(), imp) else { continue };
                    if imports.contains(&key) {
                        continue;
                    }
                    if pack.resolve_import(&f.path, imp, &loaded.project).is_none() {
                        imports.insert(key);
                    }
                }
            }
        }));
        if resolved.is_err() {
            return Err(other("a language pack panicked while resolving imports"));
        }
        Ok(RepoDeps { packages, imports: imports.into_iter().collect(), requires })
    }

    fn link(
        &self,
        _store: &Store,
        consumer: &LinkInput<'_>,
        deps: &[LinkInput<'_>],
    ) -> rueca_segment::Result<Vec<LinkEdge>> {
        let registry = default_registry();
        let own = self.load(consumer.segment, &registry);
        let mut mounts = Vec::with_capacity(deps.len());
        let mut files = own.files;
        for (k, d) in deps.iter().enumerate() {
            let loaded = self.load(d.segment, &registry);
            let prefix = format!("{MOUNT}{k}/");
            files.extend(loaded.files.into_iter().map(|mut f| {
                f.path = format!("{prefix}{}", f.path);
                f
            }));
            mounts.push(Mount { prefix, project: loaded.project, packages: d.deps.packages.clone() });
        }
        let ctx = Arc::new(LinkCtx { consumer: own.project, mounts });
        let mut wrapped = Registry::new();
        for pack in registry.into_packs() {
            wrapped.register(Box::new(LinkPack { inner: pack, ctx: ctx.clone() }));
        }
        let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs: BTreeMap::new() };
        let snapshot = consumer.segment.snapshot().clone();
        let out = catch_unwind(AssertUnwindSafe(|| rueca_resolve::resolve(snapshot, files, &project, &wrapped)))
            .map_err(|_| other("the resolver panicked while linking"))?;
        Ok(crossing_edges(&out.graph, &ctx, consumer.segment, deps))
    }
}

/// Node key of a segment: (label, qualified name, path of its file).
type NodeKey<'a> = (Label, &'a str, Option<&'a str>);

fn node_index(seg: &Segment) -> HashMap<NodeKey<'_>, NodeId> {
    let files = seg.files();
    let mut out = HashMap::with_capacity(seg.nodes().len());
    for (i, n) in seg.nodes().iter().enumerate() {
        let path = n.file.and_then(|f| files.get(f as usize)).map(|f| f.path.as_str());
        out.entry((n.label, n.qualified_name.as_str(), path)).or_insert(i as NodeId);
    }
    out
}

/// The edges of the joint graph from the consumer into a mounted
/// dependency, mapped to the repositories' own node ids (see the module
/// docs).
fn crossing_edges(
    graph: &rueca_model::ResolvedGraph,
    ctx: &LinkCtx,
    consumer: &Segment,
    deps: &[LinkInput<'_>],
) -> Vec<LinkEdge> {
    let map = JointMap {
        graph,
        ctx,
        own_index: node_index(consumer),
        dep_index: deps.iter().map(|d| node_index(d.segment)).collect(),
    };
    // Names the consumer defines itself: a dependency's definition with the
    // same label and qualified name (a test stub, a vendored copy) never
    // stands for it.
    let own_names: HashSet<(Label, &str)> = consumer
        .nodes()
        .iter()
        .filter(|n| !matches!(n.label, Label::Repository | Label::Folder | Label::File))
        .map(|n| (n.label, n.qualified_name.as_str()))
        .collect();
    let mut best: BTreeMap<(NodeId, u32, NodeId, EdgeKind, bool), LinkEdge> = BTreeMap::new();
    for e in &graph.edges {
        if matches!(e.kind, EdgeKind::Contains | EdgeKind::Defines)
            || matches!(e.meta.strategy, Strategy::UniqueName | Strategy::Heuristic)
        {
            continue;
        }
        let Some((src, k, dst, reversed)) = map.crossing(e) else { continue };
        if !usable_target(&deps[k], &own_names, dst) {
            continue;
        }
        let mut meta = e.meta;
        meta.evidence = meta.evidence.and_then(|(f, span)| {
            let path = graph.files.get(f as usize)?.path.as_str();
            if ctx.mount_of(path).is_some() {
                return None;
            }
            consumer.file_by_path(path).map(|fid| (fid, span))
        });
        let edge = LinkEdge { src, dep: k as u32, dst, kind: e.kind, meta, reversed };
        keep_most_confident(&mut best, edge);
    }
    best.into_values().collect()
}

/// Keeps `edge` unless an edge with the same key and at least its
/// confidence is kept.
fn keep_most_confident(best: &mut BTreeMap<(NodeId, u32, NodeId, EdgeKind, bool), LinkEdge>, edge: LinkEdge) {
    match best.entry((edge.src, edge.dep, edge.dst, edge.kind, edge.reversed)) {
        std::collections::btree_map::Entry::Vacant(v) => {
            v.insert(edge);
        }
        std::collections::btree_map::Entry::Occupied(mut o) => {
            if edge.meta.confidence > o.get().meta.confidence {
                o.insert(edge);
            }
        }
    }
}

/// Whether a dependency node may be the target of a link: not a test, not
/// in a test file, not named like a definition of the consumer.
fn usable_target(dep: &LinkInput<'_>, own_names: &HashSet<(Label, &str)>, d: NodeId) -> bool {
    let seg = dep.segment;
    let n = seg.node(d);
    let test_file = n.file.is_some_and(|f| rueca_segment::is_test_path(&seg.file(f).path));
    !(n.is_test || test_file || own_names.contains(&(n.label, n.qualified_name.as_str())))
}

/// The nodes of a joint graph mapped back to the repositories' own
/// segments.
struct JointMap<'g> {
    graph: &'g rueca_model::ResolvedGraph,
    ctx: &'g LinkCtx,
    own_index: HashMap<NodeKey<'g>, NodeId>,
    dep_index: Vec<HashMap<NodeKey<'g>, NodeId>>,
}

impl JointMap<'_> {
    /// Side of a joint node — `None` = consumer, `Some(k)` = mount `k` — and
    /// its id in that side's segment.
    fn locate(&self, n: NodeId) -> Option<(Option<usize>, NodeId)> {
        let node = self.graph.nodes.get(n as usize)?;
        let path = match node.file {
            Some(f) => Some(self.graph.files.get(f as usize)?.path.as_str()),
            None if node.label == Label::Folder => None,
            None => return None,
        };
        let full = path.unwrap_or(node.qualified_name.as_str());
        let Some((k, rel)) = self.ctx.mount_of(full) else {
            let id = self.own_index.get(&(node.label, node.qualified_name.as_str(), path))?;
            return Some((None, *id));
        };
        if node.label == Label::Folder && rel.is_empty() {
            // The dependency's root package: its Repository node.
            return Some((Some(k), 0));
        }
        let qn = if matches!(node.label, Label::File | Label::Folder) { rel } else { node.qualified_name.as_str() };
        let id = self.dep_index[k].get(&(node.label, qn, path.map(|_| rel)))?;
        Some((Some(k), *id))
    }

    /// A consumer → dependency edge, or (`DECORATES`) dependency →
    /// consumer: (consumer node, mount, dependency node, reversed).
    fn crossing(&self, e: &rueca_model::Edge) -> Option<(NodeId, usize, NodeId, bool)> {
        match (self.locate(e.src)?, self.locate(e.dst)?) {
            ((None, s), (Some(k), d)) => Some((s, k, d, false)),
            ((Some(k), d), (None, s)) => Some((s, k, d, true)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The pack wrapper of a link resolution
// ---------------------------------------------------------------------------

struct Mount {
    prefix: String,
    project: ProjectFiles,
    packages: Vec<Package>,
}

impl Mount {
    /// A repository-relative path of the dependency, mounted (`""`, the
    /// root directory, becomes the mount directory).
    fn mounted(&self, rel: &str) -> String {
        if rel.is_empty() { self.prefix.trim_end_matches('/').to_string() } else { format!("{}{rel}", self.prefix) }
    }
}

struct LinkCtx {
    consumer: ProjectFiles,
    mounts: Vec<Mount>,
}

impl LinkCtx {
    /// The mount of a joint path and the path inside it.
    fn mount_of<'a>(&self, path: &'a str) -> Option<(usize, &'a str)> {
        let rest = path.strip_prefix(MOUNT)?;
        let (k, rel) = rest.split_once('/').unwrap_or((rest, ""));
        let k: usize = k.parse().ok()?;
        (k < self.mounts.len()).then_some((k, rel))
    }

    /// Resolves an import nobody resolved inside its own repository against
    /// the mounted repositories publishing a package its key names, with
    /// that repository's project.
    fn link_import(
        &self,
        pack: &dyn LanguagePack,
        family: LanguageFamily,
        imp: &Import,
        skip: Option<usize>,
    ) -> Option<String> {
        let key = import_key(family, imp)?;
        for (k, m) in self.mounts.iter().enumerate() {
            if Some(k) == skip {
                continue;
            }
            for p in &m.packages {
                if !p.keys.iter().any(|pk| rueca_segment::org::link::import_matches(pk, &key)) {
                    continue;
                }
                let from =
                    if p.dir.is_empty() { "__rueca_link__".to_string() } else { format!("{}/__rueca_link__", p.dir) };
                if let Some(hit) = pack.resolve_import(&from, imp, &m.project) {
                    return Some(m.mounted(&hit));
                }
            }
        }
        None
    }
}

/// A language pack seen through the mounts of a link resolution (see the
/// module docs).
struct LinkPack {
    inner: Box<dyn LanguagePack>,
    ctx: Arc<LinkCtx>,
}

impl LinkPack {
    fn family(&self) -> Option<LanguageFamily> {
        self.inner.languages().first().map(|l| l.family())
    }
}

impl LanguagePack for LinkPack {
    fn languages(&self) -> &'static [Language] {
        self.inner.languages()
    }
    fn extractor_version(&self) -> u32 {
        self.inner.extractor_version()
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        self.inner.extract(path, language, source, budget)
    }
    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        match self.ctx.mount_of(path) {
            Some((k, rel)) => self.inner.module_name(rel, &self.ctx.mounts[k].project),
            None => self.inner.module_name(path, &self.ctx.consumer),
        }
    }
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, _project: &ProjectFiles) -> String {
        match self.ctx.mount_of(path) {
            Some((k, rel)) => self.inner.module_name_for(rel, facts, &self.ctx.mounts[k].project),
            None => self.inner.module_name_for(path, facts, &self.ctx.consumer),
        }
    }
    fn resolve_import(&self, from_path: &str, import: &Import, _project: &ProjectFiles) -> Option<String> {
        let family = self.family()?;
        match self.ctx.mount_of(from_path) {
            Some((k, rel)) => {
                let m = &self.ctx.mounts[k];
                if let Some(hit) = self.inner.resolve_import(rel, import, &m.project) {
                    return Some(m.mounted(&hit));
                }
                self.ctx.link_import(self.inner.as_ref(), family, import, Some(k))
            }
            None => self
                .inner
                .resolve_import(from_path, import, &self.ctx.consumer)
                .or_else(|| self.ctx.link_import(self.inner.as_ref(), family, import, None)),
        }
    }
    fn is_builtin(&self, name: &str) -> bool {
        self.inner.is_builtin(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        self.inner.self_receivers()
    }
    fn module_scope(&self, language: Language) -> ModuleScope {
        self.inner.module_scope(language)
    }
    fn implicit_self(&self) -> bool {
        self.inner.implicit_self()
    }
    fn overloads(&self) -> bool {
        self.inner.overloads()
    }
    fn overrides(&self) -> bool {
        self.inner.overrides()
    }
    fn facts_depend_on_path(&self) -> bool {
        self.inner.facts_depend_on_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn import_keys() {
        assert_eq!(
            import_key(LanguageFamily::EcmaScript, &imp("@vue/runtime-core/dist", 0)).as_deref(),
            Some("npm:@vue/runtime-core")
        );
        assert_eq!(import_key(LanguageFamily::EcmaScript, &imp("lodash/fp", 0)).as_deref(), Some("npm:lodash"));
        assert_eq!(import_key(LanguageFamily::EcmaScript, &imp("./x", 0)), None);
        assert_eq!(import_key(LanguageFamily::EcmaScript, &imp("node:fs", 0)), None);
        assert_eq!(import_key(LanguageFamily::Python, &imp("click.core", 0)).as_deref(), Some("py:click.core"));
        assert_eq!(import_key(LanguageFamily::Python, &imp("core", 1)), None);
        assert_eq!(import_key(LanguageFamily::Php, &imp("\\Carbon\\Carbon", 0)).as_deref(), Some("php:Carbon\\Carbon"));
        assert_eq!(import_key(LanguageFamily::Data, &imp("x", 0)), None);
    }

    #[test]
    fn manifest_parsing() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "pyproject.toml".to_string(),
            "[project]\nname = \"Click\"\ndependencies = [\n  \"colorama; platform_system == 'Windows'\",\n  \"Jinja2>=3\",\n]\n".to_string(),
        );
        configs.insert(
            "composer.json".to_string(),
            r#"{"name": "nesbot/carbon", "autoload": {"psr-4": {"Carbon\\": "src/Carbon/"}}, "require": {"Symfony/Clock": "^6"}}"#.to_string(),
        );
        configs.insert(
            "go.mod".to_string(),
            "module github.com/a/lib\n\nrequire (\n\tgithub.com/b/c v1.0.0\n)\nrequire github.com/d/e v2\n".to_string(),
        );
        configs.insert(
            "pom.xml".to_string(),
            "<project><parent><groupId>org.p</groupId><artifactId>parent</artifactId></parent><artifactId>gson</artifactId>\
             <dependencies><dependency><groupId>junit</groupId><artifactId>junit</artifactId></dependency></dependencies></project>"
                .to_string(),
        );
        configs.insert("node_modules/x/package.json".to_string(), r#"{"name": "x"}"#.to_string());
        configs.insert(
            "packages/ui/package.json".to_string(),
            r#"{"name": "@acme/ui", "dependencies": {"React": "1"}}"#.to_string(),
        );
        let project = ProjectFiles { paths: Default::default(), configs };
        let files = vec![
            ("src/click/__init__.py".to_string(), Some(Language::Python), "click".to_string()),
            ("src/click/core.py".to_string(), Some(Language::Python), "click.core".to_string()),
            ("tests/test_x.py".to_string(), Some(Language::Python), "tests.test_x".to_string()),
            ("setup.py".to_string(), Some(Language::Python), "setup".to_string()),
            ("src/main/java/com/g/Gson.java".to_string(), Some(Language::Java), "com.g".to_string()),
            ("include/fmt/core.h".to_string(), Some(Language::Cpp), String::new()),
        ];
        let (packages, requires) = manifests(&project, &files);
        let find = |eco: &str| packages.iter().find(|p| p.eco == eco).unwrap_or_else(|| panic!("{eco}: {packages:?}"));
        assert_eq!(find("pypi").keys, ["py:click"]);
        assert_eq!(find("composer").keys, ["php:Carbon"]);
        assert_eq!(find("go").name, "github.com/a/lib");
        assert_eq!(
            (find("maven").name.as_str(), find("maven").keys.clone()),
            ("org.p:gson", vec!["java:com.g".to_string()])
        );
        assert_eq!(find("c").keys, ["c:fmt"]);
        assert_eq!(
            packages.iter().filter(|p| p.eco == "npm").map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["@acme/ui"]
        );
        for r in [
            "pypi:colorama",
            "pypi:jinja2",
            "composer:symfony/clock",
            "go:github.com/b/c",
            "go:github.com/d/e",
            "maven:junit:junit",
            "npm:react",
        ] {
            assert!(requires.contains(&r.to_string()), "{r}: {requires:?}");
        }
    }
}
