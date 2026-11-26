//! Kotlin source layout: the package implied by a path (used only for files
//! without facts) and test paths.
//!
//! Imports do not go through paths: a Kotlin file may declare any package
//! wherever it lives (the Kotlin conventions even omit the common root
//! package from directories), so the pack opts into
//! [`rueca_extract::LanguagePack::namespace_imports`] and the resolver binds
//! imports by package name.

/// Source-set directory names under `src/` that hold Kotlin or Java code
/// directly (`src/main/kotlin`, `src/commonMain/kotlin`, `src/test/java`).
const LANGUAGE_DIRS: &[&str] = &["kotlin", "java"];

/// The package implied by a path under a conventional source root
/// (`…/src/<set>/kotlin/a/b/F.kt` → `a.b`); empty elsewhere.
pub(crate) fn package_of_path(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    let dir = &segs[..segs.len().saturating_sub(1)];
    let root = (0..dir.len().saturating_sub(2)).rev().find(|&i| dir[i] == "src" && LANGUAGE_DIRS.contains(&dir[i + 2]));
    match root {
        Some(i) => dir[i + 3..].join("."),
        None => String::new(),
    }
}

/// Whether a Kotlin source path is test code: a `test` / `tests` directory
/// (also KMP and Android source sets such as `src/jvmTest`,
/// `src/androidTest`, `src/commonTest`, `src/testFixtures`) or a file named
/// `*Test.kt` / `*Tests.kt` / `*Spec.kt` / `*IT.kt`.
pub fn is_test_path(path: &str) -> bool {
    let mut segs: Vec<&str> = path.split('/').collect();
    let name = segs.pop().unwrap_or("");
    let test_set = segs
        .windows(2)
        .any(|w| w[0] == "src" && (w[1].ends_with("Test") || w[1].starts_with("test") || w[1].ends_with("Tests")));
    let stem = name.strip_suffix(".kt").unwrap_or(name);
    test_set
        || segs.iter().any(|s| matches!(*s, "test" | "tests"))
        || ["Test", "Tests", "Spec", "IT"].iter().any(|s| stem.ends_with(s) && stem.len() > s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_from_paths() {
        assert_eq!(package_of_path("okhttp/src/main/kotlin/okhttp3/Call.kt"), "okhttp3");
        assert_eq!(package_of_path("lib/src/commonMain/kotlin/a/b/C.kt"), "a.b");
        assert_eq!(package_of_path("src/test/java/a/T.kt"), "a");
        assert_eq!(package_of_path("build.gradle.kts"), "");
        assert_eq!(package_of_path("scripts/x.kts"), "");
    }

    #[test]
    fn test_paths() {
        assert!(is_test_path("app/src/androidTest/kotlin/a/UiTest.kt"));
        assert!(is_test_path("lib/src/jvmTest/kotlin/a/B.kt"));
        assert!(is_test_path("lib/src/commonMain/kotlin/a/ParserTest.kt"));
        assert!(is_test_path("lib/src/test/kotlin/a/B.kt"));
        assert!(!is_test_path("lib/src/main/kotlin/a/Test.kt"));
        assert!(!is_test_path("lib/src/main/kotlin/a/Testing.kt"));
    }
}
