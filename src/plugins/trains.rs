//! Trains page (slot 1): live Egham departures by physical platform, not by
//! ultimate destination. Egham has exactly two platforms -- Platform 1
//! (east, towards London Waterloo) and Platform 2 (west, towards Reading,
//! Ascot, Chertsey, Woking and everything beyond) -- so a westbound train's
//! specific destination (Reading one service, Woking via Chertsey the next)
//! doesn't change which platform it leaves from. Earlier versions of this
//! page split westbound trains into separate per-destination columns; that
//! was wrong (a passenger stands on a platform, not in front of a
//! destination). The bottom status bar comes from `plugin::draw_status_bar`,
//! shared with every other page.
//!
//! Fetches from Gavin's own self-hosted traintimes service (`traintimes.rs`),
//! not RTT directly -- see that module's doc comment. Server-side
//! `catchable_only` filtering already trims departed/uncatchable calls, so
//! unlike the old RTT client this plugin needs no wall-clock "has it left"
//! logic of its own: it just re-fetches every `poll_interval` tick and
//! displays what comes back.
//!
//! Two separate `to`/`via`-filtered fetches (`EAST_TO`/`EAST_VIA`/`WEST_TO`
//! below), one per platform, not one unfiltered fetch split locally by the
//! `platform` field (2026-09-01, prior design) -- that field can be
//! unconfirmed and flip a train between columns for no real reason, which
//! flips `fingerprint_filtered` and triggers a spurious repush even though
//! nothing a rider cares about changed. Server-side `to`/`via` filtering
//! makes each train's column a stable, authoritative fact about its route
//! instead.

use anyhow::Result;
use chrono::{DateTime, Local};
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_ellipse_mut, draw_filled_rect_mut, draw_line_segment_mut, draw_polygon_mut};
use imageproc::point::Point;
use imageproc::rect::Rect;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, truncate_to_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};
use crate::traintimes::{self, StationCall};

pub const SLOT: u8 = 1;
const NAME: &str = "Egham Train Times";
const STATUS_LABEL: &str = "TRAINTIMES LIVE";

/// Platform 1 (east): specifically Waterloo-via-Richmond, not just any
/// Waterloo-bound working -- `via` stacks as an independent AND alongside
/// `to` (see `traintimes::fetch_departures`).
const EAST_TO: &str = "WAT";
const EAST_VIA: &str = "RMD";
/// Platform 2 (west): everything heading in the Chertsey direction --
/// Chertsey sits on the line west of Egham before it forks further (Reading,
/// Ascot, Woking, Weybridge, ...), so `to=CHY` alone catches all of it
/// without needing a `via` on top.
const WEST_TO: &str = "CHY";

pub struct TrainsPlugin;

impl TrainsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for TrainsPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    /// It's Gavin's own service (see `traintimes.rs`), not license-limited
    /// like RTT was -- polled every tick here, with the eink push itself
    /// still gated on the fingerprint actually changing (see
    /// `fingerprint_filtered`), so a quiet board costs a cheap HTTP fetch
    /// every 10s, not a BLE transfer.
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(10)
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let east_calls = traintimes::fetch_departures(EAST_TO, Some(EAST_VIA))?;
            let west_calls = traintimes::fetch_departures(WEST_TO, None)?;

            let now = Local::now();

            let east: Vec<&StationCall> = east_calls.iter().collect();
            let west: Vec<&StationCall> = west_calls.iter().collect();

            let fingerprint = fingerprint_filtered(&east, &west);
            let img = render_page(fonts, now, &east, &west);
            Ok((fingerprint, img))
        })
    }
}

/// Fingerprints the *filtered, currently-displayed* view (not the raw
/// fetch) -- reuses `traintimes::hash_call`'s per-call logic so a train
/// dropping off the (server-side, catchable-only) list changes this
/// fingerprint and triggers a repush, exactly like a real schedule/status
/// change would.
fn fingerprint_filtered(east: &[&StationCall], west: &[&StationCall]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for c in east {
        traintimes::hash_call(c, &mut hasher);
    }
    0xFFFF_FFFFu64.hash(&mut hasher); // separator between platforms
    for c in west {
        traintimes::hash_call(c, &mut hasher);
    }
    hasher.finish()
}

