//! Swift Paint: turns the picture (or one layer) into a painting made of brush strokes, one
//! transparent layer per brush size, inside a "Swift Paint" group. Because every pass is a normal
//! layer the result stays editable: mask a pass, fade it, repaint over it. The method is
//! `photocraft_algo::painterly` (Hertzmann's multi-size curved strokes); presets are original
//! parameter sets.

use photocraft_algo::painterly::{self, MAX_PASSES, MAX_PIXELS, PRESETS};
use photocraft_doc::{Layer, LayerId};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "layer.swiftPaint";

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// An optional number in `lo..=hi`; anything else is an error.
fn number(p: &Value, key: &str, lo: f64, hi: f64) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (lo..=hi).contains(&n) => Ok(Some(n)),
            _ => Err(bad(format!("`{key}` must be a number {lo}..{hi}"))),
        },
    }
}

fn swift_paint(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let bounds = d.doc.bounds();
    let (w, h) = (bounds.width() as usize, bounds.height() as usize);
    let n = w.checked_mul(h).ok_or_else(|| bad("the document is too large"))?;
    if n == 0 {
        return Err(bad("the document has no area"));
    }
    if n > MAX_PIXELS {
        return Err(bad(format!("the document is too large to paint ({n} pixels; the limit is {MAX_PIXELS})")));
    }
    let preset = match p.get("preset") {
        None | Some(Value::Null) => "impressionist",
        Some(v) => v.as_str().filter(|n| PRESETS.contains(n)).ok_or_else(|| bad(format!("`preset` must be one of {}", PRESETS.join(", "))))?,
    };
    let mut params = painterly::preset(preset, w.max(h)).ok_or_else(|| bad("unknown preset"))?;
    if let Some(size) = number(p, "brushSize", 0.1, 10.0)? {
        // Percent of the longest side for the biggest brush; the others keep their ratios.
        let top = params.radii.first().copied().unwrap_or(8.0).max(0.01);
        let want = (w.max(h) as f32 * size as f32 / 100.0).max(1.0);
        params.radii.iter_mut().for_each(|r| *r *= want / top);
    }
    if let Some(k) = number(p, "passes", 1.0, MAX_PASSES as f64)? {
        let mut radii = vec![params.radii.first().copied().unwrap_or(8.0)];
        while radii.len() < k.round() as usize {
            radii.push((radii.last().copied().unwrap_or(8.0) * 0.5).max(1.0));
        }
        params.radii = radii;
    }
    if let Some(v) = number(p, "opacity", 5.0, 100.0)? {
        params.opacity = v as f32 / 100.0;
    }
    if let Some(v) = number(p, "strokeLength", 1.0, painterly::MAX_STROKE_STEPS as f64)? {
        params.max_steps = v.round() as u32;
        params.min_steps = params.min_steps.min(params.max_steps);
    }
    if let Some(v) = number(p, "straightness", 0.0, 100.0)? {
        params.straightness = v as f32 / 100.0;
    }
    if let Some(v) = number(p, "jitter", 0.0, 100.0)? {
        params.jitter = v as f32 / 100.0;
    }
    if let Some(v) = number(p, "bristles", 0.0, 100.0)? {
        params.bristles = v as f32 / 100.0;
    }
    if let Some(v) = number(p, "edgeClip", 0.0, 100.0)? {
        params.clip = v as f32 / 100.0;
    }
    if let Some(v) = number(p, "detail", 0.0, 100.0)? {
        // More detail = a lower error threshold, so smaller brushes fire in more places.
        params.threshold = 0.16 * (1.0 - v as f32 / 100.0) + 0.01;
    }
    if let Some(v) = number(p, "seed", 0.0, 4_294_967_295.0)? {
        params.seed = v as u64;
    }

    // The picture to paint: one layer (hidden or not), or everything, over white.
    let src = match p.get("sourceLayer") {
        None | Some(Value::Null) => photocraft_compose::flatten(&d.doc).over_background([1.0, 1.0, 1.0]).px,
        Some(v) => {
            let id = v.as_u64().ok_or_else(|| bad("`sourceLayer` must be a layer id"))?;
            let layer = d.doc.layer(LayerId(id)).ok_or(EngineError::NoLayer(LayerId(id)))?;
            photocraft_compose::render_layer(layer, bounds).over_background([1.0, 1.0, 1.0]).px
        }
    };
    let fmt = d.doc.pixel_format();
    let doc_bounds = d.doc.bounds();
    // Each finished pass becomes a layer at once, so only one pass's pixels exist at a time.
    let mut children = Vec::new();
    let mut strokes = 0usize;
    let mut index = 0usize;
    painterly::paint_each(&src, w, h, &params, &mut |pass| {
        index += 1;
        strokes += pass.strokes;
        if pass.strokes == 0 {
            return Ok(());
        }
        let mut l = Layer::raster(format!("Pass {} · brush {:.0} px", index, pass.radius), fmt);
        let data: Vec<f32> = pass.px.iter().flat_map(|q| photocraft_raster::from_rgba(&fmt, *q)).collect();
        drop(pass);
        crate::pixels_mut(&mut l).map_err(|e| e.to_string())?.write_region(doc_bounds, &data);
        children.push(l);
        Ok(())
    })
    .map_err(bad)?;
    drop(src);

    let name = format!("Swift Paint ({preset})");
    let count = children.len();
    let id = s.edit("Swift Paint", |doc, active| {
        let group = Layer::group(doc.next_layer_name(&name), children);
        let id = doc.insert_above(*active, group);
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({ "layer": id.0, "passes": count, "strokes": strokes }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Swift Paint",
        menu: &["Filter", "Stylize"],
        shortcut: None,
        params: r##"{"preset":"impressionist|oil|pointillist|wash"="impressionist","brushSize":0.1..10 (% of the longest side, biggest brush),"passes":1..6 (brush sizes, each half the last),"opacity":5..100,"strokeLength":1..200 (steps),"straightness":0..100,"jitter":0..100 (colour variation),"bristles":0..100 (bristle streaks in each stroke; default from the preset, 0 = smooth),"edgeClip":0..100 (strokes stop at edges of the picture; default from the preset, 0 = off),"detail":0..100,"seed":integer,"sourceLayer":layer id (default: the whole picture)} → {layer (the new group), passes, strokes}. One layer per brush size inside a "Swift Paint" group."##,
        enabled: has_doc,
        run: swift_paint,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(depth: u64) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 48, "height": 32, "depth": depth})).unwrap();
        s.edit("paint", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            for y in 0..32 {
                for x in 0..48 {
                    let mut px = vec![1.0f32; n];
                    px[..3].copy_from_slice(&[x as f32 / 47.0, y as f32 / 31.0, 0.3]);
                    surf.write_pixel(x, y, &px);
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn top_group_children(s: &Session) -> usize {
        match &s.active().unwrap().doc.layers.last().unwrap().content {
            photocraft_doc::LayerContent::Group(g) => g.children.len(),
            _ => 0,
        }
    }

    #[test]
    fn paints_a_group_of_pass_layers_at_every_depth_and_undoes() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let before = s.active().unwrap().doc.layers.len();
            let out = s.execute(CMD, json!({"preset": "oil", "brushSize": 5, "passes": 2, "seed": 3})).unwrap();
            assert!(out["strokes"].as_u64().unwrap() > 0, "depth {depth}");
            assert_eq!(s.active().unwrap().doc.layers.len(), before + 1, "one group");
            assert!(top_group_children(&s) >= 1);
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc.layers.len(), before);
        }
    }

    #[test]
    fn every_preset_runs_and_the_same_seed_repeats() {
        for preset in PRESETS {
            let mut s = session(8);
            s.execute(CMD, json!({"preset": preset})).unwrap();
        }
        let run = || {
            let mut s = session(8);
            s.execute(CMD, json!({"seed": 7})).unwrap();
            photocraft_compose::flatten(&s.active().unwrap().doc).px
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn edge_clipping_and_bristles_are_accepted_at_both_ends() {
        let mut s = session(8);
        for v in [0, 100] {
            let out = s.execute(CMD, json!({"edgeClip": v, "bristles": v})).unwrap();
            assert!(out["strokes"].as_u64().unwrap() > 0, "value {v}");
        }
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        let mut s = session(8);
        for p in [
            json!({"preset": "nope"}),
            json!({"preset": 3}),
            json!({"brushSize": 0}),
            json!({"brushSize": 99}),
            json!({"passes": 0}),
            json!({"passes": 99}),
            json!({"opacity": "x"}),
            json!({"strokeLength": 1e9}),
            json!({"detail": -1}),
            json!({"bristles": -1}),
            json!({"bristles": 101}),
            json!({"bristles": "x"}),
            json!({"edgeClip": -1}),
            json!({"edgeClip": 101}),
            json!({"edgeClip": "x"}),
            json!({"seed": -1}),
            json!({"sourceLayer": 99999}),
            json!({"sourceLayer": "a"}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
        let _ = s.execute(CMD, json!(7));
        assert!(Session::new().execute(CMD, json!({})).is_err());
    }
}
