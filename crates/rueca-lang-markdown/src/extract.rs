//! Markdown → [`FileFacts`]: heading sections and file links.
//!
//! One pass over `pulldown-cmark`'s offset iterator collects top-level
//! headings, the first prose paragraph after each heading, and link / image
//! destinations (plus `href` / `src` attributes of raw HTML). Sections are
//! then built from the heading list: nesting by level, spans up to the next
//! heading of the same or a higher level, disambiguated qualified names.
//! The mapping rules are documented in the crate root.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Instant;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use rueca_extract::{Budget, ExtractError};
use rueca_model::{Definition, FileFacts, Import, Label, Span};

/// Separator between heading names in a section's qualified name.
pub const QN_SEPARATOR: &str = " > ";
/// At most this many sections per file; later headings are ignored.
pub const MAX_SECTIONS: usize = 20_000;
/// At most this many distinct link destinations per file.
pub const MAX_LINKS: usize = 20_000;
/// Section `doc` length cap, in characters (including the ellipsis).
pub const MAX_DOC_CHARS: usize = 500;
/// Heading name and signature cap, in characters.
const MAX_NAME_CHARS: usize = 200;
/// The deadline is checked every this many parser events.
const DEADLINE_EVERY: usize = 256;

/// A top-level heading as seen by the parser.
struct Heading {
    level: usize,
    /// Byte range of the heading block (both lines for setext headings).
    range: Range<usize>,
    /// Plain text without image alt text.
    text: String,
    /// Image alt text, used as the name when `text` is empty (`# ![logo](x)`).
    alt: String,
    /// Byte range covered by the heading's inline content.
    inline: Option<Range<usize>>,
    /// First qualifying paragraph after the heading (before the next heading).
    doc: Option<String>,
}

/// Paragraph being collected as a candidate section doc.
struct Para {
    text: String,
    image_depth: usize,
}

pub fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let deadline = Instant::now() + budget.max_parse_time;
    // Invalid UTF-8 is replaced (lines stay exact: `\n` is never part of an
    // invalid sequence; byte offsets after the first replacement may shift).
    let (text, invalid_utf8) = match std::str::from_utf8(source) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(source), true),
    };
    let text: &str = &text;
    let lines = LineIndex::new(text);

    // A BOM and a leading front-matter block are skipped; the parser sees the
    // rest and its offsets are shifted back by `base`.
    let mut base = if text.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { 0 };
    base += front_matter_len(&text[base..]);
    let body = &text[base..];

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut collector = Collector::default();
    for (n, (event, range)) in Parser::new_ext(body, options).into_offset_iter().enumerate() {
        if n % DEADLINE_EVERY == 0 && Instant::now() > deadline {
            return Err(ExtractError::Timeout);
        }
        collector.event(event, range.start + base..range.end + base);
    }
    let Collector { headings, links, .. } = collector;

    let definitions = sections(text, &lines, &headings);
    let imports = links
        .list
        .into_iter()
        .map(|(module, range)| Import {
            module,
            relative_level: 0,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span: lines.span(text, range),
        })
        .collect();
    Ok(FileFacts {
        definitions,
        references: Vec::new(),
        imports,
        parse_errors: u32::from(invalid_utf8),
        line_count: rueca_extract::line_count(source),
    })
}

/// Link kinds that can point at a repository file (autolinks and e-mail
/// autolinks are always URLs; wiki links are not enabled).
fn keeps(link_type: LinkType) -> bool {
    !matches!(link_type, LinkType::Autolink | LinkType::Email | LinkType::WikiLink { .. })
}

/// Distinct local link destinations, in document order.
#[derive(Default)]
struct Links {
    list: Vec<(String, Range<usize>)>,
    seen: HashSet<String>,
}

impl Links {
    fn add(&mut self, dest: &str, range: Range<usize>) {
        let dest = dest.trim();
        if self.list.len() >= MAX_LINKS || !crate::links::is_local(dest) || self.seen.contains(dest) {
            return;
        }
        self.seen.insert(dest.to_string());
        self.list.push((dest.to_string(), range));
    }
}

