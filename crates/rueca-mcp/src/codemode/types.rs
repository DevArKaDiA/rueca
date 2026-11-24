//! JSON Schema → type notation, for the generated script API: TypeScript
//! (`describe_api` for `js`) and a Python-like notation (for `starlark`).
//!
//! It understands the subset the tools' input schemas (schemars) and result
//! shapes (`rueca_query::ToolDef::output_shape`) use: `type`, `enum`,
//! `const`, `anyOf` / `oneOf`, `items`, `prefixItems` (tuples, elements
//! labelled by `title`), `properties` / `required`, `additionalProperties`
//! (maps), `title` (a named type, declared once) and `$ref` (a name).
//! Anything else renders as the language's "any".

use std::collections::BTreeMap;

use serde_json::Value;

/// The notation types are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Notation {
    TypeScript,
    Python,
}

impl Notation {
    fn primitive(self, name: &str) -> &'static str {
        match (self, name) {
            (Notation::TypeScript, "string") => "string",
            (Notation::TypeScript, "integer" | "number") => "number",
            (Notation::TypeScript, "boolean") => "boolean",
            (Notation::TypeScript, "null") => "null",
            (Notation::TypeScript, "object") => "object",
            (Notation::TypeScript, _) => "unknown",
            (Notation::Python, "string") => "str",
            (Notation::Python, "integer") => "int",
            (Notation::Python, "number") => "float",
            (Notation::Python, "boolean") => "bool",
            (Notation::Python, "null") => "None",
            (Notation::Python, "object") => "dict",
            (Notation::Python, _) => "any",
        }
    }

    /// Separator between object fields written on one line.
    fn field_sep(self) -> &'static str {
        match self {
            Notation::TypeScript => "; ",
            Notation::Python => ", ",
        }
    }

    pub(crate) fn comment(self) -> &'static str {
        match self {
            Notation::TypeScript => "//",
            Notation::Python => "#",
        }
    }
}

/// Renders schemas, collecting the named types (`title`) they use.
#[derive(Debug)]
pub(crate) struct Types {
    notation: Notation,
    /// Named types met so far, by name.
    pub named: BTreeMap<String, Value>,
}

impl Types {
    pub(crate) fn new(notation: Notation) -> Types {
        Types { notation, named: BTreeMap::new() }
    }

    /// The type of `schema` on one line; a titled object becomes its name.
    pub(crate) fn inline(&mut self, schema: &Value) -> String {
        if let Some(name) = schema.get("$ref").and_then(Value::as_str) {
            return name.to_string();
        }
        if let Some(name) = schema.get("title").and_then(Value::as_str)
            && is_named(schema)
        {
            self.named.entry(name.to_string()).or_insert_with(|| schema.clone());
            return name.to_string();
        }
        self.body(schema)
    }

