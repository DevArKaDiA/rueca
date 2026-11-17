//! Organization mode (RFC 0006 phase 1): one ranking over the organization
//! index, `repo` as a filter, the envelope listing only the snapshots an
//! answer touched, `list_repositories`, tools resolving ids in their own
//! repository, generations, and the visibility hook.

mod common;

use std::collections::BTreeSet;

use rueca_model::{EdgeKind, EdgeMeta, Label, Strategy};
use rueca_query::{Engine, Visibility};
use rueca_segment::org::{self, LinkEdge, LinkInput, OrgLinker, OrgMember, Package, RepoDeps, RouteMode, RouteParams};
use rueca_segment::{Segment, Store};
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
        let members: Vec<OrgMember> =
            self.roots.iter().map(|r| OrgMember { root: r.clone(), ..Default::default() }).collect();
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
    // Without routing, the organization ranking is the plain one (BM25 with
    // organization-wide statistics); routing is tested below.
    let e = f.engine().with_routing(RouteParams { mode: RouteMode::Off, ..RouteParams::default() });
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
    assert_eq!(items[2]["refs"], json!([{"ref": "worktree", "commit": "worktree"}]));
    let txt = e.respond("list_repositories", json!({})).unwrap().text;
    assert!(
        txt.contains("[1] github.com/acme/payments  refs HEAD@a1b2c3d4e5f6  1 files, 2 symbols, 2 edges  python 1"),
        "{txt}"
    );
    assert!(txt.contains("[2] local/shipping  refs worktree  1 files"), "{txt}");
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
    let members: Vec<OrgMember> =
        [0, 2].iter().map(|&i| OrgMember { root: f.roots[i].clone(), ..Default::default() }).collect();
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

/// Payments publishes `payments`; billing imports it, and its
/// `InvoiceService.issue` calls `charge_card` (a stand-in for the language
/// packs of `rueca-index`).
struct FakeLinker;

impl OrgLinker for FakeLinker {
    fn version(&self) -> u32 {
        7
    }
    fn analyze(&self, _: &Store, seg: &Segment) -> rueca_segment::Result<RepoDeps> {
        let repo = seg.snapshot().repo.as_str();
        Ok(match repo {
            PAYMENTS => RepoDeps {
                packages: vec![Package {
                    eco: "pypi".into(),
                    name: "payments".into(),
                    dir: String::new(),
                    keys: vec!["py:payments".into()],
                }],
                ..Default::default()
            },
            BILLING => RepoDeps { imports: vec!["py:payments.charge".into()], ..Default::default() },
            _ => RepoDeps::default(),
        })
    }
    fn link(
        &self,
        _: &Store,
        consumer: &LinkInput<'_>,
        deps: &[LinkInput<'_>],
    ) -> rueca_segment::Result<Vec<LinkEdge>> {
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].repo.repo, PAYMENTS);
        let src = consumer.segment.nodes_by_qualified_name("pkg.service.InvoiceService.issue")[0];
        let dst = deps[0].segment.nodes_by_qualified_name("payments.charge.charge_card")[0];
        let file = consumer.segment.file_by_path("pkg/service.py").unwrap();
        let meta =
            EdgeMeta { confidence: 0.95, strategy: Strategy::ImportResolved, evidence: Some((file, line_span(PY, 9))) };
        Ok(vec![LinkEdge { src, dep: 0, dst, kind: EdgeKind::Calls, meta, reversed: false }])
    }
}

fn linked_fixture() -> OrgFix {
    let f = org_fixture();
    let st = Store::open(&f.store).unwrap();
    let members: Vec<OrgMember> = f.roots.iter().map(|r| OrgMember { root: r.clone(), ..Default::default() }).collect();
    let up = org::update_org_with(&st, "acme", &members, &FakeLinker).unwrap();
    assert_eq!(up.linked, [(BILLING.to_string(), 1)]);
    assert_eq!(up.link_edges, 1);
    f
}