/// Every distinct destination actually present in `services`, in first-seen
/// order, joined for display -- e.g. "READING / WOKING" today, "ASCOT" some
/// other time, whatever the real fetch contains. Never a fixed list: a
/// westbound service's terminus varies by working, not something safe to
/// enumerate once in source (see this function's call site).
fn distinct_destinations(services: &[&StationCall]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for svc in services {
        if let Some(dest) = svc.destination_name.as_deref() {
            if !seen.contains(&dest) {
                seen.push(dest);
            }
        }
    }
    if seen.is_empty() {
        String::new()
    } else {
        seen.join(" / ").to_uppercase()
    }
}

fn render_page(fonts: &Fonts, now: DateTime<Local>, east: &[&StationCall], west: &[&StationCall]) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    // Real wall-clock time, not the fetch's own timestamp -- `render()` is
    // called every poll_interval() tick, and there's no per-fetch cache here
    // to go stale between ticks (unlike the old RTT client), but this stays
    // its own read of the clock regardless. This is also, in effect, the
    // page's own "last updated" time: the panel is e-ink and only repaints
    // when `fingerprint_filtered` actually changes, so whatever's rendered
    // here is exactly what it was at the last real push -- labeled
    // explicitly below (see `LABEL`) rather than left looking like a live
    // clock it isn't. No date alongside it: a page that only repaints on
    // change is always showing "today" by construction.
    let clock_text = now.format("%H:%M").to_string();

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "SOUTH WESTERN RAILWAY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 46.0, 26.0, 36.0 + 46.0, "EGHAM", BLACK);

    // Fills the header's right side top-to-bottom, PAD_Y from the y=100
    // separator at both top and bottom -- CLOCK_SIZE/BASELINE_Y are
    // calibrated (not derived from font metrics fontdue exposes cleanly) to
    // hit that padding for this specific font+string; re-check with
    // `--render 1` if either constant changes.
    const CLOCK_SIZE: f32 = 114.0;
    // 100.0 - 4px target padding, minus a further 3px: this font's digits
    // render with a few pixels of overshoot below the nominal baseline at
    // this size (measured, not derived from metrics fontdue exposes
    // cleanly) -- re-measure with `--render 1` if CLOCK_SIZE changes.
    const BASELINE_Y: f32 = 93.0;
    const LABEL_SIZE: f32 = 17.0;
    const LABEL: &str = "LAST UPDATE ";
    let cw = text_width(&fonts.mono, CLOCK_SIZE, &clock_text);
    let lw = text_width(&fonts.arial_bold, LABEL_SIZE, LABEL);
    let clock_x = W as f32 - 26.0 - cw;
    render::draw_text(&mut img, &fonts.mono, CLOCK_SIZE, clock_x, BASELINE_Y, &clock_text, BLACK);
    render::draw_text(&mut img, &fonts.arial_bold, LABEL_SIZE, clock_x - lw, BASELINE_Y, LABEL, DARK_GRAY);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    draw_line_segment_mut(&mut img, (400.0, 108.0), (400.0, 440.0), Luma([LIGHT_GRAY]));

    // Subtitle is read off whatever's actually in this fetch, not a
    // hardcoded guess -- which specific station a westbound service
    // terminates at varies train to train (Reading one working, Ascot or
    // Woking via Chertsey the next) and isn't fixed enough to enumerate in
    // source. This is exactly the assumption that made the Woking/Chertsey
    // service invisible in an earlier version of this page.
    let east_subtitle = distinct_destinations(east);
    let west_subtitle = distinct_destinations(west);

    draw_column(
        &mut img,
        fonts,
        &ColumnSpec { x0: 0.0, label: "PLATFORM 1 (EAST)", subtitle: &east_subtitle, arrow_left: true },
        east,
    );
    draw_column(
        &mut img,
        fonts,
        &ColumnSpec { x0: 400.0, label: "PLATFORM 2 (WEST)", subtitle: &west_subtitle, arrow_left: false },
        west,
    );

    plugin::draw_status_bar(&mut img, fonts, SLOT, &clock_text, STATUS_LABEL);
    img
}

