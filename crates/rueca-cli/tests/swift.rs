//! Swift end to end: `rueca index` on a small SwiftPM package, then the
//! tools through `rueca query` — target-qualified names with selectors,
//! initializers chosen by labels, an extension in another file, and an
//! import that binds a module.

mod common;

use serde_json::json;

use common::*;

const MANIFEST: &str = r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Shop",
    products: [.library(name: "Core", targets: ["Core"])],
    targets: [
        .target(name: "Core"),
        .executableTarget(name: "App", dependencies: ["Core"]),
    ]
)
"#;

const ORDER: &str = r#"/// An order.
public struct Order {
    public let id: Int

    public init(id: Int) {
        self.id = id
    }

    public func price() -> Int {
        return id * 2
    }
}
"#;

// An initializer and a method declared in an extension in another file.
const ORDER_EXT: &str = r#"extension Order {
    public init(code: String) {
        self.init(id: code.count)
    }

    public func describe() -> String {
        return String(price())
    }
}
"#;

const APP: &str = r#"import Core

struct Checkout {
    func run() -> Int {
        let order = Order(code: "abc")
        _ = order.describe()
        return order.price()
    }
}
"#;

fn swift_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root, "Package.swift", MANIFEST);
    write(&root, "Sources/Core/Order.swift", ORDER);
    write(&root, "Sources/Core/Order+Code.swift", ORDER_EXT);
    write(&root, "Sources/App/main.swift", APP);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn swift_index_then_every_tool() {
    let p = swift_project();
    index(&p);

    let r = query(&p, "search_symbols", json!({"query": "Order", "language": "swift"}));
    assert_eq!(r["results"][0]["qualified_name"], "Core.Order", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "Core.Order"}));
    assert_eq!(r["symbol"]["language"], "swift", "{r}");
    assert_eq!(r["symbol"]["doc"], "An order.");

    // Callers of `price`: the app through a local constructed with an
    // initializer of an extension, and the extension's own method.
    let r = query(&p, "trace", json!({"qualified_name": "Core.Order.price()", "direction": "callers"}));
    assert_eq!(qns(&r["nodes"]), ["App.Checkout.run()", "Core.Order.describe()"], "{r}");

    // Callees of `run`: the type, the initializer the labels choose, and
    // both methods.
    let r = query(&p, "trace", json!({"qualified_name": "App.Checkout.run()", "direction": "callees", "depth": 1}));
    let callees = qns(&r["nodes"]);
    for want in ["Core.Order", "Core.Order.init(code:)", "Core.Order.describe()", "Core.Order.price()"] {
        assert!(callees.contains(&want.to_string()), "{want} in {callees:?}");
    }
    assert!(!callees.contains(&"Core.Order.init(id:)".to_string()), "{callees:?}");

    // `import Core` → IMPORTS File → the target's folder.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "Sources/App/main.swift", "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["Sources/Core"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "Sources/Core/Order+Code.swift"}));
    assert_eq!(qns(&r["symbols"]), ["Core.Order.init(code:)", "Core.Order.describe()"], "{r}");
}
