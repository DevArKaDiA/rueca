//! Composer autoload maps and import resolution.
//!
//! Every `composer.json` in [`ProjectFiles::configs`] (the root one and any
//! nested package in a monorepo) contributes its `autoload` and
//! `autoload-dev` sections, with paths taken relative to that file's
//! directory:
//!
//! - `psr-4`: `"App\\": "app/"` (or a list of directories; `""` is a
//!   fallback for every namespace) maps `App\Models\User` to
//!   `app/Models/User.php`.
//! - `psr-0`: `"Acme_": "lib/"` maps `Acme\Util\Str` / `Acme_Util_Str` to
//!   `lib/Acme/Util/Str.php` (the prefix stays in the path and `_` in the
//!   class name becomes `/`).
//! - `classmap`: directories (or files) scanned by Composer; a class maps to
//!   `<dir>/…/<Class>.php` when exactly one such file exists below the
//!   directory (best effort: the file name usually matches the class).
//!
//! Prefixes are tried longest first (ties: shallower `composer.json`, then
//! path order), and a candidate must exist in the snapshot. When no
//! autoload rule matches, a namespaced class falls back to the unique
//! project file named `<Class>.php` whose directories end with the most
//! namespace segments (at least one), which covers projects indexed without
//! their `composer.json` and test directories without `autoload-dev`.
//! Global-namespace classes are never guessed from file names.
//!
//! `require` / `include` imports carry a path: `__DIR__/…` (from
//! `__DIR__ . '/x.php'`, `dirname(__DIR__) . '/x.php'`, …) is relative to the
//! importing file's directory; other relative paths are tried against the
//! importing file's directory, then the repository root. Absolute paths and
//! anything outside the snapshot resolve to nothing.
//!
//! The parsed maps and a file-name index are built once per [`ProjectFiles`]
//! value and cached (keyed by its address and the identity of its
//! `composer.json` buffers), so resolving thousands of imports parses each
//! `composer.json` once.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use rueca_extract::ProjectFiles;
use rueca_model::Import;
use serde_json::Value;

/// One autoload rule from one `composer.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    kind: RuleKind,
    /// Namespace prefix without leading `\`, ending with `\` (or `_` for
    /// PSR-0 underscore prefixes); empty = fallback.
    prefix: String,
    /// Repository-relative directory (no trailing `/`; empty = root).
    dir: String,
    /// Depth of the declaring `composer.json` (0 = repository root).
    depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RuleKind {
    Psr4,
    Psr0,
    Classmap,
}

/// Autoload rules of a snapshot plus a file-name index.
#[derive(Debug, Default)]
pub(crate) struct Autoload {
    rules: Vec<Rule>,
    /// `File.php` → repository-relative paths with that file name, sorted.
    by_file_name: HashMap<String, Vec<String>>,
}

fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
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

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() { rest.to_string() } else { format!("{dir}/{rest}") }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

impl Autoload {
    pub(crate) fn build(project: &ProjectFiles) -> Autoload {
        let mut rules = Vec::new();
        for (path, text) in &project.configs {
            if path.rsplit('/').next() != Some("composer.json") {
                continue;
            }
            let Ok(json) = serde_json::from_str::<Value>(text) else { continue };
            let base = dir_of(path);
            let depth = if base.is_empty() { 0 } else { base.split('/').count() };
            for section in ["autoload", "autoload-dev"] {
                let Some(sec) = json.get(section) else { continue };
                for (key, kind) in [("psr-4", RuleKind::Psr4), ("psr-0", RuleKind::Psr0)] {
                    let Some(Value::Object(map)) = sec.get(key) else { continue };
                    for (prefix, dirs) in map {
                        for d in strings(dirs) {
                            let Some(dir) = normalize(&join(base, d)) else { continue };
                            let prefix = prefix.trim_start_matches('\\').to_string();
                            rules.push(Rule { kind, prefix, dir, depth });
                        }
                    }
                }
                if let Some(cm) = sec.get("classmap") {
                    for d in strings(cm) {
                        let Some(dir) = normalize(&join(base, d)) else { continue };
                        rules.push(Rule { kind: RuleKind::Classmap, prefix: String::new(), dir, depth });
                    }
                }
            }
        }
        // Longest prefix first, then shallower composer.json, then kind and path.
        rules.sort_by(|a, b| {
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then(a.depth.cmp(&b.depth))
                .then(a.kind.cmp(&b.kind))
                .then(a.prefix.cmp(&b.prefix))
                .then(a.dir.cmp(&b.dir))
        });
        rules.dedup();
        let mut by_file_name: HashMap<String, Vec<String>> = HashMap::new();
        for p in &project.paths {
            if p.ends_with(".php") {
                by_file_name.entry(p.rsplit('/').next().unwrap_or(p).to_string()).or_default().push(p.clone());
            }
        }
        Autoload { rules, by_file_name }
    }

