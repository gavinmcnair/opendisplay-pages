//! Bin-collection status (fundamentals): which bin type is out next.
//! Sourced from Google Calendar all-day events -- "Recycling day at Egham"
//! and "Rubbish day at Egham", alternating fortnightly on the Egham/
//! Runnymede schedule -- persisted by the calendar plugin (`bin_schedule.txt`)
//! and read by the shared status-bar chrome. Kept out of `plugins::calendar`
//! so the framework's chrome (layer 2) doesn't depend on a concrete plugin,
//! same pattern as `battery.rs`.

use chrono::{Local, NaiveDate};

const SCHEDULE_FILE: &str = "bin_schedule.txt";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BinType {
    Recycle,
    Waste,
}

impl BinType {
    pub fn label(self) -> &'static str {
        match self {
            BinType::Recycle => "RECYCLE",
            BinType::Waste => "WASTE",
        }
    }
}

/// Classify a calendar event title, or `None` if it isn't a bin event.
/// "Rubbish" is the local name for the black/general-waste bin -- it does
/// NOT contain "waste" or "bin", so it needs its own keyword (a real miss:
/// a naive "bin"/"waste" search found only the recycling weeks).
pub fn classify(summary: &str) -> Option<BinType> {
    let s = summary.to_lowercase();
    if s.contains("recycl") {
        Some(BinType::Recycle)
    } else if s.contains("rubbish") || s.contains("waste") || s.contains("refuse") || s.contains("general") {
        Some(BinType::Waste)
    } else {
        None
    }
}

/// Persist the upcoming bin events (one `YYYY-MM-DD RECYCLE|WASTE` line
/// each) -- written by the calendar plugin from its fetch. Best-effort.
pub fn record_schedule(events: &[(NaiveDate, BinType)]) {
    let text: String = events.iter().map(|(d, t)| format!("{d} {}\n", t.label())).collect();
    let _ = std::fs::write(SCHEDULE_FILE, text);
}

/// The bin type due next, or `None` if unknown. Collection is Tuesday and
/// the calendar events are dated the Monday before, so an event stays "the
/// next one" through its Tuesday and the indicator flips on Wednesday --
/// i.e. keep an event while `today <= event_date + 1`. Evaluated live from
/// the persisted schedule, so the flip is exact even if the file hasn't
/// been refetched recently (the fortnight is already on disk).
pub fn next_type() -> Option<BinType> {
    let text = std::fs::read_to_string(SCHEDULE_FILE).ok()?;
    let today = Local::now().date_naive();
    text.lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            let date: NaiveDate = p.next()?.parse().ok()?;
            let ty = match p.next()? {
                "RECYCLE" => BinType::Recycle,
                "WASTE" => BinType::Waste,
                _ => return None,
            };
            Some((date, ty))
        })
        .filter(|(date, _)| today <= *date + chrono::Duration::days(1))
        .min_by_key(|(date, _)| *date)
        .map(|(_, t)| t)
}
