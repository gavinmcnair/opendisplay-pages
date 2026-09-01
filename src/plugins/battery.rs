//! Battery page (slot 1): the panel's own battery, one voltage reading per
//! hour drawn as a 7-day line graph, with the current state of charge as a
//! big headline number. Unlike every other page, the data source is the
//! panel itself: `ble::read_msd` (CMD_READ_MSD, 0x0044) returns the same
//! 16-byte telemetry buffer the device broadcasts in its advertising data --
//! battery voltage in 10mV units plus chip temperature.
//!
//! Voltage -> state-of-charge uses the `battery-estimator` crate with its
//! Li-Ion curve, matching the device config's own choice
//! (`capacity_estimator: 5` = OD_CAPACITY_EST_SEEED_LI_ION in
//! egham_display_config.yaml) -- the panel and this page should never
//! disagree about what "50%" means.
//!
//! "EST. n DAYS LEFT" is computed here, not by the crate (which is
//! stateless voltage->SOC only): a least-squares slope over the last 48h of
//! stored SOC readings, extrapolated to 0%. That's deliberately based on
//! this panel's measured drain rate rather than a datasheet capacity --
//! `battery_capacity_mah` is 0 (unknown) in the device config anyway.
//!
//! Readings persist in `battery_history.txt` in the working directory
//! (gitignored; lands on the /data volume in Docker), one
//! `<unix_ts> <mv> <temp_c>` line each, pruned to the 7-day window.

use anyhow::{Context, Result};
use battery_estimator::{BatteryChemistry, SocEstimator};
use btleplug::api::Peripheral as _;
use chrono::{DateTime, Local, TimeZone};
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use crate::ble;
use crate::chart::{self, LineStyle, Marker};
use crate::plugin::{self, Plugin};
use crate::render::{self, text_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};

pub const SLOT: u8 = 1;
const NAME: &str = "Egham Battery";
const STATUS_LABEL: &str = "PANEL BATTERY MONITOR";

const HISTORY_FILE: &str = "battery_history.txt";
/// One column per hour for a week -- matches the poll cadence below.
const WINDOW: Duration = Duration::from_secs(7 * 24 * 3600);
/// Don't append a second reading within this gap -- a restart mid-hour
/// re-renders immediately, and duplicate near-simultaneous points would put
/// a false kink in the discharge slope.
const MIN_SAMPLE_GAP: Duration = Duration::from_secs(30 * 60);
/// The discharge-rate fit uses only this much trailing history: recent
/// behaviour predicts the near future better than last week's does, and the
/// rate genuinely varies (BLE traffic, temperature).
const ETA_FIT_WINDOW: Duration = Duration::from_secs(48 * 3600);

/// Y axis == the Li-Ion curve's own domain (4.2V full, 3.3V cutoff), so the
/// line's vertical position IS the charge state, not an arbitrary zoom.
const V_MIN: f32 = 3.3;
const V_MAX: f32 = 4.2;

#[derive(Clone, Copy)]
struct Sample {
    ts: i64, // unix seconds
    mv: u16,
    temp_c: f32,
}

/// Live system facts shown in the strip above the status bar -- captured on
/// the same hourly connection as the battery sample, cached on the plugin so
/// a tick that skips the BLE read (MIN_SAMPLE_GAP) still renders the last
/// known values. Deliberately NOT part of the fingerprint: advertising RSSI
/// jitters constantly, and repushing pixel-noise to e-ink for it would defeat
/// the whole change-detection scheme -- the strip refreshes anyway whenever
/// a new battery sample repushes the page.
struct SysInfo {
    firmware: String,
    rssi_dbm: Option<i16>,
}

pub struct BatteryPlugin {
    sys: Option<SysInfo>,
}

impl BatteryPlugin {
    pub fn new() -> Self {
        Self { sys: None }
    }
}

