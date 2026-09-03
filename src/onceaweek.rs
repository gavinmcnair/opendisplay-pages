//! Generic weekly-cycling status indicators for the shared status bar
//! (fundamentals, like `battery.rs` -- kept out of any plugin so the
//! layer-2 chrome doesn't depend on one).
//!
//! Each entry in `onceaweek_schedule.txt` (a JSON array) names an optional
//! icon, a weekday it advances on, and a list of options it rotates through.
//! The bin is the motivating case: `options: ["recycling", "waste"]`
//! advancing every Wednesday (Egham's fortnightly collection is Tuesday, so
//! the indicator flips the day after). `options` of any length cycle in
//! order and wrap (a, b, c -> a, ...).
//!
//! `current` + `since` are an ANCHOR, not live state: "`current` was the
//! active option as of `since`". The live option is computed by advancing
//! that anchor by the number of `day`-weekdays elapsed since `since`. So
//! this is fully deterministic, self-contained (no calendar or any feed),
//! needs no write-back, and never runs out -- unlike a fetched schedule it
//! can't go stale. Real-world disruptions (a bank-holiday week) would need
//! the anchor nudged by hand, same as any fixed fortnightly rule.

use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use serde::Deserialize;

const SCHEDULE_FILE: &str = "onceaweek_schedule.txt";

#[derive(Deserialize)]
struct Entry {
    /// Named glyph drawn before the text; an absent or unknown name falls
    /// back to text only (see `plugin::draw_named_icon`).
    #[serde(default)]
    icon: Option<String>,
    /// Weekday the indicator advances to the next option, e.g. "Wednesday".
    day: String,
    /// Options rotated through, in order; wraps past the end.
    options: Vec<String>,
    /// Anchor option: the one active as of `since`.
    current: String,
    /// Anchor date (ISO `YYYY-MM-DD`): when `current` was the active option.
    since: String,
}

/// A resolved indicator ready to draw: the optional icon name and the
/// current option's label (upper-cased for the status bar).
pub struct Indicator {
    pub icon: Option<String>,
    pub label: String,
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s.trim().to_lowercase().as_str() {
        "monday" | "mon" => Some(Weekday::Mon),
        "tuesday" | "tue" => Some(Weekday::Tue),
        "wednesday" | "wed" => Some(Weekday::Wed),
        "thursday" | "thu" => Some(Weekday::Thu),
        "friday" | "fri" => Some(Weekday::Fri),
        "saturday" | "sat" => Some(Weekday::Sat),
        "sunday" | "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

/// How many times `w` falls in the inclusive range `[start, end]`, O(1).
fn weekdays_in(w: Weekday, start: NaiveDate, end: NaiveDate) -> i64 {
    if end < start {
        return 0;
    }
    let offset = (7 + w.num_days_from_monday() as i64 - start.weekday().num_days_from_monday() as i64) % 7;
    let first = start + Duration::days(offset);
    if first > end {
        0
    } else {
        (end - first).num_days() / 7 + 1
    }
}

fn resolve(entry: &Entry) -> Option<Indicator> {
    let day = parse_weekday(&entry.day)?;
    let since = entry.since.trim().parse::<NaiveDate>().ok()?;
    let start_idx = entry.options.iter().position(|o| o.eq_ignore_ascii_case(entry.current.trim()))?;
    if entry.options.is_empty() {
        return None;
    }
    // Steps = advances on `day` strictly after the anchor date, up to today.
    let steps = weekdays_in(day, since + Duration::days(1), Local::now().date_naive());
    let idx = (start_idx as i64 + steps).rem_euclid(entry.options.len() as i64) as usize;
    Some(Indicator {
        icon: entry.icon.clone(),
        label: entry.options[idx].to_uppercase(),
    })
}

/// Every indicator, resolved to its current option. Empty if the file is
/// missing or unparseable -- the status bar simply shows nothing then.
pub fn indicators() -> Vec<Indicator> {
    let Ok(text) = std::fs::read_to_string(SCHEDULE_FILE) else {
        return Vec::new();
    };
    let entries: Vec<Entry> = serde_json::from_str(&text).unwrap_or_default();
    entries.iter().filter_map(resolve).collect()
}
