//! Colour matching as a lookup table: move one image's colours toward another's while keeping
//! the picture's own lighting, then bake the result into a 3D LUT.
//!
//! Clean-room, from the published literature: Reinhard et al., "Color Transfer between Images"
//! (2001; per-channel mean and spread) and the Monge–Kantorovich linear transform for Gaussians
//! (Pitié & Kokaram, 2007; matches the full chroma covariance, so hue relationships carry over,
//! not just the average cast). Everything runs in Oklab (Ottosson, 2020), so lightness and
//! chroma are separate axes:
//!
//! * **Lightness** is kept as it is, or, with `match_contrast`, matched to the source's mean and
//!   spread (the source's tonal range).
//! * **Chroma** (`a`, `b`) is moved by a 2×2 linear map plus a shift: either a per-axis scale
//!   (`Method::Mean`) or the covariance-matching map (`Method::Covariance`).
//!
//! The fitted map is global and affine, so it bakes exactly into a LUT; `strength` blends the LUT
//! back toward identity.

use photocraft_color::wheel::{oklab_to_srgb, srgb_to_oklab};

/// Pixels sampled per image (a stride over the surface): plenty for stable statistics.
pub const MAX_SAMPLES: usize = 262_144;
/// Smallest and largest LUT edge accepted.
pub const MIN_LUT: usize = 9;
pub const MAX_LUT: usize = 65;
/// Variance added to the chroma covariance so grey or flat images stay well-conditioned.
const RIDGE: f64 = 1e-4;

/// Oklab statistics of an image's opaque pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub mean: [f64; 3],
    /// Lightness standard deviation.
    pub l_std: f64,
    /// Chroma covariance `[[aa, ab], [ab, bb]]`.
    pub cov: [[f64; 2]; 2],
    pub count: usize,
}

/// Statistics of `px` (straight RGBA), weighting each pixel by `alpha × mask`; `None` when
/// nothing is opaque. Samples with a stride so huge images stay cheap.
pub fn stats(px: &[[f32; 4]], mask: Option<&[f32]>) -> Option<Stats> {
    let stride = px.len().div_ceil(MAX_SAMPLES).max(1);
    let (mut w_sum, mut m, mut mm) = (0.0f64, [0.0f64; 3], [[0.0f64; 3]; 3]);
    for (i, q) in px.iter().enumerate().step_by(stride) {
        let w = f64::from(q[3] * mask.map_or(1.0, |k| k.get(i).copied().unwrap_or(0.0)));
        if w.is_nan() || w <= 0.0 {
            continue;
        }
        let lab = srgb_to_oklab([q[0], q[1], q[2]]).map(f64::from);
        w_sum += w;
        for r in 0..3 {
            m[r] += w * lab[r];
            for c in 0..3 {
                mm[r][c] += w * lab[r] * lab[c];
            }
        }
    }
    if w_sum <= 0.0 {
        return None;
    }
    let mean = m.map(|v| v / w_sum);
    let cov_of = |r: usize, c: usize| (mm[r][c] / w_sum - mean[r] * mean[c]).max(if r == c { 0.0 } else { f64::MIN });
    Some(Stats {
        mean,
        l_std: cov_of(0, 0).sqrt(),
        cov: [[cov_of(1, 1), cov_of(1, 2)], [cov_of(2, 1), cov_of(2, 2)]],
        count: px.len().div_ceil(stride),
    })
}

/// How chroma is matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Per-axis mean and spread (Reinhard).
    Mean,
    /// Full chroma covariance (Monge–Kantorovich).
    Covariance,
}

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "mean" => Some(Method::Mean),
            "covariance" => Some(Method::Covariance),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub method: Method,
    /// Match the source's lightness mean and spread instead of keeping the picture's own.
    pub match_contrast: bool,
    /// 0..=1: how far toward the matched colours the table goes.
    pub strength: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options { method: Method::Covariance, match_contrast: false, strength: 1.0 }
    }
}

