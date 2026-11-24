//! `rueca models` and `rueca embed` without weights: the presets listing,
//! the model record (computed from the manifest, no model loaded) and the
//! error that names `rueca models pull`. Nothing here downloads anything.

use assert_cmd::Command;
use serde_json::Value;

fn rueca(store: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rueca").unwrap();
    c.arg("--store").arg(store).env_remove("RUECA_LOG").env_remove("RUECA_MODEL_DIR").env_remove("RUECA_EMBED_PRESET");
    c
}

fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn models_list_shows_the_qwen3_presets_and_an_empty_cache() {
    let dir = tempfile::tempdir().unwrap();
    let out = rueca(dir.path()).args(["models", "list", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = stdout_json(&out);
    let names: Vec<&str> = v["presets"].as_array().unwrap().iter().map(|p| p["preset"].as_str().unwrap()).collect();
    assert_eq!(names, ["qwen3-0.6b-q8", "qwen3-0.6b-f16", "qwen3-4b-q4", "qwen3-4b-q8", "qwen3-8b-q4", "qwen3-8b-q8"]);
    assert!(v["presets"].as_array().unwrap().iter().all(|p| p["cache"] == "missing"));
    assert_eq!(v["models_dir"], dir.path().join("models").to_str().unwrap());
    assert!(!dir.path().join("models").exists(), "listing creates nothing");
}

#[test]
fn embed_info_describes_the_default_preset_without_weights() {
    let dir = tempfile::tempdir().unwrap();
    let out = rueca(dir.path()).args(["embed", "--info", "--max-tokens", "256"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = stdout_json(&out);
    assert_eq!(v["preset"], "qwen3-0.6b-q8");
    assert_eq!(v["dims"], 1024);
    assert_eq!((v["token_budget"].as_u64(), v["query_token_budget"].as_u64()), (Some(256), Some(512)));
    assert_eq!(v["model_key"].as_str().map(str::len), Some(64));
    assert!(v["load_ms"].is_null());
}

#[test]
fn embed_without_weights_names_the_pull_command() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rueca.toml"), "[retrieval]\npreset = \"qwen3-0.6b-f16\"\n").unwrap();
    let out = rueca(dir.path()).args(["embed", "--text", "fn main() {}"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("rueca models pull qwen3-0.6b-f16"), "{err}");
    assert!(!dir.path().join("models").exists(), "nothing was downloaded");
    let out = rueca(dir.path()).args(["embed", "--preset", "nope", "--info"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown preset `nope`"));
}
