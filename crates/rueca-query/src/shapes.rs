//! Result shapes of the tools: a JSON Schema of the envelope's `results`
//! (format `compact`) per tool, written once next to the builders in
//! [`crate::compact`] and served as [`crate::ToolDef::output_shape`].
//!
//! MCP clients never see them (the MCP layer lists names, descriptions and
//! input schemas only). Code mode (RFC 0009) generates its script API from
//! them: TypeScript result types and the Starlark reference. A test runs
//! every tool on an indexed fixture and checks each answer against its
//! shape in both directions (every field the engine writes is documented,
//! every required field is written), so a change of a builder that is not
//! reflected here fails.
//!
//! Conventions, kept to what the generators understand: `type` (`string`,
//! `integer`, `number`, `boolean`, `null`, `array`, `object`), `anyOf`,
//! `enum`, `properties` + `required`, `additionalProperties` (a map),
//! `items`, `prefixItems` (a tuple, each element may carry a `title`, the
//! element's label), `description`, and `title` on a reused object (a named
//! type; `{"$ref": "<title>"}` refers to it, recursion included).

use serde_json::{Map, Value, json};

fn ty(name: &str) -> Value {
    json!({ "type": name })
}

fn string() -> Value {
    ty("string")
}

fn int() -> Value {
    ty("integer")
}

fn number() -> Value {
    ty("number")
}

