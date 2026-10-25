//! Extraction tests. Module-name / import-resolution tests live in
//! `modules.rs`; text helpers in `text.rs`.

use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

fn facts(src: &str) -> FileFacts {
    PythonPack.extract("m.py", Language::Python, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'a>(f: &'a FileFacts, qn: &str) -> &'a Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no definition {qn}"))
}

fn idx(f: &FileFacts, qn: &str) -> u32 {
    f.definitions.iter().position(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no definition {qn}")) as u32
}

/// References as `(kind, receiver.name, enclosing qualified name)`.
fn refs(f: &FileFacts) -> Vec<(RefKind, String, String)> {
    f.references.iter().map(|r| (r.kind, ref_text(r), enc_name(f, r))).collect()
}

fn ref_text(r: &Reference) -> String {
    match &r.receiver {
        Some(recv) => format!("{recv}.{}", r.name),
        None => r.name.clone(),
    }
}

fn enc_name(f: &FileFacts, r: &Reference) -> String {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone()).unwrap_or_default()
}

fn has_ref(f: &FileFacts, kind: RefKind, text: &str, enclosing: &str) -> bool {
    refs(f).iter().any(|(k, t, e)| *k == kind && t == text && e == enclosing)
}

fn names_of_kind(f: &FileFacts, kind: RefKind) -> Vec<String> {
    f.references.iter().filter(|r| r.kind == kind).map(ref_text).collect()
}

const SERVICE: &str = include_str!("../tests/fixtures/service.py");

#[test]
fn service_definitions() {
    let f = facts(SERVICE);
    let summary: Vec<(Label, &str, Option<&str>)> = f
        .definitions
        .iter()
        .map(|d| {
            (d.label, d.qualified_name.as_str(), d.parent.map(|p| f.definitions[p as usize].qualified_name.as_str()))
        })
        .collect();
    use Label::*;
    assert_eq!(
        summary,
        vec![
            (Variable, "__all__", None),
            (Constant, "MAX_RETRIES", None),
            (Variable, "default_timeout", None),
            (Variable, "_cache", None),
            (Variable, "left", None),
            (Variable, "right", None),
            (Class, "InvoiceError", None),
            (Class, "InvoiceService", None),
            (Field, "InvoiceService.currency", Some("InvoiceService")),
            (Field, "InvoiceService.rate", Some("InvoiceService")),
            (Method, "InvoiceService.__init__", Some("InvoiceService")),
            (Field, "InvoiceService.repo", Some("InvoiceService")),
            (Field, "InvoiceService.timeout", Some("InvoiceService")),
            (Method, "InvoiceService.issue", Some("InvoiceService")),
            (Function, "InvoiceService.issue.inner", Some("InvoiceService.issue")),
            (Class, "InvoiceService.Nested", Some("InvoiceService")),
            (Method, "InvoiceService.Nested.m", Some("InvoiceService.Nested")),
            (Function, "issue_all", None),
            (Function, "_private", None),
            (Function, "test_issue", None),
            (Class, "TestService", None),
            (Method, "TestService.test_one", Some("TestService")),
            (Method, "TestService.testTwo", Some("TestService")),
            (Method, "TestService.helper", Some("TestService")),
        ]
    );
    assert_eq!(f.parse_errors, 0);
    assert_eq!(f.line_count, 88);
}

