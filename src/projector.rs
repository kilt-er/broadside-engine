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
    pub const fn top_left(&self) -> Point2 {
        Point2::new(self.corners[0][0], self.corners[0][1])
    }
    /// The top-right corner.
    pub const fn top_right(&self) -> Point2 {
        Point2::new(self.corners[1][0], self.corners[1][1])
    }
    /// The bottom-right corner.
    pub const fn bottom_right(&self) -> Point2 {
        Point2::new(self.corners[2][0], self.corners[2][1])
    }
    /// The bottom-left corner.
    pub const fn bottom_left(&self) -> Point2 {
        Point2::new(self.corners[3][0], self.corners[3][1])
    }
    /// The cell center point.
    pub const fn center_point(&self) -> Point2 {
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

    /// (#140 Bruce) STRETCH-mode blend `t ∈ [0, 1]`. `0` = the pure-perspective
    /// chase-cam (every existing path + the pixel-identity gate depend on this being
    /// the default, so `for_scene` sets it `0.0` and `grid_cell_quad` is byte-
    /// identical at `0`). `> 0` LERPS each cell's corners + center + `depth_scale`
    /// toward a UNIFORM TOP-DOWN grid (rows equal-height stacked up from the near
    /// edge, columns equal-width = the near-row width, `depth_scale` → 1.0). So cell
    /// (=> ship) size stays ~constant as the grid STRETCHES vertically toward a
    /// square top-down board — Bruce's "stretch mode" that kills the constant-
    /// footprint ballooning. Set via [`Self::with_stretch`]. Independent of
    /// `with_pitch` (the OFF/constant-footprint mode); the bin picks one per its
    /// STRETCH toggle.
    pub stretch_t: f32,

    /// (#142 Bruce) STRETCH-STRAIGHT variant. When `false` (the #140 default) the
    /// stretch blend LERPs each perspective corner toward its uniform corner, which
    /// leaves the column edges BOWED through the mid-arc (Bruce: "ok, but I expected
    /// straight lines"). When `true` the column edges are STRAIGHTENED as the arc
    /// raises — each column's far (top) edge x chases its near (bottom) edge x so the
    /// whole column verticalizes, reaching perfectly straight columns + rows at full
    /// stretch. Still byte-identical at `stretch_t == 0` (the blend block is skipped),
    /// so the step-0 no-regression gate holds in this mode too. Set via
    /// [`Self::with_stretch_straight`]. Only meaningful when `stretch_t > 0`.
    pub stretch_straight: bool,

    /// (#151 Bruce) CONTINUOUS-STRAIGHT depth lines. When `true`, each depth line
    /// (front-to-back column boundary) is ONE straight line from the grid's near
    /// (front) edge to its far (back) edge — every cell corner on that boundary lies
    /// on the line, so there are NO per-cell kinks at row boundaries (the "stepped
    /// per quadrant" look of [`Self::stretch_straight`]). Implies the stretch; set via
    /// [`Self::with_stretch_continuous`]. Byte-identical at `stretch_t == 0` (block
    /// skipped). Takes precedence over `stretch_straight` when both are set.
    pub stretch_lines_continuous: bool,

    /// (UNIFY, Bruce order) When `true`, [`grid_cell_quad`] projects each cell's
    /// ground-plane world corners through the UNIFIED real-perspective camera
    /// ([`unified_view_proj`]) instead of the hand-tuned `1/z` fan — the SAME camera
    /// the 3-D hulls render through, so ships LIVE in the grid (nose→VP + per-column
    /// outward lean fall out by construction) rather than being flat sprites pasted
    /// on a separate projection. A SANE FOV ([`UNIFIED_FOV_Y_DEG`]) so a real hull at
    /// a cell doesn't wrap the camera (the `1/z` fan's ~178° spread made that
    /// impossible — #73). `false` (default) keeps the legacy fan byte-identical, so
    /// every existing path + test is untouched until this is proven and made default.
    /// Set via [`Self::with_unified`]; `pitch_t` (the `G` arc) still drives the
    /// camera pitch in unified mode.
    pub unified: bool,

    /// (UNIFY) The grid-pitch arc `t ∈ [0,1]` (the `G` key), carried so the unified
    /// camera can read it for its look-down angle. In the legacy fan path the pitch
    /// is baked into `horizon_y`/`z_far` via [`Self::with_pitch`]; the unified camera
    /// needs the raw `t` to lerp its real pitch, so it rides here. `0` at boot-step 0.
    pub pitch_t: f32,
}

impl Default for ProjectorConfig {
    /// The 480×270 default look — `for_scene(VIRTUAL_W, VIRTUAL_H)`, so the
    /// hardcoded tuning below lives in ONE place and the default is byte-identical
    /// to a `for_scene` call at the virtual canvas size.
    fn default() -> Self {
        Self::for_scene(crate::gfx::VIRTUAL_W as f32, crate::gfx::VIRTUAL_H as f32)
    }
}

impl ProjectorConfig {
    /// (#76 scene-res) The default look SCALED to a `frame_w × frame_h` canvas, so
    /// a live scene-resolution change ([`crate::gfx::cycle_scene_res`]) reprojects
    /// the SAME scene at a different pixel count instead of leaving the board
    /// pinned to 480×270 coordinates in a resized offscreen. The vertical anchors
    /// (`horizon_y`, `near_row_y`) scale by `h / 270` and the lateral fan
    /// (`fan_half_width`) by `w / 480`, so every proportion — horizon fraction,
    /// near-row-above-HUD clearance, lane spread — is preserved across presets.
    /// `for_scene(VIRTUAL_W, VIRTUAL_H)` reproduces [`Self::default`] exactly
    /// (the scale factors are both 1.0), which the pixel-identity gate relies on.
    pub fn for_scene(frame_w: f32, frame_h: f32) -> Self {
        // Tuned for 480×270 (the reference canvas); the sx/sy factors carry the
        // look to any preset. The board occupies the lower ~70% of the frame; the
        // top ~30% is headroom for the parallax backdrop, the far nebula, the
        // range-band ruler, and the enemy telegraph icons that float above the
        // back row.
        let sx = frame_w / crate::gfx::VIRTUAL_W as f32;
        let sy = frame_h / crate::gfx::VIRTUAL_H as f32;
        Self {
            frame_w,
            frame_h,
            // (#62) Match Bruce's art-tool chase-cam reference: a LOW camera where
            // the lanes recede like a road to a vanishing point near mid-screen
            // (his tool reads ~20° pitch). Deep recession — z_far/z_near = 6.0 ⇒
            // the back row draws at ~17% the near-row size, so the five lanes
            // converge tightly toward the horizon instead of staying a shallow
            // top-down board (was 2.4 = a flat ~42% overhead board). z_* are
            // unitless depths — unaffected by the pixel canvas size.
            z_near: 1.0,
            z_far: 6.0,
            // Horizon near mid-screen (was 70, high): the vanishing point sits at
            // ~45% down like the reference, leaving the top ~half for the
            // starfield/nebula backdrop. Near row pinned JUST ABOVE the bottom HUD
            // band (band = frame_h-40 = y230..270): the road must end above the
            // status area, NOT run behind it (#64, Bruce live: the board + hero ship
            // were cut off by the gray band). 228 = near edge just clears the band.
            // Scaled by sy so the same FRACTIONS hold at any vertical resolution.
            horizon_y: 120.0 * sy,
            near_row_y: 226.0 * sy,
            // Near-row fan WIDE (lead pass-2: the road was a small central trapezoid
            // with empty starfield in the lower corners; the ref fans the near lanes
            // out toward the bottom corners and fills the lower ~2/3). 290 px each
            // side = a 580 px near row that spills past the 480 frame edges, so the
            // outer lanes run off the bottom corners like the reference road. The
            // deep z_far still converges the far rows to a tight central band. Scaled
            // by sx so the lane spread tracks the horizontal resolution.
            fan_half_width: 290.0 * sx,
            cols: COLS,
            rows: ROWS,
            stretch_t: 0.0, // (#140) pure perspective by default — byte-identical to today
            stretch_straight: false, // (#142) curved stretch by default
            stretch_lines_continuous: false, // (#151) stepped (per-cell) by default
            unified: false, // (UNIFY) legacy fan by default — zero regression
            pitch_t: 0.0,   // (UNIFY) boot grid-pitch step 0
        }
    }
}

impl ProjectorConfig {
    /// (#139 Bruce) Re-pitch the camera toward TOP-DOWN by `t` ∈ [0, 1] while
    /// keeping the grid's apparent front-to-back DEPTH CONSTANT (Bruce: "grid depth
    /// should remain constant rather than getting stretched as you raise the
    /// horizon"). `t = 0` is the current chase-cam look; `t = 1` is near-overhead.
    ///
    /// HOW depth stays constant. A row's screen-y is
    /// `horizon_y + (near_row_y - horizon_y) * (z_near / z)`, so the grid's screen
    /// footprint is `near_row_y` (front, fixed) down to the back-row y
    /// `y_back = near_row_y - (near_row_y - horizon_y) * (1 - z_near/z_far)`.
    /// Raising the pitch = flattening the perspective = pushing the depth ratio
    /// `r = z_near/z_far` toward 1 (rows spread evenly, columns stop converging =
    /// overhead). That alone would SHRINK the footprint `(1 - r)` (the "stretch"
    /// Bruce saw is the inverse — a naive horizon raise changes it per step). We
    /// COMPENSATE: pin `near_row_y`, hold the target depth `D` (the t=0 footprint)
    /// fixed, and solve `horizon_y = near_row_y - D / (1 - r)` for each `r`. Then
    /// `y_back = near_row_y - D` for ALL t — the grid's top + bottom screen edges
    /// don't move; only the INTERNAL row spacing + column convergence change, so it
    /// reads as the SAME grid seen from a steeper angle. `z_near` + `near_row_y` +
    /// `fan_half_width` are untouched (so the near-row size is identical), proving
    /// the projector is cleanly factored — depth holds by adjusting one ratio +
    /// the compensating horizon, no baked constant to fight.
    pub fn with_pitch(self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        // Base (t=0) depth ratio and the footprint depth D it produces — the
        // invariant to preserve.
        let r0 = self.z_near / self.z_far;
        let d_base = (self.near_row_y - self.horizon_y) * (1.0 - r0);
        // Steepen toward overhead: r -> ~0.9 (near-flat). Capped below 1.0 so
        // 1/(1-r) stays finite (a true r=1 is a pure top-down with no recession).
        let r = r0 + (0.9 - r0) * t;
        let z_far = self.z_near / r;
        // Compensate the horizon so the back-row screen-y (hence the grid depth)
        // is unchanged: y_back = near_row_y - d_base for every t.
        let horizon_y = self.near_row_y - d_base / (1.0 - r);
        Self {
            z_far,
            horizon_y,
            ..self
        }
    }

