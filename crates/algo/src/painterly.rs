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
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params { radii: vec![16.0, 8.0, 4.0], grid: 1.0, threshold: 0.08, straightness: 0.5, blur: 0.5, opacity: 1.0, min_steps: 4, max_steps: 16, jitter: 0.2, seed: 1 }
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
        "impressionist" => Params { radii: vec![px(1.6), px(0.8), px(0.4)], threshold: 0.08, straightness: 0.4, min_steps: 4, max_steps: 14, jitter: 0.3, ..Params::default() },
        "oil" => Params { radii: vec![px(1.2), px(0.6), px(0.3)], threshold: 0.06, straightness: 0.3, blur: 0.4, opacity: 0.9, min_steps: 6, max_steps: 24, jitter: 0.15, ..Params::default() },
        "pointillist" => Params { radii: vec![px(0.5), px(0.3)], threshold: 0.04, straightness: 1.0, min_steps: 1, max_steps: 1, jitter: 0.5, ..Params::default() },
        "wash" => Params { radii: vec![px(2.4), px(1.2)], threshold: 0.12, straightness: 0.5, blur: 0.8, opacity: 0.35, min_steps: 6, max_steps: 20, jitter: 0.1, ..Params::default() },
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
    let mut touched: Vec<usize> = Vec::new();
    let channels: [Vec<f32>; 3] = std::array::from_fn(|c| src.iter().map(|q| if q[c].is_finite() { q[c].clamp(0.0, 1.0) } else { 0.0 }).collect());

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
            let base = refc(sx, sy);
            let shift = (rng.unit() - 0.5) * p.jitter * 0.2;
            let colour: [f32; 3] = std::array::from_fn(|c| (base[c] + shift + (rng.unit() - 0.5) * p.jitter * 0.06).clamp(0.0, 1.0));
            // Follow the contour: perpendicular to the gradient, smoothed by the curvature filter.
            let heading = rng.unit() * std::f32::consts::TAU;
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
                x += step * d.0;
                y += step * d.1;
                last = d;
                pts.push((x, y));
            }
            // Stamp the stroke: soft discs along the path, max-combined, then composited once.
            let reach = radius.ceil() as i64 + 1;
            for &(px, py) in &pts {
                let (cx, cy) = (px.round() as i64, py.round() as i64);
                for yy in (cy - reach).max(0)..=(cy + reach).min(h as i64 - 1) {
                    for xx in (cx - reach).max(0)..=(cx + reach).min(w as i64 - 1) {
                        let d = ((xx as f32 - px).powi(2) + (yy as f32 - py).powi(2)).sqrt();
                        let m = (radius + 0.5 - d).clamp(0.0, 1.0);
                        if m > 0.0 {
                            let i = yy as usize * w + xx as usize;
                            if let Some(c) = cover.get_mut(i) {
                                if *c == 0.0 {
                                    touched.push(i);
                                }
                                *c = c.max(m);
                            }
                        }
                    }
                }
            }
            for &i in &touched {
                let a = p.opacity * cover[i];
                cover[i] = 0.0;
                for buf_px in [&mut layer[i], &mut canvas.px[i]] {
                    for c in 0..3 {
                        buf_px[c] = colour[c] * a + buf_px[c] * (1.0 - a);
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
}
