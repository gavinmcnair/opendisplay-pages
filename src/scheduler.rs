//! Server-side page scheduler. Decides which slot *should* be on the panel
//! right now, purely from wall-clock time; `main.rs`'s orchestrator compares
//! that against what it last forced and, on a mismatch, sends
//! `ble::switch_to_slot` (CMD_SLOT_SWITCH) -- the BLE equivalent of a button
//! press. This module is only the policy; forcing the switch is the caller's
//! job, and showing what the policy currently is lives in `plugins::index`.
//!
//! Wall-clock time comes from `chrono::Local`, which reads the OS's
//! configured timezone (including DST) for free -- correct as long as this
//! process runs on a machine actually set to the right timezone (true today:
//! it runs on the user's own Mac), and avoids pulling in a full IANA
//! timezone database dependency for a single-machine deployment.

use chrono::{DateTime, Datelike, Local, Timelike};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    pub const fn new(hour: u8, minute: u8) -> Self {
        Self { hour, minute }
    }

    fn minutes_since_midnight(self) -> u32 {
        self.hour as u32 * 60 + self.minute as u32
    }
}

impl std::fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// `[Mon, Tue, Wed, Thu, Fri, Sat, Sun]`, matching
/// `chrono::Weekday::num_days_from_monday()`.
pub type Days = [bool; 7];

pub const MON_FRI: Days = [true, true, true, true, true, false, false];
pub const ALL_DAYS: Days = [true; 7];

const DAY_ABBREV: [&str; 7] = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];

/// "MON-FRI" / "DAILY" for the two common cases, else a comma list -- good
/// enough for the index page without a general range-compression algorithm
/// nobody needs yet.
pub fn describe_days(days: Days) -> String {
    if days == ALL_DAYS {
        return "DAILY".to_string();
    }
    if days == MON_FRI {
        return "MON-FRI".to_string();
    }
    let picked: Vec<&str> = DAY_ABBREV.iter().zip(days.iter()).filter(|(_, &on)| on).map(|(&d, _)| d).collect();
    if picked.is_empty() {
        "NEVER".to_string()
    } else {
        picked.join(",")
    }
}

#[derive(Clone)]
pub struct ScheduleRule {
    pub days: Days,
    pub start: TimeOfDay,
    pub end: TimeOfDay, // exclusive
    pub slot: u8,
    pub label: &'static str,
}

impl ScheduleRule {
    fn matches(&self, now: DateTime<Local>) -> bool {
        let day_idx = now.weekday().num_days_from_monday() as usize;
        if !self.days[day_idx] {
            return false;
        }
        let mins = now.hour() * 60 + now.minute();
        mins >= self.start.minutes_since_midnight() && mins < self.end.minutes_since_midnight()
    }
}

#[derive(Clone)]
pub struct Scheduler {
    /// Checked in order; the first matching rule wins. Keep rules
    /// non-overlapping in practice -- this doesn't detect or warn about
    /// overlap, it just takes the first match.
    pub rules: Vec<ScheduleRule>,
    /// Where the panel returns when a rule's window ENDS -- forced exactly
    /// once at that transition, never continuously. Outside those two
    /// transition moments (window entry -> rule slot, window exit -> this)
    /// the schedule makes no claim on the panel at all: the buttons own it,
    /// and a process restart outside a window forces nothing. An earlier
    /// design treated this as "the slot that should be showing whenever no
    /// rule matches" and re-forced it on every restart -- which yanked the
    /// display away from wherever the user had browsed to, for no one's
    /// benefit.
    pub default_slot: u8,
    pub default_label: &'static str,
}

impl Scheduler {
    /// The slot an active RULE says should be on screen right now, with its
    /// label -- `None` when no rule's window is active. The caller detects
    /// transitions (`None`->`Some` = window began, `Some`->`None` = window
    /// ended, switch to `default_slot`) rather than this answering "what
    /// should be on screen" unconditionally -- see `default_slot`'s doc.
    pub fn active(&self, now: DateTime<Local>) -> Option<(u8, &'static str)> {
        self.rules.iter().find(|rule| rule.matches(now)).map(|rule| (rule.slot, rule.label))
    }

    pub fn active_now(&self) -> Option<(u8, &'static str)> {
        self.active(Local::now())
    }
}

/// This deployment's schedule: trains during the Mon-Fri morning commute
/// window, returning to weather when that window ends; no other claim on
/// the panel.
pub fn default_schedule() -> Scheduler {
    Scheduler {
        rules: vec![ScheduleRule {
            days: MON_FRI,
            start: TimeOfDay::new(7, 0),
            end: TimeOfDay::new(8, 30),
            slot: crate::plugins::trains::SLOT,
            label: "TRAINS",
        }],
        default_slot: crate::plugins::weather::SLOT,
        default_label: "WEATHER",
    }
}
