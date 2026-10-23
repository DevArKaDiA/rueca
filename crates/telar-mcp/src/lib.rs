//! MCP server over stdio exposing the six alpha tools (`docs/tools.md`).

use std::path::PathBuf;

pub struct ServerOptions {
    pub store_dir: PathBuf,
    /// Working tree served by default. If not indexed yet and `auto_index` is
    /// set, it is indexed before serving.
    pub root: Option<PathBuf>,
    pub auto_index: bool,
}

/// Serves MCP on stdin/stdout until the client disconnects. Logs go to stderr.
pub fn serve_stdio(_opts: ServerOptions) -> anyhow::Result<()> {
    todo!()
}
