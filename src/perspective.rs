//! Screen-space lane geometry: a flat horizontal strip bisecting the canvas.
//!
//! The lane is a horizontal line at `LaneGeometry::center_y`; cells are
//! evenly spaced left to right between `x_left` and `x_right`. The ship
//! sprite math in `hud.rs` rotates around the lane using a `view_angle`
//! parameter, so ships morph from pure side-view (θ = 0) to pure top-down
//! (θ = π/2) while the lane itself stays flat. Both parallax planes — sky
//! above the lane, floor below — foreshorten with the same angle so the
//! background reads as a revolving camera.
//!
//! This is the only module that knows about screen coordinates; everything
//! else lives in lane-cell space.

use crate::geometry::range_band;
use crate::types::RangeBand;

/// A 2-D point in virtual-pixel space (origin top-left, y-down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

/// Flat horizontal lane: cells evenly spaced from `x_left` to `x_right` at
/// vertical position `center_y`. `cell_count` is `5 | 7 | 9` per the design
/// doc; cell positions are interpolated linearly across the strip.
#[derive(Debug, Clone, Copy)]
pub struct LaneGeometry {
    /// Vertical position of the lane on screen. The horizon line.
    pub center_y: f32,
    /// X-coordinate of the leftmost cell's center.
    pub x_left: f32,
    /// X-coordinate of the rightmost cell's center.
    pub x_right: f32,
    /// Number of cells on the lane.
    pub cell_count: u32,
}

impl LaneGeometry {
    /// Half the distance between two adjacent cells. Convenience for ship
    /// sprite half-widths.
    pub fn cell_width(&self) -> f32 {
        if self.cell_count <= 1 {
            self.x_right - self.x_left
        } else {
            (self.x_right - self.x_left) / (self.cell_count - 1) as f32
        }
    }
}

/// Default geometry tuned for the engine's 1320×480 virtual canvas. A 7-cell
/// lane bisecting the window horizontally, with ~120 px margin on each side
/// for ship overhang.
pub const DEFAULT_LANE: LaneGeometry = LaneGeometry {
    center_y: 240.0,
    x_left: 130.0,
    x_right: 1190.0,
    cell_count: 7,
};

/// Map a cell index (0 .. cell_count−1) to its screen position. Linear
/// interpolation from `x_left` to `x_right` at constant `center_y`.
pub fn cell_to_screen(cell_index: u32, geom: &LaneGeometry) -> Point2 {
    let n = geom.cell_count.saturating_sub(1) as f32;
    let t = if n == 0.0 {
        0.0
    } else {
        (cell_index as f32) / n
    };
    Point2 {
        x: geom.x_left + t * (geom.x_right - geom.x_left),
        y: geom.center_y,
    }
}

/// Continuous version of `cell_to_screen` for fractional positions along the
/// lane — used by ordnance entities interpolating between cells. `t` is
/// clamped to `[0, cell_count - 1]`.
pub fn fractional_cell_to_screen(fractional_cell: f32, geom: &LaneGeometry) -> Point2 {
    let n = geom.cell_count.saturating_sub(1) as f32;
    let t = if n == 0.0 {
        0.0
    } else {
        (fractional_cell / n).clamp(0.0, 1.0)
    };
    Point2 {
        x: geom.x_left + t * (geom.x_right - geom.x_left),
        y: geom.center_y,
    }
}

/// A ship's design-pixel dimensions.
///
/// - `length` is bow-stern (horizontal along the lane in bow-on stance).
/// - `beam` is port-starboard (the depth axis perpendicular to length).
/// - `height` is the vertical extent at pure side view.
///
/// The view-angle scrubber stacks a FRONT face of vertical extent
/// `height × cos(angle)` underneath a TOP face of vertical extent
/// `beam × sin(angle) / 2`. At angle = 0 the top face collapses and the
/// ship reads as a pure side silhouette; at angle = PI/2 the front face
/// collapses and the ship reads as a pure top-down rectangle.
#[derive(Debug, Clone, Copy)]
pub struct ShipDims {
    pub length: f32,
    pub beam: f32,
    pub height: f32,
}

