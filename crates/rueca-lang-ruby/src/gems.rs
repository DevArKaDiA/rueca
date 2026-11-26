//! RubyGems manifests (RFC 0006 package identity): a `*.gemspec`'s name
//! and dependencies, a `Gemfile`'s gems. Read with the Ruby grammar, best
//! effort: literal strings only, nothing is evaluated.

use rueca_extract::tree_sitter::{Node, Parser};

use crate::extract::{literal_string, text};

/// What a gemspec or a Gemfile declares.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GemManifest {
    /// `spec.name = "x"` (gemspecs).
    pub name: Option<String>,
    /// Runtime and development dependencies (`add_dependency`,
    /// `add_runtime_dependency`, `add_development_dependency`) and `gem
    /// "x"` lines (Gemfiles), in file order, deduplicated.
    pub dependencies: Vec<String>,
}

const DEPENDENCY_CALLS: &[&str] = &["add_dependency", "add_runtime_dependency", "add_development_dependency", "gem"];

/// Reads a gemspec or a Gemfile. `None` when it does not parse at all.
pub fn parse_gem_manifest(source: &str) -> Option<GemManifest> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_ruby::LANGUAGE.into()).ok()?;
    let tree = parser.parse(source, None)?;
    let src = source.as_bytes();
    let mut out = GemManifest::default();
    let mut stack = vec![tree.root_node()];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > 200_000 {
            break;
        }
        match n.kind() {
            "assignment" => name_assignment(n, src, &mut out),
            "call" => dependency_call(n, src, &mut out),
            _ => {}
        }
        let mut c = n.walk();
        let children: Vec<Node<'_>> = n.named_children(&mut c).collect();
        stack.extend(children.into_iter().rev());
    }
    Some(out)
}

/// `spec.name = "x"`.
fn name_assignment(n: Node<'_>, src: &[u8], out: &mut GemManifest) {
    let (Some(left), Some(right)) = (n.child_by_field_name("left"), n.child_by_field_name("right")) else { return };
    let is_name = left.kind() == "call" && left.child_by_field_name("method").is_some_and(|m| text(m, src) == "name");
    if is_name && out.name.is_none() {
        out.name = literal_string(right, src).filter(|s| !s.is_empty());
    }
}

/// `spec.add_dependency "x", "~> 1"` / `gem "x"`.
fn dependency_call(n: Node<'_>, src: &[u8], out: &mut GemManifest) {
    let Some(m) = n.child_by_field_name("method") else { return };
    let name = text(m, src);
    if !DEPENDENCY_CALLS.contains(&name) || (name == "gem" && n.child_by_field_name("receiver").is_some()) {
        return;
    }
    let Some(dep) =
        n.child_by_field_name("arguments").and_then(|a| a.named_child(0)).and_then(|a| literal_string(a, src))
    else {
        return;
    };
    if !dep.is_empty() && !out.dependencies.contains(&dep) {
        out.dependencies.push(dep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gemspecs_and_gemfiles() {
        let spec = "Gem::Specification.new do |s|\n  s.name    = 'rack'\n  s.add_dependency 'rack-session', '>= 2'\n  s.add_development_dependency(\"minitest\")\nend\n";
        let m = parse_gem_manifest(spec).unwrap();
        assert_eq!(m.name.as_deref(), Some("rack"));
        assert_eq!(m.dependencies, ["rack-session", "minitest"]);
        let gemfile =
            "source 'https://rubygems.org'\ngemspec\ngem \"rails\", \"~> 7.1\"\ngroup :test do\n  gem 'rspec'\nend\n";
        let m = parse_gem_manifest(gemfile).unwrap();
        assert_eq!(m.name, None);
        assert_eq!(m.dependencies, ["rails", "rspec"]);
    }
}