    /// Repository-relative file declaring class `fqn` (no leading `\`).
    pub(crate) fn class_file(&self, fqn: &str, project: &ProjectFiles) -> Option<String> {
        let fqn = fqn.trim_start_matches('\\');
        if fqn.is_empty() {
            return None;
        }
        for rule in &self.rules {
            let found = match rule.kind {
                RuleKind::Psr4 => {
                    let Some(rest) = strip_prefix_ci(fqn, &rule.prefix) else { continue };
                    let candidate = join(&rule.dir, &format!("{}.php", rest.replace('\\', "/")));
                    project.contains(&candidate).then_some(candidate)
                }
                RuleKind::Psr0 => {
                    if strip_prefix_ci(fqn, &rule.prefix).is_none() {
                        continue;
                    }
                    let (ns, class) = fqn.rsplit_once('\\').unwrap_or(("", fqn));
                    let mut rel = ns.replace('\\', "/");
                    if !rel.is_empty() {
                        rel.push('/');
                    }
                    rel.push_str(&class.replace('_', "/"));
                    let candidate = join(&rule.dir, &format!("{rel}.php"));
                    project.contains(&candidate).then_some(candidate)
                }
                RuleKind::Classmap => {
                    let short = fqn.rsplit('\\').next().unwrap_or(fqn);
                    let below: Vec<&String> = self
                        .by_file_name
                        .get(&format!("{short}.php"))
                        .into_iter()
                        .flatten()
                        .filter(|p| rule.dir.is_empty() || p.starts_with(&format!("{}/", rule.dir)) || **p == rule.dir)
                        .collect();
                    if below.len() == 1 { Some(below[0].clone()) } else { None }
                }
            };
            if found.is_some() {
                return found;
            }
        }
        self.by_suffix(fqn)
    }

    /// Fallback: the unique `<Class>.php` whose directories end with the
    /// most namespace segments (at least one). Global-namespace names have
    /// no segment to match (`Redis`, `Exception` would hit any file of that
    /// name), so they resolve to nothing.
    fn by_suffix(&self, fqn: &str) -> Option<String> {
        let (ns, short) = fqn.rsplit_once('\\').unwrap_or(("", fqn));
        let ns_segs: Vec<String> = ns.split('\\').filter(|s| !s.is_empty()).map(str::to_ascii_lowercase).collect();
        if ns_segs.is_empty() {
            return None;
        }
        let candidates = self.by_file_name.get(&format!("{short}.php"))?;
        let mut best: Option<(usize, &String)> = None;
        let mut tie = false;
        for c in candidates {
            let dirs: Vec<String> =
                dir_of(c).split('/').filter(|s| !s.is_empty()).map(str::to_ascii_lowercase).collect();
            let score = ns_segs.iter().rev().zip(dirs.iter().rev()).take_while(|(a, b)| a == b).count();
            match best {
                Some((s, _)) if score < s => {}
                Some((s, _)) if score == s => tie = true,
                _ => {
                    best = Some((score, c));
                    tie = false;
                }
            }
        }
        match best {
            Some((score, path)) if score >= 1 && !tie => Some(path.clone()),
            _ => None,
        }
    }

