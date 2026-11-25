//! Shell language pack for Rueca (`tree-sitter-bash` 0.25): POSIX sh and
//! bash; zsh and ksh are read with the bash grammar, best effort.
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! shell file into [`FileFacts`]; cross-file resolution of sourced
//! functions and script runs is the `shell` module of `rueca-resolve`.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-bash` 0.25.1 (MIT), linked against the
//!   tree-sitter 0.27 runtime (it depends only on `tree-sitter-language`).
//! - **Variable bindings** come from a vendored, adapted
//!   `queries/locals.scm` (nvim-treesitter, Apache-2.0; provenance in
//!   `queries/README.md`, semantics in `locals.rs`).
//! - Definitions, `source` paths, script runs, bats blocks and the env
//!   bridge are Rueca code (`extract.rs`, `paths.rs`, `bats.rs`).
//!
//! # Files
//!
//! [`Language::Shell`] files: `.sh`, `.bash`, `.zsh`, `.zsh-theme`, `.ksh`,
//! `.bats`, the shell start-up files (`.bashrc`, `.bash_profile`,
//! `.profile`, `.zshrc`, …) and, detected by the indexer from their first
//! line, files without an extension whose `#!` line names a shell (`sh`,
//! `bash`, `dash`, `ash`, `ksh`, `mksh`, `zsh`, `bats`, directly or through
//! `env`; `Language::from_shebang`).
//!
//! **zsh** is parsed with the bash grammar: functions, `source`, calls and
//! expansions written in the common subset are extracted; zsh-only syntax
//! (parameter flags `${(j:,:)xs}`, glob qualifiers `*(.)`, `for x (a b)`,
//! anonymous functions `() { … }`, `always` blocks) yields ERROR nodes,
//! counted in `parse_errors` (the file is `partial`) while the rest of the
//! file is still extracted.
//!
//! # Module names and qualified names
//!
//! The module name of a file is its path with the extension
//! (`scripts/deploy.sh`), so global names are `scripts/deploy.sh:deploy`.
//! Shell has no package identity (no manifest names a shell library), so
//! the pack publishes nothing to the organization layer (RFC 0006).
//!
//! | Definition | Label | Local qualified name |
//! |---|---|---|
//! | `f() { … }`, `function f { … }`, `function f() { … }` (also nested in another function, or in an `if`) | `Function`, file level | `f` |
//! | file-level `NAME=value`, `declare NAME=…`, `typeset NAME` | `Variable` | `NAME` |
//! | file-level `readonly NAME=…`, `declare -r NAME=…` | `Constant` | `NAME` |
//! | `export NAME[=…]`, `declare -x` / `typeset -x` / `local -x NAME`, `NAME=value cmd` (the command's environment) | `EnvVar` (provider), child of the enclosing function | `NAME`, `f.NAME` in function `f` |
//! | bats `@test "name" { … }` | `Test` | the test name |
//!
//! - Every function is a file-level definition (`parent: None`) whatever
//!   its nesting: shell functions are global once the definition runs, so a
//!   function defined inside another is visible to the whole file and to
//!   every file that sources it. Two definitions of one name in a file
//!   (`if [ -n "$ZSH_VERSION" ]; then f() {…}; else f() {…}; fi`) are one
//!   node (the first).
//! - `span` is the definition statement; `name_span` the name;
//!   `signature` the text before the body (`f()`, `function f`), a
//!   variable's `NAME=value` (the first line, ≤ 80 characters; masked for
//!   secret-looking names) or an env provider's `export NAME=•••` (values of
//!   providers are never shown, as in the data pack); `doc` the `#` comment
//!   block right above (the shebang excluded, ≤ 1000 chars).
//! - `exported`: every function, variable and provider (they are global).
//! - `is_test`: every definition of a test file — a test directory
//!   (`rueca_model::is_test_path`), `*_test.sh`, `test_*.sh`, `*_spec.sh`
//!   (any shell extension) or `*.bats` — and, in a file that sources
//!   shunit2 (`. shunit2`, `source "$(which shunit2)"`), the functions
//!   named `test*` and the shunit2 hooks (`setUp`, `tearDown`,
//!   `oneTimeSetUp`, `oneTimeTearDown`, `suite`). Because this depends on
//!   the path, [`LanguagePack::facts_depend_on_path`] is true.
//!
//! # References
//!
//! `enclosing` is the innermost function or bats test (`None` at file
//! level: the script's own top-level code belongs to its `File` node).
//!
//! | Source | Reference |
//! |---|---|
//! | a command word that is a literal name: `f`, `f args`, in pipelines, lists (`a && b \|\| c`), subshells, `$(f)` / `` `f` `` / `<(f)`, `if f; then`, `! f`, `time f`, `f &` | `Call` `f` |
//! | `eval 'f x'` with a literal (expansion-free) string | `Call` for each command word of the string, re-parsed at its position |
//! | `trap 'cleanup; exit' EXIT`, `trap cleanup INT` (literal handler) | `Read` for each command word of the handler (a callback: the resolver links it with `REFERENCES`, not `CALLS`) |
//! | bats `run f` (test files) | `Call` `f` |
//! | a command word that is a path or a script name (`./deploy.sh`, `scripts/x.sh`, `"$DIR/x.sh"`), the operand of `bash` / `sh` / `zsh` / `bats` (not `-c`), of `exec`, `nohup`, `sudo`, `env`, `xargs`, `timeout`, … | `Call` with receiver `$script` and the encoded path as name (`paths.rs`); the resolver links it to the script's `File` node |
//! | `command f`, `exec f`, `xargs f`, `sudo f`, `env f` | nothing: these never run a shell function |
//! | `$NAME`, `${NAME…}` inside a function, `NAME` a file-level variable not declared `local` there | `Read` `NAME` |
//! | `$NAME`, `${NAME}`, `${NAME:-default}`, `${NAME:?msg}`, `${#NAME}` (also in strings and here-documents), `NAME` bound nowhere in the file, upper case, not a shell variable (`BASH_SOURCE`, `LINENO`, `OPTARG`, …) | `EnvVar` `NAME` (RFC 0003 §7.2); the default value has no slot in the bridge and is not kept |
//!
//! A name counts as bound when the file assigns it anywhere (`NAME=…`,
//! `local` / `declare` / `export` / `readonly` with or without a value,
//! `for NAME in`, `read NAME`, `getopts … NAME`, `mapfile NAME`, `printf -v
//! NAME`, `select NAME`): such a variable belongs to the script, whatever
//! the environment holds. Lower-case names are never environment reads
//! (shell scoping is dynamic: a function reads the caller's `local`s).
//! `Read` and `EnvVar` are deduplicated per `(enclosing, name)`; at most
//! 20,000 references per file. `$1`, `$@`, `$?` and other special
//! parameters are not references.
//!
//! # Imports
//!
//! `source FILE` and `. FILE` (also `\. FILE`) are star imports (`names:
//! ["*"]`) of the path the file determines (see `paths.rs`: literal
//! paths, the script's directory through `$(dirname "$0")`,
//! `${BASH_SOURCE%/*}`, `$(cd "$(dirname …)" && pwd)`, zsh `${0:A:h}`, and
//! variables the file assigns one of those; an unknown prefix such as
//! `$NVM_DIR/nvm.sh` keeps the known tail). In a `.bats` file, `load NAME`
//! and `bats_load_library NAME` import `NAME.bash` next to the test file.
//! [`LanguagePack::resolve_import`] maps the path to a file of the snapshot
//! (`paths::resolve`); a dynamic path (`source "$f"`, `. "$dir/$name.sh"`)
//! imports nothing.
//!
//! # Limitations
//!
//! - Calls are by name: what a function's caller has sourced is not
//!   followed into the callee's file (the resolver's unique-name rung
//!   covers functions a library calls from a sibling library).
//! - Aliases, functions defined by `eval` or `autoload` (zsh function
//!   files), `$cmd args` with a variable command word, and arrays of
//!   commands are not followed.
//! - A path built from variables the file does not assign resolves only by
//!   its literal tail.

mod bats;
mod builtins;
mod extract;
mod locals;
mod paths;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::{MAX_REFS, SCRIPT_RECEIVER};

/// Shell pack. Stateless; construct with `ShellPack`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShellPack;

impl LanguagePack for ShellPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Shell]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Shell {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(path, source, budget)
    }
    /// The path with its extension (crate docs, *Module names*).
    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        path.to_string()
    }
    /// The file a sourced or run path names (crate docs, *Imports*).
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        paths::resolve(from_path, &import.module, project)
    }
    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_builtin(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
    /// `is_test` depends on test directories and file names.
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
