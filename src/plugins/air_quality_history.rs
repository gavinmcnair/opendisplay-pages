//! Pollen history page (slot 6): the season so far, one point per day
//! instead of per hour -- built to answer "what was pollen doing on the day
//! my son had symptoms", which the daily air-quality page (today only)
//! can't. Same four "allergy triggers" as `plugins::air_quality` (grass,
//! birch, alder, weed), same line-style/marker-per-series convention (see
//! `chart.rs`), but no per-point markers here: ~90 points per series would
//! turn markers into noise, so this chart relies on line style alone.

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;

use crate::air_quality::{self, Forecast, Hourly};
use crate::chart::{self, LineStyle, Marker, Series};
use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, H, W, WHITE};

pub const SLOT: u8 = 6;
const NAME: &str = "Egham Pollen History";
const STATUS_LABEL: &str = "SEASON SO FAR";

/// How far back to fetch -- confirmed working against the real API up to at
/// least 92 (see `air_quality::fetch_forecast`'s doc comment); 90 is a round
/// number comfortably inside that with margin.
const PAST_DAYS: u32 = 90;

pub struct AirQualityHistoryPlugin;

impl Plugin for AirQualityHistoryPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    // This plugin's own data changes at most once a day (a new day's peak
    // added, the oldest day dropping out of the 90-day window) -- no need to
    // poll it on the same fast cadence as pages tracking something that
    // changes by the hour. Refetching a 90-day hourly series is also a much
    // bigger request than this project's other plugins make; asking for it
    // less often is the considerate choice, not just an optimisation.
    fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(6 * 60 * 60)
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let forecast = air_quality::fetch_forecast(PAST_DAYS)?;
            let fingerprint = air_quality::fingerprint(&forecast);
            let img = render_page(fonts, &forecast);
            Ok((fingerprint, img))
        })
    }
}

fn daily_max(hourly_values: &[f32], num_days: usize) -> Vec<f32> {
    (0..num_days)
        .map(|d| hourly_values[d * 24..(d * 24 + 24).min(hourly_values.len())].iter().cloned().fold(0.0, f32::max))
        .collect()
}

fn render_page(fonts: &Fonts, forecast: &Forecast) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    let start_date = (Local::now() - ChronoDuration::days(PAST_DAYS as i64)).date_naive();
    let end_date = Local::now().date_naive();
    let date_range = format!("{} \u{2013} {}", start_date.format("%d %b").to_string().to_uppercase(), end_date.format("%d %b %Y").to_string().to_uppercase());

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM POLLEN", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 40.0, 26.0, 76.0, "SEASON", BLACK);
    render::draw_text(&mut img, &fonts.arial_bold, 15.0, 26.0, 98.0, &date_range, DARK_GRAY);

    draw_line_segment_mut(&mut img, (0.0, 118.0), (W as f32, 118.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 119.0), (W as f32, 119.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 120.0), (W as f32, 120.0), Luma([BLACK]));

    draw_history_chart(&mut img, fonts, &forecast.hourly, start_date);

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

fn draw_history_chart(img: &mut GrayImage, fonts: &Fonts, hourly: &Hourly, start_date: NaiveDate) {
    let num_days = hourly.grass_pollen.len() / 24;
    if num_days == 0 {
        return;
    }

    let chart_x0 = 40.0;
    let chart_x1 = W as f32 - 40.0;
    let chart_top = 160.0;
    let baseline = 390.0;
    let chart_h = baseline - chart_top;
    let axis_max = 150.0f32;

    let grass = daily_max(&hourly.grass_pollen, num_days);
    let birch = daily_max(&hourly.birch_pollen, num_days);
    let alder = daily_max(&hourly.alder_pollen, num_days);
    let weed_hourly: Vec<f32> = hourly
        .mugwort_pollen
        .iter()
        .zip(&hourly.ragweed_pollen)
        .map(|(m, r)| m.max(*r))
        .collect();
    let weed = daily_max(&weed_hourly, num_days);

    // No per-point marker (`Marker::None`) -- with ~90 points a marker at
    // every one would be clutter, not signal. Line style alone still
    // distinguishes all four at this density (see this module's doc comment).
    let series = [
        Series { label: "GRASS", values: &grass, style: LineStyle::Solid, marker: Marker::None },
        Series { label: "BIRCH", values: &birch, style: LineStyle::Dashed, marker: Marker::None },
        Series { label: "ALDER", values: &alder, style: LineStyle::LongDash, marker: Marker::None },
        Series { label: "WEED", values: &weed, style: LineStyle::Dotted, marker: Marker::None },
    ];

    air_quality::draw_pollen_y_axis(img, fonts, chart_x0, chart_x1, chart_top, baseline);

    let slot_w = (chart_x1 - chart_x0) / (num_days.max(2) - 1) as f32;
    let point_at = |i: usize, v: f32| {
        let x = chart_x0 + i as f32 * slot_w;
        let y = baseline - chart_h * (v.min(axis_max) / axis_max);
        (x, y)
    };

    for s in &series {
        let points: Vec<(f32, f32)> = (0..num_days).map(|i| point_at(i, s.values[i])).collect();
        chart::draw_series(img, &points, &s.style, &s.marker, BLACK);
    }

    // Date labels roughly every two weeks -- enough to orient without
    // crowding ~90 daily points the way an hourly cadence would.
    let label_every = (num_days / 7).max(1);
    for i in (0..num_days).step_by(label_every) {
        let date = start_date + ChronoDuration::days(i as i64);
        let label = date.format("%d %b").to_string().to_uppercase();
        let lw = text_width(&fonts.mono, 11.0, &label);
        let (x, _) = point_at(i, 0.0);
        render::draw_text(img, &fonts.mono, 11.0, x - lw / 2.0, baseline + 18.0, &label, DARK_GRAY);
    }

    chart::draw_legend(img, fonts, &series, chart_x0, 138.0, BLACK);
}
