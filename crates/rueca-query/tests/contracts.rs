//! `list_contracts` (RFC 0006 phase 4) and contract link edges in `trace`:
//! three repositories with hand-built contract tables — `api` provides two
//! HTTP routes, `web` calls one of them, a route nobody visible provides and
//! an RPC method, `admin` provides the route `web` would otherwise miss.

mod common;

use std::collections::BTreeSet;

use rueca_model::contract::match_keys;
use rueca_model::{ContractKind, ContractRow, ContractSide, EdgeKind, EdgeMeta, Label, NodeId, Strategy};
use rueca_query::{Engine, Visibility};
use rueca_segment::org::{self, ContractKey, LinkEdge, LinkInput, OrgLinker, OrgMember, RepoDeps};
use rueca_segment::{Segment, Store};
use serde_json::{Value, json};

use common::*;

const API: &str = "github.com/acme/api";
const WEB: &str = "github.com/acme/web";
const ADMIN: &str = "github.com/acme/admin";

const API_PY: &str = "@app.get(\"/users/{id}\")\ndef get_user(id):\n    return id\n\n@app.post(\"/users\")\ndef create_user():\n    return 1\n";
const WEB_PY: &str = "def load_user(uid):\n    return requests.get(f\"/users/{uid}\")\n\ndef drop_user(uid):\n    return requests.delete(f\"/users/{uid}\")\n\ndef hello(stub):\n    return stub.SayHello(None)\n";
const ADMIN_PY: &str = "@app.delete(\"/users/{id}\")\ndef delete_user(id):\n    return id\n";

/// A contract row on `node`, at 0-based line `line` of `text`.
fn contract(b: &mut Builder, node: NodeId, side: ContractSide, kind: ContractKind, key: &str, raw: &str, line: u32) {
    let file = b.graph.nodes[node as usize].file.unwrap();
    let text = String::from_utf8(b.sources[&b.graph.files[file as usize].path].clone()).unwrap();
    let framework = match (side, kind) {
        (ContractSide::Provides, _) => "fastapi",
        (_, ContractKind::Rpc) => "grpc-python",
        _ => "requests",
    };
    b.graph.contracts.push(ContractRow {
        node,
        side,
        kind,
        key: key.into(),
        raw: raw.into(),
        file,
        span: line_span(&text, line),
        framework: framework.into(),
    });
}

/// A one-file Python repository with functions `(name, first line, last
/// line)`; their node ids in order.
fn repo(repo: &str, root: &str, path: &str, text: &str, defs: &[(&str, u32, u32)]) -> (Builder, Vec<NodeId>) {
    let mut b = Builder::new(repo, "0123456789abcdef0123456789abcdef01234567", root);
    let (f, n) = b.file(path, text);
    let module = path.trim_end_matches(".py").replace('/', ".");
    let ids = defs
        .iter()
        .map(|&(name, l0, l1)| {
            let qn = format!("{module}.{name}");
            b.sym(n, f, Label::Function, name, &qn, span(text, l0, l1), None, true, false)
        })
        .collect();
    (b, ids)
}

fn api(root: &str) -> Builder {
    let (mut b, ids) = repo(API, root, "api/app.py", API_PY, &[("get_user", 1, 2), ("create_user", 5, 6)]);
    contract(&mut b, ids[0], ContractSide::Provides, ContractKind::Http, "GET /users/{}", "/users/{id}", 0);
    contract(&mut b, ids[1], ContractSide::Provides, ContractKind::Http, "POST /users", "/users", 4);
    b
}

fn web(root: &str) -> Builder {
    let defs = [("load_user", 0, 1), ("drop_user", 3, 4), ("hello", 6, 7)];
    let (mut b, ids) = repo(WEB, root, "web/client.py", WEB_PY, &defs);
    let http = ContractKind::Http;
    contract(&mut b, ids[0], ContractSide::Consumes, http, "GET /users/{}", "requests.get(f\"/users/{uid}\")", 1);
    contract(&mut b, ids[1], ContractSide::Consumes, http, "DELETE /users/{}", "requests.delete(…)", 4);
    contract(&mut b, ids[2], ContractSide::Consumes, ContractKind::Rpc, "Greeter/SayHello", "stub.SayHello(None)", 7);
    b
}

fn admin(root: &str) -> Builder {
    let (mut b, ids) = repo(ADMIN, root, "admin/app.py", ADMIN_PY, &[("delete_user", 1, 2)]);
    contract(&mut b, ids[0], ContractSide::Provides, ContractKind::Http, "DELETE /users/{}", "/users/{id}", 0);
    b
}

/// Contract keys from the segments; one contract link edge per consumer
/// row to each matching provider row of a dependency (the real linker is
/// `rueca-index`'s).
struct ContractLinker;

