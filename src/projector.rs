//! 5×4 perspective projector — maps a grid [`Pos`] to a screen-space quad in
//! 480×270 frame space with Star-Wars-crawl foreshortening (blueprint lane task
//! D2).
//!
//! This is the v2 replacement for the 1-D flat-strip [`crate::perspective`]
//! (`cell: usize` → a point on a horizontal line). Where the old lane laid every
//! cell on one flat row, the v2 board is a 5-wide × 4-deep grid whose **rows
//! recede into the screen**: the near row (toward the player) sits low and large,
//! the far row (where enemies spawn) sits high and small, and the five columns
//! **fan out** — wide at the near row, converging toward a vanishing band as they
//! recede. The renderer (D3/D4) draws ships, threats, and the loft dest-quads at
//! the cells these quads describe.
//!
//! ## Why a pinhole `1/z` model
//!
//! Row spacing, per-cell scale, and the column fan are **all** derived from one
//! projection so they can never desync: each grid row is placed at a camera depth
//! `z`, the front row at `z_near` and the back row at `z_far` (`z_far > z_near`),
//! and everything on screen scales with `1/z`. A larger `z` (farther row) ⇒
//! smaller `1/z` ⇒ the row is drawn higher (nearer the horizon), narrower
//! (columns converge), and smaller ([`CellQuad::depth_scale`] shrinks). This is
//! true perspective foreshortening: equal-spaced grid rows **bunch up** toward the
//! horizon exactly as they would under a pinhole camera, with no separate ad-hoc
//! curve for each of the three effects.
//!
//! ## Coordinate frame (matches the rest of the renderer)
//!
//! Everything is in **480×270 virtual-pixel** space (`crate::gfx::VIRTUAL_W` ×
//! `VIRTUAL_H`), origin **top-left, y-down** — the convention every renderer
//! shader uses (`ndc_y = 1.0 - pixel.y * px_to_ndc.y`) and that
//! [`crate::background`] centers on `(W/2, H/2)`. A [`CellQuad`]'s four corners
//! are ordered **top-left, top-right, bottom-right, bottom-left** — the identical
//! corner order consumed by [`crate::gfx::PolygonInstance`],
//! `gfx::LoftShipInstance`, and the background `LayerUniform` — so `hud` (D3) can
//! feed a `CellQuad` straight into any of those without reshuffling.
//!
//! ## Grid frame (from [`crate::grid`])
//!
//! Per `grid`'s module docs: **`row 0` is the far/back row** (smaller, higher on
//! screen, where enemies spawn) and **`row ROWS-1` is the front row** (larger,
//! lower, nearest the player). `col` increases left → right. This module fixes
//! only screen placement; the combat model treats all rows as dodge space.
//!
//! ## Purity
//!
//! No GPU, no board, no feature gate beyond the module's own `cfg`. A
//! [`ProjectorConfig`] (the tunable look) plus [`grid::Pos`] in, a [`CellQuad`]
//! out — a pure function, unit-tested headless. The look is data on `Self`, not
//! magic constants buried in the math, so Bruce can iterate the perspective
//! without touching the projection logic.

use crate::grid::{Pos, COLS, ROWS};

/// A 2-D point in virtual-pixel space (origin top-left, y-down). Mirrors
/// [`crate::perspective::Point2`] so renderer code reads the same; kept local so
/// the v2 projector does not depend on the soon-replaced 1-D `perspective`
/// module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// The corner as the `[f32; 2]` pair the gfx instance structs take.
    pub const fn to_array(self) -> [f32; 2] {
        [self.x, self.y]
    }
}