enum Mark {
    Dot,
    Up,
    Cross,
}

fn draw_status_mark(img: &mut GrayImage, x: f32, baseline_y: f32, size: f32, kind: &Mark, fill: u8) {
    let r = size / 2.0;
    let cy = baseline_y - r + 1.0;
    let color = Luma([fill]);
    match kind {
        Mark::Dot => {
            draw_filled_ellipse_mut(img, (x as i32 + r as i32, cy as i32), r as i32, r as i32, color);
        }
        Mark::Up => {
            let pts = vec![
                Point::new((x) as i32, (cy + r) as i32),
                Point::new((x + size) as i32, (cy + r) as i32),
                Point::new((x + r) as i32, (cy - r) as i32),
            ];
            draw_polygon_mut(img, &pts, color);
        }
        Mark::Cross => {
            draw_line_segment_mut(img, (x, cy - r), (x + size, cy + r), color);
            draw_line_segment_mut(img, (x, cy + r), (x + size, cy - r), color);
        }
    }
}

fn draw_triangle(img: &mut GrayImage, cx: f32, cy: f32, size: f32, pointing_right: bool, fill: u8) {
    let s = size / 2.0;
    let pts = if pointing_right {
        vec![
            Point::new((cx - s) as i32, (cy - s) as i32),
            Point::new((cx - s) as i32, (cy + s) as i32),
            Point::new((cx + s) as i32, cy as i32),
        ]
    } else {
        vec![
            Point::new((cx + s) as i32, (cy - s) as i32),
            Point::new((cx + s) as i32, (cy + s) as i32),
            Point::new((cx - s) as i32, cy as i32),
        ]
    };
    draw_polygon_mut(img, &pts, Luma([fill]));
}

struct ColumnSpec<'a> {
    x0: f32,
    label: &'a str,
    subtitle: &'a str,
    arrow_left: bool,
}

