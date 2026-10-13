//! Colour-wheel maths: Oklab/OKLCH, hue-angle harmonies, the traditional red-yellow-blue (RYB)
//! wheel and blackbody colour temperature.
//!
//! Clean-room: written from published definitions (Björn Ottosson's Oklab, 2020; Kim et al.'s
//! Planckian-locus spline) and the textbook harmony rules. All angles are degrees,
//! wrapped to `[0, 360)`; all RGB is sRGB-encoded `0..=1`. Nothing here panics on any input
//! (NaN and infinities collapse to a defined value).

use crate::convert::{linear_to_srgb, srgb_to_linear};

/// Wrap a hue angle into `[0, 360)`. Non-finite input gives `0`.
#[inline]
pub fn wrap_hue(h: f32) -> f32 {
    if !h.is_finite() {
        return 0.0;
    }
    let r = h.rem_euclid(360.0);
    if r >= 360.0 { 0.0 } else { r }
}

#[inline]
fn finite01(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Encoded sRGB → Oklab `[L, a, b]` (L in `0..=1` for in-gamut input).
pub fn srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|v| srgb_to_linear(finite01(v)));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab → *linear* sRGB, unclamped (out-of-gamut values fall outside `0..=1`).
pub fn oklab_to_linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    let lab = lab.map(|v| if v.is_finite() { v } else { 0.0 });
    let l = (lab[0] + 0.396_337_78 * lab[1] + 0.215_803_76 * lab[2]).powi(3);
    let m = (lab[0] - 0.105_561_346 * lab[1] - 0.063_854_17 * lab[2]).powi(3);
    let s = (lab[0] - 0.089_484_18 * lab[1] - 1.291_485_5 * lab[2]).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// Oklab → encoded sRGB, clamped per channel.
pub fn oklab_to_srgb(lab: [f32; 3]) -> [f32; 3] {
    oklab_to_linear_srgb(lab).map(|v| linear_to_srgb(finite01(v)))
}

/// Encoded sRGB → OKLCH `[L, C, h°]`.
pub fn srgb_to_oklch(rgb: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = srgb_to_oklab(rgb);
    let c = a.hypot(b);
    [l, c, if c < 1e-5 { 0.0 } else { wrap_hue(b.atan2(a).to_degrees()) }]
}

/// OKLCH → Oklab.
pub fn oklch_to_oklab(lch: [f32; 3]) -> [f32; 3] {
    let h = if lch[2].is_finite() { lch[2].to_radians() } else { 0.0 };
    let c = if lch[1].is_finite() { lch[1].max(0.0) } else { 0.0 };
    [lch[0], c * h.cos(), c * h.sin()]
}

/// OKLCH → encoded sRGB, clamped per channel.
pub fn oklch_to_srgb(lch: [f32; 3]) -> [f32; 3] {
    oklab_to_srgb(oklch_to_oklab(lch))
}

/// `true` when the OKLCH colour is inside the sRGB gamut.
pub fn oklch_in_gamut(lch: [f32; 3]) -> bool {
    oklab_to_linear_srgb(oklch_to_oklab(lch)).iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
}

/// Largest chroma ≤ `lch[1]` that stays inside sRGB at the given lightness and hue (bisection).
/// This is how a wheel keeps a hue ring drawable at any lightness without clipping hue shifts.
pub fn clamp_chroma_to_gamut(lch: [f32; 3]) -> [f32; 3] {
    let [l, c, h] = lch;
    let c = if c.is_finite() { c.max(0.0) } else { 0.0 };
    if oklch_in_gamut([l, c, h]) {
        return [l, c, h];
    }
    let (mut lo, mut hi) = (0.0f32, c);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if oklch_in_gamut([l, mid, h]) { lo = mid } else { hi = mid }
    }
    [l, lo, h]
}

/// Classic colour-harmony schemes, as hue offsets from the base hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harmony {
    Complementary,
    SplitComplementary,
    Analogous,
    Triad,
    Tetrad,
    Square,
    Monochromatic,
}

impl Harmony {
    pub const ALL: [Harmony; 7] =
        [Harmony::Complementary, Harmony::SplitComplementary, Harmony::Analogous, Harmony::Triad, Harmony::Tetrad, Harmony::Square, Harmony::Monochromatic];

