//! Java end to end: `rueca index` on a small Maven project, then the tools
//! through `rueca query` — package-qualified names, symbol ids with overload
//! suffixes, typed receivers, `OVERRIDES` and package-folder imports.

mod common;

use serde_json::json;

use common::*;

const PARSER: &str = r#"package com.acme.core;

/** Parses text. */
public class Parser extends Base {
    public Object parse(String text) {
        return parse(text, 10);
    }

    public Object parse(String text, int limit) {
        return text;
    }

    @Override
    public void run() {
        super.run();
    }
}
"#;

const BASE: &str = r#"package com.acme.core;

public abstract class Base {
    public void run() {}
}
"#;

const APP: &str = r#"package com.acme.app;

import com.acme.core.*;

public class App {
    void start(Parser parser) {
        parser.parse("x", 3);
        Base b = parser;
        b.run();
    }
}
"#;

fn java_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root, "pom.xml", "<project><modules><module>core</module><module>app</module></modules></project>");
    write(&root, "core/src/main/java/com/acme/core/Parser.java", PARSER);
    write(&root, "core/src/main/java/com/acme/core/Base.java", BASE);
    write(&root, "app/src/main/java/com/acme/app/App.java", APP);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn java_index_then_every_tool() {
    let p = java_project();
    index(&p);
    let parse2 = "local/proj@worktree:core/src/main/java/com/acme/core/Parser.java#Parser.parse(String,int)";

    let r = query(&p, "search_symbols", json!({"query": "Parser", "language": "java"}));
    assert_eq!(r["results"][0]["qualified_name"], "com.acme.core.Parser", "{r}");

    // The overload suffix is optional in lookups; the id carries it.
    let r = query(&p, "get_symbol", json!({"qualified_name": "Parser.parse(String,int)"}));
    assert_eq!(r["symbol"]["id"], parse2, "{r}");
    assert_eq!(r["symbol"]["language"], "java");
    assert_eq!(r["symbol"]["signature"], "public Object parse(String text, int limit)");
    assert_eq!(r["parent"]["qualified_name"], "com.acme.core.Parser");
    let r = query(&p, "get_symbol", json!({"qualified_name": "Parser.parse"}));
    assert_eq!(r["candidates"].as_array().unwrap().len(), 1, "{r}");
    let r = query(&p, "get_symbol", json!({"qualified_name": "com.acme.core.Parser"}));
    assert_eq!(r["symbol"]["doc"], "Parses text.");
    assert_eq!(qns(&r["bases"]), ["com.acme.core.Base"]);

    // `parser.parse("x", 3)` on a typed parameter, chosen by arity; the
    // one-argument overload calls it through implicit `this`.
    let r = query(&p, "trace", json!({"id": parse2, "direction": "callers"}));
    assert_eq!(qns(&r["nodes"]), ["com.acme.app.App.start", "com.acme.core.Parser.parse(String)"], "{r}");

    // OVERRIDES: callers of Base.run through overrides.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "com.acme.core.Base.run", "direction": "callers", "edge_types": ["OVERRIDES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["com.acme.core.Parser.run"], "{r}");

    // `import com.acme.core.*` → IMPORTS File → Folder.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "app/src/main/java/com/acme/app/App.java", "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["core/src/main/java/com/acme/core"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "core/src/main/java/com/acme/core/Parser.java"}));
    assert_eq!(qns(&r["symbols"]), ["com.acme.core.Parser"], "{r}");
    assert_eq!(r["symbols"][0]["children"].as_array().unwrap().len(), 3, "{r}");

    let r = query(&p, "get_snippet", json!({"id": parse2}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("public Object parse(String text, int limit)"), "{r}");
}