    /// The namespace PSR-4 / PSR-0 expects for the file at `path`, if an
    /// autoload rule covers it (diagnostics and tests; the declared
    /// `namespace` is what qualifies names).
    pub(crate) fn expected_namespace(&self, path: &str) -> Option<String> {
        let dir = dir_of(path);
        for rule in &self.rules {
            let rel = if rule.dir.is_empty() {
                dir
            } else if dir == rule.dir {
                ""
            } else {
                match dir.strip_prefix(&format!("{}/", rule.dir)) {
                    Some(r) => r,
                    None => continue,
                }
            };
            match rule.kind {
                RuleKind::Psr4 => {
                    let prefix = rule.prefix.trim_end_matches('\\');
                    let tail = rel.replace('/', "\\");
                    return Some(match (prefix.is_empty(), tail.is_empty()) {
                        (true, _) => tail,
                        (false, true) => prefix.to_string(),
                        (false, false) => format!("{prefix}\\{tail}"),
                    });
                }
                RuleKind::Psr0 => return Some(rel.replace('/', "\\")),
                RuleKind::Classmap => {}
            }
        }
        None
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// True when an import's `module` is a file path (`require` / `include`)
/// rather than a namespaced name.
pub(crate) fn is_path_import(module: &str) -> bool {
    module.contains('/') || module.contains('.')
}

/// Resolves a `require` / `include` path written in `from_path`.
pub(crate) fn resolve_path(from_path: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    let from_dir = dir_of(from_path);
    if let Some(rest) = module.strip_prefix("__DIR__") {
        let p = normalize(&join(from_dir, rest.trim_start_matches('/')))?;
        return project.contains(&p).then_some(p);
    }
    if module.starts_with('/') || module.contains("://") {
        return None;
    }
    for base in [from_dir, ""] {
        if let Some(p) = normalize(&join(base, module))
            && project.contains(&p)
        {
            return Some(p);
        }
    }
    None
}

/// Identity of the configs an [`Autoload`] was built from.
type CacheKey = (usize, usize, Vec<(String, usize, usize)>);

fn cache_key(project: &ProjectFiles) -> CacheKey {
    let composer = project
        .configs
        .iter()
        .filter(|(p, _)| p.rsplit('/').next() == Some("composer.json"))
        .map(|(p, t)| (p.clone(), t.as_ptr() as usize, t.len()))
        .collect();
    (project as *const ProjectFiles as usize, project.paths.len(), composer)
}

/// Process-wide cache of the most recent snapshots' autoload maps.
#[derive(Default)]
pub(crate) struct AutoloadCache {
    entries: Mutex<BTreeMap<u64, (CacheKey, Arc<Autoload>)>>,
    counter: Mutex<u64>,
}

/// Snapshots kept in the cache (an MCP server may index several roots).
const CACHE_ENTRIES: usize = 4;

impl AutoloadCache {
    pub(crate) fn get(&self, project: &ProjectFiles) -> Arc<Autoload> {
        let key = cache_key(project);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (_, a))) = entries.iter().find(|(_, (k, _))| *k == key) {
            return a.clone();
        }
        let built = Arc::new(Autoload::build(project));
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

/// [`rueca_extract::LanguagePack::resolve_import`] for PHP.
pub(crate) fn resolve_import(
    from_path: &str,
    import: &Import,
    project: &ProjectFiles,
    cache: &AutoloadCache,
) -> Option<String> {
    if is_path_import(&import.module) {
        return resolve_path(from_path, &import.module, project);
    }
    // `use function` / `use const` (recorded with `names`) are not autoloaded.
    if !import.names.is_empty() {
        return None;
    }
    cache.get(project).class_file(&import.module, project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_model::{ImportedName, Span};

    fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: configs.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect(),
        }
    }

    fn class_import(module: &str) -> Import {
        Import {
            module: module.into(),
            relative_level: 0,
            names: vec![],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Span::default(),
        }
    }

    const LARAVEL: &str = r#"{
        "autoload": {
            "psr-4": {"App\\": "app/", "Database\\Factories\\": "database/factories/"},
            "classmap": ["database/seeds"],
            "files": ["app/helpers.php"]
        },
        "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}
    }"#;

    #[test]
    fn psr4_root_and_dev() {
        let p = project(
            &["app/Models/User.php", "tests/Feature/UserTest.php", "database/seeds/UserSeeder.php", "app/helpers.php"],
            &[("composer.json", LARAVEL)],
        );
        let cache = AutoloadCache::default();
        let r = |m: &str| resolve_import("app/Http/X.php", &class_import(m), &p, &cache);
        assert_eq!(r("App\\Models\\User").as_deref(), Some("app/Models/User.php"));
        assert_eq!(r("\\App\\Models\\User").as_deref(), Some("app/Models/User.php"));
        assert_eq!(r("Tests\\Feature\\UserTest").as_deref(), Some("tests/Feature/UserTest.php"));
        assert_eq!(r("UserSeeder").as_deref(), Some("database/seeds/UserSeeder.php"));
        assert_eq!(r("App\\Models\\Missing"), None);
        assert_eq!(r("Illuminate\\Support\\Str"), None);
        let a = cache.get(&p);
        assert_eq!(a.expected_namespace("app/Models/User.php").as_deref(), Some("App\\Models"));
        assert_eq!(a.expected_namespace("app/Kernel.php").as_deref(), Some("App"));
        assert_eq!(a.expected_namespace("tests/Feature/UserTest.php").as_deref(), Some("Tests\\Feature"));
        assert_eq!(a.expected_namespace("routes/web.php"), None);
    }