#[test]
fn service_definition_details() {
    let f = facts(SERVICE);
    let svc = def(&f, "InvoiceService");
    assert_eq!(svc.signature.as_deref(), Some("class InvoiceService(BaseService, models.Mixin, metaclass=Meta)"));
    assert_eq!(svc.doc.as_deref(), Some("Issues invoices.\n\nSecond paragraph."));
    assert_eq!(svc.decorators, vec!["dataclass(frozen=True)"]);
    assert_eq!(svc.bases, vec!["BaseService", "models.Mixin"]);
    assert!(svc.exported && !svc.is_test);
    // Span covers the decorator (line 25, 0-based 24); name span is the identifier.
    assert_eq!(svc.span.start_line, 24);
    assert_eq!(svc.name_span.start_line, 25);
    assert_eq!(&SERVICE[svc.name_span.start_byte as usize..svc.name_span.end_byte as usize], "InvoiceService");

    let issue = def(&f, "InvoiceService.issue");
    assert_eq!(
        issue.signature.as_deref(),
        Some("async def issue(self, invoice: Optional[models.Invoice], *args: list[Item]) -> Receipt")
    );
    assert_eq!(issue.decorators, vec!["app.route(\"/x\")", "staticmethod"]);
    assert_eq!(issue.span.start_line, 39);
    assert!(!issue.exported);

    let init = def(&f, "InvoiceService.__init__");
    assert_eq!(init.signature.as_deref(), Some("def __init__(self, repo: Repository, timeout=default_timeout)"));
    // `self.repo` assigned twice: one field, first occurrence.
    assert_eq!(f.definitions.iter().filter(|d| d.qualified_name == "InvoiceService.repo").count(), 1);
    assert_eq!(def(&f, "InvoiceService.repo").signature.as_deref(), Some("self.repo = repo"));
    assert_eq!(def(&f, "InvoiceService.currency").signature.as_deref(), Some("currency: str = \"MXN\""));

    // __all__ decides exports.
    assert!(def(&f, "MAX_RETRIES").exported);
    assert!(def(&f, "issue_all").exported);
    assert!(!def(&f, "default_timeout").exported);
    assert!(!def(&f, "InvoiceError").exported);
    assert!(!def(&f, "_private").exported);

    // Tests.
    assert!(def(&f, "test_issue").is_test);
    assert!(def(&f, "TestService").is_test);
    assert!(def(&f, "TestService.test_one").is_test);
    assert!(def(&f, "TestService.testTwo").is_test);
    assert!(!def(&f, "TestService.helper").is_test);
}

#[test]
fn service_references() {
    let f = facts(SERVICE);
    use RefKind::*;
    let expected: &[(RefKind, &str, &str)] = &[
        (Decorator, "dataclass", "InvoiceService"),
        (Type, "BaseService", "InvoiceService"),
        (Type, "models.Mixin", "InvoiceService"),
        (Read, "Meta", "InvoiceService"),
        (Read, "DEFAULT_RATE", "InvoiceService"),
        (Type, "Repository", "InvoiceService.__init__"),
        (Read, "default_timeout", "InvoiceService.__init__"),
        (Write, "self.repo", "InvoiceService.__init__"),
        (Write, "self.timeout", "InvoiceService.__init__"),
        (Decorator, "app.route", "InvoiceService.issue"),
        (Decorator, "staticmethod", "InvoiceService.issue"),
        (Type, "Optional", "InvoiceService.issue"),
        (Type, "models.Invoice", "InvoiceService.issue"),
        (Type, "Item", "InvoiceService.issue"),
        (Type, "Receipt", "InvoiceService.issue"),
        (Call, "compute", "InvoiceService.issue"),
        (Read, "os.path.sep", "InvoiceService.issue"),
        (Throw, "InvoiceError", "InvoiceService.issue"),
        (Call, "self.repo.save", "InvoiceService.issue"),
        (Read, "self.repo", "InvoiceService.issue"),
        (Write, "self.count", "InvoiceService.issue"),
        (Call, "print", "InvoiceService.issue"),
        (Read, "settings.DEBUG", "InvoiceService.issue"),
        (Call, "helper.wrap", "InvoiceService.issue"),
        (Call, "inner", "InvoiceService.issue"),
        (Read, "CONFIG", "InvoiceService.issue"),
        (Call, "InvoiceService", "issue_all"),
        (Call, "get_repo", "issue_all"),
        (Call, "service.issue", "issue_all"),
        (Call, "get_client().session.close", "issue_all"),
        (Call, "get_client", "issue_all"),
        (Throw, "ValueError", "issue_all"),
        (Call, "issue_all", "test_issue"),
    ];
    for (k, t, e) in expected {
        assert!(has_ref(&f, *k, t, e), "missing {k:?} {t} in {e}; got {:#?}", refs(&f));
    }
    // Locals, builtins, keyword names and closures produce no reads.
    let reads = names_of_kind(&f, Read);
    for absent in ["invoice", "total", "x", "n", "result", "items", "item", "service", "key", "timeout", "repo", "self"]
    {
        assert!(!reads.contains(&absent.to_string()), "unexpected read {absent}: {reads:?}");
    }
    // The raise constructor is a Throw, not a Call.
    assert!(!names_of_kind(&f, Call).contains(&"InvoiceError".to_string()));
    // Int is a builtin type.
    assert!(!names_of_kind(&f, Type).contains(&"int".to_string()));
    // Second `self.repo = None` write is deduplicated.
    assert_eq!(names_of_kind(&f, Write).iter().filter(|n| *n == "self.repo").count(), 1);
    // Module-level read of an imported name.
    assert!(has_ref(&f, Read, "TYPE_CHECKING", ""));
}

