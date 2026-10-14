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
