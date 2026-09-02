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

    // Phone-style status cluster in the bottom-right corner: signal bars,
    // then battery, rightmost -- each from the newest stored reading (the
    // battery up to an hour old, the RSSI refreshed by every successful
    // connect; the Device Stats page has the precise numbers) and skipped
    // entirely when no reading exists yet. Deliberately just chrome: they
    // refresh whenever a page repushes for its own reasons and never cause
    // a push.
    let mut right = W as f32 - 26.0;
    if let Some(soc) = crate::battery::latest_soc_percent() {
        let icon_total_w = 24.0; // 22px body + 2px terminal nub
        right -= icon_total_w;
        draw_battery_icon(img, right, 462.0, soc);
        right -= 8.0;
    }
    if let Some(dbm) = crate::state::load_rssi() {
        right -= SIGNAL_BARS_W;
        draw_signal_bars(img, right, 472.0, dbm);
        right -= 10.0;
    }

    let slot_text = format!("SLOT {slot}");
    let sw = render::text_width(&fonts.sans_bold, 13.0, &slot_text);
    render::draw_text(img, &fonts.sans_bold, 13.0, right - sw, 460.0 + 13.0, &slot_text, DARK_GRAY);

    render::quantize_to_4gray(img);
}

/// Filled-bar count for an RSSI, 0-4 -- the usual BLE rules of thumb
/// (>= -60 excellent, -70 good, -80 workable, -90 marginal, below that
/// basically out of range). Shared by the status-bar chrome and the Device
/// Stats page (which also hashes this bucket, never the jittery raw dBm).
pub fn rssi_bars(dbm: i16) -> i32 {
    match dbm {
        d if d >= -60 => 4,
        d if d >= -70 => 3,
        d if d >= -80 => 2,
        d if d >= -90 => 1,
        _ => 0,
    }
}

/// Total width of the signal icon drawn by `draw_signal_bars`.
pub const SIGNAL_BARS_W: f32 = 4.0 * 3.0 + 3.0 * 2.0; // 4 bars, 3px wide, 2px gaps

/// Four ascending bars, phone-signal style, topping out at 10px to match
/// the battery icon's height. Unfilled bars still render in light gray so
/// "2 of 4" reads as a fraction, not two floating dashes. `baseline_y` is
/// the bars' bottom edge.
pub fn draw_signal_bars(img: &mut GrayImage, x: f32, baseline_y: f32, dbm: i16) {
    let filled = rssi_bars(dbm);
    for i in 0..4u32 {
        let bar_h = 4 + i * 2; // 4,6,8,10px
        let bx = x + i as f32 * 5.0; // 3px bar + 2px gap
        let color = if (i as i32) < filled { BLACK } else { render::LIGHT_GRAY };
        draw_filled_rect_mut(
            img,
            Rect::at(bx as i32, (baseline_y as i32) - bar_h as i32).of_size(3, bar_h),
            Luma([color]),
        );
    }
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
