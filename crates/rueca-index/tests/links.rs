//! The organization link layer, packages (RFC 0006 §3.3.1) on real working
//! trees: a library and a consumer per language, cross-repository `IMPORTS`
//! and ladder edges resolved with the library's own module model, and the
//! reverse-dependency recompute when a library is re-indexed.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use rueca_index::org::{OrgConfig, OrgIndexOptions, index_org};
use rueca_segment::Store;
use rueca_segment::org::{LinkDir, OrgIndex};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// (repo, qualified name) → (repo, qualified name, kind, strategy) of every
/// link edge of the current generation.
fn link_edges(store: &Path) -> BTreeSet<(String, String, String, String, String, String)> {
    let idx = OrgIndex::open(store, "acme").unwrap();
    let st = Store::open(store).unwrap();
    let g = idx.generation();
    let segs: Vec<_> = g.repos.iter().map(|r| st.open_segment(&r.segment_id).unwrap()).collect();
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut out = BTreeSet::new();
    for (i, seg) in segs.iter().enumerate() {
        for n in 0..seg.nodes().len() as u32 {
            for e in idx.link_edges(i, n, LinkDir::Out, None, &all).unwrap() {
                out.insert((
                    g.repos[i].repo.clone(),
                    seg.node(n).qualified_name.clone(),
                    g.repos[e.repo].repo.clone(),
                    segs[e.repo].node(e.node).qualified_name.clone(),
                    e.kind.as_str().to_string(),
                    e.meta.strategy.as_str().to_string(),
                ));
            }
        }
    }
    out
}

fn has(edges: &BTreeSet<(String, String, String, String, String, String)>, src: &str, dst: &str, kind: &str) -> bool {
    edges.iter().any(|e| e.1 == src && e.3 == dst && e.4 == kind)
}

