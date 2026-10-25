//! `#include` resolution.
//!
//! An include is recorded as an import whose module is the path as written:
//! `"x.h"` → `x.h`, `<x.h>` → `<x.h>` (the angle brackets mark a system
//! include). [`IncludeIndex::resolve`] maps it to a project file:
//!
//! | Step | `"x.h"` | `<x.h>` |
//! |---|---|---|
//! | 1 | the including file's directory | — |
//! | 2 | `-iquote` directories | — |
//! | 3 | `-I` directories | `-I` directories |
//! | 4 | conventional roots `include/`, `src/`, the repository root | `include/` |
//! | 5 | the only project file whose path ends with `/x.h` | the same, only when the path has a `/` (`<fmt/core.h>`) |
//!
//! Include directories come from every `compile_commands.json` (`arguments`
//! or `command`, relative to each entry's `directory`) and
//! `compile_flags.txt` (relative to its own directory) in the snapshot.
//! Absolute paths are mapped into the repository by finding the repository
//! root from the entries' `file` paths (an entry for `/home/ci/fmt/src/os.cc`
//! and the project path `src/os.cc` give the root `/home/ci/fmt`), else by
//! the longest suffix that is a project directory. `-isystem` and
//! directories outside the repository are ignored: system headers resolve to
//! nothing unless a project include directory has them.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use rueca_extract::ProjectFiles;

/// Most include directories kept per snapshot.
const MAX_DIRS: usize = 256;

/// Include directories of a snapshot plus a basename index for step 5.
#[derive(Debug, Default)]
pub(crate) struct IncludeIndex {
    /// `-iquote` directories (repository-relative, `""` = root).
    pub(crate) quote_dirs: Vec<String>,
    /// `-I` directories.
    pub(crate) dirs: Vec<String>,
    /// Basename → project paths with that basename.
    by_name: HashMap<String, Vec<String>>,
}

/// Normalizes `.` and `..` segments; keeps a leading `/`. `None` when `..`
/// climbs above a relative path's start.
pub(crate) fn normalize(path: &str) -> Option<String> {
    let absolute = path.starts_with('/');
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
    let joined = parts.join("/");
    Some(if absolute { format!("/{joined}") } else { joined })
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else if rel.is_empty() {
        dir.to_string()
    } else {
        format!("{}/{rel}", dir.trim_end_matches('/'))
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Splits a shell command line (simple quoting, as compilation databases
/// use it).
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    let mut has = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (_, '\\') if quote != Some('\'') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (_, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// `(-iquote dirs, -I dirs)` written in a list of compiler arguments.
fn include_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut quote, mut dirs) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take = |prefix: &str, out: &mut Vec<String>| -> bool {
            if a == prefix {
                if let Some(v) = args.get(i + 1) {
                    out.push(v.clone());
                }
                i += 1;
                true
            } else if let Some(v) = a.strip_prefix(prefix) {
                out.push(v.trim_start_matches('=').to_string());
                true
            } else {
                false
            }
        };
        // `-isystem`, `-idirafter` and everything else are ignored.
        let _ = take("-iquote", &mut quote) || take("-I", &mut dirs);
        i += 1;
    }
    (quote, dirs)
}

/// Maps an absolute directory into the repository.
fn to_repo(abs: &str, root: Option<&str>, project: &ProjectFiles) -> Option<String> {
    let abs = normalize(abs)?;
    if let Some(root) = root {
        if abs == root {
            return Some(String::new());
        }
        if let Some(rest) = abs.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
            return Some(rest.to_string());
        }
        return None;
    }
    // No known root: the longest suffix that is a project directory.
    let trimmed = abs.trim_start_matches('/');
    let mut idx = 0;
    loop {
        let suffix = &trimmed[idx..];
        if !suffix.is_empty() && idx > 0 && project.is_dir(suffix) {
            return Some(suffix.to_string());
        }
        idx += suffix.find('/')? + 1;
    }
}

/// The repository root implied by an absolute source path that is a
/// project file (`/home/ci/fmt/src/os.cc` + `src/os.cc` → `/home/ci/fmt`).
fn root_from(abs_file: &str, project: &ProjectFiles) -> Option<String> {
    let abs = normalize(abs_file)?;
    let mut idx = 0;
    while let Some(p) = abs[idx..].find('/') {
        let cut = idx + p;
        let suffix = &abs[cut + 1..];
        if project.contains(suffix) {
            return Some(abs[..cut].to_string());
        }
        idx = cut + 1;
    }
    None
}

