//! Hybrid search end to end with the real default model (RFC 0007 phase 1):
//! `rueca index --embed` on a small repository, then `search_symbols` in
//! every mode. Runs only with `RUECA_EMBED_TESTS=1` and the `qwen3-0.6b-q8`
//! weights pulled (`rueca models pull`; `$RUECA_MODEL_DIR`, else
//! `~/.cache/rueca/models`); otherwise it passes without running.

mod common;

use std::path::PathBuf;

use common::{rueca, write};
use serde_json::{Value, json};

/// The question's words (`configuration`, `file`, `parsed`, `where`) are
/// the decoys' identifiers; the function that parses the configuration
/// file is `load_settings`, which shares none of them.
const FILES: &[(&str, &str)] = &[
    (
        "app/settings.py",
        "import tomllib\n\n\ndef load_settings(path):\n    with open(path, \"rb\") as fh:\n        data = tomllib.load(fh)\n    return Settings(**data)\n",
    ),
    (
        "app/ui.py",
        "def configuration_panel_title(lang):\n    return {\"en\": \"Configuration\", \"es\": \"Configuración\"}[lang]\n\n\ndef file_menu_items():\n    return [\"New file\", \"Open file\", \"Save file\"]\n\n\ndef where_is_the_cursor(editor):\n    return editor.cursor\n",
    ),
    (
        "app/cli.py",
        "def parsed_args_count(parsed):\n    return len(parsed)\n\n\ndef print_banner(out):\n    print(\"app\", file=out)\n",
    ),
    (
        "app/sql.py",
        "def where_clause(column, value):\n    return f\"WHERE {column} = {value!r}\"\n\n\ndef file_size(path):\n    import os\n    return os.path.getsize(path)\n",
    ),
    ("app/math.py", "def add(a, b):\n    return a + b\n\n\ndef mean(xs):\n    return sum(xs) / len(xs)\n"),
];

const QUESTION: &str = "where is the configuration file parsed";

fn models_dir() -> Option<PathBuf> {
    if std::env::var("RUECA_EMBED_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipped: set RUECA_EMBED_TESTS=1 with the qwen3-0.6b-q8 weights pulled");
        return None;
    }
    std::env::var_os("RUECA_MODEL_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/rueca/models")))
}

/// Function names in rank order (results are grouped by file).
fn ranked(r: &Value) -> Vec<String> {
    let mut v: Vec<(f64, String)> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["symbols"].as_array().unwrap().iter())
        .map(|s| (s["score"].as_f64().unwrap(), s["name"].as_str().unwrap().to_string()))
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().map(|(_, n)| n).collect()
}

#[test]
fn natural_language_query_with_the_default_model() {
    let Some(models) = models_dir() else { return };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    for (p, text) in FILES {
        write(&root, p, text);
    }
    let store = dir.path().join("store");
    let index = |args: &[&str]| -> Value {
        let out = rueca(&store)
            .env("RUECA_MODEL_DIR", &models)
            .env_remove("RUECA_EMBED_PRESET")
            .arg("--json")
            .arg("index")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let cold = index(&["--embed"]);
    let v = &cold["vectors"];
    assert_eq!((v["preset"].as_str(), v["publications"].as_u64()), (Some("qwen3-0.6b-q8"), Some(2)), "{v}");
    assert_eq!(v["units"], v["embedded"]);
    let warm = index(&["--embed"]);
    assert_eq!((warm["vectors"]["embedded"].as_u64(), warm["vectors"]["publications"].as_u64()), (Some(0), Some(1)));

    // `rueca status` names the preset that embedded the snapshot.
    let out = rueca(&store).args(["--json", "status"]).output().unwrap();
    let st: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(st["roots"][0]["vectors"]["preset"], "qwen3-0.6b-q8", "{st}");
    let out = rueca(&store).arg("status").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("VECTORS") && text.contains("qwen3-0.6b-q8"), "{text}");

    let query_with = |extra: Value| -> Value {
        let mut args = json!({"query": QUESTION, "limit": 5, "scores": true, "labels": ["Function"]});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let out = rueca(&store)
            .env("RUECA_MODEL_DIR", &models)
            .args(["--root"])
            .arg(&root)
            .args(["query", "search_symbols", &args.to_string()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let query = |mode: Option<&str>| query_with(mode.map_or_else(|| json!({}), |m| json!({"mode": m})));
    let (hybrid, semantic, lexical) = (query(None), query(Some("semantic")), query(Some("lexical")));
    let rank = |r: &Value| ranked(r).iter().position(|n| n == "load_settings").map(|i| i + 1);
    eprintln!(
        "load_settings rank: hybrid {:?}, semantic {:?}, lexical {:?}\nhybrid {:?}\nsemantic {:?}\nlexical {:?}",
        rank(&hybrid),
        rank(&semantic),
        rank(&lexical),
        ranked(&hybrid),
        ranked(&semantic),
        ranked(&lexical)
    );
    assert_eq!(hybrid["retrieval"]["mode"], "hybrid", "{}", hybrid["retrieval"]);
    assert!(hybrid["retrieval"]["model"].as_str().unwrap().starts_with("qwen3-0.6b-q8:"));
    assert_eq!(semantic["retrieval"]["mode"], "semantic");
    assert_eq!(lexical["retrieval"], json!({"mode": "lexical"}));
    assert!(rank(&hybrid).is_some_and(|r| r <= 5), "hybrid top 5: {:?}", ranked(&hybrid));
    assert!(rank(&semantic).is_some_and(|r| r <= 5), "semantic top 5: {:?}", ranked(&semantic));
    assert!(rank(&lexical).is_none(), "BM25 alone should miss it: {:?}", ranked(&lexical));

    // The reranker stage, when its weights were imported
    // (`rueca models import qwen3-reranker-0.6b-q8 <file>`).
    let reranker = models.join("qwen3-reranker-0.6b-q8/Qwen3-Reranker-0.6B-Q8_0.gguf");
    if !reranker.exists() {
        eprintln!("reranker part skipped: no qwen3-reranker-0.6b-q8 weights in {}", models.display());
        return;
    }
    let reranked = query_with(json!({"rerank": true, "rerank_k": 20}));
    let r = &reranked["retrieval"]["reranker"];
    eprintln!("reranked {:?} ({r})", ranked(&reranked));
    assert_eq!(r["preset"], "qwen3-reranker-0.6b-q8", "{}", reranked["retrieval"]);
    assert_eq!(ranked(&reranked)[0], "load_settings", "{:?}", ranked(&reranked));
}
