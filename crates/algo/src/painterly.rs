//! Painterly rendering: turn a picture into layers of brush strokes.
//!
//! Clean-room, from the published method of Aaron Hertzmann, "Painterly Rendering with Curved
//! Brush Strokes of Multiple Sizes" (SIGGRAPH 1998). The picture is painted in passes, largest
//! brush first. Each pass blurs the picture in proportion to the brush, measures where the canvas
//! still differs from it, and lays a curved stroke wherever the error in a grid cell is too high.
//! A stroke starts at the worst point, takes the colour there and follows the direction of
//! constant lightness (perpendicular to the gradient), smoothed by a curvature filter, until it
//! reaches its maximum length or the canvas already matches the picture better than the stroke
//! would. Big brushes lay the broad shapes; smaller ones come back only where detail needs them.
//!
//! Strokes are clipped at edges, as in the paper: a stroke stops where its path would cross a
//! strong edge of the picture, and its brush never paints across one, so paint stays inside
//! shapes instead of smearing a colour over the boundary. The edge map is the gradient of the
//! lightly blurred luminance; [`Params::clip`] sets how faint an edge still counts.
//!
//! Bristle texture ([`Params::bristles`]): each stroke carries a fixed set of bristles across its
//! width. A pixel's bristle is found from its distance to the side of the stroke's path, so the
//! streaks run along the stroke and bend with it. Each bristle thins the paint (opacity) and
//! nudges its lightness a little, which reads as dry-brush streaks rather than flat blobs.
//!
//! Every pass is returned on its own transparent layer, so the caller can mask, fade or repaint
//! any of them. Output is deterministic for a given seed. Strokes are stamped as soft discs; a
//! stroke never doubles its own opacity where it overlaps itself.

use crate::matting::gaussian_blur;

/// Largest picture accepted (pixels).
pub const MAX_PIXELS: usize = 64 * 1024 * 1024;
/// Most passes (brush sizes) per rendering.
pub const MAX_PASSES: usize = 6;
/// Brush radius limits in pixels.
pub const MIN_RADIUS: f32 = 1.0;
pub const MAX_RADIUS: f32 = 256.0;
/// Stroke length limits, in steps (one step is half a brush radius).
pub const MAX_STROKE_STEPS: u32 = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Brush radii in pixels, any order; they are painted largest first.
    pub radii: Vec<f32>,
    /// Grid cell size as a multiple of the brush radius (Hertzmann's `fg`).
    pub grid: f32,
    /// Mean colour error (0..1) in a cell above which a stroke is laid there (`T`).
    pub threshold: f32,
    /// 0 = strokes bend freely with the picture, 1 = they run straight (`fc`).
    pub straightness: f32,
    /// Blur applied to the picture before sampling, as a multiple of the brush radius (`fσ`).
    pub blur: f32,
    /// Stroke opacity, 0..=1.
    pub opacity: f32,
    /// Shortest and longest stroke, in steps.
    pub min_steps: u32,
    pub max_steps: u32,
    /// Random colour variation per stroke, 0..=1.
    pub jitter: f32,
    /// Bristle texture depth, 0..=1: 0 = smooth strokes, 1 = strongly streaked.
    pub bristles: f32,
    /// Edge clipping, 0..=1: 0 = strokes may cross anything, 1 = even faint edges stop them.
    pub clip: f32,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params { radii: vec![16.0, 8.0, 4.0], grid: 1.0, threshold: 0.08, straightness: 0.5, blur: 0.5, opacity: 1.0, min_steps: 4, max_steps: 16, jitter: 0.2, bristles: 0.0, clip: 0.0, seed: 1 }
    }
}

