//! Fixture tests, one per row of the construct tables in the crate docs.

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Label, Language, RefKind};

use super::*;

fn facts_at(path: &str, src: &str) -> FileFacts {
    ShellPack.extract(path, Language::Shell, src.as_bytes(), &Budget::default()).unwrap()
}

fn facts(src: &str) -> FileFacts {
    facts_at("scripts/x.sh", src)
}

fn defs(f: &FileFacts, label: Label) -> Vec<&str> {
    f.definitions.iter().filter(|d| d.label == label).map(|d| d.qualified_name.as_str()).collect()
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f rueca_model::Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no definition {qn}"))
}

/// `(kind, receiver, name, enclosing qualified name)` of every reference.
fn refs(f: &FileFacts) -> Vec<(RefKind, Option<String>, String, Option<String>)> {
    f.references
        .iter()
        .map(|r| {
            let enc = r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone());
            (r.kind, r.receiver.clone(), r.name.clone(), enc)
        })
        .collect()
}

fn calls(f: &FileFacts, enclosing: Option<&str>) -> Vec<String> {
    refs(f)
        .into_iter()
        .filter(|(k, recv, _, e)| *k == RefKind::Call && recv.is_none() && e.as_deref() == enclosing)
        .map(|(_, _, n, _)| n)
        .collect()
}

fn scripts(f: &FileFacts) -> Vec<(String, Option<String>)> {
    refs(f)
        .into_iter()
        .filter(|(_, recv, _, _)| recv.as_deref() == Some(SCRIPT_RECEIVER))
        .map(|(_, _, n, e)| (n, e))
        .collect()
}

fn names_of(f: &FileFacts, kind: RefKind) -> Vec<(String, Option<String>)> {
    refs(f).into_iter().filter(|(k, ..)| *k == kind).map(|(_, _, n, e)| (n, e)).collect()
}

fn imports(f: &FileFacts) -> Vec<&str> {
    f.imports.iter().map(|i| i.module.as_str()).collect()
}

#[test]
fn function_forms() {
    let f = facts("f() { :; }\nfunction g { :; }\nfunction h() {\n  :\n}\nk() ( echo sub )\n");
    assert_eq!(defs(&f, Label::Function), ["f", "g", "h", "k"]);
    assert_eq!(def(&f, "f").signature.as_deref(), Some("f()"));
    assert_eq!(def(&f, "g").signature.as_deref(), Some("function g"));
    assert_eq!(def(&f, "h").signature.as_deref(), Some("function h()"));
    assert_eq!((def(&f, "h").span.start_line, def(&f, "h").span.end_line), (2, 4));
    assert!(f.definitions.iter().all(|d| d.exported && d.parent.is_none() && !d.is_test));
    assert_eq!(f.parse_errors, 0);
}

#[test]
fn nested_and_conditional_functions_are_file_level() {
    let f = facts(
        "outer() {\n  inner() { helper; }\n  inner\n}\nif [ -n \"$ZSH_VERSION\" ]; then\n  pick() { a; }\nelse\n  pick() { b; }\nfi\n",
    );
    assert_eq!(defs(&f, Label::Function), ["outer", "inner", "pick", "pick"]);
    assert!(f.definitions.iter().all(|d| d.parent.is_none()));
    assert_eq!(calls(&f, Some("inner")), ["helper"]);
    assert_eq!(calls(&f, Some("outer")), ["inner"]);
}

#[test]
fn doc_comments() {
    let f = facts("#!/bin/sh\n# Deploys the app.\n# Usage: deploy ENV\ndeploy() { :; }\n\n# far\n\nother() { :; }\n");
    assert_eq!(def(&f, "deploy").doc.as_deref(), Some("Deploys the app.\nUsage: deploy ENV"));
    assert_eq!(def(&f, "other").doc, None);
    let f = facts("#!/bin/sh\nfirst() { :; }\n");
    assert_eq!(def(&f, "first").doc, None);
}

#[test]
fn calls_in_every_position() {
    let f = facts(
        "main() {\n  a | b\n  c && d || e\n  ( f )\n  x=$(g arg)\n  y=`h`\n  diff <(i) <(j)\n  if k; then l; fi\n  while m; do n; done\n  ! o\n  time p\n  q &\n  echo \"$(r)\"\n  local z=$(s)\n}\n",
    );
    let got = calls(&f, Some("main"));
    for want in
        ["a", "b", "c", "d", "e", "f", "g", "h", "diff", "i", "j", "k", "l", "m", "n", "o", "p", "q", "echo", "r", "s"]
    {
        assert!(got.contains(&want.to_string()), "missing {want} in {got:?}");
    }
    assert!(!got.contains(&"time".to_string()) || got.contains(&"p".to_string()));
}

