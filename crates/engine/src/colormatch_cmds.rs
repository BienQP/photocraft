//! Layer › New Adjustment Layer › Color Match: matches the picture's colours to a reference (any
//! open document, or any layer, even a hidden one) and stores the result as a non-destructive
//! Color Lookup layer, so the match can be re-weighted, masked, hidden or exported as a `.cube`.
//! The maths is `photocraft_algo::colormatch`; the lookup table is the one Color Lookup already
//! applies, so the CPU and GPU compositors, PSD export and agents all see the same layer.

use std::sync::Arc;

use photocraft_algo::colormatch::{MAX_LUT, Method, Options, build_lut, fit, stats};
use photocraft_doc::Adjustment;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "layer.newColorMatchLayer";
const BATCH: &str = "layer.batchColorMatch";
/// Most documents one batch call matches.
const MAX_BATCH: usize = 64;

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

fn flag(p: &Value, key: &str) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| bad(format!("`{key}` must be true or false"))),
    }
}

fn layer_id(p: &Value, key: &str) -> Result<Option<u64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| bad(format!("`{key}` must be a layer id"))),
    }
}

fn number(p: &Value, key: &str, lo: f64, hi: f64, default: f64) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (lo..=hi).contains(&n) => Ok(n),
            _ => Err(bad(format!("`{key}` must be a number {lo}..{hi}"))),
        },
    }
}

fn new_color_match(s: &mut Session, p: &Value) -> Result<Value> {
    let method = match p.get("method") {
        None | Some(Value::Null) => Method::Covariance,
        Some(v) => v.as_str().and_then(Method::parse).ok_or_else(|| bad("`method` must be \"covariance\" or \"mean\""))?,
    };
    let strength = number(p, "strength", 0.0, 100.0, 100.0)? as f32 / 100.0;
    let size = number(p, "size", 9.0, MAX_LUT as f64, 33.0)?.round() as usize;
    let opts = Options { method, match_contrast: flag(p, "matchContrast")?, strength };

    let active = s.active_index().ok_or(EngineError::NoDocument)?;
    let source_doc = match p.get("source") {
        None | Some(Value::Null) => active,
        Some(v) => v.as_u64().map(|i| i as usize).filter(|i| *i < s.documents().len()).ok_or_else(|| bad("`source` must be an open document index"))?,
    };
    let source_layer = layer_id(p, "sourceLayer")?;
    if source_doc == active && source_layer.is_none() {
        return Err(bad("pick a reference: another document (`source`) or a layer (`sourceLayer`)"));
    }
    let (spx, smask) = {
        let d = s.documents().get(source_doc).ok_or_else(|| bad("no such document"))?;
        crate::adjust_cmds::stats_pixels(&d.doc, source_layer, flag(p, "useSelectionInSource")?)?
    };
    let (tpx, tmask) = {
        let d = s.documents().get(active).ok_or(EngineError::NoDocument)?;
        crate::adjust_cmds::stats_pixels(&d.doc, layer_id(p, "targetLayer")?, flag(p, "useSelectionInTarget")?)?
    };
    let source = stats(&spx, smask.as_deref()).ok_or_else(|| bad("the reference has no opaque pixels"))?;
    let target = stats(&tpx, tmask.as_deref()).ok_or_else(|| bad("the picture has no opaque pixels"))?;
    drop((spx, tpx));

    let lut = build_lut(&fit(&target, &source, &opts), size, opts.strength);
    let n = (lut.len() / 3).max(1);
    let edge = (n as f64).cbrt().round() as usize;

    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = p.get("exportPath").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let file = photocraft_cms::lutfile::LutFile {
            title: "Color Match".into(),
            size: edge,
            data: lut.clone(),
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
        };
        std::fs::write(path, photocraft_cms::lutfile::write_cube(&file)).map_err(|e| bad(format!("can't write {path}: {e}")))?;
    }

    let adj = Adjustment::ColorLookup { name: "Color Match".into(), lut: Some(Arc::new(lut)), size: edge as u32, tetrahedral: true, dither: false };
    let mut out = crate::commands::new_adjustment(s, adj, p, CMD)?;
    out["lutSize"] = json!(edge);
    Ok(out)
}


