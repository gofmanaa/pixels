use ratatui::style::Color;
use rayon::prelude::*;

/// Render mode
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    HalfBlock, // default: "▀" fg=top bg=bottom  — 2 pixels per cell
    Ascii,     // 'a' toggle: ASCII ramp char, coloured, 1 pixel per cell
}

impl RenderMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::HalfBlock => "RGB",
            Self::Ascii => "ASCII",
        }
    }
}

/// Ordered dark → light. Each char represents a brightness band.
//pub static ASCII_RAMP: &[u8] = b"@%#*+=-:. ";
pub static ASCII_RAMP: &[u8] =
    b"$@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/|()1{}[]?-_+~<>i!lI;:,\"^`'. ";

#[inline(always)]
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    // BT.601 integer luma
    //((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8
    // Rec. 709 perceptual weights
    ((0.2126 * r as f32) + (0.7152 * g as f32) + (0.0722 * b as f32)) as u8
}

/// Map a packed RGB pixel to an ASCII character + its fg Color.
/// In ASCII mode we render a single char per cell (no half-block trick),
/// coloured with the pixel's own RGB so the image stays recognisable.
#[inline(always)]
pub fn to_ascii(px: u32) -> (char, Color) {
    let r = ((px >> 16) & 0xFF) as u8;
    let g = ((px >> 8) & 0xFF) as u8;
    let b = (px & 0xFF) as u8;
    let y = luma(r, g, b) as usize;
    //let idx = y * (ASCII_RAMP.len() - 1) / 255;
    let idx = (y * (ASCII_RAMP.len() - 1) / 255).min(ASCII_RAMP.len() - 1);
    let ch = ASCII_RAMP[idx] as char;
    (ch, Color::Rgb(r, g, b))
}

// LUT  (Y × U × V  →  packed RGB, filter-independent)
pub struct YuvLut {
    //table: Box<[u32; 256 * 256 * 256]>,
    table: Box<[u32]>,
}

impl YuvLut {
    pub fn build() -> Self {
        let mut table = vec![0u32; 256 * 256 * 256].into_boxed_slice();
        // Parallelize LUT construction across all (y, u, v) triples
        table
            .par_chunks_mut(256 * 256)
            .enumerate()
            .for_each(|(y, chunk)| {
                let yi = y as i32;
                for u in 0usize..256 {
                    let ui = u as i32 - 128;
                    for v in 0usize..256 {
                        let vi = v as i32 - 128;
                        let r = (yi + ((1436 * vi) >> 10)).clamp(0, 255) as u32;
                        let g = (yi - ((352 * ui + 731 * vi) >> 10)).clamp(0, 255) as u32;
                        let b = (yi + ((1814 * ui) >> 10)).clamp(0, 255) as u32;
                        chunk[u * 256 + v] = (r << 16) | (g << 8) | b;
                    }
                }
            });
        // let raw = Box::into_raw(table) as *mut [u32; 256 * 256 * 256];
        // Self {
        //     table: unsafe { Box::from_raw(raw) },
        // }
        Self { table }
    }

    #[inline(always)]
    pub fn lookup(&self, y: u8, u: u8, v: u8) -> u32 {
        self.table[(y as usize) << 16 | (u as usize) << 8 | (v as usize)]
    }
}

// bilinear sampling function
#[inline(always)]
pub fn sample_bilinear(frame: &[u32], width: usize, height: usize, fx: f32, fy: f32) -> u32 {
    let x0 = fx.floor().clamp(0.0, (width - 1) as f32) as usize;
    let y0 = fy.floor().clamp(0.0, (height - 1) as f32) as usize;

    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let dx = fx - x0 as f32;
    let dy = fy - y0 as f32;

    let p00 = frame[y0 * width + x0];
    let p10 = frame[y0 * width + x1];
    let p01 = frame[y1 * width + x0];
    let p11 = frame[y1 * width + x1];

    let lerp = |a: u32, b: u32, t: f32| -> f32 { a as f32 * (1.0 - t) + b as f32 * t };
    let mix = |c00: u32, c10: u32, c01: u32, c11: u32| -> u32 {
        let a = lerp(c00, c10, dx);
        let b = lerp(c01, c11, dx);
        lerp(a as u32, b as u32, dy) as u32
    };

    let r = mix(
        (p00 >> 16) & 0xFF,
        (p10 >> 16) & 0xFF,
        (p01 >> 16) & 0xFF,
        (p11 >> 16) & 0xFF,
    );
    let g = mix(
        (p00 >> 8) & 0xFF,
        (p10 >> 8) & 0xFF,
        (p01 >> 8) & 0xFF,
        (p11 >> 8) & 0xFF,
    );
    let b = mix(p00 & 0xFF, p10 & 0xFF, p01 & 0xFF, p11 & 0xFF);

    (r << 16) | (g << 8) | b
}

// temporal blending
#[inline(always)]
pub fn blend(a: u32, b: u32, alpha: f32) -> u32 {
    let lerp = |x: u32, y: u32| ((x as f32 * (1.0 - alpha)) + (y as f32 * alpha)) as u32;
    let r = lerp((a >> 16) & 0xFF, (b >> 16) & 0xFF);
    let g = lerp((a >> 8) & 0xFF, (b >> 8) & 0xFF);
    let b = lerp(a & 0xFF, b & 0xFF);
    (r << 16) | (g << 8) | b
}