impl Params {
    /// The same parameters with every field forced into its valid range, NaN included.
    pub fn sanitized(&self) -> Params {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let mut radii: Vec<f32> = self.radii.iter().map(|r| f(*r, MIN_RADIUS, MAX_RADIUS, 8.0)).collect();
        radii.sort_by(|a, b| b.total_cmp(a));
        radii.dedup();
        radii.truncate(MAX_PASSES);
        if radii.is_empty() {
            radii.push(8.0);
        }
        let min_steps = self.min_steps.clamp(1, MAX_STROKE_STEPS);
        Params {
            radii,
            grid: f(self.grid, 0.5, 4.0, 1.0),
            threshold: f(self.threshold, 0.0, 1.0, 0.08),
            straightness: f(self.straightness, 0.0, 1.0, 0.5),
            blur: f(self.blur, 0.0, 2.0, 0.5),
            opacity: f(self.opacity, 0.05, 1.0, 1.0),
            min_steps,
            max_steps: self.max_steps.clamp(min_steps, MAX_STROKE_STEPS),
            jitter: f(self.jitter, 0.0, 1.0, 0.2),
            bristles: f(self.bristles, 0.0, 1.0, 0.0),
            clip: f(self.clip, 0.0, 1.0, 0.0),
            seed: self.seed,
        }
    }
}

/// The named looks. Brush sizes scale with the picture, so a preset reads the same on a phone
/// photo and a poster.
pub const PRESETS: [&str; 4] = ["impressionist", "oil", "pointillist", "wash"];

/// Starting parameters for preset `name` on a picture whose longest side is `longest` pixels.
pub fn preset(name: &str, longest: usize) -> Option<Params> {
    let px = |pct: f32| (longest as f32 * pct / 100.0).clamp(MIN_RADIUS, MAX_RADIUS);
    Some(match name {
        "impressionist" => Params { radii: vec![px(1.6), px(0.8), px(0.4)], threshold: 0.08, straightness: 0.4, min_steps: 4, max_steps: 14, jitter: 0.3, clip: 0.5, bristles: 0.35, ..Params::default() },
        "oil" => Params { radii: vec![px(1.2), px(0.6), px(0.3)], threshold: 0.06, straightness: 0.3, blur: 0.4, opacity: 0.9, min_steps: 6, max_steps: 24, jitter: 0.15, clip: 0.6, bristles: 0.6, ..Params::default() },
        "pointillist" => Params { radii: vec![px(0.5), px(0.3)], threshold: 0.04, straightness: 1.0, min_steps: 1, max_steps: 1, jitter: 0.5, clip: 0.3, ..Params::default() },
        "wash" => Params { radii: vec![px(2.4), px(1.2)], threshold: 0.12, straightness: 0.5, blur: 0.8, opacity: 0.35, min_steps: 6, max_steps: 20, jitter: 0.1, clip: 0.25, bristles: 0.15, ..Params::default() },
        _ => return None,
    })
}

/// One brush size's strokes on a transparent layer (straight RGBA, row-major, `w × h`).
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    pub radius: f32,
    pub strokes: usize,
    pub px: Vec<[f32; 4]>,
}

/// xorshift64*: small, deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `0..1`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (r, g, bl) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (r * r + g * g + bl * bl).sqrt() * 0.577_350_3
}

