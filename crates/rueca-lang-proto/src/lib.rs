//! Protocol Buffers language pack for Rueca: `.proto` services, RPC methods,
//! messages and enums in the same graph as code, and the RPC contracts they
//! provide (RFC 0006 phase 4).
//!
//! Parser: [`protox-parse`](https://github.com/andrewhickman/protox) 0.10
//! (MIT OR Apache-2.0, pure Rust, no `protoc`), which turns one file into a
//! `FileDescriptorProto` with source locations (byte spans and leading
//! comments). Types are not resolved and imported files are not read. See
//! `rueca_extract::LanguagePack` for the contract; the pack does not use
//! tree-sitter.
//!
//! Files: `.proto` ([`Language::Proto`]).
//!
//! # Definitions
//!
//! The module name is empty: local qualified names carry the `package`
//! (`helloworld.v1.Greeter.SayHello`), so the global name is the protobuf
//! full name. Every definition is `exported`; spans and name spans are the
//! parser's source locations; `doc` is the leading comment (lines trimmed and
//! joined, ≤ 500 chars).
//!
//! | Declaration | Label | Qualified name | Signature |
//! |---|---|---|---|
//! | `service Greeter` | `Interface` | `pkg.Greeter` | `service Greeter` |
//! | `rpc SayHello(...)` | `RpcMethod`, child of the service | `pkg.Greeter.SayHello` | `rpc SayHello(stream Req) returns (Resp)` (`stream` kept, types as written) |
//! | `message User` (nested messages nested) | `Struct` | `pkg.User`, `pkg.User.Address` | `message User` |
//! | a field | `Field`, child of its message | `pkg.User.name` | the field as written, whitespace collapsed, without `;` (`map<string, int32> counts = 3`) |
//! | `enum Kind` (top-level or nested) | `Enum` | `pkg.Kind`, `pkg.User.Kind` | `enum Kind` |
//! | an enum value | `Constant`, child of its enum | `pkg.Kind.KIND_WEB` | `KIND_WEB = 1` |
//!
//! Synthetic `map<K, V>` entry messages are not definitions; `oneof`
//! members are fields of the message; `extend` blocks, options and
//! reserved ranges produce nothing.
//!
//! # Imports and contracts
//!
//! `import "a/b.proto"` (also `public` / `weak`) is an [`Import`] with
//! `module` = the path. [`LanguagePack::resolve_import`] finds it relative
//! to the repository root, else to each ancestor directory of the importing
//! file (nearest first); other include roots are not known.
//!
//! Every `rpc` provides an RPC contract: a `Provides` hint, framework
//! `proto`, key `pkg.Service/Method` (`rueca_model::contract::rpc_key`;
//! `Service/Method` without a package), owner = the `RpcMethod`, raw = its
//! signature, span = the `rpc` declaration.
//!
//! # Limits
//!
//! Input over `Budget::max_bytes` is rejected (`TooLarge`). A file the
//! parser rejects yields no definitions and one parse error (`partial`);
//! invalid UTF-8 is decoded lossily and counted as a parse error. No
//! references are emitted (message and RPC types are not linked yet).

mod extract;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::{FRAMEWORK, MAX_DOC_CHARS};

/// The `.proto` pack. Stateless; construct with `ProtoPack` or
/// `ProtoPack::default()`.
#[derive(Debug, Default)]
pub struct ProtoPack;

impl LanguagePack for ProtoPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Proto]
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
        if language != Language::Proto {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    /// Empty: local qualified names carry the package.
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        resolve_import(from_path, &import.module, project)
    }
    fn is_builtin(&self, _name: &str) -> bool {
        false
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
}

/// `module` from the repository root, else from each ancestor directory of
/// `from_path`, nearest first.
fn resolve_import(from_path: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    let module = module.trim().trim_start_matches("./");
    if module.is_empty() {
        return None;
    }
    if project.contains(module) {
        return Some(module.to_string());
    }
    let mut dir = from_path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        let candidate = format!("{parent}/{module}");
        if project.contains(&candidate) {
            return Some(candidate);
        }
        dir = parent;
    }
    None
}

#[cfg(test)]
mod tests;