impl IncludeIndex {
    pub(crate) fn build(project: &ProjectFiles) -> IncludeIndex {
        let mut idx = IncludeIndex::default();
        for (path, text) in &project.configs {
            let name = path.rsplit('/').next().unwrap_or(path);
            match name {
                "compile_commands.json" => idx.add_compile_commands(text, project),
                "compile_flags.txt" => idx.add_compile_flags(parent(path), text, project),
                _ => {}
            }
        }
        for p in &project.paths {
            let base = p.rsplit('/').next().unwrap_or(p);
            idx.by_name.entry(base.to_string()).or_default().push(p.clone());
        }
        idx
    }

    fn push(&mut self, quote: bool, dir: String) {
        let v = if quote { &mut self.quote_dirs } else { &mut self.dirs };
        if v.len() < MAX_DIRS && !v.contains(&dir) {
            v.push(dir);
        }
    }

    fn add_compile_commands(&mut self, text: &str, project: &ProjectFiles) {
        let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(text) else { return };
        let str_of = |e: &serde_json::Value, k: &str| e.get(k).and_then(|v| v.as_str()).map(str::to_string);
        // Repository root from the first entry whose file is a project file.
        let root = entries.iter().find_map(|e| {
            let file = str_of(e, "file")?;
            let abs = if file.starts_with('/') { file } else { join(&str_of(e, "directory")?, &file) };
            root_from(&abs, project)
        });
        let mut seen_args = std::collections::HashSet::new();
        for e in &entries {
            let directory = str_of(e, "directory").unwrap_or_default();
            let args: Vec<String> = match (e.get("arguments").and_then(|a| a.as_array()), str_of(e, "command")) {
                (Some(a), _) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                (None, Some(c)) => split_command(&c),
                (None, None) => continue,
            };
            let (quote, dirs) = include_flags(&args);
            for (is_quote, list) in [(true, quote), (false, dirs)] {
                for d in list {
                    if !seen_args.insert((is_quote, directory.clone(), d.clone())) {
                        continue;
                    }
                    let abs = if d.starts_with('/') { d } else { join(&directory, &d) };
                    let rel = if abs.starts_with('/') {
                        to_repo(&abs, root.as_deref(), project)
                    } else {
                        // A relative `directory`: relative to the repository.
                        normalize(&abs)
                    };
                    if let Some(rel) = rel {
                        self.push(is_quote, rel);
                    }
                }
            }
        }
    }

    fn add_compile_flags(&mut self, dir: &str, text: &str, project: &ProjectFiles) {
        let args: Vec<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        let (quote, dirs) = include_flags(&args);
        for (is_quote, list) in [(true, quote), (false, dirs)] {
            for d in list {
                let rel = if d.starts_with('/') { to_repo(&d, None, project) } else { normalize(&join(dir, &d)) };
                if let Some(rel) = rel {
                    self.push(is_quote, rel);
                }
            }
        }
    }

    /// Resolves an include written in `from` (see the module docs).
    /// `module` is `x.h` for `"x.h"` and `<x.h>` for a system include.
    pub(crate) fn resolve(&self, from: &str, module: &str, project: &ProjectFiles) -> Option<String> {
        let (spec, angle) = match module.strip_prefix('<').and_then(|m| m.strip_suffix('>')) {
            Some(s) => (s.trim(), true),
            None => (module.trim(), false),
        };
        if spec.is_empty() || spec.starts_with('/') {
            return None;
        }
        let try_in = |dir: &str| normalize(&join(dir, spec)).filter(|p| project.contains(p));
        if !angle {
            if let Some(p) = try_in(parent(from)) {
                return Some(p);
            }
            for d in &self.quote_dirs {
                if let Some(p) = try_in(d) {
                    return Some(p);
                }
            }
        }
        for d in &self.dirs {
            if let Some(p) = try_in(d) {
                return Some(p);
            }
        }
        let roots: &[&str] = if angle { &["include"] } else { &["include", "src", ""] };
        for r in roots {
            if let Some(p) = try_in(r) {
                return Some(p);
            }
        }
        if angle && !spec.contains('/') {
            return None;
        }
        let norm = normalize(spec)?;
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        let suffix = format!("/{norm}");
        let mut found = self.by_name.get(base)?.iter().filter(|p| **p == norm || p.ends_with(&suffix));
        let first = found.next()?;
        found.next().is_none().then(|| first.clone())
    }
}

type CacheKey = (usize, usize, Vec<(String, usize, usize)>);

fn cache_key(project: &ProjectFiles) -> CacheKey {
    let configs = project
        .configs
        .iter()
        .filter(|(p, _)| matches!(p.rsplit('/').next(), Some("compile_commands.json" | "compile_flags.txt")))
        .map(|(p, t)| (p.clone(), t.as_ptr() as usize, t.len()))
        .collect();
    (project as *const ProjectFiles as usize, project.paths.len(), configs)
}

/// Snapshots kept in the cache (an MCP server may index several roots).
const CACHE_ENTRIES: usize = 4;

