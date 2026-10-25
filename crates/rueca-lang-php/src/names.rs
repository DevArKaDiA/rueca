//! PHP name resolution (compile-time, per file) and the receiver encoding
//! the resolver understands.
//!
//! PHP resolves every class, function and constant name to a fully
//! qualified name at compile time from two pieces of the file itself: the
//! current `namespace` and the `use` imports of that namespace
//! (<https://www.php.net/manual/en/language.namespaces.rules.php>). Nothing
//! outside the file is involved, so the pack applies these rules during
//! extraction and reports names already qualified; the resolver only has to
//! find the fully qualified name in the project.
//!
//! # Encoding in [`Reference`](rueca_model::Reference)
//!
//! | Written | `name` | `receiver` | Meaning |
//! |---|---|---|---|
//! | `new User` with `use App\Models\User` | `User` | `\App\Models\` | exact name `App\Models\User` (class, function or constant) |
//! | `new \Exception` / `Foo` in the global namespace | `Exception` | `\` | exact global name |
//! | `strlen()` / `PHP_EOL` inside `namespace App;` | `strlen` | `App\` | `App\strlen`, falling back to global `strlen` (functions and constants only) |
//! | `User::find()` / `User::TABLE` / `User::$count` | `find` / `TABLE` / `$count` | `\App\Models\User` | static member of a class |
//! | `$user->save()` where `$user` is declared `User` | `save` | `\App\Models\User->` | instance member of a typed receiver |
//! | `$this->save()` / `self::X` / `static::m()` / `parent::m()` | `save` / `X` / `m` | `$this` / `self` / `static` / `parent` | relative to the enclosing class |
//! | `$repo->save()`, `foo()->bar()` | `save` / `bar` | source text | unknown receiver |
//!
//! A namespace qualifier always ends with `\`; a class receiver never does.

use std::collections::HashMap;

/// What a name denotes; selects the `use` table and the fallback rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameKind {
    Class,
    Function,
    Constant,
}

/// Class names that are keywords or builtin types, never class references.
const RESERVED: &[&str] = &[
    "array", "bool", "callable", "false", "float", "int", "iterable", "mixed", "never", "null", "object", "parent",
    "self", "static", "string", "true", "void",
];

/// `self` / `static` / `parent` (case-insensitive), as written.
pub(crate) fn relative_class(written: &str) -> Option<&'static str> {
    match written.to_ascii_lowercase().as_str() {
        "self" => Some("self"),
        "static" => Some("static"),
        "parent" => Some("parent"),
        _ => None,
    }
}

pub(crate) fn is_reserved_class_name(written: &str) -> bool {
    let lower = written.trim_start_matches('\\').to_ascii_lowercase();
    RESERVED.binary_search(&lower.as_str()).is_ok()
}

/// A resolved name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// Fully qualified name, without the leading `\`.
    Exact(String),
    /// Unqualified function or constant inside a namespace: `namespace\name`
    /// at run time, falling back to the global `name`.
    Fallback { namespace: String, name: String },
}

impl Resolved {
    /// `(name, receiver)` in the reference encoding (module docs).
    pub(crate) fn encode(&self) -> (String, String) {
        match self {
            Resolved::Exact(fqn) => match fqn.rsplit_once('\\') {
                Some((ns, short)) => (short.to_string(), format!("\\{ns}\\")),
                None => (fqn.clone(), "\\".to_string()),
            },
            Resolved::Fallback { namespace, name } => (name.clone(), format!("{namespace}\\")),
        }
    }
}

