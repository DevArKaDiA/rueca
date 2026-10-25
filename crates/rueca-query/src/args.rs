//! Tool argument types (the `inputSchema` of every tool) and argument
//! validation.
//!
//! Validation is strict about names and types and lenient about ranges:
//! unknown arguments, missing required arguments and wrong JSON types are
//! errors; out-of-range integers are clamped to the documented bounds.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Language filter accepted by `search_symbols` and `search_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LanguageArg {
    Python,
    Typescript,
    Tsx,
    Javascript,
    Php,
    Markdown,
}

impl LanguageArg {
    pub fn to_language(self) -> rueca_model::Language {
        match self {
            LanguageArg::Python => rueca_model::Language::Python,
            LanguageArg::Typescript => rueca_model::Language::TypeScript,
            LanguageArg::Tsx => rueca_model::Language::Tsx,
            LanguageArg::Javascript => rueca_model::Language::JavaScript,
            LanguageArg::Php => rueca_model::Language::Php,
            LanguageArg::Markdown => rueca_model::Language::Markdown,
        }
    }
}

/// Traversal direction for `trace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DirectionArg {
    /// Who calls the symbol (incoming edges).
    Callers,
    /// What the symbol calls (outgoing edges).
    Callees,
    /// Both directions.
    Both,
}

/// Arguments of `search_symbols`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchSymbolsArgs {
    /// Free text: identifiers, fragments of names (`invoice issue`), camelCase or
    /// snake_case words, path fragments.
    pub query: String,
    /// Restrict to these labels, e.g. `["Function", "Method", "Class"]`. Default: all symbol labels.
    pub labels: Option<Vec<String>>,
    /// Glob on the repository-relative file path, e.g. `src/**/*.py`.
    pub path_glob: Option<String>,
    /// Restrict to one language.
    pub language: Option<LanguageArg>,
    /// Maximum results, 1–200 (default 20).
    pub limit: Option<i64>,
    /// Pagination offset (default 0); pass the previous `next_offset`.
    pub offset: Option<i64>,
    /// Indexed working-tree root path or repository id (`local/<dir>`, `github.com/o/r`).
    /// Default: the server's root, else the only indexed repository.
    pub repo: Option<String>,
}

/// Arguments of `search_code`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchCodeArgs {
    /// Literal text (default) or a regular expression (with `regex: true`).
    pub pattern: String,
    /// Interpret `pattern` as a regular expression (Rust `regex` syntax). Default false.
    pub regex: Option<bool>,
    /// Case-insensitive matching. Default false.
    pub case_insensitive: Option<bool>,
    /// Glob on the repository-relative file path, e.g. `src/**/*.ts`.
    pub path_glob: Option<String>,
    /// Restrict to one language.
    pub language: Option<LanguageArg>,
    /// Maximum matches, 1–500 (default 50).
    pub limit: Option<i64>,
    /// Lines of context before and after each match, 0–10 (default 0).
    pub context_lines: Option<i64>,
    /// Indexed working-tree root path or repository id. Default: the server's root.
    pub repo: Option<String>,
}

/// Arguments of `get_symbol`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetSymbolArgs {
    /// Symbol id as returned by other tools (`<repo>@<commit>:<path>#<name>`). Takes precedence.
    pub id: Option<String>,
    /// Global qualified name (`pkg.mod.Class.method`, `src/util/index:Class.method`,
    /// `App\Models\User::save`) or a suffix of it on `.`/`:`/`/`/`\` boundaries
    /// (`Class.method`, `util:helper`, `User::save`).
    pub qualified_name: Option<String>,
    /// Simple name (`method`). Narrow it with `path` when ambiguous.
    pub name: Option<String>,
    /// Repository-relative file (or directory) path that narrows a `name` lookup.
    pub path: Option<String>,
    /// Indexed working-tree root path or repository id. Default: the server's root.
    pub repo: Option<String>,
}

