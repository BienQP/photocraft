# Swift tools

> **Last updated:** 2026-10-15 · **Status:** fork work, not yet upstream · **Not a Photoshop-parity item**

Original, clean-room tools added on the `feat/color-wheel-harmony` branch of the BienQP fork. They
are inspired by what third-party Photoshop panels do, implemented from public maths (Oklab,
Reinhard / Monge–Kantorovich colour transfer, raised-cosine masks, Hertzmann's painterly
rendering) and not from any plug-in's code. They don't move the Photoshop parity numbers in
[`target-app-parity.md`](target-app-parity.md), so those documents are unchanged.

Every tool is an engine command (so the UI, CLI, control channel and MCP all drive it) and has its
params documented in its `CommandSpec`.

| Tool | Where | Commands / code |
|---|---|---|
| Swift Panel: colour wheel, harmonies, extras | Color panel, wheel mode | `ui-egui/src/panels.rs` (`color_wheel_body`), `color::wheel` |
| Luminosity masks | Color panel › Luminosity | `select.luminosity` (`engine/luminosity_cmds.rs`, `algo/luminosity.rs`) |
| Swift Match | Color panel › Swift Match | `layer.newColorMatchLayer`, `layer.batchColorMatch` (`engine/colormatch_cmds.rs`, `algo/colormatch.rs`, `ui-egui/match_guides.rs`) |
| Swift Paint | Color panel › Swift Paint | `layer.swiftPaint` (`engine/swiftpaint_cmds.rs`, `algo/painterly.rs`) |
| Swift Brush | Brushes panel | `ui-egui/brush_swift.rs`, `brush_quick.rs`, `brush_preview.rs`, `brushes_tab.rs` |

## Luminosity masks

`select.luminosity` builds a selection from brightness (`lights`, `darks`, `mids`, one of eleven
`zone`s, or a custom `range`) or from colour (`color` by name or `hue`), with `feather`, a combine
`mode` and `sampleAllLayers`. The panel can preview the mask and save it as a channel. Colour
masks measure hue in encoded RGB.

## Swift Match

Matches the picture's colours to a reference (another document or a layer) in Oklab and bakes the
result into a non-destructive Color Lookup layer, so it can be masked, faded or hidden.
`method` is a full covariance transfer or a per-axis mean transfer; `matchContrast` also takes the
reference's tonal range. Options: selection-only on either side, Save LUT (`.cube`), and
`layer.batchColorMatch` to match every other open document.

**Guided colours** (`guides`, `onlyGuided`): each guide pins a picture colour (`from`) to a colour
it should become (`to`). Colours within `radius` (Oklab) follow, tapering over `softness`;
overlapping zones are normalised. With no `to` the zone keeps the plain match; `onlyGuided`
leaves everything outside the zones untouched. In the panel, a pipette beside each swatch reads
the colour off the image (stays armed until Esc, a tool change or a second click). Droppers
sample the active picture only, not the reference.

## Swift Paint

`layer.swiftPaint` repaints the picture as brush strokes: one raster layer per brush size inside a
"Swift Paint (preset)" group, so any pass can be masked or faded. Presets: impressionist, oil,
pointillist, wash. Each press uses a new seed.

## Swift Brush

Quick controls, brush-group colours and tags, and four stroke-preview styles. Tags and colours
live in panel UI state, not in presets or `.abr` files, and are lost when a group is renamed.

## Known limits

- Swift Paint is sequential and has not been timed on 24–36 MP images in release.
- Not verified on wasm in the build sandbox (`cargo xtask wasm` needs the target installed).
- Preview styles are original, not copies of any plug-in's.
