//! Screen-space perspective: lane trapezoid, cell projection, ship sprite
//! polygons. Pure functions; no wgpu, no winit, no rendering state.
//!
//! Port of `broadside-engine/engine/perspective.ts`. This is the only module
//! in the crate that knows about screen coordinates; everything else lives in
//! lane-cell space. When this port and the TS disagree, the TS is right
//! (modulo intentional Rust-shape changes called out below).
//!
//! ## Decisions encoded here (see `PERSPECTIVE.md` for rationale)
//!
//! 1. The lane is a tilted trapezoid running left-to-right on screen, with
//!    one-point perspective recession to the right. Vanishing point off-screen.
//! 2. Cells get smaller along the lane: linear scaling from `scale_near` to
//!    `scale_far`.
//! 3. Ship sprites use MILITARY AXONOMETRIC projection: depth (the
//!    port-starboard width axis) projects straight up in the ship's local
//!    unrotated frame.
//! 4. Every ship sprite is then rotated around its base by the lane's slope
//!    angle, so its long axis aligns with the lane (bow-on) or runs exactly
//!    perpendicular to it (broadside).
//! 5. Only the FRONT face and TOP face are rendered. Side faces collapse to
//!    zero width under military projection; that's intentional.
//! 6. The lane is a straight line, so the rotation angle is a single constant
//!    for every cell. A curved lane would compute a per-cell tangent.
//!
//! ## Rust-shape differences from `perspective.ts`
//!
//! - `ShipSprite` returns raw vertex arrays + a separate (pivot, angle) pair
//!   rather than a pre-formatted SVG `transform` string and `points` strings.
//!   The renderer composes the rotation into its vertex shader / instance
//!   transform; it never wants strings.
//! - The TS `bandBetweenCells` is renamed `band_between_cells` and lives
//!   alongside `geometry::range_band` (the canonical resolver-side bucket).
//!   They MUST agree; the test below asserts a cross-check.

use crate::geometry::range_band;
use crate::types::RangeBand;

/* ---- lane geometry --------------------------------------------------------- */

/// A 2-D point in screen space (pixels, y-down origin top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

/// The lane's screen-space footprint plus the cell count and end-to-end scale
/// gradient. One source of truth for the entire renderer.
#[derive(Debug, Clone, Copy)]
pub struct LaneGeometry {
    /// Front edge of the lane on screen, foreground end (cell 0 side).
    pub front_start: Point2,
    /// Front edge of the lane on screen, background end (cell N−1 side).
    pub front_end: Point2,
    /// Back edge of the lane (the parallel edge further from camera).
    pub back_start: Point2,
    pub back_end: Point2,
    /// Number of cells (5, 7, or 9).
    pub cell_count: u32,
    /// Sprite scale at the foreground (cell 0) end.
    pub scale_near: f32,
    /// Sprite scale at the background (cell N−1) end.
    pub scale_far: f32,
}

/// Default geometry tuned for a ~660×240 viewport. Mirrors `DEFAULT_LANE` in
/// `perspective.ts` byte-for-byte. The gfx layer either bumps the engine's
/// virtual resolution to a superset of 660×240 or supplies its own retuned
/// `LaneGeometry` via [`LaneGeometry::scaled`]; the math here is
/// viewport-agnostic.
pub const DEFAULT_LANE: LaneGeometry = LaneGeometry {
    front_start: Point2 { x: 35.0, y: 217.0 },
    front_end: Point2 { x: 615.0, y: 162.0 },
    back_start: Point2 { x: 28.0, y: 198.0 },
    back_end: Point2 { x: 615.0, y: 153.0 },
    cell_count: 7,
    scale_near: 1.0,
    scale_far: 0.55,
};

