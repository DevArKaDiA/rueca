//! Shell end to end: `rueca index` on the fixture of `rueca-lang-shell`
//! (an extension-less script found by its `#!` line, sourced libraries, a
//! script run by path, bats tests, a `.env` provider), then the tools
//! through `rueca query`.

mod common;

use std::path::Path;

use serde_json::json;

use common::*;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn shell_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-shell/tests/fixtures/tools");
    copy_dir(&fixture, &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn shell_index_then_every_tool() {
    let p = shell_project();
    index(&p);
    let deploy = "local/proj@worktree:bin/deploy#deploy";

    // `bin/deploy` has no extension: detected by its `#!` line.
    let r = query(&p, "search_symbols", json!({"query": "deploy", "language": "shell"}));
    assert_eq!(r["results"][0]["qualified_name"], "bin/deploy:deploy", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "bin/deploy:deploy"}));
    assert_eq!(r["symbol"]["id"], deploy, "{r}");
    assert_eq!(r["symbol"]["language"], "shell");
    // The dialect ends the signature (`bin/deploy` starts with `#!/usr/bin/env bash`).
    assert_eq!(r["symbol"]["signature"], "deploy() # bash");

    // Callees: its own functions, sourced ones (transitively) and the
    // script it runs; not the ambiguous `pick`.
    let r = query(&p, "trace", json!({"id": deploy, "direction": "callees"}));
    let callees = qns(&r["nodes"]);
    for want in ["bin/deploy:version", "lib/common.sh:log", "lib/net.sh:fetch", "scripts/build.sh"] {
        assert!(callees.contains(&want.to_string()), "{want}: {r}");
    }
    assert!(!callees.iter().any(|c| c.ends_with(":pick") || c.ends_with(":cleanup")), "{r}");
    // The trap handler: a reference.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "bin/deploy:cleanup", "direction": "callers", "edge_types": ["REFERENCES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["bin/deploy:deploy"], "{r}");

    // Who runs the deploy script.
    let r = query(&p, "trace", json!({"qualified_name": "bin/deploy", "direction": "callers"}));
    assert!(qns(&r["nodes"]).contains(&"bin/release.sh:release".to_string()), "{r}");

    // `source` → IMPORTS.
    let r =
        query(&p, "trace", json!({"qualified_name": "bin/deploy", "direction": "callees", "edge_types": ["IMPORTS"]}));
    let imported = qns(&r["nodes"]);
    for want in ["lib/common.sh", "lib/pick_a.sh", "lib/pick_b.sh"] {
        assert!(imported.contains(&want.to_string()), "{want}: {r}");
    }

    // The bridge: the script reads DEPLOY_URL, `.env` sets it.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "$env:DEPLOY_URL", "direction": "callers", "edge_types": ["READS", "WRITES"]}),
    );
    let names = qns(&r["nodes"]);
    for want in ["bin/deploy:deploy", ".env:DEPLOY_URL"] {
        assert!(names.contains(&want.to_string()), "{want}: {r}");
    }
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "$env:DEPLOY_TARGET", "direction": "callers", "edge_types": ["WRITES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["bin/deploy:deploy.DEPLOY_TARGET"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "lib/common.sh"}));
    let outline = qns(&r["symbols"]);
    for want in ["lib/common.sh:APP_VERSION", "lib/common.sh:log"] {
        assert!(outline.contains(&want.to_string()), "{want}: {r}");
    }

    let r = query(&p, "get_snippet", json!({"id": deploy}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("deploy() {"), "{r}");
}
