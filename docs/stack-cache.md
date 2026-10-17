# Compositor backdrop cache and occlusion skip (prototype)

CPU compositor only (`photocraft_compose::render`), RGB documents only, **off by default**.
Turn it on with `photocraft_compose::stack_cache::set_enabled(true)` or `PHOTOCRAFT_STACK_CACHE=1`;
`perf_scenarios` takes `--stack-cache`. The GPU canvas has its own pipeline and is not affected.

## What it does

A composite walks the layer stack bottom to top, so changing layer `j` leaves layers `0..j`
unchanged. Per render tile the cache keeps the backdrop *just before* the first layer that changed
since the previous render; the next render of that tile resumes the stack loop from there.
Repeating edits of one layer (an opacity or adjustment slider, a brush stroke) then recomposites
that layer and the ones above it, not the whole stack.

Separately, a tile starts its loop at the topmost pixel layer that hides everything below it there:
visible, unclipped, Normal at 100 % opacity and fill, no mask, effects, Blend If, channel
exclusions or non-default Advanced Blending, nothing clipped to it, and every pixel of the tile
fully opaque. Normal blending of an opaque source ignores the backdrop, so the state from that
layer on is identical (the skipped layers and an empty backdrop give the same bits).

## Why it is exact

Nothing is hashed. A snapshot is the buffer the loop held when it reached layer `j`, and it is
used only while `layers[..j]` compare equal (`Layer: PartialEq`; pixel tiles compare by pointer
first, so unchanged layers cost microseconds) and the document settings the composite reads (size,
mode, depth, ICC profile, global light, patterns) are unchanged. `stack_cache_tests` render
edit sequences (opacity, blend, effects, adjustments, visibility, clipping, groups, pixel edits,
insert/reorder/delete, global light, other rects) both ways and require identical pixels; breaking
the change detection or the opacity test makes them fail.

## Limits

- Snapshots are 16 bytes per pixel, kept for the last two documents, at most 256 MiB in all; tiles
  past the budget are just not cached (a full-canvas edit on a 20 MP document caches ~80 % of it).
  Entries hold a clone of the document's layers, which keeps replaced pixel tiles alive until the
  next render of that document; `stack_cache::purge()` drops everything.
- A snapshot serves only the same tile rectangle, so a different damage rectangle starts cold.
- It saves the layers *below* the edit. The edited layer's own effects and everything above it
  (adjustment layers apply per pixel) are still recomposited.

## Numbers (2-core Linux sandbox, CPU compositor, 5472 × 3648 documents, p50)

| Row | Off | On |
|---|---|---|
| Layered: set top (full-canvas Levels) layer opacity | 1700 ms | 912 ms |
| Layered: set layer 2 of 12 opacity | 227 ms | 177 ms |
| Layered: second-from-top layer opacity | 14.6 ms | 12.2 ms |
| Photo stack: set panel opacity | 309 ms | 243 ms |
| Photo stack: set top Levels opacity | 1225 ms | 895 ms |

What remains is per-pixel work. Profiled with callgrind (single thread, 3.84 MP, one opacity
edit): about 1,100 instructions per pixel before and ~900 after the bit-identical fixes below. The
Levels path was dominated by seven libm `floorf` calls per pixel (`lut` interpolation and
`quantize`; x86-64 has no inline floor before SSE4.1, so Apple silicon never paid this) and by the
generic blend dispatch for Normal adjustment layers; `lut` and `quantize` now truncate and the
adjustment blend skips the dispatch for Normal, with property tests against the old arithmetic.
The layer-effects composite (`composite_with_effects_prepared`, ~22 % of an edit) is spread over a
dozen full-tile passes and buffers with no single hot loop; a real gain there needs a different
structure, not a faster loop.