    pub const fn label(self) -> &'static str {
        match self {
            Harmony::Complementary => "Complementary",
            Harmony::SplitComplementary => "Split complementary",
            Harmony::Analogous => "Analogous",
            Harmony::Triad => "Triad",
            Harmony::Tetrad => "Tetrad",
            Harmony::Square => "Square",
            Harmony::Monochromatic => "Monochromatic",
        }
    }

    /// Hue offsets in degrees; the first is always `0` (the base colour itself).
    pub const fn offsets(self) -> &'static [f32] {
        match self {
            Harmony::Complementary => &[0.0, 180.0],
            Harmony::SplitComplementary => &[0.0, 150.0, 210.0],
            Harmony::Analogous => &[0.0, 30.0, 330.0],
            Harmony::Triad => &[0.0, 120.0, 240.0],
            Harmony::Tetrad => &[0.0, 60.0, 180.0, 240.0],
            Harmony::Square => &[0.0, 90.0, 180.0, 270.0],
            Harmony::Monochromatic => &[0.0, 0.0, 0.0],
        }
    }
}

/// Hues (degrees) of a harmony around `base_hue`, base first.
pub fn harmony_hues(scheme: Harmony, base_hue: f32) -> Vec<f32> {
    scheme.offsets().iter().map(|o| wrap_hue(base_hue + o)).collect()
}

/// RYB-wheel anchors as `(ryb_angle, rgb_hue)`: red, orange, yellow, green, blue, violet. The RGB
/// hues are the usual HSV hues of those pigments; between anchors the mapping is linear. Original
/// table (primaries at 0°/120°/240° so a triad of the RYB wheel is red-yellow-blue).
const RYB_ANCHORS: [(f32, f32); 7] = [(0.0, 0.0), (60.0, 30.0), (120.0, 60.0), (180.0, 120.0), (240.0, 240.0), (300.0, 285.0), (360.0, 360.0)];

fn warp(h: f32, from: usize, to: usize) -> f32 {
    let h = wrap_hue(h);
    let pick = |i: usize, k: usize| if k == 0 { RYB_ANCHORS[i].0 } else { RYB_ANCHORS[i].1 };
    for i in 0..RYB_ANCHORS.len() - 1 {
        let (a0, a1) = (pick(i, from), pick(i + 1, from));
        if h >= a0 && h <= a1 {
            let t = if a1 > a0 { (h - a0) / (a1 - a0) } else { 0.0 };
            return wrap_hue(pick(i, to) + t * (pick(i + 1, to) - pick(i, to)));
        }
    }
    h
}

/// RYB-wheel angle → HSV hue (degrees).
pub fn ryb_to_rgb_hue(ryb: f32) -> f32 {
    warp(ryb, 0, 1)
}

/// HSV hue (degrees) → RYB-wheel angle.
pub fn rgb_to_ryb_hue(hue: f32) -> f32 {
    warp(hue, 1, 0)
}

/// Hues of a harmony computed on the traditional RYB wheel, returned as HSV hues. On this wheel
/// the complement of red is green and of blue is orange, as painters expect.
pub fn harmony_hues_ryb(scheme: Harmony, base_hue: f32) -> Vec<f32> {
    let base = rgb_to_ryb_hue(base_hue);
    scheme.offsets().iter().map(|o| ryb_to_rgb_hue(base + o)).collect()
}

