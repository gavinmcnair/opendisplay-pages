# Darwin Push Port — notes for later

> **Superseded, 2026-08-30: see `kafka.md`.** This file's registration section
> (opendata.nationalrail.co.uk) and transport assumption (STOMP/ActiveMQ) are
> both out of date. The feed moved to raildata.org.uk (Rail Data Marketplace)
> and is accessed via **Kafka** (Confluent Cloud), not STOMP — confirmed
> against the real broker with real live data. `kafka.md` has the verified
> connection details, topic names, and message shape; the "what it is" /
> "what building this would involve" sections below are still a reasonable
> description of the underlying Darwin data itself, just not of how to reach it.

Why this file exists: our current `rtt.rs` fetches from `data.rtt.io`, which is a
polled REST API only — no webhook/streaming option. Polling any interval short
enough to feel "live" (e.g. every 60s) blows through RTT's rate limits if run
continuously (10/min, 100/hour, 1000/day, 10000/week). Darwin Push Port is the
actual real-time source RTT and similar sites sit on top of — genuine
publish/subscribe, pushed the moment a forecast/cancellation/platform changes,
not on a timer. This is what "push based updates, not timed pull" (the
project's original stated goal) actually requires.

## What it is

- Run by Network Rail / National Rail's open data programme.
- Delivers the same live train-running data Darwin (the national rail
  enquiries backend) itself consumes: forecasts, cancellations, platform
  changes, delays — network-wide, as they happen.
- Transport: STOMP over ActiveMQ (also reachable via OpenWire). A persistent
  connection, not request/response — you subscribe to a topic and messages
  arrive as events.
- Message bodies are **gzip-compressed XML** (Darwin's Push Port schema).
  Whatever STOMP client library we use needs to handle binary frames and the
  content-length header correctly.
- Supports durable subscriptions, so messages sent while we're disconnected
  can be retained and delivered on reconnect instead of lost.

## Registration (manual step, not something I can do from here)

1. Sign up at <https://opendata.nationalrail.co.uk/> — free, but approval-gated
   (not instant).
2. Once approved, the "My Feeds" page has a "Darwin Topic Information"
   section with the STOMP username/password and connection details (host,
   port, topic name for the live feed vs. the status-messages topic).

## What building this would actually involve

- A long-lived STOMP client task (separate from the current one-shot HTTP
  calls in `rtt.rs`) that stays connected and reconnects on drop.
- Gzip-decompress each message, then parse Darwin's Push Port XML schema to
  pull out just the fields we care about (scheduled/forecast times, platform,
  cancellation flag) — the feed is network-wide, so we'd filter to services
  whose calling points include Egham (CRS `EGH`) in each direction.
- Feed parsed changes into the existing `poll::FrameSource` trait
  (`poll.rs`) instead of the current timer-driven `EghamSource` — the loop,
  change-detection (`rtt::fingerprint`), rendering, and BLE push
  (`render.rs`, `ble.rs`) all stay as they are. This is a source swap, not a
  rewrite: implement `FrameSource` for a `DarwinSource` that yields a new
  frame each time a relevant push message arrives (or on first connect),
  instead of `EghamSource` yielding one every 5 minutes.
- Need to decide idle behaviour: Push Port is quiet when nothing's changing,
  so the loop would mostly just be blocked awaiting the next STOMP frame
  rather than sleeping on a timer.

## Open questions for when we pick this up

- Exact topic name(s) and message schema version to subscribe to (confirm on
  the My Feeds page after approval — varies by feed generation).
- Which Rust STOMP client to use (need one that handles gzip binary frames
  and durable subscriptions cleanly) — not yet researched.
- Whether to also drop RTT entirely at that point, or keep it as a fallback
  for the initial full board render (Push Port gives *changes*, not a full
  timetable snapshot on connect) — RTT is what fills the board on cold start;
  Push Port would only tell us what to change after that. Might want to keep
  both: RTT for a periodic full-board sanity refresh (e.g. hourly), Push Port
  for the instant-reaction path in between.

## Sources

- <https://wiki.openraildata.com/index.php/Darwin:Push_Port>
- <https://www.nationalrail.co.uk/developers/darwin-data-feeds/>
- <https://iianderson.medium.com/a-new-dawn-in-uk-rail-analysis-connecting-to-the-darwin-push-port-ba63d22d2944>