/// A deterministic hash of two integers to `0..1`.
fn hash01(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0xC2B2_AE3D;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Smooth 1-D noise in `0..1`: the bristle at lateral position `u` (in bristle widths) of the
/// stroke numbered `phase`.
fn bristle(phase: u32, u: f32) -> f32 {
    let (i, f) = (u.floor(), u - u.floor());
    let t = f * f * (3.0 - 2.0 * f);
    let a = hash01(phase, i as i32 as u32);
    let b = hash01(phase, (i as i32).wrapping_add(1) as u32);
    a + (b - a) * t
}

/// Strong edges of the picture, for clipping strokes.
struct Edges {
    w: usize,
    h: usize,
    mask: Vec<bool>,
    /// One flag per 8×8 block: does it hold any edge pixel (a cheap reject before marching).
    blocks: Vec<bool>,
    bw: usize,
}

const BLOCK: usize = 8;

impl Edges {
    /// `None` when clipping is off.
    fn build(channels: &[Vec<f32>; 3], w: usize, h: usize, clip: f32) -> Option<Edges> {
        if clip <= 0.0 {
            return None;
        }
        let lum: Vec<f32> = (0..w * h).map(|i| 0.3 * channels[0][i] + 0.59 * channels[1][i] + 0.11 * channels[2][i]).collect();
        let lum = gaussian_blur(&lum, w, h, 1.0);
        let thr = 0.12 - 0.10 * clip;
        let at = |x: usize, y: usize| lum.get(y * w + x).copied().unwrap_or(0.0);
        let mut mask = vec![false; w * h];
        let bw = w.div_ceil(BLOCK);
        let mut blocks = vec![false; bw * h.div_ceil(BLOCK)];
        for y in 0..h {
            let (y0, y1) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in 0..w {
                let (x0, x1) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let (gx, gy) = ((at(x1, y) - at(x0, y)) * 0.5, (at(x, y1) - at(x, y0)) * 0.5);
                if gx * gx + gy * gy > thr * thr {
                    mask[y * w + x] = true;
                    blocks[(y / BLOCK) * bw + x / BLOCK] = true;
                }
            }
        }
        Some(Edges { w, h, mask, blocks, bw })
    }

    fn is_edge(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.mask.get(y as usize * self.w + x as usize).copied().unwrap_or(false)
    }

    /// Whether any edge pixel may lie in the box (inclusive, in pixels).
    fn any_near(&self, x0: i64, y0: i64, x1: i64, y1: i64) -> bool {
        let (bx0, bx1) = (x0.max(0) as usize / BLOCK, (x1.max(0) as usize / BLOCK).min(self.bw.saturating_sub(1)));
        let (by0, by1) = (y0.max(0) as usize / BLOCK, (y1.max(0) as usize / BLOCK).min(self.h.div_ceil(BLOCK).saturating_sub(1)));
        (by0..=by1).any(|by| (bx0..=bx1).any(|bx| self.blocks.get(by * self.bw + bx).copied().unwrap_or(false)))
    }

    /// Whether the straight path `a → b` crosses an edge: runs into an edge band and out the other
    /// side. A path that ends inside a band doesn't count (so band pixels can be painted from
    /// either side and no gap is left), and neither does leaving a band `a` starts in.
    fn blocked(&self, a: (f32, f32), b: (f32, f32)) -> bool {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let d = (dx * dx + dy * dy).sqrt();
        let steps = (d / 0.7).ceil() as usize;
        // 0 = in open ground, 1 = inside a band entered from open ground, 2 = in the start's band.
        let mut state = if self.is_edge(a.0.round() as i64, a.1.round() as i64) { 2 } else { 0 };
        for k in 1..=steps {
            let t = k as f32 / steps as f32;
            let edge = self.is_edge((a.0 + dx * t).round() as i64, (a.1 + dy * t).round() as i64);
            match (state, edge) {
                (0, true) => state = 1,
                (1, false) => return true,
                (2, false) => state = 0,
                _ => {}
            }
        }
        false
    }
}

struct Canvas {
    w: usize,
    /// Premultiplied accumulation of every pass so far.
    px: Vec<[f32; 4]>,
}

impl Canvas {
    /// The canvas colour at a pixel, or `None` where nothing is painted yet.
    fn colour(&self, x: usize, y: usize) -> Option<[f32; 3]> {
        let p = self.px.get(y * self.w + x)?;
        (p[3] > 0.5).then(|| [p[0] / p[3], p[1] / p[3], p[2] / p[3]])
    }
}

/// Paints `src` (straight RGBA, `w × h`, alpha ignored: the picture is treated as opaque) in one
/// pass per brush size.
pub fn paint(src: &[[f32; 4]], w: usize, h: usize, params: &Params) -> Result<Vec<Pass>, String> {
    let n = w.checked_mul(h).ok_or("the picture is too large")?;
    if w == 0 || h == 0 {
        return Err("the picture has no area".into());
    }
    if n > MAX_PIXELS {
        return Err(format!("the picture is too large for painting ({n} pixels; the limit is {MAX_PIXELS})"));
    }
    if src.len() != n {
        return Err("the pixel buffer does not match the picture size".into());
    }
    let p = params.sanitized();
    let mut rng = Rng::new(p.seed);
    let mut canvas = Canvas { w, px: vec![[0.0; 4]; n] };
    let mut passes = Vec::with_capacity(p.radii.len());
    // Per-stroke coverage, kept so a stroke never doubles its own opacity; reset after each stroke.
    let mut cover = vec![0.0f32; n];
    // Bristle value (0..1) of the stamp that set each covered pixel; only used with texture on.
    let mut shade = vec![0.5f32; n];
    let mut touched: Vec<usize> = Vec::new();
    let channels: [Vec<f32>; 3] = std::array::from_fn(|c| src.iter().map(|q| if q[c].is_finite() { q[c].clamp(0.0, 1.0) } else { 0.0 }).collect());
    let edges = Edges::build(&channels, w, h, p.clip);

    for &radius in &p.radii {
        let sigma = (p.blur * radius).min(40.0);
        let blurred: Vec<Vec<f32>> = channels.iter().map(|c| gaussian_blur(c, w, h, sigma)).collect();
        let refc = |x: usize, y: usize| -> [f32; 3] {
            let i = y * w + x;
            [blurred[0].get(i).copied().unwrap_or(0.0), blurred[1].get(i).copied().unwrap_or(0.0), blurred[2].get(i).copied().unwrap_or(0.0)]
        };
        let lum: Vec<f32> = (0..n).map(|i| 0.3 * blurred[0][i] + 0.59 * blurred[1][i] + 0.11 * blurred[2][i]).collect();
        let grad = |x: usize, y: usize| -> (f32, f32) {
            let at = |xx: usize, yy: usize| lum.get(yy * w + xx).copied().unwrap_or(0.0);
            let (x0, x1, y0, y1) = (x.saturating_sub(1), (x + 1).min(w - 1), y.saturating_sub(1), (y + 1).min(h - 1));
            (at(x1, y) - at(x0, y), at(x, y1) - at(x, y0))
        };
        // Where the canvas still differs from the (blurred) picture; unpainted counts as maximal.
        let err = |x: usize, y: usize, canvas: &Canvas| -> f32 { canvas.colour(x, y).map_or(1.0, |c| dist(c, refc(x, y))) };

        let cell = ((p.grid * radius).round() as usize).clamp(2, 4096);
        let mut seeds: Vec<(usize, usize)> = Vec::new();
        for cy in (0..h).step_by(cell) {
            for cx in (0..w).step_by(cell) {
                let (x1, y1) = ((cx + cell).min(w), (cy + cell).min(h));
                let (mid_x, mid_y) = ((cx + x1) as f32 * 0.5, (cy + y1) as f32 * 0.5);
                let (mut sum, mut best, mut at) = (0.0f32, f32::NEG_INFINITY, (cx, cy));
                for y in cy..y1 {
                    for x in cx..x1 {
                        let e = err(x, y, &canvas);
                        sum += e;
                        // Ties go to the pixel nearest the cell's centre, so a stroke on bare
                        // canvas covers the whole cell.
                        let off = ((x as f32 - mid_x).powi(2) + (y as f32 - mid_y).powi(2)) / (cell * cell) as f32;
                        let score = e - 1e-3 * off;
                        if score > best {
                            best = score;
                            at = (x, y);
                        }
                    }
                }
                if sum / ((x1 - cx) * (y1 - cy)) as f32 > p.threshold {
                    seeds.push(at);
                }
            }
        }
        for i in (1..seeds.len()).rev() {
            seeds.swap(i, rng.below(i + 1));
        }

        let mut layer = vec![[0.0f32; 4]; n];
        let step = (0.5 * radius).max(1.0);
        for &(sx, sy) in &seeds {
            // With clipping on, the colour leans toward the unblurred picture at the seed, so a
            // stroke seeded next to an edge keeps its own side's colour, not the blur's grey.
            let soft = refc(sx, sy);
            let si = sy * w + sx;
            let base: [f32; 3] = std::array::from_fn(|c| {
                let sharp = channels[c].get(si).copied().unwrap_or(soft[c]);
                soft[c] + (sharp - soft[c]) * p.clip
            });
            let shift = (rng.unit() - 0.5) * p.jitter * 0.2;
            let colour: [f32; 3] = std::array::from_fn(|c| (base[c] + shift + (rng.unit() - 0.5) * p.jitter * 0.06).clamp(0.0, 1.0));
            // Follow the contour: perpendicular to the gradient, smoothed by the curvature filter.
            let heading = rng.unit() * std::f32::consts::TAU;
            // Texture draws from the generator only when on, so smooth strokes stay as they were.
            let phase = if p.bristles > 0.0 { (rng.next() >> 32) as u32 } else { 0 };
            let per_width = (radius * 0.8).clamp(3.0, 14.0);
            let mut pts = vec![(sx as f32, sy as f32)];
            let (mut x, mut y, mut last) = (sx as f32, sy as f32, (0.0f32, 0.0f32));
            for i in 1..=p.max_steps {
                let (ix, iy) = (x.round(), y.round());
                if ix < 0.0 || iy < 0.0 || ix >= w as f32 || iy >= h as f32 {
                    break;
                }
                let (ux, uy) = (ix as usize, iy as usize);
                if i > p.min_steps
                    && let Some(cc) = canvas.colour(ux, uy)
                {
                    let r = refc(ux, uy);
                    if dist(r, cc) < dist(r, colour) {
                        break;
                    }
                }
                let (gx, gy) = grad(ux, uy);
                // On flat colour there is no contour to follow: keep going the way the stroke was
                // heading (or a random way to start), so flat areas get brushed, not dotted.
                let mut d = if gx * gx + gy * gy < 1e-10 {
                    if last == (0.0, 0.0) { (heading.cos(), heading.sin()) } else { last }
                } else {
                    (-gy, gx)
                };
                if last.0 * d.0 + last.1 * d.1 < 0.0 {
                    d = (-d.0, -d.1);
                }
                let f = p.straightness;
                d = (f * last.0 + (1.0 - f) * d.0, f * last.1 + (1.0 - f) * d.1);
                let len = (d.0 * d.0 + d.1 * d.1).sqrt();
                if len < 1e-9 {
                    break;
                }
                d = (d.0 / len, d.1 / len);
                let (nx, ny) = (x + step * d.0, y + step * d.1);
                if let Some(e) = &edges {
                    // A stroke ends where it would run into an edge (one that starts on an edge
                    // band may follow it).
                    let (ox, oy) = (x.round() as i64, y.round() as i64);
                    if (e.is_edge(nx.round() as i64, ny.round() as i64) && !e.is_edge(ox, oy)) || e.blocked((x, y), (nx, ny)) {
                        break;
                    }
                }
                x = nx;
                y = ny;
                last = d;
                pts.push((x, y));
            }
            // Stamp the stroke: soft discs along the path, max-combined, then composited once.
            let reach = radius.ceil() as i64 + 1;
            for (k, &(px, py)) in pts.iter().enumerate() {
                // Direction of travel here, for the bristles' lateral axis.
                let (nx, ny) = match (pts.get(k + 1), k.checked_sub(1).and_then(|j| pts.get(j))) {
                    (Some(q), _) | (None, Some(q)) => {
                        let (ux, uy) = if k + 1 < pts.len() { (q.0 - px, q.1 - py) } else { (px - q.0, py - q.1) };
                        let l = (ux * ux + uy * uy).sqrt();
                        if l > 1e-6 { (-uy / l, ux / l) } else { (-heading.sin(), heading.cos()) }
                    }
                    (None, None) => (-heading.sin(), heading.cos()),
                };
                let (cx, cy) = (px.round() as i64, py.round() as i64);
                let clipper = edges.as_ref().filter(|e| e.any_near(cx - reach, cy - reach, cx + reach, cy + reach));
                for yy in (cy - reach).max(0)..=(cy + reach).min(h as i64 - 1) {
                    for xx in (cx - reach).max(0)..=(cx + reach).min(w as i64 - 1) {
                        let d = ((xx as f32 - px).powi(2) + (yy as f32 - py).powi(2)).sqrt();
                        let m = (radius + 0.5 - d).clamp(0.0, 1.0);
                        if m > 0.0 && !clipper.is_some_and(|e| e.blocked((px, py), (xx as f32, yy as f32))) {
                            let i = yy as usize * w + xx as usize;
                            let (m, b) = if p.bristles > 0.0 {
                                let lateral = (((xx as f32 - px) * nx + (yy as f32 - py) * ny) / radius).clamp(-1.0, 1.0);
                                let b = bristle(phase, (lateral * 0.5 + 0.5) * per_width);
                                (m * (1.0 - 0.5 * p.bristles * (1.0 - b)), b)
                            } else {
                                (m, 0.5)
                            };
                            if let Some(c) = cover.get_mut(i) {
                                if *c == 0.0 {
                                    touched.push(i);
                                }
                                if m > *c {
                                    *c = m;
                                    if let Some(sh) = shade.get_mut(i) {
                                        *sh = b;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for &i in &touched {
                let a = p.opacity * cover[i];
                cover[i] = 0.0;
                // Each bristle also nudges lightness, up to ±6% at full depth.
                let tint = (shade[i] - 0.5) * 0.12 * p.bristles;
                shade[i] = 0.5;
                for buf_px in [&mut layer[i], &mut canvas.px[i]] {
                    for c in 0..3 {
                        buf_px[c] = (colour[c] + tint).clamp(0.0, 1.0) * a + buf_px[c] * (1.0 - a);
                    }
                    buf_px[3] = a + buf_px[3] * (1.0 - a);
                }
            }
            touched.clear();
        }
        let px: Vec<[f32; 4]> = layer
            .iter()
            .map(|q| if q[3] > 1e-6 { [(q[0] / q[3]).clamp(0.0, 1.0), (q[1] / q[3]).clamp(0.0, 1.0), (q[2] / q[3]).clamp(0.0, 1.0), q[3].clamp(0.0, 1.0)] } else { [0.0; 4] })
            .collect();
        passes.push(Pass { radius, strokes: seeds.len(), px });
    }
    Ok(passes)
}

/// The passes stacked over each other (what the layers add up to), as opaque-or-not straight RGBA.
pub fn composite(passes: &[Pass], n: usize) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0f32; 4]; n];
    for pass in passes {
        for (o, q) in out.iter_mut().zip(&pass.px) {
            let a = q[3];
            let oa = o[3];
            let na = a + oa * (1.0 - a);
            if na > 0.0 {
                for c in 0..3 {
                    o[c] = (q[c] * a + o[c] * oa * (1.0 - a)) / na;
                }
            }
            o[3] = na;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| [(i % w) as f32 / w as f32, (i / w) as f32 / h as f32, 0.4, 1.0]).collect()
    }

    fn mean_err(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
        a.iter().zip(b).map(|(p, q)| dist([p[0], p[1], p[2]], [q[0], q[1], q[2]])).sum::<f32>() / a.len() as f32
    }

    fn small() -> Params {
        Params { radii: vec![8.0, 3.0], ..Params::default() }
    }

    #[test]
    fn painting_approximates_the_picture_and_covers_every_pixel() {
        let (w, h) = (64, 48);
        let src = gradient(w, h);
        let passes = paint(&src, w, h, &small()).unwrap();
        assert_eq!(passes.len(), 2);
        assert!(passes[0].radius > passes[1].radius, "largest first");
        let out = composite(&passes, w * h);
        assert!(out.iter().all(|q| q[3] > 0.99), "the first pass leaves no bare canvas");
        assert!(mean_err(&out, &src) < 0.08, "close to the picture: {}", mean_err(&out, &src));
    }

    #[test]
    fn a_flat_picture_needs_only_the_first_pass() {
        let (w, h) = (48, 48);
        let src = vec![[0.8, 0.3, 0.2, 1.0]; w * h];
        let passes = paint(&src, w, h, &small()).unwrap();
        assert!(passes[0].strokes > 0);
        assert_eq!(passes[1].strokes, 0, "nothing left to correct");
    }

    #[test]
    fn smaller_brushes_improve_the_match_where_there_is_detail() {
        let (w, h) = (64, 64);
        let src: Vec<[f32; 4]> = (0..w * h).map(|i| if ((i % w) / 4 + (i / w) / 4) % 2 == 0 { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }).collect();
        let passes = paint(&src, w, h, &Params { radii: vec![10.0, 2.0], ..Params::default() }).unwrap();
        let coarse = mean_err(&composite(&passes[..1], w * h), &src);
        let fine = mean_err(&composite(&passes, w * h), &src);
        assert!(fine < coarse, "{fine} < {coarse}");
    }

    #[test]
    fn the_same_seed_repeats_and_another_seed_differs() {
        let (w, h) = (40, 40);
        let src = gradient(w, h);
        let a = paint(&src, w, h, &small()).unwrap();
        let b = paint(&src, w, h, &small()).unwrap();
        assert_eq!(a, b);
        let c = paint(&src, w, h, &Params { seed: 99, ..small() }).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn opacity_and_straightness_change_the_result_without_breaking_it() {
        let (w, h) = (40, 40);
        let src = gradient(w, h);
        for p in [Params { opacity: 0.3, ..small() }, Params { straightness: 1.0, ..small() }, Params { straightness: 0.0, jitter: 1.0, ..small() }] {
            let out = paint(&src, w, h, &p).unwrap();
            assert!(out.iter().all(|pass| pass.px.iter().all(|q| q.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))));
        }
    }

    #[test]
    fn every_preset_scales_with_the_picture_and_paints() {
        for name in PRESETS {
            let (small, big) = (preset(name, 100).unwrap(), preset(name, 4000).unwrap());
            assert!(big.radii[0] > small.radii[0], "{name}");
            let src = gradient(32, 32);
            assert!(paint(&src, 32, 32, &small).is_ok(), "{name}");
        }
        assert!(preset("nope", 100).is_none());
    }

    #[test]
    fn bad_input_is_an_error_and_hostile_params_are_clamped() {
        assert!(paint(&[], 0, 0, &small()).is_err());
        assert!(paint(&[[0.0; 4]; 3], 2, 2, &small()).is_err(), "length mismatch");
        assert!(paint(&[], usize::MAX, 2, &small()).is_err());
        let hostile = Params {
            radii: vec![f32::NAN, f32::INFINITY, -4.0, 1e9, 0.0, 3.0, 4.0, 5.0, 6.0, 7.0],
            grid: f32::NAN,
            threshold: -1.0,
            straightness: 9.0,
            blur: f32::INFINITY,
            opacity: 0.0,
            min_steps: 0,
            max_steps: u32::MAX,
            jitter: f32::NAN,
            bristles: f32::NAN,
            clip: f32::NAN,
            seed: 0,
        };
        let s = hostile.sanitized();
        assert!(s.radii.len() <= MAX_PASSES && s.radii.windows(2).all(|w| w[0] > w[1]));
        assert!((1..=MAX_STROKE_STEPS).contains(&s.min_steps) && s.max_steps <= MAX_STROKE_STEPS);
        let src = gradient(20, 20);
        let out = paint(&src, 20, 20, &hostile).unwrap();
        assert!(!out.is_empty());
        let nan_src = vec![[f32::NAN, f32::INFINITY, -3.0, 1.0]; 16];
        assert!(paint(&nan_src, 4, 4, &small()).is_ok());
    }

    /// Left half black, right half white, an edge at x = 32.
    fn two_tone(w: usize, h: usize) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| if i % w < w / 2 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }).collect()
    }

    /// The darkest painted pixel on the white side and the lightest on the black side, a few
    /// pixels clear of the edge.
    fn leak(passes: &[Pass], w: usize, h: usize) -> (f32, f32) {
        let (mut white_min, mut black_max) = (1.0f32, 0.0f32);
        for pass in passes {
            for y in 0..h {
                for x in 0..w {
                    let q = pass.px[y * w + x];
                    if q[3] < 0.5 {
                        continue;
                    }
                    let l = (q[0] + q[1] + q[2]) / 3.0;
                    if x >= w / 2 + 3 {
                        white_min = white_min.min(l);
                    } else if x + 3 < w / 2 {
                        black_max = black_max.max(l);
                    }
                }
            }
        }
        (white_min, black_max)
    }

    #[test]
    fn clipping_keeps_paint_inside_shapes() {
        let (w, h) = (64, 48);
        let src = two_tone(w, h);
        let base = Params { radii: vec![10.0], jitter: 0.0, opacity: 1.0, straightness: 1.0, max_steps: 12, ..Params::default() };
        let off = paint(&src, w, h, &base).unwrap();
        let on = paint(&src, w, h, &Params { clip: 1.0, ..base.clone() }).unwrap();
        let (wo, bo) = leak(&off, w, h);
        let (wc, bc) = leak(&on, w, h);
        assert!(wo < 0.9 || bo > 0.1, "without clipping paint crosses the edge: {wo} {bo}");
        assert!(wc > 0.9 && bc < 0.1, "with clipping it doesn't: white side {wc}, black side {bc}");
        let out = composite(&on, w * h);
        let bare: Vec<(usize, usize)> = out.iter().enumerate().filter(|(_, q)| q[3] < 0.99).map(|(i, _)| (i % w, i / w)).collect();
        assert!(bare.is_empty(), "clipping leaves no bare canvas: {} bare, e.g. {:?}", bare.len(), &bare[..bare.len().min(12)]);
    }

    #[test]
    fn clipping_does_not_touch_a_picture_without_edges() {
        let (w, h) = (40, 40);
        let src = vec![[0.5, 0.4, 0.3, 1.0]; w * h];
        let a = paint(&src, w, h, &small()).unwrap();
        let b = paint(&src, w, h, &Params { clip: 1.0, ..small() }).unwrap();
        for (pa, pb) in a.iter().zip(&b) {
            assert_eq!(pa.strokes, pb.strokes);
            assert!(mean_err(&pa.px, &pb.px) < 1e-3);
        }
    }

    #[test]
    fn clipping_is_deterministic_and_safe_on_hostile_pictures() {
        let (w, h) = (33, 17);
        let src: Vec<[f32; 4]> = (0..w * h).map(|i| if i % 7 == 0 { [f32::NAN, 9.0, -1.0, 1.0] } else { [(i % 5) as f32 / 4.0; 4] }).collect();
        let p = Params { clip: 1.0, ..small() };
        assert_eq!(paint(&src, w, h, &p).unwrap(), paint(&src, w, h, &p).unwrap());
        // One-pixel-wide pictures have no room for a gradient but must not panic.
        assert!(paint(&[[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]], 1, 2, &p).is_ok());
        assert!(paint(&[[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]], 2, 1, &p).is_ok());
    }

    #[test]
    fn bristles_streak_the_paint_and_zero_depth_changes_nothing() {
        // One cell, so a single stroke: overlapping strokes would saturate the layer's opacity.
        let (w, h) = (40, 40);
        let src = vec![[0.6, 0.4, 0.3, 1.0]; w * h];
        let base = Params { radii: vec![10.0], jitter: 0.0, grid: 4.0, straightness: 1.0, min_steps: 6, max_steps: 6, ..Params::default() };
        let smooth = paint(&src, w, h, &base).unwrap();
        assert_eq!(smooth, paint(&src, w, h, &Params { bristles: 0.0, ..base.clone() }).unwrap());
        let rough = paint(&src, w, h, &Params { bristles: 1.0, ..base.clone() }).unwrap();
        // Inside the smooth stroke every pixel is fully painted; texture must thin some of them.
        let inside: Vec<usize> = (0..w * h).filter(|&i| smooth[0].px[i][3] > 0.99).collect();
        assert!(inside.len() > 100);
        let alphas: Vec<f32> = inside.iter().map(|&i| rough[0].px[i][3]).collect();
        let mean = alphas.iter().sum::<f32>() / alphas.len() as f32;
        let sd = (alphas.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / alphas.len() as f32).sqrt();
        assert!(sd > 0.04, "streaks vary the opacity: sd {sd}");
        assert!(alphas.iter().all(|a| *a >= 0.45), "bristles never thin the paint past half: {:?}", alphas.iter().cloned().fold(1.0, f32::min));
        assert!(rough[0].px.iter().all(|q| q.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))));
        assert!(rough[0].px.iter().filter(|q| q[3] > 0.45).count() > 100, "texture still lays paint");
        assert_eq!(rough, paint(&src, w, h, &Params { bristles: 1.0, ..base }).unwrap(), "deterministic");
    }
}