/// The projected screen-space footprint of one grid cell.
///
/// `corners` are the cell's quad in virtual-pixel space, ordered **top-left,
/// top-right, bottom-right, bottom-left** (same as [`crate::gfx::PolygonInstance`]
/// / `LoftShipInstance` / the background `LayerUniform`). Because rows recede, a
/// cell quad is a **trapezoid** — its top edge (the far edge) is shorter than its
/// bottom edge (the near edge), and both edges sit at the row-boundary depths
/// that bracket the cell, so adjacent rows' quads tile without a gap.
///
/// `center` is the cell's center point (where a ship sprite / threat marker
/// pivots). `depth_scale` is the per-cell foreshortening factor the renderer
/// applies to sprite sizes and the loft dest-quad (D4) — see the field doc for
/// its exact numeric meaning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellQuad {
    /// `[top-left, top-right, bottom-right, bottom-left]` in virtual-pixel space.
    pub corners: [[f32; 2]; 4],
    /// The cell center in virtual-pixel space.
    pub center: [f32; 2],
    /// **Absolute** per-cell foreshortening multiplier — NOT a ratio against a
    /// reference row. `depth_scale = z_near / z(cell_center)`, i.e. `1/z`
    /// normalized so the **near plane** (`z = z_near`, `d = 0`) would read
    /// exactly `1.0`. It is sampled at the cell's **center** depth, so a real
    /// cell is always `< 1.0` (its center sits behind the near plane).
    ///
    /// Concretely, with the [`ProjectorConfig::default`] (`z_near = 1.0`,
    /// `z_far = 6.0`, `ROWS = 4` — the #62 deep chase-cam) it is, by row:
    ///
    /// | `row` | position      | `depth_scale` |
    /// |-------|---------------|---------------|
    /// | 3     | front / near  | ≈ **0.615**   |
    /// | 2     |               | ≈ 0.348       |
    /// | 1     |               | ≈ 0.242       |
    /// | 0     | back / far    | ≈ **0.186**   |
    ///
    /// (Grows monotonically with `row` — the grid.rs contract "the renderer's
    /// per-row depth scale grows with row".) D4 multiplies the loft dest-quad's
    /// on-screen size by this directly; the same absolute factor scales sprite
    /// half-sizes / HUD markers so a ship at the back row reads ~half the size
    /// of one at the front. The exact numbers move if Bruce retunes
    /// `z_near`/`z_far`; the *meaning* (absolute `z_near/z`, near-plane-anchored,
    /// center-sampled) is stable.
    pub depth_scale: f32,
}

impl CellQuad {
    /// The top-left corner.
    pub fn top_left(&self) -> Point2 {
        Point2::new(self.corners[0][0], self.corners[0][1])
    }
    /// The top-right corner.
    pub fn top_right(&self) -> Point2 {
        Point2::new(self.corners[1][0], self.corners[1][1])
    }
    /// The bottom-right corner.
    pub fn bottom_right(&self) -> Point2 {
        Point2::new(self.corners[2][0], self.corners[2][1])
    }
    /// The bottom-left corner.
    pub fn bottom_left(&self) -> Point2 {
        Point2::new(self.corners[3][0], self.corners[3][1])
    }
    /// The cell center point.
    pub fn center_point(&self) -> Point2 {
        Point2::new(self.center[0], self.center[1])
    }
    /// Width of the cell's near (bottom) edge in virtual pixels.
    pub fn near_edge_width(&self) -> f32 {
        self.corners[2][0] - self.corners[3][0]
    }
    /// Width of the cell's far (top) edge in virtual pixels.
    pub fn far_edge_width(&self) -> f32 {
        self.corners[1][0] - self.corners[0][0]
    }
}

/// The tunable perspective look. Distances are in virtual pixels (480×270 frame
/// space, origin top-left, y-down); `z_*` are unitless camera depths. Defaults
/// are tuned for the 480×270 canvas with a comfortable margin so corner ships do
/// not overhang the frame edge; Bruce iterates these to taste without touching
/// the projection math.
#[derive(Debug, Clone, Copy)]
pub struct ProjectorConfig {
    /// Frame width in virtual pixels (the board is centered horizontally on
    /// `frame_w / 2`).
    pub frame_w: f32,
    /// Frame height in virtual pixels.
    pub frame_h: f32,

    /// Camera depth of the **near** row plane (`row ROWS-1`). The smaller of the
    /// two; everything scales by `1/z`, so the near row is the largest /
    /// lowest / widest.
    pub z_near: f32,
    /// Camera depth of the **far** row plane (`row 0`). `> z_near`; the larger
    /// depth ⇒ smaller / higher / narrower.
    pub z_far: f32,