#[test]
fn service_imports() {
    let f = facts(SERVICE);
    type ImportRow = (String, u32, Vec<(String, Option<String>)>, Option<String>, bool);
    let got: Vec<ImportRow> = f
        .imports
        .iter()
        .map(|i| {
            (
                i.module.clone(),
                i.relative_level,
                i.names.iter().map(|n| (n.name.clone(), n.alias.clone())).collect(),
                i.module_alias.clone(),
                i.type_only,
            )
        })
        .collect();
    let n = |s: &str| (s.to_string(), None);
    assert_eq!(
        got,
        vec![
            ("__future__".into(), 0, vec![n("annotations")], None, false),
            ("os.path".into(), 0, vec![], None, false),
            ("numpy".into(), 0, vec![], Some("np".into()), false),
            ("typing".into(), 0, vec![n("TYPE_CHECKING"), n("Optional")], None, false),
            ("".into(), 1, vec![n("models")], None, false),
            ("core.base".into(), 2, vec![n("BaseService"), ("helper".into(), Some("h".into()))], None, false),
            ("utils".into(), 1, vec![n("*")], None, false),
            ("repo".into(), 1, vec![n("Repository")], None, true),
        ]
    );
    assert!(f.imports.iter().all(|i| !i.reexport));
    assert_eq!(f.imports[1].span.start_line, 3);
}

#[test]
fn multiple_imports_and_local_imports() {
    let f = facts(
        "import a.b as x, c\ndef f():\n    import json\n    from .. import y as z\n    if typing.TYPE_CHECKING:\n        import t\n",
    );
    let got: Vec<(&str, u32, Option<&str>, bool)> = f
        .imports
        .iter()
        .map(|i| (i.module.as_str(), i.relative_level, i.module_alias.as_deref(), i.type_only))
        .collect();
    assert_eq!(
        got,
        vec![
            ("a.b", 0, Some("x"), false),
            ("c", 0, None, false),
            ("json", 0, None, false),
            ("", 2, None, false),
            ("t", 0, None, true)
        ]
    );
    assert_eq!(f.imports[3].names[0].alias.as_deref(), Some("z"));
}

#[test]
fn nested_scopes_and_qualified_names() {
    let src = r#"
def outer(a):
    class Local:
        def m(self):
            return a
    def inner():
        def deepest():
            pass
    return Local

class A:
    class B:
        class C:
            def m(self): ...
"#;
    let f = facts(src);
    let qn: Vec<(&str, Label)> = f.definitions.iter().map(|d| (d.qualified_name.as_str(), d.label)).collect();
    assert_eq!(
        qn,
        vec![
            ("outer", Label::Function),
            ("outer.Local", Label::Class),
            ("outer.Local.m", Label::Method),
            ("outer.inner", Label::Function),
            ("outer.inner.deepest", Label::Function),
            ("A", Label::Class),
            ("A.B", Label::Class),
            ("A.B.C", Label::Class),
            ("A.B.C.m", Label::Method),
        ]
    );
    assert!(!def(&f, "outer.Local").exported);
    assert_eq!(def(&f, "outer.inner.deepest").parent, Some(idx(&f, "outer.inner")));
    // `a` in the method is a closure over outer's parameter: no read.
    assert!(names_of_kind(&f, RefKind::Read).is_empty(), "{:?}", refs(&f));
}

#[test]
fn local_binding_forms_suppress_reads() {
    let src = r#"
def f(p, *args, q: int = 1, **kw):
    a = 1
    b: int
    c, (d, *e) = g()
    for i, j in pairs:
        pass
    with open(path) as fh, lock:
        pass
    try:
        pass
    except (KeyError, MyErr) as err:
        pass
    if (w := compute()):
        pass
    xs = [k for k in ks if k]
    fn = lambda m, n=DEFAULT: m + n + outer_val
    global G
    G = 2
    return p, args, q, kw, a, b, c, d, e, i, j, fh, err, w, xs, fn, G, GLOBAL_X, len
"#;
    let f = facts(src);
    let mut reads = names_of_kind(&f, RefKind::Read);
    reads.sort();
    assert_eq!(reads, ["DEFAULT", "G", "GLOBAL_X", "MyErr", "ks", "lock", "outer_val", "pairs", "path"]);
}

