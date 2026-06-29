//! Unified-camera regression guard (tracker #188).
//!
//! Locks in two render-side invariants that were Bruce-blocking before #188
//! landed (commit `f05358f`, `Render: unified camera grid fills frame + ships
//! align to cells (#188)`):
//!
//! 1. **Per-column lane lean.** A hull at column `c` with a bow facing along
//!    the lane axis (`Bow(N)` / `Bow(S)`) has its on-screen long axis
//!    PARALLEL to the column's lane direction (the screen-space tangent
//!    running near-row → far-row through that column). cross2d ≈ 0.
//!    Asserted at the edge columns (`0`, `4`) and the centre (`2`), across
//!    the live G pitch sweep (steps `0`, mid, full).
//!
//! 2. **Cell-centre alignment.** The renderer seats the hull at
//!    [`projector::cell_world_center`] and feeds that into the unified
//!    `view_proj`; the projector's own cell quad reports its `.center` as
//!    the projection of the same world point. They MUST agree to float
//!    precision (this is the property render-a diag-proved during the #188
//!    fix). Asserted on the near row AND the far row, every column.
//!
//! Both assertions key off the LIVE projector entry points (`unified_view_proj`,
//! `cell_world_center`, `grid_cell_quad`, `cell_world_corners`) so an internal
//! change to those — even a subtle one — surfaces here. We do NOT bake any
//! constants the live path doesn't already export. Ship scale (live `#190`
//! toggle) is irrelevant: the guard only reads cell-centre coincidence and
//! lean DIRECTION, never absolute hull size.
//!
//! E/W facing gets its own sub-assertion: a broadside hull's long axis is
//! HORIZONTAL on screen (its projected dy ≈ 0). Locks in Bruce's #172 ruling
//! ("horizontal on the grid is horizontal on the screen") under the unified
//! camera: world `±X` projects to a purely-horizontal screen direction
//! because the camera has no roll and world `+X` is the projector's
//! screen-LEFT axis. Stronger than perpendicular-to-lane: the lane itself
//! leans per-column under unified, but the E/W hull does NOT follow that
//! lean — it stays flat on screen.
//!
//! NOTE on the hull-yaw mapping. `hud::unified_heading_yaw(Facing)` is the
//! authority that converts a gameplay facing into the world heading vector
//! the renderer rotates the hull by, but it is `fn` (private to `hud.rs`),
//! so this integration test reproduces its dir-table inline. The table is
//! pinned by [`HEADING_DIR_TABLE_SOURCE`] below — if `hud.rs` ever shifts
//! the mapping, that comment is the trail to update both sides.
//!
//! Gated on `feature = "render"` because `projector` lives behind that
//! feature. Run locally with `cargo test --features render --test
//! render_orientation`. (Default-feature CI does not exercise this file —
//! see the team-lead's standing thread on a render-feature CI lane.)

#![cfg(feature = "render")]

use broadside_engine::grid::{Axis, Dir4, Facing, Pos, COLS, ROWS};
use broadside_engine::projector::{
    cell_world_center, cell_world_corners, grid_cell_quad, unified_project, unified_view_proj,
    ProjectorConfig,
};

/// Reference frame size — `VIRTUAL_W × VIRTUAL_H` (`480 × 270`). Matches every
/// other projector test + the bin's default scene resolution; the unified
/// camera's aspect math reads it via `cfg.frame_w / cfg.frame_h`. Sub-assertions
/// that need a sane frame use `ProjectorConfig::for_scene(FRAME_W, FRAME_H)`.
const FRAME_W: f32 = 480.0;
const FRAME_H: f32 = 270.0;

/// The columns the guard exercises — the two edges (where the per-column lean
/// is most visible / where it used to be wrong) and the centre (the no-lean
/// reference).
const SAMPLED_COLS: [usize; 3] = [0, 2, 4];

/// The `pitch_t` values the guard sweeps — boot (chase-cam), mid-arc, full
/// top-down. Mirrors the live `G` key cycle (`0..=GRID_PITCH_STEPS`).
const PITCH_T_SWEEP: [f32; 3] = [0.0, 0.5, 1.0];

