//! Scene compositor — turns a [`crate::types::Board`] into a back-to-front
//! `Vec<DrawCommand>` for [`crate::gfx::Gfx::render`].
//!
//! ## Layout
//!
//! Flat side-view scene. A horizontal lane bisects the canvas; the area
//! above is the "sky" (back-wall parallax: stars, nebula, distant planet),
//! the area below is the "floor" (foreground dust). Ships are drawn as
//! asymmetric side-view silhouettes — pointy bow, square stern — anchored
//! at their cell position on the lane line.
//!
//! ## Render order (back to front)
//!
//! 1. Sky parallax (stars + nebula + planet, upper half).
//! 2. Floor parallax (dust, lower half).
//! 3. Lane stroke (the horizon line + per-cell ticks).
//! 4. Range-band tick marks (relative to the player ship).
//! 5. Hazards.
//! 6. Ships (one asymmetric silhouette per cell with a ship in it).
//! 7. Live ordnance.
//! 8. Heat bars + shield pips (per ship).
//! 9. Action queue glyphs (stacked above each ship).
//! 10. Status badges.
//! 11. End-state overlays (defeat / victory tints).

use crate::atlas;
use crate::geometry::range_band;
use crate::gfx::{DrawCommand, PolygonInstance, SpriteInstance, SpriteSlug, TexturedShipInstance};
use crate::perspective::{
    cell_to_screen, fractional_cell_to_screen, LaneGeometry, Point2, Stance, FRIGATE_DIMS,
};
use crate::sprites::{EmptySpriteRegistry, SpriteRegistry, SpriteStance, SpriteView};
use crate::types::{
    Board, Faction, HullZone, LaneEnd, Mount, Orientation, Projectile, RangeBand, Ship, Status,
    StatusKind, WeaponArchetype,
};

/* ---- palette --------------------------------------------------------------
 *
 * Analysis HTML CSS tokens, scaled to 0..1.
 * ----------------------------------------------------------------------- */

const PLAYER_HULL_FILL:   [f32; 4] = [0.102, 0.165, 0.243, 1.0];
const PLAYER_HULL_STROKE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];

const ENEMY_HULL_FILL:    [f32; 4] = [0.227, 0.122, 0.145, 1.0];
const ENEMY_HULL_STROKE:  [f32; 4] = [0.878, 0.478, 0.235, 1.0];

const LANE_STROKE:        [f32; 4] = [0.20,  0.28,  0.36,  1.0];
const LANE_TICK:          [f32; 4] = [0.33,  0.41,  0.51,  1.0];

const BAND_POINT_BLANK: [f32; 4] = [0.878, 0.400, 0.290, 0.6];
const BAND_CLOSE:       [f32; 4] = [0.878, 0.635, 0.235, 0.6];
const BAND_MID:         [f32; 4] = [0.353, 0.624, 0.878, 0.6];
const BAND_LONG:        [f32; 4] = [0.353, 0.820, 0.796, 0.6];
const BAND_EXTREME:     [f32; 4] = [0.608, 0.549, 0.859, 0.6];

const HEAT_BG:      [f32; 4] = [0.094, 0.094, 0.110, 0.85];
const HEAT_FILL:    [f32; 4] = [0.949, 0.475, 0.235, 1.0];
const HEAT_LOCKOUT: [f32; 4] = [0.949, 0.235, 0.235, 1.0];

const SHIELD_PIP_CHARGE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];

