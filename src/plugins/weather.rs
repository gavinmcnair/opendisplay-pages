//! Weather page (slot 2): all-day rain probability by hour, plus temperature.
//! Deliberately plain -- a bar per hour, a number every few hours, no fancy
//! graphics. Humidity and surface pressure are already fetched (see
//! `crate::weather::Hourly`) if a future revision wants to show them too.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_rect_mut, draw_line_segment_mut};
use imageproc::rect::Rect;

use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};
use crate::weather::{self, Forecast};

pub const SLOT: u8 = 2;
const NAME: &str = "Egham Weather";
const STATUS_LABEL: &str = "ALL-DAY RAIN FORECAST";

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let forecast = weather::fetch_forecast()?;
            let fingerprint = weather::fingerprint(&forecast);
            let img = render_page(fonts, &forecast);
            Ok((fingerprint, img))
        })
    }
}

fn render_page(fonts: &Fonts, forecast: &Forecast) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));
    let hourly = &forecast.hourly;
    let n = hourly.time.len().min(hourly.temperature_2m.len()).min(hourly.precipitation_probability.len());

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM WEATHER", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 40.0, 26.0, 76.0, "WEATHER", BLACK);

    // Summary computed from the whole day's range, not "the current hour" --
    // Open-Meteo returns local (Europe/London) timestamps, but figuring out
    // which array index is "now" needs real timezone handling this project
    // doesn't otherwise need; a day's min/max is simple, needs no clock at
    // all, and is arguably more useful at a glance anyway.
    if n > 0 {
        let min_t = hourly.temperature_2m[..n].iter().cloned().fold(f32::INFINITY, f32::min);
        let max_t = hourly.temperature_2m[..n].iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let max_rain = hourly.precipitation_probability[..n].iter().copied().max().unwrap_or(0);
        let summary = format!("TODAY {min_t:.0}-{max_t:.0}C  UP TO {max_rain}% RAIN");
        render::draw_text(&mut img, &fonts.arial_bold, 15.0, 26.0, 98.0, &summary, DARK_GRAY);
    }

    draw_line_segment_mut(&mut img, (0.0, 118.0), (W as f32, 118.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 119.0), (W as f32, 119.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 120.0), (W as f32, 120.0), Luma([BLACK]));

    draw_rain_chart(&mut img, fonts, hourly, n);

    // NOT hourly.time[0] -- that's always this forecast's midnight entry,
    // not when we actually fetched it (a real bug caught by looking at the
    // rendered output: it showed "UPDATED 00:00" regardless of real time).
    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

fn draw_rain_chart(img: &mut GrayImage, fonts: &Fonts, hourly: &weather::Hourly, n: usize) {
    if n == 0 {
        return;
    }
    let chart_x0 = 40.0;
    let chart_x1 = W as f32 - 40.0;
    let chart_top = 150.0; // 100% rain line
    let baseline = 390.0; // 0% rain line
    let chart_h = baseline - chart_top;

    render::draw_text(img, &fonts.arial_bold, 13.0, chart_x0, chart_top - 10.0, "RAIN %", DARK_GRAY);
    draw_line_segment_mut(img, (chart_x0, baseline), (chart_x1, baseline), Luma([BLACK]));

    let slot_w = (chart_x1 - chart_x0) / n as f32;
    let bar_w = (slot_w * 0.6).max(1.0);

    for i in 0..n {
        let prob = hourly.precipitation_probability[i].min(100) as f32;
        let bar_h = chart_h * (prob / 100.0);
        let x0 = chart_x0 + i as f32 * slot_w + (slot_w - bar_w) / 2.0;
        let y0 = baseline - bar_h;
        if bar_h >= 1.0 {
            draw_filled_rect_mut(
                img,
                Rect::at(x0 as i32, y0 as i32).of_size(bar_w.max(1.0) as u32, bar_h.max(1.0) as u32),
                Luma([DARK_GRAY]),
            );
        }

        // Every 3rd hour: hour label below the baseline, temperature above
        // the bar. Labelling every hour at this width would overlap.
        if i % 3 == 0 {
            let hour_label = &weather::hhmm(&hourly.time[i])[0..2];
            let hlw = text_width(&fonts.mono, 13.0, hour_label);
            let cx = chart_x0 + i as f32 * slot_w + slot_w / 2.0;
            render::draw_text(img, &fonts.mono, 13.0, cx - hlw / 2.0, baseline + 18.0, hour_label, DARK_GRAY);

            let temp = hourly.temperature_2m.get(i).copied().unwrap_or(0.0);
            let temp_label = format!("{temp:.0}");
            let tlw = text_width(&fonts.mono, 14.0, &temp_label);
            let temp_y = (y0 - 8.0).max(chart_top - 8.0);
            render::draw_text(img, &fonts.mono, 14.0, cx - tlw / 2.0, temp_y, &temp_label, BLACK);
        }
    }

    // Faint 50% reference line so a bar's height means something without a
    // full axis -- "don't care about fancy graphics" ruled out gridlines
    // every 10%, but one midpoint line is cheap and genuinely useful.
    let mid_y = baseline - chart_h * 0.5;
    for x in (chart_x0 as i32..chart_x1 as i32).step_by(6) {
        img.put_pixel(x as u32, mid_y as u32, Luma([LIGHT_GRAY]));
    }
}