#[test]
fn module_level_loop_targets_are_not_definitions() {
    let src = "for i in range(3):\n    print(i)\nX = [y for y in Z]\nwith ctx() as c:\n    use(c)\n";
    let f = facts(src);
    let names: Vec<&str> = f.definitions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["X"]);
    assert_eq!(def(&f, "X").label, Label::Constant);
    let reads = names_of_kind(&f, RefKind::Read);
    assert_eq!(reads, vec!["Z"]);
}

#[test]
fn class_body_names_are_local_to_class_body() {
    let src = "class C:\n    BASE = 1\n    URL = BASE + 1\n    def m(self):\n        return BASE\n";
    let f = facts(src);
    assert!(!has_ref(&f, RefKind::Read, "BASE", "C"));
    // Methods do not see class scope: a bare `BASE` there is a global read.
    assert!(has_ref(&f, RefKind::Read, "BASE", "C.m"));
    assert_eq!(def(&f, "C.URL").label, Label::Field);
}

#[test]
fn reads_are_deduplicated_per_enclosing() {
    let f =
        facts("def a():\n    x = CONST\n    y = CONST\n    return mod.attr, mod.attr\ndef b():\n    return CONST\n");
    let got = refs(&f);
    assert_eq!(got.iter().filter(|(_, t, _)| t == "CONST").count(), 2);
    assert_eq!(got.iter().filter(|(_, t, _)| t == "mod.attr").count(), 1);
}

#[test]
fn self_reads_and_writes() {
    let src = r#"
class K:
    def __init__(self, cfg):
        self.cfg = cfg
        self.a, self.b = 1, 2
        other.x = 3
    def run(self):
        self.calls += 1
        n = self.cfg.retries
        self.client.session.get(self.url)
        cls.registry = {}
"#;
    let f = facts(src);
    let fields: Vec<&str> = f.definitions.iter().filter(|d| d.label == Label::Field).map(|d| d.name.as_str()).collect();
    assert_eq!(fields, vec!["cfg", "a", "b"]);
    for (k, t, e) in [
        (RefKind::Write, "self.cfg", "K.__init__"),
        (RefKind::Write, "self.a", "K.__init__"),
        (RefKind::Write, "self.b", "K.__init__"),
        (RefKind::Read, "other", "K.__init__"),
        (RefKind::Write, "self.calls", "K.run"),
        (RefKind::Read, "self.cfg", "K.run"),
        (RefKind::Call, "self.client.session.get", "K.run"),
        (RefKind::Read, "self.client", "K.run"),
        (RefKind::Read, "self.url", "K.run"),
        (RefKind::Write, "cls.registry", "K.run"),
    ] {
        assert!(has_ref(&f, k, t, e), "missing {k:?} {t} in {e}: {:#?}", refs(&f));
    }
}

#[test]
fn class_fields_win_over_init_fields() {
    let f = facts("class D:\n    x: int = 0\n    def __init__(self):\n        self.x = 1\n        self.y = 2\n");
    let fields: Vec<(&str, u32)> = f
        .definitions
        .iter()
        .filter(|d| d.label == Label::Field)
        .map(|d| (d.name.as_str(), d.span.start_line))
        .collect();
    assert_eq!(fields, vec![("x", 1), ("y", 4)]);
}

#[test]
fn bases_skip_keywords_and_splats() {
    let f = facts("class A(Generic[T], abc.ABC, *mixins, metaclass=M, **kw):\n    pass\n");
    let a = def(&f, "A");
    assert_eq!(a.bases, vec!["Generic[T]", "abc.ABC"]);
    let types = names_of_kind(&f, RefKind::Type);
    assert_eq!(types, vec!["Generic", "T", "abc.ABC"]);
    assert!(f.references.iter().all(|r| r.enclosing == Some(0)));
    assert!(has_ref(&f, RefKind::Read, "M", "A"));
    assert!(has_ref(&f, RefKind::Read, "mixins", "A"));
}

#[test]
fn type_annotations() {
    let src = r#"
def f(a: "pkg.Foo", b: dict[str, Bar] | None, c: Callable[[Baz], "Qux"], d: t.Optional[m.Item] = None) -> tuple[int, ...]:
    local: Annotated[int, Field(gt=0)] = 1
X: Final[Thing] = Thing()
type Alias = list[Elem]
"#;
    let f = facts(src);
    let types: Vec<(String, String)> =
        f.references.iter().filter(|r| r.kind == RefKind::Type).map(|r| (ref_text(r), enc_name(&f, r))).collect();
    let t = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(
        types,
        vec![
            t("pkg.Foo", "f"),
            t("Bar", "f"),
            t("Callable", "f"),
            t("Baz", "f"),
            t("Qux", "f"),
            t("t.Optional", "f"),
            t("m.Item", "f"),
            t("Annotated", "f"),
            t("Final", "X"),
            t("Thing", "X"),
            t("Elem", "Alias"),
        ]
    );
    assert!(has_ref(&f, RefKind::Call, "Field", "f"));
    assert!(has_ref(&f, RefKind::Call, "Thing", ""));
    let alias = def(&f, "Alias");
    assert_eq!(alias.label, Label::TypeAlias);
    assert!(alias.exported);
}

