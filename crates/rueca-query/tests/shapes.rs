//! The result shapes of `tool_definitions()` (`output_shape`, what code
//! mode's generated API documents) against real answers: a small Python
//! repository with a FastAPI route and a `requests` call is indexed, every
//! tool runs (with the options that add optional fields), and each answer's
//! `results` must match its shape both ways: every field the engine writes
//! is documented, every required field is written, types and enums agree.

use std::collections::BTreeMap;
use std::path::Path;

use rueca_index::{IndexOptions, index_worktree};
use rueca_query::{Engine, tool_definitions};
use serde_json::{Value, json};

const API: &str = r#"from fastapi import FastAPI
import requests

app = FastAPI()


class Base:
    """A base."""


class Service(Base):
    """Serves users."""

    def run(self, uid):
        return get_user(uid)


@app.get("/users/{id}")
def get_user(id):
    """One user."""
    return load_user(id)


def load_user(uid):
    return requests.get(f"http://api/users/{uid}")
"#;

/// Every titled schema of every shape, by title (`$ref` targets).
fn named_shapes() -> BTreeMap<String, Value> {
    fn collect(v: &Value, out: &mut BTreeMap<String, Value>) {
        match v {
            Value::Object(m) => {
                if let (Some(Value::String(t)), Some(_)) = (m.get("title"), m.get("type")) {
                    out.entry(t.clone()).or_insert_with(|| v.clone());
                }
                m.values().for_each(|x| collect(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    for d in tool_definitions() {
        collect(&d.output_shape, &mut out);
    }
    out
}

/// Whether `v` matches `schema` (see `shapes.rs` for the subset).
fn check(schema: &Value, v: &Value, defs: &BTreeMap<String, Value>, at: &str) -> Result<(), String> {
    if let Some(name) = schema.get("$ref").and_then(Value::as_str) {
        return check(&defs[name], v, defs, at);
    }
    if let Some(alts) = schema.get("anyOf").and_then(Value::as_array) {
        let errors: Vec<String> = alts.iter().filter_map(|a| check(a, v, defs, at).err()).collect();
        return if errors.len() < alts.len() { Ok(()) } else { Err(errors.join(" / ")) };
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return if values.contains(v) { Ok(()) } else { Err(format!("{at}: {v} not in {values:?}")) };
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => check_array(schema, v, defs, at),
        Some("object") => check_object(schema, v, defs, at),
        Some(t) => check_scalar(t, v, at),
        None => Ok(()),
    }
}

fn check_scalar(t: &str, v: &Value, at: &str) -> Result<(), String> {
    let ok = match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => false,
    };
    if ok { Ok(()) } else { Err(format!("{at}: {v} is not {t}")) }
}

fn check_array(schema: &Value, v: &Value, defs: &BTreeMap<String, Value>, at: &str) -> Result<(), String> {
    let list = v.as_array().ok_or_else(|| format!("{at}: not an array: {v}"))?;
    let prefix = schema.get("prefixItems").and_then(Value::as_array).cloned().unwrap_or_default();
    for (i, x) in list.iter().enumerate() {
        let at = format!("{at}[{i}]");
        match (prefix.get(i), schema.get("items")) {
            (Some(p), _) => check(p, x, defs, &at)?,
            (None, Some(items)) => check(items, x, defs, &at)?,
            (None, None) => return Err(format!("{at}: tuple longer than documented")),
        }
    }
    if list.len() < prefix.len() {
        return Err(format!("{at}: tuple shorter than documented"));
    }
    Ok(())
}

fn check_object(schema: &Value, v: &Value, defs: &BTreeMap<String, Value>, at: &str) -> Result<(), String> {
    let obj = v.as_object().ok_or_else(|| format!("{at}: not an object: {v}"))?;
    if let Some(values) = schema.get("additionalProperties").filter(|a| a.is_object()) {
        return obj.iter().try_for_each(|(k, x)| check(values, x, defs, &format!("{at}.{k}")));
    }
    let props = schema["properties"].as_object().cloned().unwrap_or_default();
    for (k, x) in obj {
        let p = props.get(k).ok_or_else(|| format!("{at}: field `{k}` is not documented"))?;
        check(p, x, defs, &format!("{at}.{k}"))?;
    }
    for r in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !obj.contains_key(r) {
            return Err(format!("{at}: required field `{r}` is missing"));
        }
    }
    Ok(())
}

fn index(root: &Path, store: &Path) {
    let opts = IndexOptions { root: root.to_path_buf(), store: store.to_path_buf(), ..IndexOptions::default() };
    index_worktree(&opts).unwrap();
}

#[test]
fn every_result_matches_its_shape() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("svc");
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/api.py"), API).unwrap();
    let store = dir.path().join("store");
    index(&root, &store);
    let engine = Engine::new(store, Some(root)).unwrap();
    let defs = named_shapes();
    let shapes: BTreeMap<&str, Value> = tool_definitions().into_iter().map(|d| (d.name, d.output_shape)).collect();
    let calls = [
        ("search_symbols", json!({"query": "user", "scores": true})),
        ("search_symbols", json!({"query": "user", "limit": 1})),
        ("search_code", json!({"pattern": "uid", "context_lines": 1})),
        ("get_symbol", json!({"qualified_name": "Service"})),
        ("get_symbol", json!({"name": "run"})),
        ("get_snippet", json!({"qualified_name": "get_user"})),
        ("get_snippet", json!({"path": "app/api.py", "start_line": 1, "max_lines": 3})),
        ("trace", json!({"qualified_name": "get_user", "direction": "both", "depth": 2})),
        ("trace", json!({"qualified_name": "Service", "edge_types": ["INHERITS"], "direction": "callees"})),
        ("get_file_outline", json!({"path": "app/api.py"})),
        ("list_repositories", json!({})),
        ("list_contracts", json!({})),
    ];
    for (tool, args) in calls {
        let json = engine.call(tool, args.clone()).unwrap_or_else(|e| panic!("{tool} {args}: {e:#}"));
        let results = &json["results"];
        assert!(
            !results.is_null() && results.as_array().is_none_or(|a| !a.is_empty()),
            "{tool} {args}: empty answer: {json}"
        );
        if let Err(e) = check(&shapes[tool], results, &defs, tool) {
            panic!("{tool} {args}: {e}\n{results}");
        }
    }
}