fn trace_names(v: &Value) -> Vec<(String, String)> {
    let snaps = repos_of(v);
    v["results"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let s = n.get("s").map_or(v["results"]["s"].as_u64().unwrap(), |s| s.as_u64().unwrap());
            (snaps[s as usize].clone(), n["qn"].as_str().unwrap().to_string())
        })
        .collect()
}

#[test]
fn trace_follows_link_edges_across_repositories() {
    let f = linked_fixture();
    let e = f.engine();
    // Callers of a library function include the consumer's caller, in its
    // own snapshot.
    let args = json!({"qualified_name": "payments.charge.charge_card", "direction": "callers"});
    let v = call(&e, "trace", args.clone());
    // The snapshots the answer touched, in selection order.
    assert_eq!(repos_of(&v), [BILLING, PAYMENTS], "{v}");
    assert_eq!(v["results"]["s"], 1, "the root's snapshot: {v}");
    let names = trace_names(&v);
    assert!(names.contains(&(BILLING.to_string(), "pkg.service.InvoiceService.issue".to_string())), "{v}");
    let edge = &v["results"]["edges"][0];
    assert_eq!(
        (edge[2].as_str(), edge[4].as_str(), edge[5].as_u64()),
        (Some("CALLS"), Some("import_resolved"), Some(10))
    );
    // The call site is in the consumer, in the caller's own file (no path).
    assert_eq!(edge.as_array().unwrap().len(), 6, "{v}");
    let txt = e.respond("trace", args.clone()).unwrap().text;
    assert!(txt.contains("1 [0] in d1 Method InvoiceService.issue  pkg/service.py"), "{txt}");
    let verbose = call(
        &e,
        "trace",
        json!({"qualified_name": "payments.charge.charge_card", "direction": "callers", "format": "verbose"}),
    );
    assert_eq!(verbose["edges"][0]["call_site"]["path"], "pkg/service.py");
    // Callees of the consumer reach into the library.
    let v =
        call(&e, "trace", json!({"qualified_name": "InvoiceService.issue", "direction": "callees", "repo": BILLING}));
    assert!(trace_names(&v).contains(&(PAYMENTS.to_string(), "payments.charge.charge_card".to_string())), "{v}");

    // Link edges are returned only when both ends are visible.
    let only_payments = f.engine().with_visibility(Visibility::Repos([PAYMENTS.to_string()].into_iter().collect()));
    let v = call(&only_payments, "trace", args);
    assert_eq!(repos_of(&v), [PAYMENTS], "{v}");
    assert_eq!(v["results"]["nodes"].as_array().unwrap().len(), 1, "the root alone: {v}");
    let only_billing = f.engine().with_visibility(Visibility::Repos([BILLING.to_string()].into_iter().collect()));
    let v = call(&only_billing, "trace", json!({"qualified_name": "InvoiceService.issue", "direction": "callees"}));
    assert!(!v.to_string().contains("acme/payments"), "{v}");

    // `list_repositories` names packages and visible dependencies.
    let v = call(&e, "list_repositories", json!({}));
    let item = |repo: &str| v["results"].as_array().unwrap().iter().find(|r| r["repo"] == repo).unwrap().clone();
    assert_eq!(item(BILLING)["depends_on"], json!([PAYMENTS]));
    assert_eq!(item(PAYMENTS)["packages"], json!(["payments"]));
    let txt = e.respond("list_repositories", json!({})).unwrap().text;
    assert!(txt.contains("depends on github.com/acme/payments"), "{txt}");
    let v = call(&only_billing, "list_repositories", json!({}));
    assert!(v["results"][0].get("depends_on").is_none(), "{v}");
}