/// `hud::unified_heading_yaw` dir-table mirror. Source of truth lives in
/// `src/hud.rs` (private `fn unified_heading_yaw(facing: Facing) -> f32`); if
/// you change that match arm set, mirror it here AND in this comment so the
/// regression guard tracks the live mapping.
///
/// Convention: world `+Z` is up-lane (N), world `+X` is screen-LEFT (the
/// projector flips X — see `cell_world_corners`).
///
/// |             facing             | dir vec        |
/// |--------------------------------|----------------|
/// | `Bow(N)` / `Broadside(NS)`     | `( 0, 0,  1)`  |
/// | `Bow(S)`                       | `( 0, 0, -1)`  |
/// | `Bow(E)` / `Broadside(EW)`     | `(-1, 0,  0)`  |
/// | `Bow(W)`                       | `( 1, 0,  0)`  |
const HEADING_DIR_TABLE_SOURCE: &str = "src/hud.rs :: fn unified_heading_yaw(facing) -> f32";

/// Returns the world heading vector a hull faces under `facing`, mirroring
/// `hud::unified_heading_yaw`'s dir-table. The renderer rotates the hull's
/// local `+X` onto this direction.
const fn heading_dir(facing: Facing) -> [f32; 3] {
    let _ = HEADING_DIR_TABLE_SOURCE; // documentation reference, not used at runtime
    match facing {
        Facing::Bow(Dir4::N) | Facing::Broadside(Axis::NorthSouth) => [0.0, 0.0, 1.0],
        Facing::Bow(Dir4::S) => [0.0, 0.0, -1.0],
        Facing::Bow(Dir4::E) | Facing::Broadside(Axis::EastWest) => [-1.0, 0.0, 0.0],
        Facing::Bow(Dir4::W) => [1.0, 0.0, 0.0],
    }
}

/// Build a unified `ProjectorConfig` at the canonical 480×270 frame with the
/// given `G` pitch arc value. Mirrors how the bin sets up the live config
/// (`ProjectorConfig::for_scene(VIRTUAL_W, VIRTUAL_H).with_unified(grid_pitch_t)`).
fn unified_cfg(pitch_t: f32) -> ProjectorConfig {
    ProjectorConfig::for_scene(FRAME_W, FRAME_H).with_unified(pitch_t)
}

/// Screen-space vector `(end - start)` after projecting both points through
/// the unified `view_proj`. Panics on behind-camera (the guard's input points
/// are all on the ground plane within frame; if projection ever fails here
/// the geometry itself is broken — exactly what we want to surface).
fn project_screen_vec(
    view_proj: &[f32; 16],
    cfg: &ProjectorConfig,
    start_world: [f32; 3],
    end_world: [f32; 3],
) -> [f32; 2] {
    let s = unified_project(view_proj, start_world, cfg).expect("start point projects");
    let e = unified_project(view_proj, end_world, cfg).expect("end point projects");
    [e.x - s.x, e.y - s.y]
}

/// 2-D cross product `a × b` (scalar). Zero ⇔ parallel.
fn cross2d(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

/// `√(x² + y²)`.
fn len2d(a: [f32; 2]) -> f32 {
    a[0].hypot(a[1])
}

/// The lane direction for column `col` is the screen-space tangent from the
/// near-row cell-centre to the far-row cell-centre in that column — the
/// "column lane line" running into the vanishing point. By construction this
/// is the direction every hull living in that column must lie ALONG to read
/// as lane-aligned (the property #188 fixed).
fn lane_screen_dir(view_proj: &[f32; 16], cfg: &ProjectorConfig, col: usize) -> [f32; 2] {
    let near = cell_world_center(Pos::new(col, ROWS - 1), cfg);
    let far = cell_world_center(Pos::new(col, 0), cfg);
    // far - near: from front (low on screen) to back (high on screen).
    project_screen_vec(view_proj, cfg, near, far)
}

/// The hull's on-screen long axis at cell `pos` with facing `facing`. The
/// renderer anchors the hull at `cell_world_center(pos)` and rotates its
/// local `+X` onto `heading_dir(facing)`; the screen vector from the
/// anchor → `anchor + heading_dir` IS the on-screen long axis (after the
/// same `view_proj` the cell quad uses).
fn hull_screen_long_axis(
    view_proj: &[f32; 16],
    cfg: &ProjectorConfig,
    pos: Pos,
    facing: Facing,
) -> [f32; 2] {
    let anchor = cell_world_center(pos, cfg);
    let dir = heading_dir(facing);
    let tip = [anchor[0] + dir[0], anchor[1] + dir[1], anchor[2] + dir[2]];
    project_screen_vec(view_proj, cfg, anchor, tip)
}

/* =========================================================================
 * (a) PER-COLUMN LEAN
 * ====================================================================== */

/// Asserts the hull's on-screen long axis is parallel to its column's lane
/// direction (cross2d / (|a||b|) is tiny).
fn assert_hull_parallel_to_lane(
    view_proj: &[f32; 16],
    cfg: &ProjectorConfig,
    pos: Pos,
    facing: Facing,
    eps: f32,
    label: &str,
) {
    let lane = lane_screen_dir(view_proj, cfg, pos.col);
    let hull = hull_screen_long_axis(view_proj, cfg, pos, facing);
    let denom = len2d(lane) * len2d(hull);
    assert!(
        denom > 1e-6,
        "{label}: degenerate vectors (lane={lane:?}, hull={hull:?})"
    );
    let sin_theta = cross2d(lane, hull).abs() / denom;
    assert!(
        sin_theta < eps,
        "{label}: hull long axis not parallel to lane (sin θ = {sin_theta}, eps = {eps}; \
         lane = {lane:?}, hull = {hull:?})"
    );
}

#[test]
fn bow_north_hull_long_axis_parallel_to_column_lane_at_edge_and_centre_columns() {
    // Front row (row = ROWS-1) is the most visually load-bearing — the lean
    // bug Bruce reported was that off-centre near-row hulls rendered
    // square-to-the-window instead of pointed at the vanishing point.
    let row = ROWS - 1;
    let facing = Facing::Bow(Dir4::N);
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_parallel_to_lane(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(N) col={col} row={row} pitch_t={pitch_t}"),
            );
        }
    }
}