fn draw_column(img: &mut GrayImage, fonts: &Fonts, spec: &ColumnSpec, services: &[&StationCall]) {
    let col_w = 400.0;
    let pad = 24.0;
    let inner_x0 = spec.x0 + pad;
    let inner_w = col_w - 2.0 * pad;

    let head_y = 126.0;
    let head_cy = head_y + 9.0;
    if spec.arrow_left {
        draw_triangle(img, inner_x0 + 6.0, head_cy, 12.0, true, BLACK);
        render::draw_text(img, &fonts.arial_bold, 19.0, inner_x0 + 18.0, head_y + 15.0, spec.label, BLACK);
    } else {
        let tw = text_width(&fonts.arial_bold, 19.0, spec.label);
        render::draw_text(img, &fonts.arial_bold, 19.0, inner_x0 + inner_w - tw - 18.0, head_y + 15.0, spec.label, BLACK);
        draw_triangle(img, inner_x0 + inner_w - 6.0, head_cy, 12.0, false, BLACK);
    }
    // A destination hint, not an exhaustive list -- each row below still
    // shows that specific service's real destination. Just tells someone
    // glancing at the board which platform to stand on before they've read
    // any individual row.
    let subtitle = truncate_to_width(&fonts.arial_bold, 12.0, spec.subtitle, inner_w);
    render::draw_text(img, &fonts.arial_bold, 12.0, inner_x0, head_y + 30.0, &subtitle, DARK_GRAY);

    // Caller has already filtered by platform; the server (`catchable_only`,
    // see `traintimes.rs`) already trimmed departed/uncatchable calls -- just
    // cap how many rows fit.
    let services: Vec<_> = services.iter().take(4).collect();

    let row_y = head_y + 56.0;
    let row_h = 64.0;
    let time_col_w = 92.0;

    if services.is_empty() {
        render::draw_text(img, &fonts.arial_bold, 15.0, inner_x0, row_y + 10.0, "No upcoming departures", LIGHT_GRAY);
        return;
    }

    let hm = |dt: Option<DateTime<Local>>| dt.map(|d| d.format("%H:%M").to_string()).unwrap_or_else(|| "-".to_string());

    for (i, svc) in services.iter().enumerate() {
        let sched_local = traintimes::scheduled_local(svc);
        // Actual once recorded, else the live estimate, else the schedule --
        // same precedence the API itself uses for `delay_minutes` (see
        // API.md): a late train hasn't left just because its original
        // scheduled time has passed, and a just-departed call (still shown
        // briefly by `catchable_only`) should read its confirmed actual, not
        // a now-stale estimate.
        let live_local = traintimes::actual_local(svc).or_else(|| traintimes::estimated_local(svc)).or(sched_local);
        let cancelled = traintimes::is_cancelled(svc);
        let dest = svc.destination_name.as_deref().unwrap_or("?");
        let origin = svc.origin_name.as_deref().unwrap_or("?");

        let ry = row_y + i as f32 * row_h;
        if i > 0 {
            draw_line_segment_mut(img, (inner_x0, ry), (inner_x0 + inner_w, ry), Luma([LIGHT_GRAY]));
        }

        let mut ty = ry + if i == 0 { 10.0 } else { 15.0 };
        if i == 0 {
            draw_filled_rect_mut(img, Rect::at(inner_x0 as i32, (ry + 5.0) as i32).of_size(40, 15), Luma([BLACK]));
            render::draw_text(img, &fonts.arial_bold, 11.0, inner_x0 + 5.0, ry + 17.0, "NEXT", WHITE);
            ty = ry + 24.0;
        }

        let time_str = hm(sched_local);
        let time_color = if cancelled { DARK_GRAY } else { BLACK };
        let tw = render::draw_text(img, &fonts.mono, 26.0, inner_x0, ty + 22.0, &time_str, time_color);
        if cancelled {
            draw_line_segment_mut(img, (inner_x0, ty + 9.0), (inner_x0 + tw, ty + 9.0), Luma([DARK_GRAY]));
        }

        let meta_x = inner_x0 + time_col_w;
        let meta_w = inner_x0 + inner_w - meta_x;
        render::draw_text(img, &fonts.arial_bold, 19.0, meta_x, ry + 3.0 + 17.0, dest, BLACK);

        let sub_y = ry + 27.0;
        let (mark_kind, status_text, status_color): (Mark, String, u8) = if cancelled {
            (Mark::Cross, "CANCELLED".to_string(), DARK_GRAY)
        } else if live_local != sched_local {
            (Mark::Up, format!("NOW {}", hm(live_local)), BLACK)
        } else {
            (Mark::Dot, "ON TIME".to_string(), LIGHT_GRAY)
        };

        let status_text_w = text_width(&fonts.mono, 13.0, &status_text);
        let mark_w = 12.0;
        let status_total_w = mark_w + 5.0 + status_text_w;
        let via_text = format!("FROM {}", origin.to_uppercase());
        let via_max_w = meta_w - status_total_w - 10.0;
        let via_text = truncate_to_width(&fonts.arial_bold, 13.0, &via_text, via_max_w);
        render::draw_text(img, &fonts.arial_bold, 13.0, meta_x, sub_y + 13.0, &via_text, DARK_GRAY);

        let mark_x = meta_x + meta_w - status_total_w;
        draw_status_mark(img, mark_x, sub_y + 25.0, mark_w, &mark_kind, status_color);
        render::draw_text(img, &fonts.mono, 13.0, mark_x + mark_w + 5.0, sub_y + 13.0, &status_text, status_color);
    }
}
