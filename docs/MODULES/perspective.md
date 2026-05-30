# `src/perspective.rs` — Module Companion

*A self-contained walkthrough of the screen-space projection layer. The same content
as the [`perspective.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcperspectivers),
but scoped: this file assumes you only care about how the simulation's cell-space
state maps to pixels and don't need the rest of the engine in context. Read this if
you are about to add a renderer feature, tune the lane geometry, or change a sprite
projection.*

**Source commit:** `70155ed` — *Port engine/perspective.ts to src/perspective.rs*.
**Mirrors:** `_drive_pull/broadside-engine/engine/perspective.ts`.
**Design anchor:** Slice A of the renderer plan; the TS `PERSPECTIVE.md` rationale doc
covers the visual decisions.

---

## Why this file exists

Broadside's simulation lives in lane-cell space: ships sit at integer cell indices
0..6 on a 1-D lane. The renderer needs pixels: a 660×240 viewport with a lane trapezoid
tilted slightly uphill to the right, ships projected with military axonometric, and
sprite vertices ready for wgpu's vertex buffers.

`perspective.rs` is the bridge. It is the **only module in the crate that knows about
screen coordinates.** Every other module — types, geometry, resolve, content, ai —
works in cell-space. Renderer code (`gfx.rs`, `hud.rs`, `bin/broadside.rs`) imports
from here to know where to draw.

Three things to know up front:

1. **Pure functions.** No wgpu, no winit, no rendering state. Take inputs, return
   geometry. The renderer composes the rotations into its vertex shader; this module
   just supplies pivots + angles + raw vertex arrays.
2. **TS is canonical, modulo three approved Rust-shape drifts.** Math is line-for-line
   ported from `perspective.ts`. Output shape changes for wgpu compatibility. The
   module rustdoc on lines 4–7 names the exception explicitly.
3. **The lane is a straight line.** A single constant rotation angle for all cells. A
   curved lane would need per-cell tangents; not implemented today.

---

## The six design decisions encoded here

Read the module rustdoc lines 9–24 before touching any function. They are summarized
here:

1. The lane is a tilted trapezoid running left-to-right, one-point perspective receding
   to the right. Vanishing point off-screen.
2. Cells get smaller along the lane: linear scale from `scale_near` (cell 0) to
   `scale_far` (cell N−1).
3. Ship sprites use **military axonometric** projection: port-starboard depth projects
   straight up in the ship's local unrotated frame, no foreshortening.
4. Every ship sprite is then rotated around its base by the lane's slope angle so its
   long axis aligns with the lane (bow-on) or runs perpendicular to it (broadside).
5. **Only the FRONT face and TOP face are rendered.** Side faces collapse to zero width
   under military projection. Intentional.
6. The lane is straight; rotation is one constant for every cell. Curve support is a
   future-care item.

---

## The public surface

Thirteen items, all `pub`:

| Item                                                  | Kind   | Role                                                          |
|-------------------------------------------------------|--------|---------------------------------------------------------------|
| `Point2`                                              | struct | 2-D screen-space point (pixels, y-down).                      |
| `LaneGeometry`                                        | struct | Lane footprint + cell count + scale gradient.                 |
| `DEFAULT_LANE`                                        | const  | 660×240-baseline `LaneGeometry`. Tuned for the TS viewport.   |
| `CellScreen`                                          | struct | One cell's screen position + scale + rotation.                |
| `cell_to_screen(cell_index, geom)`                    | fn     | Map integer cell index to `CellScreen`.                       |
| `fractional_cell_to_screen(fractional_cell, geom)`    | fn     | Same, for continuous (ordnance) positions, clamped to bounds. |
| `ShipDims`                                            | struct | A ship's world-unit length, beam, height.                     |
| `FRIGATE_DIMS`                                        | const  | The default-Frigate hull: 56 × 14 × 6.                        |
| `Stance`                                              | enum   | `BowOn` / `Broadside` — orientation for projection.           |
| `FacePoly = [Point2; 4]`                              | alias  | The four vertices of a face rect (CCW, screen y-down).        |
| `ShipSprite`                                          | struct | Output of `ship_sprite`: pivot + angle + face polys + anchors + bow direction. |
| `ship_sprite(cell, dims, stance)`                     | fn     | The core projection: vertices for a ship at a cell.           |
| `beam_endpoints(source, target, geom)`                | fn     | Two endpoints on the lane front edge for a weapon beam.       |
| `cell_footprint(cell_index, geom)`                    | fn     | Four-corner parallelogram for selection highlights.           |
| `band_between_cells(source, target)`                  | fn     | `RangeBand` between two cells. Thin wrapper over `geometry::range_band`. |

Plus one private function: `lane_slope_rad(geom)`, factored out for readability;
callers go through `cell_to_screen`'s `rotation_rad` field.

---

## How it all fits

For each frame the renderer needs to draw, the flow goes:

```
   for each ship in board.cells:
     cell = cell_to_screen(ship.cell, &lane_geom)
       └─► lane_slope_rad(&lane_geom)
     sprite = ship_sprite(cell, dims_for(ship), stance_from(ship.orientation))
       │      // sprite.front_face, sprite.top_face: vertex buffers
       │      // sprite.pivot, sprite.rotation_rad: shader inputs
       │      // sprite.bow_dir: chevron + beam-origin overlays
       └─► draw with wgpu instance transform = rotate(rotation_rad) about pivot

   for each in-flight projectile:
     cell = fractional_cell_to_screen(p.fractional_cell, &lane_geom)
     // draw projectile sprite at (cell.x, cell.y) with cell.scale

   for each active beam:
     (from, to) = beam_endpoints(src, tgt, &lane_geom)
     // draw line from (from.x, from.y) to (to.x, to.y) along the lane front edge

   for each selected/hovered cell:
     corners = cell_footprint(cell_index, &lane_geom)
     // draw highlight polygon
```

The renderer never recomputes lane math itself — every projection goes through this
module's functions with `&LaneGeometry` as the parameter.

---

## Function reference

Detailed line-by-line walkthroughs are in
[`LINE_BY_LINE.md` § src/perspective.rs](../LINE_BY_LINE.md#srcperspectivers). Quick
lookup table below.

### `cell_to_screen(cell_index: u32, geom: &LaneGeometry) -> CellScreen`
**Line:** `perspective.rs:106`. **Mirrors:** `perspective.ts` cellToScreen.
Linear interpolate (x, y, scale) along the front edge by `t = cell_index / (N − 1)`.
Returns rotation in **radians** (not degrees — Drift below).

### `fractional_cell_to_screen(fractional_cell: f32, geom: &LaneGeometry) -> CellScreen`
**Line:** `perspective.rs:118`. **Mirrors:** `perspective.ts` fractionalCellToScreen.
Same math as `cell_to_screen`, but `t` is `(fractional_cell / N).clamp(0.0, 1.0)`.
Used by ordnance entities mid-flight. Out-of-range inputs snap to nearest endpoint.

### `ship_sprite(cell: CellScreen, dims: ShipDims, stance: Stance) -> ShipSprite`
**Line:** `perspective.rs:185`. **Mirrors:** `perspective.ts` shipSprite. The core
projection. Computes:

- **Front face**: 4 vertices, `screen_w × screen_h` rectangle at the lane surface.
- **Top face**: 4 vertices, projected up by `depth_offset` (military axonometric).
- **`top_center`, `front_center`**: anchor points for chevron / bridge overlays.
- **`bow_dir`**: POST-rotation unit vector along the ship's bow direction. The only
  field that bakes in the rotation; everything else is in the unrotated frame.

Stance swap (lines 189–192): bow-on uses `(length, beam)` for (along-lane, depth);
broadside uses `(beam, length)`. Same world dimensions, swapped screen axes.

### `beam_endpoints(source_cell, target_cell, &geom) -> (Point2, Point2)`
**Line:** `perspective.rs:235`. **Mirrors:** `perspective.ts` beamEndpoints. Both
endpoints sit on the front edge — the beam visually rides the lane plane.

### `cell_footprint(cell_index, &geom) -> [Point2; 4]`
**Line:** `perspective.rs:244`. **Mirrors:** `perspective.ts` cellFootprint. Four
corners of a cell's trapezoid on the lane surface, order front-near / front-far /
back-far / back-near.

### `band_between_cells(source: u32, target: u32) -> RangeBand`
**Line:** `perspective.rs:264`. **Mirrors:** `perspective.ts` bandBetweenCells. Thin
wrapper over `geometry::range_band` — same answer, renderer-side caller. The
cross-check test at `perspective.rs:443` enforces parity over `(0..=9) × (0..=9)`.

---

## Drift from TypeScript

Three intentional drifts, all approved by team-lead and recorded in commit `70155ed`.
**All three are output-shape drifts only — the math is line-for-line TS.**

1. **`(pivot, rotation_rad)` instead of pre-baked SVG transform strings.** TS produces
   `"rotate(deg cx cy)"` strings ready for SVG. Rust returns the pivot point and angle
   separately. The wgpu vertex shader composes the rotation into its instance
   transform; it never wants strings.

2. **`[Point2; 4]` polygon arrays instead of formatted `"x,y x,y"` strings.** TS builds
   polygon point strings for SVG `<polygon points="">`. Rust returns the raw vertex
   array. wgpu wants vertex buffers.

3. **Rotation in radians, not degrees.** TS returns degrees because SVG
   `transform="rotate(deg)"` takes degrees natively. Rust returns radians because every
   downstream consumer (`f32::sin`/`cos`, rotation matrices, WGSL) wants radians.

The module rustdoc lines 26–34 cover drifts 1 and 2; the `CellScreen.rotation_rad`
doc comment at `perspective.rs:92` covers drift 3.

**One additional rename:** TS `bandBetweenCells` → Rust `band_between_cells`
(snake_case per language convention). Function body is identical.

**No other drift.** The math (linear interpolation, military-axonometric projection,
`atan2` slope) is line-for-line. If a fourth drift ever lands, it should be added to
this section.

---

## Tests

15 tests in `#[cfg(test)] mod tests` at `perspective.rs:273–458`. Test names read as
sentences:

```
cell_to_screen_near_matches_front_start
cell_to_screen_far_matches_front_end
cell_to_screen_midpoint_interpolates_evenly
lane_slope_is_modest_uphill_to_the_right
cell_to_screen_single_cell_lane_is_safe
fractional_cell_clamps_into_bounds
fractional_cell_at_4_matches_ts_reference
ship_sprite_bow_on_long_axis_runs_along_lane
ship_sprite_broadside_rotates_dimensions_90_degrees
ship_sprite_scales_with_cell_distance
ship_sprite_bow_dir_bow_on_points_along_lane
ship_sprite_bow_dir_broadside_points_off_lane
beam_endpoints_run_along_the_lane_front_edge
cell_footprint_returns_four_distinct_points
band_between_cells_matches_geometry_range_band
```

Two of these double as cross-port reference checks:

- **`fractional_cell_at_4_matches_ts_reference`** (`perspective.rs:337`) — asserts
  exact numeric output against the TS `render-example.ts` reference values to
  ±0.01 px. The canonical drift guard for projection math.
- **`band_between_cells_matches_geometry_range_band`** (`perspective.rs:443`) —
  iterates `(s, t)` in `0..=9 × 0..=9` and asserts the renderer-side wrapper agrees
  with `geometry::range_band` on every distance combination. The canonical drift
  guard against geometry / perspective getting out of sync on bucket boundaries.

---

## Cross-references

- **Type vocabulary:** `RangeBand` from [`src/types.rs`](types.md); `range_band` from
  [`src/geometry.rs`](geometry.md). Those are the only engine dependencies.
- **Consumers:** the renderer subtree — `gfx.rs`, `hud.rs`, `bin/broadside.rs`. None
  of those are documented yet (Slice A only just landed; documentation deferred until
  the renderer is feature-complete per Slices C/D/E).
- **Domain terms:** *Range band*, *Lane* (with `cell_count`, `scale_near/far`),
  *Bow-on / Broadside* in the [glossary](../GLOSSARY.md).
- **Design intent:** Slice A of the renderer plan; the TS `PERSPECTIVE.md` rationale
  doc covers the visual decisions in depth.