#[test]
fn bow_south_hull_long_axis_parallel_to_column_lane_at_edge_and_centre_columns() {
    let row = ROWS - 1;
    let facing = Facing::Bow(Dir4::S);
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_parallel_to_lane(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(S) col={col} row={row} pitch_t={pitch_t}"),
            );
        }
    }
}

#[test]
fn bow_north_hull_lane_parallel_holds_on_far_row_too() {
    // Far row (row = 0) is the second visual hot-spot: under a too-narrow FOV
    // the back-row hulls converge AROUND the vanishing point, so lean-parity
    // there guards the across-depth case.
    let row = 0;
    let facing = Facing::Bow(Dir4::N);
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_parallel_to_lane(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(N) col={col} row={row} pitch_t={pitch_t}"),
            );
        }
    }
}

/* =========================================================================
 * (a-bis) E/W BROADSIDE — hull long axis is HORIZONTAL on screen
 * ====================================================================== */

/// Asserts the hull's on-screen long axis is horizontal (its screen-y
/// component is ≈ 0 relative to its screen-x). Locks in Bruce's #172
/// ruling: "horizontal on the grid is horizontal on the screen". Under the
/// unified camera world `±X` projects to a purely-horizontal screen
/// direction (the camera has no roll and world `+X` is the projector's
/// screen-LEFT axis), so an E/W bow's long axis = world `±X` always lands
/// flat on screen. Stronger than perpendicular-to-lane: the lane itself
/// leans per-column under unified, but the E/W hull does NOT follow the
/// lean — it stays flat. Both are correct invariants in their own right;
/// horizontal-on-screen is the legibility one Bruce called.
fn assert_hull_horizontal_on_screen(
    view_proj: &[f32; 16],
    cfg: &ProjectorConfig,
    pos: Pos,
    facing: Facing,
    eps: f32,
    label: &str,
) {
    let hull = hull_screen_long_axis(view_proj, cfg, pos, facing);
    let mag = len2d(hull);
    assert!(mag > 1e-6, "{label}: degenerate hull vector ({hull:?})");
    // |sin θ| against the horizontal x-axis = |dy| / |hull|.
    let sin_theta = hull[1].abs() / mag;
    assert!(
        sin_theta < eps,
        "{label}: hull long axis not horizontal on screen (sin θ = {sin_theta}, eps = {eps}; \
         hull = {hull:?})"
    );
}

#[test]
fn bow_east_hull_long_axis_horizontal_on_screen_at_edge_and_centre_columns() {
    let row = ROWS - 1;
    let facing = Facing::Bow(Dir4::E);
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_horizontal_on_screen(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(E) col={col} row={row} pitch_t={pitch_t}"),
            );
        }
    }
}

