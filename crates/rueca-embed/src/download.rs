//! `rueca models pull`: download, verify, publish atomically.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::cache::{ModelCache, Progress, hex, write_sidecar};
use crate::error::{Error, Result};
use crate::presets::{Preset, WeightsFile};

impl ModelCache {
    /// Downloads the preset's weights unless they are already verified,
    /// checks size and sha256, and renames the file into place. Not
    /// resumable: an interrupted download starts over.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] for presets that cannot be pulled (planned,
    /// remote, or a Rueca-hosted file not published yet), network and I/O
    /// errors, [`Error::Sha256Mismatch`].
    pub fn pull(&self, preset: &Preset, progress: Progress<'_>) -> Result<PathBuf> {
        let w = preset.pullable()?;
        let path = self.weights_path(preset)?;
        if self.state(preset) == crate::cache::CacheState::Verified {
            return Ok(path);
        }
        let dir = self.preset_dir(preset);
        fs::create_dir_all(&dir).map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        let part = dir.join(format!(".{}.part-{}", w.file_name, std::process::id()));
        let result = fetch(w, &part, progress).and_then(|()| publish(&part, &path, w));
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result.map(|()| path)
    }
}

fn fetch(w: &WeightsFile, part: &Path, progress: Progress<'_>) -> Result<()> {
    let agent = crate::http::agent(None, true);
    let net = |e: ureq::Error| Error::Download { url: w.url.to_string(), message: e.to_string() };
    let response = agent.get(w.url).call().map_err(net)?;
    let mut reader = response.into_body().into_with_config().limit(w.bytes).reader();
    let file = File::create(part).map_err(|e| Error::io(format!("cannot create {}", part.display()), e))?;
    let mut out = BufWriter::new(file);
    let sha = copy_hashing(&mut reader, &mut out, w, progress)?;
    let file = out.into_inner().map_err(|e| Error::io("cannot flush the download", e.into_error()))?;
    file.sync_all().map_err(|e| Error::io("cannot sync the download", e))?;
    if sha != w.sha256 {
        return Err(Error::Sha256Mismatch { path: part.to_path_buf(), expected: w.sha256.to_string(), actual: sha });
    }
    Ok(())
}

/// Copies `reader` to `out`, hashing on the way; checks the byte count.
fn copy_hashing(reader: &mut dyn Read, out: &mut dyn Write, w: &WeightsFile, progress: Progress<'_>) -> Result<String> {
    let url = || w.url.to_string();
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 1 << 20];
    let mut done = 0_u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| Error::Download { url: url(), message: e.to_string() })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| Error::io("cannot write the download", e))?;
        done += n as u64;
        progress(done, w.bytes);
    }
    if done != w.bytes {
        return Err(Error::Download { url: url(), message: format!("got {done} bytes, expected {}", w.bytes) });
    }
    Ok(hex(&hasher.finalize()))
}

fn publish(part: &Path, path: &Path, w: &WeightsFile) -> Result<()> {
    fs::rename(part, path).map_err(|e| Error::io(format!("cannot rename into {}", path.display()), e))?;
    if let Some(dir) = path.parent()
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
    write_sidecar(path, w.sha256)
}
