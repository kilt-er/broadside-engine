# `src/bin/render_refs.rs` — offline sprite-reference renderer

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/bin/render_refs.rs`](../LINE_BY_LINE.md#srcbinrender_refsrs) section of
`LINE_BY_LINE.md`.*

---

## Why this module exists

This is an **offline tool binary**, not part of the game. It renders the
procedural ship silhouette at fixed view angles and writes PNGs to
`docs/sprite-refs/`, giving bruce visual templates to hand-paint sprite art over
(the art that [`sprites.rs`](sprites.md) then loads). It's the bridge between the
procedural renderer's geometry and the hand-painted-PNG pipeline: the references
show exactly what silhouette + proportions a sprite should match per stance and
angle.

Run with `cargo run --bin render_refs --features render,runtime`. No TS analog —
purely a Rust-side art-production aid.

It outputs, per the SPRITE_SPEC table, `frigate_<stance>_<deg>.png` for stances
`bowOnFore`/`bowOnAft`/`broadside` × angles 0°/45°/90° (pure side / isometric /
top). Scout and Gunboat dims aren't defined yet — adding them is a one-line `CLASSES`
extension once their `ShipDims` land.

---

## Configuration (src/bin/render_refs.rs:26–68)

`ClassDef { name, dims }` (src/bin/render_refs.rs:26) + the `CLASSES` table
(currently just `frigate` with `FRIGATE_DIMS` from [`perspective.rs`](perspective.md)).
`ANGLES_DEG = [0, 45, 90]` (src/bin/render_refs.rs:39) — the three anchor angles
bruce paints variants of. `Orientation` (bowOnFore/bowOnAft/broadside) with `slug()`
+ the `ORIENTATIONS` list. `BG`/`FILL`/`STROKE` colors (src/bin/render_refs.rs:66)
match the renderer's player-hull palette so the references tone-match the live art.

## `fn main()` (src/bin/render_refs.rs:70)

Create `docs/sprite-refs/`, then triple-loop classes × angles × orientations:
`render_silhouette`, save the PNG named `<class>_<stance>_<deg>.png`, log it.

---

## `fn render_silhouette(dims, orient, deg) -> RgbaImage` (src/bin/render_refs.rs:88)

**Intent:** Render one silhouette at `deg`. Computes the foreshortened total height
(`dims.height * cos + depth * sin`), sizes a canvas with a 16-px margin, fills the
background, centers the silhouette, and dispatches to the per-stance rasterizer.
This mirrors how the live renderer foreshortens ship extent with the camera angle,
so the reference matches what the player will see.

## Rasterizers (src/bin/render_refs.rs:125–198)

- `rasterize_bow_on` (src/bin/render_refs.rs:125) — square stern + tapering bow
  triangle; the `bow_fore` flag flips which lane end the bow points to. The bow
  width foreshortens with `cos_a` (the bow flattens toward the top-down view).
- `rasterize_broadside` (src/bin/render_refs.rs:164) — main hull rectangle +
  superstructure bump (the bump height scales with `cos_a`).

## Raster primitives (src/bin/render_refs.rs:201–275)

`fill_quad` → `fill_polygon` (src/bin/render_refs.rs:214, a convex-polygon scanline
fill — for each scanline, find the 2 edge crossings and paint between them) and
`stroke_line` (src/bin/render_refs.rs:249, Bresenham 1-px line). These are a tiny
self-contained software rasterizer so the tool needs no GPU — it runs headless in
CI or on a build machine.

**Cross-references:** Reads `ShipDims` / `FRIGATE_DIMS` from
[`perspective.rs`](perspective.md). Produces the PNGs that
[`SPRITE_SPEC.md`](../SPRITE_SPEC.md) documents and that bruce paints over for
[`sprites.rs`](sprites.md) to load. Standalone — no game-runtime dependency.