#[test]
fn decorators_and_calls() {
    let src = r#"
@pytest.mark.parametrize("x", [VALUES])
@functools.lru_cache
@register
def test_it(x):
    pass

@(lambda f: f)
def odd():
    pass
"#;
    let f = facts(src);
    let t = def(&f, "test_it");
    assert_eq!(t.decorators, vec!["pytest.mark.parametrize(\"x\", [VALUES])", "functools.lru_cache", "register"]);
    assert!(t.is_test);
    assert_eq!(t.span.start_line, 1);
    for (k, n) in [
        (RefKind::Decorator, "pytest.mark.parametrize"),
        (RefKind::Decorator, "functools.lru_cache"),
        (RefKind::Decorator, "register"),
        (RefKind::Read, "VALUES"),
    ] {
        assert!(has_ref(&f, k, n, "test_it"), "missing {n}: {:?}", refs(&f));
    }
    assert_eq!(def(&f, "odd").decorators, vec!["(lambda f: f)"]);
}

#[test]
fn call_receivers() {
    let src = "def f():\n    get().x()\n    a[0].m()\n    (\n        session\n        .query(A)  # comment\n        .all()\n    )\n    super().__init__()\n";
    let f = facts(src);
    for (k, t) in [
        (RefKind::Call, "get().x"),
        (RefKind::Call, "get"),
        (RefKind::Call, "a[0].m"),
        (RefKind::Read, "a"),
        (RefKind::Call, "session.query(A).all"),
        (RefKind::Call, "session.query"),
        (RefKind::Read, "A"),
        (RefKind::Call, "super().__init__"),
        (RefKind::Call, "super"),
    ] {
        assert!(has_ref(&f, k, t, "f"), "missing {t}: {:#?}", refs(&f));
    }
}

#[test]
fn no_refs_for_keywords_strings_comments() {
    let f =
        facts("def f():\n    g(key=value, other=1)  # note: hidden()\n    s = 'call() inside'\n    t = f'{shown()}'\n");
    let calls = names_of_kind(&f, RefKind::Call);
    assert_eq!(calls, vec!["g", "shown"]);
    assert_eq!(names_of_kind(&f, RefKind::Read), vec!["value"]);
}

#[test]
fn raise_forms() {
    let src = "def f():\n    try:\n        pass\n    except E as e:\n        raise\n        raise e\n        raise errors.NotFound('x') from e\n        raise Custom\n";
    let f = facts(src);
    assert_eq!(names_of_kind(&f, RefKind::Throw), vec!["errors.NotFound", "Custom"]);
}

#[test]
fn docstrings() {
    let src = r#"
def a():
    '''Single quotes.'''

def b():
    # comment first
    r"""
    Raw docstring.

        indented
    """

def c():
    x = 1
    """not a doc"""

def d():
    f"""not a doc {x}"""

def e(): "inline"
"#;
    let f = facts(src);
    assert_eq!(def(&f, "a").doc.as_deref(), Some("Single quotes."));
    assert_eq!(def(&f, "b").doc.as_deref(), Some("Raw docstring.\n\n    indented"));
    assert_eq!(def(&f, "c").doc, None);
    assert_eq!(def(&f, "d").doc, None);
    assert_eq!(def(&f, "e").doc.as_deref(), Some("inline"));
    assert_eq!(def(&f, "e").signature.as_deref(), Some("def e()"));
    let long = format!("def big():\n    \"\"\"{}\"\"\"\n", "x".repeat(5000));
    let doc = facts(&long).definitions[0].doc.clone().unwrap();
    assert_eq!(doc.chars().count(), 1001);
}

#[test]
fn signatures_collapse_and_truncate() {
    let src = "def request(\n    self,\n    method,  # verb\n    url,\n    params=None,\n) -> Response:\n    pass\n";
    let f = facts(src);
    assert_eq!(
        def(&f, "request").signature.as_deref(),
        Some("def request(self, method, url, params=None) -> Response")
    );
    let params: Vec<String> = (0..100).map(|i| format!("p{i}")).collect();
    let long = format!("def f({}):\n    pass\n", params.join(", "));
    let sig = facts(&long).definitions[0].signature.clone().unwrap();
    assert_eq!(sig.chars().count(), 201);
    assert!(sig.ends_with('…'));
}

