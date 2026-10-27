//! Python module semantics: source roots, module names, import resolution.

use rueca_extract::ProjectFiles;
use rueca_model::Import;

/// Splits a repository-relative path into `(dir, file_name)`; `dir` is `""`
/// for top-level files.
fn split_dir(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", path),
    }
}

fn parent_dir(dir: &str) -> Option<&str> {
    if dir.is_empty() { None } else { Some(dir.rsplit_once('/').map(|(p, _)| p).unwrap_or("")) }
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() { rest.to_string() } else { format!("{dir}/{rest}") }
}

/// True if `dir` contains an `__init__.py` / `__init__.pyi` in the project.
fn is_package_dir(dir: &str, project: &ProjectFiles) -> bool {
    project.contains(&join(dir, "__init__.py")) || project.contains(&join(dir, "__init__.pyi"))
}

/// True if `ancestor` is `path` itself or a directory above it (`""` is the
/// repo root and an ancestor of everything).
fn is_dir_ancestor(ancestor: &str, path: &str) -> bool {
    ancestor.is_empty()
        || path == ancestor
        || (path.len() > ancestor.len() && path.starts_with(ancestor) && path.as_bytes()[ancestor.len()] == b'/')
}

/// The deepest `src` directory that is an ancestor of `dir` (or `dir` itself).
fn nearest_src_dir(dir: &str) -> Option<&str> {
    let mut best = None;
    let mut offset = 0;
    for comp in dir.split('/') {
        let end = offset + comp.len();
        if comp == "src" {
            best = Some(&dir[..end]);
        }
        offset = end + 1;
    }
    best
}