impl LaneGeometry {
    /// Uniformly scale every screen-space coordinate by `s` while leaving
    /// `cell_count` / `scale_near` / `scale_far` alone. Used to map a design-
    /// doc geometry (660×240) onto a larger virtual canvas (1320×480 etc.)
    /// without retuning the per-sprite scale gradient.
    pub fn scaled(&self, s: f32) -> Self {
        let scale_pt = |p: Point2| Point2 { x: p.x * s, y: p.y * s };
        Self {
            front_start: scale_pt(self.front_start),
            front_end: scale_pt(self.front_end),
            back_start: scale_pt(self.back_start),
            back_end: scale_pt(self.back_end),
            cell_count: self.cell_count,
            scale_near: self.scale_near,
            scale_far: self.scale_far,
        }
    }
}

/* ---- cell → screen --------------------------------------------------------- */

/// Where a cell lands on screen, plus the uniform sprite scale and the lane
/// slope angle to apply around `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellScreen {
    /// Center of the cell on the lane's FRONT edge (where the ship's base sits).
    pub x: f32,
    pub y: f32,
    /// Uniform scale applied to sprites at this cell.
    pub scale: f32,
    /// Rotation in RADIANS, applied around `(x, y)`, to align sprites with the
    /// lane. The TS version returns degrees; this port returns radians because
    /// every downstream consumer (rotation matrices, `sin`/`cos`) wants radians.
    pub rotation_rad: f32,
}

fn lane_slope_rad(geom: &LaneGeometry) -> f32 {
    let dx = geom.front_end.x - geom.front_start.x;
    let dy = geom.front_end.y - geom.front_start.y;
    dy.atan2(dx)
}

/// Map a cell index (0 .. cell_count−1) to its screen position, scale, and
/// rotation. Linear interpolation along the lane's front edge.
pub fn cell_to_screen(cell_index: u32, geom: &LaneGeometry) -> CellScreen {
    let n = geom.cell_count.saturating_sub(1) as f32;
    let t = if n == 0.0 { 0.0 } else { cell_index as f32 / n };
    let x = geom.front_start.x + t * (geom.front_end.x - geom.front_start.x);
    let y = geom.front_start.y + t * (geom.front_end.y - geom.front_start.y);
    let scale = geom.scale_near + t * (geom.scale_far - geom.scale_near);
    CellScreen { x, y, scale, rotation_rad: lane_slope_rad(geom) }
}

/// Continuous version of `cell_to_screen` for fractional positions along the
/// lane — used by ordnance entities interpolating between cells. The TS
/// version clamps `t` to `[0, 1]`; this port matches.
pub fn fractional_cell_to_screen(fractional_cell: f32, geom: &LaneGeometry) -> CellScreen {
    let n = geom.cell_count.saturating_sub(1) as f32;
    let t = if n == 0.0 { 0.0 } else { (fractional_cell / n).clamp(0.0, 1.0) };
    let x = geom.front_start.x + t * (geom.front_end.x - geom.front_start.x);
    let y = geom.front_start.y + t * (geom.front_end.y - geom.front_start.y);
    let scale = geom.scale_near + t * (geom.scale_far - geom.scale_near);
    CellScreen { x, y, scale, rotation_rad: lane_slope_rad(geom) }
}

/* ---- ship sprite vertices -------------------------------------------------- */

/// A ship's world-unit dimensions. `length` is bow-stern, `beam` is
/// port-starboard, `height` is vertical. Other classes override these.
#[derive(Debug, Clone, Copy)]
pub struct ShipDims {
    pub length: f32,
    pub beam: f32,
    pub height: f32,
}

/// Default Frigate hull. The TS reference uses `(56, 14, 6)`; the Rust
/// renderer holds at 2x `(112, 28, 12)` after a 3x bump produced an
/// offscreen-lane regression in bruce`s review. Reverted as a stop-gap
/// while diagnosing — see commit log.
pub const FRIGATE_DIMS: ShipDims = ShipDims { length: 112.0, beam: 28.0, height: 12.0 };

/// Which way the hull is turned. `BowOn` runs along the lane axis; `Broadside`
/// runs perpendicular to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stance {
    BowOn,
    Broadside,
}

/// The four vertices of a rectangle, in the unrotated screen frame. The
/// renderer rotates them as a group around `(pivot.x, pivot.y)` by
/// `rotation_rad`. Vertex order is bottom-left, bottom-right, top-right,
/// top-left (CCW with screen y-down).
pub type FacePoly = [Point2; 4];

