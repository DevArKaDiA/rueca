//! Facts of single Ruby files: definitions, references, receivers,
//! imports, tests and env reads (the encoding the resolver relies on).

use rueca_extract::{Budget, LanguagePack};
use rueca_lang_ruby::RubyPack;
use rueca_model::{FileFacts, Label, Language, RefKind};

fn facts(path: &str, src: &str) -> FileFacts {
    RubyPack.extract(path, Language::Ruby, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f rueca_model::Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no {qn}: {:#?}", f.definitions))
}

fn refs(f: &FileFacts, kind: RefKind, name: &str) -> Vec<Option<String>> {
    f.references.iter().filter(|r| r.kind == kind && r.name == name).map(|r| r.receiver.clone()).collect()
}

const SRC: &str = r#"
module A
  class B::C < Base
    include M
    extend N
    attr_accessor :name
    LIMIT = 3

    class << self
      def build; end
    end

    def self.make(x)
      new(x)
    end

    # Runs it.
    def run(arg)
      helper
      count = 1
      count
      x = Foo::Bar.new
      x.go
      @repo.save
      self.class.make(1)
      ENV["HOME"]
      ENV.fetch("PORT", 80)
      super
      self.name = arg
    end

    private

    def helper; end
    alias_method :assist, :helper
    define_method(:dyn) { helper }
  end
end
"#;

#[test]
fn ruby_definitions() {
    let f = facts("lib/a.rb", SRC);
    assert_eq!(def(&f, "A").label, Label::Module);
    let c = def(&f, "A::B::C");
    assert_eq!(c.label, Label::Class);
    assert_eq!(c.bases, ["Base", "include:M", "extend:N"]);
    assert_eq!(c.signature.as_deref(), Some("class B::C < Base"));
    assert_eq!(def(&f, "A::B::C#name").label, Label::Field);
    assert_eq!(def(&f, "A::B::C::LIMIT").label, Label::Constant);
    assert_eq!(def(&f, "A::B::C.build").label, Label::Method);
    assert_eq!(def(&f, "A::B::C.make").signature.as_deref(), Some("def self.make(x)"));
    let run = def(&f, "A::B::C#run");
    assert_eq!(run.doc.as_deref(), Some("Runs it."));
    assert!(run.exported);
    let helper = def(&f, "A::B::C#helper");
    assert!(!helper.exported);
    assert_eq!(helper.signature.as_deref(), Some("private def helper"));
    assert_eq!(def(&f, "A::B::C#assist").signature.as_deref(), Some("alias_method :assist, :helper"));
    assert_eq!(def(&f, "A::B::C#dyn").label, Label::Method);
}

#[test]
fn ruby_references_and_receivers() {
    let f = facts("lib/a.rb", SRC);
    assert_eq!(refs(&f, RefKind::Call, "helper").len(), 3, "bare call, alias target, define_method body");
    assert!(refs(&f, RefKind::Call, "count").is_empty(), "a local after its assignment");
    assert_eq!(refs(&f, RefKind::Call, "new"), [None, Some("Foo::Bar".to_string())]);
    assert_eq!(refs(&f, RefKind::Call, "go"), [Some("$type:Foo::Bar".to_string())]);
    assert_eq!(refs(&f, RefKind::Call, "save"), [Some("?".to_string())]);
    assert_eq!(refs(&f, RefKind::Call, "make"), [Some("self.class".to_string())]);
    assert_eq!(refs(&f, RefKind::Call, "run"), [Some("$super".to_string())]);
    assert_eq!(refs(&f, RefKind::Write, "name"), [Some("self".to_string())]);
    assert_eq!(refs(&f, RefKind::Read, "Bar"), [Some("Foo".to_string())]);
    let mut env: Vec<&str> =
        f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| r.name.as_str()).collect();
    env.sort();
    assert_eq!(env, ["HOME", "PORT"]);
}

#[test]
fn ruby_declared_types() {
    let src = r#"
class Pool
  extend T::Sig

  sig { params(conn: Conn).returns(Pool) }
  def use(conn)
    conn.query
    @cache = T.let(nil, T.nilable(Cache))
    @cache.get
  end

  #: (Conn conn) -> self
  def again(conn)
    conn.query
    checkout.query
  end
end
"#;
    let f = facts("lib/pool.rb", src);
    assert_eq!(def(&f, "Pool#use").signature.as_deref(), Some("def use(conn) #: -> Pool"));
    assert_eq!(def(&f, "Pool#again").signature.as_deref(), Some("def again(conn) #: -> ::Pool"));
    assert_eq!(
        refs(&f, RefKind::Call, "query"),
        [Some("$type:Conn".to_string()), Some("$type:Conn".to_string()), Some("$ret:#checkout".to_string())]
    );
    assert_eq!(refs(&f, RefKind::Call, "get"), [Some("$type:Cache".to_string())]);
}

#[test]
fn ruby_imports_and_tests() {
    let src = "require \"json\"\nrequire_relative \"../lib/x\"\nload File.expand_path(\"y\", __dir__)\nautoload :Z, \"z/z\"\n\
               RSpec.describe Foo do\n  context \"when\" do\n    it \"works\" do\n      Foo.run\n    end\n  end\nend\n";
    let f = facts("spec/foo_spec.rb", src);
    let imports: Vec<(&str, u32)> = f.imports.iter().map(|i| (i.module.as_str(), i.relative_level)).collect();
    assert_eq!(imports, [("json", 0), ("../lib/x", 1), ("y", 1), ("z/z", 0)]);
    let it = f.definitions.iter().find(|d| d.label == Label::Test && d.name == "works").unwrap();
    assert_eq!(it.qualified_name, "Foo > when > works");
    assert!(it.is_test);
    let run = f.references.iter().find(|r| r.name == "run").unwrap();
    assert_eq!(f.definitions[run.enclosing.unwrap() as usize].name, "works");
}

#[test]
fn ruby_module_function_and_struct() {
    let src = "module Util\n  module_function\n  def slug(s); s; end\nend\nPair = Struct.new(:a, :b) do\n  def sum; a + b; end\nend\n";
    let f = facts("lib/u.rb", src);
    assert_eq!(def(&f, "Util.slug").signature.as_deref(), Some("module_function def slug(s)"));
    assert_eq!(def(&f, "Pair").bases, ["Struct"]);
    assert_eq!(def(&f, "Pair#a").label, Label::Field);
    assert_eq!(def(&f, "Pair#sum").label, Label::Method);
}
