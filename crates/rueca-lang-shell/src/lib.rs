//! Shell language pack for Rueca: POSIX sh, bash, zsh, ksh and bats files,
//! each read as its dialect (`tree-sitter-bash` 0.25, `tree-sitter-zsh`
//! 0.64 for zsh).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! shell file into [`FileFacts`]; cross-file resolution of sourced
//! functions and script runs is the `shell` module of `rueca-resolve`.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammars**: `tree-sitter-bash` 0.25.1 (MIT) and, for zsh files,
//!   `tree-sitter-zsh` 0.64.3 (MIT, a fork of the bash grammar with zsh's
//!   syntax), both linked against the tree-sitter 0.27 runtime (they
//!   depend only on `tree-sitter-language`).
//! - **Variable bindings** come from a vendored, adapted
//!   `queries/locals.scm` (nvim-treesitter, Apache-2.0; provenance in
//!   `queries/README.md`, semantics in `locals.rs`), and its rewrite for
//!   the zsh grammar's node names (`queries/locals-zsh.scm`).
//! - Dialect detection, definitions, `source` paths, script runs, bats
//!   blocks and the env bridge are Rueca code (`dialect.rs`, `extract.rs`,
//!   `paths.rs`, `bats.rs`); a few same-length repairs of constructs a
//!   grammar rejects run before parsing (`repair.rs`, one rule set per
//!   grammar).
//!
//! # Files
//!
//! [`Language::Shell`] files: `.sh`, `.bash`, `.zsh`, `.zsh-theme`, `.ksh`,
//! `.bats`, the shell start-up files (`.bashrc`, `.bash_profile`,
//! `.profile`, `.zshrc`, `.kshrc`, …) and, detected by the indexer from
//! their first line, files without an extension whose `#!` line names a
//! shell (`sh`, `bash`, `dash`, `ash`, `ksh`, `mksh`, `pdksh`, `oksh`,
//! `zsh`, `posh`, `yash`, `bats`, directly, through `env` / `env -S`, or as
//! a `busybox` applet; `Language::from_shebang`).
//!
//! # Dialects
//!
//! One dialect per file, a pure function of the path and the bytes
//! ([`dialect::detect`]); the first rule that answers wins:
//!
//! | # | Rule | Example | Dialect |
//! |---|---|---|---|
//! | 1 | the `.bats` extension | `test/install.bats` | `bats` |
//! | 2 | a ShellCheck directive in the leading comments | `# shellcheck shell=ksh` | as named |
//! | 3 | the `#!` line | `#!/usr/bin/env -S bash -eu`, `#!/bin/dash`, `#!/bin/ksh93` | `bash`, `sh`, `ksh` |
//! | 4 | zsh's own first lines | `#compdef git`, `#autoload` | `zsh` |
//! | 5 | the file name | `x.zsh`, `x.zsh-theme`, `.zshrc`; `x.bash`, `.bashrc`; `x.ksh`, `.kshrc`; `.profile` | `zsh`; `bash`; `ksh`; `sh` |
//! | 6 | an editor modeline (first / last five lines) | `# vim: ft=zsh`, `# -*- sh-shell: bash -*-` | as named |
//! | 7 | a directory named after one shell | `completions/zsh/x.sh`, `bash_completion.d/x.sh` | `zsh`, `bash` |
//! | 8 | content signals (`.sh` files left) | `setopt` / `autoload` / `${(f)x}`; `shopt` / `BASH_SOURCE` / `[[ … ]]`; `set -A` / `${.sh.file}` | `zsh`; `bash`; `ksh` |
//! | 9 | otherwise | | `sh` |
//!
//! Content signals count each distinct builtin or form once, weighted; one
//! shell's own must outweigh the others'. A file that tests
//! `$ZSH_VERSION` / `$BASH_VERSION` is written for several shells (nvm.sh,
//! completion scripts): never `zsh` or `ksh` by content, `bash` when it
//! uses anything beyond POSIX. The dialect ends every function signature
//! as a comment (`deploy() # bash`, `function precmd # zsh`), which is how
//! it reaches search results and `get_symbol`.
//!
//! **Grammar per dialect.** `zsh` files are parsed with tree-sitter-zsh
//! (parameter flags `${(j:,:)xs}`, glob qualifiers `*(.N)`, `for x (a b)`,
//! `foreach`, `repeat`, anonymous functions, `always` blocks, `&|`, nested
//! `${${…}}` are syntax there); `sh`, `bash`, `ksh` and `bats` files with
//! tree-sitter-bash (ksh93's own forms — `${.sh.file}`, compound variables
//! — have no grammar and are errors). When the dialect's grammar leaves
//! errors and the other one reads the whole file without any, the other
//! tree is extracted (the zsh grammar accepts some bash its parent
//! rejects; a mislabelled zsh file is still read); when both leave errors
//! the dialect's tree is kept. Bats files are only read as bash. ERROR /
//! MISSING nodes of the extracted tree are counted in `parse_errors` (the
//! file is `partial`) and the rest of the file is still extracted.
//!
//! Files with a parse error, per dialect (2026-11-25, 3,471 files of 17
//! checkouts, extension-less scripts and tests included; `cargo run
//! --release -p rueca-lang-shell --example dialects -- <checkouts>`; the
//! checkouts and their commits are in the decisions log):
//!
//! | Dialect | Grammar | Files with errors | Corpus | Known unparsed |
//! |---|---|---|---|---|
//! | sh | tree-sitter-bash | 4 / 720 (0.6%) | acme.sh, nvm's test scripts, installers | `[ a '(' b ')' ]`, `${x#*{…}`, case patterns such as `--*' '*)` |
//! | bash | tree-sitter-bash | 10 / 1,155 (0.9%) | bash-it, dokku, pyenv, rbenv, asdf, git-extras, pi-hole, bats-core | nvm.sh (23 errors: the grammar's here-document scanner), `[ "$a" "$@" ]`, arithmetic over several lines with subscripts |
//! | zsh | tree-sitter-zsh | 24 / 1,075 (2.2%); ohmyzsh 8 / 556 (1.4%) | ohmyzsh, prezto, spaceship-prompt, zsh-syntax-highlighting, zsh-autosuggestions, powerlevel10k | short loops without `do` (`for x in a b` + one command), `function {a,b}_x`, dynamic function names `f_$x() {…}`, `(#b)` glob flags in `[[ … ]]`, `$x:h/…`; powerlevel10k alone: 6 of 17 |
//! | ksh | tree-sitter-bash | not measured | none found with a permissive licence at scale | `${.sh.…}`, compound variables, `typeset -n` forms |
//! | bats | tree-sitter-bash | 4 / 521 (0.8%) | bats-core, dokku, pyenv, rbenv, asdf | fixtures written to fail, `[[ x = *"a"*"$b" ]]` |
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
//!   `signature` the text before the body and the dialect (`f() # bash`,
//!   `function f # zsh`), a
//!   variable's `NAME=value` (the first line, ≤ 80 characters; masked for
//!   secret-looking names) or an env provider's `export NAME=•••` (values of
//!   providers are never shown, as in the data pack); `doc` the `#` comment
//!   block right above (the shebang excluded, ≤ 1000 chars).
//! - `exported`: every function, variable and provider (they are global).
//! - `is_test`: every definition of a test file — a test directory
//!   (`rueca_model::is_test_path`), `*_test.sh`, `test_*.sh`, `*_spec.sh`
//!   (any shell extension) or a bats file — and, in a file that sources
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
pub mod dialect;
mod extract;
mod locals;
mod paths;
mod repair;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use dialect::{Dialect, Evidence, Grammar};
pub use extract::{MAX_REFS, SCRIPT_RECEIVER, extract_with};

