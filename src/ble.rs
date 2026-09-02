//! BLE central: find the device, connect, and drive the PIPE_WRITE upload.

use anyhow::{anyhow, bail, Context, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
use std::io::Write as _;
use std::time::Duration;
use uuid::Uuid;

use crate::protocol::{self, DataResponse, PipeParams, SlotSwitchResponse, StartResponse};

/// The panel's BLE advertised name. Lives here (fundamentals) rather than
/// main.rs because plugins that source their data over BLE (see
/// `plugins::battery`) need it too, not just the orchestrator's push path.
pub const DEVICE_NAME: &str = "ODC48BB0";

const NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);
// DATA-phase ACKs get a much shorter wait than other responses: on a lossy
// link (the Pi at -51dBm, unlike the Mac a metre from the panel) a lost ACK
// notification is common, and the correct reaction is a fast retransmit
// from the confirmed point -- waiting the full NOTIFY_TIMEOUT for an ACK
// that already evaporated just burns the operation budget (observed live,
// 2026-09-02: transfers stalling 30s mid-stream, then dying at OP_TIMEOUT).
const DATA_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
// btleplug's connect()/discover_services() have no timeout of their own --
// observed hanging indefinitely (multi-minute, never returning) after the
// underlying BLE stack was left in a bad state by an interrupted prior
// attempt. Wrapped explicitly so a stuck connect fails loudly instead of
// hanging the whole process.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
// Same reasoning as CONNECT_TIMEOUT, but wrapping the WHOLE upload/switch
// operation rather than each individual await: notifications()/subscribe()/
// write() inside run_pipe_write and switch_to_slot are equally unguarded by
// btleplug, and chasing every call site with its own timeout is exactly the
// kind of complexity this project avoids when one outer bound covers all of
// them at once. 60s is generous even for the largest slot payload's full
// negotiate+stream+end sequence under normal conditions.
// 180s, raised from 60s (2026-09-02): on the Pi's link a big page's
// transfer can legitimately need several retransmit rounds; killing a
// transfer that is making (slow) progress just repeats the same fight from
// zero next tick. The fast DATA_ACK_TIMEOUT above is what keeps a genuinely
// dead transfer from soaking anywhere near this long per stall.
// Requested transfer window / ACK cadence. 8/4, down from 16/8
// (2026-09-02): the panel is a two-chip design (nRF radio -> internal UART
// -> ESP32), and large pages (the ~14KB trains board = 60 frames) stalled
// mid-stream repeatedly at full-rate 16-frame bursts -- from three
// different hosts, at -56dBm near line of sight -- while small pages
// sailed through. Halving the burst gives the inter-chip path breathing
// room at the cost of a slower transfer; robustness beats speed at these
// payload sizes. If stalls persist, drop to 4/2 before blaming anything
// else.
const REQ_WINDOW: u8 = 8;
const REQ_ACK_EVERY: u8 = 4;

const OP_TIMEOUT: Duration = Duration::from_secs(180);

pub async fn find_and_connect(device_name: &str) -> Result<Peripheral> {
    Ok(find_and_connect_with_rssi(device_name).await?.0)
}

