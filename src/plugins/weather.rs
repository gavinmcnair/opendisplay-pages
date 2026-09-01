//! Weather page (slot 3): all-day rain probability by hour, plus temperature.
//! Deliberately plain -- a bar per hour, a number every few hours, no fancy
//! graphics. Humidity and surface pressure are already fetched (see
//! `crate::weather::Hourly`) if a future revision wants to show them too.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_ellipse_mut, draw_filled_rect_mut, draw_line_segment_mut, draw_polygon_mut};
use imageproc::point::Point;
use imageproc::rect::Rect;

use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, truncate_to_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};
use crate::weather::{self, Forecast};

pub const SLOT: u8 = 3;
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

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM WEATHER", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 40.0, 26.0, 76.0, "WEATHER", BLACK);

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
        render::draw_text(&mut img, &fonts.sans_bold, 15.0, 26.0, 98.0, &summary, DARK_GRAY);
    }

    // Top-right summary: current conditions as an icon + big temperature +
    // condition name, the "picture and display" companion to the hourly
    // chart below. Uses `forecast.current` (a real "now" reading), not an
    // hourly-array index -- sidesteps the same now-under-BST problem noted
    // above, for free.
    draw_current_conditions(&mut img, fonts, &forecast.current);

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

    render::draw_text(img, &fonts.sans_bold, 13.0, chart_x0, chart_top - 10.0, "RAIN %", DARK_GRAY);
    draw_line_segment_mut(img, (chart_x0, baseline), (chart_x1, baseline), Luma([BLACK]));

    // Y-axis scale -- without this, a bar's height means nothing on its own
    // (and a bare number next to it reads as "which axis is this" at a
    // glance, exactly the confusion the per-bar temperature labels below
    // have too). Three labelled reference lines (0/50/100%), not a full
    // gridline-per-10% axis -- "don't care about fancy graphics" still
    // rules that out, but an unlabelled chart doesn't meet even the plain
    // bar-chart bar the module doc sets.
    for pct in [0u8, 50, 100] {
        let y = baseline - chart_h * (pct as f32 / 100.0);
        if pct != 0 {
            for x in (chart_x0 as i32..chart_x1 as i32).step_by(6) {
                img.put_pixel(x as u32, y as u32, Luma([LIGHT_GRAY]));
            }
        }
        let label = format!("{pct}");
        let lw = text_width(&fonts.mono, 12.0, &label);
        render::draw_text(img, &fonts.mono, 12.0, chart_x0 - lw - 8.0, y + 4.0, &label, DARK_GRAY);
    }

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
            // "C" suffix isn't cosmetic here -- a bare number sitting inside
            // a 0-100 rain-% chart reads as another percentage at a glance
            // (a real ambiguity, not a hypothetical one: caught by exactly
            // that misreading).
            let temp_label = format!("{temp:.0}C");
            let tlw = text_width(&fonts.mono, 14.0, &temp_label);
            let temp_y = (y0 - 8.0).max(chart_top - 8.0);
            render::draw_text(img, &fonts.mono, 14.0, cx - tlw / 2.0, temp_y, &temp_label, BLACK);
        }
    }
}

/// A WMO weather code (`current.weather_code`) collapsed down to a handful of
/// icon shapes -- there are ~28 distinct codes but only this many silhouettes
/// worth drawing at panel resolution. Table: <https://open-meteo.com/en/docs>.
enum WeatherIcon {
    Sun,
    PartlyCloudy,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Storm,
}

fn classify_weather_code(code: u8) -> (WeatherIcon, &'static str) {
    match code {
        0 => (WeatherIcon::Sun, "CLEAR"),
        1 => (WeatherIcon::Sun, "MOSTLY CLEAR"),
        2 => (WeatherIcon::PartlyCloudy, "PARTLY CLOUDY"),
        3 => (WeatherIcon::Cloudy, "CLOUDY"),
        45 | 48 => (WeatherIcon::Fog, "FOG"),
        51 | 53 | 55 | 56 | 57 => (WeatherIcon::Rain, "DRIZZLE"),
        61 | 63 | 65 | 66 | 67 => (WeatherIcon::Rain, "RAIN"),
        80 | 81 | 82 => (WeatherIcon::Rain, "SHOWERS"),
        71 | 73 | 75 | 77 => (WeatherIcon::Snow, "SNOW"),
        85 | 86 => (WeatherIcon::Snow, "SNOW SHOWERS"),
        95 | 96 | 99 => (WeatherIcon::Storm, "THUNDERSTORM"),
        _ => (WeatherIcon::Cloudy, "UNKNOWN"),
    }
}

/// Top-right "picture and display": an icon for `current.weather_code`, the
/// current temperature in large type, and the condition name -- the at-a-glance
/// summary the hourly rain chart below doesn't give you on its own.
fn draw_current_conditions(img: &mut GrayImage, fonts: &Fonts, current: &weather::Current) {
    let (icon, label) = classify_weather_code(current.weather_code);

    let icon_x = 580.0;
    let icon_y = 10.0;
    draw_weather_icon(img, &icon, icon_x, icon_y);

    let text_x = icon_x + ICON_W + 12.0;
    let max_w = W as f32 - 26.0 - text_x;

    let temp_text = format!("{:.0}C", current.temperature_2m);
    render::draw_text(img, &fonts.sans_black, 34.0, text_x, 46.0, &temp_text, BLACK);

    let label = truncate_to_width(&fonts.sans_bold, 13.0, label, max_w);
    render::draw_text(img, &fonts.sans_bold, 13.0, text_x, 68.0, &label, DARK_GRAY);
}