    /// (UNIFY, Bruce order) Switch [`grid_cell_quad`] to the UNIFIED real-perspective
    /// camera ([`unified_view_proj`]) — the grid AND the 3-D hulls then share ONE
    /// camera/coordinate system, so ships live in the grid (nose→VP + per-column lean
    /// by construction). `t` is the grid-pitch arc (the `G` key) driving the camera's
    /// look-down. The bin selects this mode in place of the stretch/pitch fan modes.
    pub const fn with_unified(self, t: f32) -> Self {
        Self {
            unified: true,
            pitch_t: t,
            ..self
        }
    }

    /// (#199b / #213 item 4) Override the projector's [`Self::cols`] + [`Self::rows`]
    /// for variable-board encounters. `for_scene` defaults to [`crate::grid::COLS`] x
    /// [`crate::grid::ROWS`] (5x4) so every existing call site stays byte-identical;
    /// the bin chains `.with_dims(self.board.dims().cols, self.board.dims().rows)`
    /// per frame so the playable grid wireframe + every projector-derived overlay
    /// (cell quads, ship cell projection, fire beams, kill bursts) lay out at the
    /// live board's dims. The preview path also uses the same dims-aware cell math
    /// via `cell_world_corners_offset_dims`, so the upcoming-board preview at depth
    /// and the playable board it becomes share ONE projection contract.
    pub const fn with_dims(self, cols: usize, rows: usize) -> Self {
        Self { cols, rows, ..self }
    }

    /// (#140 Bruce) STRETCH mode: set the [`Self::stretch_t`] blend toward a uniform
    /// top-down grid. `t = 0` = pure perspective (unchanged); `t = 1` = a uniform
    /// square top-down board with ~constant cell (=> ship) size. The bin steps `t`
    /// with the pitch arc when STRETCH is ON. `with_stretch(0.0)` == self, so step 0
    /// is byte-identical (the no-regression invariant).
    pub const fn with_stretch(self, t: f32) -> Self {
        Self {
            stretch_t: t.clamp(0.0, 1.0),
            ..self
        }
    }

    /// (#142 Bruce) STRETCH-STRAIGHT: the same vertical stretch as [`Self::with_stretch`]
    /// but with the column edges STRAIGHTENED across the arc (no bow) — the third grid
    /// mode Bruce asked for ("I expected straight lines"). `t = 0` == self (byte-
    /// identical step 0); `t = 1` = a uniform straight top-down square. Sets both
    /// `stretch_t` and the `stretch_straight` flag the [`grid_cell_quad`] blend reads.
    pub const fn with_stretch_straight(self, t: f32) -> Self {
        Self {
            stretch_t: t.clamp(0.0, 1.0),
            stretch_straight: true,
            ..self
        }
    }

    /// (#151 Bruce) STRETCH with CONTINUOUS straight depth lines: same vertical stretch,
    /// but each front-to-back column boundary is ONE straight line (no per-cell kinks =
    /// the "stepped per quadrant" look of [`Self::with_stretch_straight`]). `t = 0` ==
    /// self (byte-identical step 0). Sets `stretch_t` + `stretch_lines_continuous`.
    pub const fn with_stretch_continuous(self, t: f32) -> Self {
        Self {
            stretch_t: t.clamp(0.0, 1.0),
            stretch_lines_continuous: true,
            ..self
        }
    }

    /// (#140) The UNIFORM top-down screen-y of row boundary `b` (`0..=rows`, far→near)
    /// and the uniform half-span (constant near-row width). At full stretch the grid
    /// is rows of EQUAL height stacked UP from the near edge (`near_row_y`, fixed),
    /// each cell ~the near cell's height — so cell size holds + the board stretches
    /// vertically toward a square. Columns are parallel at the near-row width.
    fn uniform_boundary_y(&self, b: usize) -> f32 {
        let rows = self.rows.max(1) as f32;
        // Near cell HEIGHT in the perspective view (the size we hold constant): the
        // near row's near-edge y minus its far-edge y.
        let near_cell_h = self.screen_y(1.0 / self.z_near)
            - self.screen_y(1.0 / self.z_at(self.boundary_d(self.rows - 1)));
        let total_h = rows * near_cell_h;
        // Fraction from the near edge: b = rows (near) -> 0, b = 0 (far) -> 1.
        let f = (rows - b as f32) / rows;
        self.near_row_y - f * total_h
    }

    /// (#140) Uniform (parallel) column half-span = the near-row perspective half-
    /// span, so cell WIDTH holds across rows (no fan convergence) in stretch mode.
    fn uniform_half_span(&self) -> f32 {
        self.half_span(1.0 / self.z_near)
    }

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
    // (UNIFY) When the unified camera is active, the cell quad IS the perspective
    // projection of the cell's ground-plane world corners — the same camera the
    // 3-D hulls render through, so grid + ships share one coordinate system.
    if cfg.unified {
        return unified_cell_quad(pos, cfg);
    }
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
    let mut corners = [
        [x_far_l, y_far],   // top-left (far-left)
        [x_far_r, y_far],   // top-right (far-right)
        [x_near_r, y_near], // bottom-right (near-right)
        [x_near_l, y_near], // bottom-left (near-left)
    ];

    // Center: midpoint depth of the cell for scale + a true quad centroid for the
    // pivot (the trapezoid's centroid in x is the mean of the four corners' x).
    let d_mid = 0.5 * (d_far + d_near);
    let inv_z_mid = 1.0 / cfg.z_at(d_mid);
    let mut cx = 0.25 * (x_far_l + x_far_r + x_near_l + x_near_r);
    let mut cy = cfg.screen_y(inv_z_mid);

    // depth_scale: on-screen size scales with 1/z, normalized to 1.0 at the near
    // plane (the largest cells), so the nearest row is `1.0` and farther rows are
    // proportionally smaller — the factor the renderer multiplies sprite sizes /
    // loft dest-quads by (D4).
    let mut depth_scale = inv_z_mid * cfg.z_near;