/// Default Frigate hull. Sized so the silhouette dominates a single cell
/// (lane cell width on `DEFAULT_LANE` is ~177 design px). `beam` is ~25%
/// of `length` for a recognizable side / top contrast.
pub const FRIGATE_DIMS: ShipDims = ShipDims { length: 168.0, beam: 42.0, height: 50.0 };

/// Which way the hull is turned. `BowOn` runs along the lane axis (length
/// along x); `Broadside` runs perpendicular (length along the depth axis,
/// which the view angle maps to top-face vertical extent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stance {
    BowOn,
    Broadside,
}

/// Range band a target sits in relative to a source cell. Thin convenience
/// wrapper over `geometry::range_band` so renderer code can stay
/// single-module; both paths MUST agree.
pub fn band_between_cells(source: u32, target: u32) -> RangeBand {
    range_band(source as usize, target as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn cell_to_screen_endpoints_match_lane_extents() {
        let near = cell_to_screen(0, &DEFAULT_LANE);
        assert!(approx_eq(near.x, DEFAULT_LANE.x_left, 1e-3));
        assert!(approx_eq(near.y, DEFAULT_LANE.center_y, 1e-3));
        let far = cell_to_screen(DEFAULT_LANE.cell_count - 1, &DEFAULT_LANE);
        assert!(approx_eq(far.x, DEFAULT_LANE.x_right, 1e-3));
        assert!(approx_eq(far.y, DEFAULT_LANE.center_y, 1e-3));
    }

    #[test]
    fn cell_to_screen_midpoint_is_halfway() {
        // 7 cells = 6 spans; cell 3 is at t = 3/6 = 0.5.
        let mid = cell_to_screen(3, &DEFAULT_LANE);
        let halfway = (DEFAULT_LANE.x_left + DEFAULT_LANE.x_right) / 2.0;
        assert!(approx_eq(mid.x, halfway, 1e-3));
    }

    #[test]
    fn cell_to_screen_single_cell_lane_is_safe() {
        // cell_count = 1 means n = 0; division-by-zero guard must hold.
        let geom = LaneGeometry { cell_count: 1, ..DEFAULT_LANE };
        let only = cell_to_screen(0, &geom);
        assert!(approx_eq(only.x, geom.x_left, 1e-3));
    }

    #[test]
    fn fractional_cell_clamps_into_bounds() {
        let below = fractional_cell_to_screen(-2.0, &DEFAULT_LANE);
        let at_left = cell_to_screen(0, &DEFAULT_LANE);
        assert!(approx_eq(below.x, at_left.x, 1e-3));
        let above = fractional_cell_to_screen(99.0, &DEFAULT_LANE);
        let at_right = cell_to_screen(DEFAULT_LANE.cell_count - 1, &DEFAULT_LANE);
        assert!(approx_eq(above.x, at_right.x, 1e-3));
    }

    #[test]
    fn fractional_cell_intermediate_interpolates_linearly() {
        // 7-cell lane, t = 4/6 should be 4/6 between x_left and x_right.
        let p = fractional_cell_to_screen(4.0, &DEFAULT_LANE);
        let expected_x =
            DEFAULT_LANE.x_left + (4.0 / 6.0) * (DEFAULT_LANE.x_right - DEFAULT_LANE.x_left);
        assert!(approx_eq(p.x, expected_x, 1e-3));
        assert!(approx_eq(p.y, DEFAULT_LANE.center_y, 1e-3));
    }

    #[test]
    fn cell_width_matches_lane_span_divided_by_n_minus_1() {
        let expected = (DEFAULT_LANE.x_right - DEFAULT_LANE.x_left) / 6.0;
        assert!(approx_eq(DEFAULT_LANE.cell_width(), expected, 1e-3));
    }

    #[test]
    fn band_between_cells_matches_geometry_range_band() {
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
