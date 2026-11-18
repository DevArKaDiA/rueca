//! The organization link layer, packages (RFC 0006 §3.3.1) on real working
//! trees: a library and a consumer per language, cross-repository `IMPORTS`
//! and ladder edges resolved with the library's own module model, and the
//! reverse-dependency recompute when a library is re-indexed.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use rueca_index::org::{OrgConfig, OrgIndexOptions, index_org};
use rueca_segment::Store;
use rueca_segment::org::{LinkDir, OrgIndex, OrgSelection, OrgSlot};

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
    let segs: Vec<_> = g.repos.iter().map(|r| st.open_segment(&r.default.segment_id).unwrap()).collect();
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut out = BTreeSet::new();
    for (i, seg) in segs.iter().enumerate() {
        for n in 0..seg.nodes().len() as u32 {
            for e in
                idx.link_edges(OrgSlot::default_of(i), n, LinkDir::Out, None, &all, &OrgSelection::default()).unwrap()
            {
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
    assert_eq!(app.default.links.as_ref().unwrap().deps[0].repo, "local/pylib");
    assert!(g.repos[g.repo_index("local/other").unwrap()].default.links.is_none());

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

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commit_all(dir: &Path, msg: &str) {
    assert!(git(dir, &["add", "-A"]));
    assert!(git(dir, &["commit", "-q", "-m", msg]));
}

/// `root` as a git repository on `main` holding `files`, with `branch`
/// writing `changes` on top; `main` checked out. `None` without git.
fn git_repo(root: &Path, files: &[(&str, &str)], branch: &str, changes: &[(&str, &str)]) -> Option<()> {
    fs::create_dir_all(root).unwrap();
    if !git(root, &["init", "-q"]) {
        eprintln!("git not available; skipping");
        return None;
    }
    assert!(git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    for (p, t) in files {
        write(root, p, t);
    }
    commit_all(root, "init");
    assert!(git(root, &["checkout", "-q", "-b", branch]));
    for (p, t) in changes {
        write(root, p, t);
    }
    commit_all(root, branch);
    assert!(git(root, &["checkout", "-q", "main"]));
    Some(())
}

/// (source qn, target entry `repo@ref`, target qn, kind) of the outgoing
/// link edges of entry `at` seen from `view`.
fn edges_at(
    idx: &OrgIndex,
    st: &Store,
    at: OrgSlot,
    view: &OrgSelection,
) -> BTreeSet<(String, String, String, String)> {
    let g = idx.generation();
    let open = |s: OrgSlot| st.open_segment(&g.entry(s).unwrap().segment_id).unwrap();
    let seg = open(at);
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut out = BTreeSet::new();
    for n in 0..seg.nodes().len() as u32 {
        for e in idx.link_edges(at, n, LinkDir::Out, None, &all, view).unwrap() {
            let dst = open(e.slot);
            out.insert((
                seg.node(n).qualified_name.clone(),
                format!("{}@{}", g.repos[e.repo].repo, g.ref_name(e.slot).unwrap()),
                dst.node(e.node).qualified_name.clone(),
                e.kind.as_str().to_string(),
            ));
        }
    }
    out
}

fn calls(edges: &BTreeSet<(String, String, String, String)>, src: &str, dst: &str) -> Option<String> {
    edges.iter().find(|e| e.0 == src && e.2 == dst && e.3 == "CALLS").map(|e| e.1.clone())
}

/// RFC 0006 §9.3 on real packages: a consumer branch's new cross-repository
/// call is in that branch's link set only; a library branch that renames
/// the called function drops the consumer's edge in the branch's view and
/// keeps it at the default.
#[test]
fn ref_link_sets_and_remapped_edges() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    let core = "class Gateway:\n    def send(self, payload):\n        return payload\n\ndef charge(amount):\n    return Gateway().send(amount)\n\ndef refund(amount):\n    return amount\n";
    let lib = [
        ("pyproject.toml", "[project]\nname = \"acmelib\"\n"),
        ("src/acmelib/__init__.py", "from .core import charge, refund\n"),
        ("src/acmelib/core.py", core),
    ];
    let renamed = [
        ("src/acmelib/__init__.py", "from .core import charge_amount, refund\n"),
        ("src/acmelib/core.py", &core.replace("def charge(", "def charge_amount(")[..]),
    ];
    if git_repo(&base.join("pylib"), &lib, "rename", &renamed).is_none() {
        return;
    }
    let checkout = "import acmelib\n\ndef pay(order):\n    return acmelib.charge(order.total)\n";
    let app = [
        ("pyproject.toml", "[project]\nname = \"shop\"\ndependencies = [\"acmelib>=1\"]\n"),
        ("shop/checkout.py", checkout),
    ];
    let with_refund = format!("{checkout}\ndef give_back(order):\n    return acmelib.refund(order.total)\n");
    git_repo(&base.join("pyapp"), &app, "feat/refund", &[("shop/checkout.py", &with_refund)]).unwrap();
    fs::write(
        base.join("rueca-org.toml"),
        "name = \"acme\"\n[[repo]]\npath = \"pylib\"\nrefs = [\"rename\"]\n[[repo]]\npath = \"pyapp\"\nrefs = [\"feat/refund\"]\n",
    )
    .unwrap();
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&base.join("rueca-org.toml")).unwrap(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    };
    let r = index_org(&opts).unwrap();
    assert!(r.ref_errors.is_empty() && r.update.link_errors.is_empty(), "{:?}", r.update);
    let linked: BTreeSet<&str> = r.update.linked.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(linked, BTreeSet::from(["local/pyapp", "local/pyapp@feat/refund"]));

    let idx = OrgIndex::open(&store, "acme").unwrap();
    let st = Store::open(&store).unwrap();
    let g = idx.generation();
    let (lib_i, app_i) = (g.repo_index("local/pylib").unwrap(), g.repo_index("local/pyapp").unwrap());
    let app_main = OrgSlot::default_of(app_i);
    let app_feat = g.find_ref(app_i, "feat/refund").unwrap();
    let lib_rename = g.find_ref(lib_i, "rename").unwrap();
    // The branch's link set is computed against the library's default.
    let feat_links = g.entry(app_feat).unwrap().links.as_ref().unwrap();
    assert_eq!(feat_links.deps[0].segment_id, g.repos[lib_i].default.segment_id);
    assert!(g.repos[lib_i].refs[0].entry.links.is_none(), "the library imports nothing of the organization");

    // The call that exists only on the consumer's branch.
    let default_view = OrgSelection::default();
    let main_edges = edges_at(&idx, &st, app_main, &default_view);
    let mut feat_view = OrgSelection::default();
    feat_view.set(app_i, vec![app_feat]);
    let feat_edges = edges_at(&idx, &st, app_feat, &feat_view);
    let charge = calls(&main_edges, "shop.checkout.pay", "acmelib.core.charge");
    assert_eq!(charge.as_deref(), Some("local/pylib@main"), "{main_edges:#?}");
    assert!(calls(&main_edges, "shop.checkout.give_back", "acmelib.core.refund").is_none(), "{main_edges:#?}");
    assert!(calls(&feat_edges, "shop.checkout.give_back", "acmelib.core.refund").is_some(), "{feat_edges:#?}");
    assert!(calls(&feat_edges, "shop.checkout.pay", "acmelib.core.charge").is_some(), "{feat_edges:#?}");

    // The library's branch renamed `charge`: in its view the consumer's
    // edge into it is dropped, the one into `refund` follows the branch.
    let mut renamed_view = OrgSelection::default();
    renamed_view.set(lib_i, vec![lib_rename]);
    let main_on_renamed = edges_at(&idx, &st, app_main, &renamed_view);
    assert!(calls(&main_on_renamed, "shop.checkout.pay", "acmelib.core.charge").is_none(), "{main_on_renamed:#?}");
    assert!(calls(&main_on_renamed, "shop.checkout.pay", "acmelib.core.charge_amount").is_none());
    renamed_view.set(app_i, vec![app_feat]);
    let both = edges_at(&idx, &st, app_feat, &renamed_view);
    let refund = calls(&both, "shop.checkout.give_back", "acmelib.core.refund");
    assert_eq!(refund.as_deref(), Some("local/pylib@rename"), "{both:#?}");
    assert!(both.iter().all(|e| e.1 == "local/pylib@rename"), "{both:#?}");
    // The incoming side agrees: nothing calls the renamed function, the
    // default's `charge` keeps its caller.
    let lib_seg = |s: OrgSlot| st.open_segment(&g.entry(s).unwrap().segment_id).unwrap();
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let renamed_seg = lib_seg(lib_rename);
    let amount = renamed_seg.nodes_by_qualified_name("acmelib.core.charge_amount")[0];
    assert!(idx.link_edges(lib_rename, amount, LinkDir::In, None, &all, &renamed_view).unwrap().is_empty());
    let default_seg = lib_seg(OrgSlot::default_of(lib_i));
    let charge_node = default_seg.nodes_by_qualified_name("acmelib.core.charge")[0];
    let callers =
        idx.link_edges(OrgSlot::default_of(lib_i), charge_node, LinkDir::In, None, &all, &default_view).unwrap();
    assert_eq!(callers.iter().map(|e| e.slot).collect::<Vec<_>>(), [app_main]);
}

/// Files under `dir`, recursively.
fn count_files(dir: &Path) -> usize {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten().map(|e| if e.path().is_dir() { count_files(&e.path()) } else { 1 }).sum()
}

#[test]
fn the_linker_reads_fallback_facts_and_caches_what_it_extracts() {
    use rueca_index::link::PackageLinker;
    use rueca_segment::org::{OrgMember, update_org_with};

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
    index_org(&opts).unwrap();
    let before = link_edges(&store);
    let members: Vec<OrgMember> = Store::open(&store)
        .unwrap()
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| OrgMember { root: e.root, ..Default::default() })
        .collect();
    // The store's facts move to another store (an indexer's staging store),
    // and the organization is built again from scratch.
    let staging = base.join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::rename(store.join("facts"), staging.join("facts")).unwrap();
    let rebuild = |linker: &PackageLinker| {
        fs::remove_dir_all(store.join("orgs")).unwrap();
        let u = update_org_with(&Store::open(&store).unwrap(), "acme", &members, linker).unwrap();
        assert!(u.link_errors.is_empty(), "{:?}", u.link_errors);
        assert_eq!(link_edges(&store), before);
    };
    // With the staging store as a fallback nothing is extracted again.
    rebuild(&PackageLinker::new(&store).with_fallback(&staging));
    assert_eq!(count_files(&store.join("facts")), 0);
    // Without it, the facts are extracted from the packs once and cached.
    rebuild(&PackageLinker::new(&store));
    let cached = count_files(&store.join("facts"));
    assert!(cached > 0);
    assert_eq!(cached, count_files(&staging.join("facts")));
}