#[test]
fn runners_that_never_run_functions() {
    let f = facts("main() {\n  command f\n  exec g\n  xargs h < list\n  sudo i\n  env A=1 j\n  nohup k &\n}\n");
    let got = calls(&f, Some("main"));
    for name in ["f", "g", "h", "i", "j", "k"] {
        assert!(!got.contains(&name.to_string()), "{name} in {got:?}");
    }
    assert!(scripts(&f).is_empty());
}

#[test]
fn traps_and_eval() {
    let f = facts(
        "main() {\n  trap 'cleanup; rm -f \"$t\"' EXIT\n  trap on_int INT\n  trap - EXIT\n  eval \"setup arg\"\n  eval \"$dyn\"\n}\n",
    );
    let got = calls(&f, Some("main"));
    assert!(got.contains(&"setup".to_string()), "{got:?}");
    for name in ["cleanup", "on_int", "EXIT", "INT"] {
        assert!(!got.contains(&name.to_string()), "{name} in {got:?}");
    }
    // A trap handler is a reference (a callback), not a call.
    let reads: Vec<String> = names_of(&f, RefKind::Read).into_iter().map(|(n, _)| n).collect();
    assert_eq!(reads, ["cleanup", "rm", "on_int"]);
    let cleanup = f.references.iter().find(|r| r.name == "cleanup").unwrap();
    assert_eq!((cleanup.span.start_line, cleanup.span.start_col), (1, 8));
    assert_eq!(
        &"main() {\n  trap 'cleanup; rm"[cleanup.span.start_byte as usize..cleanup.span.end_byte as usize],
        "cleanup"
    );
}

#[test]
fn script_runs() {
    let f = facts(
        "DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\nmain() {\n  ./deploy.sh prod\n  bash scripts/build.sh -x\n  sh -e \"$DIR/lib/run.sh\"\n  \"$DIR/x.sh\"\n  exec ../other.bash\n  bash -c 'f'\n  \"$TOOL\" a\n  bats test/a.bats\n}\n",
    );
    let got = scripts(&f);
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["./deploy.sh", "scripts/build.sh", "$dir/lib/run.sh", "$dir/x.sh", "../other.bash", "test/a.bats"]
    );
    assert!(got.iter().all(|(_, e)| e.as_deref() == Some("main")));
    assert!(calls(&f, Some("main")).iter().all(|c| !c.contains('/')));
}

#[test]
fn sources() {
    let f = facts_at(
        "bin/tool",
        "#!/bin/bash\n. ./lib/a.sh\nsource \"$(dirname \"$0\")/../lib/b.sh\"\nsource ${BASH_SOURCE%/*}/c.sh\n\\. \"$NVM_DIR/nvm.sh\"\nHERE=$(dirname \"${BASH_SOURCE[0]}\")\nsource \"$HERE/d.sh\"\nsource \"$f\"\n. \"$dir/$name.sh\"\nsource ~/x.sh\n",
    );
    assert_eq!(imports(&f), ["./lib/a.sh", "$dir/../lib/b.sh", "$dir/c.sh", "*/nvm.sh", "$dir/d.sh"]);
    assert!(f.imports.iter().all(|i| i.names.len() == 1 && i.names[0].name == "*"));
    let top = calls(&f, None);
    assert!(!top.iter().any(|c| c == "source" || c == "." || c.ends_with(".sh")), "{top:?}");
}

#[test]
fn zsh_script_dir() {
    let f = facts_at("plugins/x/x.plugin.zsh", "source ${0:A:h}/lib.zsh\nsource \"${0:h}/other.zsh\"\n");
    assert_eq!(imports(&f), ["$dir/lib.zsh", "$dir/other.zsh"]);
}

