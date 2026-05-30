# Sprite spec

Sizing and atlas-layout reference for hand-painted ship sprites.

The renderer draws each ship through a single silhouette polygon whose
total vertical extent interpolates with the camera angle. To make
hand-painted sprite art line up with that math, paint each ship at the
**side** (θ = 0°) and **top** (θ = 90°) extremes. The renderer blends
between the two via the camera-angle scrubber at runtime — you don't need
to paint the in-between frames unless you want higher polish.

## Virtual canvas

| | |
|---|---|
| Canvas size | **1320 × 480** virtual pixels |
| Lane center y | **240** (lane bisects the canvas horizontally) |
| Lane cells | 7 (default), evenly spaced from `x_left = 130` to `x_right = 1190` |
| Cell pitch | `(1190 - 130) / 6 = 176.7` design px per cell |

Ships sit centered on the lane line: half above `center_y`, half below.

## Ship bounding box vs. view angle

For each fixed scrub step the renderer computes
`total_h = height × cos(θ) + depth_dim × sin(θ)`, with width fixed at
`length` (BowOn) or `beam` (Broadside). `depth_dim` is the off-axis
extent: `beam` when bow-on (so the top face has depth = beam/2), `length`
when broadside (top face has depth = length/2).

### Frigate — `{ length: 168, beam: 42, height: 50 }`

| Angle | Stance | Width | total_h |
|------:|:-------|------:|--------:|
| 0°    | BowOn       | 168 | **50**  |
| 15°   | BowOn       | 168 | 59      |
| 30°   | BowOn       | 168 | 64      |
| 45°   | BowOn       | 168 | 65      |
| 60°   | BowOn       | 168 | 61      |
| 75°   | BowOn       | 168 | 54      |
| 90°   | BowOn       | 168 | **42**  |
| 0°    | Broadside   | 42  | **50**  |
| 15°   | Broadside   | 42  | 92      |
| 30°   | Broadside   | 42  | 127     |
| 45°   | Broadside   | 42  | 154     |
| 60°   | Broadside   | 42  | 170     |
| 75°   | Broadside   | 42  | 175     |
| 90°   | Broadside   | 42  | **168** |

Endpoints in **bold**: paint side.png to the bold-0° extent, top.png to
the bold-90° extent. The renderer interpolates between them at runtime.

### Scout / Gunboat — TBD

`ShipDims` for Scout and Gunboat aren't defined yet (the Frigate is the
only class today). When content lands them, regenerate this table with
`cargo run --bin render_refs --features render,runtime` and extend
`docs/sprite-refs/` with PNG references for each.

## Per-sprite PNG conventions

Filename: `assets/sprites/<class>_<stance>_<view>.png`

- `class` ∈ `{ frigate, scout, gunboat }`
- `stance` ∈ `{ bowOnFore, bowOnAft, broadside }`
- `view` ∈ `{ side, top }` — paint the 0° and 90° silhouettes only.

Pixel dimensions:
- `*_side.png`: `width × height`. Frigate side: **168 × 50** (BowOn) or
  **42 × 50** (Broadside).
- `*_top.png`: `width × depth`. Frigate top: **168 × 42** (BowOn) or
  **42 × 168** (Broadside).

Anchor point: silhouette is centered both horizontally and vertically in
the PNG (the renderer overlays the sprite at the ship's lane position,
centered on the lane line).

The PNGs should have a transparent background; the bow direction (for
BowOn variants) is encoded in the sprite asymmetry — paint the bow at the
fore end of `bowOnFore_*.png`, at the aft end of `bowOnAft_*.png`.

## Reference renders

`docs/sprite-refs/` contains procedural-silhouette PNGs at 0° / 45° / 90°
per stance. Use them as templates for your own art. Regenerate with:

```bash
cargo run --bin render_refs --features render,runtime
```

Layer hierarchy: the renderer fills the silhouette polygon with hull
paint and strokes the outline. Bruce's PNGs replace that with custom
pixel art; the polygon math stays in place as a fallback when a PNG is
missing.

## Atlas slot allocation

The procedural atlas is a 256 × 256 RGBA texture packed as an 8 × 8 grid
of 32 × 32 cells. Cells in use today:

| Row | Columns 0–7 |
|----:|:------------|
| 0 | BOW_CHEVRON, TORPEDO, MISSILE, –, –, –, –, – |
| 1 | GLYPH_BEAM, GLYPH_ORDNANCE, GLYPH_BROADSIDE, GLYPH_DISPLACEMENT, GLYPH_CONTROL, GLYPH_MOVEMENT, GLYPH_DEFENSIVE, – |
| 2 | TELEGRAPH_FIRE, TELEGRAPH_LOCK, TELEGRAPH_PUSH, TELEGRAPH_PULL, TELEGRAPH_REORIENT, TELEGRAPH_DEPLOY, –, – |
| 3 | STATUS_HULL_BREACH, STATUS_SYSTEMS_OFFLINE, STATUS_TARGET_LOCK, STATUS_SHIELDS_UP, –, –, –, – |
| 4 | PARALLAX_FAR_STARS, PARALLAX_NEBULA, PARALLAX_DISTANT_PLANET, PARALLAX_MID_STARS, PARALLAX_FOREGROUND_DUST, –, –, – |
| 5 | reserved (ship sprites — see notes below) |
| 6 | reserved (ship sprites — continued) |
| 7 | –, –, –, –, –, –, –, SOLID_WHITE |

Ship sprites are **not** packed into the procedural atlas — they're loaded
as separate textures from `assets/sprites/*.png` by `gfx.rs::load_sprite`.
Rows 5–6 of the procedural atlas are reserved for any future small
decorative sprites (weapon mounts, status pips, etc.).

## Side / top blend math

At view angle θ the fragment shader computes:

```
out_pixel = mix(side_pixel, top_pixel, sin(θ))
```

- θ = 0: `out_pixel = side_pixel` (pure side view, top fully invisible).
- θ = π/2: `out_pixel = top_pixel` (pure top view).
- θ = π/4: `out_pixel = 0.71 × side + 0.29 × top` — wait, that's not
  right. `sin(π/4) ≈ 0.707`, so it's actually `mix(side, top, 0.707)`,
  which expands to `0.293 × side + 0.707 × top`. At 45° the top sprite
  dominates the blend ~70 / 30.

The dominance ratio is intentional: at 45° the camera is already looking
more down than across, so the top silhouette carries the orientation
cue. Paint each side.png with strong asymmetry (pointy bow) so the
remaining 30% influence at 45° still reads.

If the linear-sin blend looks wrong at intermediate angles, the shader
can switch to a cosine-cross-fade (`smoothstep` or
`0.5 + 0.5 × cos(2θ - π)`) without changing the PNG conventions.
