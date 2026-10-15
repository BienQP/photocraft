//! Guided colours for Swift Match: colour pins that steer the match, with an eyedropper that
//! reads them off the image.
//!
//! A guide pins a colour in the picture (`from`) to the colour it should become (`to`). The
//! panel block lives in [`controls`]; the list is UI state (`app.ui.match_guides`) so the control
//! channel can read and drive it. Arming a swatch's dropper turns the next click on the image into
//! that swatch's colour, like the Properties Levels/Curves eyedroppers (`layer_picker`): it stays
//! armed until Esc, another tool, or a second click on the same dropper.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use egui::RichText;

/// Most guides one match takes (the engine's limit).
pub const MAX: usize = photocraft_algo::colormatch::MAX_GUIDES;

/// One guided target: a picture colour, optionally the colour it should become.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchGuide {
    pub from: [u8; 3],
    pub to: [u8; 3],
    pub use_to: bool,
    /// Range in hundredths of an Oklab unit (1..=60).
    pub radius: f32,
}

impl Default for MatchGuide {
    fn default() -> Self {
        MatchGuide { from: [200, 60, 60], to: [230, 130, 40], use_to: true, radius: 12.0 }
    }
}

/// Which swatch an armed dropper fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Slot {
    From,
    To,
}

/// An armed guide dropper: guide index, swatch, and the tool that was current when it was armed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchPick {
    pub guide: usize,
    pub slot: Slot,
    pub tool: Tool,
}

/// Whether a guide dropper is armed and still applies (same tool, guide still there).
pub fn armed(app: &PhotocraftApp) -> bool {
    app.ui.match_picker.as_ref().is_some_and(|p| p.tool == app.ui.tool && p.guide < app.ui.match_guides.len())
}

/// Drops a dropper left behind by a tool switch or a removed guide.
pub fn prune(app: &mut PhotocraftApp) {
    if app.ui.match_picker.is_some() && !armed(app) {
        app.ui.match_picker = None;
    }
}

/// Esc: disarms the dropper. Returns whether there was one.
pub fn cancel(app: &mut PhotocraftApp) -> bool {
    app.ui.match_picker.take().is_some()
}

fn to_bytes(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 })
}

/// Fills the armed swatch from the image at document point (x, y). Returns whether a dropper was
/// armed (and so owns the click), even if the sample failed (off the image): the dropper stays armed.
pub fn sample_at(app: &mut PhotocraftApp, x: f64, y: f64) -> bool {
    if !armed(app) {
        return false;
    }
    let Some(pick) = app.ui.match_picker.clone() else { return false };
    let Some(c) = crate::canvas::composite_color(app, x, y) else { return true };
    if let Some(g) = app.ui.match_guides.get_mut(pick.guide) {
        let rgb = to_bytes(c);
        match pick.slot {
            Slot::From => g.from = rgb,
            Slot::To => {
                g.to = rgb;
                g.use_to = true;
            }
        }
    }
    true
}

/// The `guides` / `onlyGuided` params for the engine (an empty object with no guides).
pub fn params(guides: &[MatchGuide], only_guided: bool) -> Value {
    if guides.is_empty() {
        return json!({});
    }
    let list: Vec<Value> = guides
        .iter()
        .take(MAX)
        .map(|g| {
            let mut v = json!({"from": g.from, "radius": (g.radius / 100.0).clamp(0.01, 0.6)});
            if g.use_to {
                v["to"] = json!(g.to);
            }
            v
        })
        .collect();
    json!({"guides": list, "onlyGuided": only_guided})
}

fn dropper(app: &mut PhotocraftApp, ui: &mut egui::Ui, guide: usize, slot: Slot) {
    let selected = app.ui.match_picker.as_ref().is_some_and(|p| p.guide == guide && p.slot == slot);
    let tip = match slot {
        Slot::From => "Pick this colour from the image",
        Slot::To => "Pick the target colour from the image",
    };
    if crate::icons::button(ui, "pipette", 22.0, selected, tip).clicked() {
        app.ui.match_picker = if selected { None } else { Some(MatchPick { guide, slot, tool: app.ui.tool }) };
    }
}

