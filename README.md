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
| `plugins::calendar_default` | 3 | Google Calendar (today + tomorrow, agenda) | `GOOGLE CALENDAR` |
| `plugins::calendar_week` | 4 | Google Calendar (7-day grid) | `7-DAY CALENDAR` |

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
on-disk change-detection state:

```bash
egham_ble --render-only  out.png          # trains page
egham_ble --render-weather out.png        # weather page
egham_ble --render-index out.png          # index page
egham_ble --render-calendar out.png       # calendar (default/agenda) page
egham_ble --render-calendar-week out.png  # calendar (week grid) page
egham_ble --compress-test                 # zlib ratio on synthetic + a real render
```

Per-slot change detection lives in `egham_state_slot<N>.txt` next to the
binary (gitignored) — a page is only re-pushed to the device when its content
fingerprint actually changes, so e.g. the trains page's 5-minute poll doesn't
re-transfer an unchanged board.

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
   egham_ble --calendar-auth
   ```

   This prints a URL — open it, sign in, grant access. The flow catches
   Google's redirect on a loopback listener and saves a refresh token to
   `calendar_token.txt` (gitignored, never commit it). `GOOGLE_CALENDAR_CLIENT_ID`/`_SECRET` need
   to stay set (e.g. in your shell profile) for every future run, same as
   any other env var this project reads.

`calendar_token.txt` is the only long-lived secret — treat it like a
password to your calendar. Re-run `--calendar-auth` if it's ever lost or
revoked.

## Adding a page

Implement `Plugin` in a new `plugins/<name>.rs`, register it in `plugins/mod.rs`,
and add `Box::new(<Name>Plugin)` to the `Vec` in `main.rs`. Pick an unused
slot number ≥ 1 (0 is the index). The status bar, quantization, and BLE push
are handled by the framework — a new page only needs to render its own
content and finish with `plugin::draw_status_bar(...)`.
