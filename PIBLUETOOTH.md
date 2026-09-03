# Running egham_ble on a Raspberry Pi — the Bluetooth field guide

Everything learned deploying this poller to Raspberry Pi Zero 2 W boards on
2026-09-02, written down so the next deployment (or the next debugging
session at 10pm) doesn't rediscover it. macOS hid *every one* of these
problems — code that ran flawlessly on the Mac for days failed in five
distinct ways on a Pi. If pushes work from a Mac and fail from a Pi, start
here, top to bottom: this is the order the layers actually failed in.

The reference deployment: Pi Zero 2 W, DietPi (Debian 13), built-in
BCM43430 radio, systemd service, panel at -45 to -56 dBm near line of
sight. `hci0` alone is not evidence of a working stack — it was present
and useless for most of a day.

## 1. Packages: BlueZ, D-Bus, and the radio firmware patch

Minimal DietPi images ship none of the Bluetooth userspace. `btleplug`
needs `bluetoothd` reachable over the system D-Bus, and — critically — the
Broadcom radio needs its vendor firmware patch:

```bash
apt-get install -y --no-install-recommends bluez dbus libdbus-1-3 bluez-firmware
systemctl enable --now bluetooth
reboot   # the .hcd patch only loads at radio init
```

**`bluez-firmware` is the one that bites.** The Pi's BCM43430 boots on a
minimal factory ROM and expects Linux to upload a `.hcd` patch file at
every boot. Without it, the radio **scans perfectly but every LE
connection dies** with `le-connection-abort-by-local` (HCI reason 0x3e,
"Connection Failed to be Established") — measured 0/5 connects before,
mostly-clean after. The kernel says it outright, so check first:

```bash
dmesg | grep -i bcm
#  bad:  Bluetooth: hci0: BCM: firmware Patch file not found, tried: ...
#  good: Bluetooth: hci0: BCM43430B0 'brcm/BCM43430B0...zero-2-w.hcd' Patch
```

## 2. The BT stack corrupts silently — reboot the Pi before deep-diving

After heavy abuse (adapter power cycles, `systemctl restart bluetooth`
mid-connection, add/remove of USB dongles, interrupted transfers), the
BlueZ/kernel stack enters a state where **connects fail 0/N while scans
still work** — with no error anywhere hinting at host-side corruption. A
Pi reboot took one such session from 0/6 to 5/5 with zero other changes.
Rule: before concluding anything about the link, the panel, or the code,
reboot the Pi and retest. (Same session also hit SD-card ext4 corruption
that remounted `/` read-only — fixed by adding
`fsck.mode=force fsck.repair=yes` to `/boot/firmware/cmdline.txt` for one
boot. Cheap SD cards in Pis: check `mount | grep ' / '` when writes fail.)

## 3. Connect etiquette (already encoded in `ble.rs`, documented here so
nobody "simplifies" it away)

- **Retry connects, spaced ~3s** — the panel light-sleeps between
  advertising events and a first LE connect attempt often lands 0x3e even
  with good signal. CoreBluetooth retries internally; BlueZ surfaces every
  failure, so we retry ourselves (`find_and_connect_with_rssi`).
- **Never cancel/disconnect after a FAILED connect.** There is no
  connection to clean, and the cancel can race the panel's own connection
  setup and wedge its advertising entirely.
- **Re-find the device before each retry.** BlueZ expires its D-Bus device
  object between attempts; a cached handle turns into
  `Method "Connect" ... doesn't exist`.
- **Disconnect on every exit path.** BlueZ never cleans up for you
  (CoreBluetooth quietly did). A dangling connection makes the next
  operation fail with `writing START: In Progress` — and it also **pins
  the panel off-air**, because a connected peripheral stops advertising.
  This is why the systemd unit carries
  `ExecStopPost=-/usr/bin/bluetoothctl disconnect <panel MAC>`: a
  SIGTERM'd process can't run its own cleanup.
- BlueZ scopes discovered devices **per adapter**: after a fresh boot (or
  on a second adapter), `bluetoothctl connect <MAC>` fails with "Device not
  available" until a scan has run on that adapter.
- macOS-side footnote: CoreBluetooth device UUIDs for the panel go stale;
  rediscover by name (`ODC48BB0`) rather than trusting a stored UUID.

## 4. The transfer stalls: two sender bugs macOS masked (fixed 2026-09-02)

The headline failure: transfers negotiated fine, then stalled mid-stream
and died at the operation timeout — **every large page, from two different
Pis and two different radios, at up to -43 dBm**, while the Mac pushed the
same pages in seconds. An HCI trace (`btmon`) plus per-ACK logging found
two compounding bugs in our own sender (`run_pipe_write`):

1. **Acting on the oldest queued SACK.** The notification stream buffers
   every SACK; the loop consumed one per iteration. On a slow connection
   interval the queue backs up, so decisions came from SACKs many frames
   old → "no progress" → retransmit a frame the device already had → which
   elicits another SACK → self-sustaining loop. Trace signature: sender
   pinned on one seq at one retransmit per connection interval while every
   reply said `highest_seen` far ahead with a full mask. Fix: after each
   blocking ACK wait, drain everything queued (`now_or_never`) and act on
   the newest.