#[test]
fn bow_west_hull_long_axis_horizontal_on_screen_at_edge_and_centre_columns() {
    let row = ROWS - 1;
    let facing = Facing::Bow(Dir4::W);
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_horizontal_on_screen(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(W) col={col} row={row} pitch_t={pitch_t}"),
            );
        }
    }
}

/* =========================================================================
 * (b) CELL-CENTRE ALIGNMENT
 * ====================================================================== */

/// Asserts the projector's reported cell `.center` equals the projection of
/// the cell's ground-plane world centre. This is the property render-a
/// diag-proved during the #188 fix: hulls are seated at
/// [`cell_world_center`] and the cell quad reports its centre by projecting
/// the same world point — they MUST agree, so any future tweak that inserts
/// a hidden offset between "where the hull lives" and "where the cell quad
/// says its centre is" trips this gate.
fn assert_cell_center_alignment(
    view_proj: &[f32; 16],
    cfg: &ProjectorConfig,
    pos: Pos,
    eps: f32,
    label: &str,
) {
    let world_center = cell_world_center(pos, cfg);
    let projected = unified_project(view_proj, world_center, cfg).expect("cell centre projects");
    let quad = grid_cell_quad(pos, cfg);
    let dx = (projected.x - quad.center[0]).abs();
    let dy = (projected.y - quad.center[1]).abs();
    assert!(
        dx < eps && dy < eps,
        "{label}: projected cell-world-centre ({:?}) ≠ grid_cell_quad.center ({:?}) — \
         Δ = ({dx}, {dy}), eps = {eps}",
        [projected.x, projected.y],
        quad.center,
    );
}

#[test]
fn projected_cell_world_center_equals_grid_cell_quad_center_on_near_row() {
    let row = ROWS - 1;
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for col in 0..COLS {
            assert_cell_center_alignment(
                &vp,
                &cfg,
                Pos::new(col, row),
                1e-3,
                &format!("near-row col={col} pitch_t={pitch_t}"),
            );
        }
    }
}

#[test]
fn projected_cell_world_center_equals_grid_cell_quad_center_on_far_row() {
    let row = 0;
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for col in 0..COLS {
            assert_cell_center_alignment(
                &vp,
                &cfg,
                Pos::new(col, row),
                1e-3,
                &format!("far-row col={col} pitch_t={pitch_t}"),
            );
        }
    }
}

/* =========================================================================
 * (c) HEADLESS SMOKE — unified projection initialises + produces sane output
 * ====================================================================== */

#[test]
fn unified_projection_initialises_and_projects_every_cell_in_frame() {
    // Smoke gate: at every pitch step, every cell's world centre projects to
    // a finite screen point in [0, frame_w] × [0, frame_h]. Catches the class
    // of "near-row hull lands behind the camera" / "back-row hull off the top
    // of the frame" bugs that were the #188 symptom.
    for &pitch_t in &PITCH_T_SWEEP {
        let cfg = unified_cfg(pitch_t);
        let vp = unified_view_proj(&cfg);
        for row in 0..ROWS {
            for col in 0..COLS {
                let p = Pos::new(col, row);
                let world = cell_world_center(p, &cfg);
                let s = unified_project(&vp, world, &cfg).unwrap_or_else(|| {
                    panic!("cell {p:?} at pitch_t={pitch_t} projected behind the camera")
                });
                assert!(
                    s.x.is_finite() && s.y.is_finite(),
                    "cell {p:?} at pitch_t={pitch_t} projected to non-finite ({}, {})",
                    s.x,
                    s.y
                );
                assert!(
                    s.x >= -1.0 && s.x <= FRAME_W + 1.0,
                    "cell {p:?} at pitch_t={pitch_t} projected x = {} outside [0, {FRAME_W}]",
                    s.x
                );
                assert!(
                    s.y >= -1.0 && s.y <= FRAME_H + 1.0,
                    "cell {p:?} at pitch_t={pitch_t} projected y = {} outside [0, {FRAME_H}]",
                    s.y
                );
                // And the four cell-quad corners likewise project (back-row
                // corner-clip was the other half of the #188 framing bug).
                for corner in cell_world_corners(p, &cfg) {
                    let c = unified_project(&vp, corner, &cfg);
                    assert!(
                        c.is_some(),
                        "cell {p:?} corner {corner:?} at pitch_t={pitch_t} \
                         projected behind the camera"
                    );
                }
            }
        }
    }
}