fn boolean() -> Value {
    ty("boolean")
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn or_null(v: Value) -> Value {
    json!({ "anyOf": [v, ty("null")] })
}

fn named(name: &str) -> Value {
    json!({ "$ref": name })
}

fn doc(mut v: Value, text: &str) -> Value {
    v["description"] = Value::String(text.to_string());
    v
}

/// An object of `fields`; a name ending in `?` is optional.
fn object(fields: &[(&str, Value)]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for (name, schema) in fields {
        let key = match name.strip_suffix('?') {
            Some(k) => k,
            None => {
                required.push(Value::String((*name).to_string()));
                name
            }
        };
        props.insert(key.to_string(), schema.clone());
    }
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn titled(name: &str, mut v: Value) -> Value {
    v["title"] = Value::String(name.to_string());
    v
}

fn snapshot() -> Value {
    doc(int(), "snapshot index")
}

fn lines() -> Value {
    doc(json!({ "type": "array", "prefixItems": [int(), int()] }), "[start, end], 1-based, inclusive")
}

/// `in` / `changed` of an item of a repository read at several refs.
fn marks() -> [(&'static str, Value); 2] {
    [
        ("in?", doc(array(int()), "snapshots it is in (several refs only)")),
        ("changed?", doc(boolean(), "not identical in every requested ref")),
    ]
}

fn with(base: &[(&str, Value)], more: &[(&str, Value)]) -> Vec<(String, Value)> {
    base.iter().chain(more).map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn object_of(fields: Vec<(String, Value)>) -> Value {
    let borrowed: Vec<(&str, Value)> = fields.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    object(&borrowed)
}

fn brief() -> Value {
    titled(
        "Brief",
        object(&[
            ("id", doc(string(), "pinned symbol id")),
            ("qn", doc(string(), "qualified name")),
            ("label", string()),
            ("path?", string()),
            ("lines?", lines()),
        ]),
    )
}

fn symbol_item() -> Value {
    let base = [
        ("id", doc(string(), "pinned symbol id")),
        ("name", string()),
        ("qn", doc(string(), "qualified name")),
        ("label", doc(string(), "Function, Method, Class, …")),
        ("lines?", lines()),
        ("sig?", doc(string(), "signature")),
        ("score?", doc(number(), "with `scores: true`")),
    ];
    titled("SymbolItem", object_of(with(&base, &marks())))
}

fn outline_symbol() -> Value {
    let base = [
        ("id", string()),
        ("name", string()),
        ("qn", string()),
        ("label", string()),
        ("lines?", lines()),
        ("sig?", string()),
        ("children?", array(named("OutlineSymbol"))),
    ];
    titled("OutlineSymbol", object(&base))
}

fn symbol_detail() -> Value {
    titled(
        "SymbolDetail",
        object(&[
            ("id", string()),
            ("name", string()),
            ("qn", string()),
            ("label", string()),
            ("path?", string()),
            ("lines?", lines()),
            ("lang?", string()),
            ("sig?", string()),
            ("doc?", string()),
            ("exported", boolean()),
            ("test", boolean()),
        ]),
    )
}

fn search_symbols() -> Value {
    array(object(&[("s", snapshot()), ("path", or_null(string())), ("symbols", array(symbol_item()))]))
}

fn search_code() -> Value {
    let hit = titled(
        "CodeMatch",
        object(&[
            ("line", int()),
            ("text", string()),
            ("before?", doc(array(string()), "context lines")),
            ("after?", doc(array(string()), "context lines")),
            ("symbol?", doc(named("Brief"), "innermost enclosing definition")),
        ]),
    );
    array(object(&[("s", snapshot()), ("path", string()), ("matches", array(hit))]))
}

fn get_symbol() -> Value {
    let version = object_of(with(
        &[("s", snapshot()), ("symbol", symbol_detail()), ("callers", int()), ("callees", int())],
        &marks(),
    ));
    let base = [
        ("s", snapshot()),
        ("symbol", symbol_detail()),
        ("parent", or_null(brief())),
        ("children", array(named("Brief"))),
        ("bases", array(named("Brief"))),
        ("subtypes", array(named("Brief"))),
        ("callers", doc(int(), "caller count")),
        ("callees", doc(int(), "callee count")),
        ("readers?", int()),
        ("writers?", int()),
        ("candidates", doc(array(named("Brief")), "other matches of an ambiguous lookup")),
        ("versions?", array(version)),
    ];
    object_of(with(&base, &marks()))
}

fn get_snippet() -> Value {
    object(&[
        ("s", snapshot()),
        ("path", string()),
        ("lang?", string()),
        ("lines", lines()),
        ("text", string()),
        ("symbol?", named("Brief")),
    ])
}

fn trace() -> Value {
    let dir = doc(json!({ "enum": ["in", "out", "both"] }), "in = caller, out = callee");
    let node = titled(
        "TraceNode",
        object_of(with(
            &[
                ("s?", doc(snapshot(), "when the walk crossed repositories")),
                ("id", string()),
                ("qn", string()),
                ("label", string()),
                ("path?", string()),
                ("lines?", lines()),
                ("d", doc(int(), "depth; the root (node 0) is 0")),
                ("dir?", dir),
            ],
            &marks(),
        )),
    );
    let tail = json!({ "anyOf": [string(), object(&marks())] });
    let edge = titled(
        "TraceEdge",
        doc(
            json!({
                "type": "array",
                "prefixItems": [
                    titled("src", int()),
                    titled("dst", int()),
                    titled("kind", string()),
                    titled("confidence", number()),
                    titled("strategy", string()),
                    titled("line", or_null(int())),
                ],
                "items": tail,
            }),
            "src calls dst (node indexes); then the call-site path when not in src's file, then {in, changed} with several refs",
        ),
    );
    object(&[("s", snapshot()), ("nodes", array(node)), ("edges", array(edge)), ("tests_excluded", int())])
}

fn get_file_outline() -> Value {
    let version = object_of(with(
        &[("s", snapshot()), ("line_count", int()), ("symbols", array(named("OutlineSymbol")))],
        &marks(),
    ));
    let base = [
        ("s", snapshot()),
        ("path", string()),
        ("lang?", string()),
        ("line_count", int()),
        ("symbols", array(outline_symbol())),
        ("versions?", array(version)),
    ];
    object_of(with(&base, &marks()))
}

fn list_repositories() -> Value {
    let reference = object(&[("ref", string()), ("commit", doc(string(), "12 hex digits"))]);
    array(titled(
        "Repository",
        object(&[
            ("s", snapshot()),
            ("repo", string()),
            ("default_ref", string()),
            ("refs", array(reference)),
            ("languages", doc(json!({ "type": "object", "additionalProperties": int() }), "files per language")),
            ("files", int()),
            ("symbols", int()),
            ("edges", int()),
            ("packages?", array(string())),
            ("packages_more?", int()),
            ("depends_on?", array(string())),
            ("score?", doc(number(), "relevance to `query`, 0–1")),
        ]),
    ))
}

fn list_contracts() -> Value {
    array(titled(
        "Contract",
        object(&[
            ("s", snapshot()),
            ("repo", string()),
            ("path", or_null(string())),
            ("line", int()),
            ("symbol", doc(string(), "qualified name of the declaring / calling symbol")),
            ("id", string()),
            ("label", string()),
            ("kind", json!({ "enum": ["http", "rpc", "topic"] })),
            ("side", json!({ "enum": ["provides", "consumes"] })),
            ("key", doc(string(), "canonical key: `GET /users/{}`, `pkg.Service/Method`")),
            ("framework", string()),
            ("raw", string()),
            ("matches", doc(int(), "counterparts in the visible repositories")),
        ]),
    ))
}

/// The shape of `tool`'s `results` (`null` for an unknown tool).
pub(crate) fn output_shape(tool: &str) -> Value {
    match tool {
        "search_symbols" => search_symbols(),
        "search_code" => search_code(),
        "get_symbol" => get_symbol(),
        "get_snippet" => get_snippet(),
        "trace" => trace(),
        "get_file_outline" => get_file_outline(),
        "list_repositories" => list_repositories(),
        "list_contracts" => list_contracts(),
        _ => Value::Null,
    }
}
