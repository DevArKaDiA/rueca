use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn run(path: &str, src: &str) -> FileFacts {
    let lang = Language::from_path(path).unwrap_or_else(|| panic!("no language for {path}"));
    extract(path, lang, src.as_bytes(), &Budget::default()).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fixture(path: &str) -> FileFacts {
    let src = std::fs::read_to_string(format!("{FIXTURES}/{path}")).unwrap();
    run(path, &src)
}

fn dump(f: &FileFacts) -> String {
    let mut s = String::new();
    for (i, d) in f.definitions.iter().enumerate() {
        s.push_str(&format!(
            "{i} {:?} {} | {} | parent {:?} | L{}-{} | sig {:?} | doc {:?} | bases {:?}\n",
            d.label,
            d.name,
            d.qualified_name,
            d.parent,
            d.span.start_line + 1,
            d.span.end_line + 1,
            d.signature,
            d.doc,
            d.bases
        ));
    }
    for r in &f.references {
        s.push_str(&format!(
            "ref {:?} {} recv {:?} enc {:?} L{}\n",
            r.kind,
            r.name,
            r.receiver,
            r.enclosing,
            r.span.start_line + 1
        ));
    }
    for i in &f.imports {
        s.push_str(&format!("import {}\n", i.module));
    }
    s.push_str(&format!("errors {} lines {}\n", f.parse_errors, f.line_count));
    s
}

#[test]
#[ignore]
fn dump_fixtures() {
    let mut paths = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(FIXTURES)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                paths.push(p.strip_prefix(FIXTURES).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    paths.sort();
    for p in paths {
        if let Some(lang) = Language::from_path(&p).filter(|l| LANGUAGES.contains(l)) {
            let src = std::fs::read(format!("{FIXTURES}/{p}")).unwrap();
            println!("=== {p} ({lang})");
            match extract(&p, lang, &src, &Budget::default()) {
                Ok(f) => println!("{}", dump(&f)),
                Err(e) => println!("ERR {e}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no {qn} in\n{}", dump(f)))
}

fn has(f: &FileFacts, qn: &str) -> bool {
    f.definitions.iter().any(|d| d.qualified_name == qn)
}

fn qns(f: &FileFacts, label: Label) -> Vec<&str> {
    f.definitions.iter().filter(|d| d.label == label).map(|d| d.qualified_name.as_str()).collect()
}

fn enc(f: &FileFacts, r: &Reference) -> String {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone()).unwrap_or_default()
}

/// `(kind, name, receiver, enclosing)` of every reference.
fn refs(f: &FileFacts) -> Vec<(RefKind, String, Option<String>, String)> {
    f.references.iter().map(|r| (r.kind, r.name.clone(), r.receiver.clone(), enc(f, r))).collect()
}

fn has_ref(f: &FileFacts, kind: RefKind, name: &str, receiver: Option<&str>, enclosing: &str) -> bool {
    refs(f).iter().any(|(k, n, r, e)| *k == kind && n == name && r.as_deref() == receiver && e == enclosing)
}

fn imports(f: &FileFacts) -> Vec<&str> {
    f.imports.iter().map(|i| i.module.as_str()).collect()
}

fn lines(d: &Definition) -> (u32, u32) {
    d.span.lines_1based()
}

fn skipped(path: &str, src: &[u8]) -> String {
    let lang = Language::from_path(path).unwrap();
    match extract(path, lang, src, &Budget::default()) {
        Err(ExtractError::Skipped(r)) => r,
        other => panic!("{path}: expected a skip, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

#[test]
fn json_keys_paths_spans_and_docs() {
    let f = run(
        "conf/app.json",
        "{\n  // Server settings.\n  \"server\": {\"port\": 8080, \"hosts\": [\"a\", \"b\"]},\n  \"a.b\": {\"\": 1},\n  \"list\": [{\"x\": 1}, 2, {\"y\": {\"z\": true}}],\n  \"token\": \"s3cr3t\", // trailing\n}\n",
    );
    assert_eq!(
        f.definitions.iter().map(|d| d.qualified_name.as_str()).collect::<Vec<_>>(),
        [
            "server",
            "server.port",
            "server.hosts",
            "\"a.b\"",
            "\"a.b\".\"\"",
            "list",
            "list[0]",
            "list[0].x",
            "list[2]",
            "list[2].y",
            "list[2].y.z",
            "token"
        ]
    );
    let server = def(&f, "server");
    assert_eq!(server.label, Label::Key);
    assert_eq!(server.signature.as_deref(), Some("server: {port, hosts}"));
    assert_eq!(server.doc.as_deref(), Some("Server settings."));
    assert_eq!(lines(server), (3, 3));
    assert_eq!(server.name_span.start_col, 2);
    assert_eq!(def(&f, "server.hosts").signature.as_deref(), Some("hosts: [2 items]"));
    assert_eq!(def(&f, "server.port").parent, Some(0));
    assert_eq!(def(&f, "\"a.b\"").name, "a.b");
    assert_eq!(def(&f, "list[2]").name, "[2]");
    // Secret-looking names never copy their value; trailing comments are docs.
    let token = def(&f, "token");
    assert_eq!(token.signature.as_deref(), Some("token: •••"));
    assert_eq!(token.doc.as_deref(), Some("trailing"));
    assert_eq!(f.parse_errors, 0);
    assert_eq!(f.line_count, 7);
    // Broken JSON is a parser skip, not a panic.
    let err = extract("x.json", Language::Json, b"{\"a\": [1, }", &Budget::default());
    assert!(matches!(err, Err(ExtractError::Parser(_))), "{err:?}");
}

#[test]
fn yaml_documents_anchors_merges_and_utf8() {
    let f = fixture("config/anchors.yaml");
    assert_eq!(def(&f, "development").bases, ["defaults"]);
    assert_eq!(def(&f, "test").bases, ["defaults"]);
    assert!(!has(&f, "development.<<"));
    assert!(has_ref(&f, RefKind::DataRef, "defaults", None, "mirror"));
    assert_eq!(def(&f, "hosts").signature.as_deref(), Some("hosts: [3 items]"));

    // Multi-document streams: `{N}` roots for documents without identity.
    let f = run("x/multi.yaml", "a: 1\n---\nb:\n  c: é\n---\n---\nd: [1]\n");
    assert_eq!(qns(&f, Label::Document), ["{0}", "{1}", "{3}"]);
    assert!(has(&f, "{1}.b.c") && has(&f, "{3}.d"));
    assert_eq!(def(&f, "{1}.b.c").parent, Some(def_id(&f, "{1}.b")));
    // Byte offsets survive multi-byte characters.
    let c = def(&f, "{1}.b.c");
    assert_eq!((c.span.start_byte, c.span.end_byte), (14, 19));
    assert_eq!(c.signature.as_deref(), Some("c: é"));

    // A syntax error keeps what was read and marks the file partial.
    let f = run("x/broken.yaml", "a: 1\nb: [unclosed\nc: 2\n");
    assert!(has(&f, "a"));
    assert!(f.parse_errors > 0);
}

fn def_id(f: &FileFacts, qn: &str) -> u32 {
    f.definitions.iter().position(|d| d.qualified_name == qn).unwrap() as u32
}

#[test]
fn toml_tables_dotted_keys_and_arrays_of_tables() {
    let f = run(
        "Cargo.toml",
        "[package]\nname = \"x\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n\n[[bin]]\nname = \"a\"\n\n[[bin]]\nname = \"b\"\n\n[a.b]\nc.d = 1\n\n[package.metadata]\nk = true\n",
    );
    for qn in [
        "package",
        "package.name",
        "package.metadata.k",
        "dependencies.serde.version",
        "dependencies.serde.features",
        "bin",
        "bin[0].name",
        "bin[1].name",
        "a",
        "a.b",
        "a.b.c",
        "a.b.c.d",
    ] {
        assert!(has(&f, qn), "{qn}\n{}", dump(&f));
    }
    // A reopened table keeps one node spanning from its first header.
    assert_eq!(lines(def(&f, "package")).0, 1);
    assert_eq!(def(&f, "bin[1]").name, "[1]");
    assert_eq!(def(&f, "dependencies.serde").signature.as_deref(), Some("serde: {version, features}"));
}

#[test]
fn dotenv_ini_properties() {
    let f = fixture(".env.example");
    assert_eq!(qns(&f, Label::EnvVar), ["DATABASE_URL", "REDIS_URL", "API_TOKEN", "LOG_LEVEL", "CACHE_URL"]);
    let db = def(&f, "DATABASE_URL");
    // Env var values never reach the index.
    assert_eq!(db.signature.as_deref(), Some("DATABASE_URL=•••"));
    assert_eq!(db.doc.as_deref(), Some("Connection string for the main database."));
    assert!(f.definitions.iter().all(|d| !d.signature.as_deref().unwrap_or("").contains("secret")));
    assert!(has_ref(&f, RefKind::EnvVar, "REDIS_URL", None, "CACHE_URL"));

    let f = fixture("config/settings.ini");
    assert_eq!(qns(&f, Label::Key), ["database", "database.host", "database.port", "cache", "cache.url"]);
    assert_eq!(def(&f, "database").doc.as_deref(), Some("Global settings."));
    let f = fixture("setup.cfg");
    assert!(has(&f, "\"options.entry_points\".console_scripts"));

    // `.properties`: one key per line, split into a key path.
    let f = fixture("src/main/resources/application-prod.properties");
    assert_eq!(qns(&f, Label::Key), ["server.port", "spring.datasource.password"]);
    assert_eq!(def(&f, "server.port").name, "port");
    assert_eq!(def(&f, "spring.datasource.password").signature.as_deref(), Some("spring.datasource.password: •••"));
    assert!(has_ref(&f, RefKind::EnvVar, "DB_PASSWORD", None, "spring.datasource.password"));
}

#[test]
fn xml_kinds() {
    let f = fixture("pom.xml");
    assert!(has(&f, "dependencies.dependency[\"org.postgresql:postgresql\"].artifactId"));
    assert_eq!(
        def(&f, "dependencies.dependency[\"org.springframework.boot:spring-boot-starter-web\"]").doc.as_deref(),
        Some("Web layer.")
    );
    assert_eq!(imports(&f), ["core"]);
    let f = fixture("dotnet/Shop/Shop.csproj");
    assert!(has(&f, "ItemGroup.PackageReference[\"Serilog\"]"));
    assert_eq!(imports(&f), ["../Lib/Lib.csproj"]);
    let f = run(
        "app/src/main/AndroidManifest.xml",
        "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\"><application><activity android:name=\".Main\"/><activity android:name=\".Settings\"/></application></manifest>",
    );
    assert!(has(&f, "application.activity[\".Main\"]") && has(&f, "application.activity[\".Settings\"]"));
    // DTDs (and so entity expansion) are refused.
    let bomb = "<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\"><!ENTITY lol2 \"&lol;&lol;\">]><project><a>&lol2;</a></project>";
    assert!(matches!(
        extract("pom.xml", Language::Xml, bomb.as_bytes(), &Budget::default()),
        Err(ExtractError::Parser(_))
    ));
}

#[test]
fn hcl_terraform() {
    let f = fixture("infra/main.tf");
    assert_eq!(qns(&f, Label::Resource), ["aws_s3_bucket.logs", "module.net", "data.aws_caller_identity.current"]);
    assert_eq!(qns(&f, Label::Variable), ["local.env"]);
    let logs = def(&f, "aws_s3_bucket.logs");
    assert_eq!(logs.name, "logs");
    assert_eq!(logs.signature.as_deref(), Some("resource \"aws_s3_bucket\" \"logs\""));
    assert_eq!(logs.doc.as_deref(), Some("Bucket for access logs."));
    assert_eq!(lines(logs), (2, 7));
    assert_eq!(def(&f, "module.net").signature.as_deref(), Some("module \"net\" (\"./modules/net\")"));
    assert_eq!(def(&f, "output.bucket_arn").doc.as_deref(), Some("ARN of the log bucket."));
    assert!(has(&f, "aws_s3_bucket.logs.tags"));
    assert_eq!(imports(&f), ["./modules/net"]);
    for (name, from) in [
        ("var.prefix", "aws_s3_bucket.logs"),
        ("local.env", "aws_s3_bucket.logs"),
        ("var.cidr", "module.net"),
        ("aws_s3_bucket.logs", "output.bucket_arn"),
    ] {
        assert!(has_ref(&f, RefKind::DataRef, name, None, from), "{name} from {from}\n{}", dump(&f));
    }
    let f = fixture("infra/variables.tf");
    assert_eq!(def(&f, "var.prefix").signature.as_deref(), Some("variable \"prefix\" = \"shop\""));
    assert_eq!(def(&f, "var.prefix").doc.as_deref(), Some("Name prefix."));
    let f = fixture("infra/prod.tfvars");
    assert_eq!(qns(&f, Label::Key), ["prefix", "cidr"]);
    // Repeated nested blocks are indexed; secrets are masked.
    let f = run(
        "sg.tf",
        "resource \"aws_security_group\" \"web\" {\n  ingress { from_port = 80 }\n  ingress { from_port = 443 }\n  password = \"x\"\n}\n",
    );
    assert!(has(&f, "aws_security_group.web.ingress[0].from_port"));
    assert!(has(&f, "aws_security_group.web.ingress[1]"));
    assert_eq!(def(&f, "aws_security_group.web.password").signature.as_deref(), Some("password = •••"));
}

#[test]
fn dockerfile_stages() {
    let f = fixture("docker/Dockerfile");
    assert_eq!(qns(&f, Label::Image), ["build", "test", "stage[2]"]);
    assert_eq!(def(&f, "test").bases, ["build"]);
    let build = def(&f, "build");
    assert_eq!(build.signature.as_deref(), Some("FROM python:${PYTHON_VERSION}-slim AS build"));
    assert_eq!(build.doc.as_deref(), Some("Build stage: install dependencies."));
    assert_eq!(lines(build), (5, 10));
    assert_eq!(qns(&f, Label::EnvVar), ["build.PIP_NO_CACHE_DIR", "build.APP_HOME", "stage[2].DATABASE_URL"]);
    assert_eq!(def(&f, "stage[2].DATABASE_URL").signature.as_deref(), Some("ENV DATABASE_URL=•••"));
    assert_eq!(qns(&f, Label::Variable), ["PYTHON_VERSION", "stage[2].RELEASE"]);
    // The `# syntax=` directive is not a doc.
    assert_eq!(def(&f, "PYTHON_VERSION").doc, None);
    assert!(has_ref(&f, RefKind::DataRef, "build", None, "stage[2]"));
    // Legacy `ENV K v`, `COPY --from=<index>`, heredocs, backtick escapes.
    let f = run(
        "Dockerfile",
        "# escape=`\nFROM alpine\nENV PATH_X /usr/bin\nRUN <<EOF\nFROM fake AS nope\nEOF\nFROM alpine AS final\nCOPY --from=0 /a /b\nRUN echo `\n  done\n",
    );
    assert_eq!(qns(&f, Label::Image), ["stage[0]", "final"]);
    assert_eq!(qns(&f, Label::EnvVar), ["stage[0].PATH_X"]);
    assert!(has_ref(&f, RefKind::DataRef, "stage[0]", None, "final"));
}

// ---------------------------------------------------------------------------
// File kinds
// ---------------------------------------------------------------------------

#[test]
fn package_json_and_tsconfig() {
    let f = fixture("package.json");
    assert_eq!(imports(&f), ["./web/server.ts", "./web/cli.js", "./web/types.d.ts"]);
    assert_eq!(def(&f, "scripts.build").signature.as_deref(), Some("build: \"tsc -p .\""));
    assert!(has(&f, "dependencies.@types/node"));
    let f = fixture("tsconfig.json");
    assert_eq!(imports(&f), ["./tsconfig.base"]);
    assert!(has(&f, "compilerOptions.paths.@/*"));
    let f = fixture("tsconfig.base.json");
    assert_eq!(def(&f, "compilerOptions").doc.as_deref(), Some("Shared compiler options."));
    let f = fixture("composer.json");
    assert!(has(&f, "autoload.psr-4.\"App\\\\\""));
}

#[test]
fn pyproject_scripts_reference_python() {
    let f = fixture("pyproject.toml");
    assert!(has_ref(&f, RefKind::DataRef, "app.main.main", Some("$qn"), "project.scripts.shop"));
    assert_eq!(def(&f, "project.scripts").doc.as_deref(), Some("Console entry points."));
    assert!(has(&f, "tool.mypy.overrides[0].strict"));
}

#[test]
fn compose_services_env_and_build() {
    let f = fixture("compose.yaml");
    assert_eq!(qns(&f, Label::Resource), ["services.web", "services.worker", "services.db", "services.cache"]);
    assert_eq!(qns(&f, Label::Image), ["services.web.build"]);
    assert_eq!(def(&f, "services.db").signature.as_deref(), Some("service db: image postgres:16"));
    assert_eq!(
        qns(&f, Label::EnvVar),
        [
            "services.web.environment.DATABASE_URL",
            "services.web.environment.LOG_LEVEL",
            "services.db.environment.POSTGRES_PASSWORD",
            "services.worker.environment[\"QUEUE\"]",
            "services.worker.environment[\"SENTRY_DSN\"]"
        ]
    );
    let q = def(&f, "services.worker.environment[\"QUEUE\"]");
    assert_eq!((q.name.as_str(), q.signature.as_deref()), ("QUEUE", Some("QUEUE=•••")));
    assert_eq!(def(&f, "services.worker.environment[\"SENTRY_DSN\"]").signature.as_deref(), Some("SENTRY_DSN"));
    assert_eq!(def(&f, "services.worker").bases, ["services.web"]);
    assert_eq!(imports(&f), ["./docker", "./docker/Dockerfile", ".env.example"]);
    assert!(has_ref(&f, RefKind::DataRef, "services.db", None, "services.web"));
    // `${VAR}` interpolations are reads by the entry that holds them.
    assert!(has_ref(&f, RefKind::EnvVar, "DATABASE_URL", None, "services.web.environment.DATABASE_URL"));
    assert!(has_ref(&f, RefKind::EnvVar, "DB_PASSWORD", None, "services.db.environment.POSTGRES_PASSWORD"));
    // Detected by shape too.
    let f = run("deploy/stack.yml", "services:\n  api:\n    image: x\n    environment: [A=1]\n");
    assert_eq!(qns(&f, Label::Resource), ["services.api"]);
    assert_eq!(qns(&f, Label::EnvVar), ["services.api.environment[\"A\"]"]);
}

#[test]
fn kubernetes_manifests() {
    let f = fixture("k8s/web.yaml");
    assert_eq!(qns(&f, Label::Resource), ["Deployment/web", "Service/web"]);
    let dep = def(&f, "Deployment/web");
    assert_eq!(dep.signature.as_deref(), Some("Deployment web (apps/v1)"));
    let c = "Deployment/web.spec.template.spec.containers[\"web\"]";
    assert!(has(&f, c));
    let env: Vec<&str> = qns(&f, Label::EnvVar);
    assert_eq!(env.len(), 3);
    let db = def(&f, &format!("{c}.env[\"DATABASE_URL\"]"));
    assert_eq!((db.label, db.name.as_str()), (Label::EnvVar, "DATABASE_URL"));
    assert_eq!(db.signature.as_deref(), Some("DATABASE_URL: •••"));
    let from = format!("{c}.env[\"LOG_LEVEL\"]");
    assert!(has_ref(&f, RefKind::DataRef, "ConfigMap/shop-config.data.LOG_LEVEL", Some("$any"), &from));
    assert!(has_ref(
        &f,
        RefKind::DataRef,
        "Secret/shop-secrets.stringData.database-url",
        Some("$any"),
        &format!("{c}.env[\"DATABASE_URL\"]")
    ));
    assert!(has_ref(&f, RefKind::DataRef, "ConfigMap/shop-config", Some("$any"), c));
    assert!(has(&f, "Service/web.spec.ports[\"http\"].port"));
    let f = fixture("k8s/config.yaml");
    assert!(has(&f, "ConfigMap/shop-config.data.\"app.properties\""));
    assert_eq!(def(&f, "Secret/shop-secrets.stringData.database-url").signature.as_deref(), Some("database-url: •••"));
}

#[test]
fn github_actions() {
    let f = fixture(".github/workflows/ci.yml");
    assert_eq!(def(&f, "jobs.lint").signature.as_deref(), Some("lint: runs-on ubuntu-latest"));
    assert_eq!(def(&f, "jobs.release").signature.as_deref(), Some("release: uses ./.github/workflows/release.yml"));
    let step = def(&f, "jobs.test.steps[\"tests\"]");
    assert_eq!(step.signature.as_deref(), Some("tests: Run tests (run pytest --db \"${{ env.DATABASE_URL }}\")"));
    assert!(has(&f, "jobs.lint.steps[1]"));
    assert!(has_ref(&f, RefKind::DataRef, "jobs.lint", None, "jobs.test"));
    assert!(has_ref(&f, RefKind::DataRef, "jobs.test", None, "jobs.release"));
    assert_eq!(imports(&f), ["./.github/actions/setup", "./.github/workflows/release.yml"]);
    assert_eq!(qns(&f, Label::EnvVar), ["env.PYTHON_VERSION", "jobs.test.env.DATABASE_URL"]);
    assert!(has_ref(&f, RefKind::EnvVar, "DATABASE_URL", None, "jobs.test.steps[\"tests\"].run"));
}

#[test]
fn openapi_and_json_schema() {
    let f = fixture("api/openapi.yaml");
    let get = def(&f, "paths./users/{id}.get");
    assert_eq!((get.label, get.name.as_str()), (Label::Route, "GET /users/{id}"));
    assert_eq!(get.signature.as_deref(), Some("GET /users/{id} (getUser)"));
    assert_eq!(get.doc.as_deref(), Some("Fetch one user."));
    assert_eq!(def(&f, "components.schemas.User").doc.as_deref(), Some("A shop customer."));
    let schema = "paths./users/{id}.get.responses.200.content.application/json.schema";
    assert!(has_ref(&f, RefKind::DataRef, "components.schemas.User", None, schema));
    assert!(has_ref(&f, RefKind::DataRef, "components.parameters.UserId", None, "paths./users/{id}.get.parameters[0]"));
    assert!(has_ref(
        &f,
        RefKind::DataRef,
        "components.responses.NotFound",
        Some("./common.yaml"),
        "paths./users/{id}.get.responses.404"
    ));
    assert_eq!(imports(&f), ["./common.yaml"]);
    // `$ref` entries are pointers, not keys.
    assert!(!f.definitions.iter().any(|d| d.name == "$ref"));
    let f = fixture("schemas/user.schema.json");
    assert_eq!(def(&f, "$defs.Email").doc.as_deref(), Some("An email address."));
    assert!(has_ref(&f, RefKind::DataRef, "$defs.Email", None, "properties.email"));
    // Pointers decode `~1` and index into arrays.
    let f = run(
        "spec.json",
        "{\"openapi\": \"3.1.0\", \"paths\": {\"/a/b\": {\"get\": {\"responses\": {}}}}, \"x\": [{\"q\": 1}], \"r\": {\"$ref\": \"#/paths/~1a~1b/get\"}, \"s\": {\"$ref\": \"#/x/0/q\"}}",
    );
    assert!(has_ref(&f, RefKind::DataRef, "paths./a/b.get", None, "r"));
    assert!(has_ref(&f, RefKind::DataRef, "x[0].q", None, "s"));
}

#[test]
fn spring_config() {
    let f = fixture("src/main/resources/application.yml");
    // Dotted YAML keys are split like `.properties` keys.
    assert!(has(&f, "spring.datasource.url"));
    assert_eq!(def(&f, "spring.datasource").name, "datasource");
    assert!(has_ref(&f, RefKind::EnvVar, "DB_URL", None, "spring.datasource.url"));
    assert!(has_ref(&f, RefKind::ConfigKey, "spring.application.name", Some("spring"), "spring.datasource.username"));
}

// ---------------------------------------------------------------------------
// What is not indexed (RFC 0003 §4.5) and budgets (§4.7)
// ---------------------------------------------------------------------------

#[test]
fn skips_by_place_marker_size_and_shape() {
    assert_eq!(skipped("test/fixtures/x.json", b"{\"a\": 1}"), "data");
    assert_eq!(skipped("pkg/testdata/k8s.yaml", b"a: 1"), "data");
    assert_eq!(skipped("src/__snapshots__/a.json", b"{}"), "data");
    assert_eq!(skipped("tests/test/data/a.toml", b"a = 1"), "data");
    assert_eq!(skipped("web/app.min.json", b"{}"), "data");
    assert_eq!(skipped("gen/schema.json", b"// Code generated by protoc. DO NOT EDIT.\n{}"), "generated");
    assert_eq!(skipped("gen/x.yaml", b"# @generated\na: 1\n"), "generated");
    assert_eq!(skipped("chart/templates/deploy.yaml", b"kind: {{ .Values.kind }}\n"), "template");
    // Large generic data: size; large known kinds get the general budget.
    let big: String = format!("{{\"items\": \"{}\"}}", "x".repeat(MAX_DATA_FILE_BYTES));
    assert_eq!(skipped("data/big.json", big.as_bytes()), "data-size");
    let mut spec = String::from("openapi: 3.0.0\npaths:\n");
    for i in 0..3_000 {
        spec.push_str(&format!("  /p{i}:\n    get:\n      summary: {}\n", "x".repeat(100)));
    }
    assert!(spec.len() > MAX_DATA_FILE_BYTES);
    let f = run("api/big.yaml", &spec);
    assert!(f.parse_errors > 0, "a capped kind file is partial");
    // The cap counts keys as the generic walk emits them (before kind
    // labels such as `Route` apply).
    let keys = f.definitions.iter().filter(|d| d.label == Label::Key).count();
    assert!(keys <= MAX_KEYS && keys > MAX_KEYS / 2, "{keys}");
    assert!(f.definitions.iter().filter(|d| d.label == Label::Route).count() > 1_000);
    // Shapes: long root arrays, too many keys, element-heavy dumps.
    let arr = format!("[{}]", vec!["{\"a\": 1}"; 101].join(","));
    assert_eq!(skipped("rows.json", arr.as_bytes()), "data-shape");
    let many: Vec<String> = (0..MAX_KEYS + 1).map(|i| format!("\"k{i}\": 1")).collect();
    assert_eq!(skipped("many.json", format!("{{{}}}", many.join(",")).as_bytes()), "data-shape");
    let dump = format!("{{\"rows\": [{}]}}", vec!["{}"; 30].join(","));
    assert_eq!(skipped("dump.json", dump.as_bytes()), "data-shape");
    // A budget overflow is `TooLarge`, as for code.
    let err = extract("a.json", Language::Json, &[b' '; 64], &Budget { max_bytes: 10, ..Budget::default() });
    assert!(matches!(err, Err(ExtractError::TooLarge { .. })));
}

#[test]
fn depth_and_element_caps() {
    // 40 nested keys: keys stop at depth 32.
    let mut src = String::new();
    for i in 0..40 {
        src.push_str(&format!("{}k{i}:\n", "  ".repeat(i)));
    }
    src.push_str(&format!("{}leaf: 1\n", "  ".repeat(40)));
    let f = run("deep.yaml", &src);
    let max = f.definitions.iter().map(|d| keypath::parse(&d.qualified_name).unwrap().len()).max().unwrap();
    assert_eq!(max, MAX_DEPTH);
    // At most 64 mapping elements per array.
    let mut src = String::from("items:\n");
    for i in 0..80 {
        src.push_str(&format!("  - name: n{i}\n    v: 1\n"));
    }
    let f = run("list.yaml", &src);
    let elements = f.definitions.iter().filter(|d| d.parent == Some(0)).count();
    assert_eq!(elements, MAX_ELEMENTS);
}

// ---------------------------------------------------------------------------
// Untrusted input: never panic, bounded work
// ---------------------------------------------------------------------------

#[test]
fn billion_laughs_yaml_is_not_expanded() {
    let mut src = String::from("a: &a [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n");
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for w in names.windows(2) {
        src.push_str(&format!("{}: &{} [*{p},*{p},*{p},*{p},*{p},*{p},*{p},*{p},*{p}]\n", w[1], w[1], p = w[0]));
    }
    let t = std::time::Instant::now();
    let f = run("bomb.yaml", &src);
    assert!(t.elapsed().as_secs() < 2);
    assert_eq!(f.definitions.len(), 9);
    // Each alias is one reference to the anchored key, never a copy.
    assert!(has_ref(&f, RefKind::DataRef, "a", None, "b"));
    assert!(f.references.len() <= 9);
}

#[test]
fn deep_nesting_is_bounded() {
    for (path, src) in [
        ("deep.json", "[".repeat(100_000)),
        ("deep2.json", format!("{}1{}", "{\"a\":".repeat(600), "}".repeat(600))),
        ("deep.yaml", "[".repeat(100_000)),
        ("deep2.yaml", "{a: ".repeat(300) + &"}".repeat(300)),
        ("deep.toml", format!("a = {}1{}", "[".repeat(10_000), "]".repeat(10_000))),
        ("deep.tf", format!("a = {}1{}", "[".repeat(10_000), "]".repeat(10_000))),
        ("pom.xml", "<a>".repeat(50_000)),
    ] {
        let lang = Language::from_path(path).unwrap();
        // Ok (partial), a parser error or a skip: never a panic or overflow.
        let _ = extract(path, lang, src.as_bytes(), &Budget::default());
    }
    let mut deep_block = String::new();
    for i in 0..600 {
        deep_block.push_str(&format!("{}k:\n", " ".repeat(i)));
    }
    let f = run("block.yaml", &deep_block);
    assert!(f.definitions.len() <= MAX_DEPTH);
    assert_eq!(skipped("nest.tf", format!("a = {}", "{".repeat(200)).as_bytes()), "data-shape");
}

/// xorshift64*: deterministic pseudo-random bytes.
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed.max(1);
    move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

#[test]
fn random_and_mutated_inputs_never_panic() {
    let paths = [
        "a.json",
        "a.yaml",
        "a.toml",
        ".env",
        "a.ini",
        "a.properties",
        "pom.xml",
        "a.tf",
        "Dockerfile",
        "compose.yaml",
        ".github/workflows/x.yml",
        "package.json",
        "pyproject.toml",
    ];
    let alphabet = b"{}[]():,.=\"'\\#&*!|>-$ \n\tak0<>/;@~%\xc3\xa9";
    let mut next = rng(0x5eed);
    for round in 0..300 {
        let len = (next() % 400) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
        let path = paths[round % paths.len()];
        let lang = Language::from_path(path).unwrap();
        let a = extract(path, lang, &bytes, &Budget::default());
        let b = extract(path, lang, &bytes, &Budget::default());
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "deterministic");
    }
    // Every fixture, truncated at many points and with bytes flipped.
    let mut stack = vec![std::path::PathBuf::from(FIXTURES)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(FIXTURES).unwrap().to_string_lossy().replace('\\', "/");
            let Some(lang) = Language::from_path(&rel).filter(|l| LANGUAGES.contains(l)) else { continue };
            let src = std::fs::read(&p).unwrap();
            for cut in (0..src.len()).step_by(7) {
                let _ = extract(&rel, lang, &src[..cut], &Budget::default());
                let mut m = src.clone();
                m[cut] = alphabet[(next() % alphabet.len() as u64) as usize];
                let _ = extract(&rel, lang, &m, &Budget::default());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key recall against reference parsers (RFC 0003 §3.3)
// ---------------------------------------------------------------------------

/// Leaf key paths of a `serde_json` value, formatted like Rueca's.
fn leaves_json(v: &serde_json::Value, prefix: &str, depth: usize, out: &mut Vec<String>) {
    let serde_json::Value::Object(m) = v else { return };
    if depth >= MAX_DEPTH {
        return;
    }
    for (k, v) in m.iter().filter(|(k, _)| *k != "$ref") {
        let p = keypath::join(prefix, &keypath::key_segment(k));
        match v {
            serde_json::Value::Object(_) => leaves_json(v, &p, depth + 1, out),
            serde_json::Value::Array(items) => {
                out.push(p.clone());
                for (i, it) in items.iter().enumerate().take(MAX_ELEMENTS).filter(|(_, it)| it.is_object()) {
                    leaves_json(it, &keypath::join(&p, &keypath::index_segment(i)), depth + 2, out);
                }
            }
            _ => out.push(p),
        }
    }
}

fn recall(path: &str, reference: Vec<String>) {
    let lang = Language::from_path(path).unwrap();
    let src = std::fs::read(format!("{FIXTURES}/{path}")).unwrap();
    // The generic walk, without kind identities (k8s, steps).
    let src = std::str::from_utf8(&src).unwrap();
    let tree = parse_tree(lang, src).unwrap();
    let mut out = emit::Out::new(src, CommentStyle::None);
    emit::emit_tree(&mut out, &tree, Kind::Generic);
    let got: std::collections::HashSet<&str> =
        out.facts.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
    let missing: Vec<&String> = reference.iter().filter(|p| !got.contains(p.as_str())).collect();
    let covered = reference.len() - missing.len();
    assert!(covered * 100 >= reference.len() * 99, "{path}: missing {missing:?}");
}

#[test]
fn key_recall_against_reference_parsers() {
    for path in
        ["package.json", "tsconfig.base.json", "composer.json", "config/default.json", "schemas/user.schema.json"]
    {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{path}")).unwrap();
        let text: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let text = text.replace(",\n  }", "\n  }").replace(",\n}", "\n}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut leaves = Vec::new();
        leaves_json(&v, "", 0, &mut leaves);
        recall(path, leaves);
    }
    for path in ["api/openapi.yaml", "api/common.yaml", "compose.yaml", ".github/workflows/ci.yml", "k8s/config.yaml"] {
        use saphyr::LoadableYamlNode;
        let text = std::fs::read_to_string(format!("{FIXTURES}/{path}")).unwrap();
        let docs = saphyr::Yaml::load_from_str(&text).unwrap();
        let multi = docs.len() >= 2;
        let mut leaves = Vec::new();
        for (i, d) in docs.iter().enumerate() {
            let json = yaml_to_json(d);
            let prefix = if multi { format!("{{{i}}}") } else { String::new() };
            leaves_json(&json, &prefix, 0, &mut leaves);
        }
        recall(path, leaves);
    }
    {
        let path = "pyproject.toml";
        let text = std::fs::read_to_string(format!("{FIXTURES}/{path}")).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        let json = serde_json::to_value(&v).unwrap();
        let mut leaves = Vec::new();
        leaves_json(&json, "", 0, &mut leaves);
        recall(path, leaves);
    }
}

fn yaml_to_json(y: &saphyr::Yaml<'_>) -> serde_json::Value {
    use saphyr::{Scalar, Yaml};
    match y {
        Yaml::Mapping(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Yaml::Value(Scalar::String(s)) => s.to_string(),
                        Yaml::Value(Scalar::Integer(i)) => i.to_string(),
                        Yaml::Value(Scalar::Boolean(b)) => b.to_string(),
                        Yaml::Representation(s, _, _) => s.to_string(),
                        other => format!("{other:?}"),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        Yaml::Sequence(s) => serde_json::Value::Array(s.iter().map(yaml_to_json).collect()),
        _ => serde_json::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Imports and the pack contract
// ---------------------------------------------------------------------------

#[test]
fn import_resolution() {
    let project = ProjectFiles {
        paths: [
            "tsconfig.json",
            "tsconfig.base.json",
            ".github/workflows/ci.yml",
            ".github/actions/setup/action.yml",
            "infra/main.tf",
            "infra/modules/net/main.tf",
            "pom.xml",
            "core/pom.xml",
            "docker/Dockerfile",
            "compose.yaml",
            "api/openapi.yaml",
            "api/common.yaml",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        configs: Default::default(),
    };
    let r = |from: &str, spec: &str| resolve_import(from, spec, &project);
    assert_eq!(r("tsconfig.json", "./tsconfig.base").as_deref(), Some("tsconfig.base.json"));
    assert_eq!(r("tsconfig.json", "@tsconfig/node20/tsconfig.json"), None);
    assert_eq!(
        r(".github/workflows/ci.yml", "./.github/actions/setup").as_deref(),
        Some(".github/actions/setup/action.yml")
    );
    assert_eq!(r("infra/main.tf", "./modules/net").as_deref(), Some("infra/modules/net"));
    assert_eq!(r("infra/main.tf", "git::https://x/y.git"), None);
    assert_eq!(r("infra/main.tf", "terraform-aws-modules/vpc/aws"), None);
    assert_eq!(r("pom.xml", "core").as_deref(), Some("core/pom.xml"));
    assert_eq!(r("compose.yaml", "./docker").as_deref(), Some("docker"));
    assert_eq!(r("api/openapi.yaml", "./common.yaml").as_deref(), Some("api/common.yaml"));
    assert_eq!(r("api/openapi.yaml", "../../x.yaml"), None);
    assert_eq!(r("api/openapi.yaml", "https://example.com/x.yaml"), None);
}

#[test]
fn pack_contract() {
    let p = DataPack;
    assert!(p.facts_depend_on_path());
    assert_eq!(p.module_scope(Language::Hcl), ModuleScope::Directory);
    assert_eq!(p.module_scope(Language::Yaml), ModuleScope::File);
    assert_eq!(p.module_name("config/app.yaml", &ProjectFiles::default()), "config/app.yaml");
    for l in [
        Language::Json,
        Language::Yaml,
        Language::Toml,
        Language::DotEnv,
        Language::Ini,
        Language::Properties,
        Language::Xml,
        Language::Hcl,
        Language::Dockerfile,
    ] {
        assert!(p.languages().contains(&l), "{l}");
    }
    assert!(matches!(p.extract("a.py", Language::Python, b"", &Budget::default()), Err(ExtractError::Unsupported(_))));
    // A BOM keeps offsets exact.
    let f = run("b.json", "\u{feff}{\"a\": 1}");
    assert_eq!(f.definitions[0].name_span.start_byte, 4);
    // Deterministic.
    assert_eq!(fixture("compose.yaml"), fixture("compose.yaml"));
}
