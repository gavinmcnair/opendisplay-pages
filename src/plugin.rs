//! The plugin framework (layer 3): every page -- trains, the index, and any
//! future page (weather, ...) -- implements `Plugin` and gets the same
//! bottom status bar for free via `draw_status_bar`, so a viewer can always
//! tell which slot they're looking at, what this page calls itself, and
//! when it last updated, without every page re-implementing that chrome.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_rect_mut, draw_line_segment_mut};
use imageproc::rect::Rect;
use std::time::Duration;

use crate::render::{self, Fonts, BLACK, DARK_GRAY, W, WHITE};

/// Default `Plugin::poll_interval` -- Open-Meteo and Google Calendar both
/// stay comfortably under their rate limits at this cadence. A plugin backed
/// by a source with no limit worth respecting (see `plugins::trains`, which
/// polls Gavin's own self-hosted service) overrides this to something much
/// faster and genuinely fetches every call.
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

    /// How often the orchestrator calls `render()`. For every current plugin
    /// this is also how often its network fetch runs (each one fetches fresh
    /// data every call), but the two aren't inherently the same knob: a
    /// plugin whose API needs protecting could override this to something
    /// fast and cache its own fetch internally, re-rendering and
    /// re-fingerprinting more often than it re-fetches. The orchestrator's
    /// loop honors this properly -- it sleeps until the earliest plugin's
    /// deadline, not a fixed global tick (see `main.rs`).
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
        let label_w = render::text_width(&fonts.sans_bold, 13.0, status_label);
        render::draw_text(img, &fonts.sans_bold, 13.0, (W as f32 - label_w) / 2.0, 460.0 + 13.0, status_label, DARK_GRAY);
    }

    // Rough battery gauge on every page, rightmost in the bar (right of the
    // slot number) -- an outline that drains as the panel's battery does,
    // from the newest stored reading (up to an hour old; the Device Stats
    // page has the precise numbers). Skipped entirely when no reading
    // exists yet. Deliberately just chrome: it refreshes whenever a page
    // repushes for its own reasons and never causes a push.
    let icon_total_w = 24.0; // 22px body + 2px terminal nub
    let soc = crate::battery::latest_soc_percent();
    let icon_x = W as f32 - 26.0 - icon_total_w;

    let slot_text = format!("SLOT {slot}");
    let sw = render::text_width(&fonts.sans_bold, 13.0, &slot_text);
    let slot_x = if soc.is_some() { icon_x - 10.0 - sw } else { W as f32 - 26.0 - sw };
    render::draw_text(img, &fonts.sans_bold, 13.0, slot_x, 460.0 + 13.0, &slot_text, DARK_GRAY);

    if let Some(soc) = soc {
        draw_battery_icon(img, icon_x, 462.0, soc);
    }

    render::quantize_to_4gray(img);
}

/// A 22x10 battery outline with a 2px terminal nub, filled from the left in
/// proportion to `soc` (0-100). A nearly-empty battery still shows a 1px
/// sliver so "critically low" reads differently from "no reading" (which
/// draws nothing at all).
fn draw_battery_icon(img: &mut GrayImage, x: f32, y: f32, soc: f32) {
    let (x, y) = (x as i32, y as i32);
    let (w, h): (u32, u32) = (22, 10);
    draw_filled_rect_mut(img, Rect::at(x, y).of_size(w, h), Luma([BLACK]));
    draw_filled_rect_mut(img, Rect::at(x + 1, y + 1).of_size(w - 2, h - 2), Luma([WHITE]));
    // Terminal nub, vertically centered on the right edge.
    draw_filled_rect_mut(img, Rect::at(x + w as i32, y + 2).of_size(2, h - 4), Luma([BLACK]));

    let inner_w = w - 4; // 2px inset inside the outline
    let fill = ((inner_w as f32 * soc.clamp(0.0, 100.0) / 100.0).round() as u32).max(1);
    draw_filled_rect_mut(img, Rect::at(x + 2, y + 2).of_size(fill, h - 4), Luma([BLACK]));
}
