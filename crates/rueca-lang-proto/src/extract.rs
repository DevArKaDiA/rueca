//! `.proto` → [`FileFacts`] through `protox-parse`'s `FileDescriptorProto`
//! and its source locations. The mapping rules are documented in the crate
//! root.

use std::collections::HashMap;

use prost_types::{DescriptorProto, EnumDescriptorProto, FileDescriptorProto, ServiceDescriptorProto};
use rueca_extract::{Budget, ExtractError};
use rueca_model::contract::{rpc_key, truncate_raw};
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, FileFacts, Import, Label, LocalDefId, Span};

/// The `framework` of the contract hints this pack emits.
pub const FRAMEWORK: &str = "proto";
/// Doc (leading comment) length cap, in characters.
pub const MAX_DOC_CHARS: usize = 500;
/// Signature length cap, in characters.
const MAX_SIGNATURE_CHARS: usize = 200;

// Field numbers of `descriptor.proto`, the steps of a location path.
const FILE_DEPENDENCY: i32 = 3;
const FILE_MESSAGE: i32 = 4;
const FILE_ENUM: i32 = 5;
const FILE_SERVICE: i32 = 6;
const MESSAGE_FIELD: i32 = 2;
const MESSAGE_NESTED: i32 = 3;
const MESSAGE_ENUM: i32 = 4;
const ENUM_VALUE: i32 = 2;
const SERVICE_METHOD: i32 = 2;
const NAME: i32 = 1;

pub fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    // Invalid UTF-8 is replaced (lines stay exact; byte offsets after the
    // first replacement may shift) and counted as a parse error.
    let text = String::from_utf8_lossy(source);
    let invalid_utf8 = u32::from(matches!(text, std::borrow::Cow::Owned(_)));
    let line_count = rueca_extract::line_count(source);
    let Ok(file) = protox_parse::parse("file.proto", &text) else {
        return Ok(FileFacts { parse_errors: 1 + invalid_utf8, line_count, ..FileFacts::default() });
    };
    let mut ex = Extractor::new(&text, &file);
    ex.file(&file);
    Ok(FileFacts { parse_errors: invalid_utf8, line_count, ..ex.facts })
}

/// Source information of one location path: its span and leading comment.
struct Loc {
    span: Span,
    doc: Option<String>,
}

/// One definition to add.
struct Def<'p> {
    label: Label,
    name: &'p str,
    qualified_name: String,
    parent: Option<LocalDefId>,
    path: &'p [i32],
    signature: String,
}

struct Extractor<'s> {
    src: &'s str,
    locs: HashMap<Vec<i32>, Loc>,
    package: String,
    facts: FileFacts,
}