/// The "Guided colours" block of Swift Match. Returns the engine params for the guides.
pub fn controls(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> Value {
    egui::CollapsingHeader::new("Guided colours").id_salt("swift-colour-match-guides-hdr").default_open(false).show(ui, |ui| {
        ui.label(RichText::new("Pin a colour in the picture to a colour you want it to become. Nearby colours follow. Use the droppers to read colours off the image (Esc to stop).").size(11.0));
        let mut remove = None;
        for i in 0..app.ui.match_guides.len() {
            ui.horizontal(|ui| {
                if let Some(g) = app.ui.match_guides.get_mut(i) {
                    ui.color_edit_button_srgb(&mut g.from).on_hover_text("A colour in the picture");
                }
                dropper(app, ui, i, Slot::From);
                if let Some(g) = app.ui.match_guides.get_mut(i) {
                    ui.checkbox(&mut g.use_to, "→").on_hover_text("Turn on to choose the colour it becomes; off keeps the plain match for this colour");
                    ui.add_enabled_ui(g.use_to, |ui| {
                        ui.color_edit_button_srgb(&mut g.to).on_hover_text("The colour it should become");
                    });
                }
                dropper(app, ui, i, Slot::To);
                if ui.small_button("✕").on_hover_text("Remove this guide").clicked() {
                    remove = Some(i);
                }
            });
            if let Some(g) = app.ui.match_guides.get_mut(i) {
                crate::widgets::slider_row(ui, "Range", &mut g.radius, 1.0..=60.0, "", None);
            }
        }
        if let Some(i) = remove {
            app.ui.match_guides.remove(i);
            app.ui.match_picker = None;
        }
        if app.ui.match_guides.len() < MAX && ui.button("Add guide").clicked() {
            app.ui.match_guides.push(MatchGuide::default());
        }
        ui.checkbox(&mut app.ui.match_only_guided, "Match only the guided colours").on_hover_text("Everything outside the guide ranges stays as it was.");
    });
    params(&app.ui.match_guides, app.ui.match_only_guided)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_clamp_and_skip_the_target_when_off() {
        assert_eq!(params(&[], true), json!({}));
        let g = MatchGuide { from: [1, 2, 3], to: [4, 5, 6], use_to: false, radius: 900.0 };
        let v = params(&[g.clone(), MatchGuide { use_to: true, radius: 0.0, ..g }], true);
        assert_eq!(v["onlyGuided"], true);
        assert!(v["guides"][0].get("to").is_none());
        assert!((v["guides"][0]["radius"].as_f64().unwrap() - 0.6).abs() < 1e-6);
        assert_eq!(v["guides"][1]["to"], json!([4, 5, 6]));
        assert!((v["guides"][1]["radius"].as_f64().unwrap() - 0.01).abs() < 1e-6);
    }

    #[test]
    fn bytes_survive_hostile_floats() {
        assert_eq!(to_bytes([f32::NAN, 2.0, -1.0]), [0, 255, 0]);
        assert_eq!(to_bytes([0.5, 1.0, 0.0]), [128, 255, 0]);
    }

    fn app() -> PhotocraftApp {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 32, "height": 48})).unwrap();
        s.execute("edit.fill", json!({"color": "#b04020"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        PhotocraftApp::new(s, crate::Services::default())
    }

    #[test]
    fn the_dropper_fills_only_its_swatch_and_stays_armed() {
        let mut app = app();
        assert!(!sample_at(&mut app, 8.0, 8.0), "nothing armed: the click is not ours");
        app.ui.match_guides.push(MatchGuide::default());
        app.ui.match_guides.push(MatchGuide { use_to: false, ..MatchGuide::default() });
        app.ui.match_picker = Some(MatchPick { guide: 1, slot: Slot::To, tool: app.ui.tool });
        assert!(armed(&app));
        assert!(sample_at(&mut app, 8.0, 8.0));
        assert_eq!(app.ui.match_guides[1].to, [0xb0, 0x40, 0x20]);
        assert!(app.ui.match_guides[1].use_to, "picking a target turns it on");
        assert_eq!(app.ui.match_guides[0], MatchGuide::default());
        assert!(sample_at(&mut app, 60.0, 8.0), "stays armed; off-colour click still ours");
        assert_ne!(app.ui.match_guides[1].to, [0xb0, 0x40, 0x20]);
        assert!(sample_at(&mut app, -50.0, 8.0), "off the image is swallowed, not a panic");
        assert!(cancel(&mut app));
        assert!(!sample_at(&mut app, 8.0, 8.0));
    }

    #[test]
    fn a_dropper_on_a_removed_guide_or_another_tool_is_dropped() {
        let mut app = app();
        app.ui.match_guides.push(MatchGuide::default());
        app.ui.match_picker = Some(MatchPick { guide: 3, slot: Slot::From, tool: app.ui.tool });
        prune(&mut app);
        assert!(app.ui.match_picker.is_none());
        app.ui.match_picker = Some(MatchPick { guide: 0, slot: Slot::From, tool: Tool::Hand });
        app.ui.tool = Tool::Brush;
        prune(&mut app);
        assert!(app.ui.match_picker.is_none());
        assert!(!sample_at(&mut app, 1.0, 1.0));
    }
}
