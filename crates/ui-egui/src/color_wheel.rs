//! Colour wheel for the Color panel: a hue ring around a saturation/brightness square, with
//! harmony markers (complementary, triad, …) on either the HSV hue wheel or a traditional
//! red-yellow-blue (RYB) wheel. Original design; the maths is in `photocraft_color::wheel`.

use egui::{Color32, Mesh, Pos2, Rect, Sense, Stroke, pos2, vec2};
use photocraft_color::wheel::{Harmony, clamp_chroma_to_gamut, harmony_hues, harmony_hues_ryb, oklch_to_srgb, rgb_to_ryb_hue, ryb_to_rgb_hue, wrap_hue};

use crate::color_picker_ui::{hsv_to_rgb, rgb_to_hsv};

/// Which hue wheel the ring shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WheelKind {
    /// Screen hues: red, yellow, green, cyan, blue, magenta.
    #[default]
    Hsb,
    /// Painter's wheel: red, yellow, blue primaries; red's complement is green, blue's is orange.
    Ryb,
    /// Perceptual wheel (OKLCH): the ring is hue, the square is chroma (across) and lightness (up).
    Oklch,
}

/// What the user did to the wheel this frame.
pub struct WheelEdit {
    /// New `[hue°, saturation, brightness]` (sRGB-based HSB), when the user changed it.
    pub hsb: Option<[f32; 3]>,
    /// The press ended this frame: a finished edit (the colour history records it).
    pub released: bool,
}

fn rgb32(c: [f32; 3]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]))
}

/// Ring angle (degrees, 0 at the top, clockwise) of an HSV hue on the chosen wheel.
fn ring_angle(kind: WheelKind, hue: f32) -> f32 {
    match kind {
        WheelKind::Hsb | WheelKind::Oklch => wrap_hue(hue),
        WheelKind::Ryb => rgb_to_ryb_hue(hue),
    }
}

/// HSV hue at a ring angle.
fn hue_at(kind: WheelKind, angle: f32) -> f32 {
    match kind {
        WheelKind::Hsb | WheelKind::Oklch => wrap_hue(angle),
        WheelKind::Ryb => ryb_to_rgb_hue(angle),
    }
}

fn polar(c: Pos2, r: f32, angle_deg: f32) -> Pos2 {
    let a = angle_deg.to_radians();
    pos2(c.x + r * a.sin(), c.y - r * a.cos())
}

/// The harmony's colours as `[hue°, s, v]`, base first. Monochromatic varies brightness instead.
pub fn harmony_colors(scheme: Harmony, kind: WheelKind, hsb: [f32; 3]) -> Vec<[f32; 3]> {
    if scheme == Harmony::Monochromatic {
        return [1.0f32, 0.8, 0.62, 0.45, 0.3].iter().map(|k| [hsb[0], hsb[1], (hsb[2] * k).clamp(0.0, 1.0)]).collect();
    }
    let hues = match kind {
        WheelKind::Hsb | WheelKind::Oklch => harmony_hues(scheme, hsb[0]),
        WheelKind::Ryb => harmony_hues_ryb(scheme, hsb[0]),
    };
    hues.into_iter().map(|h| [h, hsb[1], hsb[2]]).collect()
}

