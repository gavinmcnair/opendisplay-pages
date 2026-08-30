//! Google Calendar client (fundamentals, layer 1) -- OAuth2 refresh-token
//! flow plus event fetching. Unlike RTT/Open-Meteo, Google's API needs a
//! genuine user-consent OAuth dance (no public read-only endpoint), so this
//! file also owns the one-time interactive `--calendar-auth` setup flow.
//!
//! Credentials are deliberately NOT hardcoded like RTT's refresh token --
//! this repo is public, and a Google refresh token grants ongoing read
//! access to a real personal calendar, a different sensitivity class from a
//! rate-limited public transit API key. `GOOGLE_CALENDAR_CLIENT_ID`/
//! `_CLIENT_SECRET` come from the environment; the long-life refresh token
//! is written to `calendar_token.txt` next to the binary (gitignored, same
//! pattern as `egham_state_slot*.txt`) and never appears in source or logs.

use anyhow::{bail, Context, Result};
use chrono::{Duration, Local};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const TOKEN_FILE: &str = "calendar_token.txt";

fn client_id() -> Result<String> {
    std::env::var("GOOGLE_CALENDAR_CLIENT_ID").context(
        "GOOGLE_CALENDAR_CLIENT_ID not set -- see README's Google Calendar setup section",
    )
}

fn client_secret() -> Result<String> {
    std::env::var("GOOGLE_CALENDAR_CLIENT_SECRET").context(
        "GOOGLE_CALENDAR_CLIENT_SECRET not set -- see README's Google Calendar setup section",
    )
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// One-time interactive setup (`--calendar-auth`): prints a consent URL,
/// waits for Google's redirect on a loopback listener (the OOB copy-paste
/// flow Google removed for new OAuth clients), exchanges the code for a
/// refresh token, and saves it. Run once per machine; the refresh token
/// outlives individual access tokens (which `fetch_events` mints fresh every
/// call, same pattern as `rtt::mint_access_token`).
pub fn run_oauth_flow() -> Result<()> {
    let client_id = client_id()?;
    let client_secret = client_secret()?;

    let listener = TcpListener::bind("127.0.0.1:0").context("binding loopback OAuth listener")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");

    let auth_url = format!(
        "{AUTH_ENDPOINT}?client_id={client_id}&redirect_uri={redirect_uri}&response_type=code\
         &scope={scope}&access_type=offline&prompt=consent",
        scope = urlencoding_scope(),
    );
    eprintln!("Open this URL, sign in, and grant access:\n\n{auth_url}\n");
    eprintln!("Waiting for the redirect back to {redirect_uri} ...");

    let (stream, _) = listener.accept().context("accepting OAuth redirect connection")?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).context("reading OAuth redirect request")?;
    let code = extract_code(&request_line)
        .ok_or_else(|| anyhow::anyhow!("no ?code= in redirect request line: {request_line:?}"))?;

    let mut stream = stream;
    let body = "<html><body>Signed in -- you can close this tab.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());

    let resp: TokenResponse = ureq::post(TOKEN_ENDPOINT)
        .send_form(&[
            ("code", code.as_str()),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("redirect_uri", &redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .context("exchanging authorization code")?
        .into_json()
        .context("parsing token exchange response")?;

    let refresh_token = resp
        .refresh_token
        .ok_or_else(|| anyhow::anyhow!("no refresh_token in response -- retry with prompt=consent (already set)"))?;
    std::fs::write(TOKEN_FILE, &refresh_token).context("saving refresh token")?;
    eprintln!("Saved refresh token to {TOKEN_FILE}. Calendar fetches will work from now on.");
    Ok(())
}

fn urlencoding_scope() -> String {
    SCOPE.replace(':', "%3A").replace('/', "%2F")
}

/// Pulls `code=...` out of a raw HTTP request line like
/// `GET /?code=4/0Adeu...&scope=... HTTP/1.1`. Deliberately not a general
/// query-string parser -- this only ever reads Google's own redirect.
fn extract_code(request_line: &str) -> Option<String> {
    let path = request_line.split_whitespace().nth(1)?;
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(code) = pair.strip_prefix("code=") {
            return Some(code.to_string());
        }
    }
    None
}

fn mint_access_token() -> Result<String> {
    if !Path::new(TOKEN_FILE).exists() {
        bail!("{TOKEN_FILE} not found -- run `egham_ble --calendar-auth` once to set up Google Calendar access");
    }
    let refresh_token = std::fs::read_to_string(TOKEN_FILE).context("reading refresh token")?;
    let refresh_token = refresh_token.trim();

    let resp: TokenResponse = ureq::post(TOKEN_ENDPOINT)
        .send_form(&[
            ("refresh_token", refresh_token),
            ("client_id", &client_id()?),
            ("client_secret", &client_secret()?),
            ("grant_type", "refresh_token"),
        ])
        .context("refreshing access token")?
        .into_json()
        .context("parsing token refresh response")?;
    Ok(resp.access_token)
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct EventDateTime {
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>, // "2026-08-31T09:00:00+01:00" (timed event)
    pub date: Option<String>, // "2026-08-31" (all-day event)
}

impl EventDateTime {
    /// "YYYY-MM-DD", from whichever field is present.
    pub fn ymd(&self) -> &str {
        let s = self.date_time.as_deref().or(self.date.as_deref()).unwrap_or("");
        if s.len() >= 10 {
            &s[0..10]
        } else {
            s
        }
    }

    /// "HH:MM" for a timed event, `None` for an all-day one.
    pub fn hhmm(&self) -> Option<&str> {
        let s = self.date_time.as_deref()?;
        if s.len() >= 16 {
            Some(&s[11..16])
        } else {
            None
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Event {
    #[serde(default)]
    pub summary: String, // absent entirely for an untitled event
    #[serde(default)]
    pub start: EventDateTime,
    #[serde(default)]
    pub end: EventDateTime,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct EventsResponse {
    #[serde(default)]
    items: Vec<Event>,
}

/// Fetches events on the primary calendar from now through `days_ahead`
/// days out, in Europe/London wall-clock time (the API converts for us, same
/// trick as Open-Meteo's `timezone` param -- see `weather.rs`'s module doc
/// for why that's preferable to a bundled timezone database here).
pub fn fetch_events(days_ahead: i64) -> Result<Vec<Event>> {
    let access_token = mint_access_token()?;
    let time_min = Local::now().to_rfc3339();
    let time_max = (Local::now() + Duration::days(days_ahead)).to_rfc3339();

    let resp: EventsResponse = ureq::get("https://www.googleapis.com/calendar/v3/calendars/primary/events")
        .set("Authorization", &format!("Bearer {access_token}"))
        .query("timeMin", &time_min)
        .query("timeMax", &time_max)
        .query("singleEvents", "true")
        .query("orderBy", "startTime")
        .query("timeZone", "Europe/London")
        .query("maxResults", "250")
        .call()
        .context("fetching calendar events")?
        .into_json()
        .context("parsing calendar events response")?;
    Ok(resp.items)
}

/// Fingerprints the meaningful fields (title, start, end) of every fetched
/// event -- excludes nothing else Google returns (etags, ids, creator info,
/// ...) since none of it is ever rendered.
pub fn fingerprint(events: &[Event]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for e in events {
        e.summary.hash(&mut hasher);
        e.start.date_time.hash(&mut hasher);
        e.start.date.hash(&mut hasher);
        e.end.date_time.hash(&mut hasher);
        e.end.date.hash(&mut hasher);
    }
    hasher.finish()
}