impl OrgLinker for ContractLinker {
    fn version(&self) -> u32 {
        9
    }
    fn analyze(&self, _: &Store, seg: &Segment) -> rueca_segment::Result<RepoDeps> {
        Ok(RepoDeps { contracts: ContractKey::of_rows(seg.contracts()), ..Default::default() })
    }
    fn link(
        &self,
        _: &Store,
        consumer: &LinkInput<'_>,
        deps: &[LinkInput<'_>],
    ) -> rueca_segment::Result<Vec<LinkEdge>> {
        let mut out = Vec::new();
        for c in consumer.segment.contracts().iter().filter(|r| r.side == ContractSide::Consumes) {
            for (k, d) in deps.iter().enumerate() {
                let provided = d.segment.contracts().iter().filter(|p| p.side == ContractSide::Provides);
                for p in provided.filter(|p| match_keys(c.kind, &p.key, &c.key).is_some()) {
                    let meta =
                        EdgeMeta { confidence: 0.9, strategy: Strategy::Contract, evidence: Some((c.file, c.span)) };
                    out.push(LinkEdge {
                        src: c.node,
                        dep: k as u32,
                        dst: p.node,
                        kind: EdgeKind::Calls,
                        meta,
                        reversed: false,
                    });
                }
            }
        }
        Ok(out)
    }
}

struct Fix {
    _dir: tempfile::TempDir,
    store: std::path::PathBuf,
    roots: Vec<String>,
}

fn fixture() -> Fix {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let mut roots = Vec::new();
    for name in ["api", "web", "admin"] {
        let p = dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        roots.push(std::fs::canonicalize(&p).unwrap().to_string_lossy().into_owned());
    }
    api(&roots[0]).write(&store, &roots[0]);
    web(&roots[1]).write(&store, &roots[1]);
    admin(&roots[2]).write(&store, &roots[2]);
    let st = Store::open(&store).unwrap();
    let members: Vec<OrgMember> = roots.iter().map(|r| OrgMember { root: r.clone(), ..Default::default() }).collect();
    let up = org::update_org_with(&st, "acme", &members, &ContractLinker).unwrap();
    assert_eq!(up.linked, [(WEB.to_string(), 2)]);
    Fix { _dir: dir, store, roots }
}

fn call(e: &Engine, args: Value) -> Value {
    e.call("list_contracts", args.clone()).unwrap_or_else(|err| panic!("{args}: {err:#}"))
}

/// (repo, symbol, side, key, matches) of each entry, in order.
fn rows(v: &Value) -> Vec<(String, String, String, String, u64)> {
    let s = |e: &Value, k: &str| e[k].as_str().unwrap().to_string();
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (s(e, "repo"), s(e, "symbol"), s(e, "side"), s(e, "key"), e["matches"].as_u64().unwrap()))
        .collect()
}

fn r(repo: &str, symbol: &str, side: &str, key: &str, matches: u64) -> (String, String, String, String, u64) {
    (repo.into(), symbol.into(), side.into(), key.into(), matches)
}

#[test]
fn lists_sides_kinds_and_dangling() {
    let f = fixture();
    let e = Engine::for_org(f.store.clone(), "acme").unwrap();
    let v = call(&e, json!({}));
    assert_eq!(
        rows(&v),
        [
            r(ADMIN, "admin.app.delete_user", "provides", "DELETE /users/{}", 1),
            r(WEB, "web.client.drop_user", "consumes", "DELETE /users/{}", 1),
            r(API, "api.app.get_user", "provides", "GET /users/{}", 1),
            r(WEB, "web.client.load_user", "consumes", "GET /users/{}", 1),
            r(API, "api.app.create_user", "provides", "POST /users", 0),
            r(WEB, "web.client.hello", "consumes", "Greeter/SayHello", 0),
        ]
    );
    let first = &v["results"][1];
    assert_eq!(first["path"], "web/client.py");
    assert_eq!((first["line"].as_u64(), first["label"].as_str()), (Some(5), Some("Function")));
    assert_eq!((first["kind"].as_str(), first["framework"].as_str()), (Some("http"), Some("requests")));
    assert!(first["id"].as_str().unwrap().ends_with("web/client.py#drop_user"), "{first}");
    let snaps: Vec<&str> = v["snapshots"].as_array().unwrap().iter().map(|s| s["repo"].as_str().unwrap()).collect();
    assert_eq!(snaps[first["s"].as_u64().unwrap() as usize], WEB);

    let keys = |args: Value| -> Vec<String> { rows(&call(&e, args)).into_iter().map(|x| x.3).collect() };
    assert_eq!(keys(json!({"side": "provided"})), ["DELETE /users/{}", "GET /users/{}", "POST /users"]);
    assert_eq!(keys(json!({"side": "consumed", "kind": "rpc"})), ["Greeter/SayHello"]);
    assert_eq!(keys(json!({"side": "dangling"})), ["Greeter/SayHello"]);
    assert_eq!(keys(json!({"query": "LOAD_user"})), ["GET /users/{}"], "symbol, case-insensitive");
    assert_eq!(keys(json!({"query": "requests.delete"})), ["DELETE /users/{}"], "raw text");
    assert_eq!(keys(json!({"repo": API})), ["GET /users/{}", "POST /users"]);
    // `repo` narrows the listing, not the counterparts.
    assert_eq!(rows(&call(&e, json!({"repo": WEB, "side": "dangling"})))[0].3, "Greeter/SayHello");

    // A provider the caller cannot see does not count: the call is dangling.
    let hidden = e.with_visibility(Visibility::Repos(BTreeSet::from([API.to_string(), WEB.to_string()])));
    let v = call(&hidden, json!({"side": "dangling"}));
    assert_eq!(rows(&v).into_iter().map(|x| x.3).collect::<Vec<_>>(), ["DELETE /users/{}", "Greeter/SayHello"]);
    assert!(rows(&call(&hidden, json!({}))).iter().all(|x| x.0 != ADMIN));
    let err = format!("{:#}", hidden.call("list_contracts", json!({"repo": ADMIN})).unwrap_err());
    assert!(err.contains("not indexed in organization"), "{err}");

    // Unknown values are argument errors.
    let err = format!(
        "{:#}",
        Engine::for_org(f.store.clone(), "acme").unwrap().call("list_contracts", json!({"side": "both"})).unwrap_err()
    );
    assert!(err.contains("invalid argument `side`"), "{err}");
}

