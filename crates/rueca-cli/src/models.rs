//! `rueca models`: the weights cache (RFC 0007 §4.4).

use std::io::IsTerminal;
use std::path::Path;

use clap::Subcommand;
use rueca_embed::{CacheState, ModelCache, Preset};
use serde_json::{Value, json};

/// `rueca models` actions.
#[derive(Subcommand, Debug)]
pub enum ModelsCommand {
    /// List the presets, their status and what the cache holds.
    List,
    /// Download a preset's weights and verify their sha256.
    Pull {
        /// Preset (default: the default preset).
        preset: Option<String>,
    },
    /// Delete a preset's weights.
    Rm {
        /// Preset.
        preset: String,
    },
    /// Print where a preset's weights file lives.
    Path {
        /// Preset.
        preset: String,
    },
}

/// Runs `rueca models`.
pub fn run(store: &Path, cmd: &ModelsCommand, json_out: bool, pretty: bool) -> anyhow::Result<()> {
    let cache = ModelCache::for_store(store);
    match cmd {
        ModelsCommand::List => list(&cache, json_out, pretty),
        ModelsCommand::Pull { preset } => pull(&cache, preset.as_deref().unwrap_or(rueca_embed::DEFAULT_PRESET)),
        ModelsCommand::Rm { preset } => {
            let p = rueca_embed::preset(preset)?;
            let removed = cache.remove(p)?;
            println!("{} {}", if removed { "removed" } else { "nothing to remove for" }, cache.preset_dir(p).display());
            Ok(())
        }
        ModelsCommand::Path { preset } => {
            println!("{}", cache.weights_path(rueca_embed::preset(preset)?)?.display());
            Ok(())
        }
    }
}

fn state_str(s: CacheState) -> &'static str {
    match s {
        CacheState::Missing => "-",
        CacheState::Unverified => "unverified",
        CacheState::Verified => "verified",
        CacheState::NotApplicable => "",
    }
}

fn row(p: &Preset, state: CacheState) -> Value {
    json!({
        "preset": p.name,
        "status": p.status.as_str(),
        "cache": state,
        "model": p.model,
        "quantization": p.quantization,
        "dims": p.dims,
        "bytes": p.weights.map(|w| w.bytes),
        "license": p.license,
        "note": p.note,
    })
}

fn list(cache: &ModelCache, json_out: bool, pretty: bool) -> anyhow::Result<()> {
    let presets = cache.list();
    if json_out {
        let rows: Vec<Value> = presets.iter().map(|(p, s)| row(p, *s)).collect();
        println!("{}", crate::render(&json!({ "models_dir": cache.root(), "presets": rows }), pretty));
        return Ok(());
    }
    println!("{:<38} {:<12} {:<10} {:<7} {:>5} {:>9}  NOTE", "PRESET", "STATUS", "CACHE", "QUANT", "DIMS", "SIZE");
    for (p, s) in &presets {
        let size = p.weights.map(|w| crate::human_bytes(w.bytes)).unwrap_or_default();
        println!(
            "{:<38} {:<12} {:<10} {:<7} {:>5} {:>9}  {}",
            p.name,
            p.status.as_str(),
            state_str(*s),
            p.quantization,
            p.dims,
            size,
            p.note
        );
    }
    println!("models directory: {}", cache.root().display());
    Ok(())
}

fn pull(cache: &ModelCache, name: &str) -> anyhow::Result<()> {
    let p = rueca_embed::preset(name)?;
    let w = p.pullable()?;
    eprintln!("pulling {} ({}) from {}", p.name, crate::human_bytes(w.bytes), w.url);
    let tty = std::io::stderr().is_terminal();
    let mut last = 0_u64;
    let mut progress = |done: u64, total: u64| {
        let pct = done * 100 / total.max(1);
        if tty && pct != last {
            eprint!("\r{pct:>3}%");
            last = pct;
        }
    };
    let path = cache.pull(p, &mut progress)?;
    if tty {
        eprintln!();
    }
    println!("{} sha256 {} verified: {}", p.name, w.sha256, path.display());
    Ok(())
}