/// Output of `ship_sprite`: vertex polygons for the two visible faces in the
/// unrotated frame, plus the rotation pivot and angle that align them with
/// the lane.
#[derive(Debug, Clone, Copy)]
pub struct ShipSprite {
    /// Pivot for the lane-slope rotation. Equal to `(cell.x, cell.y)`.
    pub pivot: Point2,
    /// Lane slope in radians; apply as a rotation about `pivot`.
    pub rotation_rad: f32,
    /// Four vertices of the front face (small, at the lane surface).
    pub front_face: FacePoly,
    /// Four vertices of the top face (larger, above the front face).
    pub top_face: FacePoly,
    /// Center of the top face in the unrotated frame — chevron anchor.
    pub top_center: Point2,
    /// Center of the front face — bridge / status anchor.
    pub front_center: Point2,
    /// On-screen unit vector along the ship's bow direction, POST-rotation.
    /// Multiply by `length / 2 * scale` for the bow tip in screen space.
    /// Used for chevron placement and beam-origin offsets.
    pub bow_dir: Point2,
}

/// Compute the polygon vertices and rotation transform for a ship sprite at a
/// cell. Military-axonometric projection in the unrotated frame, then a
/// single rotation around the base aligns it with the lane.
///
/// Visually: bow-on ships run parallel to the lane (long axis along screen-x
/// in the unrotated frame); broadside ships run perpendicular to the lane
/// (long axis along screen-y in the unrotated frame, i.e. up).
pub fn ship_sprite(cell: CellScreen, dims: ShipDims, stance: Stance) -> ShipSprite {
    let CellScreen { x, y, scale, rotation_rad } = cell;
    // Stance swap: broadside rotates the hull 90° in world, so on-screen its
    // along-lane dimension is `beam` and its depth dimension is `length`.
    let (screen_w, screen_d) = match stance {
        Stance::BowOn => (dims.length * scale, dims.beam * scale),
        Stance::Broadside => (dims.beam * scale, dims.length * scale),
    };
    let screen_h = dims.height * scale;
    let hw = screen_w / 2.0;
    let depth_offset = screen_d / 2.0;

    let front_face: FacePoly = [
        Point2 { x: x - hw, y },
        Point2 { x: x + hw, y },
        Point2 { x: x + hw, y: y - screen_h },
        Point2 { x: x - hw, y: y - screen_h },
    ];
    let top_face: FacePoly = [
        Point2 { x: x - hw, y: y - screen_h },
        Point2 { x: x + hw, y: y - screen_h },
        Point2 { x: x + hw, y: y - screen_h - depth_offset },
        Point2 { x: x - hw, y: y - screen_h - depth_offset },
    ];

    // Bow direction post-rotation. For bow-on, bow is along +x in the local
    // unrotated frame, then rotated by the lane slope. For broadside, bow is
    // along the +depth axis (which projects straight UP in the unrotated
    // frame, hence -y in screen coords) and is then rotated by the lane slope.
    let (cos_r, sin_r) = (rotation_rad.cos(), rotation_rad.sin());
    let bow_dir = match stance {
        Stance::BowOn => Point2 { x: cos_r, y: sin_r },
        Stance::Broadside => Point2 { x: -sin_r, y: -cos_r },
    };

    ShipSprite {
        pivot: Point2 { x, y },
        rotation_rad,
        front_face,
        top_face,
        top_center: Point2 { x, y: y - screen_h - depth_offset / 2.0 },
        front_center: Point2 { x, y: y - screen_h / 2.0 },
        bow_dir,
    }
}

/* ---- beam endpoints, cell highlight, and other render helpers -------------- */

/// Endpoints for a beam from one cell to another. Both endpoints sit on the
/// lane's front edge, so the beam follows the lane plane automatically.
pub fn beam_endpoints(source_cell: u32, target_cell: u32, geom: &LaneGeometry) -> (Point2, Point2) {
    let a = cell_to_screen(source_cell, geom);
    let b = cell_to_screen(target_cell, geom);
    (Point2 { x: a.x, y: a.y }, Point2 { x: b.x, y: b.y })
}

