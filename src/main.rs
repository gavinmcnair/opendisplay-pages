mod ble;
mod patterns;
mod plugin;
mod plugins;
mod protocol;
mod render;
mod rtt;
mod state;
mod weather;

use anyhow::Result;
use image::GrayImage;
use plugin::Plugin;
use plugins::index::IndexPlugin;
use plugins::trains::TrainsPlugin;
use plugins::weather::WeatherPlugin;
use std::path::PathBuf;
use std::time::Duration;

const DEVICE_NAME: &str = "ODC48BB0";

// RTT allows 10 calls/min, 100/hour, 1000/day; Open-Meteo is unauthenticated
// with generous fair-use limits. Each tick costs at most 4 calls (RTT token
// mint + 2 direction fetches, + 1 Open-Meteo fetch), so 300s keeps every RTT
// window comfortably under budget even run forever (864 RTT calls/day).
const POLL_INTERVAL: Duration = Duration::from_secs(300);

fn state_path_for_slot(slot: u8) -> PathBuf {
    PathBuf::from(format!("egham_state_slot{slot}.txt"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fonts = render::Fonts::load();

    // Escape hatches for previewing a render without touching the device or
    // the change-detection state files.
    if let Some(path) = args.iter().position(|a| a == "--render-only").and_then(|i| args.get(i + 1)) {
        let mut trains = TrainsPlugin;
        let (_, img) = trains.render(&fonts).await?;
        img.save(path)?;
        eprintln!("Saved {path} ({}x{})", img.width(), img.height());
        return Ok(());
    }
    if let Some(path) = args.iter().position(|a| a == "--render-weather").and_then(|i| args.get(i + 1)) {
        let mut weather = WeatherPlugin;
        let (_, img) = weather.render(&fonts).await?;
        img.save(path)?;
        eprintln!("Saved {path} ({}x{})", img.width(), img.height());
        return Ok(());
    }
    if let Some(path) = args.iter().position(|a| a == "--render-index").and_then(|i| args.get(i + 1)) {
        let mut index = IndexPlugin::new(vec![
            (TrainsPlugin.slot(), TrainsPlugin.name()),
            (WeatherPlugin.slot(), WeatherPlugin.name()),
        ]);
        index.set_updated_at(&render::current_time_utc_hhmm());
        let (_, img) = index.render(&fonts).await?;
        img.save(path)?;
        eprintln!("Saved {path} ({}x{})", img.width(), img.height());
        return Ok(());
    }

    if args.iter().any(|a| a == "--compress-test") {
        let mut trains = TrainsPlugin;
        let (_, actual_board) = trains.render(&fonts).await?;
        let cases: [(&str, GrayImage); 4] = [
            ("checkerboard (1px)", patterns::checkerboard()),
            ("fractal (mandelbrot, 4-level)", patterns::fractal()),
            ("noise (uniform random)", patterns::noise(424_242)),
            ("actual board (right now)", actual_board),
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

    // The index's registry is built from every OTHER registered plugin's
    // (slot, name) -- one source of truth, so it can never drift out of
    // sync with what's actually running. Adding a future plugin is just one
    // more line in this Vec.
    let mut plugins: Vec<Box<dyn Plugin>> = vec![Box::new(TrainsPlugin), Box::new(WeatherPlugin)];
    let registry: Vec<(u8, &'static str)> = plugins.iter().map(|p| (p.slot(), p.name())).collect();
    let mut index = IndexPlugin::new(registry);
    let mut last_fingerprints: Vec<Option<u64>> =
        plugins.iter().map(|p| state::load(&state_path_for_slot(p.slot()))).collect();

    loop {
        let mut any_changed = false;
        for (i, plugin) in plugins.iter_mut().enumerate() {
            match tick_plugin(&fonts, plugin.as_mut(), &mut last_fingerprints[i]).await {
                Ok(true) => {
                    any_changed = true;
                    eprintln!("Pushed update to {DEVICE_NAME} slot {}.", plugin.slot());
                }
                Ok(false) => eprintln!("Slot {}: no change.", plugin.slot()),
                Err(e) => eprintln!("Slot {} tick failed: {e:#}", plugin.slot()),
            }
        }
        // Index is refreshed whenever ANY content plugin actually changed
        // (not gated on its own fingerprint) -- it's small, and this is what
        // keeps it correct both when a slot updates and whenever the plugin
        // registry itself changes (a redeploy's first real content update
        // carries any new names with it).
        if any_changed {
            match push_index(&fonts, &mut index).await {
                Ok(()) => eprintln!("Pushed index update."),
                Err(e) => eprintln!("Index push failed: {e:#}"),
            }
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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
