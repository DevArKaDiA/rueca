//! Swift modules: which module (SwiftPM target) a file belongs to, and
//! which directory an `import` names.
//!
//! **Module of a file.** The nearest enclosing `Package.swift` (the one in
//! the deepest directory above the file) is read for targets with a custom
//! `path:` (`.target(name: "Alamofire", path: "Source")`): a file under
//! `<package dir>/<path>/` belongs to that target (the longest such path
//! wins). Otherwise the conventional layout decides: the directory after
//! the last `Sources`, `Source`, `src`, `srcs`, `Tests` or `Plugins`
//! segment (`Sources/ArgumentParser/Parsing/Parser.swift` →
//! `ArgumentParser`). A file outside such a layout (an Xcode project without
//! `Package.swift`) belongs to its top-level directory (`MyApp/View.swift` →
//! `MyApp`), best effort; a file at the repository root to the module `""`.
//!
//! **Import targets.** `import Foo` names the directory of target `Foo`:
//! its custom `path:`, else `<package dir>/Sources/Foo` (or `Source`,
//! `Tests`, …) when the snapshot has it, else a top-level directory `Foo`.
//! The resolver binds imports by module name (namespace imports); this
//! answer tells the organization linker which imports the repository
//! resolves itself.
//!
//! The manifest is read with the Swift grammar, not evaluated
//! (`manifest.rs`).

use rueca_extract::ProjectFiles;

use crate::manifest::manifest_targets;

/// Directories whose children are target directories by SwiftPM convention.
const TARGET_ROOTS: &[&str] = &["Sources", "Source", "src", "srcs", "Tests", "Plugins"];

/// The directory of a path (`""` for a top-level file).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `dir/name` (or `name` when `dir` is the root).
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// The manifest governing `path`: the `Package.swift` in the deepest
/// directory above it, with its directory.
fn governing_manifest<'p>(path: &str, project: &'p ProjectFiles) -> Option<(&'p str, &'p str)> {
    project
        .configs
        .iter()
        .filter(|(p, _)| p.rsplit('/').next() == Some("Package.swift"))
        .map(|(p, t)| (dir_of(p), t.as_str()))
        .filter(|(d, _)| d.is_empty() || path.starts_with(&format!("{d}/")))
        .max_by_key(|(d, _)| d.len())
}

/// The target of `path` declared by a custom `path:` in its manifest.
fn custom_target(path: &str, project: &ProjectFiles) -> Option<String> {
    let (dir, text) = governing_manifest(path, project)?;
    manifest_targets(text)
        .iter()
        .filter_map(|t| {
            let p = t.path.as_deref()?.trim_matches('/');
            let full = if p.is_empty() || p == "." { dir.to_string() } else { join(dir, p) };
            let prefix = if full.is_empty() { String::new() } else { format!("{full}/") };
            path.starts_with(&prefix).then(|| (prefix.len(), t.name.clone()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, n)| n)
}

/// The module by SwiftPM convention: the directory after the last
/// `Sources` / `Tests` / … segment; else the top-level directory.
pub fn conventional_module(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    let dirs = &segs[..segs.len().saturating_sub(1)];
    if let Some(i) = (0..dirs.len().saturating_sub(1)).rev().find(|&i| TARGET_ROOTS.contains(&dirs[i])) {
        return dirs[i + 1].to_string();
    }
    dirs.first().map(|d| d.to_string()).unwrap_or_default()
}

/// The module (SwiftPM target) of `path`.
pub fn module_of_path(path: &str, project: &ProjectFiles) -> String {
    custom_target(path, project).unwrap_or_else(|| conventional_module(path))
}

/// The directory of module `module` in the snapshot, if any.
pub fn module_dir(module: &str, project: &ProjectFiles) -> Option<String> {
    let manifests: Vec<(&str, &str)> = project
        .configs
        .iter()
        .filter(|(p, _)| p.rsplit('/').next() == Some("Package.swift"))
        .map(|(p, t)| (dir_of(p), t.as_str()))
        .collect();
    for (dir, text) in &manifests {
        for t in manifest_targets(text).iter().filter(|t| t.name == module) {
            if let Some(p) = t.path.as_deref().map(|p| join(dir, p.trim_matches('/'))).filter(|p| project.is_dir(p)) {
                return Some(p);
            }
        }
        for root in TARGET_ROOTS {
            let cand = join(dir, &format!("{root}/{module}"));
            if project.is_dir(&cand) {
                return Some(cand);
            }
        }
    }
    project.is_dir(module).then(|| module.to_string())
}

/// Swift test code by path: a file of a test target (`Tests/<Target>/…`)
/// or named `*Tests.swift` / `*Test.swift`. Used for package publishing;
/// test definitions in facts come from the code (`XCTestCase`, `@Test`).
pub fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".swift").unwrap_or(name);
    path.split('/').any(|s| s == "Tests") || stem.ends_with("Tests") || stem.ends_with("Test")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "Alamofire",
    products: [.library(name: "Alamofire", targets: ["Alamofire"])],
    dependencies: [.package(url: "https://github.com/apple/swift-log.git", from: "1.0.0")],
    targets: [
        .target(name: "Alamofire", path: "Source", exclude: ["Info.plist"]),
        .target(name: "Helpers", dependencies: [.product(name: "Logging", package: "swift-log")]),
        .testTarget(name: "AlamofireTests", dependencies: ["Alamofire"], path: "Tests"),
    ]
)
"#;

    fn project(paths: &[&str], manifests: &[(&str, &str)]) -> ProjectFiles {
        ProjectFiles {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            configs: manifests.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        }
    }

    #[test]
    fn modules_of_paths() {
        let p = project(
            &["Source/Core/Session.swift", "Sources/Helpers/A.swift", "Tests/SessionTests.swift"],
            &[("Package.swift", MANIFEST)],
        );
        assert_eq!(module_of_path("Source/Core/Session.swift", &p), "Alamofire");
        assert_eq!(module_of_path("Sources/Helpers/A.swift", &p), "Helpers");
        assert_eq!(module_of_path("Tests/SessionTests.swift", &p), "AlamofireTests");
        let bare = project(&[], &[]);
        assert_eq!(module_of_path("Sources/ArgumentParser/Parsing/Parser.swift", &bare), "ArgumentParser");
        assert_eq!(module_of_path("Packages/Kit/Tests/KitTests/A.swift", &bare), "KitTests");
        assert_eq!(module_of_path("MyApp/Views/Main.swift", &bare), "MyApp");
        assert_eq!(module_of_path("main.swift", &bare), "");
    }

    #[test]
    fn import_directories() {
        let p = project(&["Source/Core/Session.swift", "Sources/Helpers/A.swift"], &[("Package.swift", MANIFEST)]);
        assert_eq!(module_dir("Alamofire", &p).as_deref(), Some("Source"));
        assert_eq!(module_dir("Helpers", &p).as_deref(), Some("Sources/Helpers"));
        assert_eq!(module_dir("Foundation", &p), None);
    }

    #[test]
    fn test_paths() {
        assert!(is_test_path("Tests/FooTests/BarTests.swift"));
        assert!(is_test_path("App/LoginTest.swift"));
        assert!(!is_test_path("Sources/Foo/Bar.swift"));
    }
}
