//! XML of known kinds with `roxmltree` (MIT OR Apache-2.0; DTDs rejected,
//! node limit [`MAX_NODES`]): Maven `pom.xml`, MSBuild projects (`*.csproj`,
//! `*.fsproj`, `*.vbproj`) and `AndroidManifest.xml`. Other XML is not
//! detected as a language.
//!
//! Elements under the root become `Key`s by nesting (the root element is
//! not a node: `pom.xml:dependencies.dependency["org.x:core"]`). Repeated
//! elements are keyed by identity — Maven `groupId:artifactId`, MSBuild
//! `Include`, Android `android:name` — else by index. A leaf's text (or its
//! attributes) is the preview. Imports: Maven `modules/module` and
//! `parent/relativePath`, MSBuild `ProjectReference Include`.

use std::collections::HashMap;

use roxmltree::{Document, Node as XNode, ParsingOptions};
use rueca_extract::ExtractError;
use rueca_model::{Label, LocalDefId};

use crate::emit::{Def, Out};
use crate::keypath;
use crate::text::{self, MASK};
use crate::tree::MAX_DEPTH;

/// `roxmltree` node limit (RFC 0003 §4.7).
pub const MAX_NODES: u32 = 200_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlKind {
    Maven,
    MsBuild,
    Android,
    Other,
}

fn kind_of(path: &str) -> XmlKind {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "pom.xml" {
        XmlKind::Maven
    } else if name.ends_with("proj") {
        XmlKind::MsBuild
    } else if name == "AndroidManifest.xml" {
        XmlKind::Android
    } else {
        XmlKind::Other
    }
}

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

fn identity(kind: XmlKind, n: XNode<'_, '_>) -> Option<String> {
    let child_text = |tag: &str| {
        n.children().find(|c| c.is_element() && c.tag_name().name() == tag).and_then(|c| c.text()).map(str::trim)
    };
    match kind {
        XmlKind::Maven => {
            let a = child_text("artifactId").filter(|s| !s.is_empty())?;
            Some(match child_text("groupId").filter(|s| !s.is_empty()) {
                Some(g) => format!("{g}:{a}"),
                None => a.to_string(),
            })
        }
        XmlKind::MsBuild => n.attribute("Include").or_else(|| n.attribute("Update")).map(str::to_string),
        XmlKind::Android => n.attribute((ANDROID_NS, "name")).map(str::to_string),
        XmlKind::Other => None,
    }
}

pub(crate) fn extract(out: &mut Out<'_>, path: &str) -> Result<(), ExtractError> {
    let src = out.src;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..ParsingOptions::default() };
    let doc = Document::parse_with_options(src, opts).map_err(|e| ExtractError::Parser(e.to_string()))?;
    let kind = kind_of(path);
    element(out, kind, doc.root_element(), None, "", 0);
    Ok(())
}

fn leaf_text<'a>(n: XNode<'a, '_>) -> Option<&'a str> {
    if n.children().any(|c| c.is_element()) {
        return None;
    }
    n.text().map(str::trim).filter(|t| !t.is_empty())
}

fn element(out: &mut Out<'_>, kind: XmlKind, node: XNode<'_, '_>, parent: Option<LocalDefId>, qn: &str, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for c in node.children().filter(XNode::is_element) {
        *counts.entry(c.tag_name().name()).or_default() += 1;
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for child in node.children().filter(XNode::is_element) {
        let tag = child.tag_name().name();
        let n = seen.entry(tag).or_default();
        let index = *n;
        *n += 1;
        let id = identity(kind, child);
        let mut path = keypath::join(qn, &keypath::key_segment(tag));
        let name = match (&id, counts[tag] > 1) {
            (Some(id), _) => {
                path = keypath::join(&path, &keypath::id_segment(id));
                id.clone()
            }
            (None, true) => {
                path = keypath::join(&path, &keypath::index_segment(index));
                tag.to_string()
            }
            (None, false) => tag.to_string(),
        };
        let range = child.range();
        let name_span = range.start + 1..(range.start + 1 + child.tag_name().name().len()).min(range.end);
        let preview = match leaf_text(child) {
            Some(_) if text::is_secret_name(tag) => MASK.to_string(),
            Some(t) => text::preview(t),
            None => {
                let attrs: Vec<String> =
                    child.attributes().take(3).map(|a| format!("{}={}", a.name(), text::preview(a.value()))).collect();
                let kids: Vec<&str> =
                    child.children().filter(XNode::is_element).take(3).map(|c| c.tag_name().name()).collect();
                if !kids.is_empty() { format!("{{{}}}", kids.join(", ")) } else { format!("{{{}}}", attrs.join(", ")) }
            }
        };
        let doc = child
            .prev_sibling()
            .and_then(|p| {
                if p.is_text() && p.text().is_some_and(|t| t.trim().is_empty()) { p.prev_sibling() } else { Some(p) }
            })
            .filter(XNode::is_comment)
            .and_then(|c| c.text())
            .map(|t| text::truncate(t.trim(), 500))
            .filter(|t| !t.is_empty());
        let Some(def) = out.def(Def {
            label: Label::Key,
            name,
            qn: path.clone(),
            parent,
            span: range.clone(),
            name_span,
            signature: Some(format!("{tag}: {preview}")),
            doc,
        }) else {
            return;
        };
        match (kind, tag, node.tag_name().name()) {
            (XmlKind::Maven, "module", "modules") | (XmlKind::Maven, "relativePath", "parent") => {
                if let Some(t) = leaf_text(child) {
                    out.import(t, &range);
                }
            }
            (XmlKind::MsBuild, "ProjectReference", _) => {
                if let Some(inc) = child.attribute("Include") {
                    out.import(&inc.replace('\\', "/"), &range);
                }
            }
            _ => {}
        }
        element(out, kind, child, Some(def), &path, depth + 1);
    }
}
