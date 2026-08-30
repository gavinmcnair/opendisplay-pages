//! Tiny persisted "what did we last push" marker, so the poll loop can tell
//! whether the current frame differs from whatever the device is already
//! showing. Deliberately dumb (a single u64 in a text file) -- swap for
//! something richer if a future data source needs more than one number.

use anyhow::{Context, Result};
use std::path::Path;

pub fn load(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn save(path: &Path, fingerprint: u64) -> Result<()> {
    std::fs::write(path, fingerprint.to_string())
        .with_context(|| format!("writing state file {}", path.display()))
}