#[test]
fn exported_without_all() {
    let f =
        facts("def pub(): pass\ndef _priv(): pass\nclass K:\n    def m(self): pass\nif True:\n    def cond(): pass\n");
    assert!(def(&f, "pub").exported);
    assert!(!def(&f, "_priv").exported);
    assert!(!def(&f, "K.m").exported);
    assert!(def(&f, "cond").exported);
}

#[test]
fn all_with_augmented_and_non_literal() {
    let f = facts("__all__ = ['a']\n__all__ += ('b',)\ndef a(): pass\ndef b(): pass\ndef c(): pass\n");
    assert!(def(&f, "a").exported && def(&f, "b").exported && !def(&f, "c").exported);
    // Non-literal __all__ is ignored: fall back to the underscore rule.
    let f = facts("__all__ = base.__all__ + ['a']\ndef a(): pass\ndef c(): pass\n");
    assert!(def(&f, "a").exported && def(&f, "c").exported);
}

#[test]
fn variables_first_wins_and_no_shadowing_of_defs() {
    let f = facts("x = 1\nx = 2\ndef g(): pass\ng = wrap(g)\nA = B = 3\n");
    let names: Vec<(&str, Label)> = f.definitions.iter().map(|d| (d.name.as_str(), d.label)).collect();
    assert_eq!(
        names,
        vec![("x", Label::Variable), ("g", Label::Function), ("A", Label::Constant), ("B", Label::Constant)]
    );
    assert_eq!(def(&f, "x").span.start_line, 0);
}

#[test]
fn test_detection_rules() {
    let f = facts(
        "def test(): pass\ndef testing(): pass\nclass Tests: pass\nclass Foo:\n    def test_x(self): pass\n    def testy(self): pass\n",
    );
    assert!(def(&f, "test").is_test);
    assert!(!def(&f, "testing").is_test);
    assert!(def(&f, "Tests").is_test);
    assert!(def(&f, "Foo.test_x").is_test);
    assert!(!def(&f, "Foo.testy").is_test);
}

#[test]
fn parse_errors_are_counted_and_partial_facts_kept() {
    let f = facts("def ok():\n    pass\n\ndef broken(:\n    pass\n\nclass After:\n    pass\n");
    assert!(f.parse_errors > 0);
    assert!(f.definitions.iter().any(|d| d.name == "ok"));
}

#[test]
fn deterministic() {
    let a = facts(SERVICE);
    let b = facts(SERVICE);
    assert_eq!(a, b);
    let ja = serde_json_like(&a);
    assert_eq!(ja, serde_json_like(&b));
}

fn serde_json_like(f: &FileFacts) -> String {
    format!("{f:?}")
}

#[test]
fn reference_cap() {
    let mut src = String::from("def f():\n");
    for i in 0..25_000 {
        src.push_str(&format!("    c{i}()\n"));
    }
    let budget = Budget { max_bytes: 10 * 1024 * 1024, ..Budget::default() };
    let f = PythonPack.extract("m.py", Language::Python, src.as_bytes(), &budget).unwrap();
    assert_eq!(f.references.len(), MAX_REFS);
}

#[test]
fn depth_budget_prevents_deep_recursion() {
    // Deeply nested expression; must not overflow the stack.
    let n = 5000;
    let src = format!("x = {}1{}\n", "(".repeat(n), ")".repeat(n));
    let budget = Budget { max_depth: 64, ..Budget::default() };
    let f = PythonPack.extract("m.py", Language::Python, src.as_bytes(), &budget).unwrap();
    assert_eq!(f.definitions.len(), 1);
    let src = format!("def f():\n    return {}a{}\n", "[".repeat(n), "]".repeat(n));
    let f = PythonPack.extract("m.py", Language::Python, src.as_bytes(), &Budget::default()).unwrap();
    assert_eq!(f.definitions.len(), 1);
}

#[test]
fn errors_for_budget_and_language() {
    let budget = Budget { max_bytes: 4, ..Budget::default() };
    assert!(matches!(
        PythonPack.extract("m.py", Language::Python, b"x = 1\n", &budget),
        Err(ExtractError::TooLarge { .. })
    ));
    assert!(matches!(
        PythonPack.extract("m.ts", Language::TypeScript, b"", &Budget::default()),
        Err(ExtractError::Unsupported(Language::TypeScript))
    ));
    let empty = facts("");
    assert_eq!(empty, FileFacts::default());
}