impl Plugin for BatteryPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    /// Hourly: each render costs a real BLE connect to the panel (the data
    /// lives on the device), and battery voltage moves slowly -- polling
    /// faster would cost radio time (= the very battery being measured) to
    /// draw the same line.
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(3600)
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let mut history = load_history();

            let now = Local::now().timestamp();
            let due = history.last().map_or(true, |s| now - s.ts >= MIN_SAMPLE_GAP.as_secs() as i64);
            if due {
                let (peripheral, rssi) = ble::find_and_connect_with_rssi(ble::DEVICE_NAME).await?;
                let telemetry = ble::read_telemetry(&peripheral).await;
                let _ = peripheral.disconnect().await;
                let telemetry = telemetry?;
                let mv = telemetry.battery_mv.context(
                    "device reports no battery reading (raw 0 -- sense unconfigured or not yet sampled)",
                )?;
                history.push(Sample { ts: now, mv, temp_c: telemetry.temperature_c });
                self.sys = Some(SysInfo { firmware: telemetry.firmware, rssi_dbm: rssi });
            }

            history.retain(|s| now - s.ts <= WINDOW.as_secs() as i64);
            save_history(&history)?;

            let fingerprint = fingerprint_history(&history);
            let img = render_page(fonts, &history, self.sys.as_ref());
            Ok((fingerprint, img))
        })
    }
}

fn load_history() -> Vec<Sample> {
    let Ok(text) = std::fs::read_to_string(HISTORY_FILE) else {
        return Vec::new();
    };
    let mut out: Vec<Sample> = text
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some(Sample {
                ts: parts.next()?.parse().ok()?,
                mv: parts.next()?.parse().ok()?,
                temp_c: parts.next().and_then(|t| t.parse().ok()).unwrap_or(0.0),
            })
        })
        .collect();
    out.sort_by_key(|s| s.ts);
    out
}

fn save_history(history: &[Sample]) -> Result<()> {
    let text: String = history.iter().map(|s| format!("{} {} {:.1}\n", s.ts, s.mv, s.temp_c)).collect();
    std::fs::write(HISTORY_FILE, text).context("writing battery history")
}

fn fingerprint_history(history: &[Sample]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for s in history {
        s.ts.hash(&mut hasher);
        s.mv.hash(&mut hasher);
    }
    hasher.finish()
}

fn soc_percent(mv: u16) -> Option<f32> {
    SocEstimator::new(BatteryChemistry::LiIon).estimate_soc(mv as f32 / 1000.0).ok()
}

/// Least-squares SOC slope over the trailing `ETA_FIT_WINDOW`, extrapolated
/// to 0% -- `None` unless there are enough samples spanning enough time for
/// the fit to mean anything (a two-point fit over 30 minutes is noise). A
/// non-negative slope reports `Charging` rather than an infinite ETA.
enum Eta {
    DaysLeft(f32),
    Charging,
}

fn estimate_eta(history: &[Sample]) -> Option<Eta> {
    let now = history.last()?.ts;
    let pts: Vec<(f32, f32)> = history
        .iter()
        .filter(|s| now - s.ts <= ETA_FIT_WINDOW.as_secs() as i64)
        .filter_map(|s| Some(((s.ts - now) as f32 / 3600.0, soc_percent(s.mv)?)))
        .collect();
    let span_h = pts.last()?.0 - pts.first()?.0;
    if pts.len() < 6 || span_h < 12.0 {
        return None;
    }
    let n = pts.len() as f32;
    let (sx, sy): (f32, f32) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (sxx, sxy): (f32, f32) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x * x, b + x * y));
    let denom = n * sxx - sx * sx;
    if denom.abs() < f32::EPSILON {
        return None;
    }
    let slope_per_hour = (n * sxy - sx * sy) / denom; // % per hour
    let current_soc = pts.last()?.1;
    if slope_per_hour >= -0.001 {
        return Some(Eta::Charging);
    }
    Some(Eta::DaysLeft((current_soc / -slope_per_hour) / 24.0))
}

