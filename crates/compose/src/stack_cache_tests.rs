//! The backdrop cache must be invisible: every render through it equals the plain render.

use super::*;
use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::adjust::LevelsChannel;
use photocraft_doc::{Adjustment, Document, Effect, FxCommon, Layer, LayerContent, Shadow};
use photocraft_geom::Size;

/// The cache is global: tests that count its bytes or purge it take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn solid(name: &str, rect: Rect, rgba: [f32; 4]) -> Layer {
    let mut l = Layer::raster(name, PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(rect, &rgba);
    l
}

fn shadow() -> Effect {
    Effect::DropShadow(Shadow {
        common: FxCommon::new(BlendMode::Normal, 0.75),
        color: Color::BLACK,
        angle: 120.0,
        use_global_light: false,
        distance: 9.0,
        spread: 0.0,
        size: 6.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: true,
    })
}

fn levels(gamma: f32) -> Adjustment {
    let ch = LevelsChannel { in_black: 0.05, in_white: 0.95, gamma, out_black: 0.0, out_white: 1.0 };
    Adjustment::Levels {
        master: LevelsChannel::default(),
        per_channel: [ch.clone(), ch.clone(), ch],
        space: Default::default(),
        black: LevelsChannel::default(),
    }
}

/// 700 × 520 (several render tiles): background, shapes with a shadow, a Multiply layer, Levels,
/// a layer with a clipped layer on it, and a group.
fn doc() -> Document {
    let mut d = Document::with_background("t", Size::new(700, 520), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    d.layers.push(solid("a", Rect::new(20, 20, 400, 300), [0.9, 0.2, 0.1, 1.0]));
    let mut b = solid("b", Rect::new(200, 100, 600, 450), [0.1, 0.5, 0.9, 1.0]);
    b.effects.items = vec![shadow()];
    d.layers.push(b);
    let mut c = solid("c", Rect::new(0, 200, 700, 330), [0.8, 0.8, 0.2, 0.8]);
    c.blend = BlendMode::Multiply;
    d.layers.push(c);
    d.layers.push(Layer::new("levels", LayerContent::Adjustment(levels(1.2))));
    d.layers.push(solid("e", Rect::new(300, 40, 520, 260), [0.2, 0.8, 0.3, 1.0]));
    let mut f = solid("f", Rect::new(380, 100, 700, 200), [0.9, 0.9, 0.9, 1.0]);
    f.clipped = true;
    d.layers.push(f);
    let mut gchild = solid("g1", Rect::new(50, 350, 350, 500), [0.3, 0.1, 0.6, 1.0]);
    gchild.opacity = 0.8;
    d.layers.push(Layer::group("grp", vec![gchild]));
    d
}

/// Renders `rect` of `d` both ways and requires identical pixels.
fn same(d: &Document, rect: Rect, what: &str) {
    let want = render_tiled(d, rect, RENDER_TILE);
    let got = render_cached(d, rect);
    assert_eq!(want.rect, got.rect, "{what}: rect");
    assert!(want.px == got.px, "{what}: cached render differs from the plain render");
}

fn full() -> Rect {
    Rect::new(0, 0, 700, 520)
}

#[test]
fn edits_render_the_same_with_and_without_the_cache() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    stack_cache::purge();
    let mut d = doc();
    let damage = Rect::new(150, 60, 640, 420);
    same(&d, full(), "cold");
    same(&d, full(), "unchanged");

    // Repeated edits of one layer: the first records a snapshot, the rest resume from it.
    for (i, o) in [0.5f32, 0.7, 0.3, 1.0].into_iter().enumerate() {
        d.layers[5].opacity = o;
        same(&d, damage, &format!("top layer opacity #{i}"));
        same(&d, damage, &format!("top layer opacity #{i} again"));
    }
    // The same rect through the cache after the edits above equals a full render's crop.
    let crop = render_cached(&d, damage);
    let all = render_tiled(&d, full(), RENDER_TILE);
    for y in damage.y0..damage.y1 {
        for x in damage.x0..damage.x1 {
            assert_eq!(crop.px[((y - damage.y0) as usize) * damage.width() as usize + (x - damage.x0) as usize], all.px[(y as usize) * 700 + x as usize]);
        }
    }

    // A slider on the adjustment layer in the middle of the stack.
    for (i, g) in [0.8f32, 1.0, 1.4, 2.0].into_iter().enumerate() {
        d.layers[3].content = LayerContent::Adjustment(levels(g));
        same(&d, damage, &format!("levels gamma #{i}"));
    }

    // Edits below the snapshot, then above it again.
    d.layers[2].blend = BlendMode::Overlay;
    same(&d, damage, "blend of a lower layer");
    d.layers[6].opacity = 0.4;
    same(&d, damage, "group opacity");
    d.layers[1].effects.items = vec![];
    same(&d, damage, "effects removed from a lower layer");
    d.layers[1].effects.items = vec![shadow()];
    same(&d, damage, "effects restored");

    // Visibility, including a clipped layer and the layer it is clipped to.
    d.layers[5].visible = false;
    same(&d, damage, "clipped layer hidden");
    d.layers[5].visible = true;
    d.layers[4].visible = false;
    same(&d, damage, "clipping base hidden");
    d.layers[4].visible = true;
    same(&d, damage, "clipping base shown");

    // Pixels change in a lower layer.
    d.layers[1].surface_mut().unwrap().fill_rect(Rect::new(250, 150, 330, 230), &[1.0, 1.0, 0.0, 1.0]);
    same(&d, damage, "pixels of a lower layer");
    d.layers[6].opacity = 0.9;
    same(&d, damage, "group opacity after pixel edit");

    // Stack shape: insert, reorder, delete.
    d.layers.insert(3, solid("ins", Rect::new(100, 100, 500, 400), [0.5, 0.5, 0.5, 0.5]));
    same(&d, damage, "layer inserted");
    d.layers.swap(1, 2);
    same(&d, damage, "layers swapped");
    d.layers.pop();
    same(&d, damage, "top layer deleted");
    d.layers.remove(1);
    same(&d, damage, "lower layer deleted");

    // A different rectangle of the same document (no tile matches the snapshots).
    same(&d, Rect::new(10, 10, 300, 300), "another rect");
    same(&d, damage, "back to the first rect");
    // And a rect that fits one tile.
    d.layers.last_mut().unwrap().opacity = 0.35;
    same(&d, Rect::new(300, 300, 400, 400), "single tile");
    same(&d, Rect::new(300, 300, 400, 400), "single tile again");
    stack_cache::purge();
}

