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

use broadside_engine::gfx;
use broadside_engine::grid::{Axis, Dir4, Facing, Pos, COLS, ROWS};
use broadside_engine::projector::{
    cell_world_center, cell_world_corners, grid_cell_quad, unified_project, unified_view_proj,
    ProjectorConfig,
};
use std::sync::{Mutex, MutexGuard};

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

/// The `cam_dist` values the cross-dial regression sweeps — `MIN` and `MAX`
/// of [`gfx::UNIFIED_CAM_DIST_MIN`]..[`gfx::UNIFIED_CAM_DIST_MAX`]. Reviewer-a
/// flagged that the #197 anchor coupling moves both eye-y and target-y
/// together as `d` changes, so the pitch/lean/centre-projection geometry
/// at the extremes is worth pinning alongside the boot value. The anchor
/// invariant test itself uses a denser sweep (`CAM_DIST_ANCHOR_SWEEP`).
const CAM_DIST_CROSS_SWEEP: [f32; 2] = [gfx::UNIFIED_CAM_DIST_MIN, gfx::UNIFIED_CAM_DIST_MAX];

/// The `cam_dist` values the anchor invariant test sweeps. Four well-spread
/// samples across `[MIN, MAX]` — endpoints + two interior points — so a
/// near-row screen-y drift at any point on the range trips the gate. Holding
/// 4 samples (not 10) keeps the test fast; the anchor is analytic so
/// intermediate values are implied if endpoints + interior land within eps.
const CAM_DIST_ANCHOR_SWEEP: [f32; 4] = [
    gfx::UNIFIED_CAM_DIST_MIN,
    4.0,
    gfx::BOOT_UNIFIED_CAM_DIST,
    gfx::UNIFIED_CAM_DIST_MAX,
];

/// Bruce's #197 anchor-invariance tolerance. The derivation is analytic but
/// the projection goes through an `f32` mat4·vec4 + a NDC→pixel scale, so a
/// 2 px slop band absorbs the float-roundoff at the extremes without making
/// the assertion meaningless (the anchor *visually* parks the near edge —
/// 2 px on a 270 px frame is < 1% drift, well below Bruce's "stays put"
/// threshold).
const ANCHOR_EPS_PX: f32 = 2.0;

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

/// Process-wide serialisation for any test that READS or WRITES the live
/// `gfx::unified_cam_dist` atomic. Cargo runs `#[test]`s in parallel within a
/// binary; without this, a test that pins `cam_dist` to `MIN` could race a
/// test that reads the default and flake. Every `#[test]` in this file
/// acquires this mutex via [`CamDistGuard`] (even the ones that don't touch
/// the atomic deliberately — they implicitly depend on it being the boot
/// value, which a parallel test could violate).
static CAM_DIST_TEST_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that (1) takes the [`CAM_DIST_TEST_LOCK`] mutex, (2) sets
/// `gfx::unified_cam_dist` to `target` + pins the #198 anchor mode to
/// SNAP-TO-MENU (the anchor invariant's only meaningful mode — the centred
/// mode deliberately does NOT park the near edge), and (3) on `Drop`
/// restores both to their boot values. This keeps the live atomics clean for
/// the next test even if an assertion panics mid-test.
///
/// Use [`CamDistGuard::pin_boot`] for tests that just want the boot value +
/// the serialisation lock; use [`CamDistGuard::pin`] to drive the dial
/// (anchor / cross-dial tests).
struct CamDistGuard {
    // Keep the lock alive for the lifetime of the guard. Drop order is
    // bottom-up: the lock releases AFTER we've restored the atomics.
    _lock: MutexGuard<'static, ()>,
}

impl CamDistGuard {
    fn pin_boot() -> Self {
        Self::pin(gfx::BOOT_UNIFIED_CAM_DIST)
    }

    fn pin(target: f32) -> Self {
        // Recover the lock even if a previous test panicked — a poisoned
        // mutex must not block this whole suite. `into_inner()` extracts the
        // guard regardless.
        let lock = CAM_DIST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `adjust_cam_dist(delta)` clamps + sets via `+delta`. Set to `target`
        // by computing the delta from the current value (the previous test's
        // restore should have left this at BOOT, but compute from `current`
        // so a race in a future addition can't drift us off `target`).
        let current = gfx::unified_cam_dist();
        gfx::adjust_cam_dist(target - current);
        // #198: pin the SNAP-TO-MENU anchor mode (boot default). The
        // centered-mode pose deliberately breaks the near-edge anchor
        // invariant, so a parallel test that toggled it would flake this
        // suite.
        gfx::set_anchor_mode_centered(false);
        debug_assert!(
            (gfx::unified_cam_dist() - target).abs() < 1e-3,
            "CamDistGuard::pin({target}) — atomic landed at {} (clamp range \
             [{}, {}])",
            gfx::unified_cam_dist(),
            gfx::UNIFIED_CAM_DIST_MIN,
            gfx::UNIFIED_CAM_DIST_MAX,
        );
        Self { _lock: lock }
    }
}