/// State of the single pass over the parser events.
#[derive(Default)]
struct Collector {
    headings: Vec<Heading>,
    links: Links,
    /// Block containers (block quotes, list items, footnotes) open around
    /// the current event: headings and paragraphs inside them do not
    /// structure the document.
    depth: usize,
    in_heading: Option<Heading>,
    heading_image_depth: usize,
    para: Option<Para>,
}

impl Collector {
    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        if !matches!(event, Event::End(TagEnd::Heading(_)))
            && let Some(h) = self.in_heading.as_mut()
        {
            let r = h.inline.get_or_insert(range.clone());
            r.start = r.start.min(range.start);
            r.end = r.end.max(range.end);
        }
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag),
            Event::Text(t) | Event::Code(t) => self.text(&t),
            Event::SoftBreak | Event::HardBreak => self.space(),
            Event::Html(html) | Event::InlineHtml(html) => {
                for dest in crate::links::html_link_targets(&html) {
                    self.links.add(dest, range.clone());
                }
            }
            _ => {}
        }
    }

    /// Whether the current position is at the top level, outside a heading.
    fn at_top_level(&self) -> bool {
        self.depth == 0 && self.in_heading.is_none()
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        match tag {
            Tag::Heading { level, .. } if self.at_top_level() => {
                self.para = None;
                self.in_heading = Some(Heading {
                    level: level as usize,
                    range,
                    text: String::new(),
                    alt: String::new(),
                    inline: None,
                    doc: None,
                });
                self.heading_image_depth = 0;
            }
            Tag::BlockQuote(_) | Tag::Item | Tag::FootnoteDefinition(_) => self.depth += 1,
            // Only the first qualifying paragraph of the latest heading.
            Tag::Paragraph if self.at_top_level() && self.headings.last().is_some_and(|h| h.doc.is_none()) => {
                self.para = Some(Para { text: String::new(), image_depth: 0 });
            }
            Tag::Link { link_type, dest_url, .. } if keeps(link_type) => self.links.add(&dest_url, range),
            Tag::Image { link_type, dest_url, .. } => {
                if keeps(link_type) {
                    self.links.add(&dest_url, range);
                }
                if self.in_heading.is_some() {
                    self.heading_image_depth += 1;
                }
                if let Some(p) = self.para.as_mut() {
                    p.image_depth += 1;
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                if let Some(h) = self.in_heading.take()
                    && self.headings.len() < MAX_SECTIONS
                {
                    self.headings.push(h);
                }
            }
            TagEnd::BlockQuote(_) | TagEnd::Item | TagEnd::FootnoteDefinition => {
                self.depth = self.depth.saturating_sub(1)
            }
            TagEnd::Paragraph => self.end_paragraph(),
            TagEnd::Image => {
                if self.in_heading.is_some() {
                    self.heading_image_depth = self.heading_image_depth.saturating_sub(1);
                }
                if let Some(p) = self.para.as_mut() {
                    p.image_depth = p.image_depth.saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    fn end_paragraph(&mut self) {
        let Some(p) = self.para.take() else { return };
        let doc = collapse_ws(&p.text);
        if !doc.is_empty()
            && let Some(h) = self.headings.last_mut()
        {
            h.doc = Some(truncate_chars(&doc, MAX_DOC_CHARS));
        }
    }

    fn text(&mut self, t: &str) {
        if let Some(h) = self.in_heading.as_mut() {
            if self.heading_image_depth > 0 { h.alt.push_str(t) } else { h.text.push_str(t) }
        } else if let Some(p) = self.para.as_mut()
            && p.image_depth == 0
        {
            p.text.push_str(t);
        }
    }

    fn space(&mut self) {
        if let Some(h) = self.in_heading.as_mut() {
            h.text.push(' ');
        } else if let Some(p) = self.para.as_mut() {
            p.text.push(' ');
        }
    }
}

/// Builds `Section` definitions from the heading list.
fn sections(text: &str, lines: &LineIndex, headings: &[Heading]) -> Vec<Definition> {
    let mut defs: Vec<Definition> = Vec::with_capacity(headings.len());
    // Open sections: (level, index into `defs`).
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    // Headings without any text are not sections and do not end one.
    let named: Vec<(&Heading, String)> = headings
        .iter()
        .filter_map(|h| {
            let t = collapse_ws(&h.text);
            let name = if t.is_empty() { collapse_ws(&h.alt) } else { t };
            (!name.is_empty()).then(|| (h, truncate_chars(&name, MAX_NAME_CHARS)))
        })
        .collect();
    for (i, (h, name)) in named.iter().enumerate() {
        let (h, name) = (*h, name.clone());
        while stack.last().is_some_and(|&(level, _)| level >= h.level) {
            stack.pop();
        }
        let parent = stack.last().map(|&(_, d)| d);
        let mut qn = match parent {
            Some(p) => format!("{}{QN_SEPARATOR}{name}", defs[p as usize].qualified_name),
            None => name.clone(),
        };
        if used.contains(&qn) {
            let mut n = 2;
            while used.contains(&format!("{qn} ({n})")) {
                n += 1;
            }
            qn = format!("{qn} ({n})");
        }
        used.insert(qn.clone());

        // The section runs to the next heading of the same or a higher level
        // (or the end of the file), without trailing blank lines.
        let boundary =
            named[i + 1..].iter().find(|(n, _)| n.level <= h.level).map_or(text.len(), |(n, _)| n.range.start);
        let head_end = trim_end(text, h.range.start, h.range.end);
        let end = trim_end(text, h.range.start, boundary).max(head_end);
        let inline = h.inline.clone().unwrap_or(h.range.start..head_end);
        let inline = inline.start..trim_end(text, inline.start, inline.end.min(head_end).max(inline.start));
        let raw = text.get(inline.clone()).unwrap_or("");
        let raw = rueca_extract::first_line(raw, MAX_NAME_CHARS);
        let signature = format!("{} {}", "#".repeat(h.level), if raw.is_empty() { name.as_str() } else { &raw });

        let id = defs.len() as u32;
        defs.push(Definition {
            label: Label::Section,
            name,
            qualified_name: qn,
            parent,
            span: lines.span(text, h.range.start..end),
            name_span: lines.span(text, inline),
            signature: Some(signature),
            doc: h.doc.clone(),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        stack.push((h.level, id));
    }
    defs
}

/// End of `text[start..end]` with trailing whitespace removed (≥ `start`).
fn trim_end(text: &str, start: usize, end: usize) -> usize {
    let end = end.min(text.len());
    let start = start.min(end);
    match text.get(start..end) {
        Some(s) => start + s.trim_end().len(),
        None => end,
    }
}

/// Length of a YAML (`---` … `---` / `...`) or TOML (`+++` … `+++`) front
/// matter block at the very start of `text`, including its closing line; 0
/// when there is none.
pub(crate) fn front_matter_len(text: &str) -> usize {
    let first_end = text.find('\n').map_or(text.len(), |i| i + 1);
    let fence = text[..first_end].trim_end();
    let closers: &[&str] = match fence {
        "---" => &["---", "..."],
        "+++" => &["+++"],
        _ => return 0,
    };
    let mut pos = first_end;
    while pos < text.len() {
        let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1);
        if closers.contains(&text[pos..line_end].trim_end()) {
            return line_end;
        }
        pos = line_end;
    }
    0
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Byte offset → (line, column) over the decoded text.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
        LineIndex { starts }
    }

    fn point(&self, offset: usize) -> (u32, u32) {
        let line = self.starts.partition_point(|&s| s <= offset).saturating_sub(1);
        (line as u32, (offset - self.starts[line]) as u32)
    }

    fn span(&self, text: &str, range: Range<usize>) -> Span {
        let start = range.start.min(text.len());
        let end = range.end.clamp(start, text.len());
        let (start_line, start_col) = self.point(start);
        let (end_line, end_col) = self.point(end);
        Span { start_byte: start as u32, end_byte: end as u32, start_line, start_col, end_line, end_col }
    }
}
