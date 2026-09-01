//! Fundamentals (layer 1): the font/drawing/quantization/packing toolkit
//! every page (plugin or not) is built from. Nothing in this file knows
//! about trains, the index, or any other specific page -- that content lives
//! in `plugin.rs` (the shared chrome) and `plugins/*` (each page).
//!
//! Text goes through fontdue with real antialiasing: each glyph's coverage
//! mask (0-255) is alpha-blended into the canvas in full 8-bit precision, and
//! only the *finished* image is quantized down to the panel's exact 2bpp
//! levels (0/85/170/255). That ordering is the whole point -- blend first,
//! quantize last -- so an edge that's 40% covered lands on a real
//! intermediate gray instead of being hard-thresholded to black or white,
//! giving genuinely smoother glyph edges than a 1-bit render ever could.

use fontdue::{Font, FontSettings};
use image::{GrayImage, Luma};

pub const W: u32 = 800;
pub const H: u32 = 480;

// Exact 2bpp quantization levels.
pub const WHITE: u8 = 255;
pub const LIGHT_GRAY: u8 = 170;
pub const DARK_GRAY: u8 = 85;
pub const BLACK: u8 = 0;

/// Fonts are embedded at compile time (`include_bytes!` from `fonts/`, OFL
/// licenses alongside), not read from system paths at runtime -- the old
/// macOS `/System/Library/Fonts/...` paths made the binary panic anywhere
/// else, and this is headed for a Linux Docker container. Both faces are
/// deliberately screen-first picks for a low-DPI 4-gray panel: Inter (tall
/// x-height, open apertures, designed for small-size screen legibility)
/// for labels/headers, JetBrains Mono (slashed zero, unambiguous 1/l/I,
/// uniform digit widths so a changing time never shifts its neighbors) for
/// times and the clock.
fn load_font(bytes: &'static [u8], name: &str) -> Font {
    Font::from_bytes(bytes, FontSettings::default()).unwrap_or_else(|e| panic!("parsing embedded font {name}: {e}"))
}

pub struct Fonts {
    /// Labels and body text (Inter Bold). Field names kept generic-by-role,
    /// not by family, so a future face swap is a one-line change here.
    pub sans_bold: Font,
    /// Large display headers (Inter Black).
    pub sans_black: Font,
    /// Times, clocks, anything tabular (JetBrains Mono Bold).
    pub mono: Font,
}

impl Fonts {
    pub fn load() -> Self {
        Fonts {
            sans_bold: load_font(include_bytes!("../fonts/Inter-Bold.ttf"), "Inter-Bold"),
            sans_black: load_font(include_bytes!("../fonts/Inter-Black.ttf"), "Inter-Black"),
            mono: load_font(include_bytes!("../fonts/JetBrainsMono-Bold.ttf"), "JetBrainsMono-Bold"),
        }
    }
}

/// Alpha-blend one glyph's antialiased coverage mask into the canvas at full
/// 8-bit precision (no quantization here -- that happens once, at the end,
/// via `quantize_to_4gray`).
fn blend_glyph(img: &mut GrayImage, px: i32, py: i32, w: usize, h: usize, coverage: &[u8], fg: u8) {
    for gy in 0..h {
        let iy = py + gy as i32;
        if iy < 0 || iy >= H as i32 {
            continue;
        }
        for gx in 0..w {
            let ix = px + gx as i32;
            if ix < 0 || ix >= W as i32 {
                continue;
            }
            let cov = coverage[gy * w + gx] as u32;
            if cov == 0 {
                continue;
            }
            let bg = img.get_pixel(ix as u32, iy as u32).0[0] as u32;
            let blended = (bg * (255 - cov) + fg as u32 * cov) / 255;
            img.put_pixel(ix as u32, iy as u32, Luma([blended as u8]));
        }
    }
}

/// Draws `text` with its baseline at (x, y). Returns the total advance width.
pub fn draw_text(img: &mut GrayImage, font: &Font, size: f32, x: f32, y: f32, text: &str, fg: u8) -> f32 {
    let mut pen_x = x;
    for ch in text.chars() {
        let (metrics, coverage) = font.rasterize(ch, size);
        let px = (pen_x + metrics.xmin as f32).round() as i32;
        let py = (y - metrics.height as f32 - metrics.ymin as f32).round() as i32;
        blend_glyph(img, px, py, metrics.width, metrics.height, &coverage, fg);
        pen_x += metrics.advance_width;
    }
    pen_x - x
}

