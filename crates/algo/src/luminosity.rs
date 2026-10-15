//! Luminosity masks: selections built from how bright each pixel is.
//!
//! Clean-room, from the long-published technique: take the pixel's luminance `L` in `0..=1`, then
//!
//! * **Lights n**: `Lⁿ` (n = 1 is the plain luminance; each step multiplies the mask by itself,
//!   which narrows it toward the brightest pixels),
//! * **Darks n**: `(1 − L)ⁿ`,
//! * **Mids n**: `(4·L·(1 − L))ⁿ`, peaking at mid-grey,
//! * **Zone z** (0–10, the eleven-zone scale): a raised-cosine band centred on `z / 10` with
//!   half-width `0.1`. Neighbouring zones overlap so that all eleven sum to 1 everywhere, and
//!   the end zones (0 and 10) hold at full strength out to pure black and pure white.
//!
//! The mask is multiplied by the pixel's alpha, so transparent areas are never selected.

use photocraft_geom::Rect;

use crate::selection::Region;

/// Which luminosity mask to build.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LumKind {
    Lights,
    Darks,
    Mids,
    Zone,
}

impl LumKind {
    pub fn parse(s: &str) -> Option<LumKind> {
        match s {
            "lights" => Some(LumKind::Lights),
            "darks" => Some(LumKind::Darks),
            "mids" | "midtones" => Some(LumKind::Mids),
            "zone" => Some(LumKind::Zone),
            _ => None,
        }
    }
}

/// A luminosity mask: the kind plus its level (Lights/Darks/Mids: 1–8) or zone (0–10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LumSpec {
    pub kind: LumKind,
    pub level: u32,
    pub zone: u32,
}

/// Highest Lights / Darks / Mids level.
pub const MAX_LEVEL: u32 = 8;
/// Highest zone of the eleven-zone scale.
pub const MAX_ZONE: u32 = 10;

/// Luminance of an encoded RGB pixel (Rec.601 weights, as Grayscale conversion), `0..=1`.
pub fn luminance(rgb: [f32; 3]) -> f32 {
    let f = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    0.299 * f(rgb[0]) + 0.587 * f(rgb[1]) + 0.114 * f(rgb[2])
}

/// The mask value (0–1) at luminance `l`.
pub fn mask_value(l: f32, spec: LumSpec) -> f32 {
    let l = if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 };
    let n = spec.level.clamp(1, MAX_LEVEL) as i32;
    match spec.kind {
        LumKind::Lights => l.powi(n),
        LumKind::Darks => (1.0 - l).powi(n),
        LumKind::Mids => (4.0 * l * (1.0 - l)).powi(n),
        LumKind::Zone => {
            let z = spec.zone.min(MAX_ZONE);
            let c = z as f32 / 10.0;
            let w = 0.1;
            if (z == 0 && l <= c) || (z == MAX_ZONE && l >= c) {
                return 1.0;
            }
            let d = (l - c).abs();
            if d >= w { 0.0 } else { 0.5 * (1.0 + (std::f32::consts::PI * d / w).cos()) }
        }
    }
}