    // (#140) STRETCH blend toward the uniform top-down grid. At stretch_t == 0 this
    // whole block is skipped, so the perspective output is byte-identical (the
    // no-regression gate). At t > 0 each corner/center/scale LERPS to its uniform
    // value: rows equal-height stacked up from the near edge, columns parallel at
    // the near width, depth_scale -> 1.0 (constant cell/ship size, grid stretches up).
    if cfg.stretch_t > 0.0 {
        let t = cfg.stretch_t;
        let uy_far = cfg.uniform_boundary_y(pos.row);
        let uy_near = cfg.uniform_boundary_y(pos.row + 1);
        let uspan = cfg.uniform_half_span();
        let ucenter_x = cfg.center_x();
        let cols = cfg.cols.max(1) as f32;
        let ucol = |c: usize| ucenter_x - uspan + (uspan * 2.0) * (c as f32) / cols;
        let ux_l = ucol(pos.col);
        let ux_r = ucol(pos.col + 1);
        let lerp = |a: f32, b: f32| a + (b - a) * t; // by the arc factor t
        let mix = |a: f32, b: f32, f: f32| a + (b - a) * f; // by an explicit factor

        // Y stretch is shared by BOTH stretch modes (the curved & straight variants
        // differ only in the X path): rows lerp toward the uniform stacked y's.
        let (y_far, y_near) = (lerp(corners[0][1], uy_far), lerp(corners[3][1], uy_near));

        // X path:
        //   * CURVED (#140): each corner's x lerps toward its own uniform x. The far
        //     (top) and near (bottom) edges converge at different rates, so the column
        //     edges BOW through the mid-arc (straight only at t=1).
        //   * STRAIGHT (#142 Bruce): both the far + near x of a column boundary follow
        //     ONE row-independent trajectory — lerp(per-row perspective x, [lerp(near-
        //     row perspective x, uniform x, t)], t). At t->0 it returns the true per-row
        //     perspective x (smooth from the skipped block); at t->1 it returns the
        //     row-independent uniform x. Because the SAME column boundary uses the same
        //     row-independent straightened target in every cell, the column verticalizes
        //     as the arc raises and is perfectly straight (+ rows straight) at t=1.
        let (xfl, xfr, xnl, xnr) = if cfg.stretch_lines_continuous {
            // (#151 Bruce) CONTINUOUS-STRAIGHT: each depth line (column boundary) is ONE
            // straight line on SCREEN from the grid's NEAR (front) edge to its FAR (back)
            // edge — no per-cell kinks ("stepped per quadrant"). Build the boundary's
            // straight endpoint x at the grid's near + far edges, then place every corner's
            // x by interpolating against its SCREEN-Y position along the grid's near->far Y
            // span (NOT the depth fraction — at a mid arc step screen-y isn't linear in
            // depth, so a depth-fraction lerp would still kink on screen). Interpolating by
            // screen-y puts every corner exactly on the straight screen line by construction.
            let inv_z_near_edge = 1.0 / cfg.z_at(cfg.boundary_d(cfg.rows));
            let inv_z_far_edge = 1.0 / cfg.z_at(cfg.boundary_d(0));
            let endpoint = |c: usize, ux: f32| {
                let near_x = lerp(cfg.boundary_x(c, inv_z_near_edge), ux); // grid near edge x
                let far_x = lerp(cfg.boundary_x(c, inv_z_far_edge), ux); // grid far edge x
                (near_x, far_x)
            };
            let (near_l, far_l) = endpoint(pos.col, ux_l);
            let (near_r, far_r) = endpoint(pos.col + 1, ux_r);
            // The grid's near + far SCREEN-Y at THIS t — the front row's near edge and the
            // back row's far edge, each LERPED perspective->uniform exactly as the per-cell
            // y's above (`lerp(perspective_y, uniform_y)`), so the line endpoints match the
            // grid's actual drawn extent at this arc step (not the full-uniform y, which
            // would only match at t=1 and reintroduce a screen kink mid-arc).
            let grid_near_y = lerp(
                cfg.screen_y(1.0 / cfg.z_at(cfg.boundary_d(cfg.rows))),
                cfg.uniform_boundary_y(cfg.rows),
            ); // front edge (largest y)
            let grid_far_y = lerp(
                cfg.screen_y(1.0 / cfg.z_at(cfg.boundary_d(0))),
                cfg.uniform_boundary_y(0),
            ); // back edge (smallest y)
            let span = grid_near_y - grid_far_y;
            // Screen-y fraction from the near edge: near -> 0, far -> 1.
            let yfrac = |y: f32| {
                if span.abs() < 1e-4 {
                    0.0
                } else {
                    (grid_near_y - y) / span
                }
            };
            let fy = yfrac(y_far);
            let ny = yfrac(y_near);
            (
                mix(near_l, far_l, fy), // far-left  on the left depth line
                mix(near_r, far_r, fy), // far-right on the right depth line
                mix(near_l, far_l, ny), // near-left
                mix(near_r, far_r, ny), // near-right
            )
        } else if cfg.stretch_straight {
            // Row-independent perspective reference = the NEAR row's column-boundary x
            // (the widest, the visual anchor), so all rows share one straightening target.
            let inv_z_near_row = 1.0 / cfg.z_at(cfg.boundary_d(cfg.rows));
            let near_ref_l = cfg.boundary_x(pos.col, inv_z_near_row);
            let near_ref_r = cfg.boundary_x(pos.col + 1, inv_z_near_row);
            // The straightened (row-independent) target each boundary heads toward:
            // near-row x at low t -> uniform x at t=1 (so columns are row-independent =>
            // straight ACROSS rows by t=1).
            let straight_l = lerp(near_ref_l, ux_l); // by t
            let straight_r = lerp(near_ref_r, ux_r);
            // Step 1: lerp each corner's x from its perspective value toward the shared
            // straightened boundary (smooth at t=0, the block being skipped there).
            let mut xl_far = lerp(corners[0][0], straight_l);
            let mut xr_far = lerp(corners[1][0], straight_r);
            let mut xl_near = lerp(corners[3][0], straight_l);
            let mut xr_near = lerp(corners[2][0], straight_r);
            // Step 2: collapse the far x onto the near x by t so the column edge becomes
            // VERTICAL within each cell (no bow). At t=1 far==near==straight target =>
            // straight columns; at t->0 the collapse fades to 0 => smooth from perspective.
            let ml = mix(xl_far, xl_near, 0.5);
            let mr = mix(xr_far, xr_near, 0.5);
            xl_far = lerp(xl_far, ml);
            xl_near = lerp(xl_near, ml);
            xr_far = lerp(xr_far, mr);
            xr_near = lerp(xr_near, mr);
            (xl_far, xr_far, xl_near, xr_near)
        } else {
            (
                lerp(corners[0][0], ux_l),
                lerp(corners[1][0], ux_r),
                lerp(corners[3][0], ux_l),
                lerp(corners[2][0], ux_r),
            )
        };
        corners[0] = [xfl, y_far];
        corners[1] = [xfr, y_far];
        corners[2] = [xnr, y_near];
        corners[3] = [xnl, y_near];

        cx = 0.25 * (xfl + xfr + xnl + xnr);
        cy = 0.5 * (y_far + y_near);
        depth_scale = lerp(depth_scale, 1.0); // uniform = near-size everywhere
    }

    CellQuad {
        corners,
        center: [cx, cy],
        depth_scale,
    }
}

/// (#70 scene-space) The cell's **camera-space point** under the projector's
/// pinhole — the position a 3-D hull is placed at so it projects EXACTLY onto the
/// cell, through [`camera_perspective`]. The projector is a pure `1/z` pinhole
/// (camera at the origin looking along `+z`, no pitch): a cell at camera depth
/// `z` sits at camera `(Xc, Yc, z)` with unit focal lengths, where `Xc` is the
/// lateral fan offset, `Yc = (near_row_y − horizon_y)·z_near` is the CONSTANT
/// "ground" offset (the projector shifts every cell by the same screen-Y/z, so
/// the ground is the camera-Y plane `Yc`; a hull placed here sits on it and
/// foreshortens through the same `1/z`), and `z` follows from the cell's
/// screen-Y. So `screen = (center_x + Xc/z, horizon_y + Yc/z)` reproduces the
/// projector. Returns `[Xc, Yc, z]` (the renderer yaws the hull about +Y at this
/// point, then projects via [`camera_perspective`]).
pub fn cell_camera_point(pos: Pos, cfg: &ProjectorConfig) -> [f32; 3] {
    // Derive the camera point by INVERTING the projection against the cell's
    // actual `grid_cell_quad(pos).center` — guarantees it projects back onto that
    // exact centre (the quad's centre is a trapezoid centroid, which averages the
    // near+far inv_z; recomputing from mid-depth alone drifts a fraction of a px
    // in x). Yc is the constant "ground" offset; from center_y = hy + Yc/z we get
    // z, then Xc = (center_x − cx)·z so center_x = cx + Xc/z exactly.
    let center = grid_cell_quad(pos, cfg).center;
    let cx = cfg.center_x();
    let hy = cfg.horizon_y;
    let yc = (cfg.near_row_y - cfg.horizon_y) * cfg.z_near;
    // center_y − hy = Yc/z  ⇒  z = Yc / (center_y − hy). (center_y > hy for any
    // real cell — every row sits below the horizon.)
    let dy = (center[1] - hy).max(1e-3);
    let z = yc / dy;
    let xc = (center[0] - cx) * z;
    [xc, yc, z]
}

/// (#70 scene-space) The projector's pinhole as a column-major `view_proj`
/// matrix: maps a camera-space point `(Xc, Yc, Zc)` to virtual-pixel screen via
/// `screen_x = center_x + Xc/Zc`, `screen_y = horizon_y + Yc/Zc` (unit focal
/// lengths, the basis [`cell_camera_point`] is built on). This is the SAME `1/z`
/// the grid lines + cell quads use, so a 3-D hull rendered through it agrees with
/// the grid BY CONSTRUCTION — no per-facing yaw calibration. The renderer places
/// the hull at [`cell_camera_point`], yaws it about +Y to its world heading, and
/// projects through this; all 4 facings × all cells come out correct.
///
/// Output `clip.xy` is in **virtual-pixel** space (origin top-left, y-down) after
/// the perspective divide; the GPU vertex shader converts xy to NDC. `clip.z/clip.w`
/// is a LINEAR [0,1] depth over `[z_near, z_far·DEPTH_FAR_MARGIN]` (near = 0, far
/// = 1) so the hull's own faces depth-test with `CompareFunction::Less` (nearer
/// wins). The renderer places the hull at [`cell_camera_point`], yaws it about +Y
/// to its world heading, and projects through this; all 4 facings × all cells +
/// the hull's own self-occlusion come out correct by construction.
pub fn camera_perspective(cfg: &ProjectorConfig) -> [f32; 16] {
    // Column-major (m[col*4 + row]). For a point p = (x, y, z, 1):
    //   x' = x + cx·z          → x'/w = x/z + cx   (screen x)
    //   y' = y + hy·z          → y'/w = y/z + hy   (screen y)
    //   z' = k·z − k·z_near    → z'/w = k − k·z_near/z  ... NOT linear in z.
    // We want z'/w LINEAR in z ([0,1] over [z_near, z_far_m]). Since w = z, set
    //   z' = k·z² − k·z_near·z  → z'/w = k·z − k·z_near = k·(z − z_near).  But a
    // mat4 can't make z². Instead use a SEPARATE near/far split that's linear in
    // 1/z (standard perspective depth): z'/w = A + B/z. Map z_near→0, z_far_m→1:
    //   A + B/z_near = 0 ;  A + B/z_far_m = 1  →  B = z_near·z_far_m/(z_near−z_far_m),
    //   A = −B/z_near. With w=z: z' = A·z + B  (z'/w = A + B/z). ✓ mat-expressible.
    let cx = cfg.center_x();
    let hy = cfg.horizon_y;
    let zn = cfg.z_near;
    let zf = cfg.z_far * DEPTH_FAR_MARGIN;
    let b = zn * zf / (zn - zf); // < 0 (zf > zn)
    let a = -b / zn;
    // z' = a·z + b  (col2 row2 = a ; col3 row3-as-const... but the constant goes in
    // col 3 since p.w = 1). w' = z (col2 row3 = 1).
    [
        1.0, 0.0, 0.0, 0.0, // col 0
        0.0, 1.0, 0.0, 0.0, // col 1
        cx, hy, a, 1.0, // col 2: x+=cx·z, y+=hy·z, z'=a·z(+b below), w'=z
        0.0, 0.0, b, 0.0, // col 3: +b into z' (constant, p.w=1)
    ]
}

/// How far past `z_far` the depth range extends, so a hull at the far row (or a
/// hull whose bow reaches a touch beyond its cell's depth) still maps inside
/// `[0,1]` and isn't clipped by the far plane.
const DEPTH_FAR_MARGIN: f32 = 1.5;

