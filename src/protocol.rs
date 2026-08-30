//! PIPE_WRITE wire protocol (opcodes 0x0080-0x0082), per
//! OpenDisplay/Firmware's doc/pipe-write-protocol.md.
//!
//! Only implements the slot-target variant (PIPE_FLAG_SLOT_TARGET, LOCAL
//! FORK DIVERGENCE) -- every push in this program targets an on-device PSRAM
//! slot, never the live panel directly, so there's no plain full-frame START
//! builder here. Partial refresh is unsupported on our 4-gray panel and
//! encryption is deliberately off, so neither is implemented either.

pub const SERVICE_CHAR_UUID: &str = "00002446-0000-1000-8000-00805f9b34fb";

pub const OP_PIPE_START: [u8; 2] = [0x00, 0x80];
pub const OP_PIPE_DATA: [u8; 2] = [0x00, 0x81];
pub const OP_PIPE_END: [u8; 2] = [0x00, 0x82];
/// CMD_SLOT_SWITCH (0x0084) -- LOCAL FORK DIVERGENCE, not upstream. The BLE
/// front door onto the same on-device switch a button press triggers; see
/// OpenDisplay/Firmware's opendisplay_protocol.h CHANGELOG.
pub const OP_SLOT_SWITCH: [u8; 2] = [0x00, 0x84];

pub const PIPE_VERSION: u8 = 0x01;
pub const PIPE_FLAG_COMPRESSED: u8 = 0x01;
/// LOCAL FORK DIVERGENCE (PSRAM slot storage), not upstream
/// opendisplay-protocol -- see OpenDisplay/Firmware's opendisplay_protocol.h
/// CHANGELOG. Writes into an on-device PSRAM slot instead of the live panel.
pub const PIPE_FLAG_SLOT_TARGET: u8 = 0x04;

/// Frame overhead for a plaintext (unencrypted) DATA frame: 2-byte opcode
/// (stripped before payload accounting) + 1-byte seq.
pub const PIPE_FRAME_OVERHEAD: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct PipeParams {
    pub window: u8,
    pub ack_every: u8,
    pub max_frame: u16,
    pub selective: bool,
    pub compressed: bool,
}

/// Build the PIPE_WRITE_START (0x0080) request body targeting on-device
/// PSRAM slot `slot_id` instead of the live panel (LOCAL FORK DIVERGENCE,
/// PIPE_FLAG_SLOT_TARGET). Always compressed -- slots are compressed-at-rest
/// by design, there's no uncompressed slot-write path.
///
/// `decompressed_size` is an optional parity-check hint firmware uses at
/// switch time (0 = skip it); `compressed_total_size` is the actual byte
/// total on the wire, which must fit that board's per-slot ceiling.
pub fn build_start_slot(
    slot_id: u8,
    decompressed_size: u32,
    req_w: u8,
    req_n: u8,
    client_max_frame: u16,
    compressed_total_size: u32,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + 10 + 6);
    buf.extend_from_slice(&OP_PIPE_START);
    buf.push(PIPE_VERSION);
    buf.push(PIPE_FLAG_COMPRESSED | PIPE_FLAG_SLOT_TARGET);
    buf.push(req_w);
    buf.push(req_n);
    buf.extend_from_slice(&client_max_frame.to_le_bytes());
    buf.extend_from_slice(&compressed_total_size.to_le_bytes());
    buf.push(slot_id);
    buf.push(0); // reserved, must be 0
    buf.extend_from_slice(&decompressed_size.to_le_bytes());
    buf
}

pub enum StartResponse {
    Ack { ver: u8, dev_max_window: u8, dev_max_ack_every: u8, dev_max_frame: u16, flags: u8 },
    Nack { err: u8 },
}