impl<'s> Extractor<'s> {
    fn new(src: &'s str, file: &FileDescriptorProto) -> Extractor<'s> {
        let lines = line_starts(src);
        let locations = file.source_code_info.as_ref().map_or(&[][..], |i| i.location.as_slice());
        let locs = locations
            .iter()
            .filter_map(|l| {
                let span = to_span(&lines, src.len(), &l.span)?;
                Some((l.path.clone(), Loc { span, doc: doc_comment(l.leading_comments.as_deref()) }))
            })
            .collect();
        Extractor { src, locs, package: file.package().to_string(), facts: FileFacts::default() }
    }

    fn file(&mut self, file: &FileDescriptorProto) {
        for (i, dep) in file.dependency.iter().enumerate() {
            self.import(dep, &[FILE_DEPENDENCY, i as i32]);
        }
        for (i, s) in file.service.iter().enumerate() {
            self.service(s, &[FILE_SERVICE, i as i32]);
        }
        let package = self.package.clone();
        for (i, m) in file.message_type.iter().enumerate() {
            self.message(m, &[FILE_MESSAGE, i as i32], None, &package);
        }
        for (i, e) in file.enum_type.iter().enumerate() {
            self.enumeration(e, &[FILE_ENUM, i as i32], None, &package);
        }
    }

    fn import(&mut self, module: &str, path: &[i32]) {
        let span = self.locs.get(path).map(|l| l.span).unwrap_or_default();
        self.facts.imports.push(Import {
            module: module.to_string(),
            relative_level: 0,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span,
        });
    }

    fn service(&mut self, s: &ServiceDescriptorProto, path: &[i32]) {
        let name = s.name();
        let qualified_name = join(&self.package, name);
        let signature = format!("service {name}");
        let def = Def { label: Label::Interface, name, qualified_name, parent: None, path, signature };
        let Some(id) = self.def(def) else { return };
        for (j, m) in s.method.iter().enumerate() {
            let mpath = [path, &[SERVICE_METHOD, j as i32]].concat();
            let stream = |on: bool| if on { "stream " } else { "" };
            let signature = format!(
                "rpc {}({}{}) returns ({}{})",
                m.name(),
                stream(m.client_streaming()),
                m.input_type(),
                stream(m.server_streaming()),
                m.output_type()
            );
            let qualified_name = format!("{}.{}", self.facts.definitions[id as usize].qualified_name, m.name());
            let def = Def {
                label: Label::RpcMethod,
                name: m.name(),
                qualified_name,
                parent: Some(id),
                path: &mpath,
                signature,
            };
            if let Some(mid) = self.def(def) {
                self.provides(mid, rpc_key(&self.package, name, m.name()));
            }
        }
    }

    /// A `Provides` RPC hint owned by the `RpcMethod` `id`.
    fn provides(&mut self, id: LocalDefId, key: String) {
        let d = &self.facts.definitions[id as usize];
        let hint = ContractHint {
            side: ContractSide::Provides,
            kind: ContractKind::Rpc,
            key,
            raw: truncate_raw(d.signature.as_deref().unwrap_or_default()),
            span: d.span,
            owner: Some(id),
            framework: FRAMEWORK.to_string(),
        };
        self.facts.contracts.push(hint);
    }

    fn message(&mut self, m: &DescriptorProto, path: &[i32], parent: Option<LocalDefId>, scope: &str) {
        if m.options.as_ref().is_some_and(|o| o.map_entry()) {
            return; // synthetic `map<K, V>` entry type
        }
        let name = m.name();
        let qualified_name = join(scope, name);
        let signature = format!("message {name}");
        let def = Def { label: Label::Struct, name, qualified_name, parent, path, signature };
        let Some(id) = self.def(def) else { return };
        let qn = self.facts.definitions[id as usize].qualified_name.clone();
        for (k, f) in m.field.iter().enumerate() {
            let fpath = [path, &[MESSAGE_FIELD, k as i32]].concat();
            let signature = self.source_signature(&fpath).unwrap_or_else(|| f.name().to_string());
            let qualified_name = format!("{qn}.{}", f.name());
            self.def(Def {
                label: Label::Field,
                name: f.name(),
                qualified_name,
                parent: Some(id),
                path: &fpath,
                signature,
            });
        }
        for (k, n) in m.nested_type.iter().enumerate() {
            self.message(n, &[path, &[MESSAGE_NESTED, k as i32]].concat(), Some(id), &qn);
        }
        for (k, e) in m.enum_type.iter().enumerate() {
            self.enumeration(e, &[path, &[MESSAGE_ENUM, k as i32]].concat(), Some(id), &qn);
        }
    }

    fn enumeration(&mut self, e: &EnumDescriptorProto, path: &[i32], parent: Option<LocalDefId>, scope: &str) {
        let name = e.name();
        let qualified_name = join(scope, name);
        let signature = format!("enum {name}");
        let Some(id) = self.def(Def { label: Label::Enum, name, qualified_name, parent, path, signature }) else {
            return;
        };
        let qn = self.facts.definitions[id as usize].qualified_name.clone();
        for (k, v) in e.value.iter().enumerate() {
            let vpath = [path, &[ENUM_VALUE, k as i32]].concat();
            let signature = format!("{} = {}", v.name(), v.number());
            let qualified_name = format!("{qn}.{}", v.name());
            self.def(Def {
                label: Label::Constant,
                name: v.name(),
                qualified_name,
                parent: Some(id),
                path: &vpath,
                signature,
            });
        }
    }

    /// Adds a definition located at `def.path`; `None` (nothing added) when
    /// the parser gave no location for it.
    fn def(&mut self, def: Def<'_>) -> Option<LocalDefId> {
        let loc = self.locs.get(def.path)?;
        let name_path = [def.path, &[NAME]].concat();
        let name_span = self.locs.get(&name_path).map_or(loc.span, |l| l.span);
        self.facts.definitions.push(Definition {
            label: def.label,
            name: def.name.to_string(),
            qualified_name: def.qualified_name,
            parent: def.parent,
            span: loc.span,
            name_span,
            signature: Some(def.signature.chars().take(MAX_SIGNATURE_CHARS).collect()),
            doc: loc.doc.clone(),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        Some((self.facts.definitions.len() - 1) as LocalDefId)
    }

    /// The source text at `path`, whitespace collapsed, without the final
    /// `;` (a field as written: `map<string, int32> counts = 3`).
    fn source_signature(&self, path: &[i32]) -> Option<String> {
        let span = self.locs.get(path)?.span;
        let text = self.src.get(span.start_byte as usize..span.end_byte as usize)?;
        let text = text.trim().trim_end_matches(';');
        Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// `scope.name`, or `name` when the scope (package) is empty.
fn join(scope: &str, name: &str) -> String {
    if scope.is_empty() { name.to_string() } else { format!("{scope}.{name}") }
}

/// Byte offset of the start of every line.
fn line_starts(src: &str) -> Vec<usize> {
    std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

/// A `SourceCodeInfo` span (`[line, col, end_col]` or `[line, col,
/// end_line, end_col]`, 0-based, columns in bytes) as a [`Span`].
fn to_span(lines: &[usize], len: usize, span: &[i32]) -> Option<Span> {
    let (sl, sc, el, ec) = match *span {
        [l, c, e] => (l, c, l, e),
        [l, c, el, ec] => (l, c, el, ec),
        _ => return None,
    };
    let byte = |l: i32, c: i32| -> Option<usize> {
        let at = lines.get(usize::try_from(l).ok()?)? + usize::try_from(c).ok()?;
        (at <= len).then_some(at)
    };
    let (start, end) = (byte(sl, sc)?, byte(el, ec)?);
    Some(Span {
        start_byte: start as u32,
        end_byte: end as u32,
        start_line: sl as u32,
        start_col: sc as u32,
        end_line: el as u32,
        end_col: ec as u32,
    })
}

/// A leading comment as a doc: lines trimmed, at most [`MAX_DOC_CHARS`].
fn doc_comment(comment: Option<&str>) -> Option<String> {
    let text = comment?.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| text.chars().take(MAX_DOC_CHARS).collect())
}