/// Project a camera-space point through [`camera_perspective`] to a virtual-pixel
/// screen point (the perspective divide). Returns `None` if behind/at the camera
/// (`z ≤ 0`). Used by the unit test (cell point must land on the cell centre) and
/// any CPU-side placement check.
pub fn project_point(m: &[f32; 16], p: [f32; 3]) -> Option<Point2> {
    // column-major mat·vec with w=1.
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if w <= 1e-6 {
        return None;
    }
    Some(Point2::new(x / w, y / w))
}

/// The lane grid's **vanishing point** in virtual-pixel space — where the
/// receding columns converge (`1/z → 0`). Computed GEOMETRICALLY from the
/// projection, not assumed: take an off-centre column's near-row and far-row
/// cell centres (which lie on that column's converging edge) and extend the line
/// to where it meets the frame-centre vertical (`x = frame_w/2`); every column's
/// line meets there. For the symmetric pinhole projector this lands at
/// `(frame_w/2, horizon_y)`, but deriving it survives any projector retune.
///
/// The renderer aims the chase-cam player's nose exactly at this point so an
/// off-lane ship banks toward the convergence (up-lane), with no hand-tuned
/// angle table.
pub fn vanishing_point(cfg: &ProjectorConfig) -> Point2 {
    let center_x = cfg.frame_w * 0.5;
    // Column 0 (left edge) — off-centre, so its near→far centres define a sloped
    // line toward the convergence. (Col COLS/2 sits on center_x and gives no
    // slope, so use an edge column.)
    let near = grid_cell_quad(Pos::new(0, ROWS - 1), cfg).center;
    let far = grid_cell_quad(Pos::new(0, 0), cfg).center;
    let [nx, ny] = near;
    let [fx, fy] = far;
    let dx = fx - nx;
    if dx.abs() < 1e-6 {
        // Degenerate (a column already on center) — fall back to the horizon line.
        return Point2::new(center_x, cfg.horizon_y);
    }
    // Parametric: P = near + t·(far − near). Solve for x = center_x.
    let t = (center_x - nx) / dx;
    let vp_y = ny + t * (fy - ny);
    Point2::new(center_x, vp_y)
}

// ===========================================================================
// UNIFIED real-perspective camera (Bruce order: grid + ships share ONE camera).
//
// The legacy fan above is an exact `1/z` pinhole but at a ~178° effective FOV, so
// a real hull placed in it wraps the camera (#73). This camera draws the SAME
// board as a flat grid on the ground plane (y=0) seen by a SANE-FOV perspective
// camera; the 3-D hulls render through the identical `view_proj` at their cells,
// so they live in the grid (nose→VP + per-column outward lean by construction).
//
// World layout (1 cell = 1 unit): columns along +X centred on 0 (col 0 = left);
// rows along +Z with the NEAR row (row ROWS-1) at the front (small Z) and the FAR
// row (row 0) deepest. Ground plane y = 0; +Y is up. Camera sits above & behind
// the near edge, pitched down, looking at the board centre.
// ===========================================================================

/// Vertical field of view (degrees) of the unified camera — a SANE lens so a real
/// hull at a cell projects without wrapping. Tunable look.
const UNIFIED_FOV_Y_DEG: f32 = 52.0;
/// Camera look-down pitch (degrees below horizontal) at grid-pitch `t = 0`.
/// (#188 Bruce live shot) 22° was too shallow — near-row hull projected into the
/// bottom HUD band. 30° tipped the near row up out of the menu, was the previous
/// floor.
///
/// (#P7-prep Bruce) Lowered to 20° to expose the HORIZON for the upcoming
/// distance-preview boards (P7) — they sit on the SAME ground plane at deeper Z
/// through the unified camera, so flatter pitch lets the player see further back
/// along the receding plane. The near-row-clears-HUD invariant from #188 is
/// preserved at the new BOOT step (step 4 of 10 = ~40.8° effective look-down,
/// matching the prior boot's 40.5°); step 0 = 20° is now an OPT-IN flatter view
/// reachable via G that prioritises horizon visibility over HUD clearance.
const UNIFIED_PITCH_DEG: f32 = 20.0;
/// Camera look-down pitch at full grid-pitch (`t = 1`, the `G` arc → near top-down).
const UNIFIED_TOPDOWN_PITCH_DEG: f32 = 72.0;
/// World Z of the board's NEAR edge (front of the near row) — how far the board
/// sits in front of the camera's look-at reference.
const UNIFIED_Z_FRONT: f32 = 1.3;
// (#192 Bruce) Unified-camera orbit distance is now LIVE — read from the
// gfx-side atomic so the `-` / `=` keys dial the board size at runtime without
// a rebuild. Boot value = [`gfx::BOOT_UNIFIED_CAM_DIST`] (5.5, #193 verified
// default — cleaner bottom margin than the original #191 5.0 lock). Clamped
// into `[gfx::UNIFIED_CAM_DIST_MIN, gfx::UNIFIED_CAM_DIST_MAX]` = [3.5, 7.0].
// Per-column lean + cell-center alignment are distance-independent invariants
// (covered by `tests/render_orientation.rs`).
/// Default look-at height above the ground (world units), the value Bruce
/// verified at the [`gfx::BOOT_UNIFIED_CAM_DIST`] = 5.5 default — board sits in
/// a clear central band with starfield above + clean gap to the menu below.
/// (#188) 0.6→0.3 raised the board for the bottom HUD; (#193 Bruce verified)
/// 0.3 is the locked default at d=5.5.
///
/// (#197 Bruce) From this anchor (`d=5.5`, `t_y=0.3`) the NEAR-ROW `screen_y`
/// is computed once and then [`unified_target_y_anchored`] solves for `t_y` at
/// any other `d` so the near edge stays PARKED at the same `screen_y` while the
/// board grows UPWARD into the empty sky — zoom no longer pushes the near row
/// into the bottom menu.
const UNIFIED_TARGET_Y_DEFAULT: f32 = 0.3;

/// The unified camera's look-down pitch (radians) for this `cfg`, lerped along the
/// `G` grid-pitch arc ([`ProjectorConfig::pitch_t`]).
fn unified_pitch_rad(cfg: &ProjectorConfig) -> f32 {
    let t = cfg.pitch_t.clamp(0.0, 1.0);
    (UNIFIED_PITCH_DEG + (UNIFIED_TOPDOWN_PITCH_DEG - UNIFIED_PITCH_DEG) * t).to_radians()
}

/// (#197 Bruce) Live look-at height that ANCHORS the board's near row at a
/// fixed `screen_y` above the bottom menu. As [`gfx::unified_cam_dist`] (`-`/`=`)
/// scales the camera distance, this function solves for `t_y` so the projected
/// `screen_y` of the near row's centre is invariant — only the FAR edge moves
/// (board grows UP into the sky), the near edge stays parked.
///
/// Derivation (camera-up = (0, cos p, sin p), camera-forward = (0, -sin p, cos p)):
///   `view_y_near` = `-t_y·cos p + (near_z - z_target)·sin p`          (`d` cancels)
///   `view_z_near` = ` t_y·sin p + d + (near_z - z_target)·cos p`
///   `screen_y` ∝ `view_y / view_z` = `k`  (constant by design)
/// Solving for `t_y` at any `d`, given the anchor ratio `k` computed once at
/// the default pair (`d₀` = [`crate::gfx::BOOT_UNIFIED_CAM_DIST`],
/// `t_y₀` = [`UNIFIED_TARGET_Y_DEFAULT`]).
///
/// Limitation (#200, PARKED — reviewer-a math-audit follow-up): the anchor
/// ratio `k` is solved ONCE from a fixed default pair
/// (`d₀` = [`crate::gfx::BOOT_UNIFIED_CAM_DIST`] = 5.5,
/// `t_y₀` = [`UNIFIED_TARGET_Y_DEFAULT`] = 0.3) that was tuned at
/// pitch = [`UNIFIED_PITCH_DEG`] (30°) AND cell scale = 1.0. The closed-form
/// is exact + distance-invariant within those constraints (the `d·sin·cos`
/// terms cancel in `view_y_near`), so dialling zoom in ISOLATION holds the
/// near edge to within ~1 px. But STACKING dials breaks the anchor: pressing
/// `G` (grid-pitch) or `K`/`L` (cell scale ≠ 1) FIRST and THEN zooming shifts
/// the parked near-row by tens of px at the [3.5, 7.0] extremes — `k` was
/// computed against a stale (pitch, scale) baseline that no longer matches the
/// live geometry. The common path (zoom alone) is fine; full fix is to
/// recompute (`d₀`, `t_y₀`) against the live pitch + cell-scale on each call
/// so the anchor stays exact across the cross-dial product space. Don't
/// pre-do — Bruce dials zoom in isolation today; revisit if he combines and
/// sees drift.
///
/// (#198 Bruce) Branches on [`crate::gfx::anchor_mode_centered`]: when true,
/// returns 0.0 so the look-at sits on the board centroid (ground plane,
/// `z = z_center`) — the board's centroid then projects to screen centre,
/// giving a vertically CENTERED pose (equal margin top + bottom). The default
/// (false) runs the closed-form snap-to-menu solve below.
fn unified_target_y_anchored(cfg: &ProjectorConfig) -> f32 {
    if crate::gfx::anchor_mode_centered() {
        // Mode B: look at the board's ground centroid. The look-at point
        // projects to screen centre by construction → board sits vertically
        // centered in the window (independent of d, p, and cell scale).
        return 0.0;
    }
    let p = unified_pitch_rad(cfg);
    let cos_p = p.cos();
    let sin_p = p.sin();
    // Near-row centre Z minus look-at Z. Look-at sits at the un-scaled board
    // centre (z_center in `unified_target`); near-row centre uses the live cell
    // scale (#195). At scale=1 these reduce to -2.0 for ROWS=5 (the original).
    let s = crate::gfx::unified_grid_cell_scale();
    let rows = cfg.rows.max(1) as f32;
    let near_z = UNIFIED_Z_FRONT + 0.5 * s; // cell_world_center for the near row centre
    let z_target = UNIFIED_Z_FRONT + rows * 0.5; // unified_target z (unchanged by #195)
    let dz = near_z - z_target;
    // Anchor: compute the constant ratio k once at the default (d₀, t_y₀).
    let d0 = crate::gfx::BOOT_UNIFIED_CAM_DIST;
    let ty0 = UNIFIED_TARGET_Y_DEFAULT;
    let view_y0 = -ty0 * cos_p + dz * sin_p;
    let view_z0 = ty0 * sin_p + d0 + dz * cos_p;
    if view_z0.abs() < 1e-6 {
        return ty0;
    }
    let k = view_y0 / view_z0;
    // Back-solve t_y for the LIVE d so view_y / view_z = k.
    let d = crate::gfx::unified_cam_dist();
    let denom = -cos_p - k * sin_p;
    if denom.abs() < 1e-6 {
        return ty0;
    }
    (k * (d + dz * cos_p) - dz * sin_p) / denom
}