pub fn parse_start_response(data: &[u8]) -> Option<StartResponse> {
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0x80 {
        return Some(StartResponse::Nack { err: data[2] });
    }
    if data.len() >= 8 && data[0] == 0x00 && data[1] == 0x80 {
        return Some(StartResponse::Ack {
            ver: data[2],
            dev_max_window: data[3],
            dev_max_ack_every: data[4],
            dev_max_frame: u16::from_le_bytes([data[5], data[6]]),
            flags: data[7],
        });
    }
    None
}

/// Effective (W, N, frame) via the documented min-rule.
pub fn negotiate_params(
    req_w: u8,
    req_n: u8,
    req_frame: u16,
    dev_max_window: u8,
    dev_max_ack_every: u8,
    dev_max_frame: u16,
    compressed: bool,
) -> PipeParams {
    let w_eff = req_w.min(dev_max_window).min(32).max(1);
    let n_eff = req_n.min(dev_max_ack_every).min(w_eff).max(1);
    let frame_eff = req_frame.min(dev_max_frame);
    PipeParams { window: w_eff, ack_every: n_eff, max_frame: frame_eff, selective: true, compressed }
}

/// Build one PIPE_WRITE_DATA (0x0081) frame carrying `payload` at sequence `seq`.
pub fn build_data_frame(seq: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + 1 + payload.len());
    buf.extend_from_slice(&OP_PIPE_DATA);
    buf.push(seq);
    buf.extend_from_slice(payload);
    buf
}

/// A parsed data-channel ACK/SACK: `00 81 highest_seen mask[0..3]`.
#[derive(Debug, Clone, Copy)]
pub struct DataAck {
    pub highest_seen: u8,
    pub mask: u32,
}

pub enum DataResponse {
    Ack(DataAck),
    Nack { err: u8, ack: DataAck },
}

pub fn parse_data_response(data: &[u8]) -> Option<DataResponse> {
    if data.len() >= 7 && data[0] == 0x00 && data[1] == 0x81 {
        let mask = u32::from_le_bytes([data[3], data[4], data[5], data[6]]);
        return Some(DataResponse::Ack(DataAck { highest_seen: data[2], mask }));
    }
    if data.len() >= 8 && data[0] == 0xFF && data[1] == 0x81 {
        let mask = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        return Some(DataResponse::Nack {
            err: data[2],
            ack: DataAck { highest_seen: data[3], mask },
        });
    }
    None
}

/// "Chunk (highest_seen - 1 - i) was received" for bit i (LSB first, i=0..31).
pub fn ack_has(ack: &DataAck, seq: u8) -> bool {
    let back = ack.highest_seen.wrapping_sub(seq);
    if back == 0 {
        return true; // highest_seen itself is implicitly acknowledged
    }
    if back > 32 {
        return false;
    }
    let bit = back - 1;
    (ack.mask >> bit) & 1 == 1
}

/// Build the PIPE_WRITE_END (0x0082) request. `refresh_mode`: 0 = FULL, 1 = FAST.
pub fn build_end(refresh_mode: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + 5);
    buf.extend_from_slice(&OP_PIPE_END);
    buf.push(refresh_mode);
    buf.extend_from_slice(&0u32.to_be_bytes()); // new_etag = 0 (not using partial/etag)
    buf
}

/// Build the CMD_SLOT_SWITCH (0x0084) request: `[0x00][0x84][slot_id:1]`.
pub fn build_slot_switch(slot_id: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(3);
    buf.extend_from_slice(&OP_SLOT_SWITCH);
    buf.push(slot_id);
    buf
}

pub enum SlotSwitchResponse {
    Ack,
    Nack { err: u8 },
}

pub fn parse_slot_switch_response(data: &[u8]) -> Option<SlotSwitchResponse> {
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0x84 {
        return Some(SlotSwitchResponse::Nack { err: data[2] });
    }
    if data.len() >= 2 && data[0] == 0x00 && data[1] == 0x84 {
        return Some(SlotSwitchResponse::Ack);
    }
    None
}
