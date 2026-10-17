//! Backdrop cache for the CPU compositor (prototype, off by default).
//!
//! A composite walks the layer stack bottom to top, so a change to layer `j` leaves the result of
//! layers `0..j` untouched. The cache keeps, per render tile, the backdrop *just before* layer
//! `j` (the first layer that changed since the last render) and the next render of the same tile
//! resumes the stack loop from there instead of from the bottom. Dragging an adjustment slider or
//! painting on a layer near the top of a tall stack then costs the layers from `j` up, not all of
//! them.
//!
//! Exactness is by construction: a snapshot is the very buffer the loop held when it reached `j`,
//! and it is only used while `layers[..j]` compare equal (`Layer: PartialEq`, which shares
//! pixel tiles by pointer first, so unchanged layers compare in microseconds) and the document
//! settings the composite reads are unchanged. Nothing is hashed, so a collision can't serve a
//! stale tile.
//!
//! Enable with [`set_enabled`] or `PHOTOCRAFT_STACK_CACHE=1`. Only [`crate::render`] uses it, and
//! only for RGB documents.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{DocId, Document, Layer, Pattern, Size};
use photocraft_geom::Rect;

use crate::Buffer;

/// Most snapshot bytes kept (all documents together). Past it, tiles simply aren't cached.
const BUDGET: usize = 256 << 20;
/// Documents with snapshots kept at once (least recently rendered goes first).
const MAX_DOCS: usize = 2;

fn flag() -> &'static AtomicBool {
    static F: OnceLock<AtomicBool> = OnceLock::new();
    F.get_or_init(|| AtomicBool::new(std::env::var("PHOTOCRAFT_STACK_CACHE").is_ok_and(|v| !v.is_empty() && v != "0")))
}

/// Turns the cache on or off for [`crate::render`].
pub fn set_enabled(on: bool) {
    flag().store(on, Ordering::Relaxed);
    if !on {
        purge();
    }
}

/// Whether [`crate::render`] uses the cache.
pub fn enabled() -> bool {
    flag().load(Ordering::Relaxed)
}

/// Drops every snapshot. Returns the bytes released.
pub fn purge() -> usize {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let n = c.iter().map(Entry::bytes).sum();
    c.clear();
    n
}

/// Bytes of snapshots currently held.
pub fn bytes() -> usize {
    cache().lock().unwrap_or_else(|e| e.into_inner()).iter().map(Entry::bytes).sum()
}

/// Tiles skipped below an opaque layer since start (for tests and benchmarks).
static OCCLUDED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_occluded() {
    OCCLUDED.fetch_add(1, Ordering::Relaxed);
}

/// How many tile composites started above an opaque layer instead of at the bottom.
pub fn occluded_tiles() -> usize {
    OCCLUDED.load(Ordering::Relaxed)
}

/// Layers an occluder search looks at per tile before giving up (each costs a read of the tile).
const OCCLUDER_CANDIDATES: usize = 4;

/// Whether `l`'s composite ignores the backdrop wherever its pixels are opaque: a visible, unclipped
/// pixel layer with Normal blending at full opacity and fill, no mask, effects, Blend If,
/// channel exclusions or non-default Advanced Blending, and nothing clipped to it.
fn replaces_backdrop(l: &Layer, above: Option<&Layer>) -> bool {
    l.visible
        && !l.clipped
        && l.blend == photocraft_color::BlendMode::Normal
        && l.opacity == 1.0
        && l.fill_opacity == 1.0
        && l.mask.is_none()
        && l.vector_mask.is_none()
        && !crate::effects::has_effects(l)
        && l.excluded_channels == 0
        && l.video.is_none()
        && l.advanced == Default::default()
        && l.blend_if == Default::default()
        && matches!(l.content, photocraft_doc::LayerContent::Raster(_))
        && !above.is_some_and(|a| a.clipped)
}

/// The topmost layer above `start` that hides everything under it over `tile`, if any: it
/// [`replaces_backdrop`] and every pixel of the tile is fully opaque.
pub(crate) fn occluder(layers: &[Layer], start: usize, tile: Rect) -> Option<usize> {
    let mut tried = 0;
    for i in (start + 1..layers.len()).rev() {
        let l = &layers[i];
        if !replaces_backdrop(l, layers.get(i + 1)) {
            continue;
        }
        let Some(s) = l.surface() else { continue };
        tried += 1;
        if tried > OCCLUDER_CANDIDATES {
            break;
        }
        if crate::surface_to_buffer(s, tile).px.iter().all(|p| p[3] >= 1.0) {
            return Some(i);
        }
    }
    None
}

type Tiles = HashMap<Rect, Arc<Buffer>>;

/// What a composite reads from the document besides its layers.
struct DocKey {
    size: Size,
    mode: ColorMode,
    depth: SampleType,
    icc: Option<Arc<Vec<u8>>>,
    light: String,
    patterns: Vec<Pattern>,
}

impl DocKey {
    fn of(doc: &Document) -> Self {
        Self {
            size: doc.size,
            mode: doc.mode,
            depth: doc.depth,
            icc: doc.icc_profile.clone(),
            light: format!("{:?}", doc.global_light),
            patterns: doc.patterns.clone(),
        }
    }

    fn matches(&self, doc: &Document) -> bool {
        self.size == doc.size
            && self.mode == doc.mode
            && self.depth == doc.depth
            && self.icc == doc.icc_profile
            && self.patterns == doc.patterns
            && self.light == format!("{:?}", doc.global_light)
    }
}