    /// Screen-y of the **horizon** (the `1/z → 0` vanishing line), in virtual
    /// pixels from the top. Rows are placed *below* this line; as a row recedes
    /// its center rises toward (but never reaches) `horizon_y`.
    pub horizon_y: f32,
    /// Screen-y of the **near row center** (`row ROWS-1`), in virtual pixels. The
    /// lowest row on screen. Row centers interpolate between here and `horizon_y`
    /// by `1/z`, so the vertical gap between rows shrinks with depth.
    pub near_row_y: f32,

    /// Half the lateral span (frame-center to a column-fan edge) at **depth
    /// `z = 1`** — i.e. the fan's "world" half-width before the `1/z`
    /// perspective divide. The on-screen half-width at a given row is this times
    /// `1/z` for that row, so columns fan wide near and converge far. Sized so
    /// the near row's full fan fits inside `frame_w` with margin.
    pub fan_half_width: f32,

    /// Number of grid columns (mirrors [`crate::grid::COLS`]; carried on the
    /// config so the projector stays a pure function of its inputs and is
    /// testable with a degenerate grid).
    pub cols: usize,
    /// Number of grid rows (mirrors [`crate::grid::ROWS`]).
    pub rows: usize,
}

impl Default for ProjectorConfig {
    fn default() -> Self {
        // Tuned for 480×270. The board occupies the lower ~70% of the frame; the
        // top ~30% is headroom for the parallax backdrop, the far nebula, the
        // range-band ruler, and the enemy telegraph icons that float above the
        // back row.
        Self {
            frame_w: crate::gfx::VIRTUAL_W as f32,
            frame_h: crate::gfx::VIRTUAL_H as f32,
            // (#62) Match Bruce's art-tool chase-cam reference: a LOW camera where
            // the lanes recede like a road to a vanishing point near mid-screen
            // (his tool reads ~20° pitch). Deep recession — z_far/z_near = 6.0 ⇒
            // the back row draws at ~17% the near-row size, so the five lanes
            // converge tightly toward the horizon instead of staying a shallow
            // top-down board (was 2.4 = a flat ~42% overhead board).
            z_near: 1.0,
            z_far: 6.0,
            // Horizon near mid-screen (was 70, high): the vanishing point sits at
            // ~45% down like the reference, leaving the top ~half for the
            // starfield/nebula backdrop. Near row pinned to the VERY BOTTOM edge so
            // the road dominates the lower screen and the front lane (player) is
            // large + low, road-style (lead pass-2: was 252, still sat a touch high).
            horizon_y: 120.0,
            near_row_y: 268.0,
            // Near-row fan WIDE (lead pass-2: the road was a small central trapezoid
            // with empty starfield in the lower corners; the ref fans the near lanes
            // out toward the bottom corners and fills the lower ~2/3). 290 px each
            // side = a 580 px near row that spills past the 480 frame edges, so the
            // outer lanes run off the bottom corners like the reference road. The
            // deep z_far still converges the far rows to a tight central band.
            fan_half_width: 290.0,
            cols: COLS,
            rows: ROWS,
        }
    }
}

impl ProjectorConfig {
    /// The frame-center x the board's column fan is symmetric about.
    fn center_x(&self) -> f32 {
        self.frame_w * 0.5
    }

    /// Camera depth `z` for a row **boundary** at depth parameter `d ∈ [0, 1]`,
    /// where `d = 0` is the near plane (`z_near`) and `d = 1` is the far plane
    /// (`z_far`). Linear in `d`; the perspective foreshortening comes from the
    /// later `1/z` divide, not from a curved `z(d)`.
    fn z_at(&self, d: f32) -> f32 {
        self.z_near + (self.z_far - self.z_near) * d
    }