/// The impact question (RFC 0007 §7.3): a library symbol's consumers in
/// other repositories join a search for it through graph expansion over the
/// link layer.
#[test]
fn expansion_reaches_consumers_across_repositories() {
    let f = linked_fixture();
    let expand = rueca_query::GraphParams { expand: rueca_query::ExpandEdges::Calls, ..Default::default() };
    let q = json!({"query": "charge_card", "scores": true, "mode": "lexical"});
    let consumer = (BILLING.to_string(), "pkg.service.InvoiceService.issue".to_string());
    let has = |v: &Value| ranking(v).iter().any(|(r, qn, _)| (r.clone(), qn.clone()) == consumer);
    let off = call(&f.engine(), "search_symbols", q.clone());
    assert!(!has(&off), "{off}");
    let v = call(&f.engine().with_graph(expand), "search_symbols", q.clone());
    assert!(has(&v), "{v}");
    assert_eq!(ranking(&v)[0].1, "payments.charge.charge_card", "{v}");
    assert!(v["retrieval"]["expansion"]["added"].as_u64().unwrap() >= 1, "{v}");
    // Not across what the caller cannot read, nor outside `repo`.
    let only = f.engine().with_visibility(Visibility::Repos([PAYMENTS.to_string()].into_iter().collect()));
    assert!(!has(&call(&only.with_graph(expand), "search_symbols", q.clone())));
    let mut narrowed = q;
    narrowed["repo"] = json!(PAYMENTS);
    assert!(!has(&call(&f.engine().with_graph(expand), "search_symbols", narrowed)));
}

#[test]
fn routing_ranks_repositories_and_weighs_symbols() {
    let f = linked_fixture();
    let e = f.engine();
    // `list_repositories` with a query: best repository first, scores in 0–1.
    let v = call(&e, "list_repositories", json!({"query": "charge the card through the payments gateway"}));
    let items = v["results"].as_array().unwrap();
    assert_eq!(items[0]["repo"], PAYMENTS, "{v}");
    assert_eq!(items[0]["score"], 1.0);
    assert!(items.iter().all(|r| r["score"].as_f64().is_some_and(|s| (0.0..=1.0).contains(&s))), "{v}");
    let txt = e.respond("list_repositories", json!({"query": "printLabel order address"})).unwrap().text;
    assert!(
        txt.lines().nth(3).is_some_and(|l| l.contains(SHIPPING)) || txt.contains(&format!("] {SHIPPING}")),
        "{txt}"
    );
    // Without a query the order is by id and there are no scores.
    let v = call(&e, "list_repositories", json!({}));
    assert!(v["results"][0].get("score").is_none());

    // Routing reorders the symbol ranking toward the routed repository;
    // `off` gives the plain organization ranking.
    let q = json!({"query": "refund payment gateway charge", "scores": true, "limit": 50});
    let routed = ranking(&call(&e, "search_symbols", q.clone()));
    let plain = ranking(&call(
        &f.engine().with_routing(RouteParams { mode: RouteMode::Off, ..RouteParams::default() }),
        "search_symbols",
        q.clone(),
    ));
    assert_eq!(routed[0].0, PAYMENTS, "{routed:?}");
    let as_set = |r: &[(String, String, f64)]| r.iter().map(|x| (x.0.clone(), x.1.clone())).collect::<BTreeSet<_>>();
    assert_eq!(as_set(&routed), as_set(&plain), "a prior reorders, it never drops");
    // `top` searches only the best repositories.
    let top = RouteParams { mode: RouteMode::Top, top: 1, ..RouteParams::default() };
    let r = ranking(&call(&f.engine().with_routing(top), "search_symbols", q));
    assert!(!r.is_empty() && r.iter().all(|x| x.0 == PAYMENTS), "{r:?}");
}

const KEEP: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
const RENAME: &str = "decade00decade00decade00decade00decade00";
const AUDIT_PY: &str = "def audit(payment):\n    return payment\n";
const RENAMED_PY: &str = "def charge_card_v2(card, amount):\n    return gateway.charge(card, amount)\n";

