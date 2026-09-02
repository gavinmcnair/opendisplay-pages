//! Battery fundamentals (layer 1): the persisted reading history's location,
//! a cheap "latest reading" accessor, and the voltage -> state-of-charge
//! conversion. Lives here rather than in `plugins::stats` because the shared
//! status bar (`plugin::draw_status_bar`, layer 2) also needs the current
//! charge for its battery icon, and the framework must not depend on a
//! concrete plugin.

use battery_estimator::{BatteryChemistry, SocEstimator};

/// One `<unix_ts> <mv> <temp_c>` line per sample -- written by
/// `plugins::stats` (which owns sampling and pruning), read by anyone.
pub const HISTORY_FILE: &str = "battery_history.txt";

/// Voltage -> state-of-charge via the Li-Ion curve, matching the device
/// config's own estimator choice (capacity_estimator 5 =
/// OD_CAPACITY_EST_SEEED_LI_ION) -- the panel and this program should never
/// disagree about what "50%" means.
pub fn soc_percent(mv: u16) -> Option<f32> {
    SocEstimator::new(BatteryChemistry::LiIon).estimate_soc(mv as f32 / 1000.0).ok()
}

/// Millivolts from the newest stored sample -- `None` when no history
/// exists yet. Reads the file fresh each call; it's one small file, and
/// callers draw at most once per e-ink push.
pub fn latest_mv() -> Option<u16> {
    let text = std::fs::read_to_string(HISTORY_FILE).ok()?;
    text.lines().rev().find_map(|l| l.split_whitespace().nth(1)?.parse().ok())
}

/// Latest state of charge, 0-100 -- the one-call form the status bar wants.
pub fn latest_soc_percent() -> Option<f32> {
    soc_percent(latest_mv()?)
}
