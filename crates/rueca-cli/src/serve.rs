//! `rueca serve` (RFC 0008): the service mode's query role (phase 1) and
//! indexer role (phase 2; `--role indexer`, or `all` for both), from the
//! `rueca-serve` crate (feature `service`, on by default).
//!
//! ```text
//! rueca serve [--config FILE] [--role query|indexer|all] [--check-config]
//! ```
//!
//! Precedence: argument, environment (`RUECA_CONFIG`, `RUECA_ROLE`,
//! `RUECA_STORE_URL`, `RUECA_LOG`), file, default. Logging starts after the
//! configuration is read, as its `[telemetry]` table says.

use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(feature = "service")]
pub fn cmd_serve(config: Option<PathBuf>, role: Option<&str>, check_config: bool, verbose: u8) -> ExitCode {
    use rueca_serve::{Role, Sources};

    let role = match role.map(Role::parse).transpose() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rueca: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let sources = Sources { path: config, role };
    let env = |k: &str| std::env::var(k).ok();
    if check_config {
        let report = rueca_serve::check(&sources, &env);
        for w in &report.warnings {
            println!("warning: {w}");
        }
        for e in &report.errors {
            println!("error: {e}");
        }
        if report.errors.is_empty() {
            println!("configuration is valid");
            return ExitCode::SUCCESS;
        }
        return ExitCode::FAILURE;
    }
    let loaded = match rueca_serve::config::load(&sources, &env) {
        Ok(l) => l,
        Err(problems) => {
            for p in problems {
                eprintln!("rueca: {p}");
            }
            return ExitCode::FAILURE;
        }
    };
    let filter = std::env::var("RUECA_LOG").ok().or_else(|| match verbose {
        0 => None,
        1 => Some("debug".to_string()),
        _ => Some("trace".to_string()),
    });
    let _logs = rueca_serve::logging::init_logging(&loaded.config.telemetry, filter.as_deref());
    match rueca_serve::run(loaded) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "rueca serve failed");
            eprintln!("rueca: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "service"))]
pub fn cmd_serve(_config: Option<PathBuf>, _role: Option<&str>, _check_config: bool, _verbose: u8) -> ExitCode {
    eprintln!("rueca: this build has no `serve` (rebuild with the `service` feature)");
    ExitCode::FAILURE
}
