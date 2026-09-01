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

    render::draw_text(&mut img, &fonts.sans_bold, 13.0, 26.0, 22.0 + 13.0, "EGHAM DISPLAY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.sans_black, 46.0, 26.0, 36.0 + 46.0, "INDEX", BLACK);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    // Row height is deliberately compact (not the more generous spacing a
    // 2-plugin registry could afford) -- needs to keep working as more pages
    // get registered without colliding with the status bar below it.
    let row_h = 42.0;
    let row_y0 = 130.0;
    for (i, (slot_id, name)) in registry.iter().enumerate() {
        let ry = row_y0 + i as f32 * row_h;
        if i > 0 {
            draw_line_segment_mut(&mut img, (26.0, ry), (W as f32 - 26.0, ry), Luma([LIGHT_GRAY]));
        }
        let label = format!("SLOT {slot_id}");
        render::draw_text(&mut img, &fonts.mono, 15.0, 26.0, ry + 27.0, &label, DARK_GRAY);
        render::draw_text(&mut img, &fonts.sans_bold, 19.0, 150.0, ry + 28.0, name, BLACK);

        // Schedule info folded into the same row, gray, right-aligned --
        // rather than a separate section repeating each plugin's name a
        // second time. Blank for a plugin the scheduler never mentions.
        if let Some(schedule_text) = schedule_text_for(scheduler, *slot_id) {
            let tw = render::text_width(&fonts.sans_bold, 14.0, &schedule_text);
            render::draw_text(&mut img, &fonts.sans_bold, 14.0, W as f32 - 26.0 - tw, ry + 28.0, &schedule_text, LIGHT_GRAY);
        }
    }

    plugin::draw_status_bar(&mut img, fonts, SLOT, updated_at, STATUS_LABEL);
    img
}

/// What to show, in gray, alongside a registry row for `slot_id` -- its
/// scheduled window if `crate::scheduler` has a rule for it, "DEFAULT" if
/// it's the fallback slot, or nothing for a plugin the schedule never
/// mentions (e.g. the calendar pages, which are only ever reached manually).
fn schedule_text_for(scheduler: &Scheduler, slot_id: u8) -> Option<String> {
    if let Some(rule) = scheduler.rules.iter().find(|r| r.slot == slot_id) {
        return Some(format!("{}-{} {}", rule.start, rule.end, describe_days(rule.days)));
    }
    if slot_id == scheduler.default_slot {
        return Some("DEFAULT".to_string());
    }
    None
}