/// One mask byte for a straight-alpha RGBA pixel.
pub fn pixel_byte(px: [f32; 4], spec: LumSpec) -> u8 {
    let a = if px[3].is_finite() { px[3].clamp(0.0, 1.0) } else { 0.0 };
    (mask_value(luminance([px[0], px[1], px[2]]), spec) * a * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Mask bytes for `rows` rows of a band of RGBA pixels (row-major).
pub fn band_bytes(px: &[[f32; 4]], spec: LumSpec) -> Vec<u8> {
    px.iter().map(|p| pixel_byte(*p, spec)).collect()
}

/// A [`Region`] from a whole canvas worth of pixels laid out row-major over `bbox`. A length that
/// doesn't match the box gives an empty region instead of a panic.
pub fn region(px: &[[f32; 4]], bbox: Rect, spec: LumSpec) -> Region {
    let want = (bbox.width() as usize).checked_mul(bbox.height() as usize);
    if want != Some(px.len()) {
        return Region { bbox: Rect::EMPTY, mask: Vec::new() };
    }
    Region { bbox, mask: band_bytes(px, spec) }
}

// ---------- custom ranges and colour masks ----------

/// Any mask the Luminosity tools can build: a built-in luminosity curve, a custom brightness
/// range, or a colour (hue) mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MaskSpec {
    Luma(LumSpec),
    /// Everything between `low` and `high` (luminance, 0..=1) at full strength, fading out over
    /// `soft` (0..=0.5) beyond each end with a raised-cosine edge.
    Range { low: f32, high: f32, soft: f32 },
    /// Pixels whose hue is within `width` degrees of `hue` at full strength, fading out over a
    /// further `soft` degrees. Pixels with less than `min_chroma` (max − min of the RGB channels)
    /// fade out so greys and near-blacks, whose hue is noise, are not picked up.
    Hue { hue: f32, width: f32, soft: f32, min_chroma: f32 },
}

/// The six primary and secondary hues, as the colour buttons use them (name, degrees).
pub const COLOURS: [(&str, f32); 6] = [("reds", 0.0), ("yellows", 60.0), ("greens", 120.0), ("cyans", 180.0), ("blues", 240.0), ("magentas", 300.0)];

/// Hue in degrees (`0..360`) and chroma (`max − min`) of an encoded RGB pixel.
pub fn hue_chroma(rgb: [f32; 3]) -> (f32, f32) {
    let f = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let (r, g, b) = (f(rgb[0]), f(rgb[1]), f(rgb[2]));
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let c = mx - mn;
    if c <= 1e-6 {
        return (0.0, 0.0);
    }
    let h = if mx == r {
        ((g - b) / c).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / c + 2.0
    } else {
        (r - g) / c + 4.0
    };
    (h * 60.0, c)
}

fn clean(v: f32, lo: f32, hi: f32, d: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { d }
}

/// Raised-cosine falloff: 1 at `d = 0`, 0 at `d >= soft` (a hard step when `soft` is 0).
fn fall(d: f32, soft: f32) -> f32 {
    if d <= 0.0 {
        1.0
    } else if soft <= 1e-6 || d >= soft {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * d / soft).cos())
    }
}

/// The mask value (0–1) for an encoded RGB pixel.
pub fn mask_value_rgb(rgb: [f32; 3], spec: MaskSpec) -> f32 {
    match spec {
        MaskSpec::Luma(s) => mask_value(luminance(rgb), s),
        MaskSpec::Range { low, high, soft } => {
            let (low, high) = (clean(low, 0.0, 1.0, 0.0), clean(high, 0.0, 1.0, 1.0));
            let (low, high) = if low <= high { (low, high) } else { (high, low) };
            let soft = clean(soft, 0.0, 0.5, 0.0);
            let l = luminance(rgb);
            fall(low - l, soft) * fall(l - high, soft)
        }
        MaskSpec::Hue { hue, width, soft, min_chroma } => {
            let (h, c) = hue_chroma(rgb);
            let centre = clean(hue, -720.0, 720.0, 0.0).rem_euclid(360.0);
            let width = clean(width, 0.0, 180.0, 30.0);
            let soft = clean(soft, 0.0, 180.0, 30.0);
            let d = (h - centre).abs();
            let d = d.min(360.0 - d);
            let hue_w = fall(d - width, soft);
            let floor = clean(min_chroma, 0.0, 1.0, 0.08).max(1e-3);
            let chroma_w = (c / floor).clamp(0.0, 1.0);
            // Smooth the chroma ramp so the mask's edge doesn't show a kink.
            hue_w * chroma_w * chroma_w * (3.0 - 2.0 * chroma_w)
        }
    }
}

