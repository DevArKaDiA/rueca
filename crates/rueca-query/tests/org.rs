//! Organization mode (RFC 0006 phase 1): one ranking over the organization
//! index, `repo` as a filter, the envelope listing only the snapshots an
//! answer touched, `list_repositories`, tools resolving ids in their own
//! repository, generations, and the visibility hook.

mod common;

use std::collections::BTreeSet;

use rueca_model::Label;
use rueca_query::{Engine, Visibility};
use rueca_segment::Store;
use rueca_segment::org::{self, OrgMember};
use serde_json::{Value, json};

use common::*;

const BILLING: &str = "github.com/acme/billing";
const PAYMENTS: &str = "github.com/acme/payments";
const SHIPPING: &str = "local/shipping";

const CHARGE_PY: &str = "def charge_card(card, amount):\n    return gateway.charge(card, amount)\n\ndef refund_payment(payment):\n    return gateway.refund(payment)\n";
const LABEL_TS: &str = "export function printLabel(order: Order): string {\n  return order.address;\n}\n";

struct OrgFix {
    _dir: tempfile::TempDir,
    store: std::path::PathBuf,
    roots: Vec<String>,
}

fn payments(root: &str, commit: &str, extra: bool) -> Builder {
    let mut b = Builder::new(PAYMENTS, commit, root);
    let (f, n) = b.file("payments/charge.py", CHARGE_PY);
    b.sym(
        n,
        f,
        Label::Function,
        "charge_card",
        "payments.charge.charge_card",
        span(CHARGE_PY, 0, 1),
        Some("def charge_card(card, amount)"),
        true,
        false,
    );
    b.sym(
        n,
        f,
        Label::Function,
        "refund_payment",
        "payments.charge.refund_payment",
        span(CHARGE_PY, 3, 4),
        Some("def refund_payment(payment)"),
        true,
        false,
    );
    if extra {
        let src = "def open_dispute(payment):\n    return payment\n";
        let (f, n) = b.file("payments/dispute.py", src);
        b.sym(
            n,
            f,
            Label::Function,
            "open_dispute",
            "payments.dispute.open_dispute",
            span(src, 0, 1),
            Some("def open_dispute(payment)"),
            true,
            false,
        );
    }
    b
}

fn shipping(root: &str) -> Builder {
    let mut b = Builder::new(SHIPPING, "worktree", root);
    let (f, n) = b.file("src/label.ts", LABEL_TS);
    b.sym(
        n,
        f,
        Label::Function,
        "printLabel",
        "src/label:printLabel",
        span(LABEL_TS, 0, 2),
        Some("export function printLabel(order: Order): string"),
        true,
        false,
    );
    b
}

fn org_fixture() -> OrgFix {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let mut roots = Vec::new();
    for name in ["billing", "payments", "shipping"] {
        let p = dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        roots.push(std::fs::canonicalize(&p).unwrap().to_string_lossy().into_owned());
    }
    let (mut b, _) = demo(BILLING, &roots[0]);
    b.graph.snapshot.commit = "7696364aef89a22e4a2fe84f6fd23e186bef9aa8".into();
    b.graph.snapshot.branch = Some("main".into());
    b.write(&store, &roots[0]);
    payments(&roots[1], "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", false).write(&store, &roots[1]);
    shipping(&roots[2]).write(&store, &roots[2]);
    let fix = OrgFix { _dir: dir, store, roots };
    fix.update();
    fix
}

impl OrgFix {
    fn update(&self) -> org::OrgUpdate {
        let st = Store::open(&self.store).unwrap();
        let members: Vec<OrgMember> = self.roots.iter().map(|r| OrgMember { root: r.clone(), url: None }).collect();
        org::update_org(&st, "acme", &members).unwrap()
    }

    fn engine(&self) -> Engine {
        Engine::for_org(self.store.clone(), "acme").unwrap()
    }
}

fn call(e: &Engine, tool: &str, args: Value) -> Value {
    e.call(tool, args.clone()).unwrap_or_else(|err| panic!("{tool} {args}: {err:#}"))
}

fn err(e: &Engine, tool: &str, args: Value) -> String {
    format!("{:#}", e.call(tool, args).expect_err("expected an error"))
}

fn repos_of(v: &Value) -> Vec<String> {
    v["snapshots"].as_array().unwrap().iter().map(|s| s["repo"].as_str().unwrap().to_string()).collect()
}

