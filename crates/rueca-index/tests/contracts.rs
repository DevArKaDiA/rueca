//! The organization link layer, contracts (RFC 0006 phase 4) on real
//! working trees: an OpenAPI spec, a `.proto`, a Flask + gRPC server and a
//! Python client in four repositories. Consumer → provider `CALLS` (code
//! and spec), server → spec `IMPLEMENTS`, ambiguity with a provider of the
//! consumer's own repository, dependencies through contracts only, and
//! refs (a provider ref's own link set, incoming edges dropped at a spec
//! ref that removed the operation).

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

const OPENAPI: &str = "openapi: 3.0.0\ninfo:\n  title: Users\n  version: \"1\"\npaths:\n  /users/{id}:\n    get:\n      operationId: getUser\n      responses:\n        \"200\":\n          description: ok\n";
/// The spec at ref `v2`: the operation moved to `/accounts/{id}`.
const OPENAPI_V2: &str = "openapi: 3.0.0\ninfo:\n  title: Users\n  version: \"2\"\npaths:\n  /accounts/{id}:\n    get:\n      operationId: getAccount\n      responses:\n        \"200\":\n          description: ok\n";
const PROTO: &str = "syntax = \"proto3\";\npackage helloworld;\nservice Greeter {\n  rpc SayHello (HelloRequest) returns (HelloReply);\n}\nmessage HelloRequest { string name = 1; }\nmessage HelloReply { string message = 1; }\n";
const SERVER_APP: &str = "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.route(\"/users/<int:uid>\")\ndef get_user(uid):\n    return {}\n\n\n@app.route(\"/items/<item_id>\")\ndef get_item(item_id):\n    return {}\n";
const SERVER_GRPC: &str = "from gen import helloworld_pb2_grpc\n\n\nclass Greeter(helloworld_pb2_grpc.GreeterServicer):\n    def SayHello(self, request, context):\n        return None\n";
const CLIENT: &str = "import requests\nfrom flask import Flask\nfrom gen import helloworld_pb2_grpc\n\napp = Flask(__name__)\n\n\ndef get_user(uid):\n    return requests.get(f\"http://svc/users/{uid}\")\n\n\ndef get_item(item_id):\n    return requests.get(\"http://svc/items/7\")\n\n\ndef missing():\n    return requests.post(\"http://svc/nowhere\")\n\n\ndef hello(channel):\n    stub = helloworld_pb2_grpc.GreeterStub(channel)\n    return stub.SayHello(None)\n\n\n@app.route(\"/items/<item_id>\")\ndef local_item(item_id):\n    return {}\n";

/// (source repo, source qn, target `repo@ref`, target qn, kind, strategy,
/// confidence) of the outgoing link edges of entry `at` seen from `view`.
type Row = (String, String, String, String, String, String, String);

fn edges_at(idx: &OrgIndex, st: &Store, at: OrgSlot, view: &OrgSelection) -> BTreeSet<Row> {
    let g = idx.generation();
    let open = |s: OrgSlot| st.open_segment(&g.entry(s).unwrap().segment_id).unwrap();
    let seg = open(at);
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut out = BTreeSet::new();
    for n in 0..seg.nodes().len() as u32 {
        for e in idx.link_edges(at, n, LinkDir::Out, None, &all, view).unwrap() {
            let dst = open(e.slot);
            out.insert((
                g.repos[at.repo].repo.clone(),
                seg.node(n).qualified_name.clone(),
                format!("{}@{}", g.repos[e.repo].repo, g.ref_name(e.slot).unwrap()),
                dst.node(e.node).qualified_name.clone(),
                e.kind.as_str().to_string(),
                e.meta.strategy.as_str().to_string(),
                format!("{}", e.meta.confidence),
            ));
        }
    }
    out
}

fn row(src: (&str, &str), dst: (&str, &str), kind: &str, confidence: &str) -> Row {
    let s = |x: &str| x.to_string();
    (s(src.0), s(src.1), s(dst.0), s(dst.1), s(kind), s("contract"), s(confidence))
}

const ROUTE: &str = "openapi.yaml:paths./users/{id}.get";

