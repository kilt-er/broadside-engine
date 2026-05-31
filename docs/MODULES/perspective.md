# `src/perspective.rs` — Module Companion

*A self-contained walkthrough of the screen-space lane geometry. The same content as
the [`perspective.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcperspectivers),
but scoped: this file assumes you only care about how cell-space positions map to
pixels and don't need the rest of the engine in context. Read this if you are about
to add a renderer feature that needs to know where the lane sits on screen.*

**Source commits:** `1d4d540` (flat-scene refactor — task #55, deletes the old
projection algorithm) + `2caa712` (revive view-angle scrubber on the flat base, task
#57) + `8d4a569` / `929fdf1` / `4367e8d` (ship-dim tuning) + `d4cd468` (canvas-resize
fix). Module is 203 lines, 7 inline tests, all green.

**Note:** This module used to be a mid-complexity 3-D projection layer
(tilted-trapezoid lane with military-axonometric ship sprites). The flat-scene
refactor (`1d4d540`) deleted the projection algorithm and moved ship-sprite math into
[`src/hud.rs`](hud.md). What remains is a near-trivial coordinate transform. If you
came here looking for `ShipSprite`, `FacePoly`, `cell_footprint`, or `beam_endpoints`,
they all moved to (or were absorbed by) `hud.rs`.

---

## Why this file exists

Broadside's simulation lives in lane-cell space: ships sit at integer cell indices
0..6 on a 1-D lane. The renderer needs pixels: a 1320×480 virtual canvas with a flat
horizontal lane bisecting it, ships morphing along a view-angle axis as the camera
scrubs, and parallax planes reacting to the same angle.

`perspective.rs` is the bridge between cell-space and pixel-space, **flat lane only**.
It is the only module in the crate that knows about screen coordinates. Every other
module — types, geometry, resolve, content, ai — works in cell-space. Renderer code
(`hud.rs`, `gfx.rs`, `bin/broadside.rs`) imports from here to know where the lane
sits and how cells space along it.

Three things to know up front:

1. **The lane is geometrically flat.** A horizontal strip at `center_y = 240` (half
   of `VIRTUAL_H`). Cells evenly spaced from `x_left = 130` to `x_right = 1190` —
   linear interpolation, nothing fancy.
2. **Ship projection is not in this file.** The visual depth — ships morphing from
   side-view to top-down as the camera scrubs — lives in `hud.rs`, driven by a
   runtime `view_angle` parameter. `perspective.rs` only supplies the *base
   position* `(x, y)` and the ship-dim constants.
3. **Pure functions, no state.** No wgpu, no winit, no rendering loop. Take inputs,
   return geometry.

---

## The flat-scene shift (from pre-flat)

If you remember the old `perspective.rs` walkthrough, this list catches you up:

| What was there pre-flat                            | What it became                                                     |
|----------------------------------------------------|--------------------------------------------------------------------|
| `LaneGeometry { front_*, back_*, scale_near/far }` | `LaneGeometry { center_y, x_left, x_right, cell_count }`           |
| `CellScreen { x, y, scale, rotation_rad }`         | `Point2 { x, y }` — `cell_to_screen` returns a 2-D position only.  |
| `lane_slope_rad()`                                 | Deleted. Lane has no slope.                                        |
| `ShipSprite`, `FacePoly`, `ship_sprite()`          | Deleted. Ship projection moved to `hud.rs`'s view-angle morph.     |
| `beam_endpoints()`                                 | Deleted. Inline in `hud.rs`'s beam path.                           |
| `cell_footprint()`                                 | Deleted. Cell highlights are axis-aligned rectangles in `hud.rs`.  |
| Per-cell sprite scaling (`scale_near` → `scale_far`) | Constant — every cell is the same size. The camera is square-on. |
| Per-cell sprite rotation                           | Constant zero — the lane has no slope to align with.               |

The view-angle morph in `hud.rs` stacks a **front face** of vertical extent
`height × cos(θ)` underneath a **top face** of vertical extent `beam × sin(θ) / 2`.
At `θ = 0` ships read as pure side silhouettes; at `θ = π/2` they read as pure
top-down rectangles. Default angle is 45° (task #57). Parallax planes (sky above the
lane, floor below) foreshorten with the same angle so the background reads as a
revolving camera (task #59).

---

## The public surface

Eight items, all `pub`:

| Item                                                 | Kind   | Role                                                          |
|------------------------------------------------------|--------|---------------------------------------------------------------|
| `Point2`                                             | struct | 2-D screen-space point (pixels, y-down).                      |
| `LaneGeometry`                                       | struct | Flat lane: `center_y`, `x_left`, `x_right`, `cell_count`.     |
| `LaneGeometry::cell_width()`                         | method | Half the distance between two adjacent cells.                 |
| `DEFAULT_LANE`                                       | const  | Tuned baseline for 1320×480: 7 cells, lane at `y = 240`.      |
| `cell_to_screen(cell_index, geom)`                   | fn     | Integer cell → `Point2`. Linear interpolation along x.        |
| `fractional_cell_to_screen(fractional_cell, geom)`   | fn     | Same, for continuous (ordnance) positions, clamped to bounds. |
| `ShipDims`                                           | struct | World-pixel dimensions: `length`, `beam`, `height`.           |
| `FRIGATE_DIMS`                                       | const  | The default Frigate: `168 × 42 × 50`.                         |
| `Stance`                                             | enum   | `BowOn` / `Broadside` — hull orientation for the morph.       |
| `band_between_cells(source, target)`                 | fn     | `RangeBand` between two cells; thin wrapper over `geometry::range_band`. |

No private helpers worth mentioning. The module has no `match`-based dispatch
because there's no projection logic to dispatch over.

---

## How it all fits

For each frame the renderer needs to draw, the flow is much shorter than pre-flat:

```
   for each ship in board.cells:
     base = cell_to_screen(ship.cell, &lane_geom)        // Point2 (x, y)
     dims = dims_for(ship)                                // ShipDims
     stance = stance_from(ship.orientation)               // Stance
     // hud.rs morphs the silhouette using base, dims, stance, and view_angle:
     //   front_h = dims.height * cos(view_angle)
     //   top_h   = dims.beam   * sin(view_angle) / 2
     // No projection function here — hud.rs owns it.

   for each in-flight projectile:
     pos = fractional_cell_to_screen(p.fractional_cell, &lane_geom)
     // draw projectile sprite at (pos.x, pos.y); size constant across the lane

   for each active beam:
     a = cell_to_screen(src, &lane_geom)
     b = cell_to_screen(tgt, &lane_geom)
     // hud.rs draws a line from a to b along the lane (both at center_y)
```

The renderer never recomputes lane math itself — every position query goes through
`cell_to_screen` or `fractional_cell_to_screen`.

---

## Function reference

Detailed entries are in
[`LINE_BY_LINE.md` § src/perspective.rs](../LINE_BY_LINE.md#srcperspectivers). Quick
lookup:

### `cell_to_screen(cell_index: u32, geom: &LaneGeometry) -> Point2`
**Line:** `perspective.rs:63`. Linear interpolate x from `x_left` to `x_right` by
`t = cell_index / (N − 1)`. `y` is constant at `center_y`. `saturating_sub(1)`
handles the single-cell degenerate.

### `fractional_cell_to_screen(fractional_cell: f32, geom: &LaneGeometry) -> Point2`
**Line:** `perspective.rs:79`. Same math, with `t` clamped to `[0, 1]`. Out-of-range
fractional positions snap to the nearest endpoint.

### `band_between_cells(source: u32, target: u32) -> RangeBand`
**Line:** `perspective.rs:127`. Thin wrapper over `geometry::range_band`. Both paths
must agree — the cross-module test at `perspective.rs:191` enforces parity over
`(0..=9) × (0..=9)`.

### `LaneGeometry::cell_width(&self) -> f32`
**Line:** `perspective.rs:42`. Half the distance between two adjacent cells. Used by
`hud.rs` to size ship silhouettes relative to lane spacing. Single-cell degenerate
returns the full span.

---

## Drift from the pre-flat module

The pre-flat module is documented at commit `47e9670`. Differences as of HEAD:

1. **Lane geometry: flat horizontal strip, not tilted trapezoid.** `LaneGeometry`
   shrank from 8 fields to 4. No `front_start` / `front_end` / `back_*` / `scale_*`.
2. **`cell_to_screen` returns `Point2`, not `CellScreen`.** No per-cell scale (lane
   is uniform); no per-cell rotation (lane is flat).
3. **Ship projection moved to `hud.rs`.** The military-axonometric algorithm is
   gone. `hud.rs` stacks front + top face heights via `cos(θ)` / `sin(θ)`, morphing
   the silhouette as the view angle scrubs. Default angle 45° per task #57.
4. **Ship dimensions grew ~3×** to fill the flat-scene cell. `FRIGATE_DIMS` went
   from `{ 56, 14, 6 }` to `{ 168, 42, 50 }` over multiple tuning rounds.
5. **TS is no longer canonical for this file.** The flat scene is a deliberate
   Rust-port direction the analysis doc didn't specify.

Three pre-flat drifts no longer apply:

- ~~`(pivot, rotation_rad)` vs SVG transform string~~ — `ShipSprite` deleted.
- ~~`[Point2; 4]` polygon arrays vs formatted strings~~ — `FacePoly` deleted.
- ~~rotation in radians, not degrees~~ — no rotation field returned.

One pre-flat drift survives:

- `bandBetweenCells` → `band_between_cells` snake_case rename. Function-body math
  is identical.

No open architectural items. The module is essentially complete; new rendering
primitives are more likely to land in `hud.rs` than here.

---

## Tests

7 inline tests at `perspective.rs:131–203`:

```
cell_to_screen_endpoints_match_lane_extents
cell_to_screen_midpoint_is_halfway
cell_to_screen_single_cell_lane_is_safe
fractional_cell_clamps_into_bounds
fractional_cell_intermediate_interpolates_linearly
cell_width_matches_lane_span_divided_by_n_minus_1
band_between_cells_matches_geometry_range_band
```

The last is the cross-module drift guard against `geometry::range_band` and
`perspective::band_between_cells` getting out of sync on bucket boundaries.

---

## Cross-references

- **Type vocabulary:** `RangeBand` from [`src/types.rs`](types.md); `range_band` from
  [`src/geometry.rs`](geometry.md).
- **Consumers:** the renderer subtree — [`src/hud.rs`](hud.md), `gfx.rs`, and
  `bin/broadside.rs`. `hud.rs` is the primary consumer (the view-angle morph lives
  there).
- **Domain terms:** *Range band*, *Lane*, *Bow-on / Broadside* in the
  [glossary](../GLOSSARY.md).
- **Design intent:** Tasks #55 / #57–#60 for the flat-scene direction.