/// The source as `grammar` reads it, after the same-length repairs of
/// `repair.rs` (for tools that print parse errors, such as the real-repo
/// smoke test).
pub fn repaired_source(source: &[u8], grammar: Grammar) -> Vec<u8> {
    repair::repair(source, grammar).into_owned()
}

/// How one file is read (for the dialect coverage table, `examples/dialects.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub dialect: Dialect,
    /// The rule that chose the dialect.
    pub evidence: Evidence,
    /// The grammar whose tree is extracted (the dialect's, or the other
    /// one when it leaves fewer errors).
    pub grammar: Grammar,
    /// ERROR / MISSING nodes of that tree.
    pub parse_errors: u32,
}

/// The dialect, grammar and parse errors of a file; `None` when it cannot
/// be parsed inside the default budget.
pub fn stats(path: &str, source: &[u8]) -> Option<Stats> {
    let (dialect, evidence) = dialect::detect(path, source);
    let parsed = extract::parse_file(dialect, source, &Budget::default()).ok()?;
    Some(Stats { dialect, evidence, grammar: parsed.grammar, parse_errors: parsed.errors })
}

/// Shell pack. Stateless; construct with `ShellPack`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShellPack;

impl LanguagePack for ShellPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Shell]
    }
    fn extractor_version(&self) -> u32 {
        2
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