/// Like `find_and_connect`, but also reports the advertising RSSI observed
/// while scanning for the device -- an honest "how good is the link to the
/// panel from here" number, captured for free from the discovery pass
/// (btleplug has no cross-platform connected-RSSI read). `None` when the
/// platform didn't attach an RSSI to the advertisement.
pub async fn find_and_connect_with_rssi(device_name: &str) -> Result<(Peripheral, Option<i16>)> {
    let manager = Manager::new().await.context("creating BLE manager")?;
    let adapters = manager.adapters().await.context("listing BLE adapters")?;
    let adapter = adapters.into_iter().next().ok_or_else(|| anyhow!("no BLE adapter found"))?;

    adapter.start_scan(ScanFilter::default()).await.context("starting BLE scan")?;

    let deadline = tokio::time::Instant::now() + SCAN_TIMEOUT;
    // On BlueZ the first sighting of the device is often a cached D-Bus
    // object with rssi=None -- the value only attaches when the next
    // advertising report lands (CoreBluetooth surfaces peripherals ONLY via
    // adverts, so the Mac always had it). The RSSI feeds the status-bar
    // signal icon, so give it a bounded grace period to appear rather than
    // grabbing the stale first hit; still connect if one never shows.
    let mut seen: Option<(Peripheral, Option<i16>)> = None;
    let mut rssi_deadline: Option<tokio::time::Instant> = None;
    let (peripheral, rssi) = loop {
        if let Some((p, r)) = find_by_name(&adapter, device_name).await? {
            if r.is_some() {
                let _ = adapter.stop_scan().await;
                break (p, r);
            }
            seen = Some((p, r)); // keep the handle; wait a moment for an rssi-bearing advert
            let rd = *rssi_deadline.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(2));
            if tokio::time::Instant::now() >= rd {
                let _ = adapter.stop_scan().await;
                break seen.take().expect("just set");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = adapter.stop_scan().await; // don't leave the adapter scanning forever on the failure path
            match seen {
                Some(found) => break found,
                None => bail!("device '{device_name}' not found within {SCAN_TIMEOUT:?}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };

    // Retry the connect: the panel light-sleeps between advertising events
    // (sleep_timeout_ms 30s in its config), and a first LE connection
    // attempt against it often dies with "Connection Failed to be
    // Established (0x3e)" -- measured ~50% single-attempt success from the
    // Pi even with correct radio firmware. macOS hid this by retrying inside
    // CoreBluetooth; BlueZ surfaces every failure, so we retry here.
    //
    // Retry etiquette matters more than count (learned live, 2026-09-02):
    // never issue a cancel/disconnect after a FAILED connect (there is no
    // connection to clean, and the cancel can race the panel's own
    // connection setup and wedge its advertising entirely -- a panel-side
    // firmware fragility, but we don't get to reboot the panel remotely),
    // space attempts a few seconds apart like a human retrying
    // bluetoothctl (measured harmless), and re-find the device fresh each
    // attempt (BlueZ expires its D-Bus object between attempts, turning a
    // cached handle into 'Method "Connect" doesn't exist').
    const CONNECT_ATTEMPTS: u32 = 3;
    let mut last_err = anyhow!("unreachable");
    let mut target = peripheral;
    for attempt in 1..=CONNECT_ATTEMPTS {
        let connected = tokio::time::timeout(CONNECT_TIMEOUT, target.connect())
            .await
            .map_err(|_| anyhow!("BLE connect timed out after {CONNECT_TIMEOUT:?}"))
            .and_then(|r| r.context("BLE connect"));
        match connected {
            Ok(()) => {
                tokio::time::timeout(CONNECT_TIMEOUT, target.discover_services())
                    .await
                    .map_err(|_| anyhow!("GATT service discovery timed out after {CONNECT_TIMEOUT:?}"))?
                    .context("discovering GATT services")?;
                if let Some(dbm) = rssi {
                    crate::state::record_rssi(dbm); // feed the status bar's signal icon on every connect
                }
                return Ok((target, rssi));
            }
            Err(e) => {
                last_err = e;
                if attempt < CONNECT_ATTEMPTS {
                    eprintln!("BLE connect attempt {attempt}/{CONNECT_ATTEMPTS} failed ({last_err:#}); retrying");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    // Fresh lookup -- the previous object may be stale.
                    if let Ok(Some((p, _))) = find_by_name(&adapter, device_name).await {
                        target = p;
                    }
                }
            }
        }
    }
    Err(last_err.context(format!("after {CONNECT_ATTEMPTS} connect attempts")))
}

async fn find_by_name(adapter: &btleplug::platform::Adapter, name: &str) -> Result<Option<(Peripheral, Option<i16>)>> {
    for p in adapter.peripherals().await? {
        if let Ok(Some(props)) = p.properties().await {
            if props.local_name.as_deref() == Some(name) {
                let rssi = props.rssi;
                return Ok(Some((p, rssi)));
            }
        }
    }
    Ok(None)
}

fn find_char(peripheral: &Peripheral, uuid: Uuid) -> Result<btleplug::api::Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == uuid)
        .ok_or_else(|| anyhow!("characteristic {uuid} not found"))
}

/// Firmware runs uzlib compiled with a 9-bit (512-byte) DEFLATE window and
/// rejects zlib headers advertising a larger one -- the standard zlib default
/// of 15 bits (32KB) gets NACKed with err=0x02 on the very first frame.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::{Compress, Compression};
    let compress = Compress::new_with_window_bits(Compression::best(), true, 9);
    let mut enc = ZlibEncoder::new_with_compress(Vec::new(), compress);
    enc.write_all(data).expect("zlib write");
    enc.finish().expect("zlib finish")
}

/// Uploads a raw (already 4-gray, already sized) grayscale buffer into
/// on-device PSRAM slot `slot_id`, instead of the live panel (LOCAL FORK
/// DIVERGENCE -- see OpenDisplay/Firmware's PIPE_FLAG_SLOT_TARGET). Returns
/// once the device ACKs the END; it never waits for a panel refresh here,
/// since firmware only refreshes the panel for a slot-target transfer when
/// that slot happens to be the one currently selected on-device, and does so
/// entirely on its own with no further BLE traffic either way.
pub async fn upload_pipe_write_to_slot(peripheral: &Peripheral, slot_id: u8, payload: &[u8]) -> Result<()> {
    let compressed = zlib_compress(payload);
    eprintln!("Slot {slot_id}: compressed {} bytes -> {} bytes", payload.len(), compressed.len());
    let start_req =
        protocol::build_start_slot(slot_id, payload.len() as u32, REQ_WINDOW, REQ_ACK_EVERY, 244, compressed.len() as u32);
    let result = tokio::time::timeout(OP_TIMEOUT, run_pipe_write(peripheral, &compressed, &start_req, false))
        .await
        .map_err(|_| anyhow!("slot {slot_id} upload timed out after {OP_TIMEOUT:?}"))
        .and_then(|r| r);
    // Disconnect on EVERY exit path, not just success. run_pipe_write's
    // error paths (NACK bails, END-ack timeout, the outer OP_TIMEOUT above)
    // used to leave the connection dangling; macOS's CoreBluetooth cleaned
    // that up implicitly, but BlueZ does not -- the very next tick then hit
    // "writing START: In Progress" against the stale connection, and every
    // retry after it (observed live on the Pi, 2026-09-02).
    if result.is_err() {
        let _ = peripheral.disconnect().await;
    }
    result
}

/// Sends CMD_SLOT_SWITCH (0x0084, LOCAL FORK DIVERGENCE) -- the server-driven
/// equivalent of a physical button press: tells the device to display
/// `slot_id` right now, no content transfer involved. Errs on NACK or an
/// unexpected/missing response (including old firmware without this opcode,
/// which won't reply at all -- the caller should treat any error here as
/// "couldn't force the switch this tick, try again next tick" rather than fatal).
pub async fn switch_to_slot(peripheral: &Peripheral, slot_id: u8) -> Result<()> {
    let result = tokio::time::timeout(OP_TIMEOUT, switch_to_slot_inner(peripheral, slot_id))
        .await
        .map_err(|_| anyhow!("slot switch to {slot_id} timed out after {OP_TIMEOUT:?}"))
        .and_then(|r| r);
    // Same rationale as upload_pipe_write_to_slot: never leave a dangling
    // connection on an error path -- BlueZ (unlike CoreBluetooth) won't
    // clean it up, and the next operation fails with "In Progress".
    if result.is_err() {
        let _ = peripheral.disconnect().await;
    }
    result
}

async fn switch_to_slot_inner(peripheral: &Peripheral, slot_id: u8) -> Result<()> {
    let char_uuid = Uuid::parse_str(protocol::SERVICE_CHAR_UUID)?;
    let ch = find_char(peripheral, char_uuid)?;
    let mut notifications = peripheral.notifications().await.context("getting notification stream")?;
    peripheral.subscribe(&ch).await.context("subscribing to notifications")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let req = protocol::build_slot_switch(slot_id);
    peripheral.write(&ch, &req, WriteType::WithResponse).await.context("writing SLOT_SWITCH")?;

    let resp = wait_for(&mut notifications, |d| protocol::parse_slot_switch_response(d)).await;
    let _ = peripheral.disconnect().await; // same as run_pipe_write: free the connection promptly so the next scan finds the device again
    match resp? {
        SlotSwitchResponse::Ack => Ok(()),
        SlotSwitchResponse::Nack { err } => bail!("SLOT_SWITCH NACKed, err=0x{err:02x}"),
    }
}

/// Live device telemetry gathered over one connection: the CMD_READ_MSD
/// (0x0044) buffer -- the same 16 bytes the device broadcasts as
/// manufacturer-specific advertising data, hence "MSD" -- plus the firmware
/// version (CMD_FIRMWARE_VERSION, 0x43). Field layout per the firmware's
/// `updatemsdata()` (display_service.cpp) and tools/od-device-cli.py's
/// `decode_msd_payload`.
#[derive(Debug, Clone)]
pub struct Telemetry {
    /// Battery voltage in millivolts. `None` when the device reports raw 0 --
    /// battery sense unconfigured or not yet sampled, never a real 0mV.
    pub battery_mv: Option<u16>,
    pub temperature_c: f32,
    /// "v<major>.<minor>.<patch> <short-sha>" per the 0x43 response (patch
    /// byte optional on older firmware, treated as 0).
    pub firmware: String,
}

/// Reads firmware version (0x43) then live MSD telemetry (0x44) over the
/// already-connected `peripheral` -- one subscribe covers both commands.
/// Does NOT disconnect; the caller owns the connection's lifetime.
///
/// MSD battery value is 9 bits in 10mV units: bit 0 of the status byte
/// (payload[15]) is the high bit over payload[14].
pub async fn read_telemetry(peripheral: &Peripheral) -> Result<Telemetry> {
    tokio::time::timeout(OP_TIMEOUT, read_telemetry_inner(peripheral))
        .await
        .map_err(|_| anyhow!("telemetry read timed out after {OP_TIMEOUT:?}"))?
}

async fn read_telemetry_inner(peripheral: &Peripheral) -> Result<Telemetry> {
    let char_uuid = Uuid::parse_str(protocol::SERVICE_CHAR_UUID)?;
    let ch = find_char(peripheral, char_uuid)?;
    let mut notifications = peripheral.notifications().await.context("getting notification stream")?;
    peripheral.subscribe(&ch).await.context("subscribing to notifications")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- firmware version (0x43): [0x00][0x43][major][minor][shaLen][sha..][patch?] ---
    peripheral.write(&ch, &[0x00, 0x43], WriteType::WithResponse).await.context("writing FIRMWARE_VERSION")?;
    let firmware = wait_for(&mut notifications, |d| {
        if d.len() >= 5 && d[0] == 0x00 && d[1] == 0x43 {
            let (major, minor, sha_len) = (d[2], d[3], d[4] as usize);
            let sha = d.get(5..5 + sha_len).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
            let patch = d.get(5 + sha_len).copied().unwrap_or(0); // older firmware omits it
            let mut short_sha: String = sha.chars().take(7).collect();
            if short_sha.chars().all(|c| c == '0') {
                short_sha.clear(); // an all-zero SHA is a build placeholder, not information
            }
            return Some(if short_sha.is_empty() {
                format!("v{major}.{minor}.{patch}")
            } else {
                format!("v{major}.{minor}.{patch} {short_sha}")
            });
        }
        None
    })
    .await?;

    // --- MSD (0x44): [0x00][0x44] + 16 bytes ---
    peripheral.write(&ch, &[0x00, 0x44], WriteType::WithResponse).await.context("writing READ_MSD")?;
    enum MsdResp {
        Data([u8; 16]),
        Err(u8),
    }
    let resp = wait_for(&mut notifications, |d| {
        if d.len() >= 18 && d[0] == 0x00 && d[1] == 0x44 {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&d[2..18]);
            return Some(MsdResp::Data(buf));
        }
        // Short [0x00][0x44][0xFE|0xFF] = firmware-reported read error;
        // [0xFE][0x44] = auth required (security enabled, no session).
        if d.len() == 3 && d[0] == 0x00 && d[1] == 0x44 {
            return Some(MsdResp::Err(d[2]));
        }
        if d.len() >= 2 && d[0] == 0xFE && d[1] == 0x44 {
            return Some(MsdResp::Err(0xFE));
        }
        None
    })
    .await?;

    match resp {
        MsdResp::Data(payload) => {
            let status = payload[15];
            let raw_10mv = (((status & 0x01) as u16) << 8) | payload[14] as u16;
            Ok(Telemetry {
                battery_mv: if raw_10mv > 0 { Some(raw_10mv * 10) } else { None },
                temperature_c: (payload[13] as f32 / 2.0) - 40.0,
                firmware,
            })
        }
        MsdResp::Err(code) => bail!("READ_MSD failed, code=0x{code:02x}"),
    }
}