    #[test]
    fn nested_composer_files_and_lists() {
        let p = project(
            &["src/Illuminate/Support/Str.php", "src/Illuminate/Support/Collection.php", "lib/a/X.php", "lib/b/Y.php"],
            &[
                (
                    "composer.json",
                    r#"{"autoload":{"psr-4":{"Illuminate\\":"src/Illuminate/","Multi\\":["lib/a","lib/b"]}}}"#,
                ),
                ("src/Illuminate/Support/composer.json", r#"{"autoload":{"psr-4":{"Illuminate\\Support\\":""}}}"#),
            ],
        );
        let cache = AutoloadCache::default();
        let r = |m: &str| resolve_import("x.php", &class_import(m), &p, &cache);
        assert_eq!(r("Illuminate\\Support\\Str").as_deref(), Some("src/Illuminate/Support/Str.php"));
        assert_eq!(r("Multi\\Y").as_deref(), Some("lib/b/Y.php"));
        let a = cache.get(&p);
        // The nested package's longer prefix wins.
        assert_eq!(a.rules[0].prefix, "Illuminate\\Support\\");
        assert_eq!(
            a.expected_namespace("src/Illuminate/Support/Collection.php").as_deref(),
            Some("Illuminate\\Support")
        );
    }

    #[test]
    fn psr0_and_fallbacks() {
        let p = project(
            &[
                "lib/Acme/Util/Str.php",
                "lib/Twig_Lexer.php",
                "lib/Twig/Lexer.php",
                "other/Models/Post.php",
                "a/Dup.php",
                "b/Dup.php",
            ],
            &[("composer.json", r#"{"autoload":{"psr-0":{"Acme\\":"lib/","Twig_":"lib/"}}}"#)],
        );
        let cache = AutoloadCache::default();
        let r = |m: &str| resolve_import("x.php", &class_import(m), &p, &cache);
        assert_eq!(r("Acme\\Util\\Str").as_deref(), Some("lib/Acme/Util/Str.php"));
        assert_eq!(r("Twig_Lexer").as_deref(), Some("lib/Twig/Lexer.php"));
        // No autoload rule: unique file whose directories match the namespace.
        assert_eq!(r("App\\Models\\Post").as_deref(), Some("other/Models/Post.php"));
        assert_eq!(r("Vendor\\Dup"), None);
        // Function / constant imports and invalid JSON resolve to nothing.
        let mut f = class_import("App\\Support");
        f.names.push(ImportedName { name: "helper".into(), alias: None });
        assert_eq!(resolve_import("x.php", &f, &p, &cache), None);
        let bad = project(&["lib/Foo.php"], &[("composer.json", "{not json")]);
        assert_eq!(
            resolve_import("x.php", &class_import("Lib\\Foo"), &bad, &AutoloadCache::default()).as_deref(),
            Some("lib/Foo.php")
        );
        // Global names are not guessed from file names.
        assert_eq!(resolve_import("x.php", &class_import("Foo"), &bad, &AutoloadCache::default()), None);
    }

    #[test]
    fn require_paths() {
        let p = project(&["bootstrap/app.php", "public/index.php", "lib/helpers.php", "public/inc/x.php"], &[]);
        let r = |from: &str, m: &str| resolve_path(from, m, &p);
        assert_eq!(r("public/index.php", "__DIR__/../bootstrap/app.php").as_deref(), Some("bootstrap/app.php"));
        assert_eq!(r("public/index.php", "inc/x.php").as_deref(), Some("public/inc/x.php"));
        assert_eq!(r("public/index.php", "lib/helpers.php").as_deref(), Some("lib/helpers.php"));
        assert_eq!(r("public/index.php", "./inc/x.php").as_deref(), Some("public/inc/x.php"));
        assert_eq!(r("public/index.php", "/etc/passwd"), None);
        assert_eq!(r("public/index.php", "__DIR__/../../../x.php"), None);
        assert!(
            is_path_import("__DIR__/x.php") && is_path_import("helpers.php") && !is_path_import("App\\Models\\User")
        );
    }

    #[test]
    fn cache_reuses_and_rebuilds() {
        let cache = AutoloadCache::default();
        let p = project(&["app/A.php"], &[("composer.json", r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#)]);
        let a = cache.get(&p);
        let b = cache.get(&p);
        assert!(Arc::ptr_eq(&a, &b));
        let q = project(&["src/A.php"], &[("composer.json", r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#)]);
        assert_eq!(cache.get(&q).class_file("App\\A", &q).as_deref(), Some("src/A.php"));
    }
}
