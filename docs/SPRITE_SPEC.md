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

### Frigate — `{ length: 120, beam: 60, height: 40 }`

Sized on the **6:3:2 length:beam:height ratio at N=20**. The bow-on
silhouette spans ~68% of one cell on `DEFAULT_LANE` (cell pitch 177);
adjacent ships at PointBlank fit without overlap.

| Angle | Stance | Width | total_h |
|------:|:-------|------:|--------:|
| 0°    | BowOn       | 120 | **40**  |
| 15°   | BowOn       | 120 | 54      |
| 30°   | BowOn       | 120 | 65      |
| 45°   | BowOn       | 120 | 71      |
| 60°   | BowOn       | 120 | 72      |
| 75°   | BowOn       | 120 | 68      |
| 90°   | BowOn       | 120 | **60**  |
| 0°    | Broadside   | 60  | **40**  |
| 15°   | Broadside   | 60  | 70      |
| 30°   | Broadside   | 60  | 95      |
| 45°   | Broadside   | 60  | 113     |
| 60°   | Broadside   | 60  | 124     |
| 75°   | Broadside   | 60  | 126     |
| 90°   | Broadside   | 60  | **120** |

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
- `*_side.png`: `width × height`. Frigate side: **120 × 40** (BowOn) or
  **60 × 40** (Broadside).
- `*_top.png`: `width × depth`. Frigate top: **120 × 60** (BowOn) or
  **60 × 120** (Broadside).

Anchor point: silhouette is centered both horizontally and vertically in
the PNG (the renderer overlays the sprite at the ship's lane position,
centered on the lane line).

The PNGs should have a transparent background; the bow direction (for
BowOn variants) is encoded in the sprite asymmetry — paint the bow at the
fore end of `bowOnFore_*.png`, at the aft end of `bowOnAft_*.png`.

### Fallback chain: paint just `bowOnFore_*.png`

You only need to paint **two PNGs per class**:
`<class>_bowOnFore_side.png` and `<class>_bowOnFore_top.png`. The
loader derives 3 of the other 4 sprite slots at load time. The full
chain per slot (in priority order):

| Slot                          | 1. Explicit | 2. Derived from                          | 3. Procedural |
|-------------------------------|:-----------:|:-----------------------------------------|:-------------:|
| `<class>_bowOnFore_*.png`     | yes         | —                                        | yes           |
| `<class>_bowOnAft_*.png`      | yes         | `mirror_horizontal(bowOnFore_<view>)`    | yes           |
| `<class>_broadside_top.png`   | yes         | `rotate_90_cw(bowOnFore_top)` (dims swap) | yes           |
| `<class>_broadside_side.png`  | yes         | — (no derivation defined)                 | yes           |

Net result: bruce paints just two files per class and the engine
produces five of the six views automatically. `broadside_side` is
the one slot with no auto-derivation — it's an end-on view of the
hull (front face, beam × height) that can't be reconstructed from
the side or top of a bow-on sprite. Until painted explicitly, it
renders as the procedural silhouette rectangle.

**Explicit overrides always win.** Drop a real `bowOnAft_*.png` or
`broadside_top.png` if a class has directional asymmetry (e.g. an
aft-mounted nacelle that shouldn't appear at the bow when mirrored;
or a broadside silhouette that should look different from the
rotated top view).

`rotate_90_cw` note: rotating `bowOnFore_top` (120×60 for Frigate)
swaps the dimensions to 60×120 — matching the broadside_top size
listed in the "Per-sprite PNG conventions" table above.

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
