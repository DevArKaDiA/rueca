//! End-to-end Swift resolution with the real `SwiftPack` over the SwiftPM
//! fixture in `tests/fixtures/spm`: a `Core` library (a protocol, a class,
//! a struct with an extension in another file, an extension of `String`),
//! an `App` executable importing it, and an XCTest target.

mod common;

use common::*;
use rueca_model::{EdgeKind, Strategy};

#[test]
fn modules_are_targets() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("Sources/Core/Point+Scale.swift"), "Core");
    assert_eq!(g.module_of("Sources/App/main.swift"), "App");
    assert_eq!(g.module_of("Tests/CoreTests/PointTests.swift"), "CoreTests");
    g.id("Core.Point.init(polar:angle:)");
    g.id("Core.String.shouted()");
}

#[test]
fn imports_bind_modules() {
    let out = fixture();
    let g = G(&out.graph);
    g.has("Sources/App/main.swift", "Sources/Core", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    assert!(out.stats.references_external > 0, "Foundation and XCTest are external");
}

#[test]
fn initializers_are_chosen_by_labels() {
    let out = fixture();
    let g = G(&out.graph);
    let run = "App.Runner.run()";
    // `Point(polar:angle:)`: the type and the initializer declared in an
    // extension in another file of the imported module.
    g.has_any(run, "Core.Point", EdgeKind::Calls);
    g.has_any(run, "Core.Point.init(polar:angle:)", EdgeKind::Calls);
    g.lacks(run, "Core.Point.init(x:y:)", EdgeKind::Calls);
    // `self.init(x:y:)` inside the extension initializer.
    g.has("Core.Point.init(polar:angle:)", "Core.Point.init(x:y:)", EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // A memberwise-looking call in the same module.
    g.has_any("Core.Point.scaled(by:)", "Core.Point.init(x:y:)", EdgeKind::Calls);
}

#[test]
fn receivers_typed_by_properties_locals_and_results() {
    let out = fixture();
    let g = G(&out.graph);
    let run = "App.Runner.run()";
    // `canvas.add(p)`: a property declared with its type.
    g.has(run, "Core.Canvas.add(_:)", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `Point.origin().scaled(by: 2)`: a static call, then its result.
    g.has_any(run, "Core.Point.origin()", EdgeKind::Calls);
    g.has(run, "Core.Point.scaled(by:)", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `makeCanvas().add(p)`: an imported function's declared return type.
    g.has_any(run, "Core.makeCanvas()", EdgeKind::Calls);
    // `p.normalized()`: a local constructed from the type.
    g.has("Core.Point.scaled(by:)", "Core.Point.normalized()", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Implicit self across the files of a type: `length()` in the extension.
    g.has("Core.Point.normalized()", "Core.Point.length()", EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // `origin.length()`: a stored property of the class.
    g.has("Core.Canvas.total()", "Core.Point.length()", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `s.area()` on a protocol-typed loop variable is untyped; `shape.describe()`
    // on a parameter declared with the protocol reaches the requirement.
    g.has("Core.Canvas.add(_:)", "Core.Shape.describe()", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has("Core.Canvas.add(_:)", "Core.Canvas.log(_:)", EdgeKind::Calls, Strategy::TypeResolved, 0.9);
}

#[test]
fn extensions_of_external_types_and_bridge() {
    let out = fixture();
    let g = G(&out.graph);
    // `s.shouted()` on a `String` parameter: the extension member.
    g.has("App.Runner.report(_:)", "Core.String.shouted()", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has_any("Core.makeCanvas()", "$env:CANVAS_HOME", EdgeKind::Reads);
}

#[test]
fn tests_and_conformances() {
    let out = fixture();
    let g = G(&out.graph);
    let t = "CoreTests.PointTests.testLength()";
    g.has_any(t, "Core.Point.length()", EdgeKind::Calls);
    g.has_any(t, "Core.Point.length()", EdgeKind::Tests);
    g.has_any("Core.Point", "Core.Shape", EdgeKind::Implements);
    g.has_any("Core.Point.area()", "Core.Shape.area()", EdgeKind::Overrides);
}