/// Draw the wheel in a `size` × `size` square and handle input. `hsb` is `[hue°, s, v]`.
pub fn wheel(ui: &mut egui::Ui, size: f32, hsb: [f32; 3], kind: WheelKind, scheme: Option<Harmony>, ring_edge: Color32) -> WheelEdit {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let c = rect.center();
    let outer = size * 0.5 - 2.0;
    let inner = outer * 0.80;
    let half = inner * std::f32::consts::FRAC_1_SQRT_2 * 0.94;
    let square = Rect::from_center_size(c, vec2(half * 2.0, half * 2.0));
    let painter = ui.painter_at(rect);

    // Hue ring.
    let segs = 120;
    let mut ring = Mesh::default();
    for i in 0..=segs {
        let a = 360.0 * i as f32 / segs as f32;
        let col = rgb32(hsv_to_rgb(hue_at(kind, a), 1.0, 1.0));
        ring.colored_vertex(polar(c, inner, a), col);
        ring.colored_vertex(polar(c, outer, a), col);
    }
    for i in 0..segs {
        let k = (i * 2) as u32;
        ring.add_triangle(k, k + 1, k + 3);
        ring.add_triangle(k, k + 3, k + 2);
    }
    painter.add(ring);
    painter.circle_stroke(c, outer, Stroke::new(1.0, ring_edge));
    painter.circle_stroke(c, inner, Stroke::new(1.0, ring_edge));

    // Saturation / brightness square at the current hue.
    let n = 12;
    let mut sq = Mesh::default();
    for j in 0..=n {
        for i in 0..=n {
            let (s, v) = (i as f32 / n as f32, 1.0 - j as f32 / n as f32);
            sq.colored_vertex(pos2(square.left() + s * square.width(), square.top() + (1.0 - v) * square.height()), rgb32(hsv_to_rgb(hsb[0], s, v)));
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let (b, d) = (a + 1, a + (n + 1) as u32 + 1);
            sq.add_triangle(a, b, d);
            sq.add_triangle(a, d, d - 1);
        }
    }
    painter.add(sq);
    painter.rect_stroke(square, 0.0, Stroke::new(1.0, ring_edge), egui::StrokeKind::Outside);

    // Harmony markers on the ring (no connecting lines, so nothing overlays the colours).
    let mid = (inner + outer) * 0.5;
    let marks: Vec<[f32; 3]> = match scheme {
        Some(s) if s != Harmony::Monochromatic => harmony_colors(s, kind, hsb),
        _ => vec![hsb],
    };
    let pts: Vec<Pos2> = marks.iter().map(|m| polar(c, mid, ring_angle(kind, m[0]))).collect();
    for (i, (m, p)) in marks.iter().zip(&pts).enumerate() {
        let r = if i == 0 { 7.0 } else { 5.5 };
        painter.circle_filled(*p, r, rgb32(hsv_to_rgb(m[0], m[1], m[2])));
        painter.circle_stroke(*p, r, Stroke::new(1.5, Color32::WHITE));
        painter.circle_stroke(*p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(150)));
    }
    // Square knob.
    let knob = pos2(square.left() + hsb[1] * square.width(), square.top() + (1.0 - hsb[2]) * square.height());
    painter.circle_stroke(knob, 5.0, Stroke::new(1.5, Color32::WHITE));
    painter.circle_stroke(knob, 6.5, Stroke::new(1.0, Color32::from_black_alpha(160)));

    // Input. Where the press lands decides the drag: a harmony marker, the ring, or the square.
    let id = resp.id.with("grab");
    let mut out = None;
    if (resp.drag_started() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        {
            let d = p - c;
            let grab = if let Some(i) = pts.iter().skip(1).position(|q| (p - *q).length() <= 9.0) {
                Grab::Marker(i + 1)
            } else if d.length() >= inner - 2.0 {
                Grab::Ring
            } else if square.expand(2.0).contains(p) {
                Grab::Square
            } else {
                Grab::None
            };
            ui.data_mut(|data| data.insert_temp(id, grab));
        }
    }
    if resp.dragged() || resp.clicked() {
        let grab = ui.data(|data| data.get_temp::<Grab>(id)).unwrap_or(Grab::None);
        if let Some(p) = resp.interact_pointer_pos() {
            match grab {
                Grab::Marker(i) => {
                    if let Some(m) = marks.get(i) {
                        out = Some(*m);
                    }
                }
                Grab::Ring => {
                    let d = p - c;
                    let a = wrap_hue(d.x.atan2(-d.y).to_degrees());
                    out = Some([hue_at(kind, a), hsb[1], hsb[2]]);
                }
                Grab::Square => {
                    let s = ((p.x - square.left()) / square.width()).clamp(0.0, 1.0);
                    let v = 1.0 - ((p.y - square.top()) / square.height()).clamp(0.0, 1.0);
                    out = Some([hsb[0], s, v]);
                }
                Grab::None => {}
            }
        }
    }
    WheelEdit { hsb: out, released: resp.drag_stopped() || resp.clicked() }
}

/// Largest chroma the OKLCH square reaches (sRGB's most saturated primaries are about 0.32).
pub const OKLCH_MAX_CHROMA: f32 = 0.32;
/// Lightness the OKLCH hue ring is drawn at.
const OKLCH_RING_L: f32 = 0.74;

