//! Select › Luminosity Mask: selections from brightness (Lights, Darks, Mids, the eleven zones and
//! custom ranges) or from colour (a hue and its width), combined with the current selection by `mode`. The maths is
//! `photocraft_algo::luminosity`; this module reads the composite (or the active layer) in bands
//! and applies the result like every other selection command, so the Swift Panel, scripts and MCP
//! all select identically.

use photocraft_algo::luminosity::{COLOURS, LumKind, LumSpec, MAX_LEVEL, MAX_ZONE, MaskSpec, band_bytes_for};
use photocraft_algo::segment::{Sampler, SurfaceSampler};
use photocraft_algo::selection::{Region, SelectionMode, combine_region};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "select.luminosity";
/// Rows read per band, so a huge document never holds a full-size float copy.
const BAND_ROWS: i32 = 256;
/// Largest feather radius in pixels.
const MAX_FEATHER: f64 = 250.0;

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

struct CompositeSampler<'a>(&'a photocraft_doc::Document);

impl Sampler for CompositeSampler<'_> {
    fn rgba(&self, r: Rect) -> Vec<[f32; 4]> {
        photocraft_compose::render(self.0, r).px
    }
}

/// An integer param in `lo..=hi`, or `default` when absent. Anything else is an error.
fn int_param(p: &Value, key: &str, lo: u32, hi: u32, default: u32) -> Result<u32> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_u64() {
            Some(n) if (u64::from(lo)..=u64::from(hi)).contains(&n) => Ok(n as u32),
            _ => Err(bad(format!("`{key}` must be an integer {lo}..{hi}"))),
        },
    }
}

/// An optional number in `lo..=hi`, or `default` when absent. Anything else is an error.
fn num_param(p: &Value, key: &str, lo: f64, hi: f64, default: f64) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (lo..=hi).contains(&n) => Ok(n),
            _ => Err(bad(format!("`{key}` must be a number {lo}..{hi}"))),
        },
    }
}