/// World-space look-at target (board centre on the ground, lifted by the
/// [`unified_target_y_anchored`] coupling so the near edge stays parked across
/// the `-`/`=` zoom range, and laterally panned by
/// [`crate::gfx::unified_lateral_x_offset`] so an outside-lane ship on the 5x4
/// board gets pulled in-frame per #207).
fn unified_target(cfg: &ProjectorConfig) -> [f32; 3] {
    let z_center = UNIFIED_Z_FRONT + cfg.rows as f32 * 0.5;
    [
        crate::gfx::unified_lateral_x_offset(),
        unified_target_y_anchored(cfg),
        z_center,
    ]
}

/// World-space camera eye: orbit [`crate::gfx::unified_cam_dist`] (live, #192)
/// from the target at the look-down pitch, behind the near edge (smaller Z) and
/// above. Shares the look-at's X (lateral pan) so the camera translates
/// laterally without rotating — grid + hulls slide together (#188 holds).
fn unified_eye(cfg: &ProjectorConfig) -> [f32; 3] {
    let p = unified_pitch_rad(cfg);
    let t = unified_target(cfg);
    let d = crate::gfx::unified_cam_dist();
    [t[0], t[1] + d * p.sin(), t[2] - d * p.cos()]
}

/// The unified camera's `view_proj` (column-major, RH, clip-z `0..1`, looking down
/// `-z`) — the SAME matrix the grid cells AND the 3-D hulls project through. Pure
/// function of `cfg` (frame size, cols/rows, pitch arc), so the renderer and the
/// CPU-side cell projection never disagree.
pub fn unified_view_proj(cfg: &ProjectorConfig) -> [f32; 16] {
    let aspect = cfg.frame_w / cfg.frame_h.max(1.0);
    let proj = u_perspective(UNIFIED_FOV_Y_DEG.to_radians(), aspect, 0.1, 100.0);
    let view = u_look_at(unified_eye(cfg), unified_target(cfg), [0.0, 1.0, 0.0]);
    u_mul4(proj, view)
}

/// World-space ground-plane corners of cell `pos`, ordered to match
/// [`CellQuad::corners`]: `[far-left, far-right, near-right, near-left]` (top-left,
/// top-right, bottom-right, bottom-left on screen — "far" = deeper = higher).
pub fn cell_world_corners(pos: Pos, cfg: &ProjectorConfig) -> [[f32; 3]; 4] {
    let cols = cfg.cols.max(1) as f32;
    let rows = cfg.rows.max(1) as f32;
    // (#195) Live cell-size multiplier — scales X spacing (from the grid centre
    // line at x=0) AND Z spacing (forward of UNIFIED_Z_FRONT) by the same factor
    // so the grid stays SQUARE + the cell-center == grid-cell-center invariant
    // (#188) holds (the corner-average matches `cell_world_center` which uses
    // the same scale).
    let s = crate::gfx::unified_grid_cell_scale();
    // Camera looks down +Z with +Y up, so world +X maps to screen LEFT. We want col
    // to increase LEFT→RIGHT on screen, so the screen-left edge of cell `col` is the
    // LARGER world X (boundary `col`), the screen-right edge the smaller (boundary
    // `col+1`).
    let left_x = (cols * 0.5 - pos.col as f32) * s;
    let right_x = (cols * 0.5 - (pos.col as f32 + 1.0)) * s;
    // Row pos.row occupies Z ∈ [near_z, far_z]; near row (row rows-1) front at Z_FRONT.
    let near_z = UNIFIED_Z_FRONT + (rows - 1.0 - pos.row as f32) * s;
    let far_z = near_z + s;
    [
        [left_x, 0.0, far_z],   // far-left  (top-left)
        [right_x, 0.0, far_z],  // far-right (top-right)
        [right_x, 0.0, near_z], // near-right (bottom-right)
        [left_x, 0.0, near_z],  // near-left (bottom-left)
    ]
}

/// World-space ground-plane centre of cell `pos` (where a hull is seated/yawed).
pub fn cell_world_center(pos: Pos, cfg: &ProjectorConfig) -> [f32; 3] {
    cell_world_center_frac(pos.col as f32, pos.row as f32, cfg)
}

/// (#201 fix A) FRACTIONAL variant of [`cell_world_center`] — interpolates the
/// ground-plane centre for a non-integer `(col, row)` so a moving ship's hull
/// SLIDES cell-to-cell through the unified ship pass instead of snapping. At
/// integer inputs this is exactly [`cell_world_center`] (corner-averaging plus
/// the #188 cell-centre invariant continue to hold). The unified ship pass
/// calls this with the `Tween2d`-eased fractional cell during a move.
pub fn cell_world_center_frac(col: f32, row: f32, cfg: &ProjectorConfig) -> [f32; 3] {
    let cols = cfg.cols.max(1) as f32;
    let rows = cfg.rows.max(1) as f32;
    // (#195) Same scale as cell_world_corners — ships auto-recenter as cells grow/shrink.
    let s = crate::gfx::unified_grid_cell_scale();
    // See cell_world_corners: world +X = screen LEFT, so col increasing right→ smaller X.
    let x = (cols * 0.5 - (col + 0.5)) * s;
    let z = UNIFIED_Z_FRONT + (rows - 1.0 - row + 0.5) * s;
    [x, 0.0, z]
}

/// (#P7/#213) Z-OFFSET variant of [`cell_world_corners`] over an EXPLICIT dims
/// pair — same ground-plane corners, shifted along +Z by `z_offset`. Lets the
/// renderer place an UPCOMING board at deeper Z through the SAME unified
/// camera, even when the upcoming dims differ from the current ones (#199b
/// variable boards). `cfg` is the SCENE projector cfg (frame size, base
/// cols/rows for camera FOV math) — the camera doesn't change with the
/// preview's grid dims, only the grid layout does. Pass `(cols, rows)`
/// matching the upcoming `EncounterDef::dims`. At `z_offset = 0` and
/// `(cols, rows) == (cfg.cols, cfg.rows)` this returns exactly
/// [`cell_world_corners`].
pub fn cell_world_corners_offset_dims(
    pos: Pos,
    _cfg: &ProjectorConfig,
    z_offset: f32,
    cols: usize,
    rows: usize,
) -> [[f32; 3]; 4] {
    let cols_f = cols.max(1) as f32;
    let rows_f = rows.max(1) as f32;
    let s = crate::gfx::unified_grid_cell_scale();
    let left_x = (cols_f * 0.5 - pos.col as f32) * s;
    let right_x = (cols_f * 0.5 - (pos.col as f32 + 1.0)) * s;
    let near_z = UNIFIED_Z_FRONT + (rows_f - 1.0 - pos.row as f32) * s + z_offset;
    let far_z = near_z + s;
    [
        [left_x, 0.0, far_z],
        [right_x, 0.0, far_z],
        [right_x, 0.0, near_z],
        [left_x, 0.0, near_z],
    ]
}

/// (#P7/#213) Z-OFFSET variant of [`cell_world_corners`] — same ground-plane
/// corners as [`cell_world_corners`], shifted along +Z by `z_offset`. Wraps
/// [`cell_world_corners_offset_dims`] at `(cfg.cols, cfg.rows)` for the
/// same-dims case.
pub fn cell_world_corners_offset(pos: Pos, cfg: &ProjectorConfig, z_offset: f32) -> [[f32; 3]; 4] {
    cell_world_corners_offset_dims(pos, cfg, z_offset, cfg.cols, cfg.rows)
}

/// (#P7/#213) Z-OFFSET variant of [`cell_world_center_frac`] — same ground-
/// plane centre, shifted along +Z by `z_offset`. Mirrors
/// [`cell_world_corners_offset`] so a ship rendered at the offset board's
/// `(col, row)` cell aligns with the offset grid cell quad by construction
/// (#188 alignment guard holds at any `z_offset`).
pub fn cell_world_center_frac_offset(
    col: f32,
    row: f32,
    cfg: &ProjectorConfig,
    z_offset: f32,
) -> [f32; 3] {
    let mut c = cell_world_center_frac(col, row, cfg);
    c[2] += z_offset;
    c
}

/// World-space direction a ship heading `N` (up-lane) points: deeper into the
/// board, `+Z`. The renderer yaws the hull about `+Y` from this so E/S/W follow.
/// (Provided so the ship pass + tests share the convention.)
pub const UNIFIED_HEADING_N: [f32; 3] = [0.0, 0.0, 1.0];

/// Project a world point through the unified `view_proj` `m` to virtual-pixel
/// screen space (origin top-left, y-down). `None` if behind the camera.
pub fn unified_project(m: &[f32; 16], p: [f32; 3], cfg: &ProjectorConfig) -> Option<Point2> {
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if w <= 1e-4 {
        return None;
    }
    let ndc_x = x / w;
    let ndc_y = y / w;
    Some(Point2::new(
        (ndc_x * 0.5 + 0.5) * cfg.frame_w,
        (0.5 - ndc_y * 0.5) * cfg.frame_h,
    ))
}