/// An OKLCH colour clamped into the sRGB gamut, as encoded sRGB.
pub fn oklch_rgb(lch: [f32; 3]) -> [f32; 3] {
    oklch_to_srgb(clamp_chroma_to_gamut(lch))
}

/// The harmony's colours as in-gamut `[L, C, h°]`, base first. Monochromatic varies lightness.
pub fn oklch_harmony_colors(scheme: Harmony, lch: [f32; 3]) -> Vec<[f32; 3]> {
    if scheme == Harmony::Monochromatic {
        return [1.0f32, 0.82, 0.66, 0.5, 0.36].iter().map(|k| clamp_chroma_to_gamut([(lch[0] * k).clamp(0.0, 1.0), lch[1], lch[2]])).collect();
    }
    harmony_hues(scheme, lch[2]).into_iter().map(|h| clamp_chroma_to_gamut([lch[0], lch[1], h])).collect()
}

/// What the user did to the OKLCH wheel this frame.
pub struct OklchEdit {
    /// New in-gamut `[L, C, h°]`, when the user changed it.
    pub lch: Option<[f32; 3]>,
    pub released: bool,
}

/// The OKLCH wheel: hue ring plus a chroma × lightness square. `lch` is `[L, C, h°]`.
pub fn oklch_wheel(ui: &mut egui::Ui, size: f32, lch: [f32; 3], scheme: Option<Harmony>, ring_edge: Color32) -> OklchEdit {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let c = rect.center();
    let outer = size * 0.5 - 2.0;
    let inner = outer * 0.80;
    let half = inner * std::f32::consts::FRAC_1_SQRT_2 * 0.94;
    let square = Rect::from_center_size(c, vec2(half * 2.0, half * 2.0));
    let painter = ui.painter_at(rect);

    let segs = 120;
    let mut ring = Mesh::default();
    for i in 0..=segs {
        let a = 360.0 * i as f32 / segs as f32;
        let col = rgb32(oklch_rgb([OKLCH_RING_L, 0.15, a]));
        ring.colored_vertex(polar(c, inner, a), col);
        ring.colored_vertex(polar(c, outer, a), col);
    }
    for i in 0..segs {
        let k = (i * 2) as u32;
        ring.add_triangle(k, k + 1, k + 3);
        ring.add_triangle(k, k + 3, k + 2);
    }
    painter.add(ring);
    painter.circle_stroke(c, outer, Stroke::new(1.0, ring_edge));
    painter.circle_stroke(c, inner, Stroke::new(1.0, ring_edge));

    let n = 14;
    let mut sq = Mesh::default();
    for j in 0..=n {
        for i in 0..=n {
            let (x, l) = (i as f32 / n as f32, 1.0 - j as f32 / n as f32);
            sq.colored_vertex(
                pos2(square.left() + x * square.width(), square.top() + (1.0 - l) * square.height()),
                rgb32(oklch_rgb([l, x * OKLCH_MAX_CHROMA, lch[2]])),
            );
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let (b, d) = (a + 1, a + (n + 1) as u32 + 1);
            sq.add_triangle(a, b, d);
            sq.add_triangle(a, d, d - 1);
        }
    }
    painter.add(sq);
    painter.rect_stroke(square, 0.0, Stroke::new(1.0, ring_edge), egui::StrokeKind::Outside);

    let mid = (inner + outer) * 0.5;
    let marks: Vec<[f32; 3]> = match scheme {
        Some(s) if s != Harmony::Monochromatic => oklch_harmony_colors(s, lch),
        _ => vec![lch],
    };
    let pts: Vec<Pos2> = marks.iter().map(|m| polar(c, mid, m[2])).collect();
    for (i, (m, p)) in marks.iter().zip(&pts).enumerate() {
        let r = if i == 0 { 7.0 } else { 5.5 };
        painter.circle_filled(*p, r, rgb32(oklch_rgb(*m)));
        painter.circle_stroke(*p, r, Stroke::new(1.5, Color32::WHITE));
        painter.circle_stroke(*p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(150)));
    }
    let knob =
        pos2(square.left() + (lch[1] / OKLCH_MAX_CHROMA).clamp(0.0, 1.0) * square.width(), square.top() + (1.0 - lch[0]).clamp(0.0, 1.0) * square.height());
    painter.circle_stroke(knob, 5.0, Stroke::new(1.5, Color32::WHITE));
    painter.circle_stroke(knob, 6.5, Stroke::new(1.0, Color32::from_black_alpha(160)));

    let id = resp.id.with("grab");
    let mut out = None;
    if (resp.drag_started() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let grab = if let Some(i) = pts.iter().skip(1).position(|q| (p - *q).length() <= 9.0) {
            Grab::Marker(i + 1)
        } else if (p - c).length() >= inner - 2.0 {
            Grab::Ring
        } else if square.expand(2.0).contains(p) {
            Grab::Square
        } else {
            Grab::None
        };
        ui.data_mut(|data| data.insert_temp(id, grab));
    }
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        match ui.data(|data| data.get_temp::<Grab>(id)).unwrap_or(Grab::None) {
            Grab::Marker(i) => out = marks.get(i).copied(),
            Grab::Ring => {
                let d = p - c;
                out = Some(clamp_chroma_to_gamut([lch[0], lch[1], wrap_hue(d.x.atan2(-d.y).to_degrees())]));
            }
            Grab::Square => {
                let x = ((p.x - square.left()) / square.width()).clamp(0.0, 1.0);
                let l = 1.0 - ((p.y - square.top()) / square.height()).clamp(0.0, 1.0);
                out = Some(clamp_chroma_to_gamut([l, x * OKLCH_MAX_CHROMA, lch[2]]));
            }
            Grab::None => {}
        }
    }
    OklchEdit { lch: out, released: resp.drag_stopped() || resp.clicked() }
}

