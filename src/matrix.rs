use rand::RngExt;
use rand::make_rng;
use rand::rngs::SmallRng;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use std::collections::VecDeque;

// CONFIG

/// fade speed (lower = faster fade)
const ENERGY_DECAY: u16 = 200;

// Character set

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

/// pixel difference
#[inline(always)]
fn diff_px(a: u32, b: u32) -> u8 {
    let la = luma_px(a);
    let lb = luma_px(b);
    (la - lb).abs().min(255) as u8
}

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
fn matrix_color(age: u16, energy: u8) -> Color {
    if energy > 200 {
        return Color::Rgb(0, 255, 50);
    }

    let (r_e, g_e, b_e): (u8, u8, u8) = match age {
        0 => (220, 255, 220),
        1 => (120, 250, 120),
        2..=4 => (0, 200, 0),
        5..=9 => (0, 140, 0),
        _ => (0, 110, 0),
    };

    const R_F: u8 = 0;
    const G_F: u8 = 50;
    const B_F: u8 = 0;

    let t = edge_curve().get(energy) as u32;

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

// Cell / Drop / Column

#[derive(Clone)]
struct Cell {
    ch: char,
    age: u16,
    energy: u8,
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

    fn tick(&mut self, col_idx: usize, combined_map: &[u8], term_w: usize) {
        // decay existing cells
        for (row, c) in self.cells.iter_mut().enumerate() {
            if let Some(cell) = c {
                cell.age += 1;
                cell.energy = (cell.energy as u16 * ENERGY_DECAY / 255) as u8;

                let idx = row * term_w + col_idx;
                let motion = combined_map.get(idx).copied().unwrap_or(0);
                cell.energy = cell.energy.max(motion);
            }
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
                        energy: 0,
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
    fn render(
        &self,
        col_idx: usize,
        combined_map: &[u8],
        term_w: usize,
    ) -> Vec<(char, Color, Color)> {
        self.cells
            .iter()
            .enumerate()
            .map(|(row, slot)| {
                let edge = combined_map
                    .get(row * term_w + col_idx)
                    .copied()
                    .unwrap_or(0);
                let bg = bg_color(edge);
                match slot {
                    Some(c) => (c.ch, matrix_color(c.age, c.energy), bg),
                    None => (' ', bg, bg),
                }
            })
            .collect()
    }
}

pub struct MatrixState {
    columns: Vec<Column>,
    term_w: u16,
    term_h: u16,
}

impl MatrixState {
    pub fn new(term_w: u16, term_h: u16) -> Self {
        // Warm up the edge curve LUT on first construction
        let _ = edge_curve();
        let columns = (0..term_w).map(|_| Column::new(term_h)).collect();
        Self { columns, term_w, term_h }
    }

    /// Build ratatui `Line`s from the camera frame.
    #[allow(clippy::too_many_arguments)]
    pub fn render_lines(
        &mut self,
        frame: &[u32],
        prev_frame: &[u32],
        cam_w: usize,
        cam_h: usize,
        term_w: usize,
        term_h: usize,
        paused: bool,
    ) -> Vec<Line<'static>> {
        self.resize(term_w as u16, term_h as u16);
        // build maps
        let map = build_combined_map(frame, prev_frame, cam_w, cam_h, term_w, term_h);

        if !paused {
            self.columns
                .iter_mut()
                .enumerate()
                .for_each(|(i, col)| col.tick(i, &map, term_w));
        }

        // render
        let col_data: Vec<Vec<(char, Color, Color)>> = self
            .columns
            .iter()
            .enumerate()
            .map(|(col_idx, col)| col.render(col_idx, &map, term_w))
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

    fn resize(&mut self, new_w: u16, new_h: u16) {
        if new_w == self.term_w && new_h == self.term_h {
            return;
        }

        self.columns = (0..new_w).map(|_| Column::new(new_h)).collect();

        self.term_w = new_w;
        self.term_h = new_h;
    }
}


pub fn build_combined_map(
    frame: &[u32],
    prev_frame: &[u32],
    cam_w: usize,
    cam_h: usize,
    term_w: usize,
    term_h: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; term_w * term_h];

    // rolling luma rows (cache-friendly)
    let mut row_prev = vec![0i32; term_w];
    let mut row_curr = vec![0i32; term_w];
    let mut row_next = vec![0i32; term_w];

    // helper: fill one row
    let fill_row = |row_buf: &mut [i32], ty: usize| {
        for (tx, dst) in row_buf.iter_mut().enumerate() {
            let px = sample_cam(frame, cam_w, cam_h, tx, ty, term_w, term_h);
            *dst = luma_px(px);
        }
    };

    // init first rows
    fill_row(&mut row_curr, 0);
    fill_row(&mut row_next, 1.min(term_h - 1));

    for ty in 0..term_h {
        // rotate rows (no realloc)
        std::mem::swap(&mut row_prev, &mut row_curr);
        std::mem::swap(&mut row_curr, &mut row_next);

        if ty + 1 < term_h {
            fill_row(&mut row_next, ty + 1);
        }

        for tx in 0..term_w {
            let xm = tx.saturating_sub(1);
            let xp = (tx + 1).min(term_w - 1);

            // Sobel (from cached rows)
            let tl = row_prev[xm];
            let tc = row_prev[tx];
            let tr = row_prev[xp];

            let ml = row_curr[xm];
            let mr = row_curr[xp];

            let bl = row_next[xm];
            let bc = row_next[tx];
            let br = row_next[xp];

            let gx = -tl - 2 * ml - bl + tr + 2 * mr + br;
            let gy = -tl - 2 * tc - tr + bl + 2 * bc + br;

            // let mag = ((gx * gx + gy * gy) as f32).sqrt();
            // let edge = ((mag / 800.0).min(1.0) * 255.0) as u8;
            // optimization
            let mag = (gx * gx + gy * gy) as u32;
            let edge = ((mag >> 12).min(255)) as u8;

            // motion (inline, no extra pass)
            let a = sample_cam(frame, cam_w, cam_h, tx, ty, term_w, term_h);
            let b = sample_cam(prev_frame, cam_w, cam_h, tx, ty, term_w, term_h);
            let motion = diff_px(a, b);

            // combine
            let boosted = (motion as u16 * 2).min(255) as u8;
            let combined = edge.max(boosted);

            out[ty * term_w + tx] = combined;
        }
    }

    out
}