use super::*;
use rueca_model::{ContractKind, ContractSide, Definition, Label};

const FIXTURE: &str = include_str!("../tests/fixtures/helloworld.proto");

fn facts(src: &str) -> FileFacts {
    ProtoPack.extract("x.proto", Language::Proto, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no {qn} in {:#?}", f.definitions))
}

fn id(f: &FileFacts, qn: &str) -> u32 {
    f.definitions.iter().position(|d| d.qualified_name == qn).unwrap() as u32
}

/// The source text a span covers.
fn text<'s>(src: &'s str, d: &Definition) -> &'s str {
    &src[d.span.start_byte as usize..d.span.end_byte as usize]
}

#[test]
fn services_and_rpc_methods() {
    let f = facts(FIXTURE);
    assert_eq!(f.parse_errors, 0);
    let greeter = def(&f, "helloworld.v1.Greeter");
    assert_eq!((greeter.label, greeter.name.as_str()), (Label::Interface, "Greeter"));
    assert_eq!(greeter.signature.as_deref(), Some("service Greeter"));
    assert_eq!(greeter.doc.as_deref(), Some("The greeting service."));
    assert!(text(FIXTURE, greeter).starts_with("service Greeter {") && text(FIXTURE, greeter).ends_with('}'));
    let hello = def(&f, "helloworld.v1.Greeter.SayHello");
    assert_eq!((hello.label, hello.name.as_str()), (Label::RpcMethod, "SayHello"));
    assert_eq!(hello.parent, Some(id(&f, "helloworld.v1.Greeter")));
    assert_eq!(hello.signature.as_deref(), Some("rpc SayHello(HelloRequest) returns (HelloReply)"));
    assert_eq!(hello.doc.as_deref(), Some("Sends a greeting."));
    assert_eq!(text(FIXTURE, hello), "rpc SayHello (HelloRequest) returns (HelloReply) {}");
    assert_eq!(&FIXTURE[hello.name_span.start_byte as usize..hello.name_span.end_byte as usize], "SayHello");
    assert_eq!(hello.span.start_line + 1, 14);
    let stream = def(&f, "helloworld.v1.Greeter.SayHelloStream");
    assert_eq!(
        stream.signature.as_deref(),
        Some("rpc SayHelloStream(stream HelloRequest) returns (stream HelloReply)")
    );
    let reset = def(&f, "helloworld.v1.Admin.Reset");
    assert_eq!(reset.signature.as_deref(), Some("rpc Reset(google.protobuf.Empty) returns (google.protobuf.Empty)"));
    assert!(text(FIXTURE, reset).contains("option deprecated = true;"));
    assert!(f.definitions.iter().all(|d| d.exported && !d.is_test));
}

#[test]
fn messages_fields_and_enums() {
    let f = facts(FIXTURE);
    let req = def(&f, "helloworld.v1.HelloRequest");
    assert_eq!((req.label, req.parent), (Label::Struct, None));
    assert_eq!(req.doc.as_deref(), Some("A request with a name."));
    let counts = def(&f, "helloworld.v1.HelloRequest.counts");
    assert_eq!(counts.label, Label::Field);
    assert_eq!(counts.parent, Some(id(&f, "helloworld.v1.HelloRequest")));
    assert_eq!(counts.signature.as_deref(), Some("map<string, int32> counts = 3"));
    assert_eq!(def(&f, "helloworld.v1.HelloRequest.tags").signature.as_deref(), Some("repeated string tags = 2"));
    // Nested message and nested enum; no synthetic map entry type.
    let origin = def(&f, "helloworld.v1.HelloRequest.Origin");
    assert_eq!((origin.label, origin.parent), (Label::Struct, Some(id(&f, "helloworld.v1.HelloRequest"))));
    assert_eq!(origin.doc.as_deref(), Some("Where the request comes from."));
    assert_eq!(def(&f, "helloworld.v1.HelloRequest.Origin.kind").signature.as_deref(), Some("Kind kind = 2"));
    assert!(!f.definitions.iter().any(|d| d.name.ends_with("Entry")));
    let kind = def(&f, "helloworld.v1.HelloRequest.Kind");
    assert_eq!((kind.label, kind.parent), (Label::Enum, Some(id(&f, "helloworld.v1.HelloRequest"))));
    let mood = def(&f, "helloworld.v1.Mood");
    assert_eq!((mood.label, mood.signature.as_deref()), (Label::Enum, Some("enum Mood")));
    let happy = def(&f, "helloworld.v1.Mood.MOOD_HAPPY");
    assert_eq!((happy.label, happy.signature.as_deref()), (Label::Constant, Some("MOOD_HAPPY = 1")));
    assert_eq!(happy.parent, Some(id(&f, "helloworld.v1.Mood")));
    let labels = |l: Label| f.definitions.iter().filter(|d| d.label == l).count();
    assert_eq!(
        [labels(Label::Interface), labels(Label::RpcMethod), labels(Label::Struct), labels(Label::Enum)],
        [2, 3, 3, 2]
    );
    assert_eq!(labels(Label::Field), 4 + 2 + 2);
    assert_eq!(labels(Label::Constant), 4);
    assert_eq!(f.definitions.len(), 2 + 3 + 3 + 2 + 8 + 4);
}

#[test]
fn imports_and_contract_hints() {
    let f = facts(FIXTURE);
    let imports: Vec<&str> = f.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(imports, ["google/protobuf/empty.proto", "common/status.proto"]);
    let i = &f.imports[1];
    assert_eq!(&FIXTURE[i.span.start_byte as usize..i.span.end_byte as usize], "import \"common/status.proto\";");
    let keys: Vec<&str> = f.contracts.iter().map(|h| h.key.as_str()).collect();
    assert_eq!(
        keys,
        ["helloworld.v1.Greeter/SayHello", "helloworld.v1.Greeter/SayHelloStream", "helloworld.v1.Admin/Reset"]
    );
    for h in &f.contracts {
        assert_eq!((h.side, h.kind, h.framework.as_str()), (ContractSide::Provides, ContractKind::Rpc, "proto"));
        let owner = &f.definitions[h.owner.unwrap() as usize];
        assert_eq!(owner.label, Label::RpcMethod);
        assert_eq!(h.span, owner.span);
        assert_eq!(Some(h.raw.as_str()), owner.signature.as_deref());
    }
}

#[test]
fn no_package_bad_input_and_budget() {
    let f = facts("syntax = \"proto3\";\nservice S { rpc M(A) returns (B); }\nmessage A {}\nmessage B {}\n");
    assert_eq!(def(&f, "S.M").signature.as_deref(), Some("rpc M(A) returns (B)"));
    assert_eq!(f.contracts[0].key, "S/M");
    assert_eq!(f.line_count, 4);
    let bad = facts("syntax = \"proto3\";\nmessage {\n");
    assert_eq!((bad.parse_errors, bad.definitions.len()), (1, 0));
    let lossy = ProtoPack.extract("x.proto", Language::Proto, b"message A {}\n// \xff\n", &Budget::default()).unwrap();
    assert_eq!((lossy.parse_errors, lossy.definitions.len()), (1, 1));
    let tiny = Budget { max_bytes: 4, ..Budget::default() };
    assert!(matches!(
        ProtoPack.extract("x.proto", Language::Proto, FIXTURE.as_bytes(), &tiny),
        Err(ExtractError::TooLarge { .. })
    ));
    assert!(matches!(
        ProtoPack.extract("x.proto", Language::Json, b"{}", &Budget::default()),
        Err(ExtractError::Unsupported(Language::Json))
    ));
    assert_eq!(Language::from_path("api/v1/greeter.proto"), Some(Language::Proto));
}

#[test]
fn import_resolution() {
    let project = ProjectFiles {
        paths: ["common/status.proto", "proto/acme/a.proto", "proto/acme/b.proto"].map(String::from).into(),
        configs: Default::default(),
    };
    let r = |from: &str, m: &str| resolve_import(from, m, &project);
    assert_eq!(r("api/x.proto", "common/status.proto").as_deref(), Some("common/status.proto"));
    assert_eq!(r("proto/acme/a.proto", "acme/b.proto").as_deref(), Some("proto/acme/b.proto"));
    assert_eq!(r("proto/acme/a.proto", "b.proto").as_deref(), Some("proto/acme/b.proto"));
    assert_eq!(r("api/x.proto", "google/protobuf/empty.proto"), None);
    assert_eq!(r("api/x.proto", ""), None);
}

#[test]
fn deterministic() {
    assert_eq!(facts(FIXTURE), facts(FIXTURE));
}