/// Shared negotiate/send/end sequence for both destinations above. The only
/// differences between a panel upload and a slot upload are the START
/// request body (built by the caller) and whether a panel refresh response
/// follows the END ACK -- firmware only sends one for the former.
async fn run_pipe_write(
    peripheral: &Peripheral,
    compressed: &[u8],
    start_req: &[u8],
    wait_for_refresh: bool,
) -> Result<()> {
    let char_uuid = Uuid::parse_str(protocol::SERVICE_CHAR_UUID)?;
    let ch = find_char(peripheral, char_uuid)?;
    let mut notifications = peripheral.notifications().await.context("getting notification stream")?;
    peripheral.subscribe(&ch).await.context("subscribing to notifications")?;
    // Let the CCCD subscription settle on the device side before writing --
    // the device is asleep between transfers and takes a moment to wake.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- START ---
    peripheral.write(&ch, start_req, WriteType::WithResponse).await.context("writing START")?;

    let resp = wait_for(&mut notifications, |d| protocol::parse_start_response(d)).await?;
    let params: PipeParams = match resp {
        StartResponse::Nack { err } => bail!("PIPE_WRITE_START NACKed, err=0x{err:02x}"),
        StartResponse::Ack { ver: _, dev_max_window, dev_max_ack_every, dev_max_frame, flags: _ } => {
            protocol::negotiate_params(REQ_WINDOW, REQ_ACK_EVERY, 244, dev_max_window, dev_max_ack_every, dev_max_frame, true)
        }
    };
    eprintln!(
        "PIPE_WRITE negotiated: W={} N={} frame={} compressed={}",
        params.window, params.ack_every, params.max_frame, params.compressed
    );

    // --- DATA ---
    let chunk_size = params.max_frame as usize - protocol::PIPE_FRAME_OVERHEAD;
    let chunks: Vec<&[u8]> = compressed.chunks(chunk_size.max(1)).collect();
    let total = chunks.len();
    if total == 0 {
        bail!("nothing to send");
    }
    if total > 255 {
        // 255, not 256: `confirmed` below is a u8 *count* of confirmed
        // frames, so exactly 256 frames would make its exit condition
        // (`confirmed as usize >= total`) unsatisfiable -- an infinite
        // resend loop, not a clean failure.
        bail!("payload needs {total} frames, exceeds the 255-frame limit this simple sender supports");
    }

    let mut confirmed: u8 = 0; // number of chunks (0..total) confirmed contiguous from the start
    let mut next_to_send: usize = 0; // every chunk below this has been transmitted at least once
    let mut stale_rounds: u32 = 0; // consecutive no-progress ACK reads (see retransmit gating below)
    let window = params.window as usize;

    loop {
        if confirmed as usize >= total {
            break;
        }
        // Send frames up to `window` ahead of the confirmed point -- but
        // each frame ONCE (tracked by `next_to_send`), never the whole
        // window again per iteration. The previous version re-sent
        // confirmed..confirmed+window every loop pass: on a slow link those
        // duplicates pile into the host's TX queue AHEAD of genuinely new
        // frames, so the device spends whole seconds receiving stale
        // duplicates before anything new arrives -- measured live
        // (2026-09-02, instrumented run): ~25 no-progress rounds between
        // every 4-frame advance, ~30s per window, large pages timing out.
        // Retransmission of lost frames happens ONLY in the no-progress
        // branch below, one frame at a time, on actual evidence of loss.
        let send_upto = (confirmed as usize + window).min(total);
        while next_to_send < send_upto {
            let frame = protocol::build_data_frame(next_to_send as u8, chunks[next_to_send]);
            peripheral.write(&ch, &frame, WriteType::WithoutResponse).await.context("writing DATA frame")?;
            next_to_send += 1;
        }

        match wait_for_data_ack(&mut notifications).await {
            Ok(first) => {
                // CRITICAL: drain every SACK already queued and act on the
                // NEWEST one, not the first. The stream buffers every
                // notification; consuming one per loop iteration on a slow
                // link means deciding from a SACK many frames old, "seeing
                // no progress", and retransmitting a frame the device
                // already has -- which elicits another SACK, sustaining the
                // loop forever. Observed live (2026-09-02, HCI trace): the
                // sender pinned on seq 0x0c at one retransmit per
                // connection interval while every reply said
                // highest_seen=19 "I have everything". Big pages (deeper
                // backlog) never finished; macOS's fast intervals kept the
                // queue shallow, which is why the Mac masked this.
                let mut newest = first;
                use futures::FutureExt as _;
                while let Some(Some(n)) = notifications.next().now_or_never() {
                    if let Some(r) = protocol::parse_data_response(&n.value) {
                        newest = r;
                    }
                }
                match newest {
                    DataResponse::Ack(ack) => {
                        // Advance confirmed while consecutive frames from `confirmed` are acked.
                        let mut c = confirmed;
                        while (c as usize) < total && protocol::ack_has(&ack, c) {
                            c = c.wrapping_add(1);
                            if c == 0 {
                                break; // wrapped past 256, shouldn't happen given the size guard above
                            }
                        }
                        eprintln!(
                            "  data: confirmed {confirmed}->{c}/{total} (ack highest={} mask={:#010x})",
                            ack.highest_seen, ack.mask
                        );
                        if c != confirmed {
                            confirmed = c;
                            stale_rounds = 0;
                        } else {
                            // No progress from the newest ACK. Don't retransmit
                            // on the FIRST stale read: the device SACKs only
                            // every Nth new frame, and it also re-SACKs its
                            // current state when it receives a duplicate -- so
                            // an eager retransmit here begets another stale
                            // ACK, sustaining a mini-loop of duplicates until
                            // the genuinely new SACK surfaces (measured: ~3-8
                            // wasted rounds per window). Retransmit only after
                            // two consecutive stale reads, which distinguishes
                            // "the next SACK is still in flight" from "a frame
                            // was actually lost".
                            stale_rounds += 1;
                            if stale_rounds >= 2 {
                                let frame = protocol::build_data_frame(confirmed, chunks[confirmed as usize]);
                                peripheral.write(&ch, &frame, WriteType::WithoutResponse).await?;
                                stale_rounds = 0;
                            }
                        }
                    }
                    DataResponse::Nack { err, .. } => bail!("PIPE_WRITE_DATA NACKed, err=0x{err:02x}"),
                }
            }
            Err(e) => {
                eprintln!("No ACK progress ({e}); retransmitting from confirmed point");
                let frame = protocol::build_data_frame(confirmed, chunks[confirmed as usize]);
                peripheral.write(&ch, &frame, WriteType::WithoutResponse).await?;
            }
        }
    }

    // --- END (compressed transfers never auto-complete) ---
    let end_req = protocol::build_end(0); // 0 = FULL refresh; ignored by firmware for slot-target transfers
    peripheral.write(&ch, &end_req, WriteType::WithResponse).await.context("writing END")?;

    // Tail-flush SACK, then 00 82 (finalize).
    let _ = wait_for(&mut notifications, |d| {
        if d.len() >= 2 && d[0] == 0x00 && d[1] == 0x82 {
            Some(())
        } else {
            None
        }
    })
    .await?;

    if !wait_for_refresh {
        eprintln!("END acked (slot write, no refresh to wait for).");
        let _ = peripheral.disconnect().await;
        return Ok(());
    }

    eprintln!("END acked, waiting for refresh...");
    let refresh_result = wait_for(&mut notifications, |d| {
        if d.len() >= 2 && d[0] == 0x00 && (d[1] == 0x73 || d[1] == 0x74) {
            Some(d[1])
        } else {
            None
        }
    })
    .await?;
    let _ = peripheral.disconnect().await;
    if refresh_result == 0x74 {
        bail!("device reported a refresh timeout (0x74)");
    }
    eprintln!("Refresh complete.");

    Ok(())
}