/// The fitted map: `lab' = lightness(lab.L)`, `(a', b') = A·(a - t.a, b - t.b) + (s.a, s.b)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    t_mean: [f64; 3],
    s_mean: [f64; 3],
    l_scale: f64,
    use_contrast: bool,
    a: [[f64; 2]; 2],
}

type M2 = [[f64; 2]; 2];

fn det(m: &M2) -> f64 {
    m[0][0] * m[1][1] - m[0][1] * m[1][0]
}

fn mul(a: &M2, b: &M2) -> M2 {
    std::array::from_fn(|r| std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c]))
}

fn inv(m: &M2) -> M2 {
    let d = det(m);
    let d = if d.abs() < 1e-18 { 1e-18 } else { d };
    [[m[1][1] / d, -m[0][1] / d], [-m[1][0] / d, m[0][0] / d]]
}

/// Square root of a symmetric positive semi-definite 2×2: `(M + √det·I) / √(tr + 2√det)`.
fn sqrt_psd(m: &M2) -> M2 {
    let s = det(m).max(0.0).sqrt();
    let t = (m[0][0] + m[1][1] + 2.0 * s).max(1e-18).sqrt();
    [[(m[0][0] + s) / t, m[0][1] / t], [m[1][0] / t, (m[1][1] + s) / t]]
}

fn ridge(c: &M2) -> M2 {
    [[c[0][0] + RIDGE, c[0][1]], [c[1][0], c[1][1] + RIDGE]]
}

/// Fit the map that takes `target` (the image being corrected) toward `source` (the reference).
pub fn fit(target: &Stats, source: &Stats, o: &Options) -> Fit {
    let (ct, cs) = (ridge(&target.cov), ridge(&source.cov));
    let a = match o.method {
        Method::Mean => [[(cs[0][0] / ct[0][0]).sqrt(), 0.0], [0.0, (cs[1][1] / ct[1][1]).sqrt()]],
        Method::Covariance => {
            // A = Ct^-1/2 · (Ct^1/2 · Cs · Ct^1/2)^1/2 · Ct^-1/2
            let rt = sqrt_psd(&ct);
            let rt_inv = inv(&rt);
            mul(&mul(&rt_inv, &sqrt_psd(&mul(&mul(&rt, &cs), &rt))), &rt_inv)
        }
    };
    let l_scale = if target.l_std > 1e-4 { (source.l_std / target.l_std).clamp(0.25, 4.0) } else { 1.0 };
    Fit { t_mean: target.mean, s_mean: source.mean, l_scale, use_contrast: o.match_contrast, a }
}

impl Fit {
    /// The matched colour for one sRGB pixel (before strength), in gamut.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [l, a, b] = srgb_to_oklab(rgb).map(f64::from);
        let l2 = if self.use_contrast { (l - self.t_mean[0]) * self.l_scale + self.s_mean[0] } else { l };
        let (da, db) = (a - self.t_mean[1], b - self.t_mean[2]);
        let a2 = self.a[0][0] * da + self.a[0][1] * db + self.s_mean[1];
        let b2 = self.a[1][0] * da + self.a[1][1] * db + self.s_mean[2];
        // Out of gamut: oklab_to_srgb clips channels; pull chroma in first so hues don't skew.
        let lab = fit_in_gamut([l2.clamp(0.0, 1.0) as f32, a2 as f32, b2 as f32]);
        oklab_to_srgb(lab)
    }
}