    /// The depth parameter `d ∈ [0, 1]` of a row **boundary** line. There are
    /// `rows + 1` boundaries (the edges between rows, plus the outer near/far
    /// edges); boundary `b` runs `0..=rows`. `b = 0` is the far edge of the
    /// back row (`d = 1`), `b = rows` is the near edge of the front row
    /// (`d = 0`) — because `row 0` is far and `row rows-1` is near, higher `b`
    /// (counted from the far edge) means nearer.
    ///
    /// Spreading boundaries **evenly in `d`** (then dividing by `1/z`) is what
    /// makes the rows bunch toward the horizon: equal world-depth steps project
    /// to shrinking screen steps.
    fn boundary_d(&self, b: usize) -> f32 {
        // b = 0 (far edge) -> d = 1 ; b = rows (near edge) -> d = 0.
        let rows = self.rows.max(1) as f32;
        1.0 - (b as f32) / rows
    }

    /// Screen-y for a given `1/z`. The horizon is `inv_z = 0` (`z → ∞`); the near
    /// plane is `inv_z = 1/z_near`. Linear in `inv_z` between the horizon and the
    /// near row, so a row's screen-y is a true perspective function of its depth.
    fn screen_y(&self, inv_z: f32) -> f32 {
        let inv_z_near = 1.0 / self.z_near;
        let t = inv_z / inv_z_near; // 0 at horizon, 1 at the near plane
        self.horizon_y + (self.near_row_y - self.horizon_y) * t
    }

    /// Half the on-screen lateral span of the column fan at a given `1/z`. Wide
    /// near, converging far — the column-convergence half of the projection.
    fn half_span(&self, inv_z: f32) -> f32 {
        self.fan_half_width * inv_z
    }

    /// Screen x of a column **boundary** `c` (`0..=cols`) at a given `1/z`. The
    /// fan is centered on [`center_x`]: boundary `0` is the left fan edge,
    /// boundary `cols` the right edge, evenly spaced across the span at this
    /// depth.
    fn boundary_x(&self, c: usize, inv_z: f32) -> f32 {
        let cols = self.cols.max(1) as f32;
        let span = self.half_span(inv_z) * 2.0;
        let left = self.center_x() - span * 0.5;
        left + span * (c as f32) / cols
    }
}

/// Project a grid [`Pos`] to its screen-space [`CellQuad`] under `cfg`. The pure
/// core of the renderer's spatial mapping (blueprint D2). Out-of-bounds positions
/// are still projected (the math is total) — callers that only ever pass in-bounds
/// cells, which is every real board cell, need no guard.
///
/// The quad is the trapezoid bounded by the cell's two row-boundary depths (its
/// far/near edges) and its two column-boundary lines at each of those depths, so
/// neighbouring cells share edges exactly (no seams, no overlap). `depth_scale`
/// is taken at the cell's **center** depth.
pub fn grid_cell_quad(pos: Pos, cfg: &ProjectorConfig) -> CellQuad {
    // Row `pos.row` is bracketed by boundaries `b_far = pos.row` (its far edge)
    // and `b_near = pos.row + 1` (its near edge), counted from the far edge.
    let d_far = cfg.boundary_d(pos.row);
    let d_near = cfg.boundary_d(pos.row + 1);
    let inv_z_far = 1.0 / cfg.z_at(d_far);
    let inv_z_near = 1.0 / cfg.z_at(d_near);

    let y_far = cfg.screen_y(inv_z_far); // top edge (farther = higher)
    let y_near = cfg.screen_y(inv_z_near); // bottom edge (nearer = lower)

    // Column `pos.col` spans column boundaries `pos.col`..`pos.col + 1`, taken at
    // each of the two row depths so the trapezoid sides converge with the fan.
    let x_far_l = cfg.boundary_x(pos.col, inv_z_far);
    let x_far_r = cfg.boundary_x(pos.col + 1, inv_z_far);
    let x_near_l = cfg.boundary_x(pos.col, inv_z_near);
    let x_near_r = cfg.boundary_x(pos.col + 1, inv_z_near);

    // Corner order: top-left, top-right, bottom-right, bottom-left (gfx order).
    // "Top" = the far edge (higher on screen, smaller y), "bottom" = near edge.
    let corners = [
        [x_far_l, y_far],   // top-left (far-left)
        [x_far_r, y_far],   // top-right (far-right)
        [x_near_r, y_near], // bottom-right (near-right)
        [x_near_l, y_near], // bottom-left (near-left)
    ];

    // Center: midpoint depth of the cell for scale + a true quad centroid for the
    // pivot (the trapezoid's centroid in x is the mean of the four corners' x).
    let d_mid = 0.5 * (d_far + d_near);
    let inv_z_mid = 1.0 / cfg.z_at(d_mid);
    let cx = 0.25 * (x_far_l + x_far_r + x_near_l + x_near_r);
    let cy = cfg.screen_y(inv_z_mid);

    // depth_scale: on-screen size scales with 1/z, normalized to 1.0 at the near
    // plane (the largest cells), so the nearest row is `1.0` and farther rows are
    // proportionally smaller — the factor the renderer multiplies sprite sizes /
    // loft dest-quads by (D4).
    let depth_scale = inv_z_mid * cfg.z_near;

    CellQuad {
        corners,
        center: [cx, cy],
        depth_scale,
    }
}

