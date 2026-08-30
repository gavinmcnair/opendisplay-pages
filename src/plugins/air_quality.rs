//! Air quality & pollen page (slot 5): the second half of the weather pair
//! (see `plugins::weather`) -- current air quality and UV as a one-line
//! summary, then today's three pollen "allergy triggers" (grass, tree,
//! weed -- tree and weed are each the worse of two API species, combined
//! per-hour below) as an hourly line graph, each series drawn with a
//! distinct line style and point marker rather than a color (the panel has
//! none): solid/dot for grass, dashed/x for tree, dotted/triangle for weed.
//! A legend spells out which is which.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_ellipse_mut, draw_line_segment_mut, draw_polygon_mut};
use imageproc::point::Point;

use crate::air_quality::{self, Forecast, Hourly};
use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};

pub const SLOT: u8 = 5;
const NAME: &str = "Egham Air Quality";
const STATUS_LABEL: &str = "AIR QUALITY & POLLEN";

pub struct AirQualityPlugin;

impl Plugin for AirQualityPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let forecast = air_quality::fetch_forecast()?;
            let fingerprint = air_quality::fingerprint(&forecast);
            let img = render_page(fonts, &forecast);
            Ok((fingerprint, img))
        })
    }
}

fn aqi_category(aqi: f32) -> &'static str {
    match aqi {
        a if a < 20.0 => "GOOD",
        a if a < 40.0 => "FAIR",
        a if a < 60.0 => "MODERATE",
        a if a < 80.0 => "POOR",
        a if a < 100.0 => "VERY POOR",
        _ => "EXTREMELY POOR",
    }
}

fn uv_category(uv: f32) -> &'static str {
    match uv {
        u if u < 3.0 => "LOW",
        u if u < 6.0 => "MODERATE",
        u if u < 8.0 => "HIGH",
        u if u < 11.0 => "VERY HIGH",
        _ => "EXTREME",
    }
}

fn render_page(fonts: &Fonts, forecast: &Forecast) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM AIR QUALITY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 40.0, 26.0, 76.0, "AIR & POLLEN", BLACK);

    let summary = format!(
        "AQI {:.0} {}  \u{b7}  UV {:.0} {}",
        forecast.current.european_aqi,
        aqi_category(forecast.current.european_aqi),
        forecast.current.uv_index,
        uv_category(forecast.current.uv_index)
    );
    render::draw_text(&mut img, &fonts.arial_bold, 15.0, 26.0, 98.0, &summary, DARK_GRAY);

    draw_line_segment_mut(&mut img, (0.0, 118.0), (W as f32, 118.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 119.0), (W as f32, 119.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 120.0), (W as f32, 120.0), Luma([BLACK]));

    draw_pollen_chart(&mut img, fonts, &forecast.hourly);

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

enum LineStyle {
    Solid,
    Dashed,
    Dotted,
}

enum Marker {
    Dot,
    Cross,
    Triangle,
}

struct Series<'a> {
    label: &'a str,
    values: &'a [f32],
    style: LineStyle,
    marker: Marker,
}

fn draw_pollen_chart(img: &mut GrayImage, fonts: &Fonts, hourly: &Hourly) {
    let n = hourly.grass_pollen.len();
    if n == 0 {
        return;
    }

    let chart_x0 = 40.0;
    let chart_x1 = W as f32 - 40.0;
    let chart_top = 160.0; // top of axis, see y-axis loop below
    let baseline = 390.0; // 0 grains/m3
    let chart_h = baseline - chart_top;

    // Combine each species into the three "allergy triggers" this page
    // reports (see air_quality::pollen_peaks for why tree/weed are each
    // already the worse of two species) -- but per-hour here, not just
    // today's peak, since this is a line graph of the whole day's shape.
    let tree: Vec<f32> =
        (0..n).map(|i| hourly.alder_pollen[i].max(hourly.birch_pollen[i])).collect();
    let weed: Vec<f32> =
        (0..n).map(|i| hourly.mugwort_pollen[i].max(hourly.ragweed_pollen[i])).collect();

    let series = [
        Series { label: "GRASS", values: &hourly.grass_pollen, style: LineStyle::Solid, marker: Marker::Dot },
        Series { label: "TREE", values: &tree, style: LineStyle::Dashed, marker: Marker::Cross },
        Series { label: "WEED", values: &weed, style: LineStyle::Dotted, marker: Marker::Triangle },
    ];

    // Y-axis at the published risk-band cutoffs (grains/m3), not arbitrary
    // round numbers -- same reasoning as the weather rain chart's 0/50/100%:
    // the gridlines should mean something, not just subdivide evenly.
    // 0=none, 10=low/moderate boundary, 50=moderate/high, 150=high/very high.
    let axis_max = 150.0f32;
    for level in [0.0, 10.0, 50.0, 150.0] {
        let y = baseline - chart_h * (level / axis_max).min(1.0);
        if level != 0.0 {
            for x in (chart_x0 as i32..chart_x1 as i32).step_by(6) {
                img.put_pixel(x as u32, y as u32, Luma([LIGHT_GRAY]));
            }
        }
        let label = format!("{level:.0}");
        let lw = text_width(&fonts.mono, 11.0, &label);
        render::draw_text(img, &fonts.mono, 11.0, chart_x0 - lw - 8.0, y + 4.0, &label, DARK_GRAY);
    }
    draw_line_segment_mut(img, (chart_x0, baseline), (chart_x1, baseline), Luma([BLACK]));

    let slot_w = (chart_x1 - chart_x0) / (n.max(2) - 1) as f32;
    let point_at = |i: usize, v: f32| {
        let x = chart_x0 + i as f32 * slot_w;
        let y = baseline - chart_h * (v.min(axis_max) / axis_max);
        (x, y)
    };

    for s in &series {
        let points: Vec<(f32, f32)> = (0..n).map(|i| point_at(i, s.values[i])).collect();
        draw_series_line(img, &points, &s.style, BLACK);
        for &(x, y) in &points {
            draw_marker(img, x, y, &s.marker, BLACK);
        }
    }

    // Hour-of-day labels every 3rd point, same cadence as the weather rain
    // chart. forecast_days=1 always starts at local midnight (see
    // air_quality::Hourly's doc comment -- no per-point timestamp is
    // modeled), so the array index doubles as the hour directly.
    for i in (0..n).step_by(3) {
        let label = format!("{i:02}");
        let lw = text_width(&fonts.mono, 13.0, &label);
        let (x, _) = point_at(i, 0.0);
        render::draw_text(img, &fonts.mono, 13.0, x - lw / 2.0, baseline + 18.0, &label, DARK_GRAY);
    }

    draw_legend(img, fonts, &series, chart_x0, 138.0);
}