/// The four corners of a cell's footprint on the lane top, for selection
/// highlights and cell hover. Returns a parallelogram in the lane plane,
/// vertices ordered front-near, front-far, back-far, back-near.
pub fn cell_footprint(cell_index: u32, geom: &LaneGeometry) -> [Point2; 4] {
    let n = geom.cell_count as f32;
    let t0 = cell_index as f32 / n;
    let t1 = (cell_index + 1) as f32 / n;
    let lerp_pt = |a: Point2, b: Point2, t: f32| Point2 {
        x: a.x + t * (b.x - a.x),
        y: a.y + t * (b.y - a.y),
    };
    [
        lerp_pt(geom.front_start, geom.front_end, t0),
        lerp_pt(geom.front_start, geom.front_end, t1),
        lerp_pt(geom.back_start, geom.back_end, t1),
        lerp_pt(geom.back_start, geom.back_end, t0),
    ]
}

/// Range band a target sits in relative to a source cell. Thin convenience
/// wrapper over `geometry::range_band` so renderer code can stay
/// single-module; both paths MUST agree. The test below cross-checks every
/// cell-distance up to 9.
pub fn band_between_cells(source: u32, target: u32) -> RangeBand {
    range_band(source as usize, target as usize)
}

/* =============================================================================
 * Tests — one sanity assert per pure function, plus cross-checks against the
 * TS implementation's reference outputs from `render-example.ts`.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn cell_to_screen_near_matches_front_start() {
        let c = cell_to_screen(0, &DEFAULT_LANE);
        assert!(approx_eq(c.x, 35.0, 1e-3));
        assert!(approx_eq(c.y, 217.0, 1e-3));
        assert!(approx_eq(c.scale, 1.0, 1e-3));
    }

    #[test]
    fn cell_to_screen_far_matches_front_end() {
        let c = cell_to_screen(6, &DEFAULT_LANE);
        assert!(approx_eq(c.x, 615.0, 1e-3));
        assert!(approx_eq(c.y, 162.0, 1e-3));
        assert!(approx_eq(c.scale, 0.55, 1e-3));
    }

    #[test]
    fn cell_to_screen_midpoint_interpolates_evenly() {
        // 6 spans, midpoint is cell 3.
        let c = cell_to_screen(3, &DEFAULT_LANE);
        assert!(approx_eq(c.x, 35.0 + 3.0 / 6.0 * (615.0 - 35.0), 1e-3));
        assert!(approx_eq(c.y, 217.0 + 3.0 / 6.0 * (162.0 - 217.0), 1e-3));
        // Scale halfway between 1.0 and 0.55 is 0.775.
        assert!(approx_eq(c.scale, 0.775, 1e-3));
    }

    #[test]
    fn lane_slope_is_modest_uphill_to_the_right() {
        // dy = -55, dx = 580 → slope arctan(-55/580) ≈ -5.42°. Negative because
        // screen y-down: the lane visually rises toward the background.
        let c = cell_to_screen(0, &DEFAULT_LANE);
        let deg = c.rotation_rad.to_degrees();
        assert!(deg < 0.0);
        assert!(approx_eq(deg, -5.418, 0.01));
    }

    #[test]
    fn cell_to_screen_single_cell_lane_is_safe() {
        // cell_count = 1 means n = 0; the t = 0 branch must not divide by zero.
        let geom = LaneGeometry { cell_count: 1, ..DEFAULT_LANE };
        let c = cell_to_screen(0, &geom);
        assert!(approx_eq(c.x, 35.0, 1e-3));
        assert!(approx_eq(c.scale, 1.0, 1e-3));
    }

    #[test]
    fn fractional_cell_clamps_into_bounds() {
        // Negative fractional input is clamped to t = 0; >n is clamped to t = 1.
        let a = fractional_cell_to_screen(-2.0, &DEFAULT_LANE);
        let near = cell_to_screen(0, &DEFAULT_LANE);
        assert!(approx_eq(a.x, near.x, 1e-3));
        let b = fractional_cell_to_screen(99.0, &DEFAULT_LANE);
        let far = cell_to_screen(6, &DEFAULT_LANE);
        assert!(approx_eq(b.x, far.x, 1e-3));
    }

    #[test]
    fn fractional_cell_at_4_matches_ts_reference() {
        // render-example.ts plots ordnance at fractionalCell = 4.0 in
        // DEFAULT_LANE. The TS produces x = 35 + (4/6)*580 ≈ 421.67, y = 217
        // + (4/6)*(-55) ≈ 180.33, scale = 1.0 + (4/6)*(-0.45) = 0.7.
        let p = fractional_cell_to_screen(4.0, &DEFAULT_LANE);
        assert!(approx_eq(p.x, 421.667, 0.01));
        assert!(approx_eq(p.y, 180.333, 0.01));
        assert!(approx_eq(p.scale, 0.7, 1e-3));
    }

    #[test]
    fn ship_sprite_bow_on_long_axis_runs_along_lane() {
        // For a bow-on Frigate at cell 0 with scale 1.0: front face is
        // length x height, top face has depth = beam/2 in the unrotated frame.
        let cell = cell_to_screen(0, &DEFAULT_LANE);
        let s = ship_sprite(cell, FRIGATE_DIMS, Stance::BowOn);
        let p0 = s.front_face[0];
        let p2 = s.front_face[2];
        assert!(approx_eq(p2.x - p0.x, FRIGATE_DIMS.length, 1e-3));
        assert!(approx_eq(p0.y - p2.y, FRIGATE_DIMS.height, 1e-3));
        let t0 = s.top_face[0];
        let t3 = s.top_face[3];
        assert!(approx_eq(t0.y - t3.y, FRIGATE_DIMS.beam / 2.0, 1e-3));
    }

    #[test]
    fn ship_sprite_broadside_rotates_dimensions_90_degrees() {
        // Broadside swaps the dimensions: on-screen along-lane is beam,
        // depth is length, so depth_offset = length / 2 in the unrotated
        // frame.
        let cell = cell_to_screen(0, &DEFAULT_LANE);
        let s = ship_sprite(cell, FRIGATE_DIMS, Stance::Broadside);
        let p0 = s.front_face[0];
        let p2 = s.front_face[2];
        assert!(approx_eq(p2.x - p0.x, FRIGATE_DIMS.beam, 1e-3));
        assert!(approx_eq(p0.y - p2.y, FRIGATE_DIMS.height, 1e-3));
        let t0 = s.top_face[0];
        let t3 = s.top_face[3];
        assert!(approx_eq(t0.y - t3.y, FRIGATE_DIMS.length / 2.0, 1e-3));
    }

    #[test]
    fn ship_sprite_scales_with_cell_distance() {
        // Same ship at the far end of the lane is 55% the size of the near end.
        let near = ship_sprite(cell_to_screen(0, &DEFAULT_LANE), FRIGATE_DIMS, Stance::BowOn);
        let far = ship_sprite(cell_to_screen(6, &DEFAULT_LANE), FRIGATE_DIMS, Stance::BowOn);
        let near_w = near.front_face[1].x - near.front_face[0].x;
        let far_w = far.front_face[1].x - far.front_face[0].x;
        assert!(approx_eq(far_w / near_w, 0.55, 1e-3));
    }

    #[test]
    fn ship_sprite_bow_dir_bow_on_points_along_lane() {
        // BowOn: bow_dir = (cos(slope), sin(slope)). slope is small and
        // negative; bow_dir.x is positive, bow_dir.y slightly negative.
        let cell = cell_to_screen(0, &DEFAULT_LANE);
        let s = ship_sprite(cell, FRIGATE_DIMS, Stance::BowOn);
        assert!(s.bow_dir.x > 0.99);
        assert!(s.bow_dir.y < 0.0);
        // Unit length.
        let mag = (s.bow_dir.x * s.bow_dir.x + s.bow_dir.y * s.bow_dir.y).sqrt();
        assert!(approx_eq(mag, 1.0, 1e-4));
    }

    #[test]
    fn ship_sprite_bow_dir_broadside_points_off_lane() {
        // Broadside: bow_dir = (-sin(slope), -cos(slope)). slope ≈ -0.0946 rad;
        // -sin(-0.0946) ≈ +0.0945, -cos(-0.0946) ≈ -0.9955.
        let cell = cell_to_screen(0, &DEFAULT_LANE);
        let s = ship_sprite(cell, FRIGATE_DIMS, Stance::Broadside);
        assert!(s.bow_dir.x > 0.0); // off-lane (upward + slight slope skew)
        assert!(s.bow_dir.y < -0.99);
        let mag = (s.bow_dir.x * s.bow_dir.x + s.bow_dir.y * s.bow_dir.y).sqrt();
        assert!(approx_eq(mag, 1.0, 1e-4));
    }

    #[test]
    fn beam_endpoints_run_along_the_lane_front_edge() {
        let (from, to) = beam_endpoints(0, 6, &DEFAULT_LANE);
        assert!(approx_eq(from.x, 35.0, 1e-3));
        assert!(approx_eq(to.x, 615.0, 1e-3));
        // Both points lie on the front edge, so the line between them has the
        // same slope as the lane itself.
        let beam_slope = (to.y - from.y) / (to.x - from.x);
        let lane_slope = (DEFAULT_LANE.front_end.y - DEFAULT_LANE.front_start.y)
            / (DEFAULT_LANE.front_end.x - DEFAULT_LANE.front_start.x);
        assert!(approx_eq(beam_slope, lane_slope, 1e-4));
    }

    #[test]
    fn cell_footprint_returns_four_distinct_points() {
        let fp = cell_footprint(3, &DEFAULT_LANE);
        // Front edge (entries 0, 1) lies on the front line; back edge entries
        // (2, 3) lie on the back line.
        let front_slope = (DEFAULT_LANE.front_end.y - DEFAULT_LANE.front_start.y)
            / (DEFAULT_LANE.front_end.x - DEFAULT_LANE.front_start.x);
        let back_slope = (DEFAULT_LANE.back_end.y - DEFAULT_LANE.back_start.y)
            / (DEFAULT_LANE.back_end.x - DEFAULT_LANE.back_start.x);
        let actual_front = (fp[1].y - fp[0].y) / (fp[1].x - fp[0].x);
        let actual_back = (fp[2].y - fp[3].y) / (fp[2].x - fp[3].x);
        assert!(approx_eq(actual_front, front_slope, 1e-4));
        assert!(approx_eq(actual_back, back_slope, 1e-4));
    }

    #[test]
    fn scaled_doubles_every_coordinate_but_preserves_cell_count() {
        let g = DEFAULT_LANE.scaled(2.0);
        assert!(approx_eq(g.front_start.x, 70.0, 1e-3));
        assert!(approx_eq(g.front_start.y, 434.0, 1e-3));
        assert!(approx_eq(g.front_end.x, 1230.0, 1e-3));
        assert!(approx_eq(g.front_end.y, 324.0, 1e-3));
        assert_eq!(g.cell_count, DEFAULT_LANE.cell_count);
        assert!(approx_eq(g.scale_near, DEFAULT_LANE.scale_near, 1e-6));
        assert!(approx_eq(g.scale_far, DEFAULT_LANE.scale_far, 1e-6));
        // Slope is invariant under uniform scale.
        let near_default = cell_to_screen(0, &DEFAULT_LANE);
        let near_scaled = cell_to_screen(0, &g);
        assert!(approx_eq(near_default.rotation_rad, near_scaled.rotation_rad, 1e-6));
    }

    #[test]
    fn band_between_cells_matches_geometry_range_band() {
        // The renderer-side wrapper must never drift from the resolver's
        // canonical bucket. Spot-check every distance 0..=9.
        for s in 0u32..=9 {
            for t in 0u32..=9 {
                assert_eq!(
                    band_between_cells(s, t),
                    range_band(s as usize, t as usize),
                    "drift at ({}, {})", s, t
                );
            }
        }
    }
}
