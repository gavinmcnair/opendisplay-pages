//! Client for Gavin's own self-hosted traintimes service (Darwin via Kafka,
//! see `client/traintimes_client.py` in the `traintimes` repo for the
//! reference implementation this mirrors) -- replaces the old direct
//! `data.rtt.io` client. RTT's license terms capped how often results could
//! be polled/displayed; this is Gavin's own infrastructure, so no such limit
//! applies and `plugins::trains` polls it far more often (see that module's
//! `poll_interval`).

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

const STATION: &str = "EGH";
const DEFAULT_BASE_URL: &str = "https://trainapi.41rpa.uk";

/// Overridable for local testing against a service run on a different host
/// (see that repo's README) -- same pattern as `calendar.rs`'s env-var
/// credentials, read fresh by the caller rather than plumbed through the
/// orchestrator.
fn base_url() -> String {
    std::env::var("TRAINTIMES_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

/// One `StationCall` -- see that repo's `API.md` for the full field-by-field
/// rationale. Only the fields this plugin actually renders are modeled here;
/// serde ignores the rest (`ssd`, `origin`/`destination` TIPLOCs,
/// `direction_index`, `total_stops`, `platform_confirmed`,
/// `platform_changed`, `last_updated`) rather than carrying fields nothing
/// reads.
#[derive(Deserialize, Debug, Clone)]
pub struct StationCall {
    pub rid: String,
    pub origin_name: Option<String>,
    pub destination_name: Option<String>,
    /// Real absolute UTC instants ("...Z"), not the bare local-time strings
    /// Darwin itself sends -- see API.md's epoch-normalization note. `null`
    /// means not yet published, never a fallback to a display string.
    pub scheduled: Option<String>,
    pub estimated: Option<String>,
    pub actual: Option<String>,
    pub delay_minutes: Option<i32>,
    pub platform: Option<String>,
    /// `Pending|Cancelled|Departed|Arrived` -- describes this call only, and
    /// is now authoritative for cancellation (replacing the old raw
    /// `cancelled: bool` RTT/self-built heuristic).
    pub stop_status: String,
}

/// API timestamps are UTC ISO8601 with a `Z` suffix -- mirrors
/// `traintimes_client.py`'s `parse_utc`/`to_local`. Returns `None` for a
/// malformed or absent string rather than erroring: a still-unpublished
/// event (`null` upstream) should degrade to "unknown", not abort the whole
/// render.
fn parse_local(iso: &Option<String>) -> Option<DateTime<Local>> {
    let iso = iso.as_deref()?;
    let utc = DateTime::parse_from_rfc3339(iso).ok()?.with_timezone(&Utc);
    Some(utc.with_timezone(&Local))
}

pub fn scheduled_local(c: &StationCall) -> Option<DateTime<Local>> {
    parse_local(&c.scheduled)
}

pub fn estimated_local(c: &StationCall) -> Option<DateTime<Local>> {
    parse_local(&c.estimated)
}

pub fn actual_local(c: &StationCall) -> Option<DateTime<Local>> {
    parse_local(&c.actual)
}

pub fn is_cancelled(c: &StationCall) -> bool {
    c.stop_status == "Cancelled"
}

/// Hashes the *meaningful* content of one call -- same role as the old
/// `rtt::hash_departure`, used by `plugins::trains` to fingerprint its
/// filtered, currently-displayed view.
pub(crate) fn hash_call(c: &StationCall, hasher: &mut DefaultHasher) {
    c.rid.hash(hasher);
    c.scheduled.hash(hasher);
    c.estimated.hash(hasher);
    c.actual.hash(hasher);
    c.delay_minutes.hash(hasher);
    c.platform.hash(hasher);
    c.stop_status.hash(hasher);
    c.origin_name.hash(hasher);
    c.destination_name.hash(hasher);
}

/// Departures from Egham heading toward `to` (optionally, additionally
/// via `via`) -- soonest first, already trimmed server-side to what's
/// realistically still catchable (`catchable_only`, see API.md: excludes a
/// call whose most-authoritative known time is more than 5 minutes in the
/// past, using the live estimate so a delayed train stays visible past its
/// original schedule) -- replaces the client-side `has_departed`/wall-clock
/// check the old RTT client needed, since RTT didn't offer this filter
/// server-side.
///
/// Hits `/v1/calls?from={STATION}&to=...&via=...` (2026-08-31: replaced the
/// old separate `/v1/departures?crs=`/`/v1/arrivals?crs=` endpoints, which
/// now 404 outright -- see `client/traintimes_client.py` in that repo,
/// which this mirrors). `to`/`via` are an always-independent AND, resolved
/// server-side by STANOX group (e.g. `to=WAT` matches every Waterloo
/// TIPLOC, not just whichever one CORPUS happens to tag `WAT` onto
/// directly) -- `plugins::trains` calls this once per physical platform
/// with Egham-specific anchors (Waterloo via Richmond one way, Chertsey the
/// other) instead of fetching everything and guessing direction from the
/// `platform` field locally, since that field can be unconfirmed and flip a
/// train between columns for no real reason.
pub fn fetch_departures(to: &str, via: Option<&str>) -> Result<Vec<StationCall>> {
    let mut url = format!("{}/v1/calls?from={STATION}&to={to}&catchable_only=true", base_url());
    if let Some(via) = via {
        url.push_str(&format!("&via={via}"));
    }
    let resp: Vec<StationCall> = ureq::get(&url)
        .call()
        .context("fetching traintimes departures")?
        .into_json()
        .context("parsing traintimes departures response")?;
    Ok(resp)
}