fn draw_legend(img: &mut GrayImage, fonts: &Fonts, series: &[Series], x0: f32, y: f32) {
    let mut x = x0;
    for s in series {
        let sample_w = 28.0;
        let points = [(x, y - 4.0), (x + sample_w, y - 4.0)];
        draw_series_line(img, &points, &s.style, BLACK);
        draw_marker(img, x + sample_w / 2.0, y - 4.0, &s.marker, BLACK);
        render::draw_text(img, &fonts.arial_bold, 13.0, x + sample_w + 8.0, y, s.label, DARK_GRAY);
        x += sample_w + 8.0 + text_width(&fonts.arial_bold, 13.0, s.label) + 24.0;
    }
}

fn draw_series_line(img: &mut GrayImage, points: &[(f32, f32)], style: &LineStyle, color: u8) {
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        match style {
            LineStyle::Solid => draw_line_segment_mut(img, (x0, y0), (x1, y1), Luma([color])),
            LineStyle::Dashed => draw_patterned_segment(img, (x0, y0), (x1, y1), color, 8.0, 6.0),
            LineStyle::Dotted => draw_patterned_segment(img, (x0, y0), (x1, y1), color, 2.0, 5.0),
        }
    }
}

/// Walks from `a` to `b` in fixed-length steps, alternately drawing `on_len`
/// worth of segment and skipping `off_len` -- one implementation covers both
/// "dashed" (long on, medium off) and "dotted" (short on, short off) by
/// varying the two lengths, rather than two near-duplicate functions.
fn draw_patterned_segment(img: &mut GrayImage, a: (f32, f32), b: (f32, f32), color: u8, on_len: f32, off_len: f32) {
    let (ax, ay) = a;
    let (bx, by) = b;
    let dx = bx - ax;
    let dy = by - ay;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.01 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let mut travelled = 0.0;
    let mut on = true;
    while travelled < len {
        let step = if on { on_len } else { off_len };
        let next = (travelled + step).min(len);
        if on {
            let p0 = (ax + ux * travelled, ay + uy * travelled);
            let p1 = (ax + ux * next, ay + uy * next);
            draw_line_segment_mut(img, p0, p1, Luma([color]));
        }
        travelled = next;
        on = !on;
    }
}

fn draw_marker(img: &mut GrayImage, x: f32, y: f32, marker: &Marker, color: u8) {
    let r = 4.0;
    match marker {
        Marker::Dot => {
            draw_filled_ellipse_mut(img, (x as i32, y as i32), r as i32, r as i32, Luma([color]));
        }
        Marker::Cross => {
            draw_line_segment_mut(img, (x - r, y - r), (x + r, y + r), Luma([color]));
            draw_line_segment_mut(img, (x - r, y + r), (x + r, y - r), Luma([color]));
        }
        Marker::Triangle => {
            let pts = vec![
                Point::new(x as i32, (y - r) as i32),
                Point::new((x - r) as i32, (y + r) as i32),
                Point::new((x + r) as i32, (y + r) as i32),
            ];
            draw_polygon_mut(img, &pts, Luma([color]));
        }
    }
}