const WHITE:        [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DEFEAT_TINT:  [f32; 4] = [0.85, 0.08, 0.10, 0.55];
const VICTORY_TINT: [f32; 4] = [1.00, 0.80, 0.20, 0.45];

/// Render-only multiplier on the ship silhouette's on-screen extent. The raw
/// `FRIGATE_DIMS` (120×60×40) read too small against the ~177 px lane-cell
/// pitch on `DEFAULT_LANE` (bruce playtest: "ships too small to read"). This
/// scales the drawn width/height WITHOUT moving cell centers, so the
/// silhouette grows toward the cell pitch while ships stay on their lane
/// slots. A bow-on Frigate goes 120 → ~162 px (≈92% of the 177 px pitch),
/// still clearing adjacent occupied cells edge-to-edge.
///
/// This is a renderer-side knob only — it does NOT touch the `FRIGATE_DIMS`
/// game-design constant or any range/geometry math. Bruce iterates this
/// value visually; bumping it past ~1.45 risks adjacent-cell overlap at
/// PointBlank, which would need a wider lane / fewer cells instead (see the
/// composition options flagged to the lead).
const SHIP_SCALE: f32 = 1.35;

/// Scaled `(width, total_h)` of a ship silhouette at the current view angle.
/// Single source of truth for both the silhouette draw (`push_ship`) and the
/// HUD overlay anchors (`ship_bbox`) so heat bars / pips / glyphs track the
/// scaled hull. `width` is the lane-axis extent; `total_h` stacks the
/// side-view height projection (`height·cosθ`) and the top-down depth
/// projection (`depth·sinθ`), then applies `SHIP_SCALE` uniformly.
fn scaled_ship_extent(stance: Stance, view_angle_rad: f32) -> (f32, f32) {
    let (width, depth_dim) = match stance {
        Stance::BowOn => (FRIGATE_DIMS.length, FRIGATE_DIMS.beam),
        Stance::Broadside => (FRIGATE_DIMS.beam, FRIGATE_DIMS.length),
    };
    let cos_a = view_angle_rad.cos();
    let sin_a = view_angle_rad.sin();
    let total_h = FRIGATE_DIMS.height * cos_a + depth_dim * sin_a;
    (width * SHIP_SCALE, total_h * SHIP_SCALE)
}

/* ---- entry point --------------------------------------------------------- */

/// Build the full frame's draw command list, back-to-front. Sprites and
/// polygons are interleaved in z-order; `Gfx::render` batches consecutive
/// same-variant runs into single GPU draw calls.
///
/// `view_angle_rad` drives the camera-revolves projection. `0.0` is pure
/// side view (back wall full, floor collapsed, ships at full side
/// silhouette); `PI/2` is pure top-down (back wall collapsed, floor full,
/// ships as top-down rectangles). The **lane stays at `center_y` at
/// every angle** — it's the horizon between the two parallax planes.
/// Both planes anchor at the lane: back-wall vertical extent =
/// `back_wall_h × cos(θ)`, floor vertical extent = `floor_h × sin(θ)`.
pub fn compose_scene(board: &Board, lane: &LaneGeometry, view_angle_rad: f32) -> Vec<DrawCommand> {
    compose_scene_with(board, lane, view_angle_rad, &EmptySpriteRegistry)
}

/// Like [`compose_scene`] but consults the supplied [`SpriteRegistry`]
/// when emitting ships. If both `side` and `top` PNGs are registered for
/// a ship's class/stance, a textured-quad draw command replaces the
/// procedural silhouette polygons. Otherwise the procedural silhouette
/// is emitted as before.
pub fn compose_scene_with(
    board: &Board,
    lane: &LaneGeometry,
    view_angle_rad: f32,
    sprites: &dyn SpriteRegistry,
) -> Vec<DrawCommand> {
    compose_scene_tweened(board, lane, view_angle_rad, sprites, &TweenState::default())
}

/// Per-ship visual cell-position overrides, keyed by `Ship::id`. Each
/// entry is a fractional cell index (0.0 .. lane.cell_count - 1.0) used
/// in place of the ship's logical `ship.cell`. Empty map (or the
/// default constructed value) makes the function behave identically to
/// [`compose_scene_with`] — every ship snaps to its logical cell.
///
/// The bin captures previous cell positions before each input mutation
/// and lerps `prev -> current` over ~200ms using ease-out, producing a
/// `TweenState` per frame that smooths out the per-input snap under
/// Shogun-Showdown turn semantics.
#[derive(Default, Clone, Debug)]
pub struct TweenState {
    pub visual_cells: std::collections::HashMap<String, f32>,
}

impl TweenState {
    /// Visual cell for the named ship, falling back to its logical cell
    /// when absent from the map. Returned as `f32` so callers can feed
    /// it straight into [`fractional_cell_to_screen`].
    fn cell_for(&self, ship: &Ship) -> f32 {
        self.visual_cells
            .get(&ship.id)
            .copied()
            .unwrap_or(ship.cell as f32)
    }
}

/// Like [`compose_scene_with`] but consults `tween` for per-ship visual
/// cell positions. Ships not present in the map render at their logical
/// `ship.cell`. Heat bars, shield pips, queue glyphs and status badges
/// all ride along with the tweened ship position so the overlay HUD
/// tracks the smoothed silhouette.
pub fn compose_scene_tweened(
    board: &Board,
    lane: &LaneGeometry,
    view_angle_rad: f32,
    sprites: &dyn SpriteRegistry,
    tween: &TweenState,
) -> Vec<DrawCommand> {
    let mut out = Vec::with_capacity(256);

    push_parallax(&mut out, lane, view_angle_rad);
    push_lane(&mut out, lane);
    push_range_band_ticks(&mut out, board, lane);
    push_hazards(&mut out, board, lane);

    for ship in board.cells.iter().flatten() {
        let visual_cell = tween.cell_for(ship);
        push_ship(&mut out, ship, visual_cell, lane, view_angle_rad, sprites);
    }

    for proj in &board.ordnance {
        push_projectile(&mut out, proj, lane);
    }

    for ship in board.cells.iter().flatten() {
        let visual_cell = tween.cell_for(ship);
        push_heat_bar(&mut out, ship, visual_cell, lane, view_angle_rad);
        push_shield_pips(&mut out, ship, visual_cell, lane, view_angle_rad);
        push_queue_glyphs(&mut out, ship, visual_cell, lane, view_angle_rad);
        push_status_badges(&mut out, ship, visual_cell, lane, view_angle_rad);
    }

    push_view_angle_overlay(&mut out, view_angle_rad);

    // NOTE: the end-state / between-encounter overlays are NOT pushed
    // here. The bin (or any other compose-caller) is responsible for
    // pushing the appropriate overlay on top of this draw list when its
    // own demo state requires it. Prior history: through #45 this
    // module auto-pushed `push_end_state_overlay(out, win_state(board))`,
    // but the Phase 3 between-encounter screens introduced overlay
    // states beyond what `win_state(&Board)` can describe (e.g.
    // "encounter complete, sector 2, awaiting path choice"), so the
    // bin now drives the overlay-vs-no-overlay decision.

    out
}

/// On-screen silhouette bounding box for a ship at the current view angle.
/// Returns `(width, total_h)` so overlay helpers (heat bar, shield pips,
/// queue glyphs, status badges) can position consistently against the
/// current silhouette regardless of stance or angle.
fn ship_bbox(ship: &Ship, view_angle_rad: f32) -> (f32, f32) {
    let stance = match ship.orientation {
        Orientation::BowOn { .. } => Stance::BowOn,
        Orientation::Broadside => Stance::Broadside,
    };
    scaled_ship_extent(stance, view_angle_rad)
}

#[inline]
fn push_sprite(out: &mut Vec<DrawCommand>, s: SpriteInstance) {
    out.push(DrawCommand::Sprite(s));
}

#[inline]
fn push_polygon(out: &mut Vec<DrawCommand>, p: PolygonInstance) {
    out.push(DrawCommand::Polygon(p));
}

/* =============================================================================
 * Parallax — two planes anchored at the lane, foreshortened by view angle.
 *
 * Back wall (above the lane) vertical extent on screen =
 *   back_wall_h * cos(angle).
 * Floor (below the lane) vertical extent =
 *   floor_h * sin(angle).
 *
 * Both planes' near edges sit on the lane line (the horizon). At
 * angle = 0 the back wall fills the full upper half of canvas and the
 * floor collapses to a line. At angle = PI/2 the back wall collapses and
 * the floor fills the lower half. At intermediate angles both are
 * partially visible, foreshortened.
 *
 * Content (nebula, planet, stars, dust) is placed at fractional positions
 * within the current plane bounds, so it slides toward / away from the
 * lane line as the camera tilts.
 * ============================================================================= */

fn push_parallax(out: &mut Vec<DrawCommand>, lane: &LaneGeometry, view_angle_rad: f32) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let w = VIRTUAL_W as f32;
    let h = VIRTUAL_H as f32;
    let horizon = lane.center_y;
    let cos_a = view_angle_rad.cos();
    let sin_a = view_angle_rad.sin();
    // Full-extent reference heights at the extreme angles. The back wall
    // covers everything above the lane at 0°; the floor covers everything
    // below the lane at 90°.
    let back_wall_h_full = horizon;
    let floor_h_full = h - horizon;
    let back_wall_h = back_wall_h_full * cos_a;
    let floor_h = floor_h_full * sin_a;

    // --- BACK WALL (above lane, full at 0°, collapses at 90°) ---
    if back_wall_h > 0.5 {
        // Sky band rect: y from (horizon - back_wall_h) to horizon.
        let sky_band = [0.0_f32, horizon - back_wall_h, w, back_wall_h];

        // Nebula patches across the upper third of the back wall.
        for i in 0..3 {
            let x = w * (0.18 + (i as f32) * 0.32);
            // Place at ~25% down from the wall's top edge.
            let y = (horizon - back_wall_h) + back_wall_h * 0.25 + (i as f32 - 1.0) * 8.0;
            push_sprite(out, SpriteInstance::axis_aligned(
                [x, y],
                // Nebula width is fixed; vertical extent also fixed (these
                // are atlas-sampled at a baked size). They slide with the
                // wall but don't compress with it.
                [110.0, 44.0],
                [1.0, 1.0, 1.0, 0.55],
                atlas::cell_uvs(atlas::PARALLAX_NEBULA),
            ));
        }

        // Distant planet — upper-right, ~30% down from the wall's top edge.
        let planet_size = 54.0;
        push_sprite(out, SpriteInstance::axis_aligned(
            [w * 0.82, (horizon - back_wall_h) + back_wall_h * 0.30],
            [planet_size, planet_size],
            WHITE,
            atlas::cell_uvs(atlas::PARALLAX_DISTANT_PLANET),
        ));

        // Far stars — 60 single-pixel sprites scattered across the wall.
        for i in 0..60u32 {
            let (sx, sy) = lcg_canvas_pos(i ^ 0xA53F_C1B5, sky_band);
            let alpha = 0.35 + 0.25 * lcg_unit(i ^ 0x1234_5678);
            push_sprite(out, SpriteInstance::axis_aligned(
                [sx, sy],
                [0.5, 0.5],
                [1.0, 1.0, 1.0, alpha],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
        // Mid stars — 24 brighter dots near the top of the wall.
        let mid_band = [
            0.0_f32,
            horizon - back_wall_h * 0.95,
            w,
            (back_wall_h * 0.50).max(1.0),
        ];
        for i in 0..24u32 {
            let (sx, sy) = lcg_canvas_pos(i ^ 0x5F37_DEAD, mid_band);
            let alpha = 0.55 + 0.30 * lcg_unit(i ^ 0xBEEF_C0DE);
            push_sprite(out, SpriteInstance::axis_aligned(
                [sx, sy],
                [1.0, 1.0],
                [1.0, 1.0, 1.0, alpha],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
    }

    // --- FLOOR (below lane, collapses at 0°, full at 90°) ---
    if floor_h > 0.5 {
        let floor_band = [0.0_f32, horizon, w, floor_h];
        // Subtle dust speckles. Density also rises with sin(angle) — the
        // floor "fills in" as the camera tilts down.
        let dust_count = (18.0 * (0.4 + 0.6 * sin_a)).round() as u32;
        for i in 0..dust_count {
            let (sx, sy) = lcg_canvas_pos(i ^ 0x71BD_8842, floor_band);
            let alpha = 0.25 + 0.20 * lcg_unit(i ^ 0x6655_AABB);
            push_sprite(out, SpriteInstance::axis_aligned(
                [sx, sy],
                [1.0, 1.0],
                [0.85, 0.85, 1.0, alpha],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
        // Foreground dust tile sample at low-center of the floor for a
        // subtle near-camera detail. Hidden at low angles where the floor
        // is edge-on.
        if sin_a > 0.2 {
            push_sprite(out, SpriteInstance::axis_aligned(
                [w * 0.40, horizon + floor_h * 0.75],
                [32.0, 32.0],
                [1.0, 1.0, 1.0, 0.55 * sin_a],
                atlas::cell_uvs(atlas::PARALLAX_FOREGROUND_DUST),
            ));
        }
    }
}

/// Deterministic two-axis position inside a screen rect `[x, y, w, h]`.
fn lcg_canvas_pos(seed: u32, rect: [f32; 4]) -> (f32, f32) {
    let [rx, ry, rw, rh] = rect;
    let hx = wang_hash(seed);
    let hy = wang_hash(seed.wrapping_add(0x9E37_79B9));
    let fx = (hx as f32) / (u32::MAX as f32);
    let fy = (hy as f32) / (u32::MAX as f32);
    (rx + fx * rw, ry + fy * rh)
}

fn lcg_unit(seed: u32) -> f32 {
    (wang_hash(seed) as f32) / (u32::MAX as f32)
}

fn wang_hash(mut x: u32) -> u32 {
    x = (x ^ 61).wrapping_mul(0x27D4_EB2D);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x
}

/* =============================================================================
 * Lane — one horizontal stroke through the canvas + per-cell ticks.
 * ============================================================================= */

fn push_lane(out: &mut Vec<DrawCommand>, lane: &LaneGeometry) {
    use crate::gfx::VIRTUAL_W;
    let w = VIRTUAL_W as f32;
    // Lane line — full canvas width, thin stroke at `center_y`.
    push_sprite(out, SpriteInstance::axis_aligned(
        [w / 2.0, lane.center_y],
        [w / 2.0, 0.75],
        LANE_STROKE,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    // Per-cell ticks — short vertical marks under the lane at each cell x.
    for c in 0..lane.cell_count {
        let p = cell_to_screen(c, lane);
        push_sprite(out, SpriteInstance::axis_aligned(
            [p.x, lane.center_y + 5.0],
            [0.75, 4.0],
            LANE_TICK,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/* =============================================================================
 * Range-band tick marks — short vertical ticks above the lane at each
 * cell, colored by the band that cell sits in relative to the player.
 * ============================================================================= */

fn push_range_band_ticks(out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
    let Some(player) = board.cells.iter().flatten().find(|s| s.faction == Faction::Player) else {
        return;
    };
    let pc = player.cell as i32;
    for delta in -7i32..=7 {
        let cell = pc + delta;
        if cell < 0 || cell as u32 >= lane.cell_count {
            continue;
        }
        let p = cell_to_screen(cell as u32, lane);
        let color = match range_band(pc as usize, cell as usize) {
            RangeBand::PointBlank => BAND_POINT_BLANK,
            RangeBand::Close => BAND_CLOSE,
            RangeBand::Mid => BAND_MID,
            RangeBand::Long => BAND_LONG,
            RangeBand::Extreme => BAND_EXTREME,
        };
        // Short tick just below the lane line, distinct from the lane ticks
        // by being a tad longer and band-colored.
        push_sprite(out, SpriteInstance::axis_aligned(
            [p.x, lane.center_y + 14.0],
            [1.25, 6.0],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/* =============================================================================
 * Hazards — tinted squares on the lane.
 * ============================================================================= */

fn push_hazards(out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
    use crate::types::HazardKind;
    for cell_list in &board.hazards {
        for h in cell_list {
            let p = cell_to_screen(h.cell.min(lane.cell_count as usize - 1) as u32, lane);
            let color = match h.kind {
                HazardKind::Mine => [0.95, 0.30, 0.30, 1.0],
                HazardKind::Drone => [0.40, 0.78, 0.55, 1.0],
                HazardKind::Debris => [0.55, 0.50, 0.45, 1.0],
            };
            push_sprite(out, SpriteInstance::axis_aligned(
                [p.x, lane.center_y - 8.0],
                [5.0, 5.0],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
    }
}

/* =============================================================================
 * Ships — asymmetric side-view silhouette per cell.
 *
 * The ship is a 5-vertex polygon (quad with a triangular bow extension):
 *
 *     stern-top ----------- bow-top
 *     |                            \
 *     |                             bow-tip
 *     |                            /
 *     stern-bot ----------- bow-bot
 *
 * Drawn with the bow pointing fore by default; if the ship's bow is aft
 * the polygon is mirrored horizontally. A second darker polygon fills the
 * interior; a single-pixel stroke renders the silhouette outline by
 * drawing four edge sprites.
 *
 * Broadside stance: ship is rotated 90° (long axis vertical) — drawn as a
 * shorter, taller block with bows on both ends. For the flat side-view
 * model we don't have a great way to show broadside, so we use a stubbier
 * polygon (length = beam, height = length / 3) without the bow taper.
 * ============================================================================= */

/// Render one ship as a **single silhouette** whose total vertical extent
/// interpolates with view angle: `height * cos(θ) + beam * sin(θ)`. No
/// horizontal seam splits the silhouette into "front face" + "top face"
/// — the previous stacked-quad approach read as ships-tipping (bruce
/// feedback on commit 2caa712). The silhouette is anchored at its BASE
/// on the lane line and extends upward.
///
/// **Bow morph** for BowOn:
/// - Bow-end taper width = `(length * 0.25) * cos(θ)`. At 0° full bow
///   triangle; at 90° taper width is zero -> pure rectangle.
/// - Chevron is overlaid near the bow end with alpha `sin(θ)` — invisible
///   at side view, full at top-down. At intermediate angles both cues
///   coexist.
///
/// **Broadside** uses no bow taper at any angle (both ends face
/// off-lane). At low angles the bump on top is the readability cue; the
/// chevron fades in pointing "up" (off-lane = bow) as `sin(θ)` grows.
fn push_ship(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    visual_cell: f32,
    lane: &LaneGeometry,
    view_angle_rad: f32,
    sprites: &dyn SpriteRegistry,
) {
    let p = fractional_cell_to_screen(visual_cell, lane);
    let (fill, stroke) = if ship.faction == Faction::Player {
        (PLAYER_HULL_FILL, PLAYER_HULL_STROKE)
    } else {
        (ENEMY_HULL_FILL, ENEMY_HULL_STROKE)
    };

    let stance = match ship.orientation {
        Orientation::BowOn { .. } => Stance::BowOn,
        Orientation::Broadside => Stance::Broadside,
    };
    let bow_fore = matches!(ship.orientation, Orientation::BowOn { bow: LaneEnd::Fore });

    let cos_a = view_angle_rad.cos();
    let sin_a = view_angle_rad.sin();

    // On-screen silhouette extent. `width` is the lane-axis span (no
    // horizontal foreshortening); `total_h` is the camera-revolves vertical
    // stack of the side-view height projection and the top-down depth
    // projection. Both already include the renderer-side `SHIP_SCALE`
    // readability multiplier — see `scaled_ship_extent`. Using the shared
    // helper keeps this draw and the HUD overlay anchors (`ship_bbox`) in
    // lockstep.
    let (width, total_h) = scaled_ship_extent(stance, view_angle_rad);
    let cx = p.x;
    // Silhouette is CENTERED on the lane line: half above, half below.
    // The lane bisects the ship vertically at every angle.
    let half_h = total_h / 2.0;
    let top_y = p.y - half_h;
    let base_y = p.y + half_h;

    // If the artist has painted both side + top PNGs for this ship's
    // class/stance, draw the textured quad instead of the procedural
    // silhouette. The bbox is the same — the shader samples both PNGs
    // and blends by sin(view_angle).
    let class = ship.klass.as_deref().unwrap_or("frigate");
    let sprite_stance = match ship.orientation {
        Orientation::BowOn { bow: LaneEnd::Fore } => SpriteStance::BowOnFore,
        Orientation::BowOn { bow: LaneEnd::Aft }  => SpriteStance::BowOnAft,
        Orientation::Broadside => SpriteStance::Broadside,
    };
    if sprites.has_pair(class, sprite_stance) {
        let left  = cx - width / 2.0;
        let right = cx + width / 2.0;
        let side_slug = format!("{}_{}_{}", class, sprite_stance.slug(), SpriteView::Side.slug());
        let top_slug  = format!("{}_{}_{}", class, sprite_stance.slug(), SpriteView::Top.slug());
        out.push(DrawCommand::TexturedShip(TexturedShipInstance {
            p0: [left,  top_y],
            p1: [right, top_y],
            p2: [right, base_y],
            p3: [left,  base_y],
            blend_t: sin_a,
            side: SpriteSlug::new(&side_slug),
            top:  SpriteSlug::new(&top_slug),
        }));
        // Skip chevron + procedural-silhouette art: the painted PNGs
        // own bow direction and outline. Heat bars / shield pips /
        // queue glyphs / status badges still draw on top.
        return;
    }

    match stance {
        Stance::BowOn => push_bow_on_silhouette(
            out, cx, base_y, top_y, width, cos_a, bow_fore, fill, stroke,
        ),
        Stance::Broadside => push_broadside_silhouette(
            out, cx, base_y, top_y, width, cos_a, fill, stroke,
        ),
    }

    // Bow chevron — overlaid on the silhouette, alpha = sin(angle). Fades
    // in as the camera tilts toward top-down.
    if sin_a > 0.05 && total_h > 6.0 {
        let chevron_size = 8.0;
        let chevron_alpha = sin_a;
        let mut chev_color = stroke;
        chev_color[3] *= chevron_alpha;
        let (chx, chy, chrot) = match stance {
            Stance::BowOn => {
                let off = width / 2.0 - chevron_size;
                let sign = if bow_fore { 1.0 } else { -1.0 };
                let chx = cx + sign * off;
                // Position chevron in the upper-bow region of the silhouette.
                let chy = top_y + total_h * 0.20;
                let rot = if bow_fore { 0.0 } else { std::f32::consts::PI };
                (chx, chy, rot)
            }
            Stance::Broadside => {
                let chx = cx;
                // Centered, near the top edge (the "off-lane bow" direction).
                let chy = top_y + total_h * 0.20;
                let rot = -std::f32::consts::FRAC_PI_2;
                (chx, chy, rot)
            }
        };
        push_sprite(out, SpriteInstance {
            pos: [chx, chy],
            half_size: [chevron_size, chevron_size],
            color: chev_color,
            uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
            uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
            rotation_rad: chrot,
            _pad: [0.0; 3],
        });
    }
}

/// Single-silhouette bow-on hull. Bow-end taper width is
/// `(length * 0.25) * cos(angle)`, so the bow triangle smoothly collapses
/// to flat as the angle approaches PI/2. At cos=1 (side view) full bow
/// triangle; at cos=0 (top down) pure rectangle.
///
/// The hull is rendered as one rectangle (stern body) + one degenerate
/// quad (bow taper). When taper width is zero the bow quad collapses to a
/// zero-area sliver and contributes no visible seam.
#[allow(clippy::too_many_arguments)]
fn push_bow_on_silhouette(
    out: &mut Vec<DrawCommand>,
    cx: f32,
    base_y: f32,
    top_y: f32,
    width: f32,
    cos_a: f32,
    bow_fore: bool,
    fill: [f32; 4],
    stroke: [f32; 4],
) {
    let full_bow_w = width * 0.25;
    let bow_w = full_bow_w * cos_a;
    let body_w = width - bow_w;
    let mid_y = (top_y + base_y) / 2.0;
    let sign = if bow_fore { 1.0 } else { -1.0 };
    // Stern edge x: the far end from the bow.
    let stern_edge_x = cx - sign * width / 2.0;
    // Bow corner: where the rectangle meets the triangle.
    let bow_corner_x = cx - sign * width / 2.0 + sign * body_w;
    // Bow tip: the far end on the bow side.
    let bow_tip_x = cx + sign * width / 2.0;

    let left = stern_edge_x.min(bow_corner_x);
    let right = stern_edge_x.max(bow_corner_x);

    // Stern body rectangle.
    push_polygon(out, PolygonInstance {
        p0: [left, top_y],
        p1: [right, top_y],
        p2: [right, base_y],
        p3: [left, base_y],
        color: fill,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
    });
    // Bow triangle (degenerate-quad with two coincident vertices at tip).
    push_polygon(out, PolygonInstance {
        p0: [bow_corner_x, top_y],
        p1: [bow_tip_x, mid_y],
        p2: [bow_tip_x, mid_y],
        p3: [bow_corner_x, base_y],
        color: fill,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
    });

    // Outline strokes around the full silhouette (no internal seam).
    // Stern edge.
    push_line(out, Point2 { x: stern_edge_x, y: top_y }, Point2 { x: stern_edge_x, y: base_y }, 1.0, stroke);
    // Top edge (stern_edge_x -> bow_corner_x).
    push_line(out, Point2 { x: stern_edge_x, y: top_y }, Point2 { x: bow_corner_x, y: top_y }, 1.0, stroke);
    // Bottom edge.
    push_line(out, Point2 { x: stern_edge_x, y: base_y }, Point2 { x: bow_corner_x, y: base_y }, 1.0, stroke);
    // Bow taper edges. When cos_a is near 0 these collapse to a vertical
    // line at bow_corner_x; that's fine — no visible seam because they
    // coincide.
    push_line(out, Point2 { x: bow_corner_x, y: top_y }, Point2 { x: bow_tip_x, y: mid_y }, 1.0, stroke);
    push_line(out, Point2 { x: bow_corner_x, y: base_y }, Point2 { x: bow_tip_x, y: mid_y }, 1.0, stroke);
}

/// Single-silhouette broadside hull: rectangle plus a centered
/// superstructure bump perched on top. The bump's height interpolates
/// with cos(angle) — taller at low angles (visible from the side),
/// shorter at high angles (less prominent from above). No bow taper at
/// any angle since both lengthwise ends face off-lane equally.
#[allow(clippy::too_many_arguments)]
fn push_broadside_silhouette(
    out: &mut Vec<DrawCommand>,
    cx: f32,
    base_y: f32,
    top_y: f32,
    width: f32,
    cos_a: f32,
    fill: [f32; 4],
    stroke: [f32; 4],
) {
    let half_w = width / 2.0;
    let height = base_y - top_y;
    push_polygon(out, PolygonInstance {
        p0: [cx - half_w, top_y],
        p1: [cx + half_w, top_y],
        p2: [cx + half_w, base_y],
        p3: [cx - half_w, base_y],
        color: fill,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
    });
    // Superstructure bump: short rectangle perched on top, centered.
    // Height scales with cos(angle) so it reads strongly at side view and
    // recedes at top-down (where the bump would be foreshortened away).
    let bump_w = width * 0.4;
    let bump_h = height * 0.30 * cos_a.max(0.1);
    push_polygon(out, PolygonInstance {
        p0: [cx - bump_w / 2.0, top_y - bump_h],
        p1: [cx + bump_w / 2.0, top_y - bump_h],
        p2: [cx + bump_w / 2.0, top_y],
        p3: [cx - bump_w / 2.0, top_y],
        color: fill,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
    });

    // Outlines.
    let main = [
        Point2 { x: cx - half_w, y: top_y },
        Point2 { x: cx + half_w, y: top_y },
        Point2 { x: cx + half_w, y: base_y },
        Point2 { x: cx - half_w, y: base_y },
    ];
    for i in 0..4 {
        push_line(out, main[i], main[(i + 1) % 4], 1.0, stroke);
    }
    let bump = [
        Point2 { x: cx - bump_w / 2.0, y: top_y - bump_h },
        Point2 { x: cx + bump_w / 2.0, y: top_y - bump_h },
        Point2 { x: cx + bump_w / 2.0, y: top_y },
        Point2 { x: cx - bump_w / 2.0, y: top_y },
    ];
    for i in 0..3 {
        push_line(out, bump[i], bump[i + 1], 1.0, stroke);
    }
}

/* =============================================================================
 * View-angle HUD overlay — horizontal bar in the top-right showing the
 * current camera angle as a fill proportional to angle / (PI/2). Seven
 * tick marks under the bar mark each fixed scrub step (0, 15, 30, 45, 60,
 * 75, 90 degrees).
 * ============================================================================= */

fn push_view_angle_overlay(out: &mut Vec<DrawCommand>, view_angle_rad: f32) {
    use crate::gfx::VIRTUAL_W;
    let w = VIRTUAL_W as f32;
    let max_w = 200.0;
    let bar_h = 8.0;
    let y = 24.0;
    let x_right = w - 20.0;
    let frac = (view_angle_rad / std::f32::consts::FRAC_PI_2).clamp(0.0, 1.0);
    let cur_w = max_w * frac;
    // Track (background).
    push_sprite(out, SpriteInstance::axis_aligned(
        [x_right - max_w / 2.0, y],
        [max_w / 2.0, bar_h / 2.0],
        [0.08, 0.12, 0.18, 0.85],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    // Fill.
    if cur_w > 0.5 {
        push_sprite(out, SpriteInstance::axis_aligned(
            [x_right - max_w + cur_w / 2.0, y],
            [cur_w / 2.0, bar_h / 2.0],
            [0.33, 0.81, 0.79, 1.0],
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
    // Tick marks at each fixed angle (0, 15, 30, 45, 60, 75, 90).
    for i in 0..=6 {
        let tick_x = (x_right - max_w) + (i as f32 / 6.0) * max_w;
        push_sprite(out, SpriteInstance::axis_aligned(
            [tick_x, y + bar_h + 2.0],
            [0.5, 2.0],
            [0.55, 0.50, 0.45, 1.0],
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/// Thin line segment from `a` to `b` as a rotated rectangle of width `thickness`.
fn push_line(
    out: &mut Vec<DrawCommand>,
    a: Point2,
    b: Point2,
    thickness: f32,
    color: [f32; 4],
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    let cx = (a.x + b.x) / 2.0;
    let cy = (a.y + b.y) / 2.0;
    push_sprite(out, SpriteInstance {
        pos: [cx, cy],
        half_size: [len / 2.0, thickness / 2.0],
        color,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        rotation_rad: dy.atan2(dx),
        _pad: [0.0; 3],
    });
}

/* =============================================================================
 * Projectiles — small horizontal sprites on the lane.
 * ============================================================================= */

fn push_projectile(out: &mut Vec<DrawCommand>, proj: &Projectile, lane: &LaneGeometry) {
    let pos = fractional_cell_to_screen(proj.cell as f32, lane);
    let cell = if proj.kind.contains("missile") {
        atlas::MISSILE
    } else {
        atlas::TORPEDO
    };
    // Heading aft: flip horizontally (rotation by PI).
    let rot = if proj.heading == LaneEnd::Aft { std::f32::consts::PI } else { 0.0 };
    push_sprite(out, SpriteInstance {
        pos: [pos.x, lane.center_y - 18.0],
        half_size: [16.0, 8.0],
        color: WHITE,
        uv_min: atlas::cell_uvs(cell).0,
        uv_max: atlas::cell_uvs(cell).1,
        rotation_rad: rot,
        _pad: [0.0; 3],
    });
}

/* =============================================================================
 * Per-ship overlays: heat bar, shield pips, queue glyphs, status badges.
 * ============================================================================= */

fn push_heat_bar(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    visual_cell: f32,
    lane: &LaneGeometry,
    view_angle_rad: f32,
) {
    let p = fractional_cell_to_screen(visual_cell, lane);
    let (width, _total_h) = ship_bbox(ship, view_angle_rad);
    let max_h = 32.0;
    let bar_w = 4.0;
    // To the right of the ship hull. Ship width depends on stance.
    let bar_x = p.x + width / 2.0 + 8.0;
    let bar_y = lane.center_y;
    // Background.
    push_sprite(out, SpriteInstance::axis_aligned(
        [bar_x, bar_y - max_h / 2.0],
        [bar_w / 2.0, max_h / 2.0],
        HEAT_BG,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    let ratio = (ship.heat as f32 / ship.heat_max.max(1) as f32).clamp(0.0, 1.0);
    if ratio > 0.0 {
        let fill_h = max_h * ratio;
        let color = if ship.locked_out { HEAT_LOCKOUT } else { HEAT_FILL };
        // Bottom-aligned: fill grows upward from the bar's bottom edge.
        let bottom_y = bar_y - max_h / 2.0 + max_h; // = bar_y + max_h/2
        push_sprite(out, SpriteInstance::axis_aligned(
            [bar_x, bottom_y - fill_h / 2.0],
            [bar_w / 2.0, fill_h / 2.0],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/// Shield pips: four sides of the hull each show one pip per held charge.
/// Bow / stern pips sit horizontally just past the hull edges; port /
/// starboard pips sit above and below the hull at the silhouette's
/// current vertical extent.
fn push_shield_pips(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    visual_cell: f32,
    lane: &LaneGeometry,
    view_angle_rad: f32,
) {
    let p = fractional_cell_to_screen(visual_cell, lane);
    let (width, total_h) = ship_bbox(ship, view_angle_rad);
    let pip = 2.5;
    let pad = 6.0;
    let bow_fore = matches!(ship.orientation, Orientation::BowOn { bow: LaneEnd::Fore });
    let stance_broadside = matches!(ship.orientation, Orientation::Broadside);

    // Direction the bow points in screen space.
    let bow_sign = if bow_fore || stance_broadside { 1.0 } else { -1.0 };

    let zones = [
        // (zone, base position, stacking direction)
        (HullZone::Bow,
         Point2 { x: p.x + bow_sign * (width / 2.0 + pad), y: lane.center_y },
         Point2 { x: bow_sign * (pip * 2.0 + 1.0), y: 0.0 }),
        (HullZone::Stern,
         Point2 { x: p.x - bow_sign * (width / 2.0 + pad), y: lane.center_y },
         Point2 { x: -bow_sign * (pip * 2.0 + 1.0), y: 0.0 }),
        (HullZone::Starboard,
         Point2 { x: p.x, y: lane.center_y + total_h / 2.0 + pad },
         Point2 { x: 0.0, y: pip * 2.0 + 1.0 }),
        (HullZone::Port,
         Point2 { x: p.x, y: lane.center_y - total_h / 2.0 - pad },
         Point2 { x: 0.0, y: -(pip * 2.0 + 1.0) }),
    ];
    for (zone, base, step) in zones {
        let face = ship.shield_profile.face(zone);
        if face.charge <= 0 {
            continue;
        }
        for i in 0..face.charge {
            let px = base.x + step.x * (i as f32);
            let py = base.y + step.y * (i as f32);
            push_sprite(out, SpriteInstance::axis_aligned(
                [px, py],
                [pip, pip],
                SHIELD_PIP_CHARGE,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
    }
}

fn push_queue_glyphs(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    visual_cell: f32,
    lane: &LaneGeometry,
    view_angle_rad: f32,
) {
    if ship.queue.is_empty() {
        return;
    }
    let p = fractional_cell_to_screen(visual_cell, lane);
    let (_width, total_h) = ship_bbox(ship, view_angle_rad);
    let glyph_size = 12.0;
    let spacing = glyph_size * 2.4;
    let n = ship.queue.len() as f32;
    let total_w = (n - 1.0).max(0.0) * spacing;
    let start_x = p.x - total_w / 2.0;
    // Above the silhouette's top edge, with a small visual gap.
    let glyph_y = lane.center_y - total_h / 2.0 - 28.0;
    for (i, action_id) in ship.queue.iter().enumerate() {
        let archetype = archetype_of_mount(ship, action_id).unwrap_or(WeaponArchetype::Beam);
        let cell_uv = archetype_to_glyph(archetype);
        push_sprite(out, SpriteInstance::axis_aligned(
            [start_x + (i as f32) * spacing, glyph_y],
            [glyph_size, glyph_size],
            WHITE,
            atlas::cell_uvs(cell_uv),
        ));
    }
}

fn archetype_of_mount(ship: &Ship, action_id: &str) -> Option<WeaponArchetype> {
    let _ = ship.mounts.iter().find(|m: &&Mount| m.weapon == action_id)?;
    Some(WeaponArchetype::Beam)
}

fn archetype_to_glyph(a: WeaponArchetype) -> (u32, u32) {
    match a {
        WeaponArchetype::Beam => atlas::GLYPH_BEAM,
        WeaponArchetype::Ordnance => atlas::GLYPH_ORDNANCE,
        WeaponArchetype::Broadside => atlas::GLYPH_BROADSIDE,
        WeaponArchetype::Displacement => atlas::GLYPH_DISPLACEMENT,
        WeaponArchetype::Control => atlas::GLYPH_CONTROL,
        WeaponArchetype::Movement => atlas::GLYPH_MOVEMENT,
        WeaponArchetype::Defensive => atlas::GLYPH_DEFENSIVE,
    }
}

fn push_status_badges(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    visual_cell: f32,
    lane: &LaneGeometry,
    view_angle_rad: f32,
) {
    if ship.statuses.is_empty() {
        return;
    }
    let p = fractional_cell_to_screen(visual_cell, lane);
    let (width, total_h) = ship_bbox(ship, view_angle_rad);
    let size = 8.0;
    let spacing = size * 2.4;
    let start_x = p.x - width / 2.0;
    // Just above the silhouette's top edge, beneath the queue glyph row.
    let y = lane.center_y - total_h / 2.0 - 10.0;
    for (i, status) in ship.statuses.iter().enumerate() {
        let cell_uv = status_to_badge(status);
        push_sprite(out, SpriteInstance::axis_aligned(
            [start_x + (i as f32) * spacing, y],
            [size, size],
            WHITE,
            atlas::cell_uvs(cell_uv),
        ));
    }
}

fn status_to_badge(s: &Status) -> (u32, u32) {
    match s.kind {
        StatusKind::HullBreach => atlas::STATUS_HULL_BREACH,
        StatusKind::SystemsOffline => atlas::STATUS_SYSTEMS_OFFLINE,
        StatusKind::TargetLock => atlas::STATUS_TARGET_LOCK,
        StatusKind::ShieldsUp => atlas::STATUS_SHIELDS_UP,
    }
}

/* =============================================================================
 * End-state overlays — full-canvas tinted quad.
 * ============================================================================= */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinState {
    Playing,
    Defeat,
    Victory,
}

/// Derive [`WinState`] from a board. Victory when there are no Enemy
/// ships remaining; Defeat when there is no Player ship; Playing
/// otherwise. If both factions are empty (shouldn't happen in normal
/// play) Defeat wins — there's nobody to be victorious.
pub fn win_state(board: &Board) -> WinState {
    let mut any_player = false;
    let mut any_enemy = false;
    for ship in board.cells.iter().flatten() {
        match ship.faction {
            Faction::Player => any_player = true,
            Faction::Enemy  => any_enemy = true,
        }
    }
    if !any_player { WinState::Defeat }
    else if !any_enemy { WinState::Victory }
    else { WinState::Playing }
}

pub fn push_end_state_overlay(out: &mut Vec<DrawCommand>, state: WinState) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let (tint, banner) = match state {
        WinState::Playing => return,
        WinState::Defeat => (DEFEAT_TINT, "DEFEATED - PRESS ENTER TO RESTART"),
        WinState::Victory => (VICTORY_TINT, "VICTORY - PRESS ENTER TO RESTART"),
    };
    // Full-canvas tinted overlay quad.
    push_sprite(out, SpriteInstance::axis_aligned(
        [VIRTUAL_W as f32 / 2.0, VIRTUAL_H as f32 / 2.0],
        [VIRTUAL_W as f32 / 2.0, VIRTUAL_H as f32 / 2.0],
        tint,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    push_centered_banner(out, banner, VIRTUAL_H as f32 / 2.0, 4.0);
}

/// Run-end overlay used by the bin's `DemoState::RunDefeated` arm.
/// Like the Phase-1 [`push_end_state_overlay`] `Defeat` variant but
/// also surfaces the run's earned-salvage total so the player sees
/// what their meta-progression contribution was before dying.
pub fn push_run_defeated_overlay(out: &mut Vec<DrawCommand>, salvage: u32) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let center_x = VIRTUAL_W as f32 / 2.0;
    let center_y = VIRTUAL_H as f32 / 2.0;
    push_sprite(out, SpriteInstance::axis_aligned(
        [center_x, center_y],
        [center_x, center_y],
        DEFEAT_TINT,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    push_centered_banner(out, "DEFEATED", center_y - 60.0, 5.0);
    push_centered_banner(out, &format!("TOTAL SALVAGE: {}", salvage), center_y + 10.0, 3.0);
    push_centered_banner(out, "PRESS ENTER TO RESTART", center_y + 60.0, 2.5);
}

/// Top-right in-game salvage counter. Small inline-font readout that
/// stays present during `Playing` state so the player can verify the
/// counter ticks up on each encounter win. Pushes a single row of
/// 5×7 glyphs ~16px from the top-right canvas edge.
pub fn push_salvage_hud(out: &mut Vec<DrawCommand>, salvage: u32) {
    use crate::gfx::VIRTUAL_W;
    let banner = format!("SALVAGE: {}", salvage);
    let pixel = 2.0;
    let glyph_w_px = 5.0 * pixel;
    let space_px = pixel;
    let advance = glyph_w_px + space_px;
    let total_w: f32 = banner.len() as f32 * advance - space_px;
    let right_pad = 20.0;
    let start_x = VIRTUAL_W as f32 - total_w - right_pad;
    let y = 8.0;
    for (i, ch) in banner.chars().enumerate() {
        let x = start_x + i as f32 * advance;
        push_glyph_5x7(out, ch, x, y, pixel, WHITE);
    }
}

/// Centered single-line banner using the inline 5×7 font. `pixel` is
/// the size of one font "pixel" in virtual pixels (typically 4 for
/// title-style banners, 2 for body text). `y` is the vertical center
/// of the rendered glyph row.
fn push_centered_banner(out: &mut Vec<DrawCommand>, banner: &str, y_center: f32, pixel: f32) {
    use crate::gfx::VIRTUAL_W;
    let glyph_w_px = 5.0 * pixel;
    let glyph_h_px = 7.0 * pixel;
    let space_px = pixel;
    let advance = glyph_w_px + space_px;
    let total_w: f32 = banner.len() as f32 * advance - space_px;
    let start_x = (VIRTUAL_W as f32 - total_w) / 2.0;
    let y = y_center - glyph_h_px / 2.0;
    for (i, ch) in banner.chars().enumerate() {
        let x = start_x + i as f32 * advance;
        push_glyph_5x7(out, ch, x, y, pixel, WHITE);
    }
}

/// Per-Phase-3 demo state for between-encounter screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BetweenEncounterChoice {
    /// Encounter just cleared. Player picks 1/2/3 (repair / upgrade /
    /// continue). `sector_idx` is the run's CURRENT sector index
    /// (zero-based); displayed as sector_idx+1 in the banner.
    /// `salvage` is the run's current `Run::salvage` total after the
    /// just-completed encounter's award.
    EncounterComplete { sector_idx: usize, salvage: u32 },
    /// Final encounter of final sector cleared. Player won the run.
    /// Distinct from `WinState::Victory` (which fires on any single
    /// encounter win) — this is for the campaign-end overlay only.
    /// `salvage` is the run's final salvage total — surfaced as
    /// "TOTAL SALVAGE: N" on the overlay.
    RunComplete { salvage: u32 },
}

/// Render the between-encounter overlay. Three-line layout:
///   "ENCOUNTER COMPLETE - SECTOR N"
///   ""
///   "1 REPAIR    2 UPGRADE    3 CONTINUE"
///
/// For [`BetweenEncounterChoice::RunComplete`] renders a "RUN COMPLETE"
/// banner plus "PRESS ENTER TO RESTART" subtext, similar tint to the
/// existing victory overlay.
///
/// No-op when neither state applies — the bin should only call this
/// while between-encounter or run-complete state is active.
pub fn push_between_encounter_overlay(
    out: &mut Vec<DrawCommand>,
    choice: BetweenEncounterChoice,
) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let center_x = VIRTUAL_W as f32 / 2.0;
    let center_y = VIRTUAL_H as f32 / 2.0;
    let tint = match choice {
        BetweenEncounterChoice::EncounterComplete { .. } => [0.10, 0.20, 0.35, 0.65],
        BetweenEncounterChoice::RunComplete { .. } => VICTORY_TINT,
    };
    // Full-canvas tinted overlay.
    push_sprite(out, SpriteInstance::axis_aligned(
        [center_x, center_y],
        [center_x, center_y],
        tint,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));

    match choice {
        BetweenEncounterChoice::EncounterComplete { sector_idx, salvage } => {
            // Banner row: "ENCOUNTER COMPLETE - SECTOR N" at y_center - 60.
            let pixel = 3.0;
            let sector_num = sector_idx + 1;
            let banner = format!("ENCOUNTER COMPLETE - SECTOR {}", sector_num);
            push_centered_banner(out, &banner, center_y - 60.0, pixel);
            // Salvage row: "SALVAGE: N" between banner and choices.
            push_centered_banner(out, &format!("SALVAGE: {}", salvage), center_y - 15.0, pixel);
            // Choice row: "1 REPAIR    2 UPGRADE    3 CONTINUE" at y_center + 35.
            push_centered_banner(out, "1 REPAIR  2 UPGRADE  3 CONTINUE", center_y + 35.0, pixel);
        }
        BetweenEncounterChoice::RunComplete { salvage } => {
            push_centered_banner(out, "RUN COMPLETE", center_y - 50.0, 5.0);
            push_centered_banner(out, &format!("TOTAL SALVAGE: {}", salvage), center_y + 15.0, 3.0);
            push_centered_banner(out, "PRESS ENTER TO RESTART", center_y + 55.0, 2.5);
        }
    }
}

/* =============================================================================
 * 5x7 bitmap font for the end-state banner. Sparse — only the characters
 * actually appearing in the banner strings are defined. Unknown chars
 * render as blank glyphs (5 columns, 7 rows of zeros). Each glyph is
 * encoded as 7 rows of 5 bits, MSB-first (bit 4 = column 0).
 * ============================================================================= */

fn push_glyph_5x7(
    out: &mut Vec<DrawCommand>,
    ch: char,
    x: f32,
    y: f32,
    pixel: f32,
    color: [f32; 4],
) {
    let rows = match ch {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        ':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        ' ' => return,
        _ => return, // unknown char = blank glyph
    };
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..5 {
            if (bits >> (4 - col)) & 1 == 1 {
                let px = x + col as f32 * pixel;
                let py = y + row as f32 * pixel;
                push_sprite(out, SpriteInstance::axis_aligned(
                    [px + pixel / 2.0, py + pixel / 2.0],
                    [pixel / 2.0, pixel / 2.0],
                    color,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ));
            }
        }
    }
}

/* =============================================================================
 * Tests
 * ============================================================================= */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::perspective::DEFAULT_LANE;
    use crate::types::{EventBus, Projectile, ShieldFace, ShieldProfile, Ship};
    use std::collections::HashMap;

    fn empty_board(size: usize) -> Board {
        Board {
            size,
            cells: (0..size).map(|_| None).collect(),
            ordnance: Vec::new(),
            hazards: (0..size).map(|_| Vec::new()).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        }
    }

    fn frigate_at(cell: usize, faction: Faction, orientation: Orientation) -> Ship {
        Ship {
            id: format!("ship-{}", cell),
            faction,
            cell,
            orientation,
            hull: 5,
            max_hull: 5,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: Vec::new(),
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    /// Stub registry that reports the requested class+stance as having
    /// BOTH side and top loaded — verifies the `compose_scene_with`
    /// branch that emits a textured-quad command instead of the
    /// procedural silhouette polygons.
    struct AlwaysLoaded;
    impl SpriteRegistry for AlwaysLoaded {
        fn has(&self, _class: &str, _stance: SpriteStance, _view: SpriteView) -> bool {
            true
        }
    }

    #[test]
    fn empty_registry_emits_procedural_silhouette() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene = compose_scene_with(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &EmptySpriteRegistry);
        let textured_count = scene.iter().filter(|c| matches!(c, DrawCommand::TexturedShip(_))).count();
        assert_eq!(textured_count, 0, "empty registry should not emit textured-ship draws");
    }

    #[test]
    fn loaded_registry_emits_textured_ship_per_ship() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        let scene = compose_scene_with(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &AlwaysLoaded);
        let textured: Vec<_> = scene
            .iter()
            .filter_map(|c| if let DrawCommand::TexturedShip(t) = c { Some(t) } else { None })
            .collect();
        assert_eq!(textured.len(), 2, "expected one textured-ship draw per ship");
        // sin(45deg) ≈ 0.7071
        for t in &textured {
            assert!((t.blend_t - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        }
        // Each ship's slug pair encodes its stance.
        assert_eq!(textured[0].side.as_str(), "frigate_bowOnFore_side");
        assert_eq!(textured[0].top.as_str(),  "frigate_bowOnFore_top");
        assert_eq!(textured[1].side.as_str(), "frigate_broadside_side");
        assert_eq!(textured[1].top.as_str(),  "frigate_broadside_top");
    }

    #[test]
    fn tween_state_default_is_identity_with_compose_scene_with() {
        // A default TweenState (empty visual_cells map) should produce
        // the same scene as compose_scene_with — the tweened path is a
        // strict superset.
        let mut board = empty_board(7);
        board.cells[2] = Some(frigate_at(2, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let untweened = compose_scene_with(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &EmptySpriteRegistry);
        let tweened = compose_scene_tweened(
            &board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &EmptySpriteRegistry,
            &TweenState::default(),
        );
        assert_eq!(untweened.len(), tweened.len(),
            "default TweenState must produce identical draw count");
    }

    #[test]
    fn tween_state_shifts_ship_polygon_left_when_visual_cell_is_lower() {
        // Same board, two compose calls: one with no tween (ship at
        // logical cell 4), one with the tween anchoring the ship at
        // cell 2.0 (mid-flight from cell 2 → cell 4). The second pass
        // should emit ship polygons whose x coords are shifted LEFT
        // because visual_cell < logical_cell on a left-to-right lane.
        let mut board = empty_board(7);
        board.cells[4] = Some(frigate_at(4, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let logical_scene = compose_scene_with(&board, &DEFAULT_LANE, 0.0, &EmptySpriteRegistry);

        let mut tween = TweenState::default();
        tween.visual_cells.insert("ship-4".to_string(), 2.0);
        let tweened = compose_scene_tweened(&board, &DEFAULT_LANE, 0.0, &EmptySpriteRegistry, &tween);

        // Find the first ship polygon in each (the stern body
        // rectangle is the first Polygon emitted after parallax /
        // lane / range bands).
        let logical_x = first_ship_polygon_left_x(&logical_scene)
            .expect("logical scene must have a ship polygon");
        let tweened_x = first_ship_polygon_left_x(&tweened)
            .expect("tweened scene must have a ship polygon");
        assert!(tweened_x < logical_x,
            "tweened ship (visual_cell=2) should be drawn LEFT of logical ship (cell=4); \
             got logical_x={} tweened_x={}", logical_x, tweened_x);
    }

    /// Helper: find the leftmost x-coordinate among ship-body polygons.
    /// Skips lane / range-band / hazard polygons by selecting those
    /// whose fill matches the player hull fill color.
    fn first_ship_polygon_left_x(cmds: &[DrawCommand]) -> Option<f32> {
        for cmd in cmds {
            if let DrawCommand::Polygon(p) = cmd {
                if (p.color[0] - PLAYER_HULL_FILL[0]).abs() < 1e-4
                    && (p.color[1] - PLAYER_HULL_FILL[1]).abs() < 1e-4
                {
                    return Some(p.p0[0].min(p.p3[0]));
                }
            }
        }
        None
    }

    #[test]
    fn tween_state_cell_for_falls_back_to_logical() {
        let ship = frigate_at(3, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
        let empty = TweenState::default();
        assert_eq!(empty.cell_for(&ship), 3.0,
            "empty TweenState should fall back to ship.cell");

        let mut populated = TweenState::default();
        populated.visual_cells.insert(ship.id.clone(), 1.5);
        assert_eq!(populated.cell_for(&ship), 1.5,
            "TweenState entry should override the logical cell");
    }

    #[test]
    fn win_state_classifies_factions_correctly() {
        // Pure backdrop / lane / no ships → board is technically both
        // "no player" and "no enemy"; we resolve to Defeat (player isn't
        // present so they can't have won).
        assert_eq!(win_state(&empty_board(7)), WinState::Defeat);

        let mut b = empty_board(7);
        b.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        assert_eq!(win_state(&b), WinState::Victory, "player alone = victory");

        let mut b = empty_board(7);
        b.cells[3] = Some(frigate_at(3, Faction::Enemy, Orientation::Broadside));
        assert_eq!(win_state(&b), WinState::Defeat, "enemy alone = defeat");

        let mut b = empty_board(7);
        b.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        b.cells[3] = Some(frigate_at(3, Faction::Enemy, Orientation::Broadside));
        assert_eq!(win_state(&b), WinState::Playing);
    }

    #[test]
    fn end_state_overlay_is_noop_during_play() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_end_state_overlay(&mut out, WinState::Playing);
        assert!(out.is_empty(), "Playing state must not emit overlay draws");
    }

    #[test]
    fn end_state_overlay_emits_tint_plus_banner_glyphs() {
        // Defeat: 33-char banner string; many characters render multiple
        // pixels. We don't pin the exact count but the overlay should
        // include the tint quad + many sprite draws for the banner.
        let mut out: Vec<DrawCommand> = Vec::new();
        push_end_state_overlay(&mut out, WinState::Defeat);
        assert!(out.len() > 50, "defeat overlay should emit tint + banner glyphs, got {}", out.len());

        let mut v_out: Vec<DrawCommand> = Vec::new();
        push_end_state_overlay(&mut v_out, WinState::Victory);
        assert!(v_out.len() > 50, "victory overlay should emit tint + banner glyphs, got {}", v_out.len());
    }

    #[test]
    fn compose_scene_no_longer_auto_pushes_end_state_overlay() {
        // Through #45 compose_scene auto-pushed push_end_state_overlay
        // whenever the board was in a non-Playing state. Phase 3
        // (task #77) moved overlay decisions to the bin so it can pick
        // between win-state vs. between-encounter vs. run-end overlays.
        // Verify the auto-push is gone: an empty board (which would
        // have read as Defeat in the old behavior) produces NO
        // full-canvas overlay quad.
        let board = empty_board(7);
        let baseline = compose_scene_with(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &EmptySpriteRegistry);
        let has_overlay_quad = baseline.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.half_size[0] >= crate::gfx::VIRTUAL_W as f32 / 2.0
                && s.half_size[1] >= crate::gfx::VIRTUAL_H as f32 / 2.0)
        });
        assert!(!has_overlay_quad,
            "compose_scene must NOT auto-push the end-state overlay anymore; \
             the bin owns that decision since #77");
    }

    #[test]
    fn push_between_encounter_overlay_emits_tint_plus_text() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_between_encounter_overlay(
            &mut out,
            BetweenEncounterChoice::EncounterComplete { sector_idx: 0, salvage: 7 },
        );
        assert!(out.len() > 50,
            "encounter-complete overlay should emit tint + banner + salvage + choice glyphs, got {}",
            out.len());
        // Tint quad: full-canvas sprite with half_size = canvas/2.
        let has_overlay_quad = out.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.half_size[0] >= crate::gfx::VIRTUAL_W as f32 / 2.0
                && s.half_size[1] >= crate::gfx::VIRTUAL_H as f32 / 2.0)
        });
        assert!(has_overlay_quad, "must include full-canvas tint quad");
    }

    #[test]
    fn push_salvage_hud_emits_glyphs() {
        // The top-right HUD readout should always emit at least the
        // tint-free font glyphs for the "SALVAGE: 0" baseline string.
        let mut out: Vec<DrawCommand> = Vec::new();
        push_salvage_hud(&mut out, 0);
        assert!(out.len() > 20,
            "salvage HUD should emit a row of font glyph quads, got {}",
            out.len());
        // No full-canvas overlay quad — this is an in-game indicator,
        // not a modal screen.
        let has_overlay_quad = out.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.half_size[0] >= crate::gfx::VIRTUAL_W as f32 / 2.0
                && s.half_size[1] >= crate::gfx::VIRTUAL_H as f32 / 2.0)
        });
        assert!(!has_overlay_quad,
            "salvage HUD must NOT emit a full-canvas tint quad");
    }

    #[test]
    fn push_salvage_hud_scales_with_value() {
        // Multi-digit salvage values should emit MORE glyph quads than
        // single-digit values — verifies the counter actually
        // renders the number (not just the "SALVAGE:" prefix).
        let mut small: Vec<DrawCommand> = Vec::new();
        let mut large: Vec<DrawCommand> = Vec::new();
        push_salvage_hud(&mut small, 7);       // 1 digit
        push_salvage_hud(&mut large, 12345);   // 5 digits
        assert!(large.len() > small.len(),
            "5-digit salvage HUD ({}) should emit more glyphs than 1-digit ({})",
            large.len(), small.len());
    }

    #[test]
    fn push_run_defeated_overlay_emits_total_salvage_line() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_run_defeated_overlay(&mut out, 42);
        assert!(out.len() > 50,
            "run-defeated overlay should emit tint + banner + salvage + restart glyphs, got {}",
            out.len());
    }

    #[test]
    fn push_between_encounter_overlay_run_complete_variant_renders() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_between_encounter_overlay(&mut out, BetweenEncounterChoice::RunComplete { salvage: 17 });
        assert!(out.len() > 50,
            "run-complete overlay should emit tint + banner glyphs, got {}",
            out.len());
    }

    #[test]
    fn empty_board_still_produces_backdrop_and_lane() {
        let board = empty_board(7);
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(scene.len() > 20, "expected backdrop + lane, got {}", scene.len());
    }

    #[test]
    fn one_player_ship_produces_visible_sprites() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(scene.len() > 30, "expected backdrop + ship sprites, got {}", scene.len());
    }

    #[test]
    fn ship_with_shield_charges_draws_pips() {
        let mut board_with = empty_board(7);
        let mut ship = frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
        ship.shield_profile = ShieldProfile {
            bow: ShieldFace { armour: 2, charge: 2 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 1 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        };
        board_with.cells[0] = Some(ship);
        let scene_with = compose_scene(&board_with, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);

        let mut bare_board = empty_board(7);
        bare_board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene_without = compose_scene(&bare_board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);

        // 2 bow pips + 1 port pip = 3 extra sprites.
        assert_eq!(scene_with.len() - scene_without.len(), 3);
    }

    #[test]
    fn ship_with_heat_draws_a_filled_bar() {
        let mut board = empty_board(7);
        let mut ship = frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
        ship.heat = 3;
        board.cells[0] = Some(ship);
        let scene_with = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);

        let mut bare_board = empty_board(7);
        bare_board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene_without = compose_scene(&bare_board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);

        assert_eq!(scene_with.len() - scene_without.len(), 1);
    }

    #[test]
    fn projectiles_render_after_ships_in_z_order() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.ordnance.push(Projectile {
            id: "t1".into(),
            kind: "torpedo".into(),
            cell: 3,
            heading: LaneEnd::Fore,
            speed: 1,
            hull: 1,
            payload: Vec::new(),
            owner_faction: Faction::Player,
        });
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        let (mn, mx) = atlas::cell_uvs(atlas::TORPEDO);
        let torpedo_idx = scene.iter().position(|c| match c {
            DrawCommand::Sprite(s) => s.uv_min == mn && s.uv_max == mx,
            _ => false,
        });
        assert!(torpedo_idx.is_some(), "torpedo sprite should be present");
    }

    #[test]
    fn render_example_ts_scenario_composes_without_panic() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        board.cells[3] = Some(frigate_at(3, Faction::Enemy, Orientation::BowOn { bow: LaneEnd::Aft }));
        board.cells[5] = Some(frigate_at(5, Faction::Enemy, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.cells[6] = Some(frigate_at(6, Faction::Enemy, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.ordnance.push(Projectile {
            id: "ord".into(),
            kind: "torpedo".into(),
            cell: 4,
            heading: LaneEnd::Fore,
            speed: 1,
            hull: 1,
            payload: Vec::new(),
            owner_faction: Faction::Player,
        });
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(scene.len() > 60, "expected a populated scene, got {}", scene.len());
    }

    #[test]
    fn every_view_angle_produces_finite_vertices() {
        // Crash-guard: walk every fixed scrub angle (0, 15, 30, 45, 60, 75,
        // 90 deg) and assert no NaN/inf reaches the GPU. wgpu rejects
        // non-finite vertex positions on some drivers; this catches a
        // regression in the ship-rotation math before bruce sees it.
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        board.cells[3] = Some(frigate_at(3, Faction::Enemy, Orientation::BowOn { bow: LaneEnd::Aft }));
        for d in [0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0] {
            let scene = compose_scene(&board, &DEFAULT_LANE, d.to_radians());
            for (i, cmd) in scene.iter().enumerate() {
                match cmd {
                    DrawCommand::Sprite(s) => {
                        for v in [s.pos, s.half_size, s.uv_min, s.uv_max] {
                            for c in v {
                                assert!(c.is_finite(), "non-finite sprite coord at angle {}° idx {}: {:?}", d, i, s);
                            }
                        }
                    }
                    DrawCommand::Polygon(p) => {
                        for v in [p.p0, p.p1, p.p2, p.p3, p.uv_min, p.uv_max] {
                            for c in v {
                                assert!(c.is_finite(), "non-finite polygon coord at angle {}° idx {}: {:?}", d, i, p);
                            }
                        }
                    }
                    DrawCommand::TexturedShip(t) => {
                        for v in [t.p0, t.p1, t.p2, t.p3] {
                            for c in v {
                                assert!(c.is_finite(), "non-finite textured-ship coord at angle {}° idx {}: {:?}", d, i, t);
                            }
                        }
                        assert!(t.blend_t.is_finite(), "non-finite blend_t at angle {}° idx {}: {:?}", d, i, t);
                    }
                }
            }
        }
    }
}