2. **Re-sending the whole window every iteration.** The loop transmitted
   `confirmed..confirmed+W` on every pass, flooding the host TX queue with
   duplicates that starved genuinely new frames (measured: ~25 wasted
   rounds per 4-frame advance). Fix: true sliding window — `next_to_send`
   tracks what's been transmitted; each frame goes out once, and the
   oldest unacked frame is retransmitted only after **two consecutive**
   stale ACK reads (the device re-SACKs its current state on every
   duplicate received, so an eager retransmit echoes forever).

Supporting tuning from the same session: `REQ_WINDOW/REQ_ACK_EVERY` 8/4
(down from 16/8), `DATA_ACK_TIMEOUT` 5s for fast loss recovery,
`OP_TIMEOUT` 180s so a slow-but-progressing transfer finishes. Result:
all 7 pages from cold in 58s; the 14KB trains page (which had *never*
completed from a Pi) in 11s. The per-round progress log
(`data: confirmed x->y (ack highest=… mask=…)`) is deliberately permanent —
if stalls regress, the journal shows the shape immediately.

Why macOS masked both bugs: ~15ms connection intervals drained queues and
duplicates so fast the pathologies never accumulated. BlueZ at 30–100ms
intervals turned them fatal.

## 4a. The slow death: D-Bus connection leak (fixed 2026-09-03)

Symptom: everything works for a few hours, then **all** updates stop, and
every tick logs `creating BLE manager: The maximum number of active
connections for UID 0 has been reached`. The train feed and rendering are
fine — it dies at the very first BLE step, before any radio activity.

Cause: calling `Manager::new()` per operation. On Linux each Manager opens
a D-Bus connection btleplug doesn't release on drop (~1.2 leaked sockets
per push/switch/telemetry-read), hitting the system bus's default
256-per-user cap in a few hours. CoreBluetooth has no equivalent, so it
never showed on the Mac.

Fix (in `ble.rs`): one process-lifetime `Manager`+`Adapter` in a
`OnceCell`, reused everywhere. Diagnose/verify by watching FD count while
exercising BLE ops:

```bash
PID=$(systemctl show -p MainPID --value egham-ble)
ls /proc/$PID/fd | wc -l         # before
# ...trigger several switches/pushes...
ls /proc/$PID/fd | wc -l         # must be FLAT, not climbing
```

Emergency recovery if it ever regresses: `systemctl restart egham-ble`
drops the process and all leaked connections instantly.

## 5. When the panel itself is the problem

Two firmware wedge modes (fixes belong in the Firmware fork, still TODO):

- **Stops advertising** after repeated failed/aborted connects. Nobody can
  see it; looks like the panel died. First check it isn't merely *held
  connected* by a stale host connection (`bluetoothctl info <MAC>` →
  `Connected: yes` → disconnect frees it).
- **Accepts connections but never ACKs data**, screen possibly blank,
  after an interrupted transfer. Connects succeed, negotiation succeeds,
  every upload times out.

Both clear with a power cycle — or without leaving the sofa, with a remote
reboot over BLE (opcode `CMD_REBOOT`, no payload):

```bash
# from any machine that can connect (bleak via uv):
uv run --with bleak python3 -c "
import asyncio, bleak
async def m():
    d = await bleak.BleakScanner.find_device_by_name('ODC48BB0', timeout=15)
    async with bleak.BleakClient(d) as c:
        await c.write_gatt_char('00002446-0000-1000-8000-00805f9b34fb', bytes([0x00,0x0F]), response=True)
asyncio.run(m())"
```

Quiesce the poller first (`systemctl stop egham-ble`) — hammering a
wedged panel with retries deepens the wedge.

## 6. Diagnostic toolbox (in escalation order)

```bash
ls /sys/class/bluetooth                      # kernel sees an adapter at all?
dmesg | grep -i bcm                          # radio firmware patch loaded?
sudo btmgmt find | grep -B2 'name ODC'       # panel advertising? RSSI?
bluetoothctl info 44:B1:76:B0:8B:C6          # held connected by someone?
bluetoothctl connect 44:B1:76:B0:8B:C6       # manual connect, independent of our code
sudo btmon                                   # ground truth: who stops talking, and why
journalctl -u egham-ble | grep 'data: confirmed'   # sender progress per ACK round
```

Rules of thumb: connects want roughly better than -60 dBm *and* a clean
stack — but note the stalls above happened at -43, so good RSSI clears
only layer one. Scanning working proves almost nothing (advertisements
repeat; connection setup is one-shot). And one poller at a time, ever:
two hosts driving the panel concurrently corrupts transfers and wedges it.

## 7. Deployment reference

Build on an Apple Silicon Mac (native linux/arm64 via Docker), extract,
ship, **verify the hash** — one corrupt scp caused a silent SIGSEGV
crash-loop:

```bash
docker build -t egham-ble:latest .
docker create --name x egham-ble:latest && docker cp x:/usr/local/bin/egham_ble ./egham_ble_arm64 && docker rm x
scp egham_ble_arm64 root@<pi>:/root/egham/egham_ble
shasum -a 256 egham_ble_arm64   # compare with sha256sum on the Pi -- MUST match
```

On the Pi: `/root/egham/` holds the binary + state files + battery
history; `/etc/default/egham` (mode 600) carries `TZ=Europe/London` (Pis
run UTC; the scheduler window is wall-clock) and the `GOOGLE_CALENDAR_*`
credentials; `/etc/systemd/system/egham-ble.service` runs it with
`Restart=always` and the `ExecStopPost` disconnect from §3.
