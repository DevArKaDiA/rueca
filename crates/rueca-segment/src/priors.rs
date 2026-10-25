//! Query-independent score multipliers of the symbol index (see the crate
//! docs, *Symbol index*): data and tests rank below code, declarations
//! below the bodies they declare, and generated copies of code (amalgamated
//! single headers) and vendored third-party trees below first-party code.
//!
//! Everything here is computed at open from the node and file tables plus
//! the optional `symbols.bodiless` component, so a policy change needs no
//! re-index.

use std::collections::HashMap;

use roaring::RoaringBitmap;
use rueca_model::{FileEntry, Label, Language, LanguageFamily, Node};

/// Fields, variables and constants.
pub(crate) const DATA_PRIOR: f32 = 0.5;
/// Tests (`is_test`, `Test` label or a test file path).
pub(crate) const TEST_PRIOR: f32 = 0.5;
/// C/C++ callables without a body (prototypes, in-class method
/// declarations, `= default` / `= delete`, function-like macros).
pub(crate) const BODILESS_PRIOR: f32 = 0.5;
/// Definitions of a file detected as a copy of other files (an amalgamated
/// single header such as `single_include/nlohmann/json.hpp`).
pub(crate) const COPY_PRIOR: f32 = 0.5;
/// Definitions under a vendored third-party directory.
pub(crate) const VENDORED_PRIOR: f32 = 0.5;

/// Copy detection: definitions shorter than this many lines are too generic
/// (one-line getters, empty constructors) to identify copied code.
const COPY_MIN_LINES: u32 = 4;
/// A fingerprint shared by more files than this is a common idiom, not a
/// copy.
const COPY_MAX_GROUP: usize = 8;
/// A copy has at least this many fingerprinted definitions…
const COPY_MIN_DEFS: u32 = 50;
/// …most of them also defined elsewhere…
const COPY_MIN_SHARE: f32 = 0.6;
/// …and no single other file holds more than this share of those: an
/// amalgamation copies many files, while a file and its one copy (or
/// per-platform siblings, or generated stubs) explain each other. Measured
/// on the benchmark corpora: amalgamations 0.08–0.30 (0.65 for an early
/// nlohmann/json single header), sibling files 0.79–1.0.
const COPY_MAX_PARTNER_SHARE: f32 = 0.7;

/// Directory names of vendored third-party code. `vendor` and
/// `node_modules` are not indexed at all (`rueca-index`).
const VENDORED_DIRS: [&str; 7] =
    ["third_party", "thirdparty", "third-party", "3rdparty", "vendored", "deps", "dependencies"];

fn is_c_family(lang: Option<Language>) -> bool {
    lang.is_some_and(|l| l.family() == LanguageFamily::C)
}

/// Whether a node with source text `body` is a C/C++ callable without a body:
/// a `Function` or `Method` whose text has no `{` (a prototype, an in-class
/// method declaration, `= default` / `= delete`, a pure virtual method, a
/// function-like macro without braces). Written at index time into the
/// `symbols.bodiless` component.
pub(crate) fn is_bodiless(node: &Node, body: &str) -> bool {
    is_c_family(node.language) && matches!(node.label, Label::Function | Label::Method) && !body.contains('{')
}

/// Test files by path: a `test`, `tests`, `__tests__`, `spec`, `specs`,
/// `testing` or `e2e` directory, or a `test_*.py`, `*_test.py`, `conftest.py`,
/// `*.test.*` or `*.spec.*` file name.
pub(crate) fn is_test_path(path: &str) -> bool {
    let mut segs: Vec<&str> = path.split('/').collect();
    let name = segs.pop().unwrap_or("");
    if segs.iter().any(|s| matches!(*s, "test" | "tests" | "__tests__" | "spec" | "specs" | "testing" | "e2e")) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name);
    (name.ends_with(".py") && (stem.starts_with("test_") || stem.ends_with("_test") || stem == "conftest"))
        || name.split('.').skip(1).any(|p| p == "test" || p == "spec")
}

/// A path under a vendored directory (see [`VENDORED_DIRS`]).
pub(crate) fn is_vendored_path(path: &str) -> bool {
    let mut segs = path.split('/');
    segs.next_back();
    segs.any(|s| VENDORED_DIRS.contains(&s))
}

fn span_lines(node: &Node) -> u32 {
    node.span.map_or(0, |s| s.end_line.saturating_sub(s.start_line) + 1)
}