fn render_page(fonts: &Fonts, history: &[Sample], sys: Option<&SysInfo>) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM DISPLAY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 40.0, 26.0, 76.0, "BATTERY", BLACK);

    // Headline: big SOC percentage top-right, voltage + temperature + ETA in
    // the summary line under the title -- same layout grammar as the weather
    // page's current-conditions corner.
    if let Some(last) = history.last() {
        let volts = last.mv as f32 / 1000.0;
        let soc = soc_percent(last.mv);

        let pct_text = soc.map_or("--%".to_string(), |p| format!("{p:.0}%"));
        let pw = text_width(&fonts.sans_black, 44.0, &pct_text);
        render::draw_text(&mut img, &fonts.sans_black, 44.0, W as f32 - 26.0 - pw, 56.0, &pct_text, BLACK);
        let sub = format!("{volts:.2}V \u{b7} {:.0}C", last.temp_c);
        let sw = text_width(&fonts.sans_bold, 14.0, &sub);
        render::draw_text(&mut img, &fonts.sans_bold, 14.0, W as f32 - 26.0 - sw, 78.0, &sub, DARK_GRAY);

        let eta_text = match estimate_eta(history) {
            Some(Eta::DaysLeft(d)) if d >= 1.0 => format!("  \u{b7}  EST. {d:.0} DAYS LEFT"),
            Some(Eta::DaysLeft(d)) => format!("  \u{b7}  EST. {:.0} HOURS LEFT", d * 24.0),
            Some(Eta::Charging) => "  \u{b7}  CHARGING".to_string(),
            None => String::new(),
        };
        let n = history.len();
        let summary = format!("{n} READING{} OVER 7 DAYS{eta_text}", if n == 1 { "" } else { "S" });
        render::draw_text(&mut img, &fonts.sans_bold, 15.0, 26.0, 98.0, &summary, DARK_GRAY);
    }

    draw_line_segment_mut(&mut img, (0.0, 118.0), (W as f32, 118.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 119.0), (W as f32, 119.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 120.0), (W as f32, 120.0), Luma([BLACK]));

    draw_voltage_chart(&mut img, fonts, history);
    draw_sys_strip(&mut img, fonts, sys);

    plugin::draw_status_bar(&mut img, fonts, SLOT, &render::current_time_utc_hhmm(), STATUS_LABEL);
    img
}

/// Hardware/power facts stated as constants because they ARE constant for
/// this one provisioned device -- they mirror the device config
/// (firmware repo, tools/egham_display_config.yaml: manufacturer_id 1 =
/// Seeed, ic_type 2 = ESP32-S3, power_mode 1 = battery). Update alongside
/// that file if the hardware ever changes.
const HARDWARE_LABEL: &str = "SEEED XIAO ESP32-S3";
const POWER_LABEL: &str = "BATTERY POWER";

/// System line above the status bar: firmware version and hardware on the
/// left, BLE signal strength (wifi-style bars + dBm) on the right -- the
/// bars answer "is the link getting marginal" at a glance, the dBm number
/// is there for actually comparing positions.
fn draw_sys_strip(img: &mut GrayImage, fonts: &Fonts, sys: Option<&SysInfo>) {
    let y = 436.0; // text baseline, just above the status bar's rules at 448
    let Some(sys) = sys else {
        render::draw_text(img, &fonts.sans_bold, 13.0, 26.0, y, "SYSTEM INFO PENDING NEXT READING", LIGHT_GRAY);
        return;
    };

    let left = format!("FW {}  \u{b7}  {HARDWARE_LABEL}  \u{b7}  {POWER_LABEL}", sys.firmware);
    render::draw_text(img, &fonts.sans_bold, 13.0, 26.0, y, &left, DARK_GRAY);

    match sys.rssi_dbm {
        Some(dbm) => {
            let label = format!("{dbm} dBm");
            let lw = text_width(&fonts.mono, 13.0, &label);
            let label_x = W as f32 - 26.0 - lw;
            render::draw_text(img, &fonts.mono, 13.0, label_x, y, &label, DARK_GRAY);
            draw_signal_bars(img, label_x - 8.0 - 22.0, y, dbm);
        }
        None => {
            let label = "SIGNAL ?";
            let lw = text_width(&fonts.mono, 13.0, label);
            render::draw_text(img, &fonts.mono, 13.0, W as f32 - 26.0 - lw, y, label, LIGHT_GRAY);
        }
    }
}

/// Four ascending bars, wifi-icon style. Filled count by RSSI: the
/// thresholds are the usual BLE rules of thumb (>= -60 excellent, -70 good,
/// -80 workable, -90 marginal, below that basically out of range). Unfilled
/// bars still render in light gray so "2 of 4" reads as a fraction, not
/// just two floating dashes.
fn draw_signal_bars(img: &mut GrayImage, x: f32, baseline_y: f32, dbm: i16) {
    let filled = match dbm {
        d if d >= -60 => 4,
        d if d >= -70 => 3,
        d if d >= -80 => 2,
        d if d >= -90 => 1,
        _ => 0,
    };
    let bar_w = 4.0;
    let gap = 2.0;
    for i in 0..4u32 {
        let bar_h = 4.0 + i as f32 * 3.0;
        let bx = x + i as f32 * (bar_w + gap);
        let color = if (i as i32) < filled { BLACK } else { LIGHT_GRAY };
        imageproc::drawing::draw_filled_rect_mut(
            img,
            imageproc::rect::Rect::at(bx as i32, (baseline_y - bar_h) as i32).of_size(bar_w as u32, bar_h as u32),
            Luma([color]),
        );
    }
}