#[derive(Clone, Copy)]
enum Grab {
    Marker(usize),
    Ring,
    Square,
    None,
}

/// `[hue°, s, v]` of an sRGB colour.
pub fn hsb_of(rgb: [f32; 3]) -> [f32; 3] {
    rgb_to_hsv(rgb.map(|v| v.clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harmony_colors_keep_saturation_and_brightness() {
        for s in Harmony::ALL {
            for kind in [WheelKind::Hsb, WheelKind::Ryb] {
                let cols = harmony_colors(s, kind, [200.0, 0.6, 0.8]);
                assert!(!cols.is_empty());
                if s != Harmony::Monochromatic {
                    assert!(cols.iter().all(|c| c[1] == 0.6 && c[2] == 0.8));
                    assert!((cols[0][0] - 200.0).abs() < 0.01 || kind == WheelKind::Ryb);
                }
            }
        }
    }

    #[test]
    fn oklch_harmonies_stay_in_gamut_and_keep_lightness() {
        for s in Harmony::ALL {
            for h in (0..360).step_by(40) {
                let base = clamp_chroma_to_gamut([0.7, 0.3, h as f32]);
                let cols = oklch_harmony_colors(s, base);
                assert!(!cols.is_empty());
                for c in &cols {
                    assert!(photocraft_color::wheel::oklch_in_gamut(*c), "{s:?} {h}: {c:?}");
                    if s != Harmony::Monochromatic {
                        assert!((c[0] - 0.7).abs() < 1e-4);
                    }
                }
                let rgb = oklch_rgb(base);
                assert!(rgb.iter().all(|v| (0.0..=1.0).contains(v)));
            }
        }
        assert!(oklch_rgb([f32::NAN, f32::INFINITY, -4.0]).iter().all(|v| v.is_finite()));
    }

    #[test]
    fn ryb_ring_angle_round_trips() {
        for h in [0.0, 30.0, 60.0, 120.0, 200.0, 285.0, 359.0] {
            let back = hue_at(WheelKind::Ryb, ring_angle(WheelKind::Ryb, h));
            assert!((back - h).abs() < 0.01, "{h} -> {back}");
        }
    }

    #[test]
    fn complement_on_ryb_wheel_of_red_is_green() {
        let cols = harmony_colors(Harmony::Complementary, WheelKind::Ryb, [0.0, 1.0, 1.0]);
        assert!((cols[1][0] - 120.0).abs() < 1.0);
        let cols = harmony_colors(Harmony::Complementary, WheelKind::Hsb, [0.0, 1.0, 1.0]);
        assert!((cols[1][0] - 180.0).abs() < 0.01);
    }
}