fn fixture(base: &Path) {
    // Python: a library published as `acmelib` (src layout) and a consumer.
    let lib = base.join("pylib");
    write(&lib, "pyproject.toml", "[project]\nname = \"acmelib\"\ndescription = \"Payment gateway client\"\n");
    write(&lib, "src/acmelib/__init__.py", "from .core import charge, Gateway\n");
    write(
        &lib,
        "src/acmelib/core.py",
        "class Gateway:\n    def send(self, payload):\n        return payload\n\ndef charge(amount):\n    return Gateway().send(amount)\n",
    );
    write(&lib, "tests/test_core.py", "from acmelib import charge\n\ndef test_charge():\n    assert charge(1)\n");
    let app = base.join("pyapp");
    write(&app, "pyproject.toml", "[project]\nname = \"shop\"\ndependencies = [\"acmelib>=1\"]\n");
    write(
        &app,
        "shop/checkout.py",
        "import acmelib\nfrom acmelib.core import Gateway\n\nclass RetryGateway(Gateway):\n    pass\n\ndef pay(order):\n    return acmelib.charge(order.total)\n",
    );

    // TypeScript: a workspace package and a consumer importing it by name.
    let ui = base.join("ui");
    write(&ui, "package.json", r#"{"name": "@acme/ui", "main": "dist/index.js", "description": "Button widgets"}"#);
    write(&ui, "src/index.ts", "export function renderButton(label: string): string {\n  return label;\n}\n");
    let web = base.join("web");
    write(&web, "package.json", r#"{"name": "web", "dependencies": {"@acme/ui": "1.0.0"}}"#);
    write(
        &web,
        "src/page.ts",
        "import { renderButton } from \"@acme/ui\";\n\nexport function page(): string {\n  return renderButton(\"ok\");\n}\n",
    );

    // Go: a module and a consumer importing its root package.
    let kit = base.join("kit");
    write(&kit, "go.mod", "module github.com/acme/kit\n\ngo 1.22\n");
    write(&kit, "kit.go", "package kit\n\nfunc Retry(n int) int {\n\treturn n\n}\n");
    let svc = base.join("svc");
    write(&svc, "go.mod", "module github.com/acme/svc\n\ngo 1.22\n\nrequire github.com/acme/kit v1.0.0\n");
    write(
        &svc,
        "main.go",
        "package main\n\nimport \"github.com/acme/kit\"\n\nfunc run() int {\n\treturn kit.Retry(3)\n}\n",
    );

    // PHP: a Composer package (PSR-4) and a consumer requiring it.
    let money = base.join("money");
    write(&money, "composer.json", r#"{"name": "acme/money", "autoload": {"psr-4": {"Acme\\Money\\": "src/"}}}"#);
    write(
        &money,
        "src/Money.php",
        "<?php\nnamespace Acme\\Money;\n\nclass Money\n{\n    public static function of(int $n): Money\n    {\n        return new Money();\n    }\n}\n",
    );
    let pay = base.join("pay");
    write(&pay, "composer.json", r#"{"name": "acme/pay", "require": {"acme/money": "^1"}}"#);
    write(
        &pay,
        "src/Checkout.php",
        "<?php\nnamespace Shop;\n\nuse Acme\\Money\\Money;\n\nclass Checkout\n{\n    public function total(): Money\n    {\n        return Money::of(3);\n    }\n}\n",
    );

    // Java: a Maven artifact and a consumer importing its class.
    let util = base.join("jutil");
    write(
        &util,
        "pom.xml",
        "<project><groupId>com.acme</groupId><artifactId>util</artifactId><description>String helpers</description></project>",
    );
    write(
        &util,
        "src/main/java/com/acme/util/Strings.java",
        "package com.acme.util;\n\npublic class Strings {\n    public static String shout(String s) {\n        return s;\n    }\n}\n",
    );
    let japp = base.join("japp");
    write(
        &japp,
        "pom.xml",
        "<project><groupId>com.acme</groupId><artifactId>app</artifactId><dependencies><dependency><groupId>com.acme</groupId><artifactId>util</artifactId></dependency></dependencies></project>",
    );
    write(
        &japp,
        "src/main/java/com/acme/app/Main.java",
        "package com.acme.app;\n\nimport com.acme.util.Strings;\n\npublic class Main {\n    public String run() {\n        return Strings.shout(\"x\");\n    }\n}\n",
    );

    // Unrelated repository.
    write(&base.join("other"), "other/misc.py", "def unrelated():\n    return 0\n");

    let mut org = String::from("name = \"acme\"\n");
    for r in ["pylib", "pyapp", "ui", "web", "kit", "svc", "money", "pay", "jutil", "japp", "other"] {
        org.push_str(&format!("[[repo]]\npath = \"{r}\"\n"));
    }
    fs::write(base.join("rueca-org.toml"), org).unwrap();
}

#[test]
fn packages_resolve_across_repositories() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    fixture(&base);
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&base.join("rueca-org.toml")).unwrap(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    };
    let r = index_org(&opts).unwrap();
    assert!(r.update.link_errors.is_empty(), "{:?}", r.update.link_errors);
    let linked: BTreeSet<&str> = r.update.linked.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(linked, BTreeSet::from(["local/japp", "local/pay", "local/pyapp", "local/svc", "local/web"]));

    // Packages and dependencies are in the generation.
    let idx = OrgIndex::open(&store, "acme").unwrap();
    let g = idx.generation();
    let i = g.repo_index("local/pylib").unwrap();
    let p = idx.profile(i).unwrap();
    assert_eq!(p.deps.packages[0].keys, ["py:acmelib"]);
    assert!(p.about.contains("Payment gateway client"), "{}", p.about);
    let app = &g.repos[g.repo_index("local/pyapp").unwrap()];
    assert_eq!(app.links.as_ref().unwrap().deps[0].repo, "local/pylib");
    assert!(g.repos[g.repo_index("local/other").unwrap()].links.is_none());

    let edges = link_edges(&store);
    // Python: module imports, a call through the package's re-export, a base class.
    assert!(has(&edges, "shop/checkout.py", "src/acmelib/__init__.py", "IMPORTS"), "{edges:#?}");
    assert!(has(&edges, "shop/checkout.py", "src/acmelib/core.py", "IMPORTS"), "{edges:#?}");
    assert!(has(&edges, "shop.checkout.pay", "acmelib.core.charge", "CALLS"), "{edges:#?}");
    assert!(has(&edges, "shop.checkout.RetryGateway", "acmelib.core.Gateway", "INHERITS"), "{edges:#?}");
    // TypeScript: the bare specifier through the library's package entry.
    assert!(has(&edges, "src/page.ts", "src/index.ts", "IMPORTS"), "{edges:#?}");
    assert!(has(&edges, "src/page:page", "src/index:renderButton", "CALLS"), "{edges:#?}");
    // Go: the module path, the root package.
    assert!(has(&edges, "github.com/acme/svc.run", "github.com/acme/kit.Retry", "CALLS"), "{edges:#?}");
    // PHP: Composer autoload of the library.
    assert!(has(&edges, "src/Checkout.php", "src/Money.php", "IMPORTS"), "{edges:#?}");
    assert!(has(&edges, "Shop\\Checkout::total", "Acme\\Money\\Money::of", "CALLS"), "{edges:#?}");
    // Java: the class import.
    assert!(has(&edges, "com.acme.app.Main.run", "com.acme.util.Strings.shout", "CALLS"), "{edges:#?}");
    // Ladder strategies only, never name matching.
    assert!(edges.iter().all(|e| e.5 != "unique_name" && e.5 != "heuristic"), "{edges:#?}");
    // Edges leave consumers only.
    assert!(
        edges
            .iter()
            .all(|e| ["local/pyapp", "local/web", "local/svc", "local/pay", "local/japp"].contains(&e.0.as_str()))
    );

    // Re-indexing the library recomputes its reverse dependencies only.
    write(
        &base.join("pylib"),
        "src/acmelib/core.py",
        "class Gateway:\n    def send(self, payload):\n        return payload\n\ndef charge(amount, currency='EUR'):\n    return Gateway().send(amount)\n\ndef refund(amount):\n    return amount\n",
    );
    let r = index_org(&OrgIndexOptions { only: Some(base.join("pylib")), ..opts.clone() }).unwrap();
    let linked: Vec<&str> = r.update.linked.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(linked, ["local/pyapp"], "{:?}", r.update);
    assert_eq!(r.update.profiled, ["local/pylib"]);
    assert!(has(&link_edges(&store), "shop.checkout.pay", "acmelib.core.charge", "CALLS"));
    // Re-indexing an unrelated repository links nothing.
    write(&base.join("other"), "other/misc.py", "def unrelated():\n    return 1\n");
    let r = index_org(&OrgIndexOptions { only: Some(base.join("other")), ..opts.clone() }).unwrap();
    assert!(r.update.linked.is_empty(), "{:?}", r.update.linked);
    // A consumer that drops its import loses its links.
    write(&base.join("svc"), "main.go", "package main\n\nfunc run() int {\n\treturn 3\n}\n");
    let r = index_org(&OrgIndexOptions { only: Some(base.join("svc")), ..opts }).unwrap();
    assert!(r.update.linked.is_empty() || r.update.linked.iter().all(|(_, n)| *n == 0), "{:?}", r.update.linked);
    assert!(!link_edges(&store).iter().any(|e| e.0 == "local/svc"));
}