fn draw_voltage_chart(img: &mut GrayImage, fonts: &Fonts, history: &[Sample]) {
    let chart_x0 = 64.0;
    let chart_x1 = W as f32 - 40.0;
    let chart_top = 150.0;
    let baseline = 390.0;
    let chart_h = baseline - chart_top;

    render::draw_text(img, &fonts.sans_bold, 13.0, chart_x0, chart_top - 10.0, "VOLTS", DARK_GRAY);

    // Gridlines every 0.15V from cutoff to full -- the axis is the battery's
    // real usable range (see V_MIN/V_MAX), so "line near the bottom" always
    // means "nearly empty" without reading a single label.
    let mut level = V_MIN;
    while level <= V_MAX + 0.001 {
        let y = baseline - chart_h * ((level - V_MIN) / (V_MAX - V_MIN));
        if (level - V_MIN).abs() > 0.001 {
            for x in (chart_x0 as i32..chart_x1 as i32).step_by(6) {
                img.put_pixel(x as u32, y as u32, Luma([LIGHT_GRAY]));
            }
        }
        let label = format!("{level:.2}");
        let lw = text_width(&fonts.mono, 11.0, &label);
        render::draw_text(img, &fonts.mono, 11.0, chart_x0 - lw - 8.0, y + 4.0, &label, DARK_GRAY);
        level += 0.15;
    }
    draw_line_segment_mut(img, (chart_x0, baseline), (chart_x1, baseline), Luma([BLACK]));

    if history.is_empty() {
        render::draw_text(img, &fonts.sans_bold, 15.0, chart_x0, chart_top + 40.0, "No readings yet", LIGHT_GRAY);
        return;
    }

    // X = time across the fixed 7-day window ending at the newest sample --
    // fixed, not fit-to-data, so the line visibly grows rightward as
    // readings accumulate and a gap (device unreachable for a while) shows
    // as a real gap in coverage rather than being stretched away.
    let t_end = history.last().expect("checked non-empty above").ts;
    let t_start = t_end - WINDOW.as_secs() as i64;
    let x_at = |ts: i64| chart_x0 + (chart_x1 - chart_x0) * ((ts - t_start) as f32 / WINDOW.as_secs() as f32);
    let y_at = |mv: u16| {
        let v = (mv as f32 / 1000.0).clamp(V_MIN, V_MAX);
        baseline - chart_h * ((v - V_MIN) / (V_MAX - V_MIN))
    };

    // Day boundaries as x-axis ticks: each midnight in the window, labelled
    // with its weekday.
    let mut day = Local.timestamp_opt(t_start, 0).single().map(|d| d.date_naive());
    while let Some(d) = day {
        let next = d.succ_opt();
        if let Some(midnight) = next.and_then(|n| n.and_hms_opt(0, 0, 0)) {
            let ts = Local.from_local_datetime(&midnight).single().map(|m: DateTime<Local>| m.timestamp());
            match ts {
                Some(ts) if ts <= t_end => {
                    let x = x_at(ts);
                    draw_line_segment_mut(img, (x, baseline), (x, baseline + 4.0), Luma([BLACK]));
                    let label = midnight.format("%a").to_string().to_uppercase();
                    let lw = text_width(&fonts.mono, 12.0, &label);
                    render::draw_text(img, &fonts.mono, 12.0, x - lw / 2.0, baseline + 18.0, &label, DARK_GRAY);
                    day = next;
                }
                _ => break,
            }
        } else {
            break;
        }
    }

    let points: Vec<(f32, f32)> = history.iter().map(|s| (x_at(s.ts), y_at(s.mv))).collect();
    chart::draw_series(img, &points, &LineStyle::Solid, &Marker::Dot, BLACK);
}
