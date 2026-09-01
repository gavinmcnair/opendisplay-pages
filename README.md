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
| `plugins::stats` | 1 | the panel itself (battery/fw/RSSI over BLE, hourly) | `PANEL SYSTEM STATS` |
| `plugins::trains` | 2 | Gavin's own self-hosted `traintimes` service (Darwin via Kafka) | `TRAINTIMES LIVE` |
| `plugins::weather` | 3 | [Open-Meteo](https://open-meteo.com/) | `ALL-DAY RAIN FORECAST` |
| `plugins::calendar_default` | 4 | Google Calendar (today + tomorrow, agenda) | `GOOGLE CALENDAR` |
| `plugins::calendar_week` | 5 | Google Calendar (7-day grid) | `7-DAY CALENDAR` |
| `plugins::air_quality` | 6 | Open-Meteo Air Quality (AQI/UV + pollen) | `AIR QUALITY & POLLEN` |

The index is itself a `Plugin`, not a special case — it's built from
`plugins.iter().map(|p| (p.slot(), p.name()))` in `main.rs`, so it can never
drift out of sync with what's actually registered. Slot 0 is reserved for it;
adding a page is one more line in `main.rs`'s plugin `Vec`.

## Scheduler

`scheduler.rs` decides which slot *should* be on screen right now, purely
from wall-clock time (`chrono::Local`, so DST comes from the OS for free).
The default schedule: trains 07:00–08:30 Mon–Fri, weather otherwise. Every 60
seconds (independent of the 5-minute content poll) `main.rs` compares the
schedule's answer to what it last forced and, on a mismatch, sends
`ble::switch_to_slot` — [`CMD_SLOT_SWITCH`
(0x0084)](https://github.com/gavinmcnair/Firmware/blob/feat/pipe-slot-write/include/opendisplay_protocol.h),
the server-driven equivalent of a physical button press, added to the
Firmware fork specifically because nothing else can change which slot is on
screen remotely (a slot-target push alone only auto-refreshes when it
happens to target the slot already selected). Each registry row on the index
page shows its own schedule window (or `DEFAULT`) in gray alongside its name,
so slot 0 doubles as "what's driving the display right now, and why" without
repeating each plugin's name in a separate section.

A `Plugin` can also override `autoswitch_on_change()` to force itself onto
the screen the instant its own content changes, bypassing the schedule for
that tick — meant for a future alert-style page (nothing currently uses it,
but the wiring is in place in `main.rs`'s loop).

## Running

```bash
cargo build --release
./target/release/egham_ble            # continuous: poll every 5 min, push on change
./target/release/egham_ble --once     # one pass, then exit
```

Escape hatches for previewing a render without touching the device or the
on-disk change-detection state. Both are generic — the orchestrator (`main.rs`)
looks the target up by slot number, exact name, or a case-insensitive
substring of its name, entirely through the `Plugin` trait. There's no
per-plugin flag to add here when a new page is registered:

```bash
egham_ble --render 1 out.png          # by slot number
egham_ble --render weather out.png    # by (substring of) name
egham_ble --render index out.png      # slot 0 is always the index
egham_ble --setup calendar            # one-time interactive setup (Plugin::setup, no-op for most plugins)
egham_ble --compress-test             # zlib ratio on synthetic patterns + the first registered plugin's real render
```

Per-slot change detection lives in `egham_state_slot<N>.txt` next to the
binary (gitignored) — a page is only re-pushed to the device when its content
fingerprint actually changes, so e.g. the trains page's 10-second poll (see
`plugins::trains`) doesn't re-transfer an unchanged board.

## Docker / TrueNAS SCALE deployment

The target deployment is a TrueNAS SCALE Custom App with a USB Bluetooth
dongle (ASUS BT-series) — `Dockerfile`, `entrypoint.sh`, and
`docker-compose.yml` are the packaging, and each documents its own
non-obvious requirements. The short version:

- **Fonts are embedded in the binary** (`fonts/`, `include_bytes!`) — the
  image has no runtime font dependencies.
- **The container runs its own BlueZ**: `btleplug` on Linux needs
  `bluetoothd` over D-Bus, and the TrueNAS host doesn't run one.
  `entrypoint.sh` starts `dbus-daemon` + `bluetoothd`, waits for `hci0`,
  powers the adapter, then execs `egham_ble`.
- **`network_mode: host` is mandatory** — Bluetooth HCI sockets are
  network-namespaced; a bridged container can never see the adapter.
- **The dongle's driver/firmware are the host kernel's job** (`btusb`).
  Before installing the app, confirm the host sees it:
  `ls /sys/class/bluetooth` should show `hci0` (and
  `dmesg | grep -i bluetooth` shows the firmware load).
- **`TZ=Europe/London`** in the container — the scheduler's trains window
  is local wall-clock time.
- **Mount a host path at `/data`** for the fingerprint state files, or
  every restart repushes all slots.
- The credentials (`GOOGLE_CALENDAR_*`, optional `TRAINTIMES_BASE_URL`) go
  in the app's environment settings.
- The e-ink panel must be within BLE range **of the NAS**, not of wherever
  this used to run.

## Google Calendar setup

Unlike RTT/Open-Meteo, Google Calendar needs real user consent, not just an
API key — a one-time setup, done once per machine this client runs on:

1. In [Google Cloud Console](https://console.cloud.google.com/), create (or
   pick) a project, then enable the **Google Calendar API** for it
   (APIs & Services → Library).
2. Configure the OAuth consent screen (APIs & Services → OAuth consent
   screen) — External, testing mode is fine for personal use. Add your own
   Google account as a test user.
3. Create an OAuth client (APIs & Services → Credentials → Create
   Credentials → OAuth client ID), type **Desktop app**. Copy the Client ID
   and Client Secret it gives you.
4. Export them and run the one-time consent flow:

   ```bash
   export GOOGLE_CALENDAR_CLIENT_ID=...
   export GOOGLE_CALENDAR_CLIENT_SECRET=...
   egham_ble --setup calendar
   ```

   This prints a URL — open it, sign in, grant access. The flow catches
   Google's redirect on a loopback listener and saves a refresh token to
   `calendar_token.txt` (gitignored, never commit it). `GOOGLE_CALENDAR_CLIENT_ID`/`_SECRET` need
   to stay set (e.g. in your shell profile) for every future run, same as
   any other env var this project reads.

`calendar_token.txt` is the only long-lived secret — treat it like a
password to your calendar. Re-run `--setup calendar` if it's ever lost or
revoked.

## Adding a page

Implement `Plugin` in a new `plugins/<name>.rs`, register it in `plugins/mod.rs`,
and add `Box::new(<Name>Plugin)` to the `Vec` in `main.rs`. Pick an unused
slot number ≥ 1 (0 is the index). The status bar, quantization, and BLE push
are handled by the framework — a new page only needs to render its own
content and finish with `plugin::draw_status_bar(...)`.

That `Vec` in `main.rs` is deliberately the *only* place a new plugin's
concrete type gets named. Everything else — `--render`, `--setup`,
`--compress-test`, the content-poll loop, the scheduler, the index — goes
through the `Plugin` trait alone, so nothing else in `main.rs` needs
touching. If the new page needs its own credentials or config, read them
from its own env vars inside the plugin (see `calendar.rs`'s
`GOOGLE_CALENDAR_CLIENT_ID`/`_SECRET`), not by plumbing anything through
the orchestrator. If it needs one-time interactive setup before it can
render, override `Plugin::setup()` (see `CalendarDefaultPlugin::setup`) —
that's what `--setup <name>` calls. If it needs to look fresher than the
default 5-minute poll, override `Plugin::poll_interval()` — fetch on every
tick if the source has no rate limit worth respecting (see `plugins::trains`,
which polls its own self-hosted service every 10s), or cache internally
between fetches if it does.