/// (repo, qn, score) of a compact `search_symbols` answer, in rank order.
fn ranking(v: &Value) -> Vec<(String, String, f64)> {
    let snaps = repos_of(v);
    let mut out = Vec::new();
    for g in v["results"].as_array().unwrap() {
        let repo = &snaps[g["s"].as_u64().unwrap() as usize];
        for s in g["symbols"].as_array().unwrap() {
            out.push((repo.clone(), s["qn"].as_str().unwrap().to_string(), s["score"].as_f64().unwrap_or(0.0)));
        }
    }
    out.sort_by(|a, b| b.2.total_cmp(&a.2));
    out
}

#[test]
fn whole_organization_is_the_default_scope() {
    let f = org_fixture();
    let e = f.engine();
    // `repo` omitted: the whole organization, one ranking.
    let v = call(&e, "search_symbols", json!({"query": "refund payment invoice", "scores": true}));
    let r = ranking(&v);
    let repos: BTreeSet<&str> = r.iter().map(|x| x.0.as_str()).collect();
    assert!(repos.contains(BILLING) && repos.contains(PAYMENTS), "{v}");
    // Only snapshots that appear in the answer are listed.
    assert!(!repos_of(&v).contains(&SHIPPING.to_string()), "{v}");

    // An answer from one repository lists one snapshot and no `[s]` tags.
    let v = call(&e, "search_symbols", json!({"query": "printLabel"}));
    assert_eq!(repos_of(&v), [SHIPPING]);
    let txt = e.respond("search_symbols", json!({"query": "printLabel"})).unwrap().text;
    assert!(txt.starts_with("local/shipping@worktree (worktree)\nsrc/label.ts\n"), "{txt}");

    // `repo` filters the same index before top-k: same scores.
    let all = call(&e, "search_symbols", json!({"query": "refund payment invoice", "scores": true, "limit": 50}));
    let only = call(
        &e,
        "search_symbols",
        json!({"query": "refund payment invoice", "scores": true, "limit": 50, "repo": PAYMENTS}),
    );
    let expected: Vec<_> = ranking(&all).into_iter().filter(|x| x.0 == PAYMENTS).collect();
    assert_eq!(ranking(&only), expected);
    assert_eq!(repos_of(&only), [PAYMENTS]);

    // The organization index ranks like the local mode over the same roots.
    let local = Engine::new(f.store.clone(), None).unwrap();
    let star = call(
        &local,
        "search_symbols",
        json!({"query": "refund payment invoice", "scores": true, "limit": 50, "repo": "*"}),
    );
    assert_eq!(ranking(&star), ranking(&all));
}

#[test]
fn tools_resolve_in_the_repository_of_an_id() {
    let f = org_fixture();
    let e = f.engine();
    let v = call(&e, "search_symbols", json!({"query": "charge_card"}));
    let id = v["results"][0]["symbols"][0]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with(&format!("{PAYMENTS}@a1b2c3d4e5f6:")), "{id}");
    for tool in ["get_symbol", "get_snippet", "trace"] {
        let r = call(&e, tool, json!({"id": id}));
        assert_eq!(repos_of(&r), [PAYMENTS], "{tool}: {r}");
    }
    // A name is looked up over the organization; a path unique to one
    // repository needs no `repo`.
    let r = call(&e, "get_symbol", json!({"name": "printLabel"}));
    assert_eq!(repos_of(&r), [SHIPPING]);
    let r = call(&e, "get_file_outline", json!({"path": "payments/charge.py"}));
    assert_eq!(r["results"]["symbols"].as_array().unwrap().len(), 2);
    // Unknown repositories.
    let m = err(&e, "search_symbols", json!({"query": "x", "repo": "github.com/acme/nope"}));
    assert!(m.contains("not indexed in organization `acme`"), "{m}");
    let m = err(&e, "get_symbol", json!({"id": "github.com/acme/nope@abc1234:x.py#f"}));
    assert!(m.contains("not indexed in organization `acme`"), "{m}");
}

#[test]
fn list_repositories_shows_the_organization() {
    let f = org_fixture();
    let e = f.engine();
    let v = call(&e, "list_repositories", json!({}));
    assert_eq!(v["org"], "acme");
    let items = v["results"].as_array().unwrap();
    let names: Vec<&str> = items.iter().map(|r| r["repo"].as_str().unwrap()).collect();
    assert_eq!(names, [BILLING, PAYMENTS, SHIPPING]);
    assert_eq!(items[0]["default_ref"], "main");
    assert_eq!(items[0]["languages"], json!({"python": 1, "typescript": 1}));
    assert_eq!(items[1]["symbols"], 2);
    assert_eq!(items[2]["refs"], json!(["worktree"]));
    let txt = e.respond("list_repositories", json!({})).unwrap().text;
    assert!(txt.contains("[1] github.com/acme/payments  refs HEAD  1 files, 2 symbols, 2 edges  python 1"), "{txt}");
    let v = call(&e, "list_repositories", json!({"format": "verbose"}));
    assert_eq!(v["repositories"].as_array().unwrap().len(), 3);
    // Local mode lists every indexed root.
    let local = Engine::new(f.store.clone(), None).unwrap();
    let v = call(&local, "list_repositories", json!({}));
    assert_eq!(v["org"], Value::Null);
    assert_eq!(v["results"].as_array().unwrap().len(), 3);
}