/// Registers ref `name` of payments (commit `commit`) as a ref snapshot
/// exported under the store, built by `build`.
fn payments_ref(store: &std::path::Path, name: &str, commit: &str, build: impl FnOnce(&mut Builder)) {
    let root = std::fs::canonicalize(store).unwrap().join("refs").join("github-com-acme-payments").join(name);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.to_string_lossy().into_owned();
    let mut b = Builder::new(PAYMENTS, commit, &root);
    b.graph.snapshot.branch = Some(name.into());
    build(&mut b);
    b.graph.validate().unwrap();
    let meta = rueca_segment::write_segment(&b.graph, &Sources(b.sources.clone()), store).unwrap();
    Store::open(store).unwrap().register_ref(&root, &meta, name).unwrap();
}

/// The linked organization, payments on `main` with two refs: `keep`
/// (`charge.py` unchanged, plus `aaa.py`, which shifts every node id) and
/// `rename` (`charge_card` renamed to `charge_card_v2`).
fn refs_fixture() -> OrgFix {
    let f = org_fixture();
    let mut p = payments(&f.roots[1], "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", false);
    p.graph.snapshot.branch = Some("main".into());
    p.write(&f.store, &f.roots[1]);
    payments_ref(&f.store, "keep", KEEP, |b| {
        let (f, n) = b.file("payments/aaa.py", AUDIT_PY);
        b.sym(
            n,
            f,
            Label::Function,
            "audit",
            "payments.aaa.audit",
            span(AUDIT_PY, 0, 1),
            Some("def audit(payment)"),
            true,
            false,
        );
        let fresh = payments("", "", false);
        let base = b.graph.nodes.len() as u32;
        let file_base = b.graph.files.len() as u32;
        for mut node in fresh.graph.nodes {
            node.file = node.file.map(|x| x + file_base);
            b.graph.nodes.push(node);
        }
        for mut file in fresh.graph.files {
            file.node += base;
            b.graph.files.push(file);
        }
        for mut e in fresh.graph.edges {
            e.src += base;
            e.dst += base;
            b.graph.edges.push(e);
        }
        b.sources.extend(fresh.sources);
    });
    payments_ref(&f.store, "rename", RENAME, |b| {
        let (f, n) = b.file("payments/charge.py", RENAMED_PY);
        b.sym(
            n,
            f,
            Label::Function,
            "charge_card_v2",
            "payments.charge.charge_card_v2",
            span(RENAMED_PY, 0, 1),
            Some("def charge_card_v2(card, amount)"),
            true,
            false,
        );
    });
    let st = Store::open(&f.store).unwrap();
    let members: Vec<OrgMember> = f.roots.iter().map(|r| OrgMember { root: r.clone(), ..Default::default() }).collect();
    let up = org::update_org_with(&st, "acme", &members, &FakeLinker).unwrap();
    assert_eq!(up.refs_added, ["github.com/acme/payments@keep", "github.com/acme/payments@rename"], "{up:?}");
    f
}

