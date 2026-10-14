//! Quick controls for the Brushes panel: a configurable mini panel of sliders for the current
//! brush. Pick which properties appear (the chips under the header); each shows as a slider
//! that edits the brush through the same `tools.setBrush` gesture as the Brush Settings panel.
//! The choice lives in [`crate::brush_panel::BrushesPanelState::quick`], so it is saved with the
//! panel state and drivable over the control channel.

use egui::RichText;
use photocraft_engine::paint::BrushSettings;

use crate::brush_panel::commit_gesture;
use crate::theme::Tokens;
use crate::{PhotocraftApp, widgets};

/// One property a quick slider can edit. Values are shown in display units (percent, degrees).
pub struct QuickProp {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub unit: &'static str,
    pub get: fn(&BrushSettings) -> f32,
    pub set: fn(&mut BrushSettings, f32),
}

fn pct(v: f32) -> f32 {
    if v.is_finite() { v * 100.0 } else { 0.0 }
}

fn frac(v: f32) -> f32 {
    if v.is_finite() { v / 100.0 } else { 0.0 }
}

/// The properties, in the order their chips are listed.
pub const PROPS: &[QuickProp] = &[
    QuickProp { id: "opacity", label: "Opacity", min: 0.0, max: 100.0, unit: "%", get: |b| pct(b.opacity), set: |b, v| b.opacity = frac(v).clamp(0.0, 1.0) },
    QuickProp { id: "flow", label: "Flow", min: 0.0, max: 100.0, unit: "%", get: |b| pct(b.flow), set: |b, v| b.flow = frac(v).clamp(0.0, 1.0) },
    QuickProp {
        id: "hardness",
        label: "Hardness",
        min: 0.0,
        max: 100.0,
        unit: "%",
        get: |b| pct(b.hardness),
        set: |b, v| b.hardness = frac(v).clamp(0.0, 1.0),
    },
    QuickProp { id: "spacing", label: "Spacing", min: 1.0, max: 1000.0, unit: "%", get: |b| pct(b.spacing), set: |b, v| b.spacing = frac(v).clamp(0.01, 10.0) },
    QuickProp {
        id: "angle",
        label: "Angle",
        min: -180.0,
        max: 180.0,
        unit: "°",
        get: |b| b.angle,
        set: |b, v| b.angle = if v.is_finite() { v.clamp(-180.0, 180.0) } else { 0.0 },
    },
    QuickProp {
        id: "roundness",
        label: "Roundness",
        min: 1.0,
        max: 100.0,
        unit: "%",
        get: |b| pct(b.roundness),
        set: |b, v| b.roundness = frac(v).clamp(0.01, 1.0),
    },
    QuickProp {
        id: "smoothing",
        label: "Smoothing",
        min: 0.0,
        max: 100.0,
        unit: "%",
        get: |b| pct(b.smoothing.amount),
        set: |b, v| b.smoothing.amount = frac(v).clamp(0.0, 1.0),
    },
];

/// The quick controls a new panel starts with.
pub fn default_quick() -> Vec<String> {
    ["opacity", "flow", "hardness"].iter().map(|s| (*s).to_string()).collect()
}

/// The property with `id`, if there is one.
pub fn prop(id: &str) -> Option<&'static QuickProp> {
    PROPS.iter().find(|p| p.id == id)
}

/// Turn `id` on or off in the list (unknown ids are ignored; the list keeps [`PROPS`] order).
pub fn toggle(list: &mut Vec<String>, id: &str) {
    if prop(id).is_none() {
        return;
    }
    if let Some(i) = list.iter().position(|x| x == id) {
        list.remove(i);
    } else {
        list.push(id.to_string());
    }
    list.retain(|x| prop(x).is_some());
    list.sort_by_key(|x| PROPS.iter().position(|p| p.id == x.as_str()).unwrap_or(usize::MAX));
    list.dedup();
}

/// Draw the mini panel and apply edits to the current brush.
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::CollapsingHeader::new(RichText::new("Quick controls").color(t.text_dim)).id_salt("brush-quick-controls").default_open(false).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
            for p in PROPS {
                let on = app.ui.brushes_panel.quick.iter().any(|x| x == p.id);
                if ui.selectable_label(on, p.label).on_hover_text("Show or hide this control").clicked() {
                    toggle(&mut app.ui.brushes_panel.quick, p.id);
                }
            }
        });
        let before = app.session.tools.brush.clone();
        let mut b = before.clone();
        let ids = app.ui.brushes_panel.quick.clone();
        for id in &ids {
            let Some(p) = prop(id) else { continue };
            let mut v = (p.get)(&b).clamp(p.min, p.max);
            if widgets::slider_row(ui, p.label, &mut v, p.min..=p.max, p.unit, None).changed() {
                (p.set)(&mut b, v);
            }
        }
        commit_gesture(app, ui.ctx(), &before, &b);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_property_round_trips_and_clamps() {
        for p in PROPS {
            let mut b = BrushSettings::default();
            (p.set)(&mut b, p.max);
            assert!(((p.get)(&b) - p.max).abs() < 0.01, "{}: max", p.id);
            (p.set)(&mut b, p.min);
            assert!(((p.get)(&b) - p.min).abs() < 0.01, "{}: min", p.id);
            // Hostile values never leave the brush out of range.
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e9, -1e9] {
                (p.set)(&mut b, v);
                let got = (p.get)(&b);
                assert!(got.is_finite() && got >= p.min - 0.01 - 0.01 * p.min.abs() && got <= p.max + 0.01, "{} {v}: {got}", p.id);
            }
        }
    }

    #[test]
    fn toggle_adds_removes_orders_and_ignores_unknown() {
        let mut l = default_quick();
        toggle(&mut l, "flow");
        assert_eq!(l, vec!["opacity", "hardness"]);
        toggle(&mut l, "smoothing");
        toggle(&mut l, "angle");
        assert_eq!(l, vec!["opacity", "hardness", "angle", "smoothing"]);
        toggle(&mut l, "nonsense");
        assert_eq!(l.len(), 4);
        let mut stale = vec!["gone".to_string(), "flow".to_string(), "flow".to_string()];
        toggle(&mut stale, "opacity");
        assert_eq!(stale, vec!["opacity", "flow"]);
    }
}