/// Shrinks chroma (keeping hue and lightness) until the colour fits sRGB.
fn fit_in_gamut(lab: [f32; 3]) -> [f32; 3] {
    let inside = |k: f32| {
        let c = photocraft_color::wheel::oklab_to_linear_srgb([lab[0], lab[1] * k, lab[2] * k]);
        c.iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
    };
    if inside(1.0) {
        return lab;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if inside(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    [lab[0], lab[1] * lo, lab[2] * lo]
}

/// Bakes `fit` into a `size`³ table (red fastest, values 0..=1), blended toward identity by
/// `strength`. `size` is clamped to `MIN_LUT..=MAX_LUT`.
pub fn build_lut(fit: &Fit, size: usize, strength: f32) -> Vec<f32> {
    let n = size.clamp(MIN_LUT, MAX_LUT);
    let k = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 0.0 };
    let step = 1.0 / (n - 1) as f32;
    let mut out = Vec::with_capacity(n * n * n * 3);
    for bi in 0..n {
        for gi in 0..n {
            for ri in 0..n {
                let c = [ri as f32 * step, gi as f32 * step, bi as f32 * step];
                let m = fit.apply(c);
                out.extend((0..3).map(|i| (c[i] + (m[i] - c[i]) * k).clamp(0.0, 1.0)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small image whose colours drift around `centre` in chroma.
    fn image(centre: [f32; 3], spread: f32) -> Vec<[f32; 4]> {
        (0..400)
            .map(|i| {
                let (x, y) = ((i % 20) as f32 / 19.0 - 0.5, (i / 20) as f32 / 19.0 - 0.5);
                let l = 0.3 + 0.4 * (i as f32 / 399.0);
                let rgb = oklab_to_srgb([l, centre[1] + x * spread, centre[2] + y * spread]);
                [rgb[0], rgb[1], rgb[2], 1.0]
            })
            .collect()
    }

    fn ok(px: &[[f32; 4]]) -> Stats {
        stats(px, None).unwrap()
    }

    #[test]
    fn stats_of_transparent_pixels_is_none() {
        assert!(stats(&[[0.5, 0.5, 0.5, 0.0]], None).is_none());
        assert!(stats(&[], None).is_none());
        assert!(stats(&[[0.5; 4]], Some(&[0.0])).is_none());
    }

    #[test]
    fn matching_an_image_to_itself_is_the_identity() {
        let img = image([0.0, 0.03, -0.02], 0.1);
        let s = ok(&img);
        for method in [Method::Mean, Method::Covariance] {
            let f = fit(&s, &s, &Options { method, match_contrast: true, strength: 1.0 });
            for c in [[0.2, 0.4, 0.6], [0.9, 0.1, 0.3], [0.5, 0.5, 0.5]] {
                let o = f.apply(c);
                for i in 0..3 {
                    assert!((o[i] - c[i]).abs() < 0.01, "{method:?} {c:?} -> {o:?}");
                }
            }
        }
    }

    #[test]
    fn the_cast_moves_to_the_source_and_lightness_is_kept() {
        let target = image([0.0, 0.0, 0.0], 0.1);
        let source = image([0.0, 0.08, 0.05], 0.1);
        let (st, ss) = (ok(&target), ok(&source));
        let f = fit(&st, &ss, &Options::default());
        let matched: Vec<[f32; 4]> = target.iter().map(|q| {
            let o = f.apply([q[0], q[1], q[2]]);
            [o[0], o[1], o[2], 1.0]
        }).collect();
        let sm = ok(&matched);
        assert!((sm.mean[1] - ss.mean[1]).abs() < 0.01, "a mean {} vs {}", sm.mean[1], ss.mean[1]);
        assert!((sm.mean[2] - ss.mean[2]).abs() < 0.01, "b mean {} vs {}", sm.mean[2], ss.mean[2]);
        assert!((sm.mean[0] - st.mean[0]).abs() < 0.01, "lightness kept");
        assert!((sm.l_std - st.l_std).abs() < 0.01);
    }

    #[test]
    fn match_contrast_takes_the_sources_tonal_spread() {
        let target = image([0.0; 3], 0.05);
        let mut source = image([0.0; 3], 0.05);
        for q in &mut source {
            let lab = srgb_to_oklab([q[0], q[1], q[2]]);
            let o = oklab_to_srgb([0.5 + (lab[0] - 0.5) * 0.4, lab[1], lab[2]]);
            *q = [o[0], o[1], o[2], 1.0];
        }
        let (st, ss) = (ok(&target), ok(&source));
        let f = fit(&st, &ss, &Options { match_contrast: true, ..Options::default() });
        let matched: Vec<[f32; 4]> = target.iter().map(|q| {
            let o = f.apply([q[0], q[1], q[2]]);
            [o[0], o[1], o[2], 1.0]
        }).collect();
        assert!((ok(&matched).l_std - ss.l_std).abs() < 0.01);
        assert!(ss.l_std < st.l_std * 0.6, "fixture: the source is flatter");
    }

    #[test]
    fn covariance_matches_the_chroma_spread_that_mean_only_cannot() {
        // Source chroma is stretched along a diagonal; per-axis scaling can't reproduce that.
        let target = image([0.0; 3], 0.1);
        let mut source = image([0.0; 3], 0.1);
        for q in &mut source {
            let lab = srgb_to_oklab([q[0], q[1], q[2]]);
            let (a, b) = (lab[1], lab[2]);
            let o = oklab_to_srgb([lab[0], a + 0.8 * b, b + 0.8 * a]);
            *q = [o[0], o[1], o[2], 1.0];
        }
        let (st, ss) = (ok(&target), ok(&source));
        let off_diag = |m: Method| {
            let f = fit(&st, &ss, &Options { method: m, ..Options::default() });
            let matched: Vec<[f32; 4]> = target.iter().map(|q| {
                let o = f.apply([q[0], q[1], q[2]]);
                [o[0], o[1], o[2], 1.0]
            }).collect();
            ok(&matched).cov[0][1]
        };
        let want = ss.cov[0][1];
        assert!((off_diag(Method::Covariance) - want).abs() < (off_diag(Method::Mean) - want).abs());
    }

    #[test]
    fn grey_target_and_flat_source_stay_finite() {
        let grey: Vec<[f32; 4]> = (0..100).map(|i| [i as f32 / 99.0, i as f32 / 99.0, i as f32 / 99.0, 1.0]).collect();
        let flat = vec![[0.8, 0.4, 0.2, 1.0]; 100];
        let (sg, sf) = (ok(&grey), ok(&flat));
        for (t, s) in [(&sg, &sf), (&sf, &sg), (&sg, &sg)] {
            let lut = build_lut(&fit(t, s, &Options::default()), 9, 1.0);
            assert!(lut.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn lut_has_the_right_layout_and_strength_zero_is_identity() {
        let (t, s) = (ok(&image([0.0; 3], 0.1)), ok(&image([0.0, 0.1, 0.0], 0.1)));
        let f = fit(&t, &s, &Options::default());
        let n = 9;
        let id = build_lut(&f, n, 0.0);
        assert_eq!(id.len(), n * n * n * 3);
        // Red varies fastest: entry 1 is (1/8, 0, 0).
        assert!((id[3] - 0.125).abs() < 1e-6 && id[4] == 0.0 && id[5] == 0.0);
        let full = build_lut(&f, n, 1.0);
        let half = build_lut(&f, n, 0.5);
        let probe = 3 * (n * n * n / 2);
        for i in 0..3 {
            let mid = (id[probe + i] + full[probe + i]) / 2.0;
            assert!((half[probe + i] - mid).abs() < 1e-5);
        }
        assert_ne!(full, id);
    }

    #[test]
    fn hostile_options_never_panic() {
        let (t, s) = (ok(&image([0.0; 3], 0.1)), ok(&image([0.0; 3], 0.1)));
        let f = fit(&t, &s, &Options::default());
        assert_eq!(build_lut(&f, 0, f32::NAN).len(), MIN_LUT.pow(3) * 3);
        assert_eq!(build_lut(&f, usize::MAX, 9.0).len(), MAX_LUT.pow(3) * 3);
        let _ = f.apply([f32::NAN, f32::INFINITY, -5.0]);
    }
}
