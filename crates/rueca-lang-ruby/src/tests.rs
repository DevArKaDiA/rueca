//! Test code: test files, RSpec groups and examples, Minitest and
//! ActiveSupport tests.

/// RSpec example groups (`describe`, `context`, shared groups).
const GROUPS: &[&str] = &[
    "describe",
    "context",
    "feature",
    "shared_examples",
    "shared_examples_for",
    "shared_context",
    "xdescribe",
    "fdescribe",
    "xcontext",
    "fcontext",
];

/// RSpec examples and the ActiveSupport / minitest-spec `test "…" do`.
const EXAMPLES: &[&str] =
    &["it", "specify", "example", "scenario", "its", "xit", "fit", "xspecify", "xexample", "test"];

pub(crate) fn is_group(name: &str) -> bool {
    GROUPS.contains(&name)
}

/// A call that opens a test group or example when it has a block.
pub(crate) fn is_test_call(name: &str) -> bool {
    is_group(name) || EXAMPLES.contains(&name)
}

/// Ruby test files: under `test/`, `tests/`, `spec/` or `specs/`, or named
/// `*_test.rb` or `*_spec.rb` (also the generic test paths of
/// `rueca_model::is_test_path`). `test_*.rb` counts only under a test
/// directory (`lib/rack/test_utils.rb` is library code).
pub fn is_test_file(path: &str) -> bool {
    if rueca_model::is_test_path(path) {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = name.strip_suffix(".rb") else { return false };
    stem.ends_with("_test") || stem.ends_with("_spec")
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn test_files() {
        for p in [
            "spec/models/user_spec.rb",
            "test/test_helper.rb",
            "lib/foo_test.rb",
            "test/test_parser.rb",
            "a/specs/x.rb",
        ] {
            assert!(is_test_file(p), "{p}");
        }
        for p in ["lib/rack/test_utils.rb", "test_parser.rb", "lib/spectacle.rb", "app/models/test.rb"] {
            assert!(!is_test_file(p), "{p}");
        }
    }
}