/// Files whose definitions are mostly copies of definitions spread over many
/// other files of the same language family (amalgamated single headers and
/// sources). A definition's fingerprint is (family, label, name, signature,
/// line count) — the qualified name is left out because amalgamation scripts
/// may rename the enclosing namespace (simdjson's
/// `SIMDJSON_BUILTIN_IMPLEMENTATION`). Only callables and types with a body
/// and at least [`COPY_MIN_LINES`] lines count.
pub(crate) fn copy_files(nodes: &[Node], files: &[FileEntry], bodiless: &RoaringBitmap) -> Vec<bool> {
    type Key<'a> = (LanguageFamily, Label, &'a str, &'a str, u32);
    let mut groups: HashMap<Key<'_>, Vec<u32>> = HashMap::new();
    let mut defs: Vec<u32> = vec![0; files.len()];
    let mut keyed: Vec<(u32, Key<'_>)> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let (Some(f), Some(lang)) = (n.file, n.language) else { continue };
        let callable_or_type = matches!(
            n.label,
            Label::Function
                | Label::Method
                | Label::Class
                | Label::Struct
                | Label::Interface
                | Label::Trait
                | Label::Enum
        );
        let lines = span_lines(n);
        if !callable_or_type || lines < COPY_MIN_LINES || bodiless.contains(i as u32) || f as usize >= files.len() {
            continue;
        }
        let key = (lang.family(), n.label, n.name.as_str(), n.signature.as_deref().unwrap_or(""), lines);
        defs[f as usize] += 1;
        let g = groups.entry(key).or_default();
        if !g.contains(&f) {
            g.push(f);
        }
        keyed.push((f, key));
    }
    // Per file: definitions found in another file, and per partner file how
    // many of them it holds.
    let mut shared: Vec<u32> = vec![0; files.len()];
    let mut partners: HashMap<(u32, u32), u32> = HashMap::new();
    for (f, key) in &keyed {
        let g = &groups[key];
        if g.len() < 2 || g.len() > COPY_MAX_GROUP {
            continue;
        }
        shared[*f as usize] += 1;
        for &p in g.iter().filter(|&&p| p != *f) {
            *partners.entry((*f, p)).or_default() += 1;
        }
    }
    let mut max_partner: Vec<u32> = vec![0; files.len()];
    for (&(f, _), &c) in &partners {
        let m = &mut max_partner[f as usize];
        *m = (*m).max(c);
    }
    (0..files.len())
        .map(|f| {
            let (d, s, m) = (defs[f] as f32, shared[f] as f32, max_partner[f] as f32);
            defs[f] >= COPY_MIN_DEFS && s >= COPY_MIN_SHARE * d && m <= COPY_MAX_PARTNER_SHARE * s
        })
        .collect()
}

