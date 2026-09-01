//! Calendar page (slot 4): today's and tomorrow's events, agenda-style --
//! the "Default" layout from TRMNL's Google Calendar recipe, not the 7-day
//! grid (see `plugins::calendar_week` for that).

use anyhow::Result;
use chrono::Local;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;

use crate::calendar::{self, Event};
use crate::plugin::{self, Plugin};
use crate::render::{self, truncate_to_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};

pub const SLOT: u8 = 4;
const NAME: &str = "Egham Calendar";
const STATUS_LABEL: &str = "GOOGLE CALENDAR";
const DAYS_AHEAD: i64 = 2;

pub struct CalendarDefaultPlugin;

impl Plugin for CalendarDefaultPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let events = calendar::fetch_events(DAYS_AHEAD)?;
            let fingerprint = calendar::fingerprint(&events);
            let img = render_page(fonts, &events);
            Ok((fingerprint, img))
        })
    }

    /// The one-time Google OAuth consent flow -- see `calendar::run_oauth_flow`.
    /// Both calendar plugins share the same saved refresh token
    /// (`calendar_token.txt`), so either one running setup is equivalent;
    /// this exists on both simply so `--setup` finds it under whichever
    /// plugin the caller thinks of as "the" calendar one.
    fn setup(&mut self) -> Result<()> {
        calendar::run_oauth_flow()
    }
}

fn render_page(fonts: &Fonts, events: &[Event]) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "GOOGLE CALENDAR", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 40.0, 26.0, 76.0, "AGENDA", BLACK);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    let today = Local::now().format("%Y-%m-%d").to_string();
    let tomorrow = (Local::now() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();

    let today_events: Vec<&Event> = events.iter().filter(|e| e.start.ymd() == today).collect();
    let tomorrow_events: Vec<&Event> = events.iter().filter(|e| e.start.ymd() == tomorrow).collect();

    let y = 140.0;
    let y = draw_day_group(&mut img, fonts, "TODAY", &today_events, y);
    draw_day_group(&mut img, fonts, "TOMORROW", &tomorrow_events, y);

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

/// Vertical distance from a group's last row baseline to the next group's
/// header baseline -- bigger than ROW_H so groups read as visually distinct
/// from the rows within them, not just one more row.
const GROUP_GAP: f32 = 50.0;

/// Draws one day's group header plus its event rows, returning the y
/// position the *next* group's header should start at (so callers can stack
/// groups without hardcoding each group's height).
fn draw_day_group(img: &mut GrayImage, fonts: &Fonts, label: &str, events: &[&Event], y0: f32) -> f32 {
    render::draw_text(img, &fonts.sans_black, 20.0, 26.0, y0, label, BLACK);
    draw_line_segment_mut(img, (26.0, y0 + 8.0), (W as f32 - 26.0, y0 + 8.0), Luma([LIGHT_GRAY]));

    if events.is_empty() {
        render::draw_text(img, &fonts.sans_bold, 15.0, 26.0, y0 + 34.0, "No events", LIGHT_GRAY);
        return y0 + 34.0 + GROUP_GAP;
    }

    let row_h = 34.0;
    let mut y = y0 + 30.0;
    for event in events {
        let time_text = event.start.hhmm().unwrap_or("ALL DAY");
        render::draw_text(img, &fonts.mono, 15.0, 26.0, y, time_text, DARK_GRAY);

        let title = if event.summary.is_empty() { "(untitled)" } else { &event.summary };
        let title_x = 130.0;
        let title = truncate_to_width(&fonts.sans_bold, 16.0, title, W as f32 - 26.0 - title_x);
        render::draw_text(img, &fonts.sans_bold, 16.0, title_x, y, &title, BLACK);

        y += row_h;
    }
    y + GROUP_GAP - row_h
}