#[test]
fn document_settings_the_composite_reads_invalidate_the_cache() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    stack_cache::purge();
    let mut d = doc();
    // The shadow of a layer below the snapshots follows the document's global light.
    d.layers[1].effects.items = vec![{
        let Effect::DropShadow(mut s) = shadow() else { panic!("shadow() is a drop shadow") };
        s.use_global_light = true;
        Effect::DropShadow(s)
    }];
    let damage = Rect::new(150, 60, 640, 420);
    d.layers[5].opacity = 0.5;
    same(&d, damage, "first");
    d.layers[5].opacity = 0.6;
    same(&d, damage, "snapshot taken");
    // Only the document changes: every layer is equal to the one the snapshots were made from.
    d.global_light.angle += 70.0;
    same(&d, damage, "global light changed");
    d.layers[5].opacity = 0.7;
    same(&d, damage, "snapshot after");
    d.global_light.angle -= 70.0;
    same(&d, damage, "global light changed back");
    stack_cache::purge();
}

#[test]
fn a_hit_skips_the_layers_below_the_edit() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    stack_cache::purge();
    let mut d = doc();
    let damage = Rect::new(150, 60, 640, 420);
    d.layers[6].opacity = 0.5;
    let _ = render_cached(&d, damage);
    d.layers[6].opacity = 0.6;
    let _ = render_cached(&d, damage);
    // Snapshots of the tiles now exist, taken before the group (index 6).
    assert!(stack_cache::bytes() > 0, "a snapshot was kept");
    let before = stack_cache::bytes();
    d.layers[6].opacity = 0.7;
    let _ = render_cached(&d, damage);
    assert_eq!(stack_cache::bytes(), before, "the same snapshots serve the next edit");
    stack_cache::purge();
    assert_eq!(stack_cache::bytes(), 0);
}

