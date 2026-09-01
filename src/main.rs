mod air_quality;
mod ble;
mod calendar;
mod chart;
mod patterns;
mod plugin;
mod plugins;
mod protocol;
mod render;
mod scheduler;
mod state;
mod traintimes;
mod weather;

use anyhow::{Context, Result};
use plugin::Plugin;
use plugins::air_quality::AirQualityPlugin;
use plugins::battery::BatteryPlugin;
use plugins::calendar_default::CalendarDefaultPlugin;
use plugins::calendar_week::CalendarWeekPlugin;
use plugins::index::IndexPlugin;
use plugins::trains::TrainsPlugin;
use plugins::weather::WeatherPlugin;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ble::DEVICE_NAME;

// Ceiling on how long the loop sleeps between passes -- the schedule's
// boundaries (e.g. 07:00, 08:30) are checked at least this often. It is NOT
// a floor on plugin polling: the loop sleeps until the *earliest* plugin's
// `next_poll` deadline or this interval, whichever comes first, so a plugin
// with a fast `poll_interval()` (trains at 10s) actually gets called that
// often instead of being silently rounded up to this value (a real bug this
// replaced: a fixed 60s sleep made every faster interval a dead letter).
const SCHEDULE_TICK_INTERVAL: Duration = Duration::from_secs(60);

// Single-instance guard port. Two pollers racing on the same BLE device and
// state files corrupts both (observed live, 2026-09-01: interleaved
// poll.log writes and a missed panel update). A bound localhost port is a
// self-releasing lock: the OS frees it on any exit, clean or not, so there
// is no stale pidfile case to handle.
const INSTANCE_LOCK_PORT: u16 = 48412;

fn state_path_for_slot(slot: u8) -> PathBuf {
    PathBuf::from(format!("egham_state_slot{slot}.txt"))
}