impl Drop for CamDistGuard {
    fn drop(&mut self) {
        let current = gfx::unified_cam_dist();
        gfx::adjust_cam_dist(gfx::BOOT_UNIFIED_CAM_DIST - current);
        gfx::set_anchor_mode_centered(false);
    }
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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
    let _g = CamDistGuard::pin_boot();
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

/// `pitch_t` values at which the in-frame smoke gate is asserted strictly.
///
/// **Why a subset of [`PITCH_T_SWEEP`].** `pitch_t = 0` is the boot
/// chase-cam (Bruce's default playtime regime); `pitch_t = 0.5` is the mid-
/// arc the `G` key passes through. Both project the whole board inside the
/// 480×270 frame at the live cell-scale defaults.
///
/// **Why `pitch_t = 1.0` is INTENTIONALLY excluded** (#206 lead ruling,
/// 2026-06-29). Bruce ratified a wide cell-scale default (#206 brings
/// `BOOT_UNIFIED_GRID_CELL_SCALE` to 1.90 — a wider board). At full G
/// (`pitch_t = 1.0`, the debug top-down) the wider board's far row
/// overflows the top of the frame (cell(0, 0) projects to screen-y ≈ -25
/// at the post-#206 defaults). That's an ACCEPTED cross-dial limitation —
/// `unified_target_y_anchored` already documents that the anchor coupling
/// only holds exactly at `pitch_t = 0 + cell_scale = 1.0`; #200 extends
/// that to the dial-stacking note. Full-G is a debug-key inspection mode,
/// not a gameplay default, so a wide-board hull peeking above the top edge
/// at full top-down is a known cosmetic — NOT a regression to guard.
///
/// What still holds at `pitch_t = 1.0`: see
/// [`unified_projection_returns_finite_screen_y_at_full_top_down`] below —
/// `unified_project` MUST still return `Some(finite)` (no behind-camera,
/// no NaN), just outside the in-frame window.
const PITCH_T_IN_FRAME_REGIME: [f32; 2] = [0.0, 0.5];

#[test]
fn unified_projection_initialises_and_projects_every_cell_in_frame() {
    let _g = CamDistGuard::pin_boot();
    // Smoke gate: at the gameplay-pitch regime (boot + mid-arc), every
    // cell's world centre projects to a finite screen point in
    // [0, frame_w] × [0, frame_h]. Catches the class of "near-row hull
    // lands behind the camera" / "back-row hull off the top of the frame"
    // bugs that were the #188 symptom. Skips full-G (pitch_t = 1.0); see
    // [`PITCH_T_IN_FRAME_REGIME`] for the cross-dial rationale.
    for &pitch_t in &PITCH_T_IN_FRAME_REGIME {
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

#[test]
fn unified_projection_returns_finite_screen_y_at_full_top_down() {
    let _g = CamDistGuard::pin_boot();
    // Companion to the in-frame smoke: at full G (pitch_t = 1.0, the debug
    // top-down key) the wide-cell-scale default (#206) intentionally lets
    // the far row overflow the frame top — that's a documented cross-dial
    // limitation (see [`PITCH_T_IN_FRAME_REGIME`]). But the projection
    // itself must still be MATHEMATICALLY sound — no behind-camera, no
    // NaN/inf — so a regression that broke the unified path at the full
    // top-down extreme would surface here even if the cosmetic in-frame
    // gate doesn't apply. The bar is "the math doesn't blow up," not "the
    // hull fits the window."
    let cfg = unified_cfg(1.0);
    let vp = unified_view_proj(&cfg);
    for row in 0..ROWS {
        for col in 0..COLS {
            let p = Pos::new(col, row);
            let world = cell_world_center(p, &cfg);
            let s = unified_project(&vp, world, &cfg)
                .unwrap_or_else(|| panic!("cell {p:?} at pitch_t=1.0 projected behind the camera"));
            assert!(
                s.x.is_finite() && s.y.is_finite(),
                "cell {p:?} at pitch_t=1.0 projected to non-finite ({}, {})",
                s.x,
                s.y,
            );
            for corner in cell_world_corners(p, &cfg) {
                let c = unified_project(&vp, corner, &cfg);
                assert!(
                    c.is_some(),
                    "cell {p:?} corner {corner:?} at pitch_t=1.0 projected behind the camera",
                );
                let pt = c.expect("checked Some above");
                assert!(
                    pt.x.is_finite() && pt.y.is_finite(),
                    "cell {p:?} corner {corner:?} at pitch_t=1.0 projected to non-finite \
                     ({}, {})",
                    pt.x,
                    pt.y,
                );
            }
        }
    }
}

/* =========================================================================
 * (d) #197 ANCHOR INVARIANT — near-row screen-y is constant across cam_dist
 * ====================================================================== */

/// Returns the projected screen-y of the near-row cell centre at column
/// `anchor_col` when the live `cam_dist` is pinned to `target`. The anchor
/// coupling is implemented analytically inside
/// `projector::unified_target_y_anchored` (which `unified_eye` /
/// `unified_target` read internally), so this exercises the entire
/// `unified_view_proj` path the renderer uses, not an isolated helper.
///
/// Pinned at the anchor's derivation regime: `pitch_t = 0` (default
/// chase-cam) + the boot grid cell scale = `1.0` (the projector's anchor
/// derivation explicitly assumes both; see the doc on
/// `unified_target_y_anchored`).
fn near_row_screen_y_at_cam_dist(target: f32, anchor_col: usize) -> f32 {
    let _g = CamDistGuard::pin(target);
    let cfg = unified_cfg(0.0);
    let vp = unified_view_proj(&cfg);
    let near_pos = Pos::new(anchor_col, ROWS - 1);
    unified_project(&vp, cell_world_center(near_pos, &cfg), &cfg)
        .expect("near-row centre projects under unified camera")
        .y
}

#[test]
fn near_row_screen_y_anchors_across_cam_dist_sweep_at_centre_column() {
    // Centre column (col = 2) is the reference: zero per-column lean, so the
    // anchor coupling shows as a clean "near cell-centre screen-y is
    // constant" datum. Compute against the BOOT cam_dist (the anchor's
    // reference point), then check every sweep value is within eps of that.
    let anchor_col = COLS / 2;
    let baseline = near_row_screen_y_at_cam_dist(gfx::BOOT_UNIFIED_CAM_DIST, anchor_col);
    for &cam_dist in &CAM_DIST_ANCHOR_SWEEP {
        let y = near_row_screen_y_at_cam_dist(cam_dist, anchor_col);
        let drift = (y - baseline).abs();
        assert!(
            drift < ANCHOR_EPS_PX,
            "anchor drift at cam_dist={cam_dist}: near-row screen-y = {y}, \
             baseline (cam_dist={baseline_d}) = {baseline}, |Δ| = {drift} > {ANCHOR_EPS_PX} px",
            baseline_d = gfx::BOOT_UNIFIED_CAM_DIST,
        );
    }
}

#[test]
fn near_row_screen_y_anchors_across_cam_dist_sweep_at_edge_columns() {
    // Edge columns (0, 4) also have per-column lean under unified, but the
    // anchor invariant is the screen-y of THIS CELL'S centre, not the column
    // tangent — the centre of an edge near-row cell is still a screen point
    // that must stay parked as zoom changes (Bruce's eyeball check is "the
    // bottom row doesn't slide under the menu"; that's literally screen-y).
    for &anchor_col in &[0_usize, COLS - 1] {
        let baseline = near_row_screen_y_at_cam_dist(gfx::BOOT_UNIFIED_CAM_DIST, anchor_col);
        for &cam_dist in &CAM_DIST_ANCHOR_SWEEP {
            let y = near_row_screen_y_at_cam_dist(cam_dist, anchor_col);
            let drift = (y - baseline).abs();
            assert!(
                drift < ANCHOR_EPS_PX,
                "anchor drift at edge col={anchor_col} cam_dist={cam_dist}: near-row \
                 screen-y = {y}, baseline (cam_dist={baseline_d}) = {baseline}, \
                 |Δ| = {drift} > {ANCHOR_EPS_PX} px",
                baseline_d = gfx::BOOT_UNIFIED_CAM_DIST,
            );
        }
    }
}

/* =========================================================================
 * (e) CROSS-DIAL — lean + cell-centre hold at cam_dist extremes
 * ====================================================================== */

#[test]
fn bow_north_lean_holds_at_cam_dist_min_and_max() {
    // Reviewer-a flagged: the #197 anchor coupling moves both target_y AND
    // eye-y as `d` slides, so the pitch geometry could subtly drift at the
    // [3.5, 7.0] extremes even though the lean math is theoretically
    // distance-invariant. Pin it explicitly — but only at the anchor's
    // derivation regime (pitch_t = 0; the projector doc on
    // `unified_target_y_anchored` flags that at non-zero pitch the anchor
    // drifts, which can push the camera close enough to the ground at
    // min cam_dist that off-centre cells project BEHIND the camera —
    // documented limitation, intentional, covered by the existing
    // pitch-sweep tests at the default cam_dist).
    let row = ROWS - 1;
    let facing = Facing::Bow(Dir4::N);
    for &cam_dist in &CAM_DIST_CROSS_SWEEP {
        let _g = CamDistGuard::pin(cam_dist);
        let cfg = unified_cfg(0.0);
        let vp = unified_view_proj(&cfg);
        for &col in &SAMPLED_COLS {
            assert_hull_parallel_to_lane(
                &vp,
                &cfg,
                Pos::new(col, row),
                facing,
                1e-3,
                &format!("Bow(N) col={col} row={row} cam_dist={cam_dist}"),
            );
        }
    }
}

#[test]
fn cell_center_alignment_holds_at_cam_dist_min_and_max() {
    // The (b) invariant — projected cell_world_center == grid_cell_quad.center
    // — should be distance-independent (both sides go through the same
    // view_proj), but the same anchor-coupling concern applies. Same
    // pitch_t = 0 restriction as the lean cross-dial test (see its comment
    // for the documented-limitation rationale).
    for &cam_dist in &CAM_DIST_CROSS_SWEEP {
        let _g = CamDistGuard::pin(cam_dist);
        let cfg = unified_cfg(0.0);
        let vp = unified_view_proj(&cfg);
        for &row in &[0_usize, ROWS - 1] {
            for col in 0..COLS {
                assert_cell_center_alignment(
                    &vp,
                    &cfg,
                    Pos::new(col, row),
                    1e-3,
                    &format!("col={col} row={row} cam_dist={cam_dist}"),
                );
            }
        }
    }
}

/// (#213 item 4 / #199b) Variable-board render regression lock: the bin chains
/// `.with_dims(board.dims())` on the per-frame scene cfg so a non-5x4 encounter
/// lays out at its rolled shape. This test pins the invariant by walking the
/// full #199b dims pool and asserting, on each shape, that EVERY in-bounds
/// `Pos` projects to a point inside the viewport — `grid_cell_quad(pos, cfg)
/// .center` for the centres + `cell_world_corners(pos, cfg)` for the corners.
/// If a future change reverts the renderer to compile-time COLS/ROWS (the bug
/// fixed in 4619b10), the wrong-dim layout pushes cells off-frame and the
/// assertion fires at the offending shape.
///
/// We sample the full pool — `{2x2, 2x3, 3x2, 2x4, 4x2, 3x3, 3x4, 4x3, 4x4,
/// 5x4}` — instead of just one non-5x4 case, because the projector's column
/// fan + row depth math are independent surfaces and a regression could break
/// one without the other. The 5x4 case is the existing canonical path (the
/// #188 lean + cell-centre guards above already cover it richly) — included
/// here only so this single test fails consistently if `with_dims` itself
/// regressed to a no-op.
#[test]
fn variable_dims_grid_lays_out_in_viewport() {
    let _g = CamDistGuard::pin_boot();
    // #199b pool — matches runs::VARIABLE_ENCOUNTER_DIMS_POOL.
    const POOL: &[(usize, usize)] = &[
        (2, 2),
        (2, 3),
        (3, 2),
        (2, 4),
        (4, 2),
        (3, 3),
        (3, 4),
        (4, 3),
        (4, 4),
        (5, 4),
    ];
    for &(cols, rows) in POOL {
        let cfg = unified_cfg(0.0).with_dims(cols, rows);
        // (a) cfg actually carries the override (not a no-op).
        assert_eq!(
            cfg.cols, cols,
            "with_dims must set cfg.cols (shape {cols}x{rows})",
        );
        assert_eq!(
            cfg.rows, rows,
            "with_dims must set cfg.rows (shape {cols}x{rows})",
        );
        let vp = unified_view_proj(&cfg);
        // (b) every in-bounds cell's CENTRE projects inside the viewport.
        for row in 0..rows {
            for col in 0..cols {
                let q = grid_cell_quad(Pos::new(col, row), &cfg);
                assert!(
                    q.center[0] >= 0.0 && q.center[0] <= cfg.frame_w,
                    "cell ({col}, {row}) centre x={} OUT of [0, {}] at dims {cols}x{rows}",
                    q.center[0],
                    cfg.frame_w,
                );
                assert!(
                    q.center[1] >= 0.0 && q.center[1] <= cfg.frame_h,
                    "cell ({col}, {row}) centre y={} OUT of [0, {}] at dims {cols}x{rows}",
                    q.center[1],
                    cfg.frame_h,
                );
                // (c) every cell-world-centre projects in-frame too (the
                // ship-seating math the unified loft path actually uses).
                let world =
                    broadside_engine::projector::cell_world_center(Pos::new(col, row), &cfg);
                let p = unified_project(&vp, world, &cfg).unwrap_or_else(|| {
                    panic!(
                        "cell ({col}, {row}) world centre failed to project at dims {cols}x{rows}"
                    )
                });
                assert!(
                    p.x >= 0.0 && p.x <= cfg.frame_w,
                    "cell ({col}, {row}) world-projected x={} OUT of [0, {}] at dims {cols}x{rows}",
                    p.x,
                    cfg.frame_w,
                );
                assert!(
                    p.y >= 0.0 && p.y <= cfg.frame_h,
                    "cell ({col}, {row}) world-projected y={} OUT of [0, {}] at dims {cols}x{rows}",
                    p.y,
                    cfg.frame_h,
                );
                // (d) every world corner projects (must not return None
                // = behind camera). Edge-cell corners CAN spill past the
                // frame's x extent under the canonical near-row fan (the
                // documented "lanes run off the corners like a reference
                // road" — see projector::for_scene docs at projector.rs:298),
                // so we don't assert in-frame on corners — just that they
                // project at all (in front of the camera) so a regression
                // that flips cells behind the camera fails here.
                let corners = cell_world_corners(Pos::new(col, row), &cfg);
                for (i, w) in corners.iter().enumerate() {
                    let _ = unified_project(&vp, *w, &cfg).unwrap_or_else(|| {
                        panic!(
                            "cell ({col}, {row}) corner {i} failed to project (behind camera) at dims {cols}x{rows}",
                        )
                    });
                }
            }
        }
    }
}

/// (#213 item 4) Companion lock that a non-5x4 dims layout DIFFERS from the
/// 5x4 layout — proves the `with_dims` override actually changes the
/// projection, not just stores the values. A 3x3 board's centre cell should
/// project to a screen position that's DIFFERENT from the 5x4 (1, 1) cell
/// (its column-fan + row-depth math both shift when cols/rows change). If
/// this fires the override has been silently disconnected from the cell math.
#[test]
fn variable_dims_actually_shifts_projection_vs_5x4() {
    let _g = CamDistGuard::pin_boot();
    let base = unified_cfg(0.0);
    let three_by_three = base.with_dims(3, 3);
    let four_by_two = base.with_dims(4, 2);
    let two_by_four = base.with_dims(2, 4);
    // 5x4 centre (col=2, row=2) projects somewhere.
    let q_5x4 = grid_cell_quad(Pos::new(2, 2), &base);
    // 3x3 centre (col=1, row=1) projects somewhere distinct.
    let q_3x3 = grid_cell_quad(Pos::new(1, 1), &three_by_three);
    // 4x2 centre-ish (col=2, row=0) — wider, shorter — projects somewhere
    // distinct from the 3x3 case.
    let q_4x2 = grid_cell_quad(Pos::new(2, 0), &four_by_two);
    // 2x4 centre-ish (col=0, row=2) — narrow, tall.
    let q_2x4 = grid_cell_quad(Pos::new(0, 2), &two_by_four);
    let same = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3;
    assert!(
        !same(q_5x4.center, q_3x3.center),
        "3x3 (1,1) should not collide with 5x4 (2,2) — with_dims is a no-op",
    );
    assert!(
        !same(q_5x4.center, q_4x2.center),
        "4x2 (2,0) should not collide with 5x4 (2,2) — with_dims is a no-op",
    );
    assert!(
        !same(q_5x4.center, q_2x4.center),
        "2x4 (0,2) should not collide with 5x4 (2,2) — with_dims is a no-op",
    );
    // And the three non-5x4 shapes are mutually distinct (different orientations
    // can't map to the same screen point).
    assert!(
        !same(q_3x3.center, q_4x2.center) && !same(q_3x3.center, q_2x4.center),
        "3x3 / 4x2 / 2x4 shapes should not all map to the same screen point",
    );
}
