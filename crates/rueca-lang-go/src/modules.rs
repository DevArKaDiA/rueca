//! Go module model (RFC 0003 §10.2): `go.mod` / `go.work` → import paths,
//! module names and import resolution.
//!
//! - Every `go.mod` in the snapshot maps its `module` path to its directory
//!   (multi-module repositories). Local `replace` directives of `go.mod`
//!   and `go.work` (`replace example.com/x => ./x`) map the replaced module
//!   path to the target directory. `go.work` `use` directives need nothing
//!   extra: the used modules' own `go.mod` files are in the snapshot.
//! - A file's module name is its package's **import path**: the module path
//!   of the nearest ancestor `go.mod` plus the relative directory
//!   (`github.com/gin-gonic/gin/binding`). Without a `go.mod` it is the
//!   directory path. External test packages (`package x_test`) get `_test`
//!   appended (decided from the facts, see `GoPack::module_name_for`).
//! - An import resolves to the package directory when its path is under a
//!   project module (longest module-path prefix wins) and that directory
//!   holds `.go` files; standard library and third-party imports resolve to
//!   nothing. `vendor/` is never walked by the indexer, so vendored copies
//!   are third-party.
//! - `internal/` packages resolve like any other; Go's visibility rule is not
//!   enforced (code that violates it does not compile).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rueca_extract::ProjectFiles;

/// One module root: import-path prefix → repository directory (`""` = root).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Root {
    module: String,
    dir: String,
    /// From a `module` directive (true) or a local `replace` (false).
    declared: bool,
}

/// Module roots of a snapshot.
#[derive(Debug, Default)]
pub(crate) struct Modules {
    roots: Vec<Root>,
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Joins `base` and a relative path, normalizing `.` and `..`; `None` when
/// the path escapes the repository or is absolute.
fn join(base: &str, rel: &str) -> Option<String> {
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

/// Strips a `//` comment and surrounding whitespace / quotes from a token.
fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '`')
}