#[test]
fn import_resolution() {
    let project = ProjectFiles {
        paths: ["bin/tool", "lib/a.sh", "lib/b.sh", "bin/c.sh", "nvm.sh"].into_iter().map(String::from).collect(),
        configs: Default::default(),
    };
    let imp = |m: &str| Import {
        module: m.to_string(),
        relative_level: 0,
        names: Vec::new(),
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Default::default(),
    };
    let r = |m: &str| ShellPack.resolve_import("bin/tool", &imp(m), &project);
    assert_eq!(r("./lib/a.sh").as_deref(), Some("lib/a.sh"));
    assert_eq!(r("$dir/../lib/b.sh").as_deref(), Some("lib/b.sh"));
    assert_eq!(r("$dir/c.sh").as_deref(), Some("bin/c.sh"));
    assert_eq!(r("*/nvm.sh").as_deref(), Some("nvm.sh"));
    assert_eq!(r("$dir/missing.sh"), None);
    assert_eq!(ShellPack.module_name("lib/a.sh", &project), "lib/a.sh");
}

#[test]
fn variables_and_constants() {
    let f = facts(
        "APP=demo\nreadonly VERSION=1.2\ndeclare -r MAX=3\ndeclare LIST\nAPI_TOKEN=abc\nAPP=other\nf() { local inner=1; GLOBAL_IN_FN=2; }\nfor i in 1; do :; done\n",
    );
    assert_eq!(defs(&f, Label::Variable), ["APP", "LIST", "API_TOKEN"]);
    assert_eq!(defs(&f, Label::Constant), ["VERSION", "MAX"]);
    assert_eq!(def(&f, "APP").signature.as_deref(), Some("APP=demo"));
    assert_eq!(def(&f, "API_TOKEN").signature.as_deref(), Some("API_TOKEN=•••"));
    assert_eq!(def(&f, "VERSION").signature.as_deref(), Some("VERSION=1.2"));
}

#[test]
fn reads_of_file_variables() {
    let f = facts(
        "CONF=/etc/x\nshow() { echo \"$CONF\" \"${CONF}\"; }\nshadow() { local CONF=1; echo $CONF; }\necho $CONF\n",
    );
    assert_eq!(names_of(&f, RefKind::Read), [("CONF".to_string(), Some("show".to_string()))]);
    assert!(names_of(&f, RefKind::EnvVar).is_empty());
}

#[test]
fn environment_reads() {
    let f = facts(
        "main() {\n  echo \"$HOME\" ${DATABASE_URL:-postgres://x} ${PORT:?need port} ${#PATH}\n  cat <<EOF\n$EDITOR\nEOF\n  local lower=$user_var\n  echo $BASH_SOURCE $LINENO $1 $@ $? $OPTARG\n  echo \"$SET_HERE\"\n}\nSET_HERE=1\necho $TOP_LEVEL\n",
    );
    let got = names_of(&f, RefKind::EnvVar);
    let m = |n: &str, e: Option<&str>| (n.to_string(), e.map(String::from));
    assert_eq!(
        got,
        [
            m("HOME", Some("main")),
            m("DATABASE_URL", Some("main")),
            m("PORT", Some("main")),
            m("PATH", Some("main")),
            m("EDITOR", Some("main")),
            m("TOP_LEVEL", None)
        ]
    );
}

#[test]
fn environment_writes() {
    let f = facts(
        "export DATABASE_URL=postgres://u:p@h/db\nexport PATH\ndeclare -x MODE=dev\nsetup() {\n  export API_KEY=\"$1\"\n  local -x TMPDIR=/tmp\n  LANG=C sort file\n}\nDEBUG=1 run_it\n",
    );
    assert_eq!(
        defs(&f, Label::EnvVar),
        ["DATABASE_URL", "PATH", "MODE", "setup.API_KEY", "setup.TMPDIR", "setup.LANG", "DEBUG"]
    );
    assert_eq!(def(&f, "DATABASE_URL").signature.as_deref(), Some("export DATABASE_URL=•••"));
    assert_eq!(def(&f, "PATH").signature.as_deref(), Some("export PATH"));
    assert_eq!(def(&f, "setup.API_KEY").parent, Some(3));
    assert!(defs(&f, Label::Variable).is_empty());
    // `export -f` exports a function, not a variable.
    let f = facts("helper() { :; }\nexport -f helper\ndeclare -F\n");
    assert!(defs(&f, Label::EnvVar).is_empty() && defs(&f, Label::Variable).is_empty());
    // The script sets these: no env reads.
    let f = facts("export X=1\nf() { echo $X; }\n");
    assert!(names_of(&f, RefKind::EnvVar).is_empty());
    assert_eq!(names_of(&f, RefKind::Read), [("X".to_string(), Some("f".to_string()))]);
}