/// Matches a `--render`/`--setup` identifier against a plugin's slot number
/// (as a string, e.g. "5"), its exact name, or a case-insensitive substring
/// of its name (so "weather" finds "Egham Weather" without needing the
/// full, space-containing, quoted name) -- the things a CLI caller could
/// plausibly type about a plugin without reading source. First match wins
/// on ambiguity (e.g. "egham" would match everything); exact slot number is
/// the unambiguous option when that matters. This is the one lookup every
/// plugin-targeting flag goes through, so main.rs never needs a bespoke
/// flag or branch per plugin -- see plugin.rs's own doc comments on
/// `setup`/`poll_interval` for why that matters: the orchestrator should
/// stay plugin-agnostic, and anything a specific plugin needs (credentials,
/// one-time setup, its own fetch cadence) is that plugin's own concern,
/// read from its own env vars or implemented behind the trait's
/// default-overridable methods.
fn find_plugin<'a>(plugins: &'a mut [Box<dyn Plugin>], id: &str) -> Option<&'a mut Box<dyn Plugin>> {
    let id_lower = id.to_lowercase();
    // Also try the query minus a trailing 's': "trains" should find "Egham
    // Train Times" ("trains" isn't a substring of "train times" -- a real
    // miss someone hit at the CLI, not a hypothetical).
    let id_singular = id_lower.trim_end_matches('s');
    plugins.iter_mut().find(|p| {
        let name = p.name().to_lowercase();
        p.slot().to_string() == id || name.contains(&id_lower) || name.contains(id_singular)
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fonts = render::Fonts::load();

    // The one place concrete plugin types are named. Adding a page is one
    // more line here (plus its own registration in plugins/mod.rs) --
    // nothing below this needs to change, including every --render-able
    // escape hatch, since all of them go through the Plugin trait generically.
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(BatteryPlugin::new()),
        Box::new(TrainsPlugin::new()),
        Box::new(WeatherPlugin),
        Box::new(CalendarDefaultPlugin),
        Box::new(CalendarWeekPlugin),
        Box::new(AirQualityPlugin),
    ];
    let registry: Vec<(u8, &'static str)> = plugins.iter().map(|p| (p.slot(), p.name())).collect();
    let scheduler = scheduler::default_schedule();
    let mut index = IndexPlugin::new(registry, scheduler.clone());

    // Generic escape hatches -- one `--render` and one `--setup` flag cover
    // every plugin (including the index, which is structurally special --
    // slot 0, orchestrator-built registry -- but still just a Plugin as far
    // as rendering it goes) via find_plugin's lookup, instead of a bespoke
    // `--render-<name>` flag needing to be added here for every new plugin.
    if let Some(idx) = args.iter().position(|a| a == "--render") {
        let id = args.get(idx + 1).context("--render needs <slot-or-name> <output-path>")?;
        let path = args.get(idx + 2).context("--render needs <slot-or-name> <output-path>")?;
        let id_lower = id.to_lowercase();
        let img = if id == "0" || "index".contains(&id_lower) || index.name().to_lowercase().contains(&id_lower) {
            index.set_updated_at(&render::current_time_utc_hhmm());
            index.render(&fonts).await?.1
        } else if let Some(plugin) = find_plugin(&mut plugins, id) {
            plugin.render(&fonts).await?.1
        } else {
            let known = std::iter::once(format!("0 ({})", index.name()))
                .chain(plugins.iter().map(|p| format!("{} ({})", p.slot(), p.name())))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("no plugin matches '{id}' -- registered: {known}");
        };
        img.save(path)?;
        eprintln!("Saved {path} ({}x{})", img.width(), img.height());
        return Ok(());
    }
    if let Some(id) = args.iter().position(|a| a == "--setup").and_then(|i| args.get(i + 1)) {
        let plugin = find_plugin(&mut plugins, id).with_context(|| format!("no plugin matches '{id}'"))?;
        plugin.setup()?;
        eprintln!("Setup complete for {}.", plugin.name());
        return Ok(());
    }

    if args.iter().any(|a| a == "--compress-test") {
        // First registered plugin, whichever that is -- not naming a
        // specific one keeps this generic too (see find_plugin's doc
        // comment on why main.rs stays plugin-agnostic).
        let board_label = format!("actual board ({})", plugins[0].name());
        let (_, actual_board) = plugins[0].render(&fonts).await?;
        let cases: [(&str, image::GrayImage); 4] = [
            ("checkerboard (1px)", patterns::checkerboard()),
            ("fractal (mandelbrot, 4-level)", patterns::fractal()),
            ("noise (uniform random)", patterns::noise(424_242)),
            (&board_label, actual_board),
        ];
        println!("{:<32} {:>10} {:>10} {:>8}", "pattern", "raw", "compressed", "ratio");
        for (name, img) in &cases {
            let packed = render::pack_gray4_planes(img);
            let compressed = ble::zlib_compress(&packed);
            println!(
                "{:<32} {:>10} {:>10} {:>7.2}x",
                name,
                packed.len(),
                compressed.len(),
                packed.len() as f64 / compressed.len() as f64
            );
        }
        return Ok(());
    }

    let once = args.iter().any(|a| a == "--once");

    // Held for the life of the process; deliberately AFTER the --render/
    // --setup/--compress-test escape hatches above, which are safe (and
    // useful) to run while a poller is already up.
    let _instance_lock = std::net::TcpListener::bind(("127.0.0.1", INSTANCE_LOCK_PORT)).context(
        "another egham_ble instance appears to be running (instance lock port busy) -- kill it first",
    )?;

    let mut last_fingerprints: Vec<Option<u64>> =
        plugins.iter().map(|p| state::load(&state_path_for_slot(p.slot()))).collect();

    // What we last told the device to display via CMD_SLOT_SWITCH -- None
    // means "unknown" (just started), which always forces one switch on the
    // first tick so a restarted process re-syncs the panel to the schedule
    // rather than trusting whatever it happened to be showing before.
    let mut last_forced_slot: Option<u8> = None;

    // Per-plugin, not one shared gate: each plugin's own `poll_interval()`
    // decides how often the orchestrator even calls its `render()` (see that
    // method's doc comment -- trains.rs uses a fast interval here paired
    // with its own internal, separately-paced RTT cache, so it can drop a
    // departed train from the board within a tick without any extra network
    // call). All start due (`Instant::now()`) so the very first loop pass
    // renders and pushes everything, same as before this was per-plugin.
    let mut next_poll: Vec<Instant> = plugins.iter().map(|_| Instant::now()).collect();

    loop {
        let mut any_changed = false;
        let mut autoswitch_target: Option<u8> = None;

        for (i, plugin) in plugins.iter_mut().enumerate() {
            if !once && Instant::now() < next_poll[i] {
                continue;
            }
            match tick_plugin(&fonts, plugin.as_mut(), &mut last_fingerprints[i]).await {
                Ok(true) => {
                    any_changed = true;
                    eprintln!("Pushed update to {DEVICE_NAME} slot {}.", plugin.slot());
                    if plugin.autoswitch_on_change() {
                        autoswitch_target = Some(plugin.slot());
                    }
                }
                Ok(false) => eprintln!("Slot {}: no change.", plugin.slot()),
                Err(e) => eprintln!("Slot {} tick failed: {e:#}", plugin.slot()),
            }
            next_poll[i] = Instant::now() + plugin.poll_interval();
        }
        // Index is refreshed whenever ANY content plugin actually changed
        // (not gated on its own fingerprint) -- it's small, and this is
        // what keeps it correct both when a slot updates and whenever the
        // plugin registry itself changes (a redeploy's first real content
        // update carries any new names with it).
        if any_changed {
            match push_index(&fonts, &mut index).await {
                Ok(()) => eprintln!("Pushed index update."),
                Err(e) => eprintln!("Index push failed: {e:#}"),
            }
        }

        // A flagged plugin's fresh content wins this tick outright -- see
        // Plugin::autoswitch_on_change's doc comment. Otherwise the
        // time-based schedule decides, and only forces a switch when its
        // answer actually differs from what's already on screen.
        let target = if let Some(slot) = autoswitch_target {
            Some((slot, "ALERT"))
        } else {
            let (slot, label) = scheduler.active_now();
            if last_forced_slot == Some(slot) {
                None
            } else {
                Some((slot, label))
            }
        };
        if let Some((slot, label)) = target {
            match force_switch(slot).await {
                Ok(()) => {
                    eprintln!("Switched {DEVICE_NAME} to slot {slot} ({label}).");
                    last_forced_slot = Some(slot);
                }
                Err(e) => eprintln!("Slot switch to {slot} ({label}) failed: {e:#}"),
            }
        }

        if once {
            return Ok(());
        }
        // Sleep until the earliest plugin poll deadline, capped at
        // SCHEDULE_TICK_INTERVAL so schedule boundaries are still checked at
        // least that often -- see that constant's comment.
        let schedule_deadline = Instant::now() + SCHEDULE_TICK_INTERVAL;
        let wake = next_poll.iter().min().copied().unwrap_or(schedule_deadline).min(schedule_deadline);
        tokio::time::sleep_until(tokio::time::Instant::from_std(wake)).await;
    }
}

/// Connects and sends CMD_SLOT_SWITCH. A separate connect per call, same as
/// `tick_plugin`/`push_index` -- simple and consistent, at the cost of a
/// reconnect on every forced switch; switches are infrequent (schedule
/// boundaries, or a rare autoswitch) so this isn't worth optimizing away.
async fn force_switch(slot: u8) -> Result<()> {
    let peripheral = ble::find_and_connect(DEVICE_NAME).await?;
    ble::switch_to_slot(&peripheral, slot).await
}

/// Renders one plugin, and if its content has actually changed, pushes it to
/// its own slot and persists the new fingerprint.
async fn tick_plugin(
    fonts: &render::Fonts,
    plugin: &mut dyn Plugin,
    last_fingerprint: &mut Option<u64>,
) -> Result<bool> {
    let (fingerprint, img) = plugin.render(fonts).await?;
    if Some(fingerprint) == *last_fingerprint {
        return Ok(false);
    }

    let packed = render::pack_gray4_planes(&img);
    let peripheral = ble::find_and_connect(DEVICE_NAME).await?;
    ble::upload_pipe_write_to_slot(&peripheral, plugin.slot(), &packed).await?;

    state::save(&state_path_for_slot(plugin.slot()), fingerprint)?;
    *last_fingerprint = Some(fingerprint);
    Ok(true)
}

/// Refreshes and pushes the index unconditionally -- see the call site's
/// comment for why it isn't gated on its own fingerprint here.
async fn push_index(fonts: &render::Fonts, index: &mut IndexPlugin) -> Result<()> {
    index.set_updated_at(&render::current_time_utc_hhmm());
    let (_, img) = index.render(fonts).await?;
    let packed = render::pack_gray4_planes(&img);
    let peripheral = ble::find_and_connect(DEVICE_NAME).await?;
    ble::upload_pipe_write_to_slot(&peripheral, index.slot(), &packed).await?;
    Ok(())
}
