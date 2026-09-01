//! Small multi-series line-chart toolkit (fundamentals, layer 1). Shared by
//! any plugin plotting several time series on the panel's 4 real gray
//! levels -- no color to distinguish series with, so each `Series` carries
//! its own line style (solid/dashed/long-dash/dotted) and point marker
//! (dot/cross/triangle/square) instead, plus a legend that spells out which
//! is which. Used by `plugins::air_quality`; nothing here is pollen-specific.

use image::{GrayImage, Luma};
use imageproc::drawing::{draw_filled_ellipse_mut, draw_filled_rect_mut, draw_line_segment_mut, draw_polygon_mut};
use imageproc::point::Point;
use imageproc::rect::Rect;

use crate::render::{self, text_width, Fonts, DARK_GRAY};

pub enum LineStyle {
    Solid,
    Dashed,
    LongDash,
    Dotted,
}

pub enum Marker {
    Dot,
    Cross,
    Triangle,
    Square,
}

pub struct Series<'a> {
    pub label: &'a str,
    pub values: &'a [f32],
    pub style: LineStyle,
    pub marker: Marker,
}

/// Draws one series as a connected line across `points` (already mapped to
/// pixel coordinates by the caller), plus a marker at every point.
pub fn draw_series(img: &mut GrayImage, points: &[(f32, f32)], style: &LineStyle, marker: &Marker, color: u8) {
    draw_series_line(img, points, style, color);
    for &(x, y) in points {
        draw_marker(img, x, y, marker, color);
    }
}

fn draw_series_line(img: &mut GrayImage, points: &[(f32, f32)], style: &LineStyle, color: u8) {
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        match style {
            LineStyle::Solid => draw_line_segment_mut(img, (x0, y0), (x1, y1), Luma([color])),
            LineStyle::Dashed => draw_patterned_segment(img, (x0, y0), (x1, y1), color, 8.0, 6.0),
            LineStyle::LongDash => draw_patterned_segment(img, (x0, y0), (x1, y1), color, 16.0, 8.0),
            LineStyle::Dotted => draw_patterned_segment(img, (x0, y0), (x1, y1), color, 2.0, 5.0),
        }
    }
}

/// Walks from `a` to `b` in fixed-length steps, alternately drawing `on_len`
/// worth of segment and skipping `off_len` -- one implementation covers
/// "dashed", "long-dash", and "dotted" by varying the two lengths, rather
/// than three near-duplicate functions.
fn draw_patterned_segment(img: &mut GrayImage, a: (f32, f32), b: (f32, f32), color: u8, on_len: f32, off_len: f32) {
    let (ax, ay) = a;
    let (bx, by) = b;
    let dx = bx - ax;
    let dy = by - ay;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.01 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let mut travelled = 0.0;
    let mut on = true;
    while travelled < len {
        let step = if on { on_len } else { off_len };
        let next = (travelled + step).min(len);
        if on {
            let p0 = (ax + ux * travelled, ay + uy * travelled);
            let p1 = (ax + ux * next, ay + uy * next);
            draw_line_segment_mut(img, p0, p1, Luma([color]));
        }
        travelled = next;
        on = !on;
    }
}

fn draw_marker(img: &mut GrayImage, x: f32, y: f32, marker: &Marker, color: u8) {
    let r = 4.0;
    match marker {
        Marker::Dot => {
            draw_filled_ellipse_mut(img, (x as i32, y as i32), r as i32, r as i32, Luma([color]));
        }
        Marker::Cross => {
            draw_line_segment_mut(img, (x - r, y - r), (x + r, y + r), Luma([color]));
            draw_line_segment_mut(img, (x - r, y + r), (x + r, y - r), Luma([color]));
        }
        Marker::Triangle => {
            let pts = vec![
                Point::new(x as i32, (y - r) as i32),
                Point::new((x - r) as i32, (y + r) as i32),
                Point::new((x + r) as i32, (y + r) as i32),
            ];
            draw_polygon_mut(img, &pts, Luma([color]));
        }
        Marker::Square => {
            draw_filled_rect_mut(img, Rect::at((x - r) as i32, (y - r) as i32).of_size((r * 2.0) as u32, (r * 2.0) as u32), Luma([color]));
        }
    }
}

/// One legend row: a short line+marker sample followed by the series label,
/// left to right starting at `(x0, y)`. `y` is the label's text baseline;
/// the sample sits a few pixels above it.
pub fn draw_legend(img: &mut GrayImage, fonts: &Fonts, series: &[Series], x0: f32, y: f32, color: u8) {
    let mut x = x0;
    for s in series {
        let sample_w = 28.0;
        let points = [(x, y - 4.0), (x + sample_w, y - 4.0)];
        draw_series(img, &points, &s.style, &s.marker, color);
        render::draw_text(img, &fonts.sans_bold, 13.0, x + sample_w + 8.0, y, s.label, DARK_GRAY);
        x += sample_w + 8.0 + text_width(&fonts.sans_bold, 13.0, s.label) + 24.0;
    }
}