/// Convenience: project every in-bounds [`Pos`] to its [`CellQuad`], in flat
/// row-major order (the same order as [`crate::grid::all_positions`]). Handy for
/// the renderer's per-frame cell pass and for tests.
pub fn project_all(cfg: &ProjectorConfig) -> Vec<CellQuad> {
    crate::grid::all_positions()
        .into_iter()
        .map(|p| grid_cell_quad(p, cfg))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Pos;

    fn cfg() -> ProjectorConfig {
        ProjectorConfig::default()
    }

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    /// The far row (`row 0`) sits HIGHER on screen (smaller y) than the near row
    /// (`row ROWS-1`) — the core Star-Wars-crawl recession.
    #[test]
    fn far_row_is_higher_than_near_row() {
        let c = cfg();
        let far = grid_cell_quad(Pos::new(2, 0), &c);
        let near = grid_cell_quad(Pos::new(2, ROWS - 1), &c);
        assert!(
            far.center[1] < near.center[1],
            "far row center y {} should be above (less than) near row center y {}",
            far.center[1],
            near.center[1]
        );
    }

    /// The far row is drawn SMALLER (depth_scale shrinks with recession). The
    /// scale is normalized so the **near plane** (`d = 0`) is `1.0`; each cell's
    /// `depth_scale` is taken at its CENTER depth, so even the front row reads a
    /// touch under 1.0 (its center is set back from the near plane) — but it is
    /// the largest of all rows and stays close to 1.0.
    #[test]
    fn depth_scale_shrinks_with_distance_and_near_is_largest() {
        let c = cfg();
        let mut last = f32::INFINITY;
        for row in 0..ROWS {
            // Iterate near -> far (row ROWS-1 down to 0): depth_scale must
            // strictly decrease.
            let r = ROWS - 1 - row;
            let q = grid_cell_quad(Pos::new(2, r), &c);
            assert!(
                q.depth_scale < last,
                "depth_scale should shrink toward the far row (row {r} got {})",
                q.depth_scale
            );
            assert!(
                q.depth_scale > 0.0 && q.depth_scale <= 1.0,
                "depth_scale stays in (0, 1] (near-plane normalized), row {r} got {}",
                q.depth_scale
            );
            last = q.depth_scale;
        }
        // The nearest row is the largest cell, in (0, 1] (its center sits one
        // half-row-depth behind the near plane). The exact value tracks the
        // perspective depth: with the #62 deep chase-cam (z_far/z_near = 6.0) the
        // near-row centre is ~0.62; it was ~0.85 under the old shallow 2.4 board.
        // Only the relative invariant (largest, < 1.0) is fixed — not a magic bound.
        let near = grid_cell_quad(Pos::new(2, ROWS - 1), &c);
        assert!(
            near.depth_scale > 0.5 && near.depth_scale <= 1.0,
            "near row depth_scale should be the largest, in (0.5, 1.0], got {}",
            near.depth_scale
        );
    }

    /// Columns fan: the far row's lateral extent is NARROWER than the near row's
    /// (perspective convergence). Compare full-row widths edge to edge.
    #[test]
    fn columns_converge_with_distance() {
        let c = cfg();
        let far_l = grid_cell_quad(Pos::new(0, 0), &c);
        let far_r = grid_cell_quad(Pos::new(COLS - 1, 0), &c);
        let near_l = grid_cell_quad(Pos::new(0, ROWS - 1), &c);
        let near_r = grid_cell_quad(Pos::new(COLS - 1, ROWS - 1), &c);
        let far_width = far_r.center[0] - far_l.center[0];
        let near_width = near_r.center[0] - near_l.center[0];
        assert!(
            far_width < near_width,
            "far row width {far_width} should be narrower than near row width {near_width}"
        );
        assert!(far_width > 0.0, "far row must still have positive width");
    }

    /// Within a row, columns increase left → right and are symmetric about the
    /// frame center (col 0 left of center, last col right of center, the middle
    /// column on center for an odd column count).
    #[test]
    fn columns_increase_left_to_right_and_center_is_symmetric() {
        let c = cfg();
        let center_x = c.frame_w * 0.5;
        for row in 0..ROWS {
            let mut last_x = f32::NEG_INFINITY;
            for col in 0..COLS {
                let q = grid_cell_quad(Pos::new(col, row), &c);
                assert!(
                    q.center[0] > last_x,
                    "col {col} row {row} x {} should exceed previous {last_x}",
                    q.center[0]
                );
                last_x = q.center[0];
            }
            // COLS is odd (5): the middle column's center sits on the frame center.
            let mid = grid_cell_quad(Pos::new(COLS / 2, row), &c);
            assert!(
                approx(mid.center[0], center_x, 1e-3),
                "middle column row {row} x {} should be frame-center {center_x}",
                mid.center[0]
            );
            // Symmetry: col 0 and last col are mirror distances from center.
            let l = grid_cell_quad(Pos::new(0, row), &c);
            let r = grid_cell_quad(Pos::new(COLS - 1, row), &c);
            assert!(
                approx(center_x - l.center[0], r.center[0] - center_x, 1e-3),
                "row {row} should be left/right symmetric about center"
            );
        }
    }

    /// Adjacent rows tile without a gap: a cell's near (bottom) edge coincides
    /// with the next-nearer row's far (top) edge — same y and same x's. This is
    /// the "share boundaries" invariant that keeps the grid seamless.
    #[test]
    fn adjacent_rows_share_an_edge() {
        let c = cfg();
        for col in 0..COLS {
            for row in 0..(ROWS - 1) {
                // `row` is farther than `row + 1` (row 0 = far). The far cell's
                // NEAR edge (bottom) should meet the nearer cell's FAR edge (top).
                let far_cell = grid_cell_quad(Pos::new(col, row), &c);
                let near_cell = grid_cell_quad(Pos::new(col, row + 1), &c);
                // far_cell bottom edge = corners[2],[3] (bot-right, bot-left).
                // near_cell top edge   = corners[1],[0] (top-right, top-left).
                assert!(
                    approx(far_cell.corners[3][1], near_cell.corners[0][1], 1e-3),
                    "row {row}/{} bottom-y should meet row {}/{} top-y",
                    col,
                    row + 1,
                    col
                );
                assert!(
                    approx(far_cell.corners[3][0], near_cell.corners[0][0], 1e-3),
                    "shared left x mismatch at col {col} rows {row}/{}",
                    row + 1
                );
                assert!(
                    approx(far_cell.corners[2][0], near_cell.corners[1][0], 1e-3),
                    "shared right x mismatch at col {col} rows {row}/{}",
                    row + 1
                );
            }
        }
    }

    /// Adjacent columns in a row share a vertical boundary edge (no lateral gap
    /// or overlap): cell `col`'s right edge x equals cell `col+1`'s left edge x,
    /// at both the far and near depths.
    #[test]
    fn adjacent_columns_share_an_edge() {
        let c = cfg();
        for row in 0..ROWS {
            for col in 0..(COLS - 1) {
                let left = grid_cell_quad(Pos::new(col, row), &c);
                let right = grid_cell_quad(Pos::new(col + 1, row), &c);
                // left top-right == right top-left (far edge); left bot-right ==
                // right bot-left (near edge).
                assert!(
                    approx(left.corners[1][0], right.corners[0][0], 1e-3),
                    "far-edge seam at row {row} cols {col}/{}",
                    col + 1
                );
                assert!(
                    approx(left.corners[2][0], right.corners[3][0], 1e-3),
                    "near-edge seam at row {row} cols {col}/{}",
                    col + 1
                );
            }
        }
    }

    /// A cell is a proper trapezoid that narrows with depth: its top (far) edge
    /// is shorter than its bottom (near) edge, and the top edge is above the
    /// bottom edge.
    #[test]
    fn cell_quad_is_a_receding_trapezoid() {
        let c = cfg();
        let q = grid_cell_quad(Pos::new(1, 1), &c);
        let top_w = q.far_edge_width();
        let bot_w = q.near_edge_width();
        assert!(top_w > 0.0 && bot_w > 0.0, "both edges positive width");
        assert!(
            top_w < bot_w,
            "far (top) edge {top_w} should be narrower than near (bottom) edge {bot_w}"
        );
        // Top edge y < bottom edge y (higher on screen).
        assert!(q.corners[0][1] < q.corners[3][1], "top above bottom");
    }

    /// VERTICAL bounds hold for every cell (no row spills above the top or below
    /// the bottom). HORIZONTAL is deliberately NOT bounded for the near rows: the
    /// #62 chase-cam fans the near lanes WIDE so the outer lanes run off the
    /// bottom corners (the reference "road" filling the lower screen), so near-row
    /// corners legitimately exceed [0, frame_w]. The FAR row (row 0, where enemies
    /// read) must still fit horizontally so their silhouettes aren't clipped.
    #[test]
    fn default_board_fits_inside_frame() {
        let c = cfg();
        for q in project_all(&c) {
            for corner in q.corners {
                assert!(
                    corner[1] >= 0.0 && corner[1] <= c.frame_h,
                    "corner y {} out of [0,{}]",
                    corner[1],
                    c.frame_h
                );
            }
        }
        // Far row (row 0) fits horizontally with margin — enemies read un-clipped.
        for col in 0..COLS {
            let q = grid_cell_quad(Pos::new(col, 0), &c);
            for corner in q.corners {
                assert!(
                    corner[0] >= 0.0 && corner[0] <= c.frame_w,
                    "far-row corner x {} out of [0,{}]",
                    corner[0],
                    c.frame_w
                );
            }
        }
    }

    /// The center cell's center x is the frame center and its center y lies
    /// between the horizon and the near row — a sanity check on the frame anchor.
    #[test]
    fn center_cell_is_centered_horizontally() {
        let c = cfg();
        // For an even ROWS (4) there is no single center row; check a mid cell.
        let q = grid_cell_quad(Pos::new(COLS / 2, ROWS / 2), &c);
        assert!(approx(q.center[0], c.frame_w * 0.5, 1e-3));
        assert!(q.center[1] > c.horizon_y && q.center[1] < c.near_row_y);
    }

    /// `project_all` returns one quad per cell in `grid::all_positions` order.
    #[test]
    fn project_all_matches_grid_order() {
        let c = cfg();
        let all = project_all(&c);
        assert_eq!(all.len(), crate::grid::CELLS);
        for (i, (p, q)) in crate::grid::all_positions()
            .into_iter()
            .zip(all.iter())
            .enumerate()
        {
            let direct = grid_cell_quad(p, &c);
            assert_eq!(direct, *q, "mismatch at flat index {i}");
        }
    }

    /// `Point2::to_array` round-trips a corner so hud can pass it straight to a
    /// gfx instance struct.
    #[test]
    fn point2_to_array_roundtrips() {
        let p = Point2::new(12.5, -3.0);
        assert_eq!(p.to_array(), [12.5, -3.0]);
    }
}
