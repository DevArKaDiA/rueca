//! The weights cache (RFC 0007 §4.4).
//!
//! ```text
//! <models>/<preset>/<file>          the GGUF file, as published
//! <models>/<preset>/<file>.sha256   its verified sha256 (written after the check)
//! <models>/<preset>/.<file>.part-*  a download in progress
//! ```
//!
//! `<models>` is `$RUECA_MODEL_DIR` when set (shared by several stores and
//! by the bench), else `<store>/models`. A file is hashed once, after
//! download or the first time a hand-placed file is used; afterwards its
//! sidecar and size stand for it.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};
use crate::presets::{PRESETS, Preset, PresetKind, WeightsFile};

/// Environment variable that moves the cache out of the store.
pub const MODEL_DIR_ENV: &str = "RUECA_MODEL_DIR";

/// Progress callback of a download: bytes written so far, total bytes.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// State of one preset in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// No file.
    Missing,
    /// The file is there but not verified yet (no sidecar, or a size
    /// mismatch that the next use reports).
    Unverified,
    /// The file matches the manifest.
    Verified,
    /// The preset has no local weights.
    NotApplicable,
}

/// A weights cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct ModelCache {
    root: PathBuf,
}

impl ModelCache {
    /// A cache rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The cache of a store: `$RUECA_MODEL_DIR`, else `<store>/models`.
    #[must_use]
    pub fn for_store(store: &Path) -> Self {
        match std::env::var_os(MODEL_DIR_ENV) {
            Some(dir) if !dir.is_empty() => Self::new(dir),
            _ => Self::new(store.join("models")),
        }
    }

    /// The cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<models>/<preset>/`. Preset names with `:` or `/` (remote) never
    /// get a directory.
    #[must_use]
    pub fn preset_dir(&self, preset: &Preset) -> PathBuf {
        self.root.join(preset.name)
    }

    /// Where the preset's weights file lives (or would live).
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the preset has no weights file.
    pub fn weights_path(&self, preset: &Preset) -> Result<PathBuf> {
        Ok(self.preset_dir(preset).join(preset.weights()?.file_name))
    }

    /// The preset's state in the cache.
    #[must_use]
    pub fn state(&self, preset: &Preset) -> CacheState {
        let (Some(w), PresetKind::Local) = (preset.weights.as_ref(), preset.kind) else {
            return CacheState::NotApplicable;
        };
        let path = self.preset_dir(preset).join(w.file_name);
        match fs::metadata(&path) {
            Err(_) => CacheState::Missing,
            Ok(m) if m.len() == w.bytes && sidecar_matches(&path, w.sha256) => CacheState::Verified,
            Ok(_) => CacheState::Unverified,
        }
    }

    /// Every preset with its state, in manifest order.
    #[must_use]
    pub fn list(&self) -> Vec<(&'static Preset, CacheState)> {
        PRESETS.iter().map(|p| (p, self.state(p))).collect()
    }

    /// The verified weights path, hashing a hand-placed file once.
    ///
    /// # Errors
    ///
    /// [`Error::WeightsMissing`] when the file is absent (the message names
    /// `rueca models pull`), [`Error::Sha256Mismatch`] when it differs from
    /// the manifest, [`Error::Unavailable`] for presets without weights.
    pub fn resolve(&self, preset: &Preset) -> Result<PathBuf> {
        let w = preset.weights()?;
        let path = self.weights_path(preset)?;
        match self.state(preset) {
            CacheState::Verified => Ok(path),
            CacheState::Missing => {
                Err(Error::WeightsMissing { preset: preset.name.to_string(), path, url: w.url.to_string() })
            }
            CacheState::Unverified | CacheState::NotApplicable => {
                verify_file(&path, w)?;
                write_sidecar(&path, w.sha256)?;
                Ok(path)
            }
        }
    }

    /// Deletes the preset's directory. Returns whether anything was removed.
    ///
    /// # Errors
    ///
    /// I/O errors other than "not found".
    pub fn remove(&self, preset: &Preset) -> Result<bool> {
        let dir = self.preset_dir(preset);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(format!("cannot remove {}", dir.display()), e)),
        }
    }
}

#[cfg(not(feature = "embeddings"))]
impl ModelCache {
    /// Downloading needs the `embeddings` feature.
    ///
    /// # Errors
    ///
    /// Always [`Error::NoRuntime`].
    pub fn pull(&self, _preset: &Preset, _progress: Progress<'_>) -> Result<PathBuf> {
        Err(Error::NoRuntime)
    }
}

/// Hex sha256 of a file.
///
/// # Errors
///
/// When the file cannot be read.
pub fn sha256_file(path: &Path) -> Result<String> {
    let ctx = || format!("cannot read {}", path.display());
    let mut f = File::open(path).map_err(|e| Error::io(ctx(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(ctx(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Lower-case hex of bytes.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Checks size and sha256 of a file against the manifest.
fn verify_file(path: &Path, w: &WeightsFile) -> Result<()> {
    let actual = sha256_file(path)?;
    if actual == w.sha256 {
        Ok(())
    } else {
        Err(Error::Sha256Mismatch { path: path.to_path_buf(), expected: w.sha256.to_string(), actual })
    }
}

fn sidecar_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".sha256");
    PathBuf::from(s)
}

fn sidecar_matches(path: &Path, sha256: &str) -> bool {
    fs::read_to_string(sidecar_path(path)).is_ok_and(|s| s.trim() == sha256)
}

pub(crate) fn write_sidecar(path: &Path, sha256: &str) -> Result<()> {
    let side = sidecar_path(path);
    let mut f = File::create(&side).map_err(|e| Error::io(format!("cannot write {}", side.display()), e))?;
    writeln!(f, "{sha256}").map_err(|e| Error::io(format!("cannot write {}", side.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::preset;

    #[test]
    fn missing_weights_name_the_pull_command() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        let p = preset("qwen3-0.6b-q8").unwrap();
        assert_eq!(cache.state(p), CacheState::Missing);
        let err = cache.resolve(p).unwrap_err().to_string();
        assert!(err.contains("rueca models pull qwen3-0.6b-q8"), "{err}");
        assert!(err.contains("RUECA_MODEL_DIR"), "{err}");
    }

    #[test]
    fn hand_placed_file_is_hashed_and_rejected_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        let p = preset("qwen3-0.6b-q8").unwrap();
        let path = cache.weights_path(p).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not a model").unwrap();
        assert_eq!(cache.state(p), CacheState::Unverified);
        assert!(matches!(cache.resolve(p), Err(Error::Sha256Mismatch { .. })));
        assert!(cache.remove(p).unwrap());
        assert!(!cache.remove(p).unwrap());
    }

    #[test]
    fn remote_and_planned_presets_have_no_weights() {
        let cache = ModelCache::new("/nonexistent");
        for name in ["coderank-137m", "remote:voyage/voyage-code-4"] {
            let p = preset(name).unwrap();
            assert_eq!(cache.state(p), CacheState::NotApplicable);
            assert!(matches!(cache.resolve(p), Err(Error::Unavailable { .. })));
        }
    }

    #[test]
    fn sha256_of_known_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(sha256_file(&f).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
