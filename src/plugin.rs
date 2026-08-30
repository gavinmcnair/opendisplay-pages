//! The plugin framework (layer 3): every page -- trains, the index, and any
//! future page (weather, ...) -- implements `Plugin` and gets the same
//! bottom status bar for free via `draw_status_bar`, so a viewer can always
//! tell which slot they're looking at, what this page calls itself, and
//! when it last updated, without every page re-implementing that chrome.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;
use std::time::Duration;

use crate::render::{self, Fonts, BLACK, DARK_GRAY, W};

/// Default `Plugin::poll_interval` -- matches `main.rs`'s external-API rate
/// budgeting comment (RTT/Open-Meteo/Google Calendar all stay comfortably
/// under their limits at this cadence). A plugin that needs to look fresher
/// without calling out more often (see `plugins::trains`) overrides this to
/// something faster and does its own internal caching to keep the real
/// network-call rate unchanged.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(300);

pub trait Plugin {
    /// Slot this plugin's content is pushed to.
    fn slot(&self) -> u8;

    /// Human name shown on the index page -- deliberately separate from the
    /// slot number (`slot()`), so renaming or renumbering one never means
    /// editing the other.
    fn name(&self) -> &'static str;

    /// Fetches fresh data and renders the full page, content plus the shared
    /// status bar (via `draw_status_bar`, called as this method's last
    /// step). Returns a fingerprint stable across calls whose *meaningful*
    /// content hasn't changed, so the caller can tell a real change from a
    /// mere re-render.
    ///
    /// Both `self` and `fonts` share the same lifetime `'a` deliberately:
    /// the returned future borrows from both (most plugins read their own
    /// fields inside it), and tying them together is what lets the borrow
    /// checker accept "however long the shorter of the two actually is" at
    /// each call site instead of demanding one bound the other.
    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>>;

    /// If true, the orchestrator force-switches the panel to this plugin's
    /// slot (via `ble::switch_to_slot`, CMD_SLOT_SWITCH) every time its
    /// content actually changes -- an event-driven override on top of the
    /// scheduler's time-based rules, for a page that needs to interrupt
    /// whatever's currently showing (e.g. an alert). Not needed by any
    /// current plugin (trains/weather/index all leave this false, letting
    /// `scheduler::Scheduler` decide), but the mechanism exists precisely so
    /// a future one can flip it on without any other wiring.
    fn autoswitch_on_change(&self) -> bool {
        false
    }

    /// How often the orchestrator calls `render()` at all. This is a ceiling
    /// on how often a plugin's *own* network fetch runs only for a plugin
    /// that fetches fresh data every call (the default, and true of
    /// weather/calendar/index) -- it is NOT the same knob as "how often does
    /// this plugin hit its API": a plugin can override this to something
    /// fast and still cache its own fetch internally, calling out only every
    /// Nth invocation, to re-render and re-fingerprint more often than it
    /// re-fetches (see `plugins::trains`, which re-derives "has this train
    /// already left" from wall-clock time on every call without a new RTT
    /// request).
    fn poll_interval(&self) -> Duration {
        DEFAULT_POLL_INTERVAL
    }

    /// One-time interactive configuration a plugin needs before it can
    /// `render()` at all (e.g. Google Calendar's OAuth consent flow) --
    /// no-op by default. Exists so `main.rs` can offer one generic `--setup
    /// <slot-or-name>` flag instead of a bespoke CLI flag per plugin that
    /// happens to need this (the orchestrator has no business knowing which
    /// plugins need setup or what that setup involves; that's entirely the
    /// plugin's own concern, same as it owning its own env vars for
    /// credentials rather than the orchestrator plumbing them through).
    fn setup(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Draws the shared bottom status bar every page uses, then performs the
/// final 4-gray quantization -- call this last, after a page's own content
/// is drawn at full antialiased blending precision (see render.rs's module
/// doc for why that ordering matters).
///
/// Layout: three black rules, then `UPDATED <updated_at>` bottom-left,
/// `status_label` bottom-middle (centered), `SLOT <n>` bottom-right.
pub fn draw_status_bar(img: &mut GrayImage, fonts: &Fonts, slot: u8, updated_at: &str, status_label: &str) {
    for dy in 0..3 {
        draw_line_segment_mut(img, (0.0, 448.0 + dy as f32), (W as f32, 448.0 + dy as f32), Luma([BLACK]));
    }

    let updated = format!("UPDATED {updated_at}");
    render::draw_text(img, &fonts.mono, 13.0, 26.0, 460.0 + 13.0, &updated, DARK_GRAY);

    if !status_label.is_empty() {
        let label_w = render::text_width(&fonts.arial_bold, 13.0, status_label);
        render::draw_text(img, &fonts.arial_bold, 13.0, (W as f32 - label_w) / 2.0, 460.0 + 13.0, status_label, DARK_GRAY);
    }

    let slot_text = format!("SLOT {slot}");
    let sw = render::text_width(&fonts.arial_bold, 13.0, &slot_text);
    render::draw_text(img, &fonts.arial_bold, 13.0, W as f32 - 26.0 - sw, 460.0 + 13.0, &slot_text, DARK_GRAY);

    render::quantize_to_4gray(img);
}
