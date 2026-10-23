//! Query engine over local segments. Implements the six alpha tools defined in
//! `docs/tools.md`; the MCP server and the CLI both dispatch through
//! [`Engine::call`], so their behavior is identical.

use std::path::PathBuf;

pub const TOOL_NAMES: &[&str] =
    &["search_symbols", "search_code", "get_symbol", "get_snippet", "trace", "get_file_outline"];

pub struct Engine {
    pub store_dir: PathBuf,
    /// Root used when a request does not name a repository.
    pub default_root: Option<PathBuf>,
}

impl Engine {
    pub fn new(store_dir: PathBuf, default_root: Option<PathBuf>) -> anyhow::Result<Engine> {
        Ok(Engine { store_dir, default_root })
    }

    /// Runs tool `name` with JSON `args` and returns the JSON result described
    /// in `docs/tools.md`. Unknown tools and invalid arguments are errors.
    pub fn call(&self, _name: &str, _args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        todo!()
    }
}