struct Entry {
    id: DocId,
    key: DocKey,
    /// The layers of the render that stored `tiles` (its first `cut` are what the tiles hold).
    layers: Vec<Layer>,
    /// Index of the layer the snapshots were taken before (0: none).
    cut: usize,
    tiles: Arc<Tiles>,
    used: u64,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.tiles.values().map(|b| b.px.len() * 16).sum()
    }
}

fn cache() -> &'static Mutex<Vec<Entry>> {
    static C: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

fn tick() -> u64 {
    static T: AtomicUsize = AtomicUsize::new(0);
    T.fetch_add(1, Ordering::Relaxed) as u64
}

/// How many leading layers of `a` and `b` are equal.
fn common_prefix(a: &[Layer], b: &[Layer]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Index of the first layer of the clipping group that contains layer `idx`, following the same
/// grouping as `composite_stack_at` (a layer clipped to a clipped layer is its own group).
fn group_start(layers: &[Layer], idx: usize) -> usize {
    let mut i = 0;
    while i < layers.len() {
        let mut j = i + 1;
        while j < layers.len() && layers[j].clipped && !layers[i].clipped {
            j += 1;
        }
        if idx < j {
            return i;
        }
        i = j.max(i + 1);
    }
    0
}

/// One render's use of the cache, made by [`plan`] and ended by [`Plan::finish`].
pub(crate) struct Plan {
    id: DocId,
    /// The document settings, when no entry holds them yet.
    key: Option<DocKey>,
    layers: Vec<Layer>,
    /// Layer index the tiles in `old` were taken before; a tile found there starts the loop at it.
    resume: usize,
    old: Arc<Tiles>,
    /// Layer index this render snapshots before (0: none).
    cut: usize,
    captured: Mutex<Tiles>,
    new_bytes: AtomicUsize,
}

/// Plans a render of `doc`: where each tile can resume and where it should snapshot.
pub(crate) fn plan(doc: &Document) -> Option<Plan> {
    if doc.mode != ColorMode::Rgb || doc.layers.len() < 2 {
        return None;
    }
    let n = doc.layers.len();
    let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = guard.iter().find(|e| e.id == doc.id && e.key.matches(doc));
    let (cut, resume, old) = match entry {
        None => (0, 0, Arc::default()),
        Some(e) => {
            let m = common_prefix(&e.layers, &doc.layers);
            if m == n && e.layers.len() == n {
                // Nothing changed: keep the snapshots and fill in tiles that have none.
                (e.cut, e.cut, e.tiles.clone())
            } else {
                // Layers `m..` changed: snapshot before the group holding the first one, and resume
                // from the previous snapshot while it is still below that.
                let cut = group_start(&doc.layers, m.min(n - 1));
                let resume = if e.cut > 0 && e.cut <= cut && !e.tiles.is_empty() { e.cut } else { 0 };
                (cut, resume, if resume > 0 { e.tiles.clone() } else { Arc::default() })
            }
        }
    };
    let key = entry.is_none().then(|| DocKey::of(doc));
    drop(guard);
    // Old snapshots only stay (and count against the budget) when this render keeps their cut.
    let held = if resume > 0 && resume == cut { old.values().map(|b| b.px.len() * 16).sum() } else { 0 };
    Some(Plan { id: doc.id, key, layers: doc.layers.clone(), resume, old, cut, captured: Mutex::default(), new_bytes: AtomicUsize::new(held) })
}

impl Plan {
    /// The snapshot a tile can start from, with the layer index to resume at.
    pub(crate) fn snapshot(&self, tile: Rect) -> Option<(usize, Arc<Buffer>)> {
        (self.resume > 0).then(|| self.old.get(&tile).map(|b| (self.resume, b.clone()))).flatten()
    }

    /// The layer index a tile that starts its loop at `start` snapshots before.
    pub(crate) fn capture_at(&self, start: usize) -> Option<usize> {
        (self.cut > start).then_some(self.cut)
    }

    /// Keeps `backdrop` (the buffer before layer [`Plan::cut`]) for the next render of `tile`.
    pub(crate) fn store(&self, tile: Rect, backdrop: &Buffer) {
        let bytes = backdrop.px.len() * 16;
        if self.new_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes > BUDGET {
            self.new_bytes.fetch_sub(bytes, Ordering::Relaxed);
            return;
        }
        self.captured.lock().unwrap_or_else(|e| e.into_inner()).insert(tile, Arc::new(backdrop.clone()));
    }

    /// Records this render for the next one.
    pub(crate) fn finish(self) {
        let captured = std::mem::take(&mut *self.captured.lock().unwrap_or_else(|e| e.into_inner()));
        let keep_old = self.resume > 0 && self.resume == self.cut;
        let tiles: Tiles = if keep_old { self.old.iter().map(|(k, v)| (*k, v.clone())).chain(captured).collect() } else { captured };
        let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
        let used = tick();
        let tiles = Arc::new(tiles);
        match guard.iter_mut().find(|e| e.id == self.id) {
            Some(e) => {
                if let Some(k) = self.key {
                    e.key = k;
                }
                e.layers = self.layers;
                e.cut = self.cut;
                e.tiles = tiles;
                e.used = used;
            }
            None => {
                let Some(key) = self.key else { return };
                guard.push(Entry { id: self.id, key, layers: self.layers, cut: self.cut, tiles, used });
            }
        }
        while guard.len() > MAX_DOCS {
            if let Some(i) = guard.iter().enumerate().min_by_key(|(_, e)| e.used).map(|(i, _)| i) {
                guard.swap_remove(i);
            }
        }
    }
}