/// `(ref, qn)` of a compact `search_symbols` answer.
fn hits_by_ref(v: &Value) -> Vec<(String, String)> {
    let refs: Vec<String> = v["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| format!("{}@{}", s["repo"].as_str().unwrap(), s["ref"].as_str().unwrap()))
        .collect();
    let mut out = Vec::new();
    for g in v["results"].as_array().unwrap() {
        for s in g["symbols"].as_array().unwrap() {
            out.push((refs[g["s"].as_u64().unwrap() as usize].clone(), s["qn"].as_str().unwrap().to_string()));
        }
    }
    out
}

#[test]
fn ref_views_over_the_organization() {
    let f = refs_fixture();
    let e = f.engine();
    let pay_keep = format!("{PAYMENTS}@keep");
    // `repo: A, ref: X`: A at X (envelope and results), pinned ids.
    let v = call(&e, "search_symbols", json!({"query": "audit", "repo": PAYMENTS, "ref": "keep"}));
    assert_eq!(hits_by_ref(&v)[0], (pay_keep.clone(), "payments.aaa.audit".to_string()), "{v}");
    assert_eq!(
        (v["snapshots"][0]["commit"].as_str(), v["snapshots"][0]["overlay"].as_bool()),
        (Some(KEEP), Some(false))
    );
    let id = v["results"][0]["symbols"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(id, format!("{PAYMENTS}@c0ffee00c0ff:payments/aaa.py#audit"));
    // Every repository `repo` names must have the ref.
    let m = err(&e, "search_symbols", json!({"query": "x", "repo": [BILLING, PAYMENTS], "ref": "keep"}));
    assert!(
        m.contains(
            "ref `keep` is not indexed for repository `github.com/acme/billing`; indexed refs: main@7696364aef89"
        ),
        "{m}"
    );
    assert!(m.contains("rueca index --org <file> <member> --ref <name>"), "{m}");
    // `ref` without `repo`: every repository that has it at it, others at
    // their default.
    let v = call(&e, "search_symbols", json!({"query": "audit invoice issue", "ref": "keep", "limit": 50}));
    let refs: BTreeSet<String> = hits_by_ref(&v).into_iter().map(|h| h.0).collect();
    assert!(refs.contains(&pay_keep) && refs.contains(&format!("{BILLING}@main")), "{v}");
    assert!(!refs.contains(&format!("{PAYMENTS}@main")), "{v}");
    // An unknown ref lists every selected repository's refs.
    let m = err(&e, "search_symbols", json!({"query": "x", "ref": "nope"}));
    assert!(
        m.contains("ref `nope` is not indexed for any selected repository")
            && m.contains("github.com/acme/payments: main@a1b2c3d4e5f6, keep@c0ffee00c0ff, rename@decade00deca"),
        "{m}"
    );

    // `ref` as a list: several entries of one repository, collapsed.
    let v = call(
        &e,
        "search_symbols",
        json!({"query": "charge card audit", "repo": PAYMENTS, "ref": ["main", "keep"], "limit": 50}),
    );
    let refs: Vec<&str> = v["snapshots"].as_array().unwrap().iter().map(|s| s["ref"].as_str().unwrap()).collect();
    assert_eq!(refs, ["main", "keep"]);
    let items: Vec<(String, Value, bool)> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| {
            g["symbols"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s["qn"].as_str().unwrap().to_string(), s["in"].clone(), s["changed"] == true))
        })
        .collect();
    assert!(items.contains(&("payments.charge.charge_card".to_string(), json!([0, 1]), false)), "{v}");
    assert!(items.contains(&("payments.aaa.audit".to_string(), json!([1]), true)), "{v}");
    assert_eq!(items.iter().filter(|i| i.0 == "payments.charge.charge_card").count(), 1, "{v}");
    let txt = e
        .respond("search_symbols", json!({"query": "charge card audit", "repo": PAYMENTS, "ref": ["main", "keep"]}))
        .unwrap()
        .text;
    assert!(
        txt.contains("charge_card L1-2  def charge_card(card, amount) [0,1]\n")
            && txt.contains("audit L1-2  def audit(payment) changed\n"),
        "{txt}"
    );

    // Ids whose commit is a ref snapshot's resolve to it.
    for tool in ["get_symbol", "get_snippet", "trace"] {
        let r = call(&e, tool, json!({"id": id}));
        assert_eq!(
            (r["snapshots"][0]["ref"].as_str(), r["snapshots"].as_array().unwrap().len()),
            (Some("keep"), 1),
            "{tool}: {r}"
        );
    }
    let r = call(&e, "get_symbol", json!({"id": format!("{PAYMENTS}@c0ffee0:payments/charge.py#charge_card")}));
    assert_eq!(r["results"]["symbol"]["id"], format!("{PAYMENTS}@c0ffee00c0ff:payments/charge.py#charge_card"));
}