/// Small deterministic xorshift PRNG (no external dependency).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let tokens: &[&[u8]] = &[
        b"def ",
        b"class ",
        b"(",
        b")",
        b":",
        b"\n",
        b"    ",
        b"self.",
        b"x",
        b"=",
        b"@",
        b"\"\"\"",
        b"'",
        b"import ",
        b"from ",
        b".",
        b"raise ",
        b"lambda ",
        b"[",
        b"]",
        b"{",
        b"}",
        b",",
        b"*",
        b"#",
        b"f\"{",
        b"\\",
        b"\xff\xfe",
        b"\xc3",
        b"async ",
        b"if TYPE_CHECKING:",
        b"global ",
        b"->",
        b"type ",
        b"match x:\n case ",
    ];
    for round in 0..400 {
        let len = (rng.next() % 600) as usize;
        let mut src = Vec::with_capacity(len * 4);
        for _ in 0..len {
            if round % 2 == 0 {
                src.push(rng.next() as u8);
            } else {
                src.extend_from_slice(tokens[(rng.next() as usize) % tokens.len()]);
            }
        }
        let a = PythonPack.extract("r.py", Language::Python, &src, &Budget::default()).unwrap();
        let b = PythonPack.extract("r.py", Language::Python, &src, &Budget::default()).unwrap();
        assert_eq!(a, b);
        for d in &a.definitions {
            assert!(d.span.contains(&d.name_span) || d.label == Label::Field);
            if let Some(p) = d.parent {
                assert!((p as usize) < a.definitions.len());
            }
        }
        for r in &a.references {
            if let Some(e) = r.enclosing {
                assert!((e as usize) < a.definitions.len());
            }
        }
    }
}

#[test]
fn builtins() {
    let p = PythonPack;
    for b in [
        "print",
        "len",
        "isinstance",
        "ValueError",
        "None",
        "True",
        "object",
        "super",
        "type",
        "dict",
        "list",
        "__name__",
        "__file__",
        "PythonFinalizationError",
    ] {
        assert!(p.is_builtin(b), "{b}");
    }
    for n in ["self", "os", "Foo", "printf", ""] {
        assert!(!p.is_builtin(n), "{n}");
    }
    assert!(builtins::BUILTINS.windows(2).all(|w| w[0] < w[1]), "builtins must be sorted");
    assert_eq!(p.self_receivers(), &["self", "cls"]);
    assert_eq!(p.extractor_version(), 2);
}

// ---------------------------------------------------------------------------
// Locals (queries/locals.scm) driving `Read` filtering
// ---------------------------------------------------------------------------

fn sorted_reads(src: &str) -> Vec<String> {
    let mut reads = names_of_kind(&facts(src), RefKind::Read);
    reads.sort();
    reads
}

#[test]
fn locals_comprehensions_and_generators() {
    let src = r#"
def f(rows):
    a = [r.id for r in rows if r.ok]
    b = {k: v for k, v in PAIRS}
    c = {s for s in SEEN}
    d = sum(x * FACTOR for x in rows)
    e = [[i * j for j in COLS] for i in ROWS]
    return a, b, c, d, e
"#;
    assert_eq!(sorted_reads(src), ["COLS", "FACTOR", "PAIRS", "ROWS", "SEEN"]);
    // Comprehension variables do not leak into the function scope.
    let leak = "def g():\n    [n for n in NS]\n    return n\n";
    assert_eq!(sorted_reads(leak), ["NS", "n"]);
}

#[test]
fn locals_lambdas_with_except_walrus() {
    let src = r#"
def f():
    key = lambda item, default=FALLBACK: item.score or default or BONUS
    with lock_for(RESOURCE) as (guard, token), Session() as session:
        session.use(guard, token)
    try:
        run()
    except Exception as exc:
        log(exc, LOGGER)
    if any((hit := probe(v)) for v in VALUES):
        return hit, key
"#;
    assert_eq!(sorted_reads(src), ["BONUS", "FALLBACK", "LOGGER", "RESOURCE", "VALUES"]);
}

