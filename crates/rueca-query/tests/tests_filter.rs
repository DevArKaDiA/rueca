//! `trace` with `include_tests: false` leaves out code nested in tests:
//! closures of a `describe` callback and of a `beforeEach` hook
//! (TypeScript), a function nested in a `test_*` function (Python) or in a
//! `#[test]` function (Rust). Go `t.Run` closures are not definitions: their
//! calls belong to the `Test…` function. A module-level helper of a test
//! file is not a test (the classification is by definition, not by path).

use std::path::Path;

use rueca_index::{IndexOptions, index_worktree};
use rueca_query::Engine;
use serde_json::{Value, json};

const FILES: &[(&str, &str)] = &[
    ("src/lib.ts", "export function target(): number {\n  return 1;\n}\n"),
    (
        "src/lib.test.ts",
        "import { target } from './lib';\n\
         function setupAll() {\n  return target();\n}\n\
         beforeEach(() => {\n  function seed() {\n    return target();\n  }\n  seed();\n});\n\
         describe('target', () => {\n  const Wrapper = () => target();\n  it('works', () => {\n    Wrapper();\n  });\n});\n",
    ),
    ("pkg/__init__.py", ""),
    ("pkg/core.py", "def target():\n    return 1\n"),
    (
        "tests/test_core.py",
        "from pkg.core import target\n\n\ndef test_target():\n    def inner():\n        return target()\n    assert inner() == 1\n",
    ),
    (
        "src/lib.rs",
        "pub fn target() -> u32 {\n    1\n}\n\n#[test]\nfn works() {\n    fn nested() -> u32 {\n        target()\n    }\n    assert_eq!(nested(), 1);\n}\n",
    ),
    ("go.mod", "module example.com/m\n\ngo 1.22\n"),
    ("core/core.go", "package core\n\nfunc Target() int { return 1 }\n"),
    (
        "core/core_test.go",
        "package core\n\nimport \"testing\"\n\nfunc TestTarget(t *testing.T) {\n\tt.Run(\"one\", func(t *testing.T) {\n\t\t_ = Target()\n\t})\n}\n",
    ),
];

fn engine(dir: &Path) -> Engine {
    let root = dir.join("mixed");
    for (path, text) in FILES {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let store = dir.join("store");
    let opts = IndexOptions { root: root.clone(), store: store.clone(), ..IndexOptions::default() };
    index_worktree(&opts).unwrap();
    Engine::new(store, Some(root)).unwrap()
}

/// `(local name, test)` of every caller of `args`'s root, and `tests_excluded`.
fn callers(e: &Engine, args: Value) -> (Vec<(String, bool)>, u64) {
    let r = e.call("trace", args.clone()).unwrap_or_else(|err| panic!("{args}: {err:#}"));
    let nodes = r["results"]["nodes"].as_array().unwrap();
    let mut out: Vec<(String, bool)> = nodes[1..]
        .iter()
        .map(|n| {
            let id = n["id"].as_str().unwrap();
            let sym = e.call("get_symbol", json!({ "id": id })).unwrap();
            (n["local"].as_str().unwrap().to_string(), sym["results"]["symbol"]["test"] == true)
        })
        .collect();
    out.sort();
    (out, r["results"]["tests_excluded"].as_u64().unwrap())
}

fn names(v: &[(&str, bool)]) -> Vec<(String, bool)> {
    v.iter().map(|(n, t)| (n.to_string(), *t)).collect()
}

#[test]
fn code_nested_in_tests_is_a_test() {
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path());

    // TypeScript: `seed` (in a hook) and `Wrapper` (in a `describe`) are
    // tests; the module-level `setupAll` is not.
    let ts = json!({"name": "target", "path": "src/lib.ts", "direction": "callers"});
    let (shown, excluded) = callers(&e, ts.clone());
    assert_eq!(shown, names(&[("setupAll", false)]));
    assert_eq!(excluded, 2);
    let (all, _) =
        callers(&e, json!({"name": "target", "path": "src/lib.ts", "direction": "callers", "include_tests": true}));
    assert_eq!(all, names(&[("seed", true), ("setupAll", false), ("target.Wrapper", true)]));

    // Python: `inner` is nested in `test_target`.
    let py = json!({"qualified_name": "pkg.core.target", "direction": "callers", "include_tests": true});
    assert_eq!(callers(&e, py).0, names(&[("test_target.inner", true)]));
    let (shown, excluded) = callers(&e, json!({"qualified_name": "pkg.core.target", "direction": "callers"}));
    assert_eq!((shown, excluded), (vec![], 1));

    // Rust: `nested` is nested in the `#[test]` function `works`.
    let rs = json!({"name": "target", "path": "src/lib.rs", "direction": "callers", "include_tests": true});
    assert_eq!(callers(&e, rs).0, names(&[("works.nested", true)]));
    let (shown, excluded) = callers(&e, json!({"name": "target", "path": "src/lib.rs", "direction": "callers"}));
    assert_eq!((shown, excluded), (vec![], 1));

    // Go: the `t.Run` closure's call belongs to `TestTarget`.
    let go = json!({"name": "Target", "path": "core/core.go", "direction": "callers", "include_tests": true});
    assert_eq!(callers(&e, go).0, names(&[("TestTarget", true)]));
    let (shown, excluded) = callers(&e, json!({"name": "Target", "path": "core/core.go", "direction": "callers"}));
    assert_eq!((shown, excluded), (vec![], 1));
}