#[test]
fn contracts_link_across_repositories() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    if git_repo(&base.join("api"), &[("openapi.yaml", OPENAPI)], "v2", &[("openapi.yaml", OPENAPI_V2)]).is_none() {
        return;
    }
    let server = [("svc/app.py", SERVER_APP), ("svc/grpc.py", SERVER_GRPC)];
    let moved = SERVER_APP.replace("/users/<int:uid>", "/members/<int:uid>");
    git_repo(&base.join("svc"), &server, "moved", &[("svc/app.py", &moved)]).unwrap();
    write(&base.join("proto"), "greeter.proto", PROTO);
    write(&base.join("client"), "app/users.py", CLIENT);
    let org = "name = \"acme\"\n[[repo]]\npath = \"api\"\nrefs = [\"v2\"]\n[[repo]]\npath = \"client\"\n[[repo]]\npath = \"proto\"\n[[repo]]\npath = \"svc\"\nrefs = [\"moved\"]\n";
    fs::write(base.join("rueca-org.toml"), org).unwrap();
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
    assert_eq!(linked, BTreeSet::from(["local/client", "local/svc", "local/svc@moved"]));

    let idx = OrgIndex::open(&store, "acme").unwrap();
    let st = Store::open(&store).unwrap();
    let g = idx.generation();
    let [api, client, proto, svc] =
        ["local/api", "local/client", "local/proto", "local/svc"].map(|r| g.repo_index(r).unwrap());
    // Dependencies through contracts only: no package is published.
    let deps = |i: usize| -> Vec<String> {
        g.repos[i].default.links.as_ref().map_or(vec![], |l| l.deps.iter().map(|d| d.repo.clone()).collect())
    };
    assert_eq!(deps(client), ["local/api", "local/proto", "local/svc"]);
    assert_eq!(deps(svc), ["local/api", "local/proto"]);
    assert!(deps(api).is_empty() && deps(proto).is_empty());
    assert!(idx.profile(client).unwrap().deps.contracts.iter().any(|k| k.key == "POST /nowhere"));

    let default_view = OrgSelection::default();
    let mut all = edges_at(&idx, &st, OrgSlot::default_of(client), &default_view);
    all.extend(edges_at(&idx, &st, OrgSlot::default_of(svc), &default_view));
    let c = |qn| ("local/client", qn);
    let s = |qn| ("local/svc", qn);
    let expected = BTreeSet::from([
        // Code and spec providers, matched separately: not ambiguous.
        row(c("app.users.get_user"), ("local/svc@main", "svc.app.get_user"), "CALLS", "0.9"),
        row(c("app.users.get_user"), ("local/api@main", ROUTE), "CALLS", "0.9"),
        // A matching route in the client's own repository: ambiguous.
        row(c("app.users.get_item"), ("local/svc@main", "svc.app.get_item"), "CALLS", "0.5"),
        // A package-less stub key: exact against the servicer, template
        // against the `.proto` method.
        row(c("app.users.hello"), ("local/svc@main", "svc.grpc.Greeter.SayHello"), "CALLS", "0.9"),
        row(c("app.users.hello"), ("local/proto@worktree", "helloworld.Greeter.SayHello"), "CALLS", "0.85"),
        // Servers implement the specs published elsewhere.
        row(s("svc.app.get_user"), ("local/api@main", ROUTE), "IMPLEMENTS", "0.9"),
        row(
            s("svc.grpc.Greeter.SayHello"),
            ("local/proto@worktree", "helloworld.Greeter.SayHello"),
            "IMPLEMENTS",
            "0.85",
        ),
    ]);
    assert_eq!(all, expected);

    // The server's ref moved the route: its own link set (no IMPLEMENTS for
    // `get_user`), the default's untouched.
    let svc_moved = g.find_ref(svc, "moved").unwrap();
    assert_ne!(
        g.entry(svc_moved).unwrap().links.as_ref().unwrap().file,
        g.repos[svc].default.links.as_ref().unwrap().file
    );
    let mut moved_view = OrgSelection::default();
    moved_view.set(svc, vec![svc_moved]);
    let at_moved = edges_at(&idx, &st, svc_moved, &moved_view);
    assert!(at_moved.iter().all(|e| e.1 != "svc.app.get_user"), "{at_moved:#?}");
    assert!(at_moved.iter().any(|e| e.1 == "svc.grpc.Greeter.SayHello" && e.4 == "IMPLEMENTS"), "{at_moved:#?}");
    assert_eq!(edges_at(&idx, &st, OrgSlot::default_of(svc), &default_view).len(), 2);

    // The spec's ref removed the operation: in that view the incoming edges
    // into it are dropped; the default keeps them.
    let api_v2 = g.find_ref(api, "v2").unwrap();
    let mut v2_view = OrgSelection::default();
    v2_view.set(api, vec![api_v2]);
    let on_v2 = edges_at(&idx, &st, OrgSlot::default_of(client), &v2_view);
    assert!(on_v2.iter().all(|e| !e.2.starts_with("local/api")), "{on_v2:#?}");
    assert!(on_v2.iter().any(|e| e.3 == "svc.app.get_user"), "{on_v2:#?}");
    let api_seg = st.open_segment(&g.repos[api].default.segment_id).unwrap();
    let route = api_seg.nodes_by_qualified_name(ROUTE)[0];
    let visible: Vec<usize> = (0..g.repos.len()).collect();
    let callers = idx.link_edges(OrgSlot::default_of(api), route, LinkDir::In, None, &visible, &default_view).unwrap();
    assert_eq!(callers.len(), 2, "the client's call and the server's implementation");
    assert!(callers.iter().all(|e| e.meta.strategy == rueca_model::Strategy::Contract));
}
