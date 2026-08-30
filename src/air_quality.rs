//! Open-Meteo Air Quality client (fundamentals, layer 1) -- same provider,
//! same no-API-key/no-auth shape as `weather.rs`, just a different endpoint
//! (`air-quality-api` instead of `api`). Covers what a household actually
//! wants to know before going outside: air quality, UV, and pollen.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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
/// model, not an instantaneous sensor reading) -- shown as a full-day line
/// graph by `plugins::air_quality` instead of a single "now" figure, which
/// sidesteps needing one anyway. No per-point timestamp: `forecast_days=1`
/// always starts at local midnight, so array index *is* hour-of-day.
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

pub fn fetch_forecast() -> Result<Forecast> {
    let url = format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={LATITUDE}&longitude={LONGITUDE}\
         &current=european_aqi,uv_index\
         &hourly=alder_pollen,birch_pollen,grass_pollen,mugwort_pollen,ragweed_pollen\
         &timezone=Europe%2FLondon&forecast_days=1"
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