#[test]
fn bats_tests() {
    let src = "load test_helper\nload 'lib/assert.bash'\nsetup() {\n  prepare\n}\n\n# Adds numbers.\n@test \"addition works\" {\n  run add 1 2\n  [ \"$status\" -eq 0 ]\n  helper\n}\n\n@test 'second' {\n  ./bin/tool --version\n}\n";
    let f = facts_at("test/math.bats", src);
    assert_eq!(defs(&f, Label::Test), ["addition works", "second"]);
    assert_eq!(defs(&f, Label::Function), ["setup"]);
    assert!(f.definitions.iter().all(|d| d.is_test));
    let t = def(&f, "addition works");
    assert_eq!((t.span.start_line, t.span.end_line), (7, 11));
    assert_eq!(t.doc.as_deref(), Some("Adds numbers."));
    assert_eq!(calls(&f, Some("addition works")), ["add", "helper"]);
    assert_eq!(scripts(&f), [("./bin/tool".to_string(), Some("second".to_string()))]);
    assert_eq!(imports(&f), ["$dir/test_helper.bash", "$dir/lib/assert.bash"]);
    assert_eq!(f.parse_errors, 0);
}

#[test]
fn shunit2_tests() {
    let src = "testAdds() { add 1 2; }\nsetUp() { :; }\nhelper() { :; }\n. shunit2\n";
    let f = facts_at("scripts/math_checks.sh", src);
    assert!(def(&f, "testAdds").is_test && def(&f, "setUp").is_test);
    assert!(!def(&f, "helper").is_test);
    let f = facts_at("scripts/math.sh", "testAdds() { :; }\n");
    assert!(!def(&f, "testAdds").is_test);
}

#[test]
fn test_files_by_path() {
    for p in [
        "test/x.sh",
        "tests/helpers.bash",
        "scripts/deploy_test.sh",
        "scripts/test_deploy.sh",
        "spec/a_spec.sh",
        "a.bats",
    ] {
        assert!(extract::is_test_file(p), "{p}");
        assert!(facts_at(p, "f() { :; }\n").definitions[0].is_test, "{p}");
    }
    for p in ["scripts/deploy.sh", "lib/testing_utils.sh", "bin/latest.sh"] {
        assert!(!extract::is_test_file(p), "{p}");
    }
}

#[test]
fn zsh_best_effort() {
    // The bash-compatible part of a zsh file is extracted; parameter flags
    // are repaired (`repair.rs`).
    let src = "autoload -Uz compinit\nzfn() {\n  print -P '%F{red}x%f'\n  [[ $a == b ]] && helper\n}\nhelper() { echo ok; }\nx=${(j:,:)parts}\nafter() { helper; }\n";
    let f = facts_at("lib/x.zsh", src);
    assert_eq!(defs(&f, Label::Function), ["zfn", "helper", "after"]);
    assert!(calls(&f, Some("zfn")).contains(&"helper".to_string()));
    assert_eq!(calls(&f, Some("after")), ["helper"]);
    assert_eq!(f.parse_errors, 0);
    // zsh-only syntax the grammar cannot read is an ERROR node, counted,
    // and the walk goes on (glob qualifiers, `for x (…)` loops).
    let f = facts_at("lib/y.zsh", "for f in *(.); do :; done\nfor x (a b c) { print $x }\nlater() { helper; }\n");
    assert!(f.parse_errors > 0);
}

#[test]
fn parse_errors_are_counted() {
    let f = facts("f() {\n  if then fi\n");
    assert!(f.parse_errors > 0);
}

#[test]
fn determinism_and_budget() {
    let src = "a() { b; }\nb() { export X=1; echo $HOME; }\n";
    assert_eq!(facts(src), facts(src));
    let tiny = Budget { max_bytes: 4, ..Budget::default() };
    assert!(ShellPack.extract("x.sh", Language::Shell, src.as_bytes(), &tiny).is_err());
    assert!(ShellPack.extract("x.py", Language::Python, src.as_bytes(), &Budget::default()).is_err());
    // Invalid UTF-8 never panics.
    let f = ShellPack.extract("x.sh", Language::Shell, b"f() { echo \xff; }\n", &Budget::default()).unwrap();
    assert_eq!(f.definitions.len(), 1);
}

#[test]
fn pack_contract() {
    assert_eq!(ShellPack.languages(), &[Language::Shell]);
    assert!(ShellPack.facts_depend_on_path());
    assert!(ShellPack.is_builtin("echo") && !ShellPack.is_builtin("deploy"));
    assert!(ShellPack.self_receivers().is_empty());
    assert_eq!(Language::from_path("scripts/deploy.sh"), Some(Language::Shell));
}