/// One mask byte for a straight-alpha RGBA pixel.
pub fn pixel_byte_for(px: [f32; 4], spec: MaskSpec) -> u8 {
    let a = if px[3].is_finite() { px[3].clamp(0.0, 1.0) } else { 0.0 };
    (mask_value_rgb([px[0], px[1], px[2]], spec) * a * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Mask bytes for a band of RGBA pixels (row-major).
pub fn band_bytes_for(px: &[[f32; 4]], spec: MaskSpec) -> Vec<u8> {
    px.iter().map(|p| pixel_byte_for(*p, spec)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: LumKind, level: u32, zone: u32) -> LumSpec {
        LumSpec { kind, level, zone }
    }

    #[test]
    fn lights_and_darks_follow_their_formulas() {
        for &l in &[0.0f32, 0.25, 0.5, 0.8, 1.0] {
            assert!((mask_value(l, spec(LumKind::Lights, 1, 0)) - l).abs() < 1e-6);
            assert!((mask_value(l, spec(LumKind::Lights, 3, 0)) - l.powi(3)).abs() < 1e-6);
            assert!((mask_value(l, spec(LumKind::Darks, 1, 0)) - (1.0 - l)).abs() < 1e-6);
            assert!((mask_value(l, spec(LumKind::Darks, 2, 0)) - (1.0 - l).powi(2)).abs() < 1e-6);
        }
        // Higher levels only ever narrow the mask.
        for l in (0..=100).map(|i| i as f32 / 100.0) {
            for k in [LumKind::Lights, LumKind::Darks, LumKind::Mids] {
                for n in 1..MAX_LEVEL {
                    assert!(mask_value(l, spec(k, n + 1, 0)) <= mask_value(l, spec(k, n, 0)) + 1e-6);
                }
            }
        }
    }

    #[test]
    fn mids_peak_at_mid_grey() {
        let s = spec(LumKind::Mids, 1, 0);
        assert!((mask_value(0.5, s) - 1.0).abs() < 1e-6);
        assert!(mask_value(0.0, s) < 1e-6 && mask_value(1.0, s) < 1e-6);
        assert!((mask_value(0.3, s) - mask_value(0.7, s)).abs() < 1e-6);
    }

    #[test]
    fn zones_sum_to_one_and_ends_hold() {
        for l in (0..=1000).map(|i| i as f32 / 1000.0) {
            let sum: f32 = (0..=MAX_ZONE).map(|z| mask_value(l, spec(LumKind::Zone, 1, z))).sum();
            assert!((sum - 1.0).abs() < 1e-4, "l={l}: {sum}");
        }
        assert_eq!(mask_value(0.0, spec(LumKind::Zone, 1, 0)), 1.0);
        assert_eq!(mask_value(1.0, spec(LumKind::Zone, 1, 10)), 1.0);
        assert!((mask_value(0.5, spec(LumKind::Zone, 1, 5)) - 1.0).abs() < 1e-6);
        assert_eq!(mask_value(0.5, spec(LumKind::Zone, 1, 2)), 0.0);
    }

    #[test]
    fn a_custom_range_is_full_inside_and_fades_outside() {
        let g = |l: f32| [l, l, l];
        let r = MaskSpec::Range { low: 0.3, high: 0.6, soft: 0.1 };
        for l in [0.3f32, 0.45, 0.6] {
            assert!((mask_value_rgb(g(l), r) - 1.0).abs() < 1e-5, "inside {l}");
        }
        assert!(mask_value_rgb(g(0.1), r) < 1e-5 && mask_value_rgb(g(0.9), r) < 1e-5, "far outside");
        let edge = mask_value_rgb(g(0.25), r);
        assert!(edge > 0.0 && edge < 1.0, "inside the falloff: {edge}");
        assert!((mask_value_rgb(g(0.25), r) - mask_value_rgb(g(0.65), r)).abs() < 1e-5, "symmetric");
        let hard = MaskSpec::Range { low: 0.3, high: 0.6, soft: 0.0 };
        assert_eq!((mask_value_rgb(g(0.29), hard), mask_value_rgb(g(0.31), hard)), (0.0, 1.0));
        // Swapped ends mean the same range.
        assert_eq!(mask_value_rgb(g(0.45), MaskSpec::Range { low: 0.6, high: 0.3, soft: 0.1 }), 1.0);
    }

    #[test]
    fn colour_masks_pick_their_hue_and_ignore_greys() {
        let reds = MaskSpec::Hue { hue: 0.0, width: 30.0, soft: 30.0, min_chroma: 0.08 };
        assert!(mask_value_rgb([1.0, 0.0, 0.0], reds) > 0.99, "pure red");
        assert!(mask_value_rgb([1.0, 0.1, 0.1], reds) > 0.9, "a pale red");
        assert!(mask_value_rgb([1.0, 0.0, 0.2], reds) > 0.9, "red wraps through 0 degrees");
        assert!(mask_value_rgb([0.0, 1.0, 0.0], reds) < 1e-5, "green is not red");
        assert!(mask_value_rgb([0.0, 0.0, 1.0], reds) < 1e-5);
        assert!(mask_value_rgb([0.5, 0.5, 0.5], reds) < 1e-5, "grey has no hue");
        assert!(mask_value_rgb([0.02, 0.0, 0.0], reds) < 0.5, "near-black is mostly ignored");
        // Each named colour finds itself.
        let probe = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0]];
        for (i, (name, hue)) in COLOURS.iter().enumerate() {
            let spec = MaskSpec::Hue { hue: *hue, width: 30.0, soft: 30.0, min_chroma: 0.08 };
            for (j, rgb) in probe.iter().enumerate() {
                let v = mask_value_rgb(*rgb, spec);
                if i == j {
                    assert!(v > 0.99, "{name}: {v}");
                } else {
                    assert!(v < 0.05, "{name} vs probe {j}: {v}");
                }
            }
        }
    }

    #[test]
    fn hue_is_measured_correctly_and_hostile_values_stay_defined() {
        let h = |c| hue_chroma(c).0;
        assert!((h([1.0, 0.5, 0.0]) - 30.0).abs() < 1e-3 && (h([0.0, 0.0, 1.0]) - 240.0).abs() < 1e-3 && (h([1.0, 0.0, 0.5]) - 330.0).abs() < 1e-3);
        for spec in [
            MaskSpec::Range { low: f32::NAN, high: f32::INFINITY, soft: -3.0 },
            MaskSpec::Range { low: 9.0, high: -9.0, soft: 9.0 },
            MaskSpec::Hue { hue: f32::NAN, width: f32::INFINITY, soft: -1.0, min_chroma: f32::NAN },
            MaskSpec::Hue { hue: 1e9, width: 1e9, soft: 1e9, min_chroma: -5.0 },
        ] {
            for rgb in [[f32::NAN, 0.5, 2.0], [0.0; 3], [1.0; 3], [0.9, 0.1, 0.4]] {
                let v = mask_value_rgb(rgb, spec);
                assert!((0.0..=1.0).contains(&v), "{spec:?} {rgb:?}: {v}");
            }
        }
        assert_eq!(pixel_byte_for([1.0, 0.0, 0.0, 0.5], MaskSpec::Hue { hue: 0.0, width: 30.0, soft: 30.0, min_chroma: 0.08 }), 128);
        assert_eq!(band_bytes_for(&[[1.0; 4]; 3], MaskSpec::Luma(spec(LumKind::Lights, 1, 0))), vec![255; 3]);
    }

    #[test]
    fn hostile_values_stay_defined() {
        for kind in [LumKind::Lights, LumKind::Darks, LumKind::Mids, LumKind::Zone] {
            for l in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -5.0, 9.0] {
                let v = mask_value(l, spec(kind, u32::MAX, u32::MAX));
                assert!((0.0..=1.0).contains(&v), "{kind:?} {l}: {v}");
            }
        }
        assert_eq!(pixel_byte([f32::NAN, 0.5, 0.5, f32::NAN], spec(LumKind::Lights, 1, 0)), 0);
    }

    #[test]
    fn alpha_scales_the_mask_and_regions_check_their_size() {
        let s = spec(LumKind::Lights, 1, 0);
        assert_eq!(pixel_byte([1.0, 1.0, 1.0, 1.0], s), 255);
        assert_eq!(pixel_byte([1.0, 1.0, 1.0, 0.0], s), 0);
        assert_eq!(pixel_byte([1.0, 1.0, 1.0, 0.5], s), 128);
        let px = vec![[1.0, 1.0, 1.0, 1.0]; 6];
        assert_eq!(region(&px, Rect::new(0, 0, 3, 2), s).mask.len(), 6);
        assert!(region(&px, Rect::new(0, 0, 4, 2), s).mask.is_empty());
        assert!(region(&px, Rect::new(0, 0, i32::MAX, i32::MAX), s).mask.is_empty());
    }
}
