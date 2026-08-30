//! Trains page (slot 1): live Egham departures by physical platform, not by
//! ultimate destination. Egham has exactly two platforms -- Platform 1
//! (east, towards London Waterloo) and Platform 2 (west, towards Reading,
//! Ascot, Chertsey, Woking and everything beyond) -- so a westbound train's
//! specific destination (Reading one service, Woking via Chertsey the next)
//! doesn't change which platform it leaves from. Earlier versions of this
//! page split westbound trains into separate per-destination columns; that
//! was wrong (a passenger stands on a platform, not in front of a
//! destination) and silently made the Woking/Chertsey services invisible
//! when only the Reading-filtered query was fetched. One unfiltered fetch,
//! split by platform locally, fixes both. The bottom status bar comes from
//! `plugin::draw_status_bar`, shared with every other page.

use anyhow::Result;
use futures::future::LocalBoxFuture;
use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_ellipse_mut, draw_filled_rect_mut, draw_line_segment_mut, draw_polygon_mut};
use imageproc::point::Point;
use imageproc::rect::Rect;

use crate::plugin::{self, Plugin};
use crate::render::{self, hhmm, text_width, truncate_to_width, Fonts, BLACK, DARK_GRAY, H, LIGHT_GRAY, W, WHITE};
use crate::rtt::{self, Departure, DeparturesResponse};

pub const SLOT: u8 = 1;
const NAME: &str = "Egham Train Times";
const STATUS_LABEL: &str = "REALTIME TRAINS";

pub struct TrainsPlugin;