/// Source roots declared in packaging configuration (best effort, line
/// based, no TOML/INI dependency):
///
/// - `pyproject.toml`: `[tool.setuptools] package-dir = {"" = "lib"}`,
///   `[tool.setuptools.package-dir] "" = "lib"`,
///   `[tool.setuptools.packages.find] where = ["lib"]`,
///   poetry `packages = [{ include = "x", from = "lib" }]`,
///   hatch `[tool.hatch.build.targets.wheel] packages = ["lib/x"]`.
/// - `setup.cfg`: `[options] package_dir = =lib`, `[options.packages.find] where = lib`.
///
/// Roots are repository-relative (prefixed with the config file's directory)
/// and sorted for determinism.
pub(crate) fn configured_roots(project: &ProjectFiles) -> Vec<String> {
    let mut roots = Vec::new();
    for (path, content) in &project.configs {
        let (dir, name) = split_dir(path);
        let found = match name {
            "pyproject.toml" => pyproject_roots(content),
            "setup.cfg" => setup_cfg_roots(content),
            _ => Vec::new(),
        };
        for r in found {
            let r = r.trim().trim_start_matches("./").trim_end_matches('/');
            if r.is_empty() || r == "." || r.contains("..") {
                continue;
            }
            roots.push(join(dir, r));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Every double- or single-quoted string on a line, in order.
fn quoted_strings(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[start] as char;
        let after = &rest[start + 1..];
        match after.find(quote) {
            Some(end) => {
                out.push(&after[..end]);
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

fn strip_comment(line: &str) -> &str {
    // `#` inside quoted strings is rare in these keys; keep it simple.
    line.split_once('#').map(|(a, _)| a).unwrap_or(line).trim()
}

fn pyproject_roots(content: &str) -> Vec<String> {
    let mut roots = Vec::new();
    let mut section = String::new();
    let mut in_poetry_packages = false;
    for raw in content.lines() {
        let line = strip_comment(raw);
        if line.starts_with('[') && !line.starts_with("[{") && line.ends_with(']') && !line.contains('=') {
            section = line.trim_matches(['[', ']']).trim().to_string();
            in_poetry_packages = false;
            continue;
        }
        let key = line.split_once('=').map(|(k, _)| k.trim()).unwrap_or("");
        if section == "tool.poetry" || section.starts_with("tool.poetry.") {
            in_poetry_packages = poetry_line_roots(line, key, in_poetry_packages, &mut roots);
        } else {
            setuptools_line_roots(&section, key, line, &mut roots);
        }
    }
    roots
}

/// Roots declared by one line of a setuptools / hatch section of
/// `pyproject.toml`.
fn setuptools_line_roots(section: &str, key: &str, line: &str, roots: &mut Vec<String>) {
    match section {
        "tool.setuptools" if key == "package-dir" || key == "package_dir" => {
            let strings = quoted_strings(line);
            // {"" = "lib"}: an empty key followed by its value.
            if let Some(pos) = strings.iter().position(|s| s.is_empty())
                && let Some(v) = strings.get(pos + 1)
            {
                roots.push(v.to_string());
            }
        }
        "tool.setuptools.package-dir" | "tool.setuptools.package_dir" => {
            let strings = quoted_strings(line);
            if strings.len() >= 2 && strings[0].is_empty() {
                roots.push(strings[1].to_string());
            }
        }
        "tool.setuptools.packages.find" if key == "where" => {
            roots.extend(quoted_strings(line).into_iter().map(String::from));
        }
        "tool.hatch.build.targets.wheel" if key == "packages" => {
            let dirs = quoted_strings(line).into_iter().filter_map(|s| s.trim_end_matches('/').rsplit_once('/'));
            roots.extend(dirs.map(|(d, _)| d.to_string()));
        }
        _ => {}
    }
}

/// One line of a `tool.poetry*` section: collects the `from = "…"` roots of
/// `packages = [...]`. Returns whether the next line is still inside the
/// `packages` list.
fn poetry_line_roots(line: &str, key: &str, in_packages: bool, roots: &mut Vec<String>) -> bool {
    if key != "packages" && !in_packages {
        return false;
    }
    if let Some(idx) = line.find("from") {
        let after = &line[idx + 4..];
        if after.trim_start().starts_with('=')
            && let Some(v) = quoted_strings(after).first()
        {
            roots.push(v.to_string());
        }
    }
    !line.ends_with(']')
}

fn setup_cfg_roots(content: &str) -> Vec<String> {
    let mut roots = Vec::new();
    let mut section = String::new();
    let mut continuation_key = String::new();
    for raw in content.lines() {
        let indented = raw.starts_with([' ', '\t']);
        let line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }
        if !indented && line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']).trim().to_string();
            continuation_key.clear();
            continue;
        }
        let (key, value) = if indented {
            (continuation_key.clone(), line.to_string())
        } else {
            let Some((k, v)) = line.split_once('=') else { continue };
            continuation_key = k.trim().to_string();
            (continuation_key.clone(), v.trim().to_string())
        };
        if let Some(root) = setup_cfg_root(&section, &key, &value) {
            roots.push(root);
        }
    }
    roots
}

/// The source root a `setup.cfg` entry declares, if any.
fn setup_cfg_root(section: &str, key: &str, value: &str) -> Option<String> {
    let v = match (section, key) {
        // `=src` or ` = src` (empty package name maps to the root).
        ("options", "package_dir") => value.trim().strip_prefix('=')?.trim(),
        ("options.packages.find", "where") => value.trim(),
        _ => return None,
    };
    (!v.is_empty()).then(|| v.to_string())
}

/// Strips `.py` / `.pyi`; `None` for other extensions.
fn strip_py_ext(file: &str) -> Option<&str> {
    file.strip_suffix(".py").or_else(|| file.strip_suffix(".pyi"))
}

/// The source root of a Python file: the directory module names are
/// relative to. See the crate docs for the rules.
pub(crate) fn source_root(path: &str, project: &ProjectFiles, configured: &[String]) -> String {
    let (dir, _) = split_dir(path);
    // Known roots that contain the file: configured roots and the nearest
    // `src/`. The deepest one wins.
    let known = configured
        .iter()
        .map(String::as_str)
        .chain(nearest_src_dir(dir))
        .filter(|r| is_dir_ancestor(r, dir))
        .max_by_key(|r| r.len());

    if is_package_dir(dir, project) {
        let mut top = dir;
        while let Some(parent) = parent_dir(top) {
            if !is_package_dir(parent, project) {
                break;
            }
            top = parent;
        }
        let root = parent_dir(top).unwrap_or("");
        // Namespace packages under a known root: `src/ns/pkg/__init__.py`
        // without `src/ns/__init__.py` is still `ns.pkg`.
        if let Some(k) = known
            && is_dir_ancestor(k, root)
            && k != root
        {
            return k.to_string();
        }
        return root.to_string();
    }
    known.unwrap_or("").to_string()
}

/// Dotted module name of a Python file.
pub(crate) fn module_name(path: &str, project: &ProjectFiles) -> String {
    let configured = configured_roots(project);
    let root = source_root(path, project, &configured);
    let rel = if root.is_empty() {
        path
    } else {
        path.strip_prefix(&root).map(|r| r.trim_start_matches('/')).unwrap_or(path)
    };
    let (dir, file) = split_dir(rel);
    let stem = strip_py_ext(file).unwrap_or(file);
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    if stem != "__init__" {
        parts.push(stem);
    }
    if parts.is_empty() {
        // A top-level `__init__.py`: name it after nothing; use the stem.
        return stem.to_string();
    }
    parts.join(".")
}

/// Tries `<base>/<parts>.py`, `.pyi`, `/__init__.py`, `/__init__.pyi`.
fn find_module_file(base: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    let rel = module.replace('.', "/");
    let stem = join(base, &rel);
    [format!("{stem}.py"), format!("{stem}.pyi"), join(&stem, "__init__.py"), join(&stem, "__init__.pyi")]
        .into_iter()
        .find(|candidate| project.contains(candidate))
}

pub(crate) fn resolve_import(from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
    let module = import.module.trim();
    if module.split('.').any(|p| p.is_empty()) && !module.is_empty() {
        return None;
    }
    if import.relative_level == 0 {
        return resolve_absolute(from_path, module, project);
    }
    let (mut base, _) = split_dir(from_path);
    for _ in 1..import.relative_level {
        base = parent_dir(base)?;
    }
    if module.is_empty() {
        return ["__init__.py", "__init__.pyi"].into_iter().map(|init| join(base, init)).find(|c| project.contains(c));
    }
    find_module_file(base, module, project)
}

/// `import a.b`: tried under the repository root, `src/`, the configured
/// roots and the importing file's own source root, in that order.
fn resolve_absolute(from_path: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    if module.is_empty() {
        return None;
    }
    let configured = configured_roots(project);
    let own_root = source_root(from_path, project, &configured);
    let mut roots: Vec<String> = vec![String::new(), "src".to_string()];
    roots.extend(configured);
    roots.push(own_root);
    let mut seen = std::collections::BTreeSet::new();
    roots.into_iter().filter(|root| seen.insert(root.clone())).find_map(|root| find_module_file(&root, module, project))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(paths: &[&str]) -> ProjectFiles {
        ProjectFiles { paths: paths.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn imp(module: &str, level: u32) -> Import {
        Import {
            module: module.to_string(),
            relative_level: level,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Default::default(),
        }
    }

    #[test]
    fn flat_layout_names() {
        let p = project(&["pkg/__init__.py", "pkg/sub/__init__.py", "pkg/sub/mod.py", "setup.py", "scripts/run.py"]);
        assert_eq!(module_name("pkg/__init__.py", &p), "pkg");
        assert_eq!(module_name("pkg/sub/mod.py", &p), "pkg.sub.mod");
        assert_eq!(module_name("pkg/sub/__init__.py", &p), "pkg.sub");
        assert_eq!(module_name("setup.py", &p), "setup");
        assert_eq!(module_name("scripts/run.py", &p), "scripts.run");
    }

    #[test]
    fn src_layout_names() {
        let p = project(&[
            "src/pkg/__init__.py",
            "src/pkg/mod.py",
            "src/tool.py",
            "src/ns/inner/__init__.py",
            "src/ns/inner/m.pyi",
        ]);
        assert_eq!(module_name("src/pkg/mod.py", &p), "pkg.mod");
        assert_eq!(module_name("src/pkg/__init__.py", &p), "pkg");
        assert_eq!(module_name("src/tool.py", &p), "tool");
        // Namespace package (no src/ns/__init__.py) under a src root.
        assert_eq!(module_name("src/ns/inner/m.pyi", &p), "ns.inner.m");
    }

    #[test]
    fn nested_src_and_tests() {
        let p = project(&["services/api/src/app/__init__.py", "services/api/src/app/main.py", "tests/test_x.py"]);
        assert_eq!(module_name("services/api/src/app/main.py", &p), "app.main");
        assert_eq!(module_name("tests/test_x.py", &p), "tests.test_x");
    }

    #[test]
    fn configured_roots_from_pyproject_and_setup_cfg() {
        let mut p = project(&["lib/pkg/__init__.py", "lib/pkg/a.py", "lib/solo.py", "be/code/x/__init__.py"]);
        p.configs.insert(
            "pyproject.toml".into(),
            "[project]\nname='x'\n[tool.setuptools]\npackage-dir = {\"\" = \"lib\"}\n".into(),
        );
        p.configs.insert("be/setup.cfg".into(), "[options]\npackage_dir =\n    =code\n".into());
        assert_eq!(configured_roots(&p), vec!["be/code".to_string(), "lib".to_string()]);
        assert_eq!(module_name("lib/solo.py", &p), "solo");
        assert_eq!(module_name("lib/pkg/a.py", &p), "pkg.a");

        let mut q = ProjectFiles::default();
        q.configs.insert(
            "pyproject.toml".into(),
            "[tool.setuptools.packages.find]\nwhere = [\"lib\"]  # comment\n[tool.poetry]\npackages = [\n  { include = \"x\", from = \"source\" },\n]\n".into(),
        );
        assert_eq!(configured_roots(&q), vec!["lib".to_string(), "source".to_string()]);
    }

    #[test]
    fn absolute_imports() {
        let p = project(&[
            "src/pkg/__init__.py",
            "src/pkg/mod.py",
            "src/pkg/stub.pyi",
            "top.py",
            "src/pkg/sub/__init__.py",
        ]);
        assert_eq!(resolve_import("x.py", &imp("pkg.mod", 0), &p).as_deref(), Some("src/pkg/mod.py"));
        assert_eq!(resolve_import("x.py", &imp("pkg", 0), &p).as_deref(), Some("src/pkg/__init__.py"));
        assert_eq!(resolve_import("x.py", &imp("pkg.sub", 0), &p).as_deref(), Some("src/pkg/sub/__init__.py"));
        assert_eq!(resolve_import("x.py", &imp("pkg.stub", 0), &p).as_deref(), Some("src/pkg/stub.pyi"));
        assert_eq!(resolve_import("src/pkg/mod.py", &imp("top", 0), &p).as_deref(), Some("top.py"));
        assert_eq!(resolve_import("x.py", &imp("os.path", 0), &p), None);
        assert_eq!(resolve_import("x.py", &imp("", 0), &p), None);
        assert_eq!(resolve_import("x.py", &imp("pkg..mod", 0), &p), None);
    }

    #[test]
    fn absolute_imports_from_own_root() {
        // A nested src root is only found through the importing file's root.
        let p = project(&["svc/src/app/__init__.py", "svc/src/app/db.py", "svc/src/app/main.py"]);
        assert_eq!(resolve_import("svc/src/app/main.py", &imp("app.db", 0), &p).as_deref(), Some("svc/src/app/db.py"));
        assert_eq!(resolve_import("other.py", &imp("app.db", 0), &p), None);
    }

    #[test]
    fn relative_imports() {
        let p = project(&[
            "pkg/__init__.py",
            "pkg/a.py",
            "pkg/sub/__init__.py",
            "pkg/sub/b.py",
            "pkg/sub/c/__init__.py",
            "pkg/util/x.py",
        ]);
        assert_eq!(resolve_import("pkg/sub/b.py", &imp("", 1), &p).as_deref(), Some("pkg/sub/__init__.py"));
        assert_eq!(resolve_import("pkg/sub/b.py", &imp("c", 1), &p).as_deref(), Some("pkg/sub/c/__init__.py"));
        assert_eq!(resolve_import("pkg/sub/b.py", &imp("a", 2), &p).as_deref(), Some("pkg/a.py"));
        assert_eq!(resolve_import("pkg/sub/b.py", &imp("util.x", 2), &p).as_deref(), Some("pkg/util/x.py"));
        assert_eq!(resolve_import("pkg/sub/__init__.py", &imp("b", 1), &p).as_deref(), Some("pkg/sub/b.py"));
        assert_eq!(resolve_import("pkg/sub/b.py", &imp("", 2), &p).as_deref(), Some("pkg/__init__.py"));
        // No __init__ in pkg/util: `from . import` has no target.
        assert_eq!(resolve_import("pkg/util/x.py", &imp("", 1), &p), None);
        // Going above the repository root.
        assert_eq!(resolve_import("pkg/a.py", &imp("z", 5), &p), None);
        assert_eq!(resolve_import("pkg/a.py", &imp("missing", 1), &p), None);
    }
}