/// Matches several open documents to one reference: each gets its own Color Lookup layer (so each
/// can be adjusted or undone on its own). `source` is the reference document; `targets` the
/// documents to correct (default: every other open document). A target that fails (say, it has no
/// opaque pixels) is reported and the rest still run. The active document is restored afterwards.
fn batch_color_match(s: &mut Session, p: &Value) -> Result<Value> {
    let count = s.documents().len();
    let source = p.get("source").and_then(Value::as_u64).map(|i| i as usize).filter(|i| *i < count).ok_or_else(|| EngineError::BadParams { cmd: BATCH.into(), msg: "`source` must be an open document index".into() })?;
    let targets: Vec<usize> = match p.get("targets") {
        None | Some(Value::Null) => (0..count).filter(|i| *i != source).collect(),
        Some(v) => {
            let list = v.as_array().ok_or_else(|| EngineError::BadParams { cmd: BATCH.into(), msg: "`targets` must be a list of document indices".into() })?;
            let mut out = Vec::new();
            for t in list {
                let i = t.as_u64().map(|i| i as usize).filter(|i| *i < count && *i != source).ok_or_else(|| EngineError::BadParams { cmd: BATCH.into(), msg: "`targets` must be open document indices other than `source`".into() })?;
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            out
        }
    };
    if targets.is_empty() {
        return Err(EngineError::BadParams { cmd: BATCH.into(), msg: "no documents to match: open another document besides the reference".into() });
    }
    if targets.len() > MAX_BATCH {
        return Err(EngineError::BadParams { cmd: BATCH.into(), msg: format!("at most {MAX_BATCH} documents per batch") });
    }
    let home = s.active_index();
    let mut each = p.clone();
    each["source"] = json!(source);
    // The reference layer id is the reference document's; the targets are matched as a whole.
    if let Some(o) = each.as_object_mut() {
        o.remove("targets");
        o.remove("targetLayer");
        o.remove("exportPath");
    }
    let mut results = Vec::new();
    let mut matched = 0usize;
    for &t in &targets {
        s.execute("document.activate", json!({ "document": t }))?;
        match new_color_match(s, &each) {
            Ok(v) => {
                matched += 1;
                results.push(json!({ "document": t, "layer": v["layer"] }));
            }
            Err(e) => results.push(json!({ "document": t, "error": e.to_string() })),
        }
    }
    if let Some(h) = home {
        s.execute("document.activate", json!({ "document": h }))?;
    }
    Ok(json!({ "matched": matched, "results": results }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Color Match (Lookup)",
        menu: &["Layer", "New Adjustment Layer"],
        shortcut: None,
        params: r##"{"source":document index (default: the active document),"sourceLayer":layer id (a layer, hidden or not, as the reference),"targetLayer":layer id (default: the whole picture),"useSelectionInSource":bool=false,"useSelectionInTarget":bool=false,"method":"covariance|mean"="covariance","matchContrast":bool=false (take the reference's tonal range; off keeps the picture's own lighting),"strength":0..100=100,"size":9..65=33 (LUT edge),"exportPath":text (native: also write the table as a .cube)} → {layer, lutSize}. Needs a reference: another document or a layer."##,
        enabled: has_doc,
        run: new_color_match,
        journal: true,
    },
    CommandSpec {
        id: BATCH,
        label: "Color Match All Documents",
        menu: &["Layer", "New Adjustment Layer"],
        shortcut: None,
        params: r##"{"source":document index (the reference; required),"sourceLayer":layer id in it,"targets":[document indices] (default: every other open document),"method","matchContrast","strength","size","useSelectionInSource","useSelectionInTarget": as layer.newColorMatchLayer} → {matched, results:[{document, layer | error}]}. Each target gets its own Color Lookup layer; the active document is restored."##,
        enabled: has_doc,
        run: batch_color_match,
        journal: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(s: &mut Session, rgb: [f32; 3]) {
        s.edit("paint", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            for y in 0..16 {
                for x in 0..16 {
                    let v = 0.3 + 0.4 * (x as f32 / 15.0);
                    let mut px = vec![1.0f32; n];
                    px[..3].copy_from_slice(&[rgb[0] * v, rgb[1] * v, rgb[2] * v]);
                    surf.write_pixel(x, y, &px);
                }
            }
            Ok(())
        })
        .unwrap();
    }

    /// Document 0: a neutral grey ramp (the picture). Document 1: an orange ramp (the reference).
    fn two_docs(depth: u64) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16, "depth": depth})).unwrap();
        fill(&mut s, [1.0, 1.0, 1.0]);
        s.execute("file.new", json!({"width": 16, "height": 16, "depth": depth})).unwrap();
        fill(&mut s, [1.0, 0.5, 0.2]);
        s.execute("document.activate", json!({"document": 0})).unwrap();
        s
    }

    fn mid(s: &Session) -> [f32; 4] {
        photocraft_compose::flatten(&s.active().unwrap().doc).px[8 * 16 + 8]
    }

    #[test]
    fn matches_the_picture_to_another_document_at_every_depth() {
        for depth in [8, 16, 32] {
            let mut s = two_docs(depth);
            let before = mid(&s);
            let layers = s.active().unwrap().doc.layers.len();
            let out = s.execute(CMD, json!({"source": 1})).unwrap();
            assert_eq!(out["lutSize"], 33);
            assert_eq!(s.active().unwrap().doc.layers.len(), layers + 1, "one new adjustment layer");
            let after = mid(&s);
            assert!(after[0] > before[0] + 0.03 && after[2] < before[2] - 0.03, "depth {depth}: {before:?} → {after:?}");
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc.layers.len(), layers, "undo removes it");
        }
    }

    #[test]
    fn strength_zero_changes_nothing_and_a_layer_can_be_the_reference() {
        let mut s = two_docs(8);
        let before = mid(&s);
        s.execute(CMD, json!({"source": 1, "strength": 0})).unwrap();
        let after = mid(&s);
        for i in 0..3 {
            assert!((after[i] - before[i]).abs() < 0.01);
        }
        // The reference can be a hidden layer of the same document.
        let mut s = two_docs(8);
        s.execute("document.activate", json!({"document": 1})).unwrap();
        s.execute("layer.duplicate", json!({})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap().0;
        s.execute("document.activate", json!({"document": 0})).unwrap();
        assert!(s.execute(CMD, json!({"sourceLayer": id})).is_err(), "a layer of another document is not in this one");
    }

    #[test]
    fn batch_matches_every_other_document_and_restores_the_active_one() {
        let mut s = Session::new();
        for rgb in [[1.0, 1.0, 1.0], [0.9, 0.9, 1.0], [1.0, 0.5, 0.2]] {
            s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
            fill(&mut s, rgb);
        }
        // Documents 0 and 1 are bluish-grey ramps; document 2 (active) is the orange reference.
        let layers = |s: &Session, i: usize| s.documents()[i].doc.layers.len();
        let (n0, n1, n2) = (layers(&s, 0), layers(&s, 1), layers(&s, 2));
        let out = s.execute(BATCH, json!({"source": 2})).unwrap();
        assert_eq!(out["matched"], 2);
        assert_eq!((layers(&s, 0), layers(&s, 1), layers(&s, 2)), (n0 + 1, n1 + 1, n2), "the reference is left alone");
        assert_eq!(s.active_index(), Some(2), "the active document is restored");
        let px = |s: &Session, i: usize| photocraft_compose::flatten(&s.documents()[i].doc).px[8 * 16 + 8];
        assert!(px(&s, 0)[0] > 0.45 || px(&s, 1)[0] > 0.45, "they moved toward orange");
        // Explicit targets, and a partial failure doesn't stop the rest.
        let out = s.execute(BATCH, json!({"source": 2, "targets": [1]})).unwrap();
        assert_eq!(out["matched"], 1);
    }

    #[test]
    fn batch_bad_params_are_errors() {
        let mut s = two_docs(8);
        for p in [json!({}), json!({"source": 9}), json!({"source": 1, "targets": [1]}), json!({"source": 1, "targets": "x"}), json!({"source": 1, "targets": [9]}), json!({"source": 1, "strength": 500})] {
            let r = s.execute(BATCH, p.clone());
            assert!(r.is_err() || r.unwrap()["matched"] == 0, "{p}");
        }
        assert!(Session::new().execute(BATCH, json!({"source": 0})).is_err());
        assert_eq!(s.active_index(), Some(0), "still where it was");
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        let mut s = two_docs(8);
        for p in [
            json!({}),
            json!({"source": 9}),
            json!({"source": -1}),
            json!({"source": "x"}),
            json!({"source": 1, "method": "magic"}),
            json!({"source": 1, "strength": 101}),
            json!({"source": 1, "strength": "a"}),
            json!({"source": 1, "size": 2}),
            json!({"source": 1, "size": 1e9}),
            json!({"source": 1, "matchContrast": 3}),
            json!({"sourceLayer": 99999}),
            json!({"source": 1, "targetLayer": 99999}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
        let _ = s.execute(CMD, json!(7));
        let mut empty = Session::new();
        assert!(empty.execute(CMD, json!({"source": 0})).is_err());
    }

    #[test]
    fn exports_a_cube_that_reads_back() {
        let mut s = two_docs(8);
        let dir = std::env::temp_dir().join(format!("pc-colormatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("match.cube");
        s.execute(CMD, json!({"source": 1, "size": 9, "exportPath": path.to_str().unwrap()})).unwrap();
        let f = photocraft_cms::lutfile::parse("match.cube", &std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((f.size, f.data.len()), (9, 9 * 9 * 9 * 3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
