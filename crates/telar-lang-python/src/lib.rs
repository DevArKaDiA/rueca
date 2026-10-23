//! Python language pack. See `telar_extract::LanguagePack` for the contract.

use telar_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use telar_model::{FileFacts, Import, Language};

/// Python pack. Stateless; construct with `PythonPack::default()`.
#[derive(Debug, Default)]
pub struct PythonPack;

impl LanguagePack for PythonPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(&self, _path: &str, _language: Language, _source: &[u8], _budget: &Budget) -> Result<FileFacts, ExtractError> {
        todo!("python extraction")
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
        &["self", "cls"]
    }
}