/// Score multiplier per node (index = node id): the product of every prior
/// that applies, 1 otherwise. `bodiless` is the `symbols.bodiless`
/// component (empty for segments written before it existed).
pub(crate) fn priors(nodes: &[Node], files: &[FileEntry], bodiless: &RoaringBitmap) -> Vec<f32> {
    let copies = copy_files(nodes, files, bodiless);
    let vendored: Vec<bool> = files.iter().map(|f| is_vendored_path(&f.path)).collect();
    let tests: Vec<bool> = files.iter().map(|f| is_test_path(&f.path)).collect();
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut p = match n.label {
                Label::Field | Label::Variable | Label::Constant => DATA_PRIOR,
                _ => 1.0,
            };
            let file = n.file.map(|f| f as usize).filter(|&f| f < files.len());
            if n.is_test || n.label == Label::Test || file.is_some_and(|f| tests[f]) {
                p *= TEST_PRIOR;
            }
            if bodiless.contains(i as u32) {
                p *= BODILESS_PRIOR;
            }
            if file.is_some_and(|f| copies[f]) {
                p *= COPY_PRIOR;
            }
            if file.is_some_and(|f| vendored[f]) {
                p *= VENDORED_PRIOR;
            }
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_model::Span;

    #[test]
    fn test_paths() {
        for p in [
            "tests/x.py",
            "pkg/test_x.py",
            "pkg/x_test.py",
            "conftest.py",
            "src/__tests__/a.ts",
            "a.spec.ts",
            "b.test.jsx",
            "e2e/run.js",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in ["src/testing_utils.py", "src/latest.ts", "pkg/contest.py", "spec.md"] {
            assert!(!is_test_path(p), "{p}");
        }
    }

    #[test]
    fn vendored_paths() {
        for p in ["third_party/zlib/inflate.c", "src/deps/lua/lapi.c", "dependencies/jsoncpp/json.h"] {
            assert!(is_vendored_path(p), "{p}");
        }
        for p in ["deps.c", "src/dependencies.py", "src/vendoring/x.go", "lib/third_party_api.ts"] {
            assert!(!is_vendored_path(p), "{p}");
        }
    }

    fn node(label: Label, name: &str, lang: Language) -> Node {
        Node {
            label,
            name: name.into(),
            qualified_name: name.into(),
            file: Some(0),
            span: Some(Span::default()),
            language: Some(lang),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        }
    }

    #[test]
    fn bodiless_is_c_family_callables_without_braces() {
        let f = node(Label::Function, "f", Language::C);
        assert!(is_bodiless(&f, "int f(int x);"));
        assert!(is_bodiless(&f, "#define F(x) ((x) + 1)"));
        assert!(!is_bodiless(&f, "int f(int x) { return x; }"));
        let m = node(Label::Method, "draw", Language::Cpp);
        assert!(is_bodiless(&m, "virtual void draw() const = 0;"));
        assert!(is_bodiless(&m, "Widget() = default;"));
        // Other languages and labels never are.
        assert!(!is_bodiless(&node(Label::Method, "f", Language::Java), "abstract void f();"));
        assert!(!is_bodiless(&node(Label::Function, "f", Language::Python), "def f(): pass"));
        assert!(!is_bodiless(&node(Label::Class, "W", Language::Cpp), "class W;"));
    }

    fn file(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            blob_sha: String::new(),
            language: Some(Language::Cpp),
            size: 0,
            line_count: 0,
            node: 0,
            status: "ok".into(),
        }
    }

    /// `n` functions `f{k}` of 5 lines each in file `f`.
    fn defs(out: &mut Vec<Node>, f: u32, names: impl Iterator<Item = usize>) {
        for k in names {
            let mut n = node(Label::Function, &format!("f{k}"), Language::Cpp);
            n.file = Some(f);
            n.span = Some(Span { start_line: 10, end_line: 14, ..Span::default() });
            n.signature = Some(format!("void f{k}()"));
            out.push(n);
        }
    }

    #[test]
    fn amalgamation_is_a_copy_its_sources_are_not() {
        // Files 0..4 each define 20 functions; file 4 (the amalgamation)
        // copies all 80; file 5 and its twin file 6 share all 60 of theirs.
        let files: Vec<FileEntry> = ["a.h", "b.h", "c.h", "d.h", "single/all.h", "x.cc", "y.cc"].map(file).to_vec();
        let mut nodes = Vec::new();
        for f in 0..4 {
            defs(&mut nodes, f, f as usize * 20..f as usize * 20 + 20);
        }
        defs(&mut nodes, 4, 0..80);
        defs(&mut nodes, 5, 100..160);
        defs(&mut nodes, 6, 100..160);
        let copies = copy_files(&nodes, &files, &RoaringBitmap::new());
        assert_eq!(copies, [false, false, false, false, true, false, false]);
        let p = priors(&nodes, &files, &RoaringBitmap::new());
        assert_eq!(p[0], 1.0);
        assert_eq!(p[20 * 4], COPY_PRIOR);
        // Declarations do not count as copies of their bodies.
        let all: RoaringBitmap = (0..nodes.len() as u32).collect();
        assert!(copy_files(&nodes, &files, &all).iter().all(|c| !c));
    }

    #[test]
    fn priors_multiply() {
        let files = vec![file("third_party/x/tests/a.cc")];
        let mut n = node(Label::Function, "f", Language::Cpp);
        n.is_test = true;
        let c = node(Label::Constant, "K", Language::Cpp);
        let bodiless: RoaringBitmap = [0].into_iter().collect();
        let p = priors(&[n, c], &files, &bodiless);
        assert_eq!(p[0], TEST_PRIOR * BODILESS_PRIOR * VENDORED_PRIOR);
        assert_eq!(p[1], DATA_PRIOR * TEST_PRIOR * VENDORED_PRIOR);
    }
}
