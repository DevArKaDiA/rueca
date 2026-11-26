//! Kotlin end to end: `rueca index` on a small Gradle project, then the
//! tools through `rueca query` — package-qualified names, companion and
//! extension calls, imports bound by package and a Java caller.

mod common;

use serde_json::json;

use common::*;

const ORDER: &str = r#"package com.acme.core

/** An order. */
data class Order(val id: Long, val qty: Int = 1) {
    fun price(): Long = id * qty

    companion object {
        fun create(id: Long): Order = Order(id)
    }
}
"#;

// The package is not the directory: imports bind it by name.
const TEXT: &str = r#"package com.acme.core.text

fun String.shout(): String = uppercase()
"#;

const APP: &str = r#"package com.acme.app

import com.acme.core.Order
import com.acme.core.text.shout

class App {
    fun checkout(id: Long): Long {
        val order = Order.create(id)
        "done".shout()
        return order.price()
    }
}
"#;

const CLIENT: &str = r#"package com.acme.app;

import com.acme.core.Order;

public class Client {
    long run() {
        return new Order(1L, 2).price();
    }
}
"#;

fn kotlin_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root, "settings.gradle.kts", "rootProject.name = \"shop\"\ninclude(\"core\", \"app\")\n");
    write(&root, "core/src/main/kotlin/com/acme/core/Order.kt", ORDER);
    write(&root, "core/src/main/kotlin/text/Strings.kt", TEXT);
    write(&root, "app/src/main/kotlin/com/acme/app/App.kt", APP);
    write(&root, "app/src/main/java/com/acme/app/Client.java", CLIENT);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn kotlin_index_then_every_tool() {
    let p = kotlin_project();
    index(&p);

    // The class and its primary constructor share the name and the line.
    let r = query(&p, "search_symbols", json!({"query": "Order", "language": "kotlin"}));
    let top: Vec<&str> = (0..2).filter_map(|i| r["results"][i]["qualified_name"].as_str()).collect();
    assert!(top.contains(&"com.acme.core.Order") && top.contains(&"com.acme.core.Order.Order"), "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "com.acme.core.Order"}));
    assert_eq!(r["symbol"]["language"], "kotlin", "{r}");
    assert_eq!(r["symbol"]["doc"], "An order.");

    // Callers of `price`: a Kotlin caller through the companion's return
    // type, and a Java one through `new Order(…)`.
    let r = query(&p, "trace", json!({"qualified_name": "com.acme.core.Order.price", "direction": "callers"}));
    assert_eq!(qns(&r["nodes"]), ["com.acme.app.App.checkout", "com.acme.app.Client.run"], "{r}");

    // Callees of `checkout`: the companion member and the imported extension.
    let r = query(&p, "trace", json!({"qualified_name": "com.acme.app.App.checkout", "direction": "callees"}));
    let callees = qns(&r["nodes"]);
    for want in ["com.acme.core.Order.Companion.create", "com.acme.core.text.String.shout", "com.acme.core.Order.price"]
    {
        assert!(callees.contains(&want.to_string()), "{want} in {callees:?}");
    }

    // `import com.acme.core.text.shout` → IMPORTS File → the package's folder.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "app/src/main/kotlin/com/acme/app/App.kt", "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["core/src/main/kotlin/com/acme/core", "core/src/main/kotlin/text"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "core/src/main/kotlin/com/acme/core/Order.kt"}));
    assert_eq!(qns(&r["symbols"]), ["com.acme.core.Order"], "{r}");
}