/// Process-wide cache of the most recent snapshots' include indexes.
#[derive(Default)]
pub(crate) struct IncludeCache {
    entries: Mutex<BTreeMap<u64, (CacheKey, Arc<IncludeIndex>)>>,
    counter: Mutex<u64>,
}

impl IncludeCache {
    pub(crate) fn get(&self, project: &ProjectFiles) -> Arc<IncludeIndex> {
        let key = cache_key(project);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (_, idx))) = entries.iter().find(|(_, (k, _))| *k == key) {
            return idx.clone();
        }
        let built = Arc::new(IncludeIndex::build(project));
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

    fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: configs.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect(),
        }
    }

    #[test]
    fn relative_roots_and_suffix() {
        let p = project(
            &["src/a.c", "src/a.h", "include/lib/api.h", "src/util/u.h", "third/x/deep.h", "other/deep.h"],
            &[],
        );
        let idx = IncludeIndex::build(&p);
        let r = |from: &str, m: &str| idx.resolve(from, m, &p);
        assert_eq!(r("src/a.c", "a.h").as_deref(), Some("src/a.h"));
        assert_eq!(r("src/a.c", "util/u.h").as_deref(), Some("src/util/u.h"));
        assert_eq!(r("src/util/u.h", "../a.h").as_deref(), Some("src/a.h"));
        assert_eq!(r("src/a.c", "lib/api.h").as_deref(), Some("include/lib/api.h"));
        assert_eq!(r("src/a.c", "<lib/api.h>").as_deref(), Some("include/lib/api.h"));
        assert_eq!(r("src/a.c", "<stdio.h>"), None);
        assert_eq!(r("src/a.c", "x/deep.h").as_deref(), Some("third/x/deep.h"), "unique suffix");
        assert_eq!(r("src/a.c", "deep.h"), None, "ambiguous suffix");
        assert_eq!(r("src/a.c", "/abs.h"), None);
        assert_eq!(r("src/a.c", "../../../x.h"), None);
    }

    #[test]
    fn compile_commands_include_dirs() {
        let db = r#"[
          {"directory": "/home/ci/proj/build", "file": "/home/ci/proj/src/main.cpp",
           "arguments": ["c++", "-I/home/ci/proj/inc", "-I", "../vendor/inc", "-iquote", "/home/ci/proj/q",
                         "-isystem", "/home/ci/proj/sys", "-I/usr/include", "-c", "../src/main.cpp"]},
          {"directory": "/home/ci/proj/build", "file": "../src/other.cpp",
           "command": "c++ -I\"/home/ci/proj/inc2\" -DX=1 -c ../src/other.cpp"}
        ]"#;
        let p = project(
            &[
                "src/main.cpp",
                "src/other.cpp",
                "inc/a.h",
                "vendor/inc/v.h",
                "q/qh.h",
                "sys/s.h",
                "inc2/b.h",
                "build/compile_commands.json",
            ],
            &[("build/compile_commands.json", db)],
        );
        let idx = IncludeIndex::build(&p);
        assert_eq!(idx.dirs, ["inc", "vendor/inc", "inc2"]);
        assert_eq!(idx.quote_dirs, ["q"]);
        let r = |m: &str| idx.resolve("src/main.cpp", m, &p);
        assert_eq!(r("a.h").as_deref(), Some("inc/a.h"));
        assert_eq!(r("<v.h>").as_deref(), Some("vendor/inc/v.h"));
        assert_eq!(r("qh.h").as_deref(), Some("q/qh.h"));
        assert_eq!(r("<qh.h>"), None, "-iquote is for quoted includes only");
        assert_eq!(r("<s.h>"), None, "-isystem is ignored");
        assert_eq!(r("b.h").as_deref(), Some("inc2/b.h"));
    }

    #[test]
    fn compile_flags_and_unknown_roots() {
        let p = project(
            &["lib/compile_flags.txt", "lib/src/a.c", "lib/include/x.h", "gen/y.h"],
            &[("lib/compile_flags.txt", "-xc\n-Iinclude\n-I\n/somewhere/else/gen\n-isystem\n/usr/include\n")],
        );
        let idx = IncludeIndex::build(&p);
        assert_eq!(idx.dirs, ["lib/include", "gen"]);
        assert_eq!(idx.resolve("lib/src/a.c", "<x.h>", &p).as_deref(), Some("lib/include/x.h"));
        assert_eq!(idx.resolve("lib/src/a.c", "y.h", &p).as_deref(), Some("gen/y.h"));
    }

    #[test]
    fn command_splitting_and_normalizing() {
        assert_eq!(split_command(r#"cc -I"a b" -D'X=1' x\ y.c"#), ["cc", "-Ia b", "-DX=1", "x y.c"]);
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("/a/../b").as_deref(), Some("/b"));
        assert_eq!(normalize("../x"), None);
    }
}