/// Splits a written name into segments, dropping whitespace and comments the
/// grammar may leave inside a `qualified_name` (`App \ Models`).
fn clean(written: &str) -> String {
    written.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Namespace and imports in effect at a point of a file.
#[derive(Debug, Clone, Default)]
pub(crate) struct NameCtx {
    /// Current namespace without leading or trailing `\`; empty = global.
    pub(crate) namespace: String,
    /// Class / namespace aliases, keyed lowercase (case-insensitive).
    classes: HashMap<String, String>,
    /// `use function` aliases, keyed lowercase.
    functions: HashMap<String, String>,
    /// `use const` aliases (case-sensitive).
    constants: HashMap<String, String>,
}

impl NameCtx {
    pub(crate) fn new(namespace: &str) -> Self {
        NameCtx { namespace: clean(namespace).trim_matches('\\').to_string(), ..Default::default() }
    }

    /// Records `use <kind> <fqn> [as <alias>]`.
    pub(crate) fn add_use(&mut self, kind: NameKind, fqn: &str, alias: &str) {
        let fqn = clean(fqn).trim_start_matches('\\').to_string();
        match kind {
            NameKind::Class => self.classes.insert(alias.to_ascii_lowercase(), fqn),
            NameKind::Function => self.functions.insert(alias.to_ascii_lowercase(), fqn),
            NameKind::Constant => self.constants.insert(alias.to_string(), fqn),
        };
    }

    fn prefixed(&self, rest: &str) -> String {
        if self.namespace.is_empty() { rest.to_string() } else { format!("{}\\{rest}", self.namespace) }
    }

    /// Resolves a name as written (`\A\B`, `namespace\A`, `A\B`, `A`).
    /// `None` for empty names and, for classes, `self` / `static` /
    /// `parent` and builtin type keywords.
    pub(crate) fn resolve(&self, written: &str, kind: NameKind) -> Option<Resolved> {
        let w = clean(written);
        if w.is_empty() || w == "\\" {
            return None;
        }
        if kind == NameKind::Class && is_reserved_class_name(&w) {
            return None;
        }
        // Fully qualified.
        if let Some(rest) = w.strip_prefix('\\') {
            return Some(Resolved::Exact(rest.to_string()));
        }
        // Relative to the current namespace: `namespace\Foo`.
        if let Some(rest) = w.get(..10).filter(|p| p.eq_ignore_ascii_case("namespace\\")).map(|_| &w[10..])
            && !rest.is_empty()
        {
            return Some(Resolved::Exact(self.prefixed(rest)));
        }
        match w.split_once('\\') {
            // Qualified: the first segment may be an imported alias.
            Some((first, rest)) => match self.classes.get(&first.to_ascii_lowercase()) {
                Some(target) => Some(Resolved::Exact(format!("{target}\\{rest}"))),
                None => Some(Resolved::Exact(self.prefixed(&w))),
            },
            None => {
                let imported = match kind {
                    NameKind::Class => self.classes.get(&w.to_ascii_lowercase()),
                    NameKind::Function => self.functions.get(&w.to_ascii_lowercase()),
                    NameKind::Constant => self.constants.get(&w),
                };
                if let Some(target) = imported {
                    return Some(Resolved::Exact(target.clone()));
                }
                if kind == NameKind::Class || self.namespace.is_empty() {
                    Some(Resolved::Exact(self.prefixed(&w)))
                } else {
                    Some(Resolved::Fallback { namespace: self.namespace.clone(), name: w })
                }
            }
        }
    }

    /// Fully qualified class name for a class reference, if it is one.
    pub(crate) fn class_fqn(&self, written: &str) -> Option<String> {
        match self.resolve(written, NameKind::Class)? {
            Resolved::Exact(f) => Some(f),
            Resolved::Fallback { .. } => None,
        }
    }

    /// Qualified name of a declaration named `name` in the current namespace.
    pub(crate) fn declare(&self, name: &str) -> String {
        self.prefixed(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> NameCtx {
        let mut c = NameCtx::new("App\\Http");
        c.add_use(NameKind::Class, "App\\Models\\User", "User");
        c.add_use(NameKind::Class, "\\Foo\\Bar", "Baz");
        c.add_use(NameKind::Class, "App\\Models", "Models");
        c.add_use(NameKind::Function, "App\\Support\\format_money", "format_money");
        c.add_use(NameKind::Constant, "App\\Support\\MAX", "MAX");
        c
    }

    fn exact(s: &str) -> Option<Resolved> {
        Some(Resolved::Exact(s.into()))
    }

    #[test]
    fn reserved_list_is_sorted() {
        assert!(RESERVED.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn class_rules() {
        let c = ctx();
        assert_eq!(c.resolve("User", NameKind::Class), exact("App\\Models\\User"));
        assert_eq!(c.resolve("user", NameKind::Class), exact("App\\Models\\User"));
        assert_eq!(c.resolve("Baz", NameKind::Class), exact("Foo\\Bar"));
        assert_eq!(c.resolve("\\Exception", NameKind::Class), exact("Exception"));
        assert_eq!(c.resolve("Request", NameKind::Class), exact("App\\Http\\Request"));
        assert_eq!(c.resolve("Models\\Post", NameKind::Class), exact("App\\Models\\Post"));
        assert_eq!(c.resolve("Sub\\Thing", NameKind::Class), exact("App\\Http\\Sub\\Thing"));
        assert_eq!(c.resolve("namespace\\Thing", NameKind::Class), exact("App\\Http\\Thing"));
        assert_eq!(c.resolve("self", NameKind::Class), None);
        assert_eq!(c.resolve("int", NameKind::Class), None);
        assert_eq!(c.resolve("App \\ Models", NameKind::Class), exact("App\\Http\\App\\Models"));
    }

    #[test]
    fn function_and_constant_rules() {
        let c = ctx();
        assert_eq!(c.resolve("format_money", NameKind::Function), exact("App\\Support\\format_money"));
        assert_eq!(
            c.resolve("strlen", NameKind::Function),
            Some(Resolved::Fallback { namespace: "App\\Http".into(), name: "strlen".into() })
        );
        assert_eq!(c.resolve("\\strlen", NameKind::Function), exact("strlen"));
        assert_eq!(c.resolve("Models\\helper", NameKind::Function), exact("App\\Models\\helper"));
        assert_eq!(c.resolve("MAX", NameKind::Constant), exact("App\\Support\\MAX"));
        assert_eq!(
            c.resolve("max", NameKind::Constant),
            Some(Resolved::Fallback { namespace: "App\\Http".into(), name: "max".into() })
        );
        let global = NameCtx::new("");
        assert_eq!(global.resolve("strlen", NameKind::Function), exact("strlen"));
        assert_eq!(global.resolve("Foo", NameKind::Class), exact("Foo"));
    }

    #[test]
    fn encoding() {
        assert_eq!(Resolved::Exact("App\\Models\\User".into()).encode(), ("User".into(), "\\App\\Models\\".into()));
        assert_eq!(Resolved::Exact("Exception".into()).encode(), ("Exception".into(), "\\".into()));
        assert_eq!(
            Resolved::Fallback { namespace: "App".into(), name: "strlen".into() }.encode(),
            ("strlen".into(), "App\\".into())
        );
    }
}
