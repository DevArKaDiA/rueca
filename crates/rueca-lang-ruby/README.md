# rueca-lang-ruby

Ruby language pack for [Rueca](../../README.md): `tree-sitter-ruby` 0.23
(MIT) with a vendored, adapted nvim-treesitter `locals.scm`
([`queries/README.md`](queries/README.md)).

- **Files**: `.rb`, `.rake`, `.gemspec`, `.ru`, `Rakefile`, `Gemfile`.
- **Definitions**: modules and classes by constant path (`A::B`, compact and
  nested forms, reopened classes are one constant), instance methods
  `A::B#m`, singleton methods `A::B.m` (`def self.m`, `class << self`,
  `module_function`), accessors (`attr_*` → `Field`), aliases,
  `define_method(:m)`, constants, `Struct.new` / `Data.define` /
  `Class.new` classes, RSpec / ActiveSupport test blocks.
- **References**: calls with and without parentheses (a bare identifier
  that is not a local variable is a call), `super`, `send(:m)`, constants,
  `rescue` / `raise` classes, attribute writes, `ENV["X"]` /
  `ENV.fetch("X")` reads for the code ↔ config bridge.
- **Receivers**: `self`, `self.class`, constant paths, variables typed by
  `Foo.new`, Sorbet `sig`s, RBS comments and `T.let`, call results typed by
  declared return types (`$ret:`); constants and methods are resolved by
  the Ruby part of `rueca-resolve` (lexical nesting, ancestors and mixins).
- **Imports**: `require`, `require_relative`, `load`, `autoload` → files
  (`lib/` load paths of the repository).
- **Packages** (RFC 0006): `*.gemspec` names and dependencies, `Gemfile`
  gems ([`parse_gem_manifest`](src/gems.rs)).

Crate docs (`src/lib.rs`) have the facts encoding and the limitations.
