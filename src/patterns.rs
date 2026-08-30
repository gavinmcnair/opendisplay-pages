//! Synthetic test images for probing compression behaviour -- not part of the
//! normal render path. Each pattern is already quantized to the panel's 4
//! real gray levels, same as `render::render`'s output.

use crate::render::{BLACK, DARK_GRAY, LIGHT_GRAY, W, H, WHITE};
use image::{GrayImage, Luma};

const LEVELS: [u8; 4] = [BLACK, DARK_GRAY, LIGHT_GRAY, WHITE];

/// Deceptively easy case: after bit-packing to 1bpp planes, a 1px checkerboard
/// becomes a literal repeating 0xAA/0x55 byte pattern -- about the most
/// LZ77-friendly input possible, not a stress test.
pub fn checkerboard() -> GrayImage {
    GrayImage::from_fn(W, H, |x, y| Luma([if (x + y) % 2 == 0 { BLACK } else { WHITE }]))
}

/// Mandelbrot escape-time, quantized to 4 levels. Smooth interior/exterior
/// regions still compress well once bit-packed -- included for comparison,
/// not as the adversarial case.
pub fn fractal() -> GrayImage {
    GrayImage::from_fn(W, H, |x, y| {
        let cx = (x as f64 / W as f64) * 3.0 - 2.0;
        let cy = (y as f64 / H as f64) * 2.0 - 1.0;
        let (mut zx, mut zy) = (0.0f64, 0.0f64);
        let mut iter = 0u32;
        const MAX_ITER: u32 = 48;
        while zx * zx + zy * zy < 4.0 && iter < MAX_ITER {
            let nzx = zx * zx - zy * zy + cx;
            zy = 2.0 * zx * zy + cy;
            zx = nzx;
            iter += 1;
        }
        Luma([LEVELS[(iter % 4) as usize]])
    })
}

/// splitmix64 -- a tiny, dependency-free PRNG. Only used to generate an
/// incompressible test pattern, no need for cryptographic quality.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

/// True worst case: independent uniform-random level per pixel. Maximal
/// entropy -- no lossless compressor can shrink this.
pub fn noise(seed: u64) -> GrayImage {
    let mut rng = SplitMix64(seed);
    let mut bits: u64 = 0;
    let mut bits_left = 0u32;
    GrayImage::from_fn(W, H, |_, _| {
        if bits_left < 2 {
            bits = rng.next();
            bits_left = 64;
        }
        let level = (bits & 0b11) as usize;
        bits >>= 2;
        bits_left -= 2;
        Luma([LEVELS[level]])
    })
}
