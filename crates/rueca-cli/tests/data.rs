//! End to end over the structured-data fixtures of `rueca-lang-data` (a
//! small polyglot service: compose, `.env`, Kubernetes, GitHub Actions,
//! OpenAPI, Terraform, Dockerfile, tsconfig, Laravel config, plus Python,
//! TypeScript and PHP code that reads env vars and config keys): `rueca
//! index`, then the tools through `rueca query` (RFC 0003 §6–§7).

mod common;

use std::path::Path;

use serde_json::{Value, json};

use common::*;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

fn shop() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("shop");
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-data/tests/fixtures"), &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

/// `(direction, qualified_name)` of trace nodes.
fn trace_nodes(r: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["direction"].as_str().unwrap().into(), n["qualified_name"].as_str().unwrap().into()))
        .collect();
    v.sort();
    v
}

/// `(src qualified name, kind, dst qualified name, strategy)` of trace edges.
fn trace_edges(r: &Value) -> Vec<(String, String, String, String)> {
    let mut names = std::collections::HashMap::new();
    names.insert(
        r["root"]["id"].as_str().unwrap().to_string(),
        r["root"]["qualified_name"].as_str().unwrap().to_string(),
    );
    for n in r["nodes"].as_array().unwrap() {
        names.insert(n["id"].as_str().unwrap().to_string(), n["qualified_name"].as_str().unwrap().to_string());
    }
    let mut v: Vec<_> = r["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                names[e["src"].as_str().unwrap()].clone(),
                e["kind"].as_str().unwrap().to_string(),
                names[e["dst"].as_str().unwrap()].clone(),
                e["strategy"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    v.sort();
    v
}

fn edge(src: &str, kind: &str, dst: &str, strategy: &str) -> (String, String, String, String) {
    (src.into(), kind.into(), dst.into(), strategy.into())
}

#[test]
fn bridge_env_var_readers_and_providers() {
    let p = shop();
    let report = index(&p);
    assert_eq!(report["files_skipped"], 0, "{report}");
    assert!(report["resolved_by_strategy"]["contract"].as_u64().unwrap() >= 20, "{report}");

    // Who reads DATABASE_URL and who sets it.
    let args =
        json!({"qualified_name": "$env:DATABASE_URL", "direction": "callers", "edge_types": ["READS", "WRITES"]});
    let r = query(&p, "trace", args);
    assert_eq!(r["root"]["id"], "local/shop@worktree:$env#DATABASE_URL");
    assert_eq!(r["root"]["path"], Value::Null);
    let c = |q: &str| ("callers".to_string(), q.to_string());
    assert_eq!(
        trace_nodes(&r),
        [
            c(".env.example:DATABASE_URL"),
            c(".github/workflows/ci.yml:jobs.test.env.DATABASE_URL"),
            c(".github/workflows/ci.yml:jobs.test.steps[\"tests\"].run"),
            c("App\\Branding::title"),
            c("app.main.database_url"),
            c("compose.yaml:services.web.environment.DATABASE_URL"),
            c("docker/Dockerfile:stage[2].DATABASE_URL"),
            c("k8s/web.yaml:Deployment/web.spec.template.spec.containers[\"web\"].env[\"DATABASE_URL\"]"),
            c("web/server:start"),
        ]
    );
    let edges = trace_edges(&r);
    let env = "$env:DATABASE_URL";
    assert!(edges.contains(&edge("app.main.database_url", "READS", env, "contract")), "{edges:?}");
    assert!(edges.contains(&edge("web/server:start", "READS", env, "contract")));
    assert!(edges.contains(&edge(".env.example:DATABASE_URL", "WRITES", env, "syntactic")));
    // compose `DATABASE_URL: ${DATABASE_URL}` passes the variable through.
    let compose = "compose.yaml:services.web.environment.DATABASE_URL";
    assert!(edges.contains(&edge(compose, "READS", env, "contract")));
    assert!(edges.contains(&edge(compose, "WRITES", env, "syntactic")));

    // Counts before tracing.
    let r = query(&p, "get_symbol", json!({"qualified_name": "$env:DATABASE_URL"}));
    assert_eq!(r["symbol"]["label"], "EnvVar");
    assert_eq!(r["readers_count"], 5, "{r}");
    assert_eq!(r["writers_count"], 5, "{r}");
    // Env vars are found by search_symbols by default; their values are not indexed.
    let r = query(&p, "search_symbols", json!({"query": "DATABASE_URL", "limit": 50}));
    let hits: Vec<&Value> = r["results"].as_array().unwrap().iter().collect();
    assert!(hits.iter().any(|h| h["qualified_name"] == "$env:DATABASE_URL"), "{r}");
    assert!(hits.iter().all(|h| !h["signature"].as_str().unwrap_or("").contains("shop:secret")), "{r}");

    // What a function depends on.
    let r =
        query(&p, "trace", json!({"qualified_name": "app.main.main", "direction": "callees", "edge_types": ["READS"]}));
    let names: Vec<String> = trace_nodes(&r).into_iter().map(|(_, q)| q).collect();
    assert_eq!(names, ["$env:API_TOKEN", "$env:LOG_LEVEL", "$env:QUEUE", "$env:SENTRY_DSN"]);
}

#[test]
fn laravel_config_and_config_keys() {
    let p = shop();
    index(&p);
    // config('app.name') → key `name` of config/app.php → env('APP_NAME').
    let args =
        json!({"qualified_name": "App\\Branding::title", "direction": "callees", "edge_types": ["READS"], "depth": 2});
    let r = query(&p, "trace", args);
    let edges = trace_edges(&r);
    assert!(edges.contains(&edge("App\\Branding::title", "READS", "name", "contract")), "{edges:?}");
    assert!(edges.contains(&edge("App\\Branding::title", "READS", "providers.cache.driver", "contract")));
    assert!(edges.contains(&edge("name", "READS", "$env:APP_NAME", "contract")));
    let key = r["nodes"].as_array().unwrap().iter().find(|n| n["qualified_name"] == "name").unwrap();
    assert_eq!(key["path"], "config/app.php");
    assert_eq!(key["label"], "Key");
    // node-config: config.get("server.port") → config/default.json.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "web/server:start", "direction": "callees", "edge_types": ["READS"]}),
    );
    assert!(trace_edges(&r).contains(&edge(
        "web/server:start",
        "READS",
        "config/default.json:server.port",
        "contract"
    )));
    // Spring: a key that reads an env var.
    let r = query(&p, "get_symbol", json!({"qualified_name": "spring.datasource.url"}));
    assert_eq!(r["symbol"]["path"], "src/main/resources/application.yml");
    assert_eq!(r["readers_count"], 0);
}

#[test]
fn data_pointers_imports_and_kinds() {
    let p = shop();
    index(&p);
    let imports = |path: &str| -> Vec<String> {
        let r = query(
            &p,
            "trace",
            json!({"qualified_name": path, "direction": "callees", "edge_types": ["IMPORTS"], "depth": 1}),
        );
        trace_nodes(&r).into_iter().map(|(_, q)| q).collect()
    };
    // GitHub Actions `uses: ./…` from the repository root.
    assert_eq!(
        imports(".github/workflows/ci.yml"),
        [".github/actions/setup/action.yml", ".github/workflows/release.yml"]
    );
    assert_eq!(imports("tsconfig.json"), ["tsconfig.base.json"]);
    // Terraform module source → the module's folder.
    assert_eq!(imports("infra/main.tf"), ["infra/modules/net"]);
    assert_eq!(imports("compose.yaml"), [".env.example", "docker", "docker/Dockerfile"]);
    assert_eq!(imports("api/openapi.yaml"), ["api/common.yaml"]);

    // OpenAPI `$ref` → REFERENCES, also across files.
    let refs = |qn: &str| -> Vec<(String, String, String, String)> {
        let r = query(
            &p,
            "trace",
            json!({"qualified_name": qn, "direction": "callees", "edge_types": ["REFERENCES"], "depth": 1}),
        );
        trace_edges(&r)
    };
    let e = refs("api/openapi.yaml:paths./users/{id}.get.responses.404");
    assert_eq!(e[0].2, "api/common.yaml:components.responses.NotFound", "{e:?}");
    assert_eq!(e[0].3, "import_resolved");
    // Terraform references resolve within the module directory.
    let e = refs("infra/main.tf:aws_s3_bucket.logs");
    let targets: Vec<&str> = e.iter().map(|x| x.2.as_str()).collect();
    assert_eq!(targets, ["infra/main.tf:local.env", "infra/variables.tf:var.prefix"]);
    // Kubernetes: secretKeyRef / configMapKeyRef into another manifest.
    let e = refs("k8s/web.yaml:Deployment/web.spec.template.spec.containers[\"web\"].env[\"LOG_LEVEL\"]");
    assert_eq!(e[0].2, "k8s/config.yaml:ConfigMap/shop-config.data.LOG_LEVEL", "{e:?}");
    // pyproject scripts → the Python function.
    let e = refs("pyproject.toml:project.scripts.shop");
    assert_eq!(e[0].2, "app.main.main");

    // Routes are symbols; keys are searchable with a data language.
    let r = query(&p, "search_symbols", json!({"query": "users"}));
    assert_eq!(r["results"][0]["label"], "Route", "{r}");
    assert_eq!(r["results"][0]["name"], "GET /users/{id}");
    let r = query(&p, "search_symbols", json!({"query": "replicas"}));
    assert!(r["results"].as_array().unwrap().iter().all(|h| h["label"] != "Key"), "{r}");
    let r = query(&p, "search_symbols", json!({"query": "replicas", "language": "yaml"}));
    assert_eq!(r["results"][0]["qualified_name"], "k8s/web.yaml:Deployment/web.spec.replicas", "{r}");
    let r = query(&p, "get_symbol", json!({"qualified_name": "k8s/web.yaml:Deployment/web"}));
    assert_eq!(
        (r["symbol"]["label"].as_str(), r["symbol"]["signature"].as_str()),
        (Some("Resource"), Some("Deployment web (apps/v1)"))
    );

    // Outline of a compose file: services and their keys.
    let r = query(&p, "get_file_outline", json!({"path": "compose.yaml"}));
    assert_eq!(r["language"], "yaml");
    let services = &r["symbols"][0];
    assert_eq!(services["qualified_name"], "compose.yaml:services");
    let names: Vec<&str> =
        services["children"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["web", "worker", "db", "cache"]);
    // Snippets cover the key and its value.
    let r = query(&p, "get_snippet", json!({"qualified_name": "compose.yaml:services.db"}));
    assert_eq!(
        r["text"],
        "  db:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: ${DB_PASSWORD:-postgres}"
    );
}
