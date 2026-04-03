#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Normal,    // 1 — passthrough
    Grayscale, // 2 — luminance only
    Invert,    // 3 — bitwise NOT per channel
    Sepia,     // 4 — warm brownish tone
    RedBoost,  // 5 — red channel ×1.4, others ×0.7
    CoolBlue,  // 6 — blue channel ×1.4, red ×0.7
    Threshold, // 7 — posterise to black or white
    Scanlines, // 8 — every other row darkened 50%
    Vaporwave, // 9 — swap R↔B, boost G
    Noir,      // 0 — high-contrast grayscale
}

impl Filter {
    pub fn from_key(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::Normal),
            '2' => Some(Self::Grayscale),
            '3' => Some(Self::Invert),
            '4' => Some(Self::Sepia),
            '5' => Some(Self::RedBoost),
            '6' => Some(Self::CoolBlue),
            '7' => Some(Self::Threshold),
            '8' => Some(Self::Scanlines),
            '9' => Some(Self::Vaporwave),
            '0' => Some(Self::Noir),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "1 Normal",
            Self::Grayscale => "2 Grayscale",
            Self::Invert => "3 Invert",
            Self::Sepia => "4 Sepia",
            Self::RedBoost => "5 Red boost",
            Self::CoolBlue => "6 Cool blue",
            Self::Threshold => "7 Threshold",
            Self::Scanlines => "8 Scanlines",
            Self::Vaporwave => "9 Vaporwave",
            Self::Noir => "0 Noir",
        }
    }

    /// Apply filter to a packed 0x00RRGGBB pixel.
    /// `row` is the terminal row index, used by Scanlines.
    #[inline(always)]
    pub fn apply(self, px: u32, row: usize) -> u32 {
        let r = ((px >> 16) & 0xFF) as u8;
        let g = ((px >> 8) & 0xFF) as u8;
        let b = (px & 0xFF) as u8;

        let (r, g, b): (u8, u8, u8) = match self {
            Self::Normal => (r, g, b),

            Self::Grayscale => {
                // BT.601 luma
                let y = (77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8;
                let y = y as u8;
                (y, y, y)
            }

            Self::Invert => (!r, !g, !b),

            Self::Sepia => {
                let ri = r as u32;
                let gi = g as u32;
                let bi = b as u32;
                let nr = ((ri * 112 + gi * 88 + bi * 56) >> 8).min(255) as u8;
                let ng = ((ri * 100 + gi * 78 + bi * 50) >> 8).min(255) as u8;
                let nb = ((ri * 78 + gi * 62 + bi * 39) >> 8).min(255) as u8;
                (nr, ng, nb)
            }

            Self::RedBoost => (
                ((r as u32 * 358) >> 8).min(255) as u8, // ×1.4
                ((g as u32 * 179) >> 8).min(255) as u8, // ×0.7
                ((b as u32 * 179) >> 8).min(255) as u8,
            ),

            Self::CoolBlue => (
                ((r as u32 * 179) >> 8).min(255) as u8,
                ((g as u32 * 179) >> 8).min(255) as u8,
                ((b as u32 * 358) >> 8).min(255) as u8,
            ),

            Self::Threshold => {
                let y = (77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8;
                let v = if y > 127 { 255 } else { 0 };
                (v, v, v)
            }

            Self::Scanlines => {
                if row.is_multiple_of(2) {
                    (r, g, b)
                } else {
                    (r / 2, g / 2, b / 2)
                }
            }

            Self::Vaporwave => {
                // R↔B swap + green nudge
                let ng = ((g as u32 * 230) >> 8).min(255) as u8;
                (b, ng, r)
            }

            Self::Noir => {
                // High-contrast grayscale: steeper S-curve via simple stretch
                let y = (77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8;
                let y = if y < 64 {
                    y / 2
                } else if y > 192 {
                    (y + 255) / 2
                } else {
                    y
                } as u8;
                (y, y, y)
            }
        };

        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }
}
