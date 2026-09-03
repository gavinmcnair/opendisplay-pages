//! Open-Meteo hourly forecast client (fundamentals, layer 1) -- free, no API
//! key, no token to mint (unlike RTT), and `timezone=Europe/London` means
//! the API itself handles GMT/BST so this file never has to.

use anyhow::{Context, Result};
use chrono::Local;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Hours shown in the rolling forecast window (see `current_hour_index`).
pub const WINDOW_HOURS: usize = 24;

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

/// Open-Meteo's `current` block, unlike `hourly`, is a real "now" reading --
/// no guessing which hourly index corresponds to the current time (which
/// would need real timezone/DST handling this project otherwise avoids, see
/// `plugins::weather`'s header comment). `weather_code` is a WMO code, mapped
/// to an icon by `plugins::weather::classify_weather_code`.
#[derive(Deserialize, Debug, Clone)]
pub struct Current {
    pub temperature_2m: f32,
    pub weather_code: u8,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Forecast {
    pub current: Current,
    pub hourly: Hourly,
}

pub fn fetch_forecast() -> Result<Forecast> {
    // 2 days (48 hourly points): enough that a full WINDOW_HOURS-ahead
    // window still fits even at 23:00, when it spills well into tomorrow.
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={LATITUDE}&longitude={LONGITUDE}\
         &current=temperature_2m,weather_code\
         &hourly=temperature_2m,precipitation_probability,relative_humidity_2m,surface_pressure\
         &timezone=Europe%2FLondon&forecast_days=2"
    );
    let resp: Forecast = ureq::get(&url)
        .call()
        .context("fetching Open-Meteo forecast")?
        .into_json()
        .context("parsing Open-Meteo forecast response")?;
    Ok(resp)
}

/// Index of the current wall-clock hour within `hourly.time`, so the chart
/// can roll forward and show only the hours still ahead (no point showing
/// what the weather WAS). Sound because both sides use Europe/London: the
/// API request sets `timezone=Europe/London` and the process runs with
/// `TZ=Europe/London`, so `chrono::Local` and the returned timestamps
/// agree. Falls back to 0 (midnight) if no match -- a clock/tz mismatch
/// degrades to the old full-day view rather than erroring.
pub fn current_hour_index(hourly: &Hourly) -> usize {
    let now_hour = Local::now().format("%Y-%m-%dT%H").to_string(); // e.g. "2026-09-03T14"
    hourly
        .time
        .iter()
        .position(|t| t.len() >= 13 && t[..13].as_ref() as &str >= now_hour.as_str())
        .unwrap_or(0)
}

/// Fingerprints exactly the displayed window (current conditions + the
/// `WINDOW_HOURS` of temperature/rain starting at `start`) PLUS `start`
/// itself -- so the panel repaints when the window rolls to a new hour even
/// if the forecast numbers are unchanged, and does NOT repaint for changes
/// to hours that have scrolled off or aren't shown.
pub fn fingerprint_window(forecast: &Forecast, start: usize, hours: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    forecast.current.temperature_2m.to_bits().hash(&mut hasher);
    forecast.current.weather_code.hash(&mut hasher);
    start.hash(&mut hasher);
    let temps = &forecast.hourly.temperature_2m;
    let rain = &forecast.hourly.precipitation_probability;
    let end = (start + hours).min(temps.len()).min(rain.len());
    for i in start..end {
        temps[i].to_bits().hash(&mut hasher);
        rain[i].hash(&mut hasher);
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
