//! The script API reference `describe_api` returns, generated from
//! [`rueca_query::tool_definitions`]: one function per tool (JavaScript:
//! `rueca.<camelCase>(args)`, Starlark: `rueca.<snake_case>(**kwargs)`), its
//! arguments from the tool's input schema (names, types, required, the
//! schema's descriptions, which state the defaults and ranges) and its
//! result from the tool's `output_shape`. Arguments every tool takes with
//! the same schema are written once (`CommonArgs`); `format` is left out
//! (code mode always reads the compact shape). Only the preamble (the
//! script contract and limits) and the two examples per language are
//! written here, and the examples are run by the tests.

use std::fmt::Write as _;

use rueca_codemode::to_camel_case;
use rueca_query::ToolDef;
use serde_json::Value;

use super::execute::{MAX_TIMEOUT_MS, SCRIPT_CALL_BUDGET_TOKENS};
use super::types::{Field, Notation, Types};

/// Arguments of the tools that scripts do not take.
const HIDDEN_ARGS: &[&str] = &["format"];

/// The two JavaScript examples of the reference (run by the tests).
pub(crate) const JS_EXAMPLES: [(&str, &str); 2] = [
    (
        "callers of the best matches for a name, as qualified names",
        r#"const groups = await rueca.searchSymbols({ query: "issue", labels: ["Function", "Method"], limit: 3 });
const out = [];
for (const g of groups) for (const s of g.symbols) {
  const t = await rueca.trace({ id: s.id, direction: "callers" });
  out.push({ symbol: s.qn, callers: t.nodes.filter(n => n.dir === "in").map(n => n.qn) });
}
return out;"#,
    ),
    (
        "a lookup that may fail, then the HTTP calls to a route",
        r#"let sym;
try { sym = await rueca.getSymbol({ qualified_name: "InvoiceService.issue" }); }
catch (e) { return "not found: " + e.message; }
const calls = await rueca.listContracts({ side: "consumed", query: "/invoices" });
return { sig: sym.symbol.sig, callers: sym.callers, consumers: [...new Set(calls.map(c => c.repo))] };"#,
    ),
];

/// The two Starlark examples of the reference (run by the tests).
pub(crate) const STARLARK_EXAMPLES: [(&str, &str); 2] = [
    (
        "callers of the best matches for a name, as qualified names",
        r#"groups = rueca.search_symbols(query = "issue", labels = ["Function", "Method"], limit = 3)
out = []
for g in groups:
    for s in g["symbols"]:
        t = rueca.trace(id = s["id"], direction = "callers")
        out.append({"symbol": s["qn"], "callers": [n["qn"] for n in t["nodes"] if n["dir"] == "in"]})
result = out"#,
    ),
    (
        "a symbol's signature, then the repositories calling a route",
        r#"sym = rueca.get_symbol(qualified_name = "InvoiceService.issue")
calls = rueca.list_contracts(side = "consumed", query = "/invoices")
result = {"sig": sym["symbol"].get("sig"), "callers": sym["callers"], "consumers": sorted(set([c["repo"] for c in calls]))}"#,
    ),
];

fn js_preamble() -> String {
    format!(
        "\
// Rueca code mode, JavaScript API (experimental). Pass the script to `execute` as `code`.
// Contract: the script is the body of an async function. `await` the rueca.* calls and `return`
// the answer (any JSON value); only that answer comes back, cut to execute's `budget_tokens`
// (arrays from the end). console.log(...) goes to `logs`. A failed call rejects with an Error whose
// message names the call (try/catch works); an uncaught error, a syntax error or a limit fails the
// script with its line. Promise.all works (calls run one after another).
// Each function resolves to the tool's `results` (the types below). Ids are pinned
// (`repo@commit:path#name`) and valid in any tool; a symbol also has that `name` as `local`, its
// global `qn`, and its `repo` and `ref`; `s` and `in` index execute's `snapshots`. A call
// cut by its `limit` (or `max_lines`) is listed in execute's `partial` with its `next_cursor` (pass
// it as `cursor`). execute's `repo` / `ref` apply to every call that names neither them nor an `id`.
// Argument names are the tools' own (snake_case): rueca.getSymbol({{ qualified_name: \"A.b\" }}).
// In scripts a call's `budget_tokens` defaults to {SCRIPT_CALL_BUDGET_TOKENS}; `format` is not available.
// Limits: {wall} of wall time (`timeout_ms`, at most {max_s} s), 64 MB of memory, 200 rueca.* calls,
// 16 KB of script, 4 KB of logs. Not available: Date, Math.random, timers, import/require, fetch, eval, Function.
",
        wall = "2 s",
        max_s = MAX_TIMEOUT_MS / 1000,
    )
}

