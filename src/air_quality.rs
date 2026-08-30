//! Open-Meteo Air Quality client (fundamentals, layer 1) -- same provider,
//! same no-API-key/no-auth shape as `weather.rs`, just a different endpoint
//! (`air-quality-api` instead of `api`). Covers what a household actually
//! wants to know before going outside: air quality, UV, and pollen.

use anyhow::{Context, Result};
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, LIGHT_GRAY};

// Egham, Surrey -- same coordinates as weather.rs.
const LATITUDE: f64 = 51.4295;
const LONGITUDE: f64 = -0.5510;

/// A real "now" reading, same reasoning as `weather::Current`: no guessing
/// which hourly index is "now". Unlike pollen, AQI/UV genuinely have an
/// instantaneous `current` value in this API.
#[derive(Deserialize, Debug, Clone)]
pub struct Current {
    pub european_aqi: f32,
    pub uv_index: f32,
}

/// Pollen has no `current` block in this API at all (it's a daily/hourly
/// model, not an instantaneous sensor reading) -- shown as a line graph by
/// `plugins::air_quality`/`plugins::air_quality_history` instead of a single
/// "now" figure, which sidesteps needing one anyway. No per-point
/// timestamp: `forecast_days=1` and a given `past_days` (see
/// `fetch_forecast`) together mean the series always starts at local
/// midnight `past_days` days ago, so array index `i` is always
/// `i / 24` days in and `i % 24` hours-of-day from there -- callers derive
/// both directly rather than parsing a timestamp string.
#[derive(Deserialize, Debug, Clone)]
pub struct Hourly {
    pub alder_pollen: Vec<f32>,
    pub birch_pollen: Vec<f32>,
    pub grass_pollen: Vec<f32>,
    pub mugwort_pollen: Vec<f32>,
    pub ragweed_pollen: Vec<f32>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Forecast {
    pub current: Current,
    pub hourly: Hourly,
}

/// `past_days` extends `hourly` backwards from today (0 = just today, up to
/// at least 92 confirmed working against the real API) -- the same
/// forecast/current shape either way, just a longer `hourly` series. Built
/// for `plugins::air_quality_history`: matching a symptom date against which
/// pollen was actually elevated that day needs more than today's snapshot.
pub fn fetch_forecast(past_days: u32) -> Result<Forecast> {
    let url = format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={LATITUDE}&longitude={LONGITUDE}\
         &current=european_aqi,uv_index\
         &hourly=alder_pollen,birch_pollen,grass_pollen,mugwort_pollen,ragweed_pollen\
         &timezone=Europe%2FLondon&forecast_days=1&past_days={past_days}"
    );
    let resp: Forecast = ureq::get(&url)
        .call()
        .context("fetching Open-Meteo air quality forecast")?
        .into_json()
        .context("parsing Open-Meteo air quality response")?;
    Ok(resp)
}

/// Fingerprints the meaningful values -- current AQI/UV plus every hourly
/// pollen series (not just today's peak, so a change in the *shape* of the
/// day -- e.g. a peak shifting from noon to evening -- still counts as a
/// change even when the peak value itself doesn't move).
pub fn fingerprint(forecast: &Forecast) -> u64 {
    let mut hasher = DefaultHasher::new();
    forecast.current.european_aqi.to_bits().hash(&mut hasher);
    forecast.current.uv_index.to_bits().hash(&mut hasher);
    for v in &forecast.hourly.alder_pollen {
        v.to_bits().hash(&mut hasher);
    }
    for v in &forecast.hourly.birch_pollen {
        v.to_bits().hash(&mut hasher);
    }
    for v in &forecast.hourly.grass_pollen {
        v.to_bits().hash(&mut hasher);
    }
    for v in &forecast.hourly.mugwort_pollen {
        v.to_bits().hash(&mut hasher);
    }
    for v in &forecast.hourly.ragweed_pollen {
        v.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Y-axis for a pollen chart (grains/m3): gridlines and labels at the
/// published risk-band cutoffs -- 0=none, 10=low/moderate, 50=moderate/high,
/// 150=high/very-high -- not arbitrary round numbers, same reasoning as the
/// weather rain chart's 0/50/100%. Shared by `plugins::air_quality` (today,
/// hourly) and `plugins::air_quality_history` (weeks, daily) so both charts
/// read against the same scale.
pub fn draw_pollen_y_axis(img: &mut GrayImage, fonts: &Fonts, chart_x0: f32, chart_x1: f32, chart_top: f32, baseline: f32) {
    let chart_h = baseline - chart_top;
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
}
