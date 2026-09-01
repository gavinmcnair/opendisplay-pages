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

const LAST_PUSH_FILE: &str = "last_push.txt";

/// Records "a push to ANY slot just succeeded" (unix seconds) -- called by
/// the orchestrator on every successful content/index upload, read back by
/// `plugins::stats` for its LAST PUSH line. A file, not process memory, so
/// the fact survives restarts. Best-effort: failing to note the timestamp
/// must never fail the push that just succeeded.
pub fn record_push() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(LAST_PUSH_FILE, now.to_string());
}

pub fn load_last_push() -> Option<i64> {
    std::fs::read_to_string(LAST_PUSH_FILE).ok()?.trim().parse().ok()
}