#[test]
fn trace_sees_link_edges_remapped_into_a_ref() {
    let f = refs_fixture();
    let e = f.engine();
    let callee = |r: &Value| {
        trace_names(r).into_iter().filter(|(repo, _)| repo == PAYMENTS).map(|(_, qn)| qn).collect::<Vec<_>>()
    };
    // Default view: billing's call into payments' default.
    let args = |reference: Option<&str>| {
        let mut a = json!({"qualified_name": "InvoiceService.issue", "direction": "callees"});
        if let Some(r) = reference {
            a["ref"] = json!(r);
        }
        a
    };
    let v = call(&e, "trace", args(None));
    assert_eq!(callee(&v), ["payments.charge.charge_card"]);
    // Payments viewed at `keep`: the edge lands on keep's own node.
    let v = call(&e, "trace", args(Some("keep")));
    assert_eq!(callee(&v), ["payments.charge.charge_card"], "{v}");
    let snaps = v["snapshots"].as_array().unwrap();
    let pay = snaps.iter().find(|s| s["repo"] == PAYMENTS).unwrap();
    assert_eq!((pay["ref"].as_str(), pay["commit"].as_str()), (Some("keep"), Some(KEEP)));
    let node = v["results"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["qn"] == "payments.charge.charge_card")
        .unwrap()
        .clone();
    assert_eq!(node["id"], format!("{PAYMENTS}@c0ffee00c0ff:payments/charge.py#charge_card"));
    let txt = e.respond("trace", args(Some("keep"))).unwrap().text;
    assert!(
        txt.contains("github.com/acme/payments@c0ffee00c0ff (keep)")
            && txt.contains("Function charge_card  payments/charge.py L1-2  ←0 CALLS 0.95 import_resolved @10"),
        "{txt}"
    );
    // At `rename` the target is gone: the edge is dropped.
    let v = call(&e, "trace", args(Some("rename")));
    assert!(callee(&v).is_empty(), "{v}");
    // Callers of keep's node: billing's default, through the remapped edge.
    let v = call(
        &e,
        "trace",
        json!({"qualified_name": "payments.charge.charge_card", "direction": "callers", "repo": PAYMENTS, "ref": "keep"}),
    );
    assert!(trace_names(&v).contains(&(BILLING.to_string(), "pkg.service.InvoiceService.issue".to_string())), "{v}");
    // Both refs at once: one node for charge_card, the edge in both.
    let v = call(
        &e,
        "trace",
        json!({"qualified_name": "payments.charge.charge_card", "direction": "callers", "repo": PAYMENTS, "ref": ["main", "keep"]}),
    );
    let nodes = v["results"]["nodes"].as_array().unwrap();
    assert_eq!((nodes.len(), nodes[0]["in"].clone(), nodes[0].get("changed")), (2, json!([0, 1]), None), "{v}");
    assert_eq!(v["results"]["edges"][0].as_array().unwrap().last().unwrap(), &json!({"in": [0, 1]}), "{v}");
}

