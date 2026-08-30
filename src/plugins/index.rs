//! Index/home page (slot 0): lists every other registered plugin by name,
//! kept separate from its numeric slot (`Plugin::name()` vs `Plugin::slot()`
//! on each entry). Structurally just another `Plugin` -- shares the trait
//! and bottom chrome with every content page -- but the orchestrator in
//! main.rs drives its push timing differently: it's refreshed whenever ANY
//! other plugin's content actually changes (see `set_updated_at`), not on
//! its own independent schedule, since its own content (the registry) is
//! static within one running process.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::plugin::{self, Plugin};
use crate::render::{self, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};
use crate::scheduler::{describe_days, Scheduler};

pub const SLOT: u8 = 0;
const NAME: &str = "Index";
const STATUS_LABEL: &str = "PRESS KEY1 / KEY2 TO BROWSE";

pub struct IndexPlugin {
    registry: Vec<(u8, &'static str)>,
    updated_at: String,
    scheduler: Scheduler,
}

impl IndexPlugin {
    pub fn new(registry: Vec<(u8, &'static str)>, scheduler: Scheduler) -> Self {
        Self { registry, updated_at: String::from("-"), scheduler }
    }

    /// Called by the orchestrator right before `render()`, so the index
    /// shows the same "updated at" time as whichever plugin just triggered
    /// this refresh.
    pub fn set_updated_at(&mut self, updated_at: &str) {
        self.updated_at = updated_at.to_string();
    }
}

impl Plugin for IndexPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            // NOT part of the fingerprint: the active schedule rule changes
            // by itself as the clock ticks past a boundary, with no other
            // plugin's content changing -- fingerprinting it would mean the
            // index never looks stale between real content changes, at the
            // cost of never re-pushing to reflect a schedule boundary either.
            // The orchestrator already re-renders the index on every tick it
            // force-switches the panel (see main.rs), so the displayed
            // schedule stays accurate in practice without needing this.
            let fingerprint = fingerprint_registry(&self.registry, &self.updated_at);
            let img = render_page(fonts, &self.registry, &self.updated_at, &self.scheduler);
            Ok((fingerprint, img))
        })
    }
}

fn fingerprint_registry(registry: &[(u8, &'static str)], updated_at: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    registry.hash(&mut hasher);
    updated_at.hash(&mut hasher);
    hasher.finish()
}

fn render_page(fonts: &Fonts, registry: &[(u8, &'static str)], updated_at: &str, scheduler: &Scheduler) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM DISPLAY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 46.0, 26.0, 36.0 + 46.0, "INDEX", BLACK);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    let row_h = 60.0;
    let row_y0 = 140.0;
    for (i, (slot_id, name)) in registry.iter().enumerate() {
        let ry = row_y0 + i as f32 * row_h;
        if i > 0 {
            draw_line_segment_mut(&mut img, (26.0, ry), (W as f32 - 26.0, ry), Luma([LIGHT_GRAY]));
        }
        let label = format!("SLOT {slot_id}");
        render::draw_text(&mut img, &fonts.mono, 22.0, 26.0, ry + 32.0, &label, DARK_GRAY);
        render::draw_text(&mut img, &fonts.arial_bold, 24.0, 190.0, ry + 33.0, name, BLACK);
    }

    let schedule_y0 = row_y0 + registry.len() as f32 * row_h + 30.0;
    draw_schedule(&mut img, fonts, scheduler, schedule_y0);

    plugin::draw_status_bar(&mut img, fonts, SLOT, updated_at, STATUS_LABEL);
    img
}

/// Lists the server-side schedule (see `crate::scheduler`) so the index
/// doubles as "what's driving the display right now, and why" -- each rule's
/// time window and days, the default, and which one is active this instant
/// (bold black; the rest dark gray).
fn draw_schedule(img: &mut GrayImage, fonts: &Fonts, scheduler: &Scheduler, y0: f32) {
    render::draw_text(img, &fonts.arial_bold, 15.0, 26.0, y0, "SCHEDULE", DARK_GRAY);
    draw_line_segment_mut(img, (26.0, y0 + 10.0), (W as f32 - 26.0, y0 + 10.0), Luma([LIGHT_GRAY]));

    let (active_slot, _) = scheduler.active_now();
    let row_h = 32.0;
    let mut ry = y0 + 34.0;

    for rule in &scheduler.rules {
        let active = rule.slot == active_slot;
        let color = if active { BLACK } else { DARK_GRAY };
        let when = format!("{}-{} {}", rule.start, rule.end, describe_days(rule.days));
        render::draw_text(img, &fonts.mono, 15.0, 26.0, ry, &when, color);
        let what = format!("SLOT {} {}", rule.slot, rule.label);
        let ww = render::text_width(&fonts.arial_bold, 15.0, &what);
        render::draw_text(img, &fonts.arial_bold, 15.0, W as f32 - 26.0 - ww, ry, &what, color);
        ry += row_h;
    }

    let default_active = scheduler.rules.iter().all(|r| r.slot != active_slot);
    let color = if default_active { BLACK } else { DARK_GRAY };
    render::draw_text(img, &fonts.mono, 15.0, 26.0, ry, "DEFAULT", color);
    let what = format!("SLOT {} {}", scheduler.default_slot, scheduler.default_label);
    let ww = render::text_width(&fonts.arial_bold, 15.0, &what);
    render::draw_text(img, &fonts.arial_bold, 15.0, W as f32 - 26.0 - ww, ry, &what, color);
}