#[test]
fn locals_match_case_captures() {
    let src = r#"
def handle(cmd):
    match cmd:
        case Move(x=dx, y=dy) as move:
            return dx, dy, move, ORIGIN
        case [first, *rest]:
            return first, rest
        case {"k": val, **others}:
            return val, others
        case Color.RED:
            return RED_VALUE
"#;
    assert_eq!(sorted_reads(src), ["ORIGIN", "RED_VALUE"]);
}

#[test]
fn locals_global_nonlocal_and_closures() {
    let src = r#"
COUNTER = 0
def outer():
    total = 0
    def bump():
        global COUNTER
        nonlocal total
        COUNTER += 1
        total += STEP
        return total
    return bump
"#;
    let f = facts(src);
    assert!(has_ref(&f, RefKind::Read, "COUNTER", "outer.bump"));
    assert!(has_ref(&f, RefKind::Read, "STEP", "outer.bump"));
    assert!(!names_of_kind(&f, RefKind::Read).contains(&"total".to_string()));
}

#[test]
fn locals_header_defaults_use_enclosing_scope() {
    // `limit=limit` reads the module-level name, not the parameter.
    let src = "limit = 10\ndef page(items, limit=limit):\n    return items[:limit]\n";
    let f = facts(src);
    assert!(has_ref(&f, RefKind::Read, "limit", "page"));
    assert_eq!(names_of_kind(&f, RefKind::Read), ["limit"]);
}

#[test]
fn locals_pep695_type_parameters_are_not_type_refs() {
    let src = "class Box[T](Generic):\n    def get(self) -> T: ...\ndef first[U](xs: list[U]) -> U | Other: ...\n";
    let f = facts(src);
    let types = names_of_kind(&f, RefKind::Type);
    assert!(!types.contains(&"T".to_string()) && !types.contains(&"U".to_string()), "{types:?}");
    assert!(types.contains(&"Other".to_string()) && types.contains(&"Generic".to_string()), "{types:?}");
}

#[test]
fn locals_imports_inside_functions_still_read() {
    // Function-level imports bind names, but reads of them are reported so
    // the resolver can follow the import.
    let src = "def f():\n    import json\n    from os import path\n    return json.dumps, path.sep\n";
    let reads = sorted_reads(src);
    assert_eq!(reads, ["json.dumps", "path.sep"]);
}

#[test]
fn deeply_nested_scopes_stay_fast() {
    // Every level reads a free name: resolution walks are capped and lazy.
    let n = 3000;
    let mut src = String::from("f = ");
    for _ in 0..n {
        src.push_str("lambda: (free, ");
    }
    src.push('0');
    for _ in 0..n {
        src.push(')');
    }
    src.push('\n');
    let start = std::time::Instant::now();
    let f = facts(&src);
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(f.definitions.len(), 1);
    assert!(has_ref(&f, RefKind::Read, "free", ""));
}

// ---------------------------------------------------------------------------
// Env var reads (the code ↔ config bridge, RFC 0003 §7.2)
// ---------------------------------------------------------------------------

#[test]
fn env_var_reads() {
    let f = facts(
        "import os
import os as o
from os import environ as env, getenv

URL = os.getenv(\"DATABASE_URL\")


def main(name):
    a = os.environ[\"LOG_LEVEL\"]
    b = os.environ.get(\"API_TOKEN\", \"x\")
    c = o.getenv('QUEUE')
    d = env[\"SENTRY_DSN\"]
    e = env.get(\"REGION\")
    g = getenv(\"HOME\")
    os.environ[\"SET_ONLY\"] = \"1\"
    del os.environ[\"GONE\"]
    dynamic = os.getenv(name)
    fstr = os.getenv(f\"X_{name}\")
    other = config.get(\"NOT_ENV\")
    return a, b, c, d, e, g, dynamic, fstr, other
",
    );
    let mut env: Vec<(String, String)> =
        f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| (r.name.clone(), enc_name(&f, r))).collect();
    env.sort();
    let expect: Vec<(String, String)> = [
        ("API_TOKEN", "main"),
        ("DATABASE_URL", ""),
        ("HOME", "main"),
        ("LOG_LEVEL", "main"),
        ("QUEUE", "main"),
        ("REGION", "main"),
        ("SENTRY_DSN", "main"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(env, expect);
    // The ordinary call facts are unchanged.
    assert!(has_ref(&f, RefKind::Call, "os.getenv", ""));

    // Without `import os` there is no evidence: nothing is emitted.
    let f = facts("def f():\n    return os.getenv(\"X\"), environ[\"Y\"], getenv(\"Z\")\n");
    assert!(f.references.iter().all(|r| r.kind != RefKind::EnvVar));
}