pub fn text_width(font: &Font, size: f32, text: &str) -> f32 {
    text.chars().map(|c| font.metrics(c, size).advance_width).sum()
}

/// Truncates `text` (from the end) until it fits within `max_w` at this
/// font/size. Removes whole chars, never bytes: `String::truncate` panics on
/// a non-char-boundary cut, and calendar event titles (user-authored) freely
/// contain multi-byte chars -- one accented title must not kill the poller.
pub fn truncate_to_width(font: &Font, size: f32, text: &str, max_w: f32) -> String {
    let mut s = text.to_string();
    while text_width(font, size, &s) > max_w && s.chars().count() > 4 {
        s.pop();
    }
    s
}

/// Current wall-clock time as "HH:MM UTC", dependency-free. For a plugin
/// with no natural "as of" timestamp of its own to show in its status bar
/// (e.g. the index, or weather where the forecast's own hour-0 entry is
/// always midnight, not "now") -- labeled UTC so it's never confused with a
/// data source's own local-time values (trains converts its API's UTC
/// instants to local, GMT/BST-aware, via chrono).
pub fn current_time_utc_hhmm() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    format!("{hh:02}:{mm:02} UTC")
}

/// Quantizes the finished, fully-antialiased image down to the panel's 4 real
/// gray levels via nearest-level rounding. Called once, last, by
/// `plugin::draw_status_bar` -- individual pages don't call this themselves.
pub(crate) fn quantize_to_4gray(img: &mut GrayImage) {
    let levels = [BLACK, DARK_GRAY, LIGHT_GRAY, WHITE];
    for p in img.pixels_mut() {
        let v = p.0[0];
        let nearest = *levels.iter().min_by_key(|&&l| (l as i16 - v as i16).abs()).unwrap();
        p.0[0] = nearest;
    }
}

/// level (0=black..3=white, ascending brightness) -> panel-stored 2-bit code.
/// `(3,1,2,0)` is bb_epaper's `u8Colors_4gray` default table (py-opendisplay's
/// `_GRAY4_CODES_BASE`); our panel_ic_type (21) isn't in the per-panel
/// override list, so the default applies.
const GRAY4_CODES: [u8; 4] = [3, 1, 2, 0];

fn level_for_gray(v: u8) -> u8 {
    match v {
        BLACK => 0,
        DARK_GRAY => 1,
        LIGHT_GRAY => 2,
        _ => 3, // WHITE
    }
}

/// Packs a finished, already-4-gray-quantized image into the two 1bpp
/// controller planes the firmware expects (plane0=code bit0, plane1=code
/// bit1), each row-major, MSB-first, 8 pixels/byte -- matching
/// py-opendisplay's `encode_gray4_bitplanes` exactly. 800 is a multiple of 8
/// so no row padding is needed. Concatenated (plane0 then plane1), this is
/// the `total_size = plane_size * 2` payload the wire protocol expects.
pub fn pack_gray4_planes(img: &GrayImage) -> Vec<u8> {
    assert_eq!(W % 8, 0, "width must be a multiple of 8 for byte-aligned rows");
    let row_bytes = (W / 8) as usize;
    let mut plane0 = vec![0u8; row_bytes * H as usize];
    let mut plane1 = vec![0u8; row_bytes * H as usize];

    for y in 0..H {
        for x in 0..W {
            let v = img.get_pixel(x, y).0[0];
            let level = level_for_gray(v);
            let code = GRAY4_CODES[level as usize];
            let byte_i = (y as usize) * row_bytes + (x as usize) / 8;
            let bit = 7 - (x % 8); // MSB-first
            if code & 0x01 != 0 {
                plane0[byte_i] |= 1 << bit;
            }
            if code & 0x02 != 0 {
                plane1[byte_i] |= 1 << bit;
            }
        }
    }

    let mut out = plane0;
    out.extend_from_slice(&plane1);
    out
}