async fn wait_for<T>(
    notifications: &mut (impl StreamExt<Item = btleplug::api::ValueNotification> + Unpin),
    parse: impl FnMut(&[u8]) -> Option<T>,
) -> Result<T> {
    wait_for_within(notifications, parse, NOTIFY_TIMEOUT).await
}

async fn wait_for_within<T>(
    notifications: &mut (impl StreamExt<Item = btleplug::api::ValueNotification> + Unpin),
    mut parse: impl FnMut(&[u8]) -> Option<T>,
    timeout: Duration,
) -> Result<T> {
    let fut = async {
        while let Some(n) = notifications.next().await {
            if let Some(v) = parse(&n.value) {
                return Some(v);
            }
        }
        None
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => bail!("notification stream ended"),
        Err(_) => bail!("timed out waiting for a response"),
    }
}

/// Short timeout on purpose (`DATA_ACK_TIMEOUT`, not `NOTIFY_TIMEOUT`) --
/// the caller's error path retransmits from the confirmed point, which is
/// the right response to a lost ACK and needs to happen quickly.
async fn wait_for_data_ack(
    notifications: &mut (impl StreamExt<Item = btleplug::api::ValueNotification> + Unpin),
) -> Result<DataResponse> {
    wait_for_within(notifications, |d| protocol::parse_data_response(d), DATA_ACK_TIMEOUT).await
}
