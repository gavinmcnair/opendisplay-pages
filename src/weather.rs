//! Open-Meteo hourly forecast client (fundamentals, layer 1) -- free, no API
//! key, no token to mint (unlike RTT), and `timezone=Europe/London` means
//! the API itself handles GMT/BST so this file never has to.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// Egham, Surrey.
const LATITUDE: f64 = 51.4295;
const LONGITUDE: f64 = -0.5510;

#[derive(Deserialize, Debug, Clone)]
pub struct Hourly {
    pub time: Vec<String>, // "2026-08-30T00:00", local (Europe/London) per the request below
    pub temperature_2m: Vec<f32>,
    pub precipitation_probability: Vec<u8>,
    pub relative_humidity_2m: Vec<u8>,
    pub surface_pressure: Vec<f32>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Forecast {
    pub hourly: Hourly,
}

pub fn fetch_forecast() -> Result<Forecast> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={LATITUDE}&longitude={LONGITUDE}\
         &hourly=temperature_2m,precipitation_probability,relative_humidity_2m,surface_pressure\
         &timezone=Europe%2FLondon&forecast_days=1"
    );
    let resp: Forecast = ureq::get(&url)
        .call()
        .context("fetching Open-Meteo forecast")?
        .into_json()
        .context("parsing Open-Meteo forecast response")?;
    Ok(resp)
}

/// Fingerprints the meaningful hourly values (temperature, rain probability,
/// humidity, pressure) -- excludes nothing here deliberately, since unlike
/// RTT's `query.time_from` this response has no "generated at" field that
/// would otherwise make every poll look like a change.
pub fn fingerprint(forecast: &Forecast) -> u64 {
    let mut hasher = DefaultHasher::new();
    for t in &forecast.hourly.temperature_2m {
        t.to_bits().hash(&mut hasher);
    }
    forecast.hourly.precipitation_probability.hash(&mut hasher);
    forecast.hourly.relative_humidity_2m.hash(&mut hasher);
    for p in &forecast.hourly.surface_pressure {
        p.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Extracts "HH:MM" from an Open-Meteo hourly timestamp ("2026-08-30T14:00").
pub fn hhmm(iso: &str) -> &str {
    if iso.len() >= 16 {
        &iso[11..16]
    } else {
        iso
    }
}