fn starlark_preamble() -> String {
    format!(
        "\
Rueca code mode, Starlark API (experimental). Pass the script to `execute` as `code`, with
`language: \"starlark\"`.

Contract: the script is a Starlark module (top-level statements run in order). Assign the answer
(dict, list, str, int, float, bool or None) to the global `result`; only that answer comes back, cut
to execute's `budget_tokens` (lists from the end). print(...) goes to `logs`. A failed rueca.* call
stops the script with an error naming the call and the line (there are no exceptions to catch), as
do a syntax error, an undefined name and a limit.
Each function returns the tool's `results` (shapes below) as dicts and lists: read fields with
d[\"key\"] or d.get(\"key\"). Ids are pinned (`repo@commit:path#name`) and valid in any tool; a
symbol also has that `name` as `local`, its global `qn`, and its `repo` and `ref`; `s` and `in` index
execute's `snapshots`. A call cut by its `limit` (or `max_lines`) is listed in execute's
`partial` with its `next_cursor` (pass it as `cursor`). execute's `repo` / `ref` apply to every call
that names neither them nor an `id`. rueca.* functions take keyword arguments only (a dict spreads
with **args). In scripts a call's `budget_tokens` defaults to {SCRIPT_CALL_BUDGET_TOKENS}; `format` is not
available.

Dialect (Starlark, not Python): no `while`, no classes, no try/except/raise, no
`import` or `load`, no `**` power operator, no `is`, no set literals (set([...]) works); an
assignment inside a function makes a local (there is no `global`); range() takes 32-bit integers; at most 200 nested calls. Available: def,
lambda, if/for (also at top level), comprehensions, f-strings, str/list/dict methods, sorted,
enumerate, zip, min, max, any, all, set(), struct(), json.encode / json.decode.
Limits: 2 s wall time (`timeout_ms`, at most {max_s} s), 64 MB of memory (checked periodically, not
on every allocation), 200 rueca.* calls, 16 KB of script, 4 KB of logs. No clock, randomness,
files or network.
",
        max_s = MAX_TIMEOUT_MS / 1000,
    )
}

/// `search_symbols` → `SearchSymbols`.
fn pascal(snake: &str) -> String {
    let camel = to_camel_case(snake);
    let mut c = camel.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The first sentence of a tool description.
fn first_sentence(text: &str) -> &str {
    text.find(". ").map_or(text, |i| &text[..=i])
}

/// The arguments every tool takes with the same schema (minus the hidden
/// ones), in the first tool's order.
fn common_args(defs: &[ToolDef]) -> Vec<String> {
    let Some(first) = defs.first().and_then(|d| d.input_schema.get("properties")).and_then(Value::as_object) else {
        return Vec::new();
    };
    first
        .iter()
        .filter(|(name, schema)| {
            !HIDDEN_ARGS.contains(&name.as_str())
                && defs.iter().all(|d| d.input_schema.pointer(&format!("/properties/{name}")) == Some(*schema))
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// `schema` keeping only the properties `keep` accepts (and their
/// `required` entries).
fn only_args(schema: &Value, keep: impl Fn(&str) -> bool) -> Value {
    let mut s = schema.clone();
    if let Some(Value::Object(props)) = s.get_mut("properties") {
        props.retain(|k, _| keep(k));
    }
    if let Some(Value::Array(req)) = s.get_mut("required") {
        req.retain(|r| r.as_str().is_some_and(&keep));
    }
    s
}

/// The `budget_tokens` documentation of scripts.
fn script_budget_doc(field: &mut Field) {
    if field.name == "budget_tokens" {
        field.doc = Some(format!(
            "Token budget of the call's own answer, 100–1,000,000 (default {SCRIPT_CALL_BUDGET_TOKENS} in scripts)."
        ));
    }
}

fn has_required(schema: &Value) -> bool {
    schema.get("required").and_then(Value::as_array).is_some_and(|r| !r.is_empty())
}

/// The TypeScript declarations (`describe_api` for `js`).
pub(crate) fn typescript() -> String {
    let defs = rueca_query::tool_definitions();
    let common = common_args(&defs);
    let mut types = Types::new(Notation::TypeScript);
    let mut out = js_preamble();
    out.push('\n');
    let common_schema = only_args(&defs[0].input_schema, |k| common.iter().any(|c| c == k));
    out.push_str("interface CommonArgs ");
    out.push_str(&ts_fields(&mut types, &common_schema));
    out.push('\n');
    let mut api = String::from("declare const rueca: {\n");
    for d in &defs {
        let args = only_args(&d.input_schema, |k| !HIDDEN_ARGS.contains(&k) && !common.iter().any(|c| c == k));
        let name = pascal(d.name);
        let _ = writeln!(out, "interface {name}Args extends CommonArgs {}", ts_fields(&mut types, &args));
        let _ = writeln!(out, "type {name}Result = {};", types.definition(&d.output_shape));
        let optional = if has_required(&args) { "" } else { "?" };
        let _ = writeln!(api, "  /** {} */", first_sentence(d.description));
        let _ = writeln!(api, "  {}(args{optional}: {name}Args): Promise<{name}Result>;", to_camel_case(d.name));
    }
    api.push_str("};\n");
    declare_named(&mut types, &mut out, |name, body| format!("type {name} = {body};\n"));
    out.push_str(&api);
    for (title, code) in JS_EXAMPLES {
        let _ = write!(out, "\n// Example: {title}.\n{code}\n");
    }
    out
}

/// `{ field: type; // doc … }` for a TypeScript interface.
fn ts_fields(types: &mut Types, schema: &Value) -> String {
    let mut fields = types.fields(schema);
    let mut out = String::from("{\n");
    for f in &mut fields {
        script_budget_doc(f);
        let _ = write!(out, "  {};", f.decl);
        if let Some(d) = &f.doc {
            let _ = write!(out, " // {d}");
        }
        out.push('\n');
    }
    out.push('}');
    out
}

/// Declares every named type the declarations used (and those they use),
/// each once, in name order per round.
fn declare_named(types: &mut Types, out: &mut String, decl: impl Fn(&str, &str) -> String) {
    let mut declared = Vec::new();
    loop {
        let batch = types.take_undeclared(&mut declared);
        if batch.is_empty() {
            return;
        }
        for (name, schema) in batch {
            let body = types.definition(&schema);
            out.push_str(&decl(&name, &body));
        }
    }
}

/// The Starlark reference (`describe_api` for `starlark`).
pub(crate) fn starlark() -> String {
    let defs = rueca_query::tool_definitions();
    let common = common_args(&defs);
    let mut types = Types::new(Notation::Python);
    let mut out = starlark_preamble();
    out.push_str("\nKeyword arguments of every function:\n");
    let common_schema = only_args(&defs[0].input_schema, |k| common.iter().any(|c| c == k));
    for mut f in types.fields(&common_schema) {
        script_budget_doc(&mut f);
        out.push_str(&arg_line(&f));
    }
    out.push_str("\nFunctions:\n");
    for d in &defs {
        let args = only_args(&d.input_schema, |k| !HIDDEN_ARGS.contains(&k) && !common.iter().any(|c| c == k));
        let fields = types.fields(&args);
        let mut sig: Vec<String> = fields.iter().filter(|f| !f.optional).map(|f| f.name.clone()).collect();
        sig.extend(fields.iter().filter(|f| f.optional).map(|f| format!("{}=None", f.name)));
        let star = if sig.is_empty() { "" } else { "*, " };
        let result = types.definition(&d.output_shape).replace('\n', "\n    ");
        let _ = writeln!(out, "\nrueca.{}({star}{}) -> {result}", d.name, sig.join(", "));
        let _ = writeln!(out, "  {}", first_sentence(d.description));
        for f in &fields {
            out.push_str(&arg_line(f));
        }
    }
    out.push_str("\nTypes:\n");
    declare_named(&mut types, &mut out, |name, body| format!("{name} = {body}\n"));
    for (title, code) in STARLARK_EXAMPLES {
        let _ = write!(out, "\nExample: {title}.\n{code}\n");
    }
    out
}

/// `  name: type — doc` (`(required)` for a required argument).
fn arg_line(f: &Field) -> String {
    let req = if f.optional { "" } else { " (required)" };
    match &f.doc {
        Some(d) => format!("  {}: {}{req} — {d}\n", f.name, f.ty),
        None => format!("  {}: {}{req}\n", f.name, f.ty),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(pascal("search_symbols"), "SearchSymbols");
        assert_eq!(pascal("trace"), "Trace");
        assert_eq!(first_sentence("Find things. More text."), "Find things.");
        assert_eq!(first_sentence("No period"), "No period");
    }

    #[test]
    fn common_args_are_shared_and_format_is_hidden() {
        let common = common_args(&rueca_query::tool_definitions());
        assert!(common.iter().any(|c| c == "repo") && common.iter().any(|c| c == "ref"), "{common:?}");
        assert!(!common.iter().any(|c| c == "format"));
    }
}