#[test]
fn pages_text_verbose_and_local_mode() {
    let f = fixture();
    let e = Engine::for_org(f.store.clone(), "acme").unwrap();
    let all = rows(&call(&e, json!({})));
    let page1 = call(&e, json!({"limit": 4}));
    assert_eq!((page1["truncated"].as_bool(), page1["next_cursor"].as_str()), (Some(true), Some("4")));
    let page2 = call(&e, json!({"limit": 4, "cursor": "4"}));
    assert_eq!(page2["truncated"], false);
    assert_eq!([rows(&page1), rows(&page2)].concat(), all);

    // Text: one line per entry.
    let text = e.respond("list_contracts", json!({"repo": WEB})).unwrap().text;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "github.com/acme/web@0123456789ab (HEAD)", "{text}");
    assert_eq!(lines[1], "DELETE /users/{}  consumes  requests  github.com/acme/web:web/client.py#drop_user L5  (1)");
    assert_eq!(lines.len(), 4, "{text}");
    // Several snapshots: `[s]` tags.
    let text = e.respond("list_contracts", json!({"side": "provided"})).unwrap().text;
    assert!(
        text.contains("\n[1] GET /users/{}  provides  fastapi  github.com/acme/api:api/app.py#get_user L1  (1)\n"),
        "{text}"
    );

    // Verbose: the flat list, every field.
    let v = call(&e, json!({"format": "verbose", "limit": 1}));
    assert_eq!((v["truncated"].as_bool(), v["next_offset"].as_u64()), (Some(true), Some(1)));
    assert_eq!(v["contracts"][0]["repo"], ADMIN);
    assert_eq!(v["contracts"][0]["ref"], "HEAD");

    // Local mode: the default root alone, counterparts in it only.
    let local = Engine::new(f.store.clone(), Some(f.roots[1].clone().into())).unwrap();
    let v = call(&local, json!({"side": "dangling"}));
    assert_eq!(rows(&v).len(), 3, "{v}");
    let v = call(&local, json!({"repo": "*", "side": "dangling"}));
    assert_eq!(rows(&v).into_iter().map(|x| x.3).collect::<Vec<_>>(), ["Greeter/SayHello"]);
}

#[test]
fn trace_follows_contract_edges() {
    let f = fixture();
    let e = Engine::for_org(f.store.clone(), "acme").unwrap();
    let v = e.call("trace", json!({"qualified_name": "web.client.load_user", "direction": "callees"})).unwrap();
    let nodes = v["results"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2, "{v}");
    assert_eq!(nodes[1]["qn"], "api.app.get_user");
    let edge = &v["results"]["edges"][0];
    assert_eq!(
        (edge[2].as_str(), edge[4].as_str(), edge[5].as_u64()),
        (Some("CALLS"), Some("contract"), Some(2)),
        "{v}"
    );
    let text = e.respond("trace", json!({"qualified_name": "api.app.get_user", "direction": "callers"})).unwrap().text;
    assert!(text.contains("CALLS 0.9 contract @2"), "{text}");
    // `get_symbol` counts stay per repository: link edges are not counted.
    let v = e.call("get_symbol", json!({"qualified_name": "web.client.load_user", "format": "verbose"})).unwrap();
    assert_eq!(v["callees_count"], 0, "{v}");
}