#[test]
fn refs_respect_visibility_and_are_listed() {
    let f = refs_fixture();
    let e = f.engine();
    // `list_repositories`: every indexed snapshot, the default first, and
    // routing unchanged.
    let v = call(&e, "list_repositories", json!({}));
    let pay = v["results"].as_array().unwrap().iter().find(|r| r["repo"] == PAYMENTS).unwrap().clone();
    assert_eq!(
        pay["refs"],
        json!([{"ref": "main", "commit": "a1b2c3d4e5f6"}, {"ref": "keep", "commit": "c0ffee00c0ff"}, {"ref": "rename", "commit": "decade00deca"}])
    );
    assert_eq!(
        (pay["default_ref"].as_str(), pay["commit"].as_str()),
        (Some("main"), None),
        "compact keeps no commit: {pay}"
    );
    let txt = e.respond("list_repositories", json!({})).unwrap().text;
    assert!(
        txt.contains(
            "github.com/acme/payments  refs main@a1b2c3d4e5f6, keep@c0ffee00c0ff, rename@decade00deca  1 files"
        ),
        "{txt}"
    );
    let v = call(&e, "list_repositories", json!({"format": "verbose"}));
    let pay = v["repositories"].as_array().unwrap().iter().find(|r| r["repo"] == PAYMENTS).unwrap().clone();
    assert_eq!(
        (pay["commit"].as_str(), pay["refs"][1]["ref"].as_str()),
        (Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"), Some("keep"))
    );
    let v = call(&e, "list_repositories", json!({"query": "charge the card through the payments gateway"}));
    assert_eq!(v["results"][0]["repo"], PAYMENTS, "{v}");
    let v = call(&e, "list_repositories", json!({"query": "charge the card", "ref": "keep"}));
    assert_eq!(v["results"][0]["repo"], PAYMENTS, "routing with payments at keep: {v}");

    // A hidden repository's refs are hidden too, and errors do not name it.
    let hidden = f
        .engine()
        .with_visibility(Visibility::Repos([BILLING.to_string(), SHIPPING.to_string()].into_iter().collect()));
    let keep = err(&hidden, "search_symbols", json!({"query": "x", "ref": "keep"}));
    let nope = err(&hidden, "search_symbols", json!({"query": "x", "ref": "nope"}));
    assert_eq!(keep.replace("keep", "<r>"), nope.replace("nope", "<r>"));
    assert!(!keep.contains("payments") && !keep.contains("c0ffee"), "{keep}");
    let v = call(&hidden, "list_repositories", json!({}));
    assert!(!v.to_string().contains("payments") && !v.to_string().contains("c0ffee"), "{v}");
    let m = err(&hidden, "get_symbol", json!({"id": format!("{PAYMENTS}@c0ffee00c0ff:payments/aaa.py#audit")}));
    assert!(m.contains("not indexed in organization") && !m.contains("keep"), "{m}");
}

#[test]
fn a_pull_request_without_repo_names_one_visible_repository() {
    let f = refs_fixture();
    payments_ref(&f.store, "pr/7", KEEP, |b| {
        b.file("payments/charge.py", CHARGE_PY);
    });
    let root = std::fs::canonicalize(&f.store).unwrap().join("refs").join("github-com-acme-billing").join("pr-7");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.to_string_lossy().into_owned();
    let (mut b, _) = demo(BILLING, &root);
    b.graph.snapshot.commit = RENAME.into();
    b.graph.snapshot.branch = Some("pr/7".into());
    let meta = rueca_segment::write_segment(&b.graph, &Sources(b.sources.clone()), &f.store).unwrap();
    Store::open(&f.store).unwrap().register_ref(&root, &meta, "pr/7").unwrap();
    let st = Store::open(&f.store).unwrap();
    let members: Vec<OrgMember> = f.roots.iter().map(|r| OrgMember { root: r.clone(), ..Default::default() }).collect();
    org::update_org_with(&st, "acme", &members, &FakeLinker).unwrap();

    let e = f.engine();
    let m = err(&e, "search_symbols", json!({"query": "x", "ref": "pr/7"}));
    assert!(
        m.contains(
            "`pr/7` exists in several repositories (github.com/acme/billing, github.com/acme/payments): name `repo`"
        ),
        "{m}"
    );
    let v = call(&e, "search_symbols", json!({"query": "charge card", "repo": PAYMENTS, "ref": "pr/7"}));
    assert_eq!(v["snapshots"][0]["ref"], "pr/7", "{v}");
    // Only one visible repository has it: accepted, and nothing names the
    // hidden one.
    let hidden = f
        .engine()
        .with_visibility(Visibility::Repos([BILLING.to_string(), SHIPPING.to_string()].into_iter().collect()));
    let v = call(&hidden, "search_symbols", json!({"query": "helper", "ref": "pr/7"}));
    assert!(!v.to_string().contains("payments"), "{v}");
    assert!(v["snapshots"].as_array().unwrap().iter().any(|s| s["repo"] == BILLING && s["ref"] == "pr/7"), "{v}");
}