/// Clip-w (≈ camera distance) of a world point through `m` — used for the
/// depth-scale normalisation (on-screen size ∝ `1/w`).
fn unified_clip_w(m: &[f32; 16], p: [f32; 3]) -> f32 {
    m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15]
}

/// [`grid_cell_quad`] for the UNIFIED camera: project the cell's four ground
/// corners + centre through [`unified_view_proj`], and set `depth_scale` from the
/// `1/w` ratio against the near row (so near ≈ 1.0, far shrinks — same semantics as
/// the fan path). The screen quad is a true perspective trapezoid, so the grid and
/// any hull rendered through the same matrix agree by construction.
fn unified_cell_quad(pos: Pos, cfg: &ProjectorConfig) -> CellQuad {
    let m = unified_view_proj(cfg);
    let wc = cell_world_corners(pos, cfg);
    let project = |p: [f32; 3]| {
        unified_project(&m, p, cfg)
            .unwrap_or_else(|| Point2::new(cfg.frame_w * 0.5, cfg.frame_h * 0.5))
    };
    let c0 = project(wc[0]);
    let c1 = project(wc[1]);
    let c2 = project(wc[2]);
    let c3 = project(wc[3]);
    let center_w = cell_world_center(pos, cfg);
    let center = project(center_w);
    // depth_scale: near-row-normalised 1/w (near ≈ 1.0, far < 1.0).
    let w_near = unified_clip_w(&m, cell_world_center(Pos::new(pos.col, cfg.rows - 1), cfg));
    let w_cell = unified_clip_w(&m, center_w);
    let depth_scale = if w_cell.abs() > 1e-4 {
        (w_near / w_cell).clamp(0.05, 1.0)
    } else {
        1.0
    };
    CellQuad {
        corners: [c0.to_array(), c1.to_array(), c2.to_array(), c3.to_array()],
        center: center.to_array(),
        depth_scale,
    }
}

// --- small column-major mat4 helpers for the unified camera (RH, clip-z 0..1) ---

fn u_perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = near - far;
    [
        f / aspect,
        0.0,
        0.0,
        0.0, //
        0.0,
        f,
        0.0,
        0.0, //
        0.0,
        0.0,
        far / nf,
        -1.0, //
        0.0,
        0.0,
        (far * near) / nf,
        0.0,
    ]
}

fn u_look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let norm = |v: [f32; 3]| {
        let m = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-8);
        [v[0] / m, v[1] / m, v[2] / m]
    };
    let f = norm(sub(eye, center)); // +z points toward the eye (RH, look down -z)
    let s = norm(cross(up, f));
    let u = cross(f, s);
    [
        s[0],
        u[0],
        f[0],
        0.0, //
        s[1],
        u[1],
        f[1],
        0.0, //
        s[2],
        u[2],
        f[2],
        0.0, //
        -dot(s, eye),
        -dot(u, eye),
        -dot(f, eye),
        1.0,
    ]
}

