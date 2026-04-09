use rand::RngExt;
use rand::make_rng;
use rand::rngs::SmallRng;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use rayon::prelude::*;
use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Character set
// ---------------------------------------------------------------------------

const MATRIX_CHARS: &[char] = &[
    'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', 'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l',
    'z', 'x', 'c', 'v', 'b', 'n', 'm', 'Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P', 'A', 'S',
    'D', 'F', 'G', 'H', 'J', 'K', 'L', 'Z', 'X', 'C', 'V', 'B', 'N', 'M', 'ｦ', 'ｧ', 'ｨ', 'ｩ', 'ｪ',
    'ｫ', 'ｬ', 'ｭ', 'ｮ', 'ｯ', 'ｰ', 'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ',
    'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', 'ﾏ', 'ﾐ',
    'ﾑ', 'ﾒ', 'ﾓ', 'ﾔ', 'ﾕ', 'ﾖ', 'ﾗ', 'ﾘ', 'ﾙ', 'ﾚ', 'ﾛ', 'ﾜ', 'ﾝ', '1', '2', '3', '4', '5', '6',
    '7', '8', '9', '0', '-', '=', '*', '_', '+', '|', ':', '<', '>',
];

#[inline(always)]
fn rand_matrix_char(rng: &mut SmallRng) -> char {
    MATRIX_CHARS[rng.random_range(0..MATRIX_CHARS.len())]
}

// ---------------------------------------------------------------------------
// Edge map — computed once per frame at terminal resolution
// ---------------------------------------------------------------------------

/// BT.601 luma from packed 0x00RRGGBB
#[inline(always)]
fn luma_px(px: u32) -> i32 {
    let r = ((px >> 16) & 0xFF) as i32;
    let g = ((px >> 8) & 0xFF) as i32;
    let b = (px & 0xFF) as i32;
    (77 * r + 150 * g + 29 * b) >> 8
}

/// Sample camera frame at terminal cell (tx, ty) - nearest neighbour.
#[inline(always)]
fn sample_cam(
    frame: &[u32],
    cam_w: usize,
    cam_h: usize,
    tx: usize,
    ty: usize,
    term_w: usize,
    term_h: usize,
) -> u32 {
    let cx = ((tx * cam_w) / term_w.max(1)).min(cam_w.saturating_sub(1));
    let cy = ((ty * cam_h) / term_h.max(1)).min(cam_h.saturating_sub(1));
    frame[cy * cam_w + cx]
}