impl Plugin for TrainsPlugin {
    fn slot(&self) -> u8 {
        SLOT
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn render<'a>(&'a mut self, fonts: &'a Fonts) -> LocalBoxFuture<'a, Result<(u64, GrayImage)>> {
        Box::pin(async move {
            let access_token = rtt::mint_access_token()?;
            let all = rtt::fetch_departures(&access_token)?;
            let fingerprint = rtt::fingerprint(&all);
            let img = render_page(fonts, &all);
            Ok((fingerprint, img))
        })
    }
}

/// A destination of "London Waterloo" means Platform 1 (east); every other
/// destination (Reading, Ascot, Chertsey, Woking, ...) leaves from Platform
/// 2 (west) -- Egham only has the two platforms, so this is exhaustive by
/// construction, not a guess at which destinations happen to exist today.
fn is_eastbound(svc: &Departure) -> bool {
    svc.destination.first().map(|d| d.location.description.contains("Waterloo")).unwrap_or(false)
}

fn render_page(fonts: &Fonts, all: &DeparturesResponse) -> GrayImage {
    let mut img = GrayImage::from_pixel(W, H, Luma([WHITE]));

    let now_ref = all.query.time_from.as_str();

    render::draw_text(&mut img, &fonts.arial_bold, 13.0, 26.0, 22.0 + 13.0, "SOUTH WESTERN RAILWAY", DARK_GRAY);
    render::draw_text(&mut img, &fonts.arial_black, 46.0, 26.0, 36.0 + 46.0, "EGHAM", BLACK);

    let clock_text = hhmm(now_ref);
    let date_text = &now_ref[0..10.min(now_ref.len())];
    let cw = text_width(&fonts.mono, 30.0, clock_text);
    let dw = text_width(&fonts.arial_bold, 13.0, date_text);
    render::draw_text(&mut img, &fonts.mono, 30.0, W as f32 - 26.0 - cw, 30.0 + 30.0, clock_text, BLACK);
    render::draw_text(&mut img, &fonts.arial_bold, 13.0, W as f32 - 26.0 - dw, 62.0 + 13.0, date_text, DARK_GRAY);

    draw_line_segment_mut(&mut img, (0.0, 100.0), (W as f32, 100.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 101.0), (W as f32, 101.0), Luma([BLACK]));
    draw_line_segment_mut(&mut img, (0.0, 102.0), (W as f32, 102.0), Luma([BLACK]));

    draw_line_segment_mut(&mut img, (400.0, 108.0), (400.0, 440.0), Luma([LIGHT_GRAY]));

    let east: Vec<&Departure> = all.services.iter().filter(|s| is_eastbound(s)).collect();
    let west: Vec<&Departure> = all.services.iter().filter(|s| !is_eastbound(s)).collect();

    draw_column(
        &mut img,
        fonts,
        &ColumnSpec { x0: 0.0, label: "PLATFORM 1 (EAST)", subtitle: "WATERLOO", arrow_left: true },
        &east,
    );
    draw_column(
        &mut img,
        fonts,
        &ColumnSpec { x0: 400.0, label: "PLATFORM 2 (WEST)", subtitle: "READING / ASCOT / CHERTSEY", arrow_left: false },
        &west,
    );

    plugin::draw_status_bar(&mut img, fonts, SLOT, hhmm(now_ref), STATUS_LABEL);
    img
}

enum Mark {
    Dot,
    Up,
    Cross,
}

fn draw_status_mark(img: &mut GrayImage, x: f32, baseline_y: f32, size: f32, kind: &Mark, fill: u8) {
    let r = size / 2.0;
    let cy = baseline_y - r + 1.0;
    let color = Luma([fill]);
    match kind {
        Mark::Dot => {
            draw_filled_ellipse_mut(img, (x as i32 + r as i32, cy as i32), r as i32, r as i32, color);
        }
        Mark::Up => {
            let pts = vec![
                Point::new((x) as i32, (cy + r) as i32),
                Point::new((x + size) as i32, (cy + r) as i32),
                Point::new((x + r) as i32, (cy - r) as i32),
            ];
            draw_polygon_mut(img, &pts, color);
        }
        Mark::Cross => {
            draw_line_segment_mut(img, (x, cy - r), (x + size, cy + r), color);
            draw_line_segment_mut(img, (x, cy + r), (x + size, cy - r), color);
        }
    }
}

fn draw_triangle(img: &mut GrayImage, cx: f32, cy: f32, size: f32, pointing_right: bool, fill: u8) {
    let s = size / 2.0;
    let pts = if pointing_right {
        vec![
            Point::new((cx - s) as i32, (cy - s) as i32),
            Point::new((cx - s) as i32, (cy + s) as i32),
            Point::new((cx + s) as i32, cy as i32),
        ]
    } else {
        vec![
            Point::new((cx + s) as i32, (cy - s) as i32),
            Point::new((cx + s) as i32, (cy + s) as i32),
            Point::new((cx - s) as i32, cy as i32),
        ]
    };
    draw_polygon_mut(img, &pts, Luma([fill]));
}

struct ColumnSpec<'a> {
    x0: f32,
    label: &'a str,
    subtitle: &'a str,
    arrow_left: bool,
}