fn u_mul4(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = sum;
        }
    }
    out
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

    /// (#213 preview centering) The upcoming preview's world-X centerline must
    /// project to the SAME screen X as the playable board's centerline (both
    /// sit at world X=0, the camera looks at X=0 by default), regardless of
    /// either board's dims. Bruce's "preview shifted off the midlane axis"
    /// regression would fire here at the offending dims pair.
    #[test]
    fn preview_midline_aligns_with_playable_midline_across_variable_dims() {
        // Test against the canonical 480x270 frame size.
        let frame_cx = 480.0 * 0.5;
        // Pairs of (playable dims, preview dims) that exercise the cross-
        // product: small + wider, wider + small, same dims, edge cases.
        let pairs = [
            ((2, 2), (4, 4)), // Bruce's bug case shape
            ((2, 2), (5, 4)),
            ((5, 4), (2, 2)),
            ((3, 3), (4, 2)),
            ((4, 2), (3, 3)),
            ((5, 4), (5, 4)),
            ((2, 4), (4, 2)),
        ];
        for (playable, preview) in pairs {
            let cfg = ProjectorConfig::for_scene(480.0, 270.0)
                .with_unified(0.0)
                .with_dims(playable.0, playable.1);
            let m = unified_view_proj(&cfg);
            // Playable midline: boundary between cols (playable.0-1)/2 and that+1.
            // For even cols, X=0 lies between them; for odd cols, the centre col
            // straddles X=0. In both cases the cell-centre col-midpoint at the
            // even cell or the (col, row)=mid cell projects to ~frame_cx.
            // We measure the projected screen-X for the playable's column
            // boundary at col=playable.0/2 (the geometric midline), at the near
            // row. That world point has X = (playable.0*0.5 - playable.0/2) * s,
            // which is either 0 (even cols) or 0.5*s (odd cols — half a cell
            // offset). Use the even-grid case for cleaner test.
            //
            // For the comparison: take the WORLD point at (0, 0, near_z) of the
            // playable's NEAR row, and the SAME world X=0 point at (0, 0,
            // near_z+z_offset) of the preview. Both world points have X=0; both
            // should project to screen X = frame_cx exactly.
            //
            // Playable near-row Z. cell_world_corners_offset_dims(pos(0, rows-1), 0)
            // gives the near row's corners — its near_z is the front edge of
            // the playable board.
            let playable_near = cell_world_corners_offset_dims(
                Pos::new(0, playable.1 - 1),
                &cfg,
                0.0,
                playable.0,
                playable.1,
            );
            // playable_near[2] = near-right of cell (0, rows-1). For even cols
            // the cell (0,...) right edge is at X=0 → boundary. For odd cols the
            // cell (cols/2,...) straddles X=0; use a fractional cell instead.
            // Simpler: build the world point (0, 0, playable_near_z) directly
            // and project — the X=0 IS the midline by construction.
            let playable_near_z = playable_near[2][2];
            let p_screen =
                unified_project(&m, [0.0, 0.0, playable_near_z], &cfg).expect("playable midline");

            // Preview midline at the same world X=0, at the preview's near row.
            let preview_near = cell_world_corners_offset_dims(
                Pos::new(0, preview.1 - 1),
                &cfg,
                12.5,
                preview.0,
                preview.1,
            );
            let preview_near_z = preview_near[2][2];
            let pv_screen =
                unified_project(&m, [0.0, 0.0, preview_near_z], &cfg).expect("preview midline");

            assert!(
                (p_screen.x - frame_cx).abs() < 1.0,
                "playable midline X=0 should project to frame_cx ({frame_cx}); got {} (dims={:?})",
                p_screen.x,
                playable,
            );
            assert!(
                (pv_screen.x - frame_cx).abs() < 1.0,
                "preview midline X=0 should project to frame_cx ({frame_cx}); got {} (dims={:?})",
                pv_screen.x,
                preview,
            );
            assert!(
                (p_screen.x - pv_screen.x).abs() < 1.0,
                "playable + preview midlines must project to same screen X; playable={} (dims={:?}), preview={} (dims={:?})",
                p_screen.x,
                playable,
                pv_screen.x,
                preview,
            );
        }
    }

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    /// The vanishing point sits on the frame-centre vertical, at/above the
    /// horizon, and EVERY column's near→far centre line, extended, passes
    /// through it (that's what makes it the convergence). Verifies the geometric
    /// computation rather than the assumed `(W/2, horizon_y)`.
    #[test]
    fn vanishing_point_is_the_column_convergence() {
        let c = cfg();
        let vp = vanishing_point(&c);
        // On the frame-centre vertical.
        assert!(
            approx(vp.x, c.frame_w * 0.5, 1e-3),
            "vp.x {} != center",
            vp.x
        );
        // Near the horizon line. NOT exactly equal: the VP is extrapolated from
        // cell CENTRES (mid-depth `1/z`), whose converging lines meet a few px
        // below the pure `1/z→0` horizon constant — the cell-centre convergence
        // is what the rendered ship aims at, which is the point. Sanity-bound it
        // to the horizon's neighbourhood.
        assert!(
            (vp.y - c.horizon_y).abs() <= 8.0,
            "vp.y {} should be near horizon {}",
            vp.y,
            c.horizon_y
        );
        // Every column's near→far line, extended, hits the VP: the t that takes
        // near→far to x=vp.x must land y≈vp.y for cols 0..COLS (skip the centre
        // column, whose line is vertical / already at vp.x).
        for col in 0..COLS {
            if col == COLS / 2 {
                continue;
            }
            let n = grid_cell_quad(Pos::new(col, ROWS - 1), &c).center;
            let f = grid_cell_quad(Pos::new(col, 0), &c).center;
            let dx = f[0] - n[0];
            assert!(dx.abs() > 1e-4, "col {col} edge should slope");
            let t = (vp.x - n[0]) / dx;
            let y = n[1] + t * (f[1] - n[1]);
            assert!(
                approx(y, vp.y, 1.0),
                "col {col} line hits y {y}, vp.y {}",
                vp.y
            );
        }
    }

    /// (#70 scene-space) THE deterministic camera-derivation oracle: every cell's
    /// camera-space point, projected through `camera_perspective`, must land
    /// EXACTLY on that cell's `grid_cell_quad(pos).center`. This is what makes the
    /// scene-space ship render correct BY CONSTRUCTION (a hull placed at the cell
    /// point + projected through the same matrix as the grid agrees with the grid)
    /// — verified by math, no rendering, no eyeball.
    #[test]
    fn cell_camera_point_projects_to_cell_center() {
        let c = cfg();
        let m = camera_perspective(&c);
        for row in 0..ROWS {
            for col in 0..COLS {
                let pos = Pos::new(col, row);
                let p = cell_camera_point(pos, &c);
                let proj = project_point(&m, p).expect("cell in front of camera");
                let center = grid_cell_quad(pos, &c).center;
                assert!(
                    approx(proj.x, center[0], 1e-2) && approx(proj.y, center[1], 1e-2),
                    "cell {col},{row}: projected ({:.3},{:.3}) != cell center ({:.3},{:.3})",
                    proj.x,
                    proj.y,
                    center[0],
                    center[1]
                );
            }
        }
    }

    /// (#70 deterministic bow gate) THE non-eyeball verification that a facing-N
    /// (up-lane) hull banks its BOW TOWARD the vanishing point at every column —
    /// the thing that slipped past screenshot review ~5 times. Under scene-space
    /// (`camera_perspective`), world-heading-N = +Z (deeper = toward the VP), so the
    /// bow point = `cell_camera_point` + (`0,0,+bow_len`). We project the bow + the
    /// cell centre and assert the bow's screen-x is on the VP side of centre:
    ///   col 0 (left of centre)  → `bow_x` > `centre_x` (banks up-RIGHT toward VP)
    ///   col 2 (centre)          → `bow_x` ≈ `centre_x` (straight up)
    ///   col 4 (right of centre) → `bow_x` < `centre_x` (banks up-LEFT toward VP)
    /// This is correct BY CONSTRUCTION for `camera_perspective` (no yaw-sign to get
    /// wrong) — it's the oracle the billboard must match or be replaced by.
    #[test]
    fn facing_n_bow_banks_toward_vp_every_column() {
        let c = cfg();
        let m = camera_perspective(&c);
        let vp = vanishing_point(&c);
        let bow_len = 2.0_f32; // any positive +Z reach toward the VP
        let row = ROWS - 1; // the player's front row
        for col in 0..COLS {
            let pos = Pos::new(col, row);
            let cp = cell_camera_point(pos, &c);
            // World-heading N (up-lane) = +Z (deeper toward the VP).
            let bow = [cp[0], cp[1], cp[2] + bow_len];
            let cell_scr = project_point(&m, cp).expect("cell in front");
            let bow_scr = project_point(&m, bow).expect("bow in front");
            // The bow must be NEARER the VP-x (frame centre) than the cell centre
            // — i.e. it banks toward the convergence, never toward the screen edge.
            let cell_off = (cell_scr.x - vp.x).abs();
            let bow_off = (bow_scr.x - vp.x).abs();
            assert!(
                bow_off <= cell_off + 1e-3,
                "col {col}: facing-N bow must bank TOWARD the VP — bow_x {:.2} (off {:.2}) not nearer VP.x {:.2} than cell_x {:.2} (off {:.2})",
                bow_scr.x, bow_off, vp.x, cell_scr.x, cell_off
            );
            // And the bow must sit higher on screen than the cell (it points up-lane
            // / into the distance), which is what makes it READ as bow-on.
            assert!(
                bow_scr.y < cell_scr.y,
                "col {col}: facing-N bow should be higher (up-lane) than the cell centre"
            );
        }
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

    /// (UNIFY) The unified camera produces a SANE, correctly-oriented grid AND — the
    /// crux of the #73 fix — a hull-sized box at every cell projects in FRONT of the
    /// camera to a sane screen position (it does NOT wrap, the failure the legacy fan
    /// forced). Plus the same grid invariants as the fan: col 0 screen-left, far row
    /// higher, `depth_scale` shrinks far<near<=1.
    #[test]
    fn unified_grid_is_sane_and_a_hull_does_not_wrap() {
        let c = ProjectorConfig::for_scene(480.0, 270.0).with_unified(0.0);
        // Orientation: col 0 is screen-LEFT of the last col on the near row.
        let near_l = grid_cell_quad(Pos::new(0, ROWS - 1), &c);
        let near_r = grid_cell_quad(Pos::new(COLS - 1, ROWS - 1), &c);
        assert!(
            near_l.center[0] < near_r.center[0],
            "col 0 ({}) must be screen-left of col {} ({})",
            near_l.center[0],
            COLS - 1,
            near_r.center[0]
        );
        // Middle column sits ~on the frame centre (symmetric board).
        let mid = grid_cell_quad(Pos::new(COLS / 2, ROWS - 1), &c);
        assert!(
            (mid.center[0] - c.frame_w * 0.5).abs() < 2.0,
            "middle column should be ~centred, got {}",
            mid.center[0]
        );
        // Far row higher (smaller y) than near row.
        let far = grid_cell_quad(Pos::new(2, 0), &c);
        assert!(far.center[1] < near_l.center[1], "far row should be higher");
        // NO WRAP: a hull-sized box (±0.6 around each cell centre, up to +0.6 tall)
        // projects in front of the camera to a sane screen position at EVERY cell —
        // the thing the ~178° fan made impossible (#73).
        let m = unified_view_proj(&c);
        for row in 0..ROWS {
            for col in 0..COLS {
                let ctr = cell_world_center(Pos::new(col, row), &c);
                for dx in [-0.6f32, 0.6] {
                    for dz in [-0.6f32, 0.6] {
                        for dy in [0.0f32, 0.6] {
                            let p = [ctr[0] + dx, ctr[1] + dy, ctr[2] + dz];
                            let s = unified_project(&m, p, &c)
                                .expect("hull corner is in front of the camera (no wrap)");
                            assert!(
                                s.x > -300.0
                                    && s.x < c.frame_w + 300.0
                                    && s.y > -300.0
                                    && s.y < c.frame_h + 300.0,
                                "cell {col},{row} hull corner projects sane, got {},{}",
                                s.x,
                                s.y
                            );
                        }
                    }
                }
            }
        }
        // depth_scale shrinks with depth, near ~1.
        let dn = grid_cell_quad(Pos::new(2, ROWS - 1), &c).depth_scale;
        let df = grid_cell_quad(Pos::new(2, 0), &c).depth_scale;
        assert!(
            df < dn && dn <= 1.0 && df > 0.0,
            "depth_scale should shrink far {df} < near {dn} <= 1"
        );
    }

    /// The far row is drawn SMALLER (`depth_scale` shrinks with recession). The
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
    /// corners legitimately exceed [0, `frame_w`]. The FAR row (row 0, where enemies
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

    /// (#76 scene-res GATE) `for_scene` at the virtual canvas size is BYTE-identical
    /// to `default()` — the invariant the pixel-identity gate stands on (a scene-res
    /// cycle at the 480×270 default must reproduce the baseline frame exactly). All
    /// look fields must match, not just the framing, so a future field added to one
    /// constructor can't silently drift the default.
    #[test]
    fn for_scene_at_virtual_size_equals_default() {
        let d = ProjectorConfig::default();
        let s =
            ProjectorConfig::for_scene(crate::gfx::VIRTUAL_W as f32, crate::gfx::VIRTUAL_H as f32);
        assert_eq!(d.frame_w, s.frame_w);
        assert_eq!(d.frame_h, s.frame_h);
        assert_eq!(d.z_near, s.z_near);
        assert_eq!(d.z_far, s.z_far);
        assert_eq!(d.horizon_y, s.horizon_y);
        assert_eq!(d.near_row_y, s.near_row_y);
        assert_eq!(d.fan_half_width, s.fan_half_width);
        assert_eq!(d.cols, s.cols);
        assert_eq!(d.rows, s.rows);
        // And projecting any cell gives the identical quad.
        let q_d = grid_cell_quad(Pos::new(0, 0), &d);
        let q_s = grid_cell_quad(Pos::new(0, 0), &s);
        assert_eq!(q_d, q_s);
    }

    /// (#76 scene-res) A scaled scene preserves the look's PROPORTIONS: doubling the
    /// canvas doubles the vertical anchors + lateral fan (the fractions — horizon
    /// position, near-row clearance, lane spread — are invariant), while the
    /// unitless camera depths stay put. This is what makes a larger/smaller preset
    /// render the SAME scene rather than a board pinned to 480×270 coordinates.
    #[test]
    fn for_scene_scales_anchors_proportionally() {
        let base = ProjectorConfig::for_scene(480.0, 270.0);
        let big = ProjectorConfig::for_scene(960.0, 540.0);
        assert!(approx(big.horizon_y, base.horizon_y * 2.0, 1e-3));
        assert!(approx(big.near_row_y, base.near_row_y * 2.0, 1e-3));
        assert!(approx(big.fan_half_width, base.fan_half_width * 2.0, 1e-3));
        // Unitless depths unchanged by the pixel canvas size.
        assert_eq!(big.z_near, base.z_near);
        assert_eq!(big.z_far, base.z_far);
        // The center column still lands on the (now-doubled) frame centre.
        let mid = grid_cell_quad(Pos::new(COLS / 2, ROWS - 1), &big);
        assert!(approx(mid.center[0], big.frame_w * 0.5, 1e-3));
    }

    /// (#213 item 4 / #199b) `with_dims` overrides `cfg.cols/cfg.rows` so a
    /// variable-board encounter lays out at its `EncounterDef` dims. Step-0
    /// invariant: chaining `.with_dims(COLS, ROWS)` is byte-identical to the
    /// default (the bin's old 5x4 path stays unchanged). And: at 3x3 the
    /// `grid_cell_quad` of the center (1, 1) cell projects to roughly the
    /// frame's horizontal centre line, proving the projection respects the
    /// shrunken grid (rather than treating it as the 5x4 (1, 1) offset which
    /// would be left of centre).
    #[test]
    fn with_dims_step0_identity_and_3x3_centers_at_frame_center() {
        let base = cfg().with_unified(0.0);
        // Step-0: with_dims(COLS, ROWS) is byte-identical to the default.
        let same = base.with_dims(COLS, ROWS);
        assert_eq!(same.cols, base.cols);
        assert_eq!(same.rows, base.rows);
        let q_b = grid_cell_quad(Pos::new(COLS / 2, ROWS - 1), &base);
        let q_s = grid_cell_quad(Pos::new(COLS / 2, ROWS - 1), &same);
        assert_eq!(q_b, q_s);

        // At 3x3 the centre cell (1, 1) lands roughly at the frame horizontal
        // centre (within a small tolerance — perspective makes "exactly the
        // centre" depend on the camera). At 5x4 the same (1, 1) would be
        // LEFT of centre — proving the projector reads the new dims.
        let three = base.with_dims(3, 3);
        let mid_3x3 = grid_cell_quad(Pos::new(1, 1), &three);
        let frame_cx = three.frame_w * 0.5;
        assert!(
            (mid_3x3.center[0] - frame_cx).abs() < 4.0,
            "3x3 centre cell (1,1) should project near frame_w/2 ({} vs {})",
            mid_3x3.center[0],
            frame_cx
        );

        // Sanity: dims override is independent — cols can differ from rows
        // (e.g. a 4x2 from the #199b dims pool).
        let four_by_two = base.with_dims(4, 2);
        assert_eq!(four_by_two.cols, 4);
        assert_eq!(four_by_two.rows, 2);
    }

    /// (#139) `with_pitch` keeps the grid's front-to-back screen DEPTH CONSTANT as it
    /// pitches toward top-down (Bruce: "grid depth should remain constant rather than
    /// getting stretched"). The grid's footprint = the front row's near edge (bottom)
    /// down to the back row's far edge (top); both screen-y's must hold across every
    /// pitch step. Also: the near-row size (`near_row_y`, `z_near`, fan) is untouched, so
    /// only the VIEWING ANGLE changes — the proof the projector is cleanly factored.
    #[test]
    fn with_pitch_holds_grid_depth_constant() {
        let base = cfg();
        let front = Pos::new(COLS / 2, ROWS - 1); // near row
        let back = Pos::new(COLS / 2, 0); // far row
        let near_y0 = grid_cell_quad(front, &base).bottom_left().y;
        let far_y0 = grid_cell_quad(back, &base).top_left().y;
        let depth0 = near_y0 - far_y0;
        assert!(depth0 > 0.0, "grid has positive screen depth at t=0");

        for step in 0..=8u32 {
            let t = step as f32 / 8.0;
            let p = base.with_pitch(t);
            let near_y = grid_cell_quad(front, &p).bottom_left().y;
            let far_y = grid_cell_quad(back, &p).top_left().y;
            // Front + back screen edges (hence the depth) are unchanged across t.
            assert!(
                approx(near_y, near_y0, 0.5),
                "near edge fixed at t={t}: {near_y} vs {near_y0}"
            );
            assert!(
                approx(near_y - far_y, depth0, 0.5),
                "grid depth constant at t={t}"
            );
            // The near-row size knobs are not touched — only the angle changes.
            assert_eq!(p.z_near, base.z_near);
            assert!(approx(p.near_row_y, base.near_row_y, 1e-3));
            assert!(approx(p.fan_half_width, base.fan_half_width, 1e-3));
            // Higher pitch = flatter perspective = larger z_near/z_far ratio (rows
            // spread toward even = more overhead).
            if step > 0 {
                assert!(
                    p.z_near / p.z_far > base.z_near / base.z_far,
                    "pitch flattens recession at t={t}"
                );
            }
        }
    }

    /// (#140) STRETCH mode: `with_stretch(0)` is byte-identical (the step-0 / no-
    /// regression invariant), and at t=1 the grid is UNIFORM — every row's cell is
    /// the SAME height (constant ship size, no balloon) and columns are PARALLEL (a
    /// true top-down square), with `depth_scale` == 1 everywhere.
    #[test]
    fn with_stretch_step0_identity_and_uniform_at_full() {
        let base = cfg();
        // Step-0 identity: with_stretch(0) projects byte-identical to base.
        let z = base.with_stretch(0.0);
        for r in 0..ROWS {
            for c in 0..COLS {
                let a = grid_cell_quad(Pos::new(c, r), &base);
                let b = grid_cell_quad(Pos::new(c, r), &z);
                assert_eq!(
                    a.corners, b.corners,
                    "stretch(0) corners identical at ({c},{r})"
                );
                assert_eq!(
                    a.center, b.center,
                    "stretch(0) center identical at ({c},{r})"
                );
                assert_eq!(
                    a.depth_scale, b.depth_scale,
                    "stretch(0) depth_scale identical"
                );
            }
        }
        // Full stretch: uniform grid. Cell heights equal across rows; columns parallel
        // (far width == near width); depth_scale == 1.
        let full = base.with_stretch(1.0);
        let col = COLS / 2;
        let h0 = {
            let q = grid_cell_quad(Pos::new(col, 0), &full);
            q.bottom_left().y - q.top_left().y
        };
        for r in 0..ROWS {
            let q = grid_cell_quad(Pos::new(col, r), &full);
            let h = q.bottom_left().y - q.top_left().y;
            assert!(
                approx(h, h0, 0.5),
                "uniform cell height at row {r}: {h} vs {h0}"
            );
            assert!(
                approx(q.near_edge_width(), q.far_edge_width(), 0.5),
                "parallel columns at row {r}"
            );
            assert!(
                approx(q.depth_scale, 1.0, 1e-3),
                "uniform depth_scale==1 at row {r}"
            );
        }
    }

    /// (#142) STRETCH-STRAIGHT: byte-identical at t=0 (the block is skipped), and at
    /// full stretch the columns are STRAIGHT — a single column's left-edge x is the SAME
    /// at every row (the far/top edge no longer narrower than the near/bottom), so the
    /// grid reads as a true rectangular top-down lattice (Bruce: "straight lines").
    /// Also: at a MID arc step the straight variant's columns are STRAIGHTER (less x
    /// spread across rows) than the curved variant — the whole point of the mode.
    #[test]
    fn with_stretch_straight_step0_identity_and_straight_columns() {
        let base = cfg();
        // Step-0 identity: with_stretch_straight(0) projects byte-identical to base.
        let z = base.with_stretch_straight(0.0);
        for r in 0..ROWS {
            for c in 0..COLS {
                let a = grid_cell_quad(Pos::new(c, r), &base);
                let b = grid_cell_quad(Pos::new(c, r), &z);
                assert_eq!(
                    a.corners, b.corners,
                    "straight(0) corners identical at ({c},{r})"
                );
            }
        }
        // Full stretch: a column's left-edge x is constant across rows (straight vertical).
        let full = base.with_stretch_straight(1.0);
        for c in 0..COLS {
            let x0 = grid_cell_quad(Pos::new(c, 0), &full).top_left().x;
            for r in 0..ROWS {
                let q = grid_cell_quad(Pos::new(c, r), &full);
                // far (top-left) and near (bottom-left) x equal => vertical edge, and the
                // same across rows => one straight column.
                assert!(
                    approx(q.top_left().x, q.bottom_left().x, 0.5),
                    "vertical col {c} edge at row {r}"
                );
                assert!(
                    approx(q.top_left().x, x0, 0.5),
                    "col {c} left-x constant across rows (row {r})"
                );
            }
        }
        // Mid arc: straight columns have LESS x-spread (far vs near) than curved.
        let mid_curved = base.with_stretch(0.5);
        let mid_straight = base.with_stretch_straight(0.5);
        let edge_col = 0; // an off-centre column bows the most
        let spread = |cfgm: &ProjectorConfig| {
            let mut s = 0.0f32;
            for r in 0..ROWS {
                let q = grid_cell_quad(Pos::new(edge_col, r), cfgm);
                s += (q.top_left().x - q.bottom_left().x).abs();
            }
            s
        };
        let curved_spread = spread(&mid_curved);
        let straight_spread = spread(&mid_straight);
        assert!(
            straight_spread <= curved_spread + 1e-3,
            "straight mid-arc columns should be straighter (spread {straight_spread} <= curved {curved_spread})"
        );
    }

    /// (#151) STRETCH-CONTINUOUS: byte-identical at t=0, and at ANY t>0 each depth line
    /// (column boundary) is ONE straight line front-to-back — every cell corner on that
    /// boundary lies on the line from the grid's near-edge point to its far-edge point
    /// (no per-cell kink = no "stepped per quadrant"). Verified at a MID arc step (the
    /// stepped variant kinks there) AND at full stretch.
    #[test]
    fn with_stretch_continuous_step0_identity_and_one_line_front_to_back() {
        let base = cfg();
        // Step-0 identity.
        let z = base.with_stretch_continuous(0.0);
        for r in 0..ROWS {
            for c in 0..COLS {
                let a = grid_cell_quad(Pos::new(c, r), &base);
                let b = grid_cell_quad(Pos::new(c, r), &z);
                assert_eq!(
                    a.corners, b.corners,
                    "continuous(0) corners identical at ({c},{r})"
                );
            }
        }
        // For a column boundary, gather every corner that lies on it across all rows
        // (each row contributes its far-left + near-left for the LEFT boundary of column
        // `col`), and assert they are colinear with the overall near->far line. Test the
        // left edge of an off-centre column (the one that bows/steps the most).
        for &t in &[0.5_f32, 1.0] {
            let p = base.with_stretch_continuous(t);
            let col = 0usize; // left boundary of column 0 = the leftmost depth line
                              // Endpoints: the near edge (front row's near-left) and far edge (far row's
                              // far-left) of this depth line.
            let near_pt = {
                let q = grid_cell_quad(Pos::new(col, ROWS - 1), &p);
                q.bottom_left()
            };
            let far_pt = {
                let q = grid_cell_quad(Pos::new(col, 0), &p);
                q.top_left()
            };
            // Every row's far-left + near-left corner must lie on the near->far line.
            let on_line = |x: f32, y: f32| {
                // Solve the line param by y, compare x.
                let dy = far_pt.y - near_pt.y;
                if dy.abs() < 1e-4 {
                    return true; // degenerate (flat) — skip
                }
                let s = (y - near_pt.y) / dy;
                let lx = near_pt.x + (far_pt.x - near_pt.x) * s;
                (lx - x).abs() <= 0.5
            };
            for r in 0..ROWS {
                let q = grid_cell_quad(Pos::new(col, r), &p);
                assert!(
                    on_line(q.top_left().x, q.top_left().y),
                    "continuous t={t}: row {r} far-left off the depth line"
                );
                assert!(
                    on_line(q.bottom_left().x, q.bottom_left().y),
                    "continuous t={t}: row {r} near-left off the depth line"
                );
            }
        }
    }
}