/// Build a flat edge-strength map at terminal resolution (term_w × term_h).
/// Values are 0..=255 (u8) - avoids f32 in the per-cell hot path.
/// Sobel is applied on the already-downscaled luma grid, which is fast and
/// avoids redundant camera fetches inside each Column::render call.
pub fn build_edge_map(
    frame: &[u32],
    cam_w: usize,
    cam_h: usize,
    term_w: usize,
    term_h: usize,
) -> Vec<u8> {
    // Step 1: build luma grid at terminal resolution in parallel
    let luma_grid: Vec<i32> = (0..term_h * term_w)
        .into_par_iter()
        .map(|i| {
            let tx = i % term_w;
            let ty = i / term_w;
            let px = sample_cam(frame, cam_w, cam_h, tx, ty, term_w, term_h);
            luma_px(px)
        })
        .collect();

    // Step 2: Sobel on the luma grid in parallel → u8 edge map
    let get = |x: usize, y: usize| -> i32 {
        let x = x.min(term_w.saturating_sub(1));
        let y = y.min(term_h.saturating_sub(1));
        luma_grid[y * term_w + x]
    };

    (0..term_h * term_w)
        .into_par_iter()
        .map(|i| {
            let tx = i % term_w;
            let ty = i / term_w;

            let xm = tx.saturating_sub(1);
            let xp = (tx + 1).min(term_w.saturating_sub(1));
            let ym = ty.saturating_sub(1);
            let yp = (ty + 1).min(term_h.saturating_sub(1));

            let tl = get(xm, ym);
            let tc = get(tx, ym);
            let tr = get(xp, ym);
            let ml = get(xm, ty);
            let mr = get(xp, ty);
            let bl = get(xm, yp);
            let bc = get(tx, yp);
            let br = get(xp, yp);

            let gx = -tl - 2 * ml - bl + tr + 2 * mr + br;
            let gy = -tl - 2 * tc - tr + bl + 2 * bc + br;

            // Integer sqrt approximation: use isqrt via u32 cast
            let mag = ((gx * gx + gy * gy) as f32).sqrt();
            // Normalise: 800 maps typical real-world edges to ~1.0
            ((mag / 800.0).min(1.0) * 255.0) as u8
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Color — integer only, no powf in the hot path
// ---------------------------------------------------------------------------

/// Precomputed sharpening curve: maps edge u8 (0..=255) → blend factor u8.
/// Equivalent to powf(e/255, 0.4) * 255, computed once at startup.
struct EdgeCurve([u8; 256]);

impl EdgeCurve {
    fn build() -> Self {
        let mut t = [0u8; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = ((i as f32 / 255.0).powf(0.4) * 255.0).round() as u8;
        }
        Self(t)
    }

    #[inline(always)]
    fn get(&self, edge: u8) -> u8 {
        self.0[edge as usize]
    }
}

// Global curve — built once, used read-only everywhere
static EDGE_CURVE: std::sync::OnceLock<EdgeCurve> = std::sync::OnceLock::new();

fn edge_curve() -> &'static EdgeCurve {
    EDGE_CURVE.get_or_init(EdgeCurve::build)
}

/// Map cell age + edge strength (0..=255) → Matrix green Color.
/// Flat areas: near-black. Edges: classic age-based green.
#[inline(always)]
fn matrix_color(age: u16, edge: u8) -> Color {
    let (r_e, g_e, b_e): (u8, u8, u8) = match age {
        0 => (220, 255, 220), // head  — near white
        1 => (120, 250, 120), // neck  — bright green
        2..=4 => (0, 200, 0), // upper trail
        5..=9 => (0, 140, 0), // mid trail
        _ => (0, 70, 0),      // deep tail
    };

    // Flat pole: nearly invisible
    const R_F: u8 = 0;
    const G_F: u8 = 50;
    const B_F: u8 = 0;

    // t is the sharpened blend factor (0=flat, 255=full edge)
    let t = edge_curve().get(edge) as u32;

    let blend = |flat: u8, on_edge: u8| -> u8 {
        let f = flat as u32;
        let e = on_edge as u32;
        (f + ((e - f) * t / 255)) as u8
    };

    Color::Rgb(blend(R_F, r_e), blend(G_F, g_e), blend(B_F, b_e))
}

/// Background for blank cells: black on flat, dim green glow on edges.
#[inline(always)]
fn bg_color(edge: u8) -> Color {
    // sqrt curve: (edge/255)^0.5 * 80
    let t = edge_curve().get(edge) as u32;
    let g = (t * 90 / 255) as u8;
    //let b = (t * 40 / 255) as u8;
    Color::Rgb(0, g, 0)
}

// ---------------------------------------------------------------------------
// Cell / Drop / Column
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Cell {
    ch: char,
    age: u16,
}

struct Drop {
    row: u16,
    length: u16,
    erase: bool,
    rng: SmallRng,
}

impl Drop {
    fn new(length: u16, erase: bool) -> Self {
        Self {
            row: 0,
            length,
            erase,
            rng: make_rng(),
        }
    }

    #[inline(always)]
    fn rand_char(&mut self) -> char {
        rand_matrix_char(&mut self.rng)
    }
}

struct Column {
    height: u16,
    drops: VecDeque<Drop>,
    cells: Vec<Option<Cell>>,
    wait: u16,
    rng: SmallRng,
}

impl Column {
    fn new(height: u16) -> Self {
        let mut rng: SmallRng = make_rng();
        let wait = rng.random_range(0..height.max(1));
        Self {
            height,
            drops: VecDeque::new(),
            cells: vec![None; height as usize],
            wait,
            rng,
        }
    }

    fn spawn(&mut self) {
        let length = self.rng.random_range(4..self.height.max(5));
        let erase = self.drops.back().is_some_and(|d| !d.erase);
        self.drops.push_back(Drop::new(length, erase));
        self.wait = self.rng.random_range(2..self.height.max(3));
    }

    fn tick(&mut self) {
        for c in self.cells.iter_mut().flatten() {
            c.age += 1;
        }

        let mut to_remove: Vec<usize> = Vec::new();

        for (i, drop) in self.drops.iter_mut().enumerate() {
            let row = drop.row as usize;
            if row < self.cells.len() {
                if drop.erase {
                    self.cells[row] = None;
                } else {
                    self.cells[row] = Some(Cell {
                        ch: drop.rand_char(),
                        age: 0,
                    });
                    if drop.row > 1 && drop.rng.random_bool(0.15) {
                        let lo = drop.row.saturating_sub(drop.length) as usize;
                        let hi = drop.row as usize;
                        if hi > lo {
                            let target = drop.rng.random_range(lo..hi);
                            if let Some(c) = self.cells.get_mut(target).and_then(|s| s.as_mut()) {
                                c.ch = drop.rand_char();
                            }
                        }
                    }
                }
            }

            drop.row += 1;

            let tail = drop.row.saturating_sub(drop.length) as usize;
            if tail < self.cells.len()
                && let Some(c) = &self.cells[tail]
                && c.age > drop.length + 2
            {
                self.cells[tail] = None;
            }

            if drop.row > self.height + drop.length {
                to_remove.push(i);
            }
        }

        for i in to_remove.into_iter().rev() {
            self.drops.remove(i);
        }

        if self.wait == 0 {
            self.spawn();
        } else {
            self.wait -= 1;
        }
    }

    /// Render using the pre-built edge map (no Sobel inside the hot path).
    fn render(&self, col_idx: usize, edge_map: &[u8], term_w: usize) -> Vec<(char, Color, Color)> {
        self.cells
            .iter()
            .enumerate()
            .map(|(row, slot)| {
                let edge = edge_map.get(row * term_w + col_idx).copied().unwrap_or(0);
                let bg = bg_color(edge);
                match slot {
                    Some(c) => (c.ch, matrix_color(c.age, edge), bg),
                    None => (' ', bg, bg),
                }
            })
            .collect()
    }
}
pub struct MatrixState {
    columns: Vec<Column>,
}

impl MatrixState {
    pub fn new(term_w: u16, term_h: u16) -> Self {
        // Warm up the edge curve LUT on first construction
        let _ = edge_curve();
        let columns = (0..term_w).map(|_| Column::new(term_h)).collect();
        Self { columns }
    }

    fn tick(&mut self) {
        self.columns
            .par_iter_mut()
            .for_each(|col: &mut Column| col.tick());
    }

    /// Build ratatui `Line`s from the camera frame.
    /// Edge map is computed once here in parallel, then reused per column.
    pub fn render_lines(
        &mut self,
        frame: &[u32],
        cam_w: usize,
        cam_h: usize,
        term_w: usize,
        term_h: usize,
        paused: bool,
    ) -> Vec<Line<'static>> {
        if !paused {
            self.tick();
        }
        // One parallel pass over the terminal grid -> edge map
        let edge_map = build_edge_map(frame, cam_w, cam_h, term_w, term_h);

        // Render columns in parallel, each reading from the shared edge map
        let col_data: Vec<Vec<(char, Color, Color)>> = self
            .columns
            .par_iter()
            .enumerate()
            .map(|(col_idx, col): (usize, &Column)| col.render(col_idx, &edge_map, term_w))
            .collect();

        (0..term_h)
            .map(|row| {
                let spans: Vec<Span<'static>> = (0..term_w)
                    .rev()
                    .map(|col| {
                        let (ch, fg, bg) = col_data
                            .get(col)
                            .and_then(|rows| rows.get(row))
                            .copied()
                            .unwrap_or((' ', Color::Black, Color::Black));
                        Span::styled(ch.to_string(), Style::default().fg(fg).bg(bg))
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    }
}
