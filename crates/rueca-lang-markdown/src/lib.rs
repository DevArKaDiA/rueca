//! Markdown language pack for Rueca: documents become navigable structure in
//! the same graph as code.
//!
//! Parser: [`pulldown-cmark`](https://github.com/pulldown-cmark/pulldown-cmark)
//! 0.13 (MIT, pure Rust, CommonMark + GFM tables, footnotes, strikethrough,
//! task lists and alert block quotes), used through its offset iterator so
//! every event carries a byte range (RFC 0002 §3.2–3.3: data file kinds use
//! pure-Rust parsers on untrusted input). See `rueca_extract::LanguagePack`
//! for the contract; the pack does not use tree-sitter.
//!
//! Files: `.md` and `.markdown` ([`Language::Markdown`]). MDX (`.mdx`) is not
//! handled: its JSX and ESM blocks are not CommonMark.
//!
//! # Sections (definitions)
//!
//! Every ATX (`## Linux`) and setext (`Linux` / `-----`) heading at the top
//! level of the document becomes a [`Label::Section`](rueca_model::Label::Section) definition. Headings
//! inside block quotes, list items and footnote definitions, and raw HTML
//! headings (`<h1>…</h1>`), are not sections.
//!
//! - `name`: the heading's plain text — inline code and link texts included,
//!   markup removed, whitespace collapsed, ≤ 200 chars. Image alt text is
//!   dropped unless the heading has no other text (`# ![Logo](logo.svg)` →
//!   `Logo`). Headings with no text at all are skipped (their content belongs
//!   to the enclosing section).
//! - `parent`: the nearest preceding heading with a lower level, so `DEFINES`
//!   forms the document tree (`# A` contains `## B`; a skipped level such as
//!   `#` → `###` nests directly).
//! - `qualified_name`: the heading chain joined by ` > ` (`Install > Linux`).
//!   The module name is the repository path **with** its extension
//!   ([`LanguagePack::module_name`]) and the Markdown module separator is `:`,
//!   so the global name is `docs/guide.md:Install > Linux` (and `README.md`
//!   never collides with `README.py`).
//! - **Duplicates**: when a qualified name is already taken in the file, the
//!   later section gets the first free ` (n)` suffix, `n` = 2, 3, …
//!   (`Changelog > Fixed`, `Changelog > Fixed (2)`); its children are
//!   qualified by the suffixed name. `name` keeps the plain heading text.
//! - `span`: from the heading to the end of the last non-blank line before
//!   the next heading of the same or a higher level (or the end of the file),
//!   so `get_snippet` returns the whole section including its subsections.
//! - `name_span`: the heading's inline content (first line for setext).
//! - `signature`: the heading as an ATX line, `#` × level + the heading
//!   source (`## Linux`, ``## The `run` command``), ≤ 200 chars.
//! - `doc`: the first top-level paragraph after the heading and before the
//!   next heading of any level, as plain text (≤ 500 chars). Paragraphs that
//!   are only images (badges) are skipped. Code blocks never contribute.
//! - `exported` is always true, `is_test` always false.
//! - A YAML (`---`) or TOML (`+++`) front matter block at the start of the
//!   file is skipped (it produces nothing; a front-matter `title` is not
//!   used). A leading BOM is skipped too.
//!
//! # Links (imports)
//!
//! Inline, reference-style (`[x][ref]`, `[x][]`, `[x]`) and footnote links,
//! images, and the `href` / `src` attributes of raw HTML (`<img src=…>`,
//! `<a href=…>`) become one [`Import`] each: `module` = the destination as
//! written (anchor and query kept), no names, deduplicated per file by
//! destination (first occurrence wins, at most 20,000). Destinations with a
//! URL scheme (`https:`, `mailto:`, `data:`, …), protocol-relative URLs
//! (`//host/x`), autolinks and same-document anchors (`#usage`) are not
//! recorded. Reference definitions that no link uses are not recorded.
//!
//! [`LanguagePack::resolve_import`] maps a destination to a repository file
//! (rules in [`links::resolve`]): strip `#anchor` / `?query`, percent-decode,
//! join to the linking file's directory (`/x` = repository root), fold `.` /
//! `..`; try the path, `path.md`, `x.md` for `x.html`, then directory indexes
//! (`README.md`, `readme.md`, `Readme.md`, `index.md`, `_index.md`); a bare
//! path that misses (`src/main.py` from `docs/guide.md`) is retried from the
//! repository root. Any file of the snapshot can be a target (code, images,
//! other documents), so the resolver emits `IMPORTS` File→File edges with
//! the link as evidence. Links to directories without an index file, and
//! anchors to sections, produce no edge (anchor → `Section` references are
//! future work).
//!
//! # What is not extracted
//!
//! - **No references.** Inline code spans that look like identifiers
//!   (`` `Session.request` ``) are not emitted as `Read` references: the
//!   resolver would match unique names at 0.6 confidence, which turns prose
//!   into noise edges. Future work, behind an opt-in.
//! - **Code blocks** (fenced and indented) record nothing in the graph; they
//!   stay content-searchable through `search_code`, and matches inside them
//!   are placed in their section.
//! - Raw HTML outside `href` / `src`, math, and front-matter fields.
//!
//! # Limits
//!
//! Input over `Budget::max_bytes` is rejected (`TooLarge`); the parse is
//! cancelled with `Timeout` after `Budget::max_parse_time` (checked every 256
//! events). At most 20,000 sections per file. Invalid UTF-8 is decoded
//! lossily and counted as one parse error (`partial`): lines stay exact,
//! byte offsets after the first invalid sequence may be shifted. Extraction
//! never panics on arbitrary bytes and is deterministic.
//!
//! # Testing
//!
//! `tests/real_repo.rs` is an ignored smoke test over a real checkout
//! (`RUECA_MD_REPO=/path/to/checkout cargo test -p rueca-lang-markdown --test
//! real_repo -- --ignored --nocapture`): invariants, determinism, counts,
//! timing and the share of file links that resolve.

mod extract;
pub mod links;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::{MAX_DOC_CHARS, MAX_LINKS, MAX_SECTIONS, QN_SEPARATOR};

/// Markdown pack. Stateless; construct with `MarkdownPack::default()`.
#[derive(Debug, Default)]
pub struct MarkdownPack;

impl LanguagePack for MarkdownPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Markdown]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(
        &self,
        _path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Markdown {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    /// The repository-relative path, extension included (`docs/guide.md`).
    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        path.to_string()
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        links::resolve(from_path, &import.module, project)
    }
    fn is_builtin(&self, _name: &str) -> bool {
        false
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
}

#[cfg(test)]
mod tests;