#[test]
fn visibility_hides_repositories_everywhere() {
    let f = org_fixture();
    let vis = Visibility::Repos([BILLING.to_string(), SHIPPING.to_string()].into_iter().collect());
    let e = f.engine().with_visibility(vis);
    for q in ["refund payment", "charge_card", "payments charge", "printLabel helper"] {
        for args in [json!({"query": q, "limit": 200}), json!({"query": q, "repo": "*", "limit": 200})] {
            let v = call(&e, "search_symbols", args);
            assert!(!v.to_string().contains("acme/payments"), "{v}");
        }
    }
    let v = call(&e, "list_repositories", json!({}));
    let names: Vec<&str> = v["results"].as_array().unwrap().iter().map(|r| r["repo"].as_str().unwrap()).collect();
    assert_eq!(names, [BILLING, SHIPPING]);
    // Naming it is answered like a repository that does not exist.
    let hidden = err(&e, "search_symbols", json!({"query": "x", "repo": PAYMENTS}));
    let missing = err(&e, "search_symbols", json!({"query": "x", "repo": "github.com/acme/nope"}));
    assert_eq!(hidden.replace(PAYMENTS, "<r>"), missing.replace("github.com/acme/nope", "<r>"));
    let m = err(&e, "get_symbol", json!({"id": format!("{PAYMENTS}@a1b2c3d4e5f6:payments/charge.py#charge_card")}));
    assert!(m.contains("not indexed in organization"), "{m}");
    assert!(err(&e, "get_symbol", json!({"name": "charge_card"})).contains("no symbol found"));
    let v = call(&e, "search_code", json!({"pattern": "gateway"}));
    assert!(v["results"].as_array().unwrap().is_empty(), "{v}");

    // Scores do not depend on the hidden repository: they equal those of an
    // organization without it.
    let st = Store::open(&f.store).unwrap();
    let members: Vec<OrgMember> = [0, 2].iter().map(|&i| OrgMember { root: f.roots[i].clone(), url: None }).collect();
    org::update_org(&st, "small", &members).unwrap();
    let small = Engine::for_org(f.store.clone(), "small").unwrap();
    let args = json!({"query": "refund payment invoice helper", "scores": true, "limit": 50});
    assert_eq!(call(&e, "search_symbols", args.clone()), call(&small, "search_symbols", args));
    let none = f.engine().with_visibility(Visibility::Repos(BTreeSet::new()));
    assert!(err(&none, "search_symbols", json!({"query": "x"})).contains("no repositories you can read"));
}

#[test]
fn a_new_generation_is_picked_up() {
    let f = org_fixture();
    let e = f.engine();
    let before = call(&e, "search_symbols", json!({"query": "open_dispute", "repo": PAYMENTS}));
    assert!(!before.to_string().contains("payments.dispute.open_dispute"), "{before}");
    payments(&f.roots[1], "b1b2c3d4e5f60718293a4b5c6d7e8f9012345678", true).write(&f.store, &f.roots[1]);
    // The catalog moved on, but the organization still serves generation 1
    // until it is updated.
    let still = call(&e, "search_symbols", json!({"query": "open_dispute", "repo": PAYMENTS}));
    assert_eq!(still, before);
    let up = f.update();
    assert_eq!((up.generation, up.replaced.clone()), (2, vec![PAYMENTS.to_string()]));
    let after = call(&e, "search_symbols", json!({"query": "open_dispute"}));
    assert_eq!(after["results"][0]["symbols"][0]["qn"], "payments.dispute.open_dispute", "{after}");
    assert_eq!(repos_of(&after)[0], PAYMENTS);
}

#[test]
fn organization_not_built_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::for_org(dir.path().join("store"), "acme").unwrap();
    let m = err(&e, "search_symbols", json!({"query": "x"}));
    assert!(m.contains("organization `acme` is not built"), "{m}");
    assert!(Engine::for_org(dir.path().into(), "../x").is_err());
}
