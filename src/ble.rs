//! BLE central: find the device, connect, and drive the PIPE_WRITE upload.

use anyhow::{anyhow, bail, Context, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
use std::io::Write as _;
use std::time::Duration;
use uuid::Uuid;

use crate::protocol::{self, DataResponse, PipeParams, StartResponse};

const NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn find_and_connect(device_name: &str) -> Result<Peripheral> {
    let manager = Manager::new().await.context("creating BLE manager")?;
    let adapters = manager.adapters().await.context("listing BLE adapters")?;
    let adapter = adapters.into_iter().next().ok_or_else(|| anyhow!("no BLE adapter found"))?;

    adapter.start_scan(ScanFilter::default()).await.context("starting BLE scan")?;

    let deadline = tokio::time::Instant::now() + SCAN_TIMEOUT;
    let peripheral = loop {
        if let Some(p) = find_by_name(&adapter, device_name).await? {
            let _ = adapter.stop_scan().await;
            break p;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("device '{device_name}' not found within {SCAN_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };

    peripheral.connect().await.context("BLE connect")?;
    peripheral.discover_services().await.context("discovering GATT services")?;
    Ok(peripheral)
}

async fn find_by_name(adapter: &btleplug::platform::Adapter, name: &str) -> Result<Option<Peripheral>> {
    for p in adapter.peripherals().await? {
        if let Ok(Some(props)) = p.properties().await {
            if props.local_name.as_deref() == Some(name) {
                return Ok(Some(p));
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
    run_pipe_write(peripheral, &compressed, &start_req, false).await
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
    if total > 256 {
        bail!("payload needs {total} frames, exceeds the 8-bit sequence space (256) this simple sender supports");
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