/// Arguments of `get_snippet`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetSnippetArgs {
    /// Symbol id (`<repo>@<commit>:<path>#<name>`).
    pub id: Option<String>,
    /// Global qualified name or a suffix of it (`Class.method`).
    pub qualified_name: Option<String>,
    /// Simple name; narrowed by `path`.
    pub name: Option<String>,
    /// Repository-relative file path. With `start_line` (and optional `end_line`)
    /// selects a line range instead of a symbol; with `name` narrows the lookup.
    pub path: Option<String>,
    /// First line (1-based) of the range to return when no symbol is given.
    pub start_line: Option<i64>,
    /// Last line (1-based, inclusive). Default: end of file (bounded by `max_lines`).
    pub end_line: Option<i64>,
    /// Extra lines before and after, 0–50 (default 0).
    pub context_lines: Option<i64>,
    /// Maximum lines returned, 1–2000 (default 300); longer ranges set `truncated`.
    pub max_lines: Option<i64>,
    /// Indexed working-tree root path or repository id. Default: the server's root.
    pub repo: Option<String>,
}

/// Arguments of `trace`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TraceArgs {
    /// Symbol id of the root.
    pub id: Option<String>,
    /// Global qualified name of the root, or a suffix of it.
    pub qualified_name: Option<String>,
    /// Simple name of the root; narrowed by `path`.
    pub name: Option<String>,
    /// Repository-relative path that narrows a `name` lookup.
    pub path: Option<String>,
    /// `callers`, `callees` or `both` (default).
    pub direction: Option<DirectionArg>,
    /// Maximum hops from the root, 1–10 (default 2).
    pub depth: Option<i64>,
    /// Edge kinds to follow (default `["CALLS"]`), e.g. `["CALLS", "INHERITS"]`.
    pub edge_types: Option<Vec<String>>,
    /// Ignore edges below this confidence, 0.0–1.0 (default 0.0).
    pub min_confidence: Option<f64>,
    /// Maximum nodes returned, 1–2000 (default 200); hitting it sets `truncated`.
    pub limit: Option<i64>,
    /// Only report and traverse neighbours with these labels, e.g. `["Function", "Method"]`.
    /// Default: every label (a `File` caller is module-level code, a `Class` callee a constructor call).
    pub labels: Option<Vec<String>>,
    /// Include tests (test functions, `describe`/`it` blocks) as neighbours. Default false,
    /// unless `edge_types` contains `TESTS`.
    pub include_tests: Option<bool>,
    /// Indexed working-tree root path or repository id. Default: the server's root.
    pub repo: Option<String>,
}

/// Arguments of `get_file_outline`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetFileOutlineArgs {
    /// Repository-relative file path, e.g. `src/billing/service.py`.
    pub path: String,
    /// Indexed working-tree root path or repository id. Default: the server's root.
    pub repo: Option<String>,
}

/// Generates the JSON schema of an argument type as a plain JSON object
/// (`type: object`, `properties`, `required`), without `$schema`.
pub(crate) fn input_schema<T: JsonSchema>() -> Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true);
    let mut v = settings.into_generator().into_root_schema_for::<T>().to_value();
    if let Value::Object(m) = &mut v {
        m.remove("$schema");
        m.remove("title");
        m.insert("additionalProperties".into(), Value::Bool(false));
        if let Some(Value::Object(props)) = m.get_mut("properties") {
            props.values_mut().for_each(drop_null_variant);
        }
    }
    v
}

