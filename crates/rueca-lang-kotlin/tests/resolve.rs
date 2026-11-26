//! End-to-end Kotlin resolution with the real `KotlinPack` (and the Java
//! pack, for interop) over the Gradle fixture in `tests/fixtures/gradle`:
//! a `core` module with a data class and its companion, an interface and
//! its implementation, an extension function, a file whose package is not
//! its directory, a Java class, a kotlin.test test; an `app` module in
//! Kotlin and Java using them.

mod common;

use common::*;
use rueca_model::{EdgeKind, Strategy};

const CORE: &str = "com.acme.core";
const APP: &str = "com.acme.app";

fn c(local: &str) -> String {
    format!("{CORE}.{local}")
}
fn a(local: &str) -> String {
    format!("{APP}.{local}")
}

#[test]
fn packages_are_declared_not_directories() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("core/src/main/kotlin/com/acme/core/Order.kt"), CORE);
    assert_eq!(g.module_of("core/src/main/kotlin/util/Strings.kt"), "com.acme.core.text");
    assert_eq!(g.module_of("core/src/test/kotlin/com/acme/core/OrderTest.kt"), CORE);
    // Gradle scripts are Kotlin without a package.
    assert_eq!(g.module_of("settings.gradle.kts"), "");
    g.id(&c("Order.Companion.create"));
    g.id("com.acme.core.text.String.shout");
}

#[test]
fn imports_bind_packages_wherever_their_files_are() {
    let out = fixture();
    let g = G(&out.graph);
    let app = "app/src/main/kotlin/com/acme/app/App.kt";
    g.has(app, "core/src/main/kotlin/com/acme/core", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    // `com.acme.core.text` lives in `util/`.
    g.has(app, "core/src/main/kotlin/util", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    assert!(out.stats.references_external > 0, "kotlin.* and JDK references are external");
}

#[test]
fn calls_through_companions_constructors_and_typed_receivers() {
    let out = fixture();
    let g = G(&out.graph);
    let checkout = a("App.checkout");
    g.has(&checkout, &c("Order.Companion.create"), EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    // `Order(id, 3)`: the class and its constructor.
    g.has_any(&checkout, &c("Order"), EdgeKind::Calls);
    g.has_any(&checkout, &c("Order.Order"), EdgeKind::Calls);
    // A constructor parameter property declared with the interface.
    g.has(&checkout, &c("Repo.save"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `order.total()`: `order` holds what `Order.create` returns; the
    // extension of `Order` is found as its member.
    g.has(&checkout, &c("Order.total"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // An imported extension of a type outside the project.
    g.has(&checkout, "com.acme.core.text.String.shout", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // In an extension function, `price()` is called on its receiver.
    g.has(&c("Order.total"), &c("Order.price"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&c("Order.Companion.create"), &c("Order.Order"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    g.has(&c("MemRepo.save"), &c("Repo.save"), EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    g.has(&c("MemRepo"), &c("Repo"), EdgeKind::Implements, Strategy::SameModule, 0.9);
}

#[test]
fn java_and_kotlin_share_packages() {
    let out = fixture();
    let g = G(&out.graph);
    // Kotlin → Java.
    g.has(&a("App.checkout"), &c("LegacyPricer.price"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Java → Kotlin: the import resolves to `Order.kt`, `new Order(…)` to
    // the Kotlin constructor.
    g.has(
        "app/src/main/java/com/acme/app/JavaClient.java",
        "core/src/main/kotlin/com/acme/core/Order.kt",
        EdgeKind::Imports,
        Strategy::ImportResolved,
        1.0,
    );
    g.has_any(&a("JavaClient.run"), &c("Order.Order"), EdgeKind::Calls);
    g.has(&a("JavaClient.run"), &c("Order.price"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&c("LegacyPricer.price"), &c("Order.price"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
}

#[test]
fn tests_and_env_vars() {
    let out = fixture();
    let g = G(&out.graph);
    g.has_any(&c("OrderTest.totals"), &c("Order.total"), EdgeKind::Tests);
    g.has(&a("App.checkout"), "$env:SHOP_PORT", EdgeKind::Reads, Strategy::Contract, 0.95);
}