/// An opaque, non-uniform pixel layer over `rect`.
fn opaque(name: &str, rect: Rect) -> Layer {
    let mut l = solid(name, rect, [0.2, 0.3, 0.4, 1.0]);
    let s = l.surface_mut().unwrap();
    s.fill_rect(Rect::new(rect.x0 + 30, rect.y0 + 30, rect.x1 - 30, rect.y1 - 30), &[0.7, 0.6, 0.1, 1.0]);
    l
}

#[test]
fn layers_below_an_opaque_layer_are_skipped_without_changing_the_result() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    stack_cache::purge();
    let damage = Rect::new(150, 60, 640, 420);

    // Covering the whole canvas: every tile starts above it.
    let mut d = doc();
    d.layers.insert(3, opaque("photo", full()));
    let n = stack_cache::occluded_tiles();
    same(&d, full(), "full cover");
    assert!(stack_cache::occluded_tiles() >= n + 6, "tiles above a full-canvas opaque layer start at it");
    for (i, o) in [0.5f32, 0.8, 0.3].into_iter().enumerate() {
        d.layers[6].opacity = o;
        same(&d, damage, &format!("edit above the occluder #{i}"));
        same(&d, damage, &format!("edit above the occluder #{i} again"));
    }
    // Edits under the occluder change nothing it hides, and still match the plain render.
    d.layers[1].opacity = 0.3;
    same(&d, damage, "edit below the occluder");
    d.layers[2].blend = BlendMode::Screen;
    same(&d, damage, "blend below the occluder");

    // Covering only the left part: tiles on the left skip, the others don't.
    let mut d = doc();
    d.layers.insert(3, opaque("half", Rect::new(0, 0, 400, 520)));
    let n = stack_cache::occluded_tiles();
    same(&d, full(), "partial cover");
    let skipped = stack_cache::occluded_tiles() - n;
    assert!(skipped > 0 && skipped < 9, "some of the 9 tiles skip ({skipped})");

    // One pixel short of opaque, a mask, an opacity just under 1, another blend mode, a hidden
    // layer or a clipped layer on top: none of them hides what is below.
    type Break = (&'static str, usize, fn(&mut Document));
    // (what changes, tiles that may still start above the layer, the change). The first one only
    // spoils the tile it is in.
    let breaks: [Break; 6] = [
        ("one transparent pixel", 8, |d| d.layers[3].surface_mut().unwrap().fill_rect(Rect::new(333, 222, 334, 223), &[0.2, 0.3, 0.4, 0.99])),
        ("opacity 0.999", 0, |d| d.layers[3].opacity = 0.999),
        ("blend", 0, |d| d.layers[3].blend = BlendMode::Multiply),
        ("hidden", 0, |d| d.layers[3].visible = false),
        ("mask", 0, |d| d.layers[3].mask = Some(photocraft_doc::LayerMask::reveal_all())),
        ("clipped layer on it", 0, |d| {
            let mut c = solid("clip", Rect::new(0, 0, 700, 520), [1.0, 0.0, 0.0, 0.5]);
            c.clipped = true;
            d.layers.insert(4, c);
        }),
    ];
    for (name, allowed, apply) in breaks {
        let mut d = doc();
        d.layers.insert(3, opaque("photo", full()));
        apply(&mut d);
        let n = stack_cache::occluded_tiles();
        same(&d, full(), name);
        let hit = stack_cache::occluded_tiles() - n;
        assert_eq!(hit, allowed, "{name}: only tiles that really are hidden may start above the layer");
    }
    stack_cache::purge();
}
