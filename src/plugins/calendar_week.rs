//! Calendar page (slot 4): a 7-day grid, one column per day -- TRMNL's
//! "Week" layout. Compact by necessity (7 columns across 800px leaves
//! ~107px each): a day header, then that day's events stacked as
//! time + truncated title, no time-axis grid (see module doc in
//! `plugins::calendar_default` for the "no fancy graphics" reasoning this
//! shares with the weather chart).

use anyhow::Result;
use chrono::{Duration, Local};
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;

use crate::calendar::{self, Event};
use crate::plugin::{self, Plugin};
use crate::render::{self, truncate_to_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};

pub const SLOT: u8 = 4;
const NAME: &str = "Egham Week Ahead";
const STATUS_LABEL: &str = "7-DAY CALENDAR";
const DAYS_AHEAD: i64 = 7;

pub struct CalendarWeekPlugin;

impl Plugin for CalendarWeekPlugin {
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

    /// See `CalendarDefaultPlugin::setup` -- same shared token, present on
    /// both plugins so `--setup` finds it under either.
    fn setup(&mut self) -> Result<()> {
        calendar::run_oauth_flow()
    }
}

fn render_page(fonts: &Fonts, events: &[Event]) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "GOOGLE CALENDAR", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 40.0, 26.0, 76.0, "WEEK", BLACK);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    let grid_x0 = 26.0;
    let grid_x1 = W as f32 - 26.0;
    let col_w = (grid_x1 - grid_x0) / 7.0;
    let header_y = 130.0;
    let rows_y0 = 150.0;
    let row_h = 40.0;

    for day in 0..7 {
        let date = Local::now() + Duration::days(day);
        let ymd = date.format("%Y-%m-%d").to_string();
        let col_x = grid_x0 + day as f32 * col_w;

        let day_label = format!("{} {}", date.format("%a").to_string().to_uppercase(), date.format("%-d"));
        render::draw_text(&mut img, &fonts.sans_bold, 14.0, col_x + 4.0, header_y, &day_label, BLACK);
        draw_line_segment_mut(&mut img, (col_x, header_y + 8.0), (col_x + col_w - 8.0, header_y + 8.0), Luma([LIGHT_GRAY]));
        if day > 0 {
            draw_line_segment_mut(&mut img, (col_x, header_y - 14.0), (col_x, H as f32 - 40.0), Luma([LIGHT_GRAY]));
        }

        let day_events: Vec<&Event> = events.iter().filter(|e| e.start.ymd() == ymd).collect();
        let max_rows = ((H as f32 - 40.0 - rows_y0) / row_h) as usize;
        let mut y = rows_y0;
        for event in day_events.iter().take(max_rows) {
            let time_text = event.start.hhmm().unwrap_or("ALL");
            render::draw_text(&mut img, &fonts.mono, 11.0, col_x + 4.0, y, time_text, DARK_GRAY);
            let title = if event.summary.is_empty() { "(untitled)" } else { &event.summary };
            let title = truncate_to_width(&fonts.sans_bold, 12.0, title, col_w - 8.0);
            render::draw_text(&mut img, &fonts.sans_bold, 12.0, col_x + 4.0, y + 15.0, &title, BLACK);
            y += row_h;
        }
        if day_events.len() > max_rows {
            let more = format!("+{} more", day_events.len() - max_rows);
            render::draw_text(&mut img, &fonts.sans_bold, 11.0, col_x + 4.0, y, &more, LIGHT_GRAY);
        }
    }

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}
