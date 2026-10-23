//! TypeScript / TSX / JavaScript language pack. See `telar_extract::LanguagePack`.

use telar_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use telar_model::{FileFacts, Import, Language};

/// ECMAScript pack (TypeScript, TSX, JavaScript/JSX). Construct with `TypeScriptPack::default()`.
#[derive(Debug, Default)]
pub struct TypeScriptPack;

impl LanguagePack for TypeScriptPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::TypeScript, Language::Tsx, Language::JavaScript]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(&self, _path: &str, _language: Language, _source: &[u8], _budget: &Budget) -> Result<FileFacts, ExtractError> {
        todo!("typescript extraction")
    }
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        todo!()
    }
    fn resolve_import(&self, _from_path: &str, _import: &Import, _project: &ProjectFiles) -> Option<String> {
        todo!()
    }
    fn is_builtin(&self, _name: &str) -> bool {
        todo!()
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
}
