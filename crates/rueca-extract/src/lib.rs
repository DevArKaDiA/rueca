//! Extraction contract for Rueca language packs.
//!
//! A [`LanguagePack`] turns one file into [`FileFacts`] (pure, deterministic,
//! no I/O) and knows its language's module semantics: how a path maps to a
//! module name and how an import specifier maps to a file in the project.
//! The generic resolver (`rueca-resolve`) combines facts from every file of a
//! snapshot using these two hooks.
//!
//! This crate also provides tree-sitter helpers shared by every pack:
//! budget-limited parsing, span conversion, and node text access.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use rueca_model::{FileFacts, Import, Language, Span};
use tree_sitter::{Node, Parser, Tree};

pub use rueca_model;
pub use tree_sitter;

// ---------------------------------------------------------------------------
// Budget and errors
// ---------------------------------------------------------------------------

/// Resource limits for extracting one file. Files over the limits are recorded
/// as coverage gaps by the indexer, never crash it.
#[derive(Debug, Clone)]
pub struct Budget {
    pub max_bytes: usize,
    pub max_parse_time: Duration,
    /// Maximum syntax-tree nesting depth walked by extractors.
    pub max_depth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { max_bytes: 2 * 1024 * 1024, max_parse_time: Duration::from_secs(5), max_depth: 512 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("file too large ({size} bytes > {limit})")]
    TooLarge { size: usize, limit: usize },
    #[error("parse timed out")]
    Timeout,
    #[error("parser failed: {0}")]
    Parser(String),
    #[error("unsupported language: {0}")]
    Unsupported(Language),
    #[error("invalid query: {0}")]
    Query(String),
}

// ---------------------------------------------------------------------------
// Project context
// ---------------------------------------------------------------------------

/// What a pack may know about the whole snapshot when computing module names
/// and resolving imports. Built once per snapshot by the indexer.
#[derive(Debug, Clone, Default)]
pub struct ProjectFiles {
    /// Every repository-relative path in the snapshot (all languages, `/` separators).
    pub paths: BTreeSet<String>,
    /// Contents of small configuration files that affect module resolution,
    /// keyed by repository-relative path: `tsconfig*.json`, `jsconfig.json`,
    /// `package.json`, `pyproject.toml`, `setup.cfg`, `setup.py`.
    pub configs: BTreeMap<String, String>,
}

impl ProjectFiles {
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// Config file names the indexer must load into [`ProjectFiles::configs`].
    pub fn is_config_file(path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        name == "package.json"
            || name == "jsconfig.json"
            || name == "pyproject.toml"
            || name == "setup.cfg"
            || name == "setup.py"
            || (name.starts_with("tsconfig") && name.ends_with(".json"))
    }
}

// ---------------------------------------------------------------------------
// Language pack trait
// ---------------------------------------------------------------------------

pub trait LanguagePack: Send + Sync {
    /// Languages this pack extracts.
    fn languages(&self) -> &'static [Language];

    /// Bumped whenever extraction output changes for the same input bytes.
    /// Part of the facts cache key.
    fn extractor_version(&self) -> u32;

    /// Extracts facts from one file. Must be deterministic: same `path`,
    /// `language`, `source` and version → identical output.
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError>;

    /// The module name other files use to refer to `path`.
    /// Python: dotted path relative to its source root (`pkg.sub.mod`,
    /// `pkg` for `pkg/__init__.py`). ECMAScript: the path without extension
    /// (`src/util/index`).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String;

    /// Resolves an import written in `from_path` to the repository-relative
    /// path of the imported file, if it is part of the project. Returns `None`
    /// for third-party / builtin modules.
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String>;

    /// Names that are always in scope (builtins, globals). References to
    /// them are never reported as unresolved and never fuzzy-matched.
    fn is_builtin(&self, name: &str) -> bool;

    /// Receiver texts that denote "the current instance" (`self`, `cls`, `this`).
    fn self_receivers(&self) -> &'static [&'static str];
}

/// Registry of packs, keyed by language.
#[derive(Default)]
pub struct Registry {
    packs: Vec<Box<dyn LanguagePack>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pack: Box<dyn LanguagePack>) {
        self.packs.push(pack);
    }

    pub fn for_language(&self, language: Language) -> Option<&dyn LanguagePack> {
        self.packs.iter().find(|p| p.languages().contains(&language)).map(|b| b.as_ref())
    }

    pub fn for_path(&self, path: &str) -> Option<(Language, &dyn LanguagePack)> {
        let lang = Language::from_path(path)?;
        self.for_language(lang).map(|p| (lang, p))
    }

    /// Cache key component combining every pack's version.
    pub fn versions(&self) -> Vec<(Language, u32)> {
        let mut v: Vec<_> =
            self.packs.iter().flat_map(|p| p.languages().iter().map(move |l| (*l, p.extractor_version()))).collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------------------
// tree-sitter helpers
// ---------------------------------------------------------------------------

/// Parses `source` with `language`, enforcing size and time budgets.
pub fn parse(language: &tree_sitter::Language, source: &[u8], budget: &Budget) -> Result<Tree, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let mut parser = Parser::new();
    parser.set_language(language).map_err(|e| ExtractError::Parser(e.to_string()))?;
    let deadline = Instant::now() + budget.max_parse_time;
    let mut timed_out = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let len = source.len();
    let mut read =
        |offset: usize, _: tree_sitter::Point| -> &[u8] { if offset < len { &source[offset..] } else { &[] } };
    let tree = parser.parse_with_options(
        &mut read,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    match tree {
        Some(t) => Ok(t),
        None if timed_out => Err(ExtractError::Timeout),
        None => Err(ExtractError::Parser("parser returned no tree".into())),
    }
}

/// Converts a tree-sitter node range to a [`Span`].
pub fn span_of(node: Node<'_>) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: s.row as u32,
        start_col: s.column as u32,
        end_line: e.row as u32,
        end_col: e.column as u32,
    }
}

/// UTF-8 text of a node (lossy for invalid bytes).
pub fn text_of<'s>(node: Node<'_>, source: &'s [u8]) -> std::borrow::Cow<'s, str> {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()])
}

/// Counts ERROR and MISSING nodes in a tree.
pub fn count_errors(tree: &Tree) -> u32 {
    if !tree.root_node().has_error() {
        return 0;
    }
    let mut count = 0;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            count += 1;
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}

/// Number of lines in `source` (a trailing newline does not start a new line).
pub fn line_count(source: &[u8]) -> u32 {
    if source.is_empty() {
        return 0;
    }
    let n = source.iter().filter(|b| **b == b'\n').count() as u32;
    if source.ends_with(b"\n") { n } else { n + 1 }
}

/// First line of `text`, trimmed and truncated to `max` chars — for signatures.
pub fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        let mut s: String = line.chars().take(max).collect();
        s.push('…');
        s
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counting() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a"), 1);
        assert_eq!(line_count(b"a\n"), 1);
        assert_eq!(line_count(b"a\nb"), 2);
    }

    #[test]
    fn config_detection() {
        assert!(ProjectFiles::is_config_file("web/tsconfig.base.json"));
        assert!(ProjectFiles::is_config_file("pyproject.toml"));
        assert!(!ProjectFiles::is_config_file("src/config.json"));
    }
}