const ICON_W: f32 = 56.0;

/// Draws one weather icon in a fixed `ICON_W x ICON_H` box whose top-left
/// corner is (x, y). Flat-filled ellipses/polygons throughout, matching the
/// bold, no-gradient style `trains.rs` already uses for its own marks --
/// legible at panel resolution and honest about not being fine art.
fn draw_weather_icon(img: &mut GrayImage, icon: &WeatherIcon, x: f32, y: f32) {
    match icon {
        WeatherIcon::Sun => draw_sun(img, x, y, BLACK),
        WeatherIcon::PartlyCloudy => {
            draw_sun_partial(img, x, y);
            draw_cloud(img, x, y + 10.0, DARK_GRAY);
        }
        WeatherIcon::Cloudy => draw_cloud(img, x, y + 6.0, BLACK),
        WeatherIcon::Fog => {
            draw_cloud(img, x, y, LIGHT_GRAY);
            for i in 0..3 {
                let ly = y + 34.0 + i as f32 * 6.0;
                draw_line_segment_mut(img, (x + 6.0, ly), (x + ICON_W - 6.0, ly), Luma([DARK_GRAY]));
            }
        }
        WeatherIcon::Rain => {
            draw_cloud(img, x, y, BLACK);
            draw_drops(img, x, y);
        }
        WeatherIcon::Snow => {
            draw_cloud(img, x, y, BLACK);
            draw_flakes(img, x, y);
        }
        WeatherIcon::Storm => {
            draw_cloud(img, x, y, BLACK);
            draw_bolt(img, x, y);
        }
    }
}

/// A cloud silhouette: three overlapping filled lobes plus a base rect to
/// square off the bottom edge, all within the icon box's lower two-thirds.
fn draw_cloud(img: &mut GrayImage, x: f32, y: f32, fill: u8) {
    let color = Luma([fill]);
    draw_filled_ellipse_mut(img, ((x + 18.0) as i32, (y + 26.0) as i32), 14, 11, color);
    draw_filled_ellipse_mut(img, ((x + 30.0) as i32, (y + 20.0) as i32), 16, 13, color);
    draw_filled_ellipse_mut(img, ((x + 42.0) as i32, (y + 27.0) as i32), 12, 10, color);
    draw_filled_rect_mut(img, Rect::at((x + 10.0) as i32, (y + 24.0) as i32).of_size(36, 10), color);
}

fn draw_sun(img: &mut GrayImage, x: f32, y: f32, fill: u8) {
    let color = Luma([fill]);
    let (cx, cy, r) = (x + 28.0, y + 22.0, 14.0);
    draw_filled_ellipse_mut(img, (cx as i32, cy as i32), r as i32, r as i32, color);
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::FRAC_PI_4;
        let (dx, dy) = (a.cos(), a.sin());
        let inner = r + 3.0;
        let outer = r + 9.0;
        draw_line_segment_mut(img, (cx + dx * inner, cy + dy * inner), (cx + dx * outer, cy + dy * outer), color);
    }
}

/// Smaller sun, offset to the top-right of the icon box, peeking out from
/// behind the cloud `PartlyCloudy` draws on top of it.
fn draw_sun_partial(img: &mut GrayImage, x: f32, y: f32) {
    let color = Luma([BLACK]);
    let (cx, cy, r) = (x + 38.0, y + 12.0, 9.0);
    draw_filled_ellipse_mut(img, (cx as i32, cy as i32), r as i32, r as i32, color);
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::FRAC_PI_4;
        let (dx, dy) = (a.cos(), a.sin());
        let inner = r + 2.0;
        let outer = r + 6.0;
        draw_line_segment_mut(img, (cx + dx * inner, cy + dy * inner), (cx + dx * outer, cy + dy * outer), color);
    }
}

fn draw_drops(img: &mut GrayImage, x: f32, y: f32) {
    let color = Luma([DARK_GRAY]);
    for dx in [16.0, 28.0, 40.0] {
        draw_line_segment_mut(img, (x + dx, y + 38.0), (x + dx - 4.0, y + 45.0), color);
    }
}

fn draw_flakes(img: &mut GrayImage, x: f32, y: f32) {
    let color = Luma([DARK_GRAY]);
    for dx in [16.0, 28.0, 40.0] {
        let (cx, cy) = (x + dx, y + 41.0);
        draw_line_segment_mut(img, (cx - 4.0, cy), (cx + 4.0, cy), color);
        draw_line_segment_mut(img, (cx, cy - 4.0), (cx, cy + 4.0), color);
        draw_line_segment_mut(img, (cx - 3.0, cy - 3.0), (cx + 3.0, cy + 3.0), color);
        draw_line_segment_mut(img, (cx - 3.0, cy + 3.0), (cx + 3.0, cy - 3.0), color);
    }
}

/// Six-point zigzag bolt, normalized over a 16x22 box anchored at
/// `(x+20, y+32)` (below the cloud, extending a little past the icon box's
/// nominal bottom edge -- harmless, nothing else occupies that space for a
/// storm icon).
fn draw_bolt(img: &mut GrayImage, x: f32, y: f32) {
    let (bx, by, bw, bh) = (x + 20.0, y + 32.0, 16.0, 22.0);
    let pt = |nx: f32, ny: f32| Point::new((bx + nx * bw) as i32, (by + ny * bh) as i32);
    let pts = vec![pt(0.65, 0.0), pt(0.30, 0.45), pt(0.55, 0.45), pt(0.15, 1.0), pt(0.85, 0.40), pt(0.55, 0.40)];
    draw_polygon_mut(img, &pts, Luma([BLACK]));
}