/// Blackbody colour temperature (1000–40000 K) → encoded sRGB, normalised so the brightest
/// channel is 1. Uses Kim et al.'s published cubic spline for the Planckian locus chromaticity
/// (x, y) — coefficients below — then xyY→linear sRGB (D65).
pub fn kelvin_to_srgb(kelvin: f32) -> [f32; 3] {
    let t = if kelvin.is_finite() { kelvin.clamp(1000.0, 40000.0) } else { 6500.0 };
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 4000.0 {
        -0.266_123_9e9 / t3 - 0.234_358_9e6 / t2 + 0.877_695_6e3 / t + 0.179_91
    } else {
        -3.025_847e9 / t3 + 2.107_038e6 / t2 + 0.222_634_7e3 / t + 0.240_39
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_2 * x2 + 2.185_558_3 * x - 0.202_196_8
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_186 * x2 + 2.091_37 * x - 0.167_488_7
    } else {
        3.081_758 * x3 - 5.873_387 * x2 + 3.751_13 * x - 0.370_014_8
    };
    let y = y.max(1e-4);
    let (xx, zz) = (x / y, (1.0 - x - y) / y);
    let lin =
        [3.240_454_2 * xx - 1.537_138_5 - 0.498_531_4 * zz, -0.969_266 * xx + 1.876_010_8 + 0.041_556 * zz, 0.055_643_4 * xx - 0.204_025_9 + 1.057_225_2 * zz];
    let m = lin.iter().cloned().fold(1e-6f32, f32::max);
    lin.map(|v| linear_to_srgb((v / m).clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
    }

    #[test]
    fn oklab_round_trips_and_known_values() {
        for rgb in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.2, 0.5, 0.8], [0.9, 0.7, 0.1]] {
            assert!(close(oklab_to_srgb(srgb_to_oklab(rgb)), rgb, 2e-3), "{rgb:?}");
        }
        let white = srgb_to_oklab([1.0, 1.0, 1.0]);
        assert!((white[0] - 1.0).abs() < 1e-3 && white[1].abs() < 1e-3 && white[2].abs() < 1e-3);
        let red = srgb_to_oklch([1.0, 0.0, 0.0]);
        assert!((red[0] - 0.628).abs() < 2e-3 && (red[1] - 0.258).abs() < 2e-3 && (red[2] - 29.2).abs() < 0.5, "{red:?}");
    }

    #[test]
    fn grey_has_no_chroma_or_hue() {
        let g = srgb_to_oklch([0.5, 0.5, 0.5]);
        assert!(g[1] < 1e-3 && g[2] == 0.0);
    }

    #[test]
    fn chroma_clamp_lands_in_gamut() {
        for h in (0..360).step_by(15) {
            let c = clamp_chroma_to_gamut([0.7, 0.5, h as f32]);
            assert!(oklch_in_gamut(c) && c[1] <= 0.5);
        }
        assert!(!oklch_in_gamut([0.7, 0.5, 140.0]));
    }

    #[test]
    fn wrap_and_nonfinite_are_safe() {
        assert_eq!(wrap_hue(-30.0), 330.0);
        assert_eq!(wrap_hue(725.0), 5.0);
        assert_eq!(wrap_hue(f32::NAN), 0.0);
        assert_eq!(wrap_hue(f32::INFINITY), 0.0);
        let _ = oklch_to_srgb([f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
        let _ = srgb_to_oklab([f32::NAN, -5.0, 9.0]);
        let _ = kelvin_to_srgb(f32::NAN);
        let _ = ryb_to_rgb_hue(f32::NAN);
    }

    #[test]
    fn harmonies_have_expected_hues() {
        assert_eq!(harmony_hues(Harmony::Complementary, 30.0), vec![30.0, 210.0]);
        assert_eq!(harmony_hues(Harmony::Triad, 300.0), vec![300.0, 60.0, 180.0]);
        assert_eq!(harmony_hues(Harmony::SplitComplementary, 0.0), vec![0.0, 150.0, 210.0]);
        assert_eq!(harmony_hues(Harmony::Analogous, 10.0), vec![10.0, 40.0, 340.0]);
        for s in Harmony::ALL {
            assert_eq!(s.offsets()[0], 0.0);
        }
    }

    #[test]
    fn ryb_wheel_has_painters_complements() {
        // Red ↔ green, yellow ↔ violet, blue ↔ orange.
        let c = |h: f32| harmony_hues_ryb(Harmony::Complementary, h)[1];
        assert!((c(0.0) - 120.0).abs() < 1.0);
        assert!((c(60.0) - 285.0).abs() < 1.0);
        assert!((c(240.0) - 30.0).abs() < 1.0);
        // RYB primaries make the triad.
        let t = harmony_hues_ryb(Harmony::Triad, 0.0);
        assert!((t[1] - 60.0).abs() < 1.0 && (t[2] - 240.0).abs() < 1.0);
    }

    #[test]
    fn ryb_mapping_round_trips_and_is_monotonic() {
        let mut prev = -1.0;
        for a in 0..360 {
            let h = ryb_to_rgb_hue(a as f32);
            assert!(h >= prev - 1e-3 || a == 0, "not monotonic at {a}");
            prev = h;
            let back = rgb_to_ryb_hue(h);
            let d = (back - a as f32).abs();
            assert!(d < 0.01 || (360.0 - d) < 0.01, "{a} -> {h} -> {back}");
        }
    }

    #[test]
    fn kelvin_goes_from_warm_to_cool() {
        let warm = kelvin_to_srgb(2000.0);
        let mid = kelvin_to_srgb(6500.0);
        let cool = kelvin_to_srgb(12000.0);
        assert!(warm[0] > warm[2] + 0.4, "{warm:?}");
        assert!(close(mid, [1.0, 1.0, 1.0], 0.08), "{mid:?}");
        assert!(cool[2] > cool[0] + 0.1, "{cool:?}");
    }
}
