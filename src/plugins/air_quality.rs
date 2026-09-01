//! Air quality & pollen page (slot 6): the second half of the weather pair
//! (see `plugins::weather`) -- current air quality and UV as a one-line
//! summary (which also names whichever pollen peaks highest today and its
//! risk category, so that's answered before anyone has to go read the
//! chart's legend), then today's four pollen "allergy triggers" as an
//! hourly line graph: grass, birch, alder (kept separate from birch, unlike
//! weed -- birch is the most potent UK tree allergen and the one with an
//! oral allergy syndrome food-crossover reaction, worth telling apart from
//! alder), and weed (worse of ragweed/mugwort per hour). See `chart.rs` for
//! why each series gets its own line style and marker instead of a color.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;

use crate::air_quality::{self, Forecast, Hourly};
use crate::chart::{self, LineStyle, Marker, Series};
use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, H, W, WHITE};

pub const SLOT: u8 = 6;
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

/// Same risk bands as the chart's own Y-axis (`air_quality::draw_pollen_y_axis`,
/// 0/10/50/150 grains/m3) -- the summary line and the chart it sits above
/// should never disagree about what "high" means.
fn pollen_category(p: f32) -> &'static str {
    match p {
        v if v < 1.0 => "NONE",
        v if v < 10.0 => "LOW",
        v if v < 50.0 => "MODERATE",
        v if v < 150.0 => "HIGH",
        _ => "VERY HIGH",
    }
}

/// Which of today's four pollen series peaks highest, and by how much --
/// the answer to "what kind of pollen" the summary line states outright,
/// rather than leaving it to the chart legend below to explain (real
/// panel size makes that legend small print, not a glance-and-know answer).
fn dominant_pollen(hourly: &Hourly) -> Option<(&'static str, f32)> {
    let n = hourly.grass_pollen.len();
    if n == 0 {
        return None;
    }
    let day_max = |v: &[f32]| v.iter().cloned().fold(0.0f32, f32::max);
    let weed_max = (0..n).map(|i| hourly.mugwort_pollen[i].max(hourly.ragweed_pollen[i])).fold(0.0f32, f32::max);
    let candidates = [
        ("GRASS", day_max(&hourly.grass_pollen)),
        ("BIRCH", day_max(&hourly.birch_pollen)),
        ("ALDER", day_max(&hourly.alder_pollen)),
        ("WEED", weed_max),
    ];
    candidates.into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
}

fn render_page(fonts: &Fonts, forecast: &Forecast) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM AIR QUALITY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 40.0, 26.0, 76.0, "AIR & POLLEN", BLACK);

    let pollen_note = dominant_pollen(&forecast.hourly)
        .map(|(name, peak)| format!("  \u{b7}  {name} POLLEN {}", pollen_category(peak)))
        .unwrap_or_default();
    let summary = format!(
        "AQI {:.0} {}  \u{b7}  UV {:.0} {}{}",
        forecast.current.european_aqi,
        aqi_category(forecast.current.european_aqi),
        forecast.current.uv_index,
        uv_category(forecast.current.uv_index),
        pollen_note
    );
    render::draw_text(&mut img, &fonts.sans_bold, 15.0, 26.0, 98.0, &summary, DARK_GRAY);

    draw_line_segment_mut(&mut img, (0.0, 118.0), (W as f32, 118.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 119.0), (W as f32, 119.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 120.0), (W as f32, 120.0), Luma([BLACK]));

    draw_pollen_chart(&mut img, fonts, &forecast.hourly);

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

fn draw_pollen_chart(img: &mut GrayImage, fonts: &Fonts, hourly: &Hourly) {
    let n = hourly.grass_pollen.len();
    if n == 0 {
        return;
    }

    let chart_x0 = 40.0;
    let chart_x1 = W as f32 - 40.0;
    let chart_top = 160.0;
    let baseline = 390.0; // 0 grains/m3
    let chart_h = baseline - chart_top;
    let axis_max = 150.0f32;

    let weed: Vec<f32> = (0..n).map(|i| hourly.mugwort_pollen[i].max(hourly.ragweed_pollen[i])).collect();

    let series = [
        Series { label: "GRASS", values: &hourly.grass_pollen, style: LineStyle::Solid, marker: Marker::Dot },
        Series { label: "BIRCH", values: &hourly.birch_pollen, style: LineStyle::Dashed, marker: Marker::Cross },
        Series { label: "ALDER", values: &hourly.alder_pollen, style: LineStyle::LongDash, marker: Marker::Square },
        Series { label: "WEED", values: &weed, style: LineStyle::Dotted, marker: Marker::Triangle },
    ];

    air_quality::draw_pollen_y_axis(img, fonts, chart_x0, chart_x1, chart_top, baseline);

    let slot_w = (chart_x1 - chart_x0) / (n.max(2) - 1) as f32;
    let point_at = |i: usize, v: f32| {
        let x = chart_x0 + i as f32 * slot_w;
        let y = baseline - chart_h * (v.min(axis_max) / axis_max);
        (x, y)
    };

    for s in &series {
        let points: Vec<(f32, f32)> = (0..n).map(|i| point_at(i, s.values[i])).collect();
        chart::draw_series(img, &points, &s.style, &s.marker, BLACK);
    }

    // Hour-of-day labels every 3rd point, same cadence as the weather rain
    // chart. `air_quality::Hourly`'s doc comment: array index is hour-of-day
    // directly for a `past_days=0` fetch (this page's case).
    for i in (0..n).step_by(3) {
        let label = format!("{i:02}");
        let lw = text_width(&fonts.mono, 13.0, &label);
        let (x, _) = point_at(i, 0.0);
        render::draw_text(img, &fonts.mono, 13.0, x - lw / 2.0, baseline + 18.0, &label, DARK_GRAY);
    }

    chart::draw_legend(img, fonts, &series, chart_x0, 138.0, BLACK);
}