    /// The type of `schema` itself, ignoring its own `title`.
    fn body(&mut self, schema: &Value) -> String {
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            // A nullable enum lists `null` among its values: optional
            // arguments are omitted instead, so it is left out.
            return values.iter().filter(|v| !v.is_null()).map(literal).collect::<Vec<_>>().join(" | ");
        }
        if let Some(c) = schema.get("const") {
            return literal(c);
        }
        if let Some(alts) = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(Value::as_array) {
            return self.union(alts);
        }
        match schema.get("type") {
            Some(Value::String(t)) if t == "array" => self.array(schema),
            Some(Value::String(t)) if t == "object" => self.object_inline(schema),
            Some(Value::String(t)) => self.notation.primitive(t).to_string(),
            Some(Value::Array(ts)) => {
                let alts: Vec<Value> = ts.iter().map(|t| serde_json::json!({ "type": t })).collect();
                self.union(&alts)
            }
            _ => self.notation.primitive("").to_string(),
        }
    }

    fn union(&mut self, alts: &[Value]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for a in alts {
            let t = self.inline(a);
            if !parts.contains(&t) {
                parts.push(t);
            }
        }
        parts.join(" | ")
    }

    fn array(&mut self, schema: &Value) -> String {
        if let Some(elems) = schema.get("prefixItems").and_then(Value::as_array) {
            return self.tuple(elems, schema.get("items"));
        }
        let item = match schema.get("items") {
            Some(i) => self.inline(i),
            None => self.notation.primitive("").to_string(),
        };
        match self.notation {
            Notation::TypeScript if top_level_union(&item) => format!("({item})[]"),
            Notation::TypeScript => format!("{item}[]"),
            Notation::Python => format!("list[{item}]"),
        }
    }

    fn tuple(&mut self, elems: &[Value], rest: Option<&Value>) -> String {
        let mut parts: Vec<String> = elems
            .iter()
            .map(|e| {
                let t = self.body(e);
                match e.get("title").and_then(Value::as_str) {
                    Some(label) => format!("{label}: {t}"),
                    None => t,
                }
            })
            .collect();
        if let Some(r) = rest.filter(|r| r.is_object()) {
            let t = self.inline(r);
            parts.push(match self.notation {
                Notation::TypeScript => format!("...rest: ({t})[]"),
                Notation::Python => format!("*({t})"),
            });
        }
        format!("[{}]", parts.join(", "))
    }

    fn object_inline(&mut self, schema: &Value) -> String {
        if let Some(values) = schema.get("additionalProperties").filter(|v| v.is_object()) {
            let v = self.inline(values);
            return match self.notation {
                Notation::TypeScript => format!("Record<string, {v}>"),
                Notation::Python => format!("dict[str, {v}]"),
            };
        }
        let fields = self.fields(schema);
        if fields.is_empty() {
            return self.notation.primitive("object").to_string();
        }
        let parts: Vec<String> = fields.into_iter().map(|f| f.decl).collect();
        format!("{{{}}}", parts.join(self.notation.field_sep()))
    }

    /// The fields of an object schema, in schema order.
    pub(crate) fn fields(&mut self, schema: &Value) -> Vec<Field> {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let Some(props) = schema.get("properties").and_then(Value::as_object) else { return Vec::new() };
        props
            .iter()
            .map(|(name, prop)| {
                let optional = !required.contains(&name.as_str());
                let t = self.inline(prop);
                let mark = if optional { "?" } else { "" };
                Field {
                    name: name.clone(),
                    optional,
                    ty: t.clone(),
                    decl: format!("{name}{mark}: {t}"),
                    doc: description(prop),
                }
            })
            .collect()
    }

    /// `schema` (an object) with one field per line and its description as
    /// a trailing comment, indented by `indent`.
    pub(crate) fn object_block(&mut self, schema: &Value, indent: &str) -> String {
        let fields = self.fields(schema);
        let sep = match self.notation {
            Notation::TypeScript => ";",
            Notation::Python => ",",
        };
        let mut out = String::from("{\n");
        for f in fields {
            out.push_str(indent);
            out.push_str("  ");
            out.push_str(&f.decl);
            out.push_str(sep);
            if let Some(d) = f.doc {
                out.push_str(&format!(" {} {d}", self.notation.comment()));
            }
            out.push('\n');
        }
        out.push_str(indent);
        out.push('}');
        out
    }

    /// The definition of a type: a block for an object, else one line.
    pub(crate) fn definition(&mut self, schema: &Value) -> String {
        if is_named(schema) && schema.get("properties").is_some() {
            self.object_block(schema, "")
        } else {
            self.body(schema)
        }
    }

    /// Named types met but not yet declared, in name order, each once.
    pub(crate) fn take_undeclared(&mut self, declared: &mut Vec<String>) -> Vec<(String, Value)> {
        let out: Vec<(String, Value)> =
            self.named.iter().filter(|(n, _)| !declared.contains(n)).map(|(n, v)| (n.clone(), v.clone())).collect();
        declared.extend(out.iter().map(|(n, _)| n.clone()));
        out
    }
}

/// A field of an object type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Field {
    pub name: String,
    pub optional: bool,
    pub ty: String,
    /// `name?: type`.
    pub decl: String,
    pub doc: Option<String>,
}

/// A titled schema that is a type of its own (an object, or a tuple),
/// as opposed to a tuple element's label.
fn is_named(schema: &Value) -> bool {
    matches!(schema.get("type").and_then(Value::as_str), Some("object" | "array"))
}

/// Whether `t` has a `|` outside brackets (`A | B`, not `{a: A | B}`).
fn top_level_union(t: &str) -> bool {
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            '|' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

fn literal(v: &Value) -> String {
    v.to_string()
}

/// The schema's description on one line.
pub(crate) fn description(schema: &Value) -> Option<String> {
    let d = schema.get("description").and_then(Value::as_str)?;
    let one: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
    (!one.is_empty()).then_some(one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notations() {
        let s = json!({"type": "array", "items": {"type": "object", "properties": {
            "a": {"type": "integer"}, "b": {"anyOf": [{"type": "string"}, {"type": "null"}]},
            "m": {"type": "object", "additionalProperties": {"type": "integer"}},
            "t": {"type": "array", "prefixItems": [{"title": "x", "type": "integer"}, {"type": "string"}]}
        }, "required": ["a"]}});
        assert_eq!(
            Types::new(Notation::TypeScript).inline(&s),
            "{a: number; b?: string | null; m?: Record<string, number>; t?: [x: number, string]}[]"
        );
        assert_eq!(
            Types::new(Notation::Python).inline(&s),
            "list[{a: int, b?: str | None, m?: dict[str, int], t?: [x: int, str]}]"
        );
        let e = json!({"oneOf": [{"const": "in"}, {"const": "out"}]});
        assert_eq!(Types::new(Notation::TypeScript).inline(&e), "\"in\" | \"out\"");
        let named = json!({"type": "array", "items": {"title": "Brief", "type": "object", "properties": {"id": {"type": "string"}}}});
        let mut t = Types::new(Notation::TypeScript);
        assert_eq!(t.inline(&named), "Brief[]");
        assert!(t.named.contains_key("Brief"));
    }
}