fn draw_column(img: &mut GrayImage, fonts: &Fonts, spec: &ColumnSpec, services: &[&Departure]) {
    let col_w = 400.0;
    let pad = 24.0;
    let inner_x0 = spec.x0 + pad;
    let inner_w = col_w - 2.0 * pad;

    let head_y = 126.0;
    let head_cy = head_y + 9.0;
    if spec.arrow_left {
        draw_triangle(img, inner_x0 + 6.0, head_cy, 12.0, true, BLACK);
        render::draw_text(img, &fonts.arial_bold, 19.0, inner_x0 + 18.0, head_y + 15.0, spec.label, BLACK);
    } else {
        let tw = text_width(&fonts.arial_bold, 19.0, spec.label);
        render::draw_text(img, &fonts.arial_bold, 19.0, inner_x0 + inner_w - tw - 18.0, head_y + 15.0, spec.label, BLACK);
        draw_triangle(img, inner_x0 + inner_w - 6.0, head_cy, 12.0, false, BLACK);
    }
    // A destination hint, not an exhaustive list -- each row below still
    // shows that specific service's real destination. Just tells someone
    // glancing at the board which platform to stand on before they've read
    // any individual row.
    let subtitle = truncate_to_width(&fonts.arial_bold, 12.0, spec.subtitle, inner_w);
    render::draw_text(img, &fonts.arial_bold, 12.0, inner_x0, head_y + 30.0, &subtitle, DARK_GRAY);

    let services: Vec<_> = services.iter().filter(|s| s.temporal_data.departure.is_some()).take(4).collect();

    let row_y = head_y + 56.0;
    let row_h = 64.0;
    let time_col_w = 92.0;

    if services.is_empty() {
        render::draw_text(img, &fonts.arial_bold, 15.0, inner_x0, row_y + 10.0, "No departures in the next 2h", LIGHT_GRAY);
        return;
    }

    for (i, svc) in services.iter().enumerate() {
        let dep = svc.temporal_data.departure.as_ref().unwrap();
        let sched = dep.schedule_advertised.as_str();
        let live = dep.realtime_forecast.as_deref().unwrap_or(sched);
        let cancelled = dep.is_cancelled;
        let dest = svc.destination.first().map(|d| d.location.description.as_str()).unwrap_or("?");
        let origin = svc.origin.first().map(|d| d.location.description.as_str()).unwrap_or("?");

        let ry = row_y + i as f32 * row_h;
        if i > 0 {
            draw_line_segment_mut(img, (inner_x0, ry), (inner_x0 + inner_w, ry), Luma([LIGHT_GRAY]));
        }

        let mut ty = ry + if i == 0 { 10.0 } else { 15.0 };
        if i == 0 {
            draw_filled_rect_mut(img, Rect::at(inner_x0 as i32, (ry + 5.0) as i32).of_size(40, 15), Luma([BLACK]));
            render::draw_text(img, &fonts.arial_bold, 11.0, inner_x0 + 5.0, ry + 17.0, "NEXT", WHITE);
            ty = ry + 24.0;
        }

        let time_str = hhmm(sched);
        let time_color = if cancelled { DARK_GRAY } else { BLACK };
        let tw = render::draw_text(img, &fonts.mono, 26.0, inner_x0, ty + 22.0, time_str, time_color);
        if cancelled {
            draw_line_segment_mut(img, (inner_x0, ty + 9.0), (inner_x0 + tw, ty + 9.0), Luma([DARK_GRAY]));
        }

        let meta_x = inner_x0 + time_col_w;
        let meta_w = inner_x0 + inner_w - meta_x;
        render::draw_text(img, &fonts.arial_bold, 19.0, meta_x, ry + 3.0 + 17.0, dest, BLACK);

        let sub_y = ry + 27.0;
        let (mark_kind, status_text, status_color): (Mark, String, u8) = if cancelled {
            (Mark::Cross, "CANCELLED".to_string(), DARK_GRAY)
        } else if live != sched {
            (Mark::Up, format!("NOW {}", hhmm(live)), BLACK)
        } else {
            (Mark::Dot, "ON TIME".to_string(), LIGHT_GRAY)
        };

        let status_text_w = text_width(&fonts.mono, 13.0, &status_text);
        let mark_w = 12.0;
        let status_total_w = mark_w + 5.0 + status_text_w;
        let via_text = format!("FROM {}", origin.to_uppercase());
        let via_max_w = meta_w - status_total_w - 10.0;
        let via_text = truncate_to_width(&fonts.arial_bold, 13.0, &via_text, via_max_w);
        render::draw_text(img, &fonts.arial_bold, 13.0, meta_x, sub_y + 13.0, &via_text, DARK_GRAY);

        let mark_x = meta_x + meta_w - status_total_w;
        draw_status_mark(img, mark_x, sub_y + 25.0, mark_w, &mark_kind, status_color);
        render::draw_text(img, &fonts.mono, 13.0, mark_x + mark_w + 5.0, sub_y + 13.0, &status_text, status_color);
    }
}
