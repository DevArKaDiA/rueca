//! Extraction tests on small Swift sources.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{FileFacts, Label, Language, RefKind};

use crate::SwiftPack;

fn facts(src: &str) -> FileFacts {
    SwiftPack.extract("Sources/App/a.swift", Language::Swift, src.as_bytes(), &Budget::default()).expect("extract")
}

fn qns(f: &FileFacts) -> Vec<(Label, String)> {
    f.definitions.iter().map(|d| (d.label, d.qualified_name.clone())).collect()
}

fn calls(f: &FileFacts) -> Vec<(String, Option<String>, Option<String>)> {
    f.references
        .iter()
        .filter(|r| r.kind == RefKind::Call)
        .map(|r| {
            let enc = r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone());
            (r.name.clone(), r.receiver.clone(), enc)
        })
        .collect()
}

#[test]
fn definitions_and_selectors() {
    let f = facts(
        r#"
public protocol Shape { func draw(at x: Int, _ y: Int) -> Bool; init(size: Int) }
struct Point: Shape {
    var x: Int = 0
    init(size: Int) { }
    init?(x: Int, label: String = "") { }
    deinit { }
    func draw(at x: Int, _ y: Int) -> Bool { func local() {}; return true }
    subscript(index: Int) -> Int { index }
    static func + (lhs: Point, rhs: Point) -> Point { lhs }
}
extension Point {
    func extra() {}
    struct Nested { func n() {} }
}
enum Color { case red, green; case custom(Int) }
func top(_ a: Int...) {}
let limit = 3
typealias Handler = () -> Void
"#,
    );
    let got = qns(&f);
    for want in [
        (Label::Interface, "Shape"),
        (Label::Method, "Shape.draw(at:_:)"),
        (Label::Method, "Shape.init(size:)"),
        (Label::Struct, "Point"),
        (Label::Field, "Point.x"),
        (Label::Method, "Point.init(size:)"),
        (Label::Method, "Point.init(x:label:)"),
        (Label::Method, "Point.deinit"),
        (Label::Method, "Point.draw(at:_:)"),
        (Label::Function, "Point.draw.local()"),
        (Label::Method, "Point.subscript(_:)"),
        (Label::Method, "Point.+(_:_:)"),
        (Label::Method, "Point.extra()"),
        (Label::Struct, "Point.Nested"),
        (Label::Method, "Point.Nested.n()"),
        (Label::Enum, "Color"),
        (Label::Constant, "Color.red"),
        (Label::Constant, "Color.custom"),
        (Label::Function, "top(_:)"),
        (Label::Constant, "limit"),
        (Label::TypeAlias, "Handler"),
    ] {
        assert!(got.contains(&(want.0, want.1.to_string())), "missing {want:?} in {got:#?}");
    }
    let extra = f.definitions.iter().find(|d| d.qualified_name == "Point.extra()").unwrap();
    assert_eq!(extra.parent, None, "extension members are top-level");
    let point = f.definitions.iter().find(|d| d.qualified_name == "Point").unwrap();
    assert_eq!(point.implements, ["Shape"]);
    assert_eq!(f.parse_errors, 0);
}

#[test]
fn calls_receivers_and_labels() {
    let f = facts(
        r#"
import Foundation
struct Point {
    var origin: Point?
    let helper = Helper()
    init(size: Int) { self.init(x: size, y: 0) }
    func run(p: Point, items: [Int]) {
        draw(at: 1, 2)
        self.draw(at: 1, 2)
        p.draw(at: 1, 2)
        let q = Point(size: 3)
        q?.draw(at: 1, 2)
        helper.go()
        origin?.go()
        Point.make(1) { _ in }
        make().go()
        let r = make()
        r.go()
        items[0].go()
        super.run()
        let env = ProcessInfo.processInfo.environment["API_KEY"]
        let home = getenv("HOME")
    }
}
"#,
    );
    let c = calls(&f);
    let has = |name: &str, recv: Option<&str>| c.iter().any(|(n, r, _)| n == name && r.as_deref() == recv);
    assert!(has("draw$args:at,_", None), "{c:#?}");
    assert!(has("draw$args:at,_", Some("$self")));
    assert!(has("draw$args:at,_", Some("$type:Point")));
    assert!(has("init$args:x,y", Some("$self")));
    assert!(has("Point$args:size", None));
    assert!(has("go", Some("$type:Helper")));
    assert!(has("go", Some("$type:Point")));
    assert!(has("make$args:_,?", Some("Point")));
    assert!(has("go", Some("$ret:#make")));
    assert!(has("run", Some("super")));
    let envs: Vec<&str> = f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| r.name.as_str()).collect();
    assert_eq!(envs, ["API_KEY", "HOME"]);
    assert!(!c.iter().any(|(n, _, _)| n.starts_with("items")), "subscripts are not calls");
}

#[test]
fn imports_and_tests() {
    let f = facts(
        r#"
@testable import ArgumentParser
import struct Foo.Bar
import XCTest
final class ParserTests: XCTestCase {
    func testParse() { helper() }
    func helper() {}
}
@Suite struct Checks { @Test func works() {} }
struct Lib { func notATest() {} }
"#,
    );
    let imps: Vec<(String, String)> = f.imports.iter().map(|i| (i.module.clone(), i.names[0].name.clone())).collect();
    assert_eq!(
        imps,
        [("ArgumentParser".into(), "*".into()), ("Foo".into(), "Bar".into()), ("XCTest".into(), "*".into())]
    );
    let test = |qn: &str| f.definitions.iter().find(|d| d.qualified_name == qn).map(|d| d.is_test);
    assert_eq!(test("ParserTests"), Some(true));
    assert_eq!(test("ParserTests.testParse()"), Some(true));
    assert_eq!(test("Checks.works()"), Some(true));
    assert_eq!(test("Lib.notATest()"), Some(false));
    let parser = f.definitions.iter().find(|d| d.name == "ParserTests").unwrap();
    assert_eq!(parser.bases, ["XCTestCase"]);
}

#[test]
fn a_local_shadowing_itself_terminates() {
    let f = facts(
        r#"
func adapt(_ request: Request) throws -> Request {
    var request = try request.adapted()
    let urlRequest = request.build()
    return urlRequest.finish()
}
"#,
    );
    assert!(f.references.iter().any(|r| r.name == "finish"));
}