/// Directive lines of a `go.mod` / `go.work` file: `(verb, argument text)`,
/// with blocks (`replace ( … )`) expanded and comments removed.
fn directives(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut block: Option<String> = None;
    for raw in text.lines() {
        let line = match raw.find("//") {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if let Some(verb) = &block {
            if line == ")" {
                block = None;
            } else {
                out.push((verb.clone(), line.to_string()));
            }
            continue;
        }
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        if rest == "(" {
            block = Some(verb.to_string());
        } else {
            out.push((verb.to_string(), rest.to_string()));
        }
    }
    out
}

impl Modules {
    pub(crate) fn build(project: &ProjectFiles) -> Modules {
        let mut roots = Vec::new();
        for (path, text) in &project.configs {
            let name = path.rsplit('/').next().unwrap_or(path);
            if name != "go.mod" && name != "go.work" {
                continue;
            }
            let dir = parent_dir(path);
            for (verb, arg) in directives(text) {
                match verb.as_str() {
                    "module" if name == "go.mod" => {
                        let module = unquote(&arg).to_string();
                        if !module.is_empty() {
                            roots.push(Root { module, dir: dir.to_string(), declared: true });
                        }
                    }
                    "replace" => {
                        // `old [v] => new [v]`; only local targets matter.
                        let Some((old, new)) = arg.split_once("=>") else { continue };
                        let old = unquote(old.split_whitespace().next().unwrap_or(""));
                        let new = unquote(new.split_whitespace().next().unwrap_or(""));
                        if old.is_empty() || !(new.starts_with("./") || new.starts_with("../")) {
                            continue;
                        }
                        if let Some(target) = join(dir, new) {
                            roots.push(Root { module: old.to_string(), dir: target, declared: false });
                        }
                    }
                    _ => {}
                }
            }
        }
        // Deterministic preference: declared before replaced, shallower
        // directories first (a module path declared twice, e.g. by an
        // example copy, maps to the outermost one).
        roots.sort_by(|a, b| {
            (!a.declared, a.dir.matches('/').count(), &a.dir, &a.module).cmp(&(
                !b.declared,
                b.dir.matches('/').count(),
                &b.dir,
                &b.module,
            ))
        });
        roots.dedup_by(|b, a| a.module == b.module && a.dir == b.dir);
        Modules { roots }
    }

    /// Import path of the package in directory `dir`: the declared module
    /// of the nearest ancestor `go.mod` plus the relative path; the
    /// directory itself when no `go.mod` covers it.
    pub(crate) fn import_path(&self, dir: &str) -> String {
        let mut best: Option<&Root> = None;
        for r in self.roots.iter().filter(|r| r.declared) {
            let covers = r.dir.is_empty() || dir == r.dir || dir.starts_with(&format!("{}/", r.dir));
            if covers && best.is_none_or(|b| r.dir.len() > b.dir.len()) {
                best = Some(r);
            }
        }
        match best {
            Some(r) => {
                let rest = if r.dir.is_empty() { dir } else { dir[r.dir.len()..].trim_start_matches('/') };
                if rest.is_empty() { r.module.clone() } else { format!("{}/{rest}", r.module) }
            }
            None => dir.to_string(),
        }
    }

    /// Repository directory of an import path under a project module
    /// (longest module path wins), if any.
    pub(crate) fn package_dir(&self, import: &str) -> Option<String> {
        let mut best: Option<(&Root, &str)> = None;
        for r in &self.roots {
            let rest = if import == r.module {
                ""
            } else if let Some(rest) = import.strip_prefix(&r.module).and_then(|s| s.strip_prefix('/')) {
                rest
            } else {
                continue;
            };
            if best.is_none_or(|(b, _)| r.module.len() > b.module.len()) {
                best = Some((r, rest));
            }
        }
        let (r, rest) = best?;
        Some(match (r.dir.is_empty(), rest.is_empty()) {
            (true, _) => rest.to_string(),
            (false, true) => r.dir.clone(),
            (false, false) => format!("{}/{rest}", r.dir),
        })
    }
}

/// Whether directory `dir` directly contains a `.go` file.
pub(crate) fn has_go_files(project: &ProjectFiles, dir: &str) -> bool {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    project
        .paths
        .range(prefix.clone()..)
        .take_while(|p| p.starts_with(&prefix))
        .any(|p| !p[prefix.len()..].contains('/') && p.ends_with(".go"))
}

type CacheKey = (usize, usize, Vec<(String, usize, usize)>);

fn cache_key(project: &ProjectFiles) -> CacheKey {
    let mods = project
        .configs
        .iter()
        .filter(|(p, _)| matches!(p.rsplit('/').next(), Some("go.mod" | "go.work")))
        .map(|(p, t)| (p.clone(), t.as_ptr() as usize, t.len()))
        .collect();
    (project as *const ProjectFiles as usize, project.paths.len(), mods)
}

/// Snapshots kept in the cache (an MCP server may index several roots).
const CACHE_ENTRIES: usize = 4;

/// Process-wide cache of the most recent snapshots' module tables.
#[derive(Default)]
pub(crate) struct ModulesCache {
    entries: Mutex<BTreeMap<u64, (CacheKey, Arc<Modules>)>>,
    counter: Mutex<u64>,
}

impl ModulesCache {
    pub(crate) fn get(&self, project: &ProjectFiles) -> Arc<Modules> {
        let key = cache_key(project);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (_, m))) = entries.iter().find(|(_, (k, _))| *k == key) {
            return m.clone();
        }
        let built = Arc::new(Modules::build(project));
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

/// Binding name of an unaliased import by Go convention (RFC 0003 §10.2):
/// the last path element without a `/vN` major-version element, a
/// `gopkg.in`-style `.vN` suffix, a `go-` prefix or a `-go` / `.go`
/// suffix (`github.com/mattn/go-isatty` → `isatty`,
/// `gopkg.in/yaml.v3` → `yaml`, `github.com/urfave/cli/v3` → `cli`).
/// `None` when no valid identifier remains (`github.com/json-iterator/go`).
pub(crate) fn conventional_name(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let is_major = |s: &str| s.len() > 1 && s.starts_with('v') && s[1..].bytes().all(|b| b.is_ascii_digit());
    if parts.len() > 1 && parts.last().is_some_and(|l| is_major(l)) {
        parts.pop();
    }
    let mut name = *parts.last()?;
    if let Some((stem, ver)) = name.rsplit_once('.')
        && is_major(ver)
    {
        name = stem;
    }
    for prefix in ["go-", "go_"] {
        if let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
        {
            name = rest;
        }
    }
    for suffix in ["-go", ".go", "_go"] {
        if let Some(rest) = name.strip_suffix(suffix)
            && !rest.is_empty()
        {
            name = rest;
        }
    }
    let name: String = name.chars().map(|c| if c == '-' || c == '.' { '_' } else { c }).collect();
    let valid = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !crate::builtins::is_keyword(&name);
    valid.then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: configs.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect(),
        }
    }

    #[test]
    fn go_mod_directives() {
        let text = "// comment\nmodule \"example.com/app\" // trailing\n\ngo 1.22\n\nrequire (\n\tgithub.com/x/y v1.0.0\n)\nreplace example.com/lib v1.2.0 => ../lib\nreplace (\n\texample.com/tool => ./tools/tool\n\texample.com/remote => example.com/fork v1.0.0\n)\n";
        let d = directives(text);
        assert!(d.contains(&("module".into(), "\"example.com/app\"".into())));
        assert!(d.contains(&("require".into(), "github.com/x/y v1.0.0".into())));
        assert!(d.contains(&("replace".into(), "example.com/tool => ./tools/tool".into())));
        let p = project(
            &["svc/go.mod", "svc/main.go", "lib/go.mod", "lib/x.go", "svc/tools/tool/t.go"],
            &[("svc/go.mod", text), ("lib/go.mod", "module example.com/lib\n")],
        );
        let m = Modules::build(&p);
        assert_eq!(m.import_path("svc"), "example.com/app");
        assert_eq!(m.import_path("svc/internal/db"), "example.com/app/internal/db");
        assert_eq!(m.import_path("lib"), "example.com/lib");
        assert_eq!(m.import_path("other/pkg"), "other/pkg");
        assert_eq!(m.package_dir("example.com/app/internal/db").as_deref(), Some("svc/internal/db"));
        assert_eq!(m.package_dir("example.com/lib").as_deref(), Some("lib"));
        assert_eq!(m.package_dir("example.com/tool/sub").as_deref(), Some("svc/tools/tool/sub"));
        assert_eq!(m.package_dir("example.com/remote"), None);
        assert_eq!(m.package_dir("example.com/application"), None);
        assert_eq!(m.package_dir("fmt"), None);
    }

    #[test]
    fn root_module_and_nesting() {
        let p = project(
            &["go.mod", "a.go", "sub/go.mod", "sub/b.go"],
            &[("go.mod", "module github.com/o/r\n"), ("sub/go.mod", "module github.com/o/r/sub/v2\n")],
        );
        let m = Modules::build(&p);
        assert_eq!(m.import_path(""), "github.com/o/r");
        assert_eq!(m.import_path("sub"), "github.com/o/r/sub/v2");
        assert_eq!(m.import_path("sub/x"), "github.com/o/r/sub/v2/x");
        assert_eq!(m.package_dir("github.com/o/r").as_deref(), Some(""));
        assert_eq!(m.package_dir("github.com/o/r/sub/v2/x").as_deref(), Some("sub/x"));
        // Without the major suffix the path belongs to the root module.
        assert_eq!(m.package_dir("github.com/o/r/sub").as_deref(), Some("sub"));
        assert!(has_go_files(&p, "") && has_go_files(&p, "sub") && !has_go_files(&p, "sub/x"));
    }

    #[test]
    fn go_work_replace() {
        let p = project(
            &["go.work", "app/go.mod", "app/m.go", "shared/s.go"],
            &[
                ("go.work", "go 1.22\nuse (\n\t./app\n)\nreplace example.com/shared => ./shared\n"),
                ("app/go.mod", "module example.com/app\n"),
            ],
        );
        let m = Modules::build(&p);
        assert_eq!(m.package_dir("example.com/shared").as_deref(), Some("shared"));
        assert_eq!(m.package_dir("example.com/app").as_deref(), Some("app"));
        // A replaced directory without its own go.mod keeps its directory path.
        assert_eq!(m.import_path("shared"), "shared");
    }

    #[test]
    fn conventional_binding_names() {
        let cases = [
            ("fmt", Some("fmt")),
            ("net/http", Some("http")),
            ("github.com/urfave/cli/v3", Some("cli")),
            ("gopkg.in/yaml.v3", Some("yaml")),
            ("github.com/mattn/go-isatty", Some("isatty")),
            ("github.com/goccy/go-json", Some("json")),
            ("github.com/pelletier/go-toml/v2", Some("toml")),
            ("github.com/json-iterator/go", None),
            ("github.com/foo/bar-baz", Some("bar_baz")),
            ("github.com/foo/client-go", Some("client")),
        ];
        for (path, want) in cases {
            assert_eq!(conventional_name(path).as_deref(), want, "{path}");
        }
    }
}