/// Optional arguments are expressed by omission; `null` is accepted at run
/// time but not advertised, which keeps schemas simple for LLM clients
/// (`"type": "integer"` rather than `"type": ["integer", "null"]`).
fn drop_null_variant(prop: &mut Value) {
    let Value::Object(m) = prop else { return };
    if let Some(Value::Array(types)) = m.get("type") {
        let rest: Vec<Value> = types.iter().filter(|t| *t != "null").cloned().collect();
        let new = if rest.len() == 1 { rest[0].clone() } else { Value::Array(rest) };
        m.insert("type".into(), new);
    }
    if let Some(Value::Array(any)) = m.get("anyOf") {
        let rest: Vec<Value> = any.iter().filter(|s| s.get("type").is_none_or(|t| t != "null")).cloned().collect();
        if rest.len() == 1 {
            m.remove("anyOf");
            if let Value::Object(inner) = &rest[0] {
                for (k, v) in inner {
                    m.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
    }
}

/// Validates `args` against `schema` (unknown and missing arguments) and
/// deserializes it, reporting the offending argument on type errors.
pub(crate) fn parse_args<T: DeserializeOwned>(tool: &str, schema: &Value, args: Value) -> anyhow::Result<T> {
    let mut obj = match args {
        Value::Null => serde_json::Map::new(),
        Value::Object(m) => m,
        other => anyhow::bail!("arguments of `{tool}` must be a JSON object, got {}", json_type(&other)),
    };
    let props = schema.get("properties").and_then(Value::as_object);
    let known: Vec<&str> = props.map(|p| p.keys().map(String::as_str).collect()).unwrap_or_default();
    let mut unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !known.contains(k)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        anyhow::bail!(
            "unknown argument{} {} for `{tool}` (valid arguments: {})",
            if unknown.len() > 1 { "s" } else { "" },
            unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
            known.join(", ")
        );
    }
    // `null` means "not given".
    obj.retain(|_, v| !v.is_null());
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        for r in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(r) {
                anyhow::bail!("missing required argument `{r}` for `{tool}`");
            }
        }
    }
    let value = Value::Object(obj);
    serde_path_to_error::deserialize::<_, T>(&value).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.into_inner();
        if path.is_empty() || path == "." {
            anyhow::anyhow!("invalid arguments for `{tool}`: {inner}")
        } else {
            anyhow::anyhow!("invalid argument `{path}` for `{tool}`: {inner}")
        }
    })
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Clamps an optional integer argument to `[min, max]`, defaulting to `default`.
pub(crate) fn clamp_int(v: Option<i64>, default: i64, min: i64, max: i64) -> i64 {
    v.unwrap_or(default).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_is_plain_object() {
        let s = input_schema::<SearchSymbolsArgs>();
        assert_eq!(s["type"], "object");
        assert!(s.get("$schema").is_none());
        assert_eq!(s["required"], json!(["query"]));
        assert_eq!(s["properties"]["limit"]["type"], "integer");
        let t = input_schema::<TraceArgs>();
        assert!(t.get("$defs").is_none());
        assert_eq!(t["properties"]["direction"]["oneOf"].as_array().unwrap().len(), 3);
        assert_eq!(t["properties"]["edge_types"]["type"], "array");
    }

    #[test]
    fn rejects_unknown_and_missing() {
        let s = input_schema::<SearchSymbolsArgs>();
        let e = parse_args::<SearchSymbolsArgs>("search_symbols", &s, json!({"query": "x", "bogus": 1})).unwrap_err();
        assert!(e.to_string().contains("unknown argument `bogus`"), "{e}");
        let e = parse_args::<SearchSymbolsArgs>("search_symbols", &s, json!({})).unwrap_err();
        assert!(e.to_string().contains("missing required argument `query`"), "{e}");
        let e =
            parse_args::<SearchSymbolsArgs>("search_symbols", &s, json!({"query": "x", "limit": "ten"})).unwrap_err();
        assert!(e.to_string().contains("invalid argument `limit`"), "{e}");
        let e = parse_args::<SearchSymbolsArgs>("search_symbols", &s, json!([1])).unwrap_err();
        assert!(e.to_string().contains("must be a JSON object"), "{e}");
        let a = parse_args::<SearchSymbolsArgs>("search_symbols", &s, json!({"query": "x", "limit": null})).unwrap();
        assert_eq!(a.limit, None);
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp_int(None, 20, 1, 200), 20);
        assert_eq!(clamp_int(Some(0), 20, 1, 200), 1);
        assert_eq!(clamp_int(Some(9999), 20, 1, 200), 200);
    }
}