fn parse_spec(p: &Value) -> Result<MaskSpec> {
    let kind = match p.get("kind") {
        None | Some(Value::Null) => "lights",
        Some(v) => v.as_str().ok_or_else(|| bad("`kind` must be a string"))?,
    };
    match kind {
        "range" => {
            let (low, high) = (num_param(p, "low", 0.0, 100.0, 30.0)?, num_param(p, "high", 0.0, 100.0, 70.0)?);
            if low > high {
                return Err(bad("`low` must not be above `high`"));
            }
            Ok(MaskSpec::Range { low: (low / 100.0) as f32, high: (high / 100.0) as f32, soft: (num_param(p, "softness", 0.0, 50.0, 10.0)? / 100.0) as f32 })
        }
        "color" | "colour" => {
            let hue = match (p.get("color").or_else(|| p.get("colour")), p.get("hue")) {
                (Some(c), _) if !c.is_null() => {
                    let name = c.as_str().ok_or_else(|| bad("`color` must be a name"))?;
                    COLOURS.iter().find(|(n, _)| *n == name || n.trim_end_matches('s') == name).map(|(_, h)| f64::from(*h)).ok_or_else(|| {
                        bad(format!("`color` must be one of {}", COLOURS.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")))
                    })?
                }
                (_, Some(h)) if !h.is_null() => num_param(p, "hue", 0.0, 360.0, 0.0)?,
                _ => 0.0,
            };
            Ok(MaskSpec::Hue {
                hue: hue as f32,
                width: num_param(p, "width", 0.0, 180.0, 30.0)? as f32,
                soft: num_param(p, "softness", 0.0, 180.0, 30.0)? as f32,
                min_chroma: (num_param(p, "minChroma", 0.0, 100.0, 8.0)? / 100.0) as f32,
            })
        }
        other => {
            let kind = LumKind::parse(other).ok_or_else(|| bad("`kind` must be lights, darks, mids, zone, range or color"))?;
            Ok(MaskSpec::Luma(LumSpec { kind, level: int_param(p, "level", 1, MAX_LEVEL, 1)?, zone: int_param(p, "zone", 0, MAX_ZONE, 5)? }))
        }
    }
}

fn parse_mode(p: &Value) -> Result<SelectionMode> {
    let m = match p.get("mode") {
        None | Some(Value::Null) => "replace",
        Some(v) => v.as_str().ok_or_else(|| bad("`mode` must be a string"))?,
    };
    match m {
        "replace" | "new" => Ok(SelectionMode::parse("replace")),
        "add" | "subtract" | "intersect" => Ok(SelectionMode::parse(m)),
        _ => Err(bad("`mode` must be replace, add, subtract or intersect")),
    }
}

fn label(spec: MaskSpec) -> String {
    match spec {
        MaskSpec::Luma(s) => match s.kind {
            LumKind::Lights => format!("Lights {} Mask", s.level),
            LumKind::Darks => format!("Darks {} Mask", s.level),
            LumKind::Mids => format!("Mids {} Mask", s.level),
            LumKind::Zone => format!("Zone {} Mask", s.zone),
        },
        MaskSpec::Range { low, high, .. } => format!("Range {:.0}–{:.0}% Mask", low * 100.0, high * 100.0),
        MaskSpec::Hue { hue, .. } => COLOURS.iter().find(|(_, h)| (*h - hue).abs() < 0.5).map_or_else(|| format!("Hue {hue:.0}° Mask"), |(n, _)| format!("{} Mask", n.trim_end_matches('s'))),
    }
}

fn select_luminosity(s: &mut Session, p: &Value) -> Result<Value> {
    let spec = parse_spec(p)?;
    let mode = parse_mode(p)?;
    let all = match p.get("sampleAllLayers") {
        None | Some(Value::Null) => true,
        Some(v) => v.as_bool().ok_or_else(|| bad("`sampleAllLayers` must be true or false"))?,
    };
    let feather = match p.get("feather") {
        None | Some(Value::Null) => 0.0,
        Some(v) => v.as_f64().filter(|n| n.is_finite() && (0.0..=MAX_FEATHER).contains(n)).ok_or_else(|| bad(format!("`feather` must be a number 0..{MAX_FEATHER}")))? as f32,
    };
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let canvas = doc.bounds();
    if canvas.is_empty() {
        return Err(bad("the document has no area"));
    }
    let len = (canvas.width() as usize).checked_mul(canvas.height() as usize).ok_or_else(|| bad("the document is too large"))?;
    let layer = d.active_layer.and_then(|id| doc.layer(id)).and_then(|l| l.surface());
    let mut mask: Vec<u8> = Vec::with_capacity(len);
    let mut y = canvas.y0;
    while y < canvas.y1 {
        let y1 = y.saturating_add(BAND_ROWS).min(canvas.y1);
        let band = Rect::new(canvas.x0, y, canvas.x1, y1);
        let px = match (all, layer) {
            (false, Some(surf)) => SurfaceSampler(surf).rgba(band),
            _ => CompositeSampler(&doc).rgba(band),
        };
        let want = band.width() as usize * band.height() as usize;
        if px.len() != want {
            return Err(bad("could not read the document's pixels"));
        }
        mask.extend(band_bytes_for(&px, spec));
        y = y1;
    }
    let total: f64 = mask.iter().map(|v| f64::from(*v) / 255.0).sum();
    let coverage = total / len.max(1) as f64;
    if mask.iter().all(|v| *v == 0) {
        return Ok(json!({ "selected": doc.selection.is_some(), "changed": false, "coverage": 0.0, "message": "No pixels in that range" }));
    }
    if feather > 0.0 {
        let soft: Vec<f32> = mask.iter().map(|v| f32::from(*v) / 255.0).collect();
        let soft = photocraft_algo::selection::feather(&soft, canvas.width() as usize, canvas.height() as usize, feather);
        mask = soft.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    }
    let region = Region { bbox: canvas, mask };
    let name = label(spec);
    let selected = s.edit(&name, |doc, _| {
        doc.selection = combine_region(doc.selection.as_ref(), Some(&region), mode);
        Ok(doc.selection.is_some())
    })?;
    let b = canvas;
    Ok(json!({ "selected": selected, "changed": true, "coverage": coverage, "bounds": [b.x0, b.y0, b.width() as i32, b.height() as i32] }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Luminosity Mask",
        menu: &["Select"],
        shortcut: None,
        params: r##"{"kind":"lights|darks|mids|zone|range|color"="lights","level":1..8=1 (lights/darks/mids: higher = narrower),"zone":0..10=5 (kind zone: the eleven-zone scale),"low":0..100=30,"high":0..100=70,"softness":range: 0..50 % (default 10) / color: 0..180° (default 30) (kind range: a brightness band, full between low and high %),"color":reds|yellows|greens|cyans|blues|magentas,"hue":0..360°,"width":0..180°=30,"minChroma":0..100 %=8 (kind color: pixels of that hue; greys and near-blacks fade out),"mode":"replace|add|subtract|intersect"="replace","sampleAllLayers":bool=true,"feather":0..250 px=0 (softens the mask's edge before it is combined)} → {selected, changed, coverage (0–1 of the canvas), bounds}"##,
        enabled: has_doc,
        run: select_luminosity,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document whose luminance runs from black (left) to white (right).
    fn ramp(depth: u64) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 20, "depth": depth})).unwrap();
        s.edit("paint", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            for y in 0..20 {
                for x in 0..100 {
                    let v = x as f32 / 99.0;
                    let mut px = vec![1.0f32; n];
                    px[..3].copy_from_slice(&[v, v, v]);
                    surf.write_pixel(x, y, &px);
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn cov(s: &Session, x: i32) -> f32 {
        s.active().unwrap().doc.selection.as_ref().map_or(0.0, |sel| sel.sample_channel(x, 10, 0))
    }

    #[test]
    fn lights_select_the_bright_side_at_every_depth() {
        for depth in [8, 16, 32] {
            let mut s = ramp(depth);
            let r = s.execute(CMD, json!({"kind": "lights"})).unwrap();
            assert_eq!(r["changed"], json!(true), "{depth}");
            assert!(cov(&s, 95) > 0.85 && cov(&s, 5) < 0.12, "{depth}: {} {}", cov(&s, 95), cov(&s, 5));
            s.undo();
            assert!(s.active().unwrap().doc.selection.is_none());
        }
    }

    #[test]
    fn darks_mids_and_zones_pick_their_ranges() {
        let mut s = ramp(8);
        s.execute(CMD, json!({"kind": "darks"})).unwrap();
        assert!(cov(&s, 3) > 0.9 && cov(&s, 96) < 0.1);
        s.execute(CMD, json!({"kind": "mids"})).unwrap();
        assert!(cov(&s, 50) > 0.9 && cov(&s, 2) < 0.2 && cov(&s, 97) < 0.2);
        s.execute(CMD, json!({"kind": "zone", "zone": 5})).unwrap();
        assert!(cov(&s, 50) > 0.9 && cov(&s, 10) < 0.05 && cov(&s, 90) < 0.05);
        s.execute(CMD, json!({"kind": "zone", "zone": 10})).unwrap();
        assert!(cov(&s, 99) > 0.95 && cov(&s, 50) < 0.05);
    }

    #[test]
    fn modes_combine_with_the_current_selection() {
        let mut s = ramp(8);
        s.execute(CMD, json!({"kind": "lights"})).unwrap();
        let before = cov(&s, 60);
        s.execute(CMD, json!({"kind": "darks", "mode": "intersect"})).unwrap();
        // Lights ∩ Darks leaves only the middle, weaker than either alone.
        assert!(cov(&s, 60) <= before + 1e-3);
        assert!(cov(&s, 99) < 0.05 && cov(&s, 1) < 0.05);
        let mut s = ramp(8);
        s.execute(CMD, json!({"kind": "lights"})).unwrap();
        s.execute(CMD, json!({"kind": "darks", "mode": "add"})).unwrap();
        assert!(cov(&s, 99) > 0.9 && cov(&s, 1) > 0.9);
        s.execute(CMD, json!({"kind": "lights", "mode": "subtract"})).unwrap();
        assert!(cov(&s, 99) < 0.1);
    }

    #[test]
    fn nothing_in_range_changes_nothing() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 30, "height": 30})).unwrap();
        s.execute("edit.fill", json!({"color": "#ffffff"})).ok();
        let r = s.execute(CMD, json!({"kind": "darks", "level": 8})).unwrap();
        assert_eq!(r["changed"], json!(false));
        assert!(s.active().unwrap().doc.selection.is_none());
    }

    /// Left half red, right half green.
    fn swatches() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 20})).unwrap();
        s.edit("paint", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            for y in 0..20 {
                for x in 0..100 {
                    let mut px = vec![1.0f32; n];
                    px[..3].copy_from_slice(if x < 50 { &[1.0, 0.0, 0.0] } else { &[0.0, 1.0, 0.0] });
                    surf.write_pixel(x, y, &px);
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    #[test]
    fn colour_masks_select_their_hue() {
        let mut s = swatches();
        s.execute(CMD, json!({"kind": "color", "color": "reds"})).unwrap();
        assert!(cov(&s, 10) > 0.95 && cov(&s, 90) < 0.05, "reds: {} {}", cov(&s, 10), cov(&s, 90));
        s.execute(CMD, json!({"kind": "color", "color": "green"})).unwrap();
        assert!(cov(&s, 90) > 0.95 && cov(&s, 10) < 0.05, "singular names work");
        s.execute(CMD, json!({"kind": "colour", "hue": 120, "width": 10, "softness": 5})).unwrap();
        assert!(cov(&s, 90) > 0.95 && cov(&s, 10) < 0.05, "a numeric hue");
        let none = s.execute(CMD, json!({"kind": "color", "color": "blues"})).unwrap();
        assert_eq!(none["changed"], json!(false), "nothing blue here: the selection is left alone");
        // Combining: reds, then add greens.
        let mut s = swatches();
        s.execute(CMD, json!({"kind": "color", "color": "reds"})).unwrap();
        s.execute(CMD, json!({"kind": "color", "color": "greens", "mode": "add"})).unwrap();
        assert!(cov(&s, 10) > 0.95 && cov(&s, 90) > 0.95);
    }

    #[test]
    fn custom_ranges_select_a_brightness_band() {
        let mut s = ramp(8);
        s.execute(CMD, json!({"kind": "range", "low": 40, "high": 60, "softness": 5})).unwrap();
        assert!(cov(&s, 50) > 0.95 && cov(&s, 20) < 0.05 && cov(&s, 80) < 0.05, "{} {} {}", cov(&s, 20), cov(&s, 50), cov(&s, 80));
        let mut s = ramp(16);
        s.execute(CMD, json!({"kind": "range", "low": 0, "high": 100})).unwrap();
        assert!(cov(&s, 0) > 0.95 && cov(&s, 99) > 0.95, "the whole range");
        assert!(s.execute(CMD, json!({"kind": "range", "low": 80, "high": 20})).is_err());
    }

    #[test]
    fn custom_and_colour_params_are_validated() {
        let mut s = swatches();
        for p in [
            json!({"kind": "color", "color": "purple"}),
            json!({"kind": "color", "color": 4}),
            json!({"kind": "color", "hue": 400}),
            json!({"kind": "color", "hue": "x"}),
            json!({"kind": "color", "width": -1}),
            json!({"kind": "color", "softness": 999}),
            json!({"kind": "color", "minChroma": 101}),
            json!({"kind": "range", "low": -1}),
            json!({"kind": "range", "high": 101}),
            json!({"kind": "range", "softness": 51}),
            json!({"kind": 7}),
            json!({"kind": "sepia"}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
    }

    #[test]
    fn feathering_softens_the_edge_and_is_validated() {
        let mut hard = ramp(8);
        hard.execute(CMD, json!({"kind": "zone", "zone": 10})).unwrap();
        let mut soft = ramp(8);
        soft.execute(CMD, json!({"kind": "zone", "zone": 10, "feather": 6})).unwrap();
        // Zone 10 is the bright end only: feathering lets some of it spill left of where the hard mask stops.
        let probe = (0..100).rev().find(|&x| cov(&hard, x) == 0.0).unwrap_or(0);
        assert!(cov(&soft, probe) > cov(&hard, probe), "{} vs {}", cov(&soft, probe), cov(&hard, probe));
        for bad in [json!({"feather": -1}), json!({"feather": 1000}), json!({"feather": "x"})] {
            assert!(soft.execute(CMD, bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        let mut s = ramp(8);
        for p in [
            json!({"kind": "x"}),
            json!({"kind": 3}),
            json!({"level": 0}),
            json!({"level": 9}),
            json!({"level": -1}),
            json!({"level": 1.5}),
            json!({"level": "a"}),
            json!({"zone": 11}),
            json!({"mode": "xor"}),
            json!({"mode": 5}),
            json!({"sampleAllLayers": "yes"}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
        // A non-object is refused or read as defaults; it never panics.
        let _ = s.execute(CMD, json!([1, 2]));
        let mut none = Session::new();
        assert!(none.execute(CMD, json!({})).is_err());
    }
}
