//! `rueca contracts` and contract edges in `rueca links` (RFC 0006 phase
//! 4) end to end: an OpenAPI spec and a Python client in an organization,
//! and the same listing in local mode.

mod common;

use assert_cmd::Command;
use serde_json::Value;

use common::write;

fn rueca(store: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rueca").unwrap();
    c.arg("--store").arg(store).env_remove("RUECA_LOG");
    c
}

fn run(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn json(c: &mut Command) -> Value {
    let s = run(c);
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("{e}: {s}"))
}

const OPENAPI: &str = "openapi: 3.0.0\ninfo:\n  title: Users\n  version: \"1\"\npaths:\n  /users/{id}:\n    get:\n      operationId: getUser\n      responses:\n        \"200\":\n          description: ok\n";
const CLIENT: &str = "import requests\n\n\ndef get_user(uid):\n    return requests.get(f\"http://svc/users/{uid}\")\n\n\ndef missing():\n    return requests.post(\"http://svc/nowhere\")\n";

/// (repo, side, key, matches) of every entry.
fn entries(v: &Value) -> Vec<(String, String, String, u64)> {
    let s = |e: &Value, k: &str| e[k].as_str().unwrap_or_default().to_string();
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (s(e, "repo"), s(e, "side"), s(e, "key"), e["matches"].as_u64().unwrap()))
        .collect()
}

#[test]
fn contracts_and_contract_links() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    write(&base.join("api"), "openapi.yaml", OPENAPI);
    write(&base.join("client"), "app/users.py", CLIENT);
    let file = base.join("rueca-org.toml");
    std::fs::write(&file, "name = \"acme\"\n[[repo]]\npath = \"api\"\n[[repo]]\npath = \"client\"\n").unwrap();
    run(rueca(&store).args(["index", "--org"]).arg(&file));

    // Every entry, every page, in key order.
    let v = json(rueca(&store).args(["contracts", "--org", "acme", "--json"]));
    let pair = |repo: &str, side: &str, key: &str, n: u64| (repo.to_string(), side.to_string(), key.to_string(), n);
    assert_eq!(
        entries(&v),
        [
            pair("local/api", "provides", "GET /users/{}", 1),
            pair("local/client", "consumes", "GET /users/{}", 1),
            pair("local/client", "consumes", "POST /nowhere", 0),
        ]
    );
    assert_eq!((v["truncated"].as_bool(), v["snapshots"].as_array().unwrap().len()), (Some(false), 2));
    let e = &v["results"][1];
    assert_eq!(
        (e["path"].as_str(), e["symbol"].as_str(), e["framework"].as_str()),
        (Some("app/users.py"), Some("app.users.get_user"), Some("requests"))
    );
    assert_eq!(v["snapshots"][e["s"].as_u64().unwrap() as usize]["repo"], "local/client");

    // Filters and the limit; pages merged into one envelope.
    let dangling = json(rueca(&store).args(["contracts", "--org"]).arg(&file).args(["--side", "dangling", "--json"]));
    assert_eq!(entries(&dangling), [pair("local/client", "consumes", "POST /nowhere", 0)]);
    let provided =
        json(rueca(&store).args(["contracts", "--org", "acme", "--side", "provided", "--kind", "http", "--json"]));
    assert_eq!(entries(&provided), [pair("local/api", "provides", "GET /users/{}", 1)]);
    assert!(entries(&json(rueca(&store).args(["contracts", "--org", "acme", "--kind", "rpc", "--json"]))).is_empty());
    let one = json(rueca(&store).args(["contracts", "--org", "acme", "--limit", "1", "--json"]));
    assert_eq!(
        (entries(&one).len(), one["truncated"].as_bool(), one["next_cursor"].as_str()),
        (1, Some(true), Some("1"))
    );
    let q = json(rueca(&store).args([
        "contracts",
        "--org",
        "acme",
        "--query",
        "nowhere",
        "--repo",
        "local/client",
        "--json",
    ]));
    assert_eq!(entries(&q).len(), 1);

    // Text: one line per entry.
    let text = run(rueca(&store).args(["contracts", "--org", "acme", "--side", "consumed"]));
    assert!(text.contains("POST /nowhere  consumes  requests  local/client:app/users.py#missing L9  (0)\n"), "{text}");
    assert!(
        run(rueca(&store).args(["contracts", "--org", "acme", "--json", "--text"]))
            .contains("GET /users/{}  provides  openapi")
    );
    // The tool through `query`.
    let v = json(rueca(&store).args(["query", "list_contracts", r#"{"side": "dangling"}"#, "--org", "acme"]));
    assert_eq!(entries(&v).len(), 1);
    // Unknown values are errors.
    let out = rueca(&store).args(["contracts", "--org", "acme", "--side", "nope"]).output().unwrap();
    assert!(!out.status.success());

    // `links`: the contract edge, its target's name and label.
    let v = json(rueca(&store).args(["links", "--org", "acme", "--json"]));
    let edges = v["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1, "{v}");
    let e = &edges[0];
    assert_eq!((e["src"].as_str(), e["dst_repo"].as_str()), (Some("app.users.get_user"), Some("local/api")));
    assert_eq!((e["dst_label"].as_str(), e["dst_name"].as_str()), (Some("Route"), Some("GET /users/{id}")));
    assert_eq!(
        (e["via"].as_str(), e["strategy"].as_str(), e["key"].as_str()),
        (Some("contract"), Some("contract"), Some("GET /users/{}"))
    );
    assert_eq!(
        (e["confidence"].as_f64(), e["path"].as_str(), e["line"].as_u64()),
        (Some(0.9), Some("app/users.py"), Some(5))
    );
    let text = run(rueca(&store).args(["links", "--org", "acme"]));
    assert!(text.contains("CALLS contract 0.9  [contract GET /users/{}]"), "{text}");

    // Local mode: one root, its own counterparts only.
    let v = json(rueca(&store).arg("--root").arg(base.join("client")).args(["contracts", "--json"]));
    assert_eq!(entries(&v).iter().filter(|e| e.3 == 0).count(), 2, "{v}");
}
