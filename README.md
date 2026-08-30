# OpenDisplay Pages

A Rust BLE client that renders pages and pushes them to an
[OpenDisplay](https://opendisplay.org/) e-paper panel — built plugin-style so
each page (live train times, an all-day weather forecast, an auto-generated
index of what's on which slot) is fetched, rendered, and pushed independently,
only when its own content actually changed.

It targets a fork of OpenDisplay's firmware
([`gavinmcnair/Firmware`](https://github.com/gavinmcnair/Firmware), branch
`feat/pipe-slot-write`) that adds **PSRAM slot storage**: several pre-rendered
pages held in fixed on-device slots, switched between locally by the panel's
own buttons with no BLE round trip at switch time. See that repo's
[`docs/pipe-write-protocol.md` §10](https://github.com/gavinmcnair/Firmware/blob/feat/pipe-slot-write/docs/pipe-write-protocol.md#10-psram-slot-storage-pipe_flag_slot_target--local-fork-divergence)
for the wire protocol this client speaks — this is a **local fork
divergence**, not part of the canonical OpenDisplay protocol upstream.

## Architecture

Three layers, each only aware of the one below it:

1. **Fundamentals** (`render.rs`, `ble.rs`, `protocol.rs`, `state.rs`) — font
   loading and antialiased text, the 4-gray quantization/packing pipeline, the
   PIPE_WRITE BLE transfer (negotiate → stream → end), and per-slot
   change-detection state on disk. Nothing here knows what a "page" is.
2. **Plugin framework** (`plugin.rs`) — the `Plugin` trait every page
   implements (`slot()`, `name()`, `render()`), and `draw_status_bar()`, the
   one piece of chrome every page gets for free: three rules, then `UPDATED
   <time>` bottom-left, a page-chosen label centered bottom-middle, `SLOT <n>`
   bottom-right. Quantization to the panel's 4 real gray levels happens here,
   last, after a page's own content is drawn at full antialiased precision.
3. **Plugins** (`plugins/`) — the actual pages. Each renders its own content,
   then calls `draw_status_bar` as its last step.

| Plugin | Slot | Source | Status label |
|---|---|---|---|
| `plugins::index` | 0 | built from every other plugin's `(slot, name)` | `PRESS KEY1 / KEY2 TO BROWSE` |
| `plugins::trains` | 1 | [Realtime Trains](https://api.rtt.io/) | `REALTIME TRAINS` |
| `plugins::weather` | 2 | [Open-Meteo](https://open-meteo.com/) | `ALL-DAY RAIN FORECAST` |

The index is itself a `Plugin`, not a special case — it's built from
`plugins.iter().map(|p| (p.slot(), p.name()))` in `main.rs`, so it can never
drift out of sync with what's actually registered. Slot 0 is reserved for it;
adding a page is one more line in `main.rs`'s plugin `Vec`.

## Running

```bash
cargo build --release
./target/release/egham_ble            # continuous: poll every 5 min, push on change
./target/release/egham_ble --once     # one pass, then exit
```

Escape hatches for previewing a render without touching the device or the
on-disk change-detection state:

```bash
egham_ble --render-only  out.png      # trains page
egham_ble --render-weather out.png    # weather page
egham_ble --render-index out.png      # index page
egham_ble --compress-test             # zlib ratio on synthetic + a real render
```

Per-slot change detection lives in `egham_state_slot<N>.txt` next to the
binary (gitignored) — a page is only re-pushed to the device when its content
fingerprint actually changes, so e.g. the trains page's 5-minute poll doesn't
re-transfer an unchanged board.

## Adding a page

Implement `Plugin` in a new `plugins/<name>.rs`, register it in `plugins/mod.rs`,
and add `Box::new(<Name>Plugin)` to the `Vec` in `main.rs`. Pick an unused
slot number ≥ 1 (0 is the index). The status bar, quantization, and BLE push
are handled by the framework — a new page only needs to render its own
content and finish with `plugin::draw_status_bar(...)`.
