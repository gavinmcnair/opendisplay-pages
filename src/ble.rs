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
const OP_TIMEOUT: Duration = Duration::from_secs(60);

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
    let (peripheral, rssi) = loop {
        if let Some(found) = find_by_name(&adapter, device_name).await? {
            let _ = adapter.stop_scan().await;
            break found;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = adapter.stop_scan().await; // don't leave the adapter scanning forever on the failure path
            bail!("device '{device_name}' not found within {SCAN_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };

    tokio::time::timeout(CONNECT_TIMEOUT, peripheral.connect())
        .await
        .map_err(|_| anyhow!("BLE connect timed out after {CONNECT_TIMEOUT:?}"))?
        .context("BLE connect")?;
    tokio::time::timeout(CONNECT_TIMEOUT, peripheral.discover_services())
        .await
        .map_err(|_| anyhow!("GATT service discovery timed out after {CONNECT_TIMEOUT:?}"))?
        .context("discovering GATT services")?;
    Ok((peripheral, rssi))
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
        protocol::build_start_slot(slot_id, payload.len() as u32, 16, 8, 244, compressed.len() as u32);
    tokio::time::timeout(OP_TIMEOUT, run_pipe_write(peripheral, &compressed, &start_req, false))
        .await
        .map_err(|_| anyhow!("slot {slot_id} upload timed out after {OP_TIMEOUT:?}"))?
}

/// Sends CMD_SLOT_SWITCH (0x0084, LOCAL FORK DIVERGENCE) -- the server-driven
/// equivalent of a physical button press: tells the device to display
/// `slot_id` right now, no content transfer involved. Errs on NACK or an
/// unexpected/missing response (including old firmware without this opcode,
/// which won't reply at all -- the caller should treat any error here as
/// "couldn't force the switch this tick, try again next tick" rather than fatal).
pub async fn switch_to_slot(peripheral: &Peripheral, slot_id: u8) -> Result<()> {
    tokio::time::timeout(OP_TIMEOUT, switch_to_slot_inner(peripheral, slot_id))
        .await
        .map_err(|_| anyhow!("slot switch to {slot_id} timed out after {OP_TIMEOUT:?}"))?
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
            protocol::negotiate_params(16, 8, 244, dev_max_window, dev_max_ack_every, dev_max_frame, true)
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
    let window = params.window as usize;

    loop {
        if confirmed as usize >= total {
            break;
        }
        // Send up to `window` frames ahead of the last confirmed point.
        let send_upto = (confirmed as usize + window).min(total);
        for seq in confirmed as usize..send_upto {
            let frame = protocol::build_data_frame(seq as u8, chunks[seq]);
            peripheral.write(&ch, &frame, WriteType::WithoutResponse).await.context("writing DATA frame")?;
        }

        match wait_for_data_ack(&mut notifications).await {
            Ok(DataResponse::Ack(ack)) => {
                // Advance confirmed while consecutive frames from `confirmed` are acked.
                let mut c = confirmed;
                while (c as usize) < total && protocol::ack_has(&ack, c) {
                    c = c.wrapping_add(1);
                    if c == 0 {
                        break; // wrapped past 256, shouldn't happen given the size guard above
                    }
                }
                if c != confirmed {
                    confirmed = c;
                } else {
                    // No progress from this ACK -- retransmit the oldest unacked frame.
                    let frame = protocol::build_data_frame(confirmed, chunks[confirmed as usize]);
                    peripheral.write(&ch, &frame, WriteType::WithoutResponse).await?;
                }
            }
            Ok(DataResponse::Nack { err, .. }) => bail!("PIPE_WRITE_DATA NACKed, err=0x{err:02x}"),
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
    mut parse: impl FnMut(&[u8]) -> Option<T>,
) -> Result<T> {
    let fut = async {
        while let Some(n) = notifications.next().await {
            if let Some(v) = parse(&n.value) {
                return Some(v);
            }
        }
        None
    };
    match tokio::time::timeout(NOTIFY_TIMEOUT, fut).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => bail!("notification stream ended"),
        Err(_) => bail!("timed out waiting for a response"),
    }
}

async fn wait_for_data_ack(
    notifications: &mut (impl StreamExt<Item = btleplug::api::ValueNotification> + Unpin),
) -> Result<DataResponse> {
    wait_for(notifications, |d| protocol::parse_data_response(d)).await
}
