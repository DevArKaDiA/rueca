//! `rueca status --store-url` against a `file://` service store, filled by
//! the experimental `rueca publish` and mirrored by `rueca pull` (RFC 0008
//! phase 0).

mod common;

use assert_cmd::Command;
use serde_json::Value;

fn rueca(store: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rueca").unwrap();
    c.arg("--store").arg(store).env_remove("RUECA_LOG");
    c
}

fn json_of(cmd: &mut Command) -> Value {
    let out = cmd.arg("--json").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn publish_status_and_pull_through_a_file_store() {
    let project = tempfile::tempdir().unwrap();
    common::write(project.path(), "pkg/models.py", common::MODELS_PY);
    common::write(project.path(), "pkg/service.py", common::SERVICE_PY);
    let store = tempfile::tempdir().unwrap();
    let bucket = tempfile::tempdir().unwrap();
    let url = format!("file://{}", bucket.path().canonicalize().unwrap().display());

    // Not a service store yet.
    let out = rueca(store.path()).args(["status", "--store-url", &url]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a Rueca service store"));

    assert!(rueca(store.path()).arg("index").arg(project.path()).output().unwrap().status.success());
    let published =
        json_of(rueca(store.path()).arg("publish").arg(project.path()).args(["--store-url", &url, "--org", "acme"]));
    assert_eq!(published["duplicate"], false);
    assert_eq!(published["segment_uploaded"], true);
    let repo = published["repo"].as_str().unwrap().to_string();

    let st = json_of(rueca(store.path()).args(["status", "--store-url", &url]));
    assert_eq!(st["org"], "acme");
    assert_eq!(st["repos"][0]["repo"], repo.as_str());
    // A working tree outside git: its default ref is `HEAD`.
    assert_eq!(st["repos"][0]["default_ref"], "HEAD");
    assert_eq!(st["repos"][0]["refs"][0]["kind"], "default");
    assert_eq!(st["repos"][0]["refs"][0]["snapshots"], 1);
    assert_eq!(st["classes"]["segments"]["objects"], 1);
    let text = rueca(store.path()).args(["status", "--store-url", &url]).output().unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("organization acme") && text.contains(&repo) && text.contains("segments"), "{text}");

    // A second local store mirrors it and answers queries.
    let mirror = tempfile::tempdir().unwrap();
    let pulled = json_of(rueca(mirror.path()).args(["pull", "--store-url", &url]));
    assert_eq!(pulled["repos"][0]["segments_downloaded"], 1);
    let hits = json_of(rueca(mirror.path()).args(["query", "search_symbols", r#"{"query": "issue invoice"}"#]));
    assert!(hits.to_string().contains("InvoiceService"), "{hits}");
}

#[test]
fn pull_publishes_a_generation_and_serve_checks_its_config() {
    let project = tempfile::tempdir().unwrap();
    common::write(project.path(), "pkg/models.py", common::MODELS_PY);
    let store = tempfile::tempdir().unwrap();
    let bucket = tempfile::tempdir().unwrap();
    let url = format!("file://{}", bucket.path().canonicalize().unwrap().display());
    assert!(rueca(store.path()).arg("index").arg(project.path()).output().unwrap().status.success());
    json_of(rueca(store.path()).arg("publish").arg(project.path()).args(["--store-url", &url, "--org", "acme"]));

    // `pull --generation` builds the organization on a dedicated mirror and
    // publishes generation 1.
    let builder = tempfile::tempdir().unwrap();
    let pulled = json_of(rueca(builder.path()).args(["pull", "--generation", "--store-url", &url]));
    assert_eq!(pulled["published"]["generation"], 1, "{pulled}");
    let st = json_of(rueca(store.path()).args(["status", "--store-url", &url]));
    assert_eq!(st["generation"], 1, "{st}");

    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("rueca-service.toml");
    let good = format!("[store]\nurl = \"{url}\"\ncache_dir = \"{}\"\n", dir.path().join("cache").display());
    std::fs::write(&config, &good).unwrap();
    let out = rueca(store.path()).args(["serve", "--check-config", "--config"]).arg(&config).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stdout).contains("configuration is valid"));

    let bad = format!("{good}[indexing]\ncredentials = {{ \"github.com\" = \"ghp_literal\" }}\n");
    std::fs::write(&config, bad).unwrap();
    let out = rueca(store.path()).args(["serve", "--check-config"]).env("RUECA_CONFIG", &config).output().unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("environment reference") && !text.contains("configuration is valid"), "{text}");
}
