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
use crate::gfx::{
    DrawCommand, LoftShipInstance, PolygonInstance, SpriteInstance, SpriteSlug,
    TexturedShipInstance,
};
use crate::perspective::{
    cell_to_screen, fractional_cell_to_screen, LaneGeometry, Point2, Stance, FRIGATE_DIMS,
};
use crate::sprites::{EmptySpriteRegistry, SpriteRegistry, SpriteStance, SpriteView};
use crate::types::{
    Board, Faction, HullZone, LaneEnd, Mount, Orientation, Projectile, RangeBand, Ship, Status,
    StatusKind, WeaponArchetype,
};

// v2 (D3): the 2-D scene compositor reads these alongside the 1-D surface above.
// Both coexist during A3 EXPAND→CONTRACT — see `compose_scene_2d`.
use crate::grid::{Axis, Dir4, Facing};
use crate::projector::{grid_cell_quad, CellQuad, ProjectorConfig};

/* ---- palette --------------------------------------------------------------
 *
 * Analysis HTML CSS tokens, scaled to 0..1.
 * ----------------------------------------------------------------------- */

const PLAYER_HULL_FILL: [f32; 4] = [0.102, 0.165, 0.243, 1.0];
const PLAYER_HULL_STROKE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];

const ENEMY_HULL_FILL: [f32; 4] = [0.227, 0.122, 0.145, 1.0];
const ENEMY_HULL_STROKE: [f32; 4] = [0.878, 0.478, 0.235, 1.0];

// (#62) Brighter, cooler grid lanes to read like the art-tool reference's crisp
// cyan road (was a dim slate that barely registered against the starfield).
// (#76 grid polish, Bruce: thinner + more transparent) The lane wireframe reads
// as a FAINT lattice, not bold lines. Lines stay 1px (the pixel-art floor — a
// sub-px quad flickers under nearest sampling), so TRANSPARENCY carries the
// "thinner" read: the far rows are a low-alpha hairline, the front (player) row a
// touch stronger so "near = where you are" still reads. RGB unchanged (cool
// cyan-slate); only alpha dropped from the old fully-opaque 1.0.
const LANE_STROKE: [f32; 4] = [0.33, 0.52, 0.62, 0.28];
const LANE_TICK: [f32; 4] = [0.50, 0.74, 0.84, 0.42]; // brighter front row

const BAND_POINT_BLANK: [f32; 4] = [0.878, 0.400, 0.290, 0.6];
const BAND_CLOSE: [f32; 4] = [0.878, 0.635, 0.235, 0.6];
const BAND_MID: [f32; 4] = [0.353, 0.624, 0.878, 0.6];
const BAND_LONG: [f32; 4] = [0.353, 0.820, 0.796, 0.6];
const BAND_EXTREME: [f32; 4] = [0.608, 0.549, 0.859, 0.6];

const HEAT_BG: [f32; 4] = [0.094, 0.094, 0.110, 0.85];
const HEAT_FILL: [f32; 4] = [0.949, 0.475, 0.235, 1.0];
const HEAT_LOCKOUT: [f32; 4] = [0.949, 0.235, 0.235, 1.0];

// D4 enemy-intent telegraph (`push_threats_2d`): semi-transparent cell fills
// (read THROUGH to the grid/ship) by ThreatKind, plus the source→target beam.
const THREAT_FILL: [f32; 4] = [0.878, 0.235, 0.235, 0.42]; // damage (move!)
const THREAT_FILL_LETHAL: [f32; 4] = [0.961, 0.341, 0.286, 0.62]; // would-kill flash
const THREAT_FILL_DISPLACE: [f32; 4] = [0.353, 0.624, 0.878, 0.42]; // push/pull/swap
const THREAT_FILL_STATUS: [f32; 4] = [0.608, 0.549, 0.859, 0.42]; // debuff
const THREAT_FILL_OTHER: [f32; 4] = [0.55, 0.55, 0.55, 0.34]; // catch-all
// (#99) THREAT_BEAM (the persistent red enemy→cell intent line) removed — Bruce:
// clutter. The threatened-cell outline is the cue; the fire beam shows the shot.

// (#90) Fired-shot VFX in the 2-D scene (Bruce: see weapons firing + results
// clearly). A bright BEAM along the shot line + an IMPACT flash on the struck
// cell, both well above the faint threat-beam alpha so a fired shot READS as a
// discrete event, not the standing intent line. Colour comes from the firing
// faction (player cyan / enemy red, via `vfx::faction_beam_tint`); a MISS draws
// dimmer. Impact = a bright near-white core flash so the hit cell pops.
const IMPACT_FLASH: [f32; 4] = [1.0, 0.93, 0.72, 0.85]; // warm white impact core
const DESTROY_FLASH: [f32; 4] = [1.0, 0.62, 0.28, 0.8]; // orange destruction burst

// Player weapon-arc legibility (`push_weapon_arcs_2d`): a cool player-accent
// outline on cells the player's weapons currently bear on (so "where can I fire"
// reads, and the broadside gun's coverage visibly appears on reorient).
const WEAPON_ARC_OUTLINE: [f32; 4] = [0.329, 0.812, 0.788, 0.55];

// Per-ship 2D hull/health bar (`push_hull_bar_2d`): dark bg + fraction fill that
// ramps green → amber → red as hull drops.
const HULL_BAR_BG: [f32; 4] = [0.094, 0.094, 0.110, 0.85];
const HULL_BAR_HIGH: [f32; 4] = [0.353, 0.820, 0.553, 1.0]; // >60% green
const HULL_BAR_MID: [f32; 4] = [0.878, 0.741, 0.235, 1.0]; // >30% amber
const HULL_BAR_LOW: [f32; 4] = [0.878, 0.286, 0.235, 1.0]; // <=30% red

// Screen-space bottom HUD band (`push_bottom_hud_2d`, #56): fixed health bar +
// weapon-tile row (Shogun-Showdown style).
const HUD_BAND_BG: [f32; 4] = [0.055, 0.067, 0.094, 0.92]; // dark panel
const HUD_LABEL: [f32; 4] = [0.70, 0.78, 0.88, 0.9]; // small text
// (#98) HUD_TILE_BG / HUD_TILE_COOLDOWN removed with the old mount-tile row —
// the ability tiles now use the TILE_* palette in push_ability_tiles_2d.

const SHIELD_PIP_CHARGE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];

// Bow-direction chevron (#55): BOLD, high-contrast so the bow reads at a glance
// against both the dark backdrop and the hull (the old chevron used the hull
// STROKE colour, which blended into the hull). The mark contrasts its OWN hull's
// temperature: the player's hull is cool blue, so its chevron is WARM gold-white;
// the enemy hull is warm orange, so its chevron is COOL cyan-white. A dark
// drop-shadow chevron is drawn behind it so it reads on any backdrop layer.
// Retained pending Bruce's final call on the player bow-arrow (#62 dropped it as
// chase-view clutter, but the lead is holding the final drop for his nod — keep
// the colour so re-enabling is a one-liner).
#[allow(dead_code)]
const BOW_MARK_PLAYER: [f32; 4] = [1.0, 0.91, 0.62, 1.0]; // warm gold-white (vs cool hull)
// (#99 Bruce) Enemy facing/move arrow = YELLOW. Was cyan-white [0.72,0.97,1.0],
// which read as white and got lost against the white background stars. Saturated
// yellow pops against both the dark field and the stars.
const BOW_MARK_ENEMY: [f32; 4] = [1.0, 0.86, 0.20, 1.0]; // yellow (vs white starfield)
const BOW_MARK_SHADOW: [f32; 4] = [0.02, 0.03, 0.05, 0.9]; // dark backing

// (#62) Player engine glow — the reference ship's signature read: a cluster of
// bright cyan thruster lights at the stern (toward the camera). Bright core +
// a dimmer halo behind it so the cluster reads as glowing engines, not flat dots.
const ENGINE_GLOW_CORE: [f32; 4] = [0.45, 0.95, 1.0, 1.0]; // bright cyan
const ENGINE_GLOW_HALO: [f32; 4] = [0.30, 0.75, 1.0, 0.45]; // soft cyan halo

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DEFEAT_TINT: [f32; 4] = [0.85, 0.08, 0.10, 0.55];
const VICTORY_TINT: [f32; 4] = [1.00, 0.80, 0.20, 0.45];

/// Render-only multiplier on the ship silhouette's on-screen extent. The raw
/// `FRIGATE_DIMS` (120×60×40) read too small against the ~177 px lane-cell
/// pitch on `DEFAULT_LANE` (bruce playtest, twice: "ships too small to
/// read"). This scales the drawn width/height WITHOUT moving cell centers,
/// so the silhouette grows while ships stay on their lane slots.
///
/// At 2.0× a bow-on Frigate draws ~240 px wide vs the ~177 px cell pitch, so
/// adjacent ships **overlap by design at PointBlank** — that reads as
/// close-quarters crowding, not breakage (bruce's call: point-blank ships
/// *should* look jammed together). Broadside ships (beam-on, 60 px base)
/// stay ~120 px and never overlap. Vertically the worst case (broadside at
/// 45°, total_h ≈ 113 px unscaled → ~226 px) still fits inside the 480 px
/// canvas centered on `center_y = 240`.
///
/// Renderer-side knob only — does NOT touch the `FRIGATE_DIMS` game-design
/// constant, lane positions, or any range/geometry math, and is fully
/// revertible. Bruce iterates this value. Going much past ~2.2× starts
/// clipping tall broadside silhouettes against the canvas top/bottom; making
/// ships bigger than that needs fewer lane cells (gameplay-significant — see
/// the lane-cell-count option flagged to the lead, needs his ruling).
const SHIP_SCALE: f32 = 2.0;

/// Whether to draw the per-ship overlay HUD (heat bar, shield pips, queue
/// glyphs, status badges) and the range-band ruler. ON: these are functional
/// combat/threat readouts bruce wants present + clean. The #45 fix RE-ANCHORED
/// them to the loft ship footprint (`ship_bbox` now returns the loft dest-rect
/// extent, not the stale 2D `scaled_ship_extent` the #44 seating decoupled
/// from) so they sit ON the ships instead of floating off as artifacts.
const SHOW_PLACEHOLDER_HUD: bool = true;

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

/// On-screen height (virtual px) of a loft ship's blit quad. The loft pipeline
/// renders every ship into the SAME `LOW_W`×`LOW_H` (320×200) offscreen with the
/// hull centred at the world origin, so the ship's content is centred in that
/// texture. Blitting that texture into a quad of a FIXED height (this constant)
/// × the texture's aspect, centred on the lane, does two things the per-stance
/// 2D `scaled_ship_extent` bbox did not:
///   1. SEATS the ship on the lane — a content-centred texture into a
///      lane-centred quad puts the hull's centre on the lane line (the 2D bbox
///      varied wildly by stance — broadside's `height·cosθ + length·sinθ` made a
///      very tall quad that dipped the ship below the lane).
///   2. CONSISTENT SCALE — one height for every ship/stance, so a ship doesn't
///      jump size when it reorients (true relative ship size still comes from
///      the 3D framing inside the loft pass, not here).
///
/// Tuned to ~fill a lane cell at the 7-cell layout; bruce dials final size.
const LOFT_SHIP_HEIGHT_PX: f32 = 150.0;

/// Aspect of the loft offscreen (`loft_gpu::LOW_W / LOW_H` = 160/100 = 1.6).
/// Mirrored here as a plain const so `hud` stays buildable without the `render`
/// feature (where `loft_gpu` isn't compiled). MUST equal the loft texture's
/// aspect: the loft blit stretches that texture to fill the dest quad, so any
/// dest-rect built here divides a width by THIS to stay un-squashed (#74). Kept
/// in sync with the house-style res (160×100 and the old 320×200 are both 1.6).
const LOFT_TEXTURE_ASPECT: f32 = 160.0 / 100.0;

/// Lane-seated blit rect (`(left, top, right, bottom)`) for a loft ship centred
/// at screen-x `cx` on the lane at `center_y`. Fixed height × the loft texture's
/// aspect, centred on the lane so the content-centred texture sits ON the lane
/// line. Stance-independent (the 3D pose lives in the texture).
fn loft_dest_rect(cx: f32, center_y: f32) -> (f32, f32, f32, f32) {
    let h = LOFT_SHIP_HEIGHT_PX;
    let w = h * LOFT_TEXTURE_ASPECT;
    (
        cx - w / 2.0,
        center_y - h / 2.0,
        cx + w / 2.0,
        center_y + h / 2.0,
    )
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
    if SHOW_PLACEHOLDER_HUD {
        push_range_band_ticks(&mut out, board, lane);
    }
    push_hazards(&mut out, board, lane);

    for ship in board.cells.iter().flatten() {
        let visual_cell = tween.cell_for(ship);
        push_ship(&mut out, ship, visual_cell, lane, view_angle_rad, sprites);
    }

    for proj in &board.ordnance {
        push_projectile(&mut out, proj, lane);
    }

    // Per-ship overlay HUD (heat bar / shield pips / queue glyphs / status
    // badges). GATED OFF for the ship-showcase (#45): these are placeholder
    // glyphs anchored to the 2D `ship_bbox`, which the loft seating (#44)
    // decoupled from — so they float off the 3D ships and read as artifacts
    // (the dark-blue side-bands + teal specks bruce flagged). They return as
    // real, loft-anchored HUD in the #46 HUD pass; the structure stays here.
    if SHOW_PLACEHOLDER_HUD {
        for ship in board.cells.iter().flatten() {
            let visual_cell = tween.cell_for(ship);
            push_heat_bar(&mut out, ship, visual_cell, lane, view_angle_rad);
            push_shield_pips(&mut out, ship, visual_cell, lane, view_angle_rad);
            push_queue_glyphs(&mut out, ship, visual_cell, lane, view_angle_rad);
            push_status_badges(&mut out, ship, visual_cell, lane, view_angle_rad);
        }
        push_view_angle_overlay(&mut out, view_angle_rad);
    }

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

/* =============================================================================
 * v2 2-D scene compositor (D3) — the perspective-grid render path.
 *
 * This is the v2 replacement for the 1-D `compose_scene*` flat-lane path above.
 * Where the 1-D path lays ships on a horizontal strip by `ship.cell`, this path
 * places each ship on the 5×4 perspective grid by `ship.pos`, scaled by the
 * projector's per-cell `depth_scale`, with a bow-direction arrow encoding
 * `Facing::forward_axis()` (the SAME forward axis the resolver's `facing_zone`
 * table uses — the correctness-critical orientation contract) and gold shield
 * pips per zone.
 *
 * It lands ALONGSIDE the 1-D path (A3 EXPAND→CONTRACT): the demo bin still
 * drives the 1-D `compose_scene_tweened` until bin wiring is green-lit, so the
 * old path stays intact. Lifted near-verbatim from the proven
 * `src/bin/grid_preview.rs` staging.
 *
 * NOT in this path yet (deferred, by sequencing):
 *  - Threat-fill + the queued move-arrow telegraph — both consume the telegraph
 *    data source (`Board.threats`, populated by resolver R8), which is not in the
 *    tree yet. The styling is staged in grid_preview; it wires here when
 *    Board.threats lands (D4). The move destination is derived from a queued
 *    action (resolver), not plain ship state, so it is part of that same D4 step.
 *  - The live loft / textured-ship draw — D3 emits the flat placeholder hull;
 *    the depth_scale-driven loft seating follows.
 * ============================================================================= */

/// Build the v2 perspective-grid scene as a back-to-front `Vec<DrawCommand>`:
/// the grid wireframe, then every ship (hull + bow-direction arrow + shield
/// pips) placed at its projected cell, far row first so nearer ships overlap
/// farther ones. `cfg` is the perspective tuning ([`ProjectorConfig::default`]
/// matches the 480×270 canvas).
///
/// Reads `ship.pos` / `ship.facing` (the v2 fields). Until the resolver/content
/// MIGRATE writes real positions (every ship currently defaults to
/// `Pos::new(0,0)`), all ships render stacked at the back-left origin cell — the
/// render logic is correct and lights up as the data fills in (the
/// `grid_preview` bin proves the path with non-default mock positions).
pub fn compose_scene_2d(board: &Board, cfg: &ProjectorConfig) -> Vec<DrawCommand> {
    compose_scene_2d_with(board, cfg, &EmptySpriteRegistry)
}

/// (#79) The interpolated RENDER position + facing for one ship mid-move, so its
/// hull SLIDES cell-to-cell + TURNS smoothly instead of snapping. The bin
/// computes these by easing `from`→`to` over ~0.12s (it owns the per-move timer);
/// `hud` just draws them. `center` / `near_edge_width` / `depth_scale` are the
/// lerp of the two cells' [`crate::projector::CellQuad`]s (so the ship follows
/// the perspective, not a flat screen line); `facing_yaw_deg` is the shortest-
/// path angular lerp of the ground-plane facing yaw (the player-loft hull's
/// rotation). Absent ⇒ the ship renders at its logical cell (snap), so the
/// default (empty [`Tween2d`]) reproduces [`compose_scene_2d_with`] exactly.
#[derive(Clone, Copy, Debug)]
pub struct VisualShip2d {
    /// Interpolated cell-centre in virtual-pixel space.
    pub center: [f32; 2],
    /// Interpolated cell NEAR (bottom) edge screen-y — the loft hero hull seats
    /// its base here so it FOLLOWS its cell up-lane as it moves (#80), instead of
    /// pinning to the HUD band.
    pub near_edge_y: f32,
    /// Interpolated near-edge width (drives the hero hull's on-screen size).
    pub near_edge_width: f32,
    /// Interpolated per-cell foreshortening factor (HUD marker sizes).
    pub depth_scale: f32,
    /// Interpolated ground-plane facing yaw (deg) for the loft hull's rotation.
    pub facing_yaw_deg: f32,
}

/// (#79) Per-ship visual tween overrides for the 2-D live path, keyed by
/// `Ship::id`. Empty ⇒ every ship snaps to its logical cell (identical to the
/// untweened compose). The 2-D analog of the 1-D [`TweenState`].
#[derive(Default, Clone, Debug)]
pub struct Tween2d {
    pub visual: std::collections::HashMap<String, VisualShip2d>,
}

/// Linear-interpolate two [`crate::projector::CellQuad`]s corner-for-corner (+
/// centre + depth_scale) by `t∈[0,1]`. Used to slide a ship between its previous
/// and current cell along the perspective grid (#79). At `t=0` returns `a`, at
/// `t=1` returns `b`. `pub` so the bin builds a [`VisualShip2d`] (it owns the
/// per-move timer + the from/to cells).
pub fn lerp_cell_quad(
    a: &crate::projector::CellQuad,
    b: &crate::projector::CellQuad,
    t: f32,
) -> crate::projector::CellQuad {
    let l1 = |x: f32, y: f32| x + (y - x) * t;
    let lc = |i: usize| [l1(a.corners[i][0], b.corners[i][0]), l1(a.corners[i][1], b.corners[i][1])];
    crate::projector::CellQuad {
        corners: [lc(0), lc(1), lc(2), lc(3)],
        center: [l1(a.center[0], b.center[0]), l1(a.center[1], b.center[1])],
        depth_scale: l1(a.depth_scale, b.depth_scale),
    }
}

/// Like [`compose_scene_2d`] but consults `sprites` for the loft/3-D ship path
/// (#51): if [`SpriteRegistry::loft_kind`] returns a mesh kind for a ship, that
/// ship is emitted as a [`DrawCommand::LoftShip`] (the real 3-D hull blitted into
/// its projected cell quad) instead of the flat placeholder box. The bin passes
/// its `Gfx` (which implements `SpriteRegistry` + has the CAD/loft meshes
/// installed) so the player renders as the Aegis model; the default
/// [`compose_scene_2d`] (and the headless tests) pass [`EmptySpriteRegistry`],
/// keeping the flat-box path. Same back-to-front order; the registry only changes
/// HOW each ship body is drawn, not the grid / telegraph / overlays.
pub fn compose_scene_2d_with(
    board: &Board,
    cfg: &ProjectorConfig,
    sprites: &dyn SpriteRegistry,
) -> Vec<DrawCommand> {
    compose_scene_2d_tweened(board, cfg, sprites, &Tween2d::default())
}

/// Like [`compose_scene_2d_with`] but applies per-ship visual tween overrides
/// (#79) so a moving/turning ship SLIDES + ROTATES smoothly instead of snapping.
/// `tween` is the bin's eased `from`→`to` interpolation for this frame; a ship
/// absent from `tween.visual` renders at its logical cell (so an empty
/// [`Tween2d`] == the untweened compose). Only the ship BODY (the loft hull /
/// placeholder) + its arrow/pips ride the override; the grid, threat fills, and
/// the screen-space bottom HUD are position-independent and unchanged.
pub fn compose_scene_2d_tweened(
    board: &Board,
    cfg: &ProjectorConfig,
    sprites: &dyn SpriteRegistry,
    tween: &Tween2d,
) -> Vec<DrawCommand> {
    let mut out = Vec::with_capacity(256);
    push_grid_2d(&mut out, cfg);

    // Player weapon-arc legibility: outline the cells the PLAYER's weapons bear
    // on (given the player's current facing) so the player reads WHICH cells they
    // can fire along — and watches the coverage change as they REORIENT (the
    // broadside gun only bears on the flanks when turned broadside). Drawn under
    // the threat fills + ships. Single source: the resolver's `arc_bears`.
    push_weapon_arcs_2d(&mut out, board, cfg);

    // D4 enemy-intent telegraph: paint threatened cells UNDER the ships (so the
    // ship draws on top of the red fill) from the resolver's ThreatMap
    // (`board.threats`, populated by R8 via resolve_targeting — the single
    // source). The intent BEAM (source→target) is drawn here too so the player
    // reads which enemy threatens which cell — the core "who's doing what" cue.
    push_threats_2d(&mut out, board, cfg);

    // Far row (row 0) first → front row last, so nearer ships overlap farther.
    let mut ships: Vec<&Ship> = board.cells.iter().flatten().collect();
    ships.sort_by_key(|s| s.pos.row);
    for ship in &ships {
        push_ship_2d(&mut out, ship, cfg, sprites, tween.visual.get(&ship.id));
    }
    // Per-ship overlays LAST, so health bars + queue tiles sit on top of every
    // hull (incl. a nearer ship that overlaps a farther one). Same far→near order.
    // (#62) The PLAYER's lane-anchored hull bar is SKIPPED — it's redundant with
    // the screen-space bottom HUD health bar (push_bottom_hud_2d) and, at the
    // hero hull's size, the small cell-anchored bar collided with the big hull.
    // The player's health reads from the prominent bottom band; enemies (no
    // bottom bar) keep their lane hull bars. Queue tiles stay for both.
    for ship in &ships {
        if ship.faction != Faction::Player {
            push_hull_bar_2d(&mut out, ship, cfg);
        }
        push_queue_tiles_2d(&mut out, ship, cfg);
    }
    // (#90) Resolved weapon fire on TOP of the hulls (Bruce: see weapons firing +
    // results clearly): bright faction-tinted beams along each shot line + an
    // impact flash on every struck cell. Driven straight off the board
    // (fire_events) — the live bin holds these for the round. The DESTRUCTION
    // burst is NOT here: a dead ship is removed same-action so a board scan can't
    // see it; the bin diffs prev-vs-current ship ids on a combat resolve + calls
    // `push_destruction_at` with the vanished cells (see broadside.rs kill_bursts).
    push_fire_2d(&mut out, board, cfg);
    // Bottom HUD band LAST of all — a screen-space fixed (NOT projected) health
    // bar + large weapon-tile row, drawn on top of everything (Bruce: a fixed
    // centered Shogun-Showdown-style bottom bar so health + abilities always read).
    push_bottom_hud_2d(&mut out, board);
    out
}

/// Screen-space FIXED bottom HUD band (#56): the player's health bar + a row of
/// large weapon tiles, pinned to the bottom of the 480×270 frame (NOT projected
/// on the board), Shogun-Showdown style. Reads the player ship: hull/max_hull for
/// the bar; `mounts` for the tiles (one per weapon, hotkey 1.. in mount order),
/// each tinted by state — ready (player accent), heated (amber, scaled by
/// heat/heat_max), locked-out (red), on-cooldown (dim) — with the hotkey digit +
/// a weapon-archetype glyph. No-op if there's no player ship.
fn push_bottom_hud_2d(out: &mut Vec<DrawCommand>, board: &Board) {
    let Some(player) = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
    else {
        return;
    };
    // (#76 scene-res) Read the LIVE scene size so the HUD band spans + sits at the
    // bottom of the current offscreen, not a fixed 480×270 (== at the default).
    let w = crate::gfx::scene_w() as f32;
    let h = crate::gfx::scene_h() as f32;
    let band_h = 40.0;
    let band_top = h - band_h;

    // Band background — a dark panel across the full width.
    push_polygon(
        out,
        PolygonInstance::flat(
            [[0.0, band_top], [w, band_top], [w, h], [0.0, h]],
            HUD_BAND_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );

    // --- Player HEALTH bar (left of the band) ---
    let hp_x = 10.0;
    let hp_y = band_top + 8.0;
    let hp_w = 150.0;
    let hp_h = 9.0;
    // "HP" label above the bar.
    push_text_left(out, "HULL", hp_x, hp_y - 8.0, 1.0, HUD_LABEL);
    // Track.
    push_polygon(
        out,
        PolygonInstance::flat(
            [[hp_x, hp_y], [hp_x + hp_w, hp_y], [hp_x + hp_w, hp_y + hp_h], [hp_x, hp_y + hp_h]],
            HULL_BAR_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    if player.max_hull > 0 {
        let frac = (player.hull as f32 / player.max_hull as f32).clamp(0.0, 1.0);
        if frac > 0.0 {
            let fw = hp_w * frac;
            let color = if frac > 0.6 {
                HULL_BAR_HIGH
            } else if frac > 0.3 {
                HULL_BAR_MID
            } else {
                HULL_BAR_LOW
            };
            push_polygon(
                out,
                PolygonInstance::flat(
                    [[hp_x, hp_y], [hp_x + fw, hp_y], [hp_x + fw, hp_y + hp_h], [hp_x, hp_y + hp_h]],
                    color,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
        }
    }

    // (#98) Weapon TILES moved to `push_ability_tiles_2d` (the bin calls it with
    // the real AbilityTile data — damage / cooldown_max — which the Board alone
    // doesn't carry). This fn now owns just the band + health bar.
    let _ = (w, band_h); // (kept for the band geometry above)
}

/// (#98 Bruce) The player's ability-tile row in the bottom HUD band — Shogun-
/// Showdown style, drawn from the bin's [`AbilityTile`]s (the only place carrying
/// per-weapon damage / range / cooldown_max; the Board doesn't). Per tile: the
/// weapon ICON centred; a LARGE DAMAGE number top-left (blank if 0); a smaller
/// RANGE number (cells) top-right (blank if 0); the KEY (slot char) bottom-right;
/// and COOLDOWN as TICKS along the bottom edge, one per `cooldown_max`, GREY by
/// default and charging WHITE from the RIGHT as each round passes (rightmost fills
/// first). When all ticks are white (cooldown elapsed) — or `cooldown_max == 0`
/// (no ticks) — the whole tile gets a WHITE BORDER = "ready to queue"; a charging
/// tile keeps a dim violet frame. The bin calls this after the scene compose (it
/// holds the tiles); replaces the old mount-only tile row.
pub fn push_ability_tiles_2d(out: &mut Vec<DrawCommand>, tiles: &[AbilityTile]) {
    if tiles.is_empty() {
        return;
    }
    let w = crate::gfx::scene_w() as f32;
    let h = crate::gfx::scene_h() as f32;
    let band_h = 40.0;
    let band_top = h - band_h;
    let tile = 30.0;
    let gap = 8.0;
    let n = tiles.len();
    let row_w = n as f32 * tile + (n as f32 - 1.0) * gap;
    // Centre the row in the band's right portion (after the left-edge health bar).
    let start_x = (w - row_w) / 2.0 + 60.0;
    let tile_y = band_top + (band_h - tile) / 2.0;
    for (i, t) in tiles.iter().enumerate() {
        let tx = start_x + i as f32 * (tile + gap);
        // READY = no cooldown active (cooldown_max 0 is always ready; otherwise all
        // ticks charged white = cooldown elapsed). A ready tile gets a WHITE BORDER
        // ("queue me"); a charging tile keeps the dim cooldown frame.
        let ready = t.cooldown <= 0;
        let border = if ready { TILE_BORDER_READY } else { TILE_COOLDOWN };
        // Tile bg + border (2px when ready so the white frame reads as a clear cue).
        push_polygon(
            out,
            PolygonInstance::flat(
                [[tx, tile_y], [tx + tile, tile_y], [tx + tile, tile_y + tile], [tx, tile_y + tile]],
                TILE_BG,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
        let c = [
            [tx, tile_y],
            [tx + tile, tile_y],
            [tx + tile, tile_y + tile],
            [tx, tile_y + tile],
        ];
        let bth = if ready { 2.0 } else { 1.0 };
        for k in 0..4 {
            push_line(out, pt(c[k]), pt(c[(k + 1) % 4]), bth, border);
        }
        // Icon, centred (nudged up a touch to leave room for the bottom tick row).
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [tx + tile / 2.0, tile_y + tile / 2.0 - 1.0],
                [7.0, 7.0],
                TILE_ICON,
                atlas::cell_uvs(t.icon.atlas_cell()),
            ),
        );
        // DAMAGE = LARGE number, TOP-LEFT (pixel=2; skip 0 = non-damage ability).
        if t.damage > 0 {
            push_text_left(out, &t.damage.to_string(), tx + 2.0, tile_y + 2.0, 2.0, TILE_DAMAGE);
        }
        // RANGE = smaller number, TOP-RIGHT (cells; skip 0 = non-targeted).
        if t.range > 0 {
            let s = t.range.to_string();
            // Right-align: one glyph is 5px at pixel=1.
            let rx = tx + tile - 2.0 - (s.len() as f32 * 6.0 - 1.0);
            push_text_left(out, &s, rx, tile_y + 2.0, 1.0, TILE_RANGE);
        }
        // KEY = number, BOTTOM-RIGHT.
        let key = t.slot.to_string();
        push_text_left(out, &key, tx + tile - 6.0, tile_y + tile - 8.0, 1.0, HUD_LABEL);
        // COOLDOWN TICKS along the bottom edge: one per cooldown_max, GREY by
        // default, charging WHITE from the RIGHT as each round passes (rightmost
        // fills first). `elapsed = cooldown_max - cooldown` ticks are charged, and
        // they're the RIGHTMOST ones: tick k (0=leftmost) is white iff
        // k >= cooldown_remaining. All white (cooldown 0) => the ready border above.
        if t.cooldown_max > 0 {
            let ticks = t.cooldown_max;
            let remaining = t.cooldown.clamp(0, ticks);
            let pad = 2.0;
            let tick_gap = 1.0;
            let avail = tile - 2.0 * pad;
            let tw = ((avail - (ticks as f32 - 1.0) * tick_gap) / ticks as f32).max(1.0);
            let ty = tile_y + tile - 3.0;
            for k in 0..ticks {
                let kx = tx + pad + k as f32 * (tw + tick_gap);
                // Charge from the right: the leftmost `remaining` stay grey.
                let col = if k >= remaining { TILE_TICK_ELAPSED } else { TILE_TICK_REMAIN };
                push_polygon(
                    out,
                    PolygonInstance::flat(
                        [[kx, ty], [kx + tw, ty], [kx + tw, ty + 2.0], [kx, ty + 2.0]],
                        col,
                        atlas::cell_uvs(atlas::SOLID_WHITE),
                    ),
                );
            }
        }
    }
}

// (#98) weapon_archetype removed with the old mount-tile row — the ability tiles
// now carry their icon via AbilityTile::icon (the bin maps it from the catalog
// action archetype, the real source, not an id substring).

/// Queued-action tiles above a ship on the 2D board — the v1 "tiles" Bruce
/// misses, showing what a ship has lined up. A horizontal row of small glyph
/// icons (one per entry in `ship.queue`, in fire order, mapped to its weapon
/// archetype glyph), centered over the ship's cell and floated above the hull
/// bar. Shrinks with `depth_scale`; no-op when the queue is empty (most enemies
/// most turns), so it only appears when a ship is actually readying something —
/// a direct "this ship is about to act" cue complementing the threat telegraph.
fn push_queue_tiles_2d(out: &mut Vec<DrawCommand>, ship: &Ship, cfg: &ProjectorConfig) {
    if ship.queue.is_empty() {
        return;
    }
    let q = grid_cell_quad(ship.pos, cfg);
    let scale = q.depth_scale;
    let base = 22.0 * scale;
    let tile = (5.0 * scale).max(2.5); // glyph half-size
    let gap = tile * 2.0 + 2.0 * scale;
    let n = ship.queue.len() as f32;
    let total_w = (n - 1.0).max(0.0) * gap;
    let start_x = q.center[0] - total_w * 0.5;
    // Above the hull bar (which sits at center.y − (base + 6*scale)).
    let ty = q.center[1] - (base + 14.0 * scale);
    for (i, action_id) in ship.queue.iter().enumerate() {
        let archetype = archetype_of_mount(ship, action_id).unwrap_or(WeaponArchetype::Beam);
        let cell_uv = archetype_to_glyph(archetype);
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [start_x + i as f32 * gap, ty],
                [tile, tile],
                WHITE,
                atlas::cell_uvs(cell_uv),
            ),
        );
    }
}

/// A compact per-ship hull/health bar floating just above the ship's cell on the
/// 2D board (Bruce's "who's hurt" readability). A dark background bar with a
/// left-aligned fill = `hull / max_hull`, colored green → amber → red as health
/// drops. Width tracks the hull box and the bar shrinks with `depth_scale`, so a
/// back-row ship gets a smaller bar — consistent with the perspective. Drawn for
/// every ship (player + enemies); skipped if `max_hull <= 0` (degenerate).
fn push_hull_bar_2d(out: &mut Vec<DrawCommand>, ship: &Ship, cfg: &ProjectorConfig) {
    if ship.max_hull <= 0 {
        return;
    }
    let q = grid_cell_quad(ship.pos, cfg);
    let scale = q.depth_scale;
    // Match the hull box half-width used in push_ship_2d (base = 22 * depth_scale).
    let base = 22.0 * scale;
    let half_w = base; // bar spans the hull width
    let bar_h = (3.0 * scale).max(1.5);
    // Float above the hull: cell center − (hull half-height + gap). Use the
    // bow-stance vertical half-extent (the taller case) so the bar clears the hull.
    let cy = q.center[1] - (base + 6.0 * scale);
    let cx = q.center[0];
    let left = cx - half_w;

    // Background.
    push_polygon(
        out,
        PolygonInstance::flat(
            [
                [left, cy - bar_h * 0.5],
                [left + half_w * 2.0, cy - bar_h * 0.5],
                [left + half_w * 2.0, cy + bar_h * 0.5],
                [left, cy + bar_h * 0.5],
            ],
            HULL_BAR_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );

    // Fill = hull fraction, left-aligned.
    let frac = (ship.hull as f32 / ship.max_hull as f32).clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill_w = half_w * 2.0 * frac;
        let color = if frac > 0.6 {
            HULL_BAR_HIGH
        } else if frac > 0.3 {
            HULL_BAR_MID
        } else {
            HULL_BAR_LOW
        };
        push_polygon(
            out,
            PolygonInstance::flat(
                [
                    [left, cy - bar_h * 0.5],
                    [left + fill_w, cy - bar_h * 0.5],
                    [left + fill_w, cy + bar_h * 0.5],
                    [left, cy + bar_h * 0.5],
                ],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
}

/// D4: render the enemy-intent telegraph from `board.threats` (the resolver's
/// ThreatMap — single source, populated by R8 from each enemy's queued action).
/// Per threat:
///   1. a bright cell OUTLINE — colour by [`ThreatKind`] (red = damage, brighter
///      red = lethal, blue = displace, violet = status) — so "this cell is
///      dangerous, move" reads at a glance;
///   2. a FAINT tint inside the cell (very low alpha) so the cell reads as
///      threatened WITHOUT burying the ship/field;
///   3. a thin intent BEAM from the threatening enemy (`Threat::source`) to the
///      target cell, so the player sees WHO is threatening WHERE.
///
/// (Bruce-facing fix) The telegraph was a FULL-CELL OPAQUE FILL. On the PLAYER's
/// NEAR (front) cell — the largest trapezoid on screen — that became a big
/// salmon-orange SLAB covering the ship + most of the lower field ("an
/// incomprehensible square covers my ship"). The fix: outline + a hairline-faint
/// fill, so a near-row threat no longer slabs the playfield. Drawn after the grid,
/// before ships.
fn push_threats_2d(out: &mut Vec<DrawCommand>, board: &Board, cfg: &ProjectorConfig) {
    use crate::types::ThreatKind;
    for threat in &board.threats {
        let q = grid_cell_quad(threat.pos, cfg);
        // Is this damage lethal to the cell's current occupant? (amount ≥ hull.)
        let lethal = matches!(threat.kind, ThreatKind::Damage { amount }
            if board
                .cells
                .get(threat.pos.to_index())
                .and_then(|c| c.as_ref())
                .is_some_and(|s| amount >= s.hull));
        let fill = match threat.kind {
            ThreatKind::Damage { .. } if lethal => THREAT_FILL_LETHAL,
            ThreatKind::Damage { .. } => THREAT_FILL,
            ThreatKind::Displace => THREAT_FILL_DISPLACE,
            ThreatKind::Status => THREAT_FILL_STATUS,
            ThreatKind::Other => THREAT_FILL_OTHER,
        };
        // VERY FAINT interior tint — just enough to read as a coloured cell, never
        // an opaque slab over the hull (the OUTLINE carries the cue). A lethal
        // threat gets a touch more so it still stands out.
        let faint_a = if lethal { 0.10 } else { 0.06 };
        let faint = [fill[0], fill[1], fill[2], faint_a];
        push_polygon(
            out,
            PolygonInstance::flat(q.corners, faint, atlas::cell_uvs(atlas::SOLID_WHITE)),
        );
        // Bright cell OUTLINE in the threat colour (full alpha) — the crisp "this
        // cell is threatened" cue that survives without slabbing the field.
        let outline = [fill[0], fill[1], fill[2], 1.0];
        let th = if lethal { 2.0 } else { 1.0 };
        let c = q.corners;
        push_line(out, pt(c[0]), pt(c[1]), th, outline);
        push_line(out, pt(c[1]), pt(c[2]), th, outline);
        push_line(out, pt(c[2]), pt(c[3]), th, outline);
        push_line(out, pt(c[3]), pt(c[0]), th, outline);
        // (#99 Bruce) The persistent RED enemy→cell INTENT BEAM is REMOVED — it drew
        // every frame an enemy held a threat ("a red line always projecting from the
        // enemy" = clutter). The threatened-cell OUTLINE above is the dodge cue
        // ("this cell will be hit, move"); the actual shot still shows as the
        // momentary fire beam (`push_fire_2d`) on CommitTurn. So the enemy-fire
        // telegraph survives without the standing line.
    }
}

/// (#90) Draw the RESOLVED weapon fire for this round in the 2-D scene (Bruce:
/// "see weapons firing better and the results more cleanly"). For each
/// [`crate::types::FireEvent`] in `board.fire_events` (the resolver's single
/// source — one event per attacker→target shot): a bright archetype-styled BEAM
/// along the shot line, faction-tinted (player cyan / enemy red via
/// [`crate::vfx::faction_beam_tint`]), dimmed on a MISS; plus an IMPACT flash on
/// the struck cell for a hit. This is the 2-D analog of the 1-D `vfx` beams
/// (which positioned via the old `LaneGeometry` and were dropped from the 2-D
/// path in #43) — driven straight off the board so it needs no separate timing
/// state: the live bin holds the events for the round + redraws.
///
/// Drawn AFTER ships (over the hulls) so the shot + impact read on top. The fade
/// over a shot's life still lives in the windowed `vfx` system for the 1-D path;
/// here the beam is the full-strength round read (clarity over a micro-fade —
/// Bruce wants to SEE the shots), which the next round's events replace.
fn push_fire_2d(out: &mut Vec<DrawCommand>, board: &Board, cfg: &ProjectorConfig) {
    for fe in &board.fire_events {
        let (thickness, _life) = crate::vfx::archetype_beam_style(fe.archetype);
        let tint = crate::vfx::faction_beam_tint(fe.attacker_faction);
        let from = grid_cell_quad(fe.from_pos, cfg).center;
        let to = grid_cell_quad(fe.to_pos, cfg).center;
        // A hit reads full + bright; a miss is dimmer + thinner so "fired but
        // didn't connect" reads without faking a separate animation.
        let (alpha, th) = if fe.hit {
            (0.95, thickness)
        } else {
            (0.45, thickness * 0.7)
        };
        let beam = [tint[0], tint[1], tint[2], alpha];
        push_line(out, pt(from), pt(to), th, beam);
        // Impact flash on the struck cell (hits only) — a bright warm-white core so
        // the result POPS where the shot landed.
        if fe.hit {
            let q = grid_cell_quad(fe.to_pos, cfg);
            let r = (q.near_edge_width() * 0.5).clamp(6.0, 26.0);
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    q.center,
                    [r, r],
                    IMPACT_FLASH,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
        }
    }
}

/// (#90 kill-burst) Draw a DESTRUCTION burst at each given board cell — a clear "a
/// ship died here" cue (Bruce: results read cleanly). A bright two-layer flash
/// (wide soft orange halo + near-white core) at each cell's projected centre,
/// sized by the cell's near-edge width so far kills read smaller in perspective.
///
/// Takes EXPLICIT cells (not a board scan) because the resolver removes a dead
/// ship the same action it dies (`destroy()` → `cells[c].take()`), so a hull<=0
/// ship never survives to a frame. The bin supplies the cells by diffing the
/// previous vs current ship-id set on a combat-turn resolve (the renderer-side
/// death signal — the 2-D analog of `vfx::CombatVfx`'s vanish detection) and
/// holds each for ~0.35s. Drawn last (over everything) so the burst reads.
pub fn push_destruction_at(
    out: &mut Vec<DrawCommand>,
    cells: &[crate::grid::Pos],
    cfg: &ProjectorConfig,
) {
    for &pos in cells {
        let q = grid_cell_quad(pos, cfg);
        let r = (q.near_edge_width() * 0.7).clamp(8.0, 34.0);
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                q.center,
                [r, r],
                DESTROY_FLASH,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                q.center,
                [r * 0.5, r * 0.5],
                IMPACT_FLASH,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
}

/// Player weapon-arc legibility: outline every cell the PLAYER's weapons bear on
/// from the player's current pos/facing, so the player reads WHERE they can fire
/// — and sees the coverage shift as they reorient (the `BroadsideArc` gun only
/// bears on the flank cardinals when turned broadside; `Forward`/`Rear` out the
/// bow/stern cardinal; `Turret` everywhere). Uses the resolver's
/// [`crate::geometry2d::arc_bears`] as the single source (firing is
/// cardinal-exact, so only cells due-N/S/E/W bear unless a turret is mounted) — no
/// re-derivation of the firing geometry in the renderer.
///
/// This is the ARC half; the per-weapon RANGE-band refinement (dimming
/// out-of-band cells) needs each weapon's `Targeting.range_band` from the
/// catalog, which `compose_scene_2d` doesn't hold — a follow-up that threads a
/// `Content`/catalog lookup.
fn push_weapon_arcs_2d(out: &mut Vec<DrawCommand>, board: &Board, cfg: &ProjectorConfig) {
    use crate::geometry2d::arc_bears;
    use crate::grid::{all_positions, from_to};

    let Some(player) = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
    else {
        return;
    };
    if player.mounts.is_empty() {
        return;
    }
    for cell in all_positions() {
        if cell == player.pos {
            continue;
        }
        // Direction player → cell (exact octant); arc_bears rejects diagonals for
        // every non-turret arc, matching the cardinal-exact firing model.
        let Some(dir) = from_to(player.pos, cell) else {
            continue;
        };
        let bears = player
            .mounts
            .iter()
            .any(|m| arc_bears(player.facing, m.arc, dir));
        if bears {
            let q = grid_cell_quad(cell, cfg);
            outline_cell_2d(out, &q, WEAPON_ARC_OUTLINE);
        }
    }
}

/// Draw the empty 5×4 grid: each cell's wireframe trapezoid via the projector.
/// The front (player) row is drawn brighter so "near = where you are" reads at a
/// glance.
fn push_grid_2d(out: &mut Vec<DrawCommand>, cfg: &ProjectorConfig) {
    use crate::grid::Pos as GridPos;
    use crate::grid::{COLS, ROWS};
    for row in 0..ROWS {
        for col in 0..COLS {
            let q = grid_cell_quad(GridPos::new(col, row), cfg);
            let color = if row == ROWS - 1 {
                LANE_TICK // brighter front row
            } else {
                LANE_STROKE
            };
            outline_cell_2d(out, &q, color);
        }
    }
}

/// Outline a projected cell quad's four edges (the grid wireframe).
fn outline_cell_2d(out: &mut Vec<DrawCommand>, q: &CellQuad, color: [f32; 4]) {
    let c = q.corners;
    push_line(out, pt(c[0]), pt(c[1]), 1.0, color); // far (top) edge
    push_line(out, pt(c[1]), pt(c[2]), 1.0, color); // right edge
    push_line(out, pt(c[2]), pt(c[3]), 1.0, color); // near (bottom) edge
    push_line(out, pt(c[3]), pt(c[0]), 1.0, color); // left edge
}

/// `[f32; 2]` → the `perspective::Point2` the existing `push_line` takes.
#[inline]
fn pt(p: [f32; 2]) -> Point2 {
    Point2 { x: p[0], y: p[1] }
}

/// D3/#51: draw one ship at its projected cell. If `sprites` reports a loft mesh
/// for this ship ([`SpriteRegistry::loft_kind`]) the real 3-D hull is emitted as
/// a [`DrawCommand::LoftShip`] seated in the cell (the Aegis model for the
/// player); otherwise a flat faction-tinted placeholder box. Either way a
/// bow-direction arrow (encoding `Facing::forward_axis()`) + gold shield pips per
/// zone are drawn on top so orientation + buffer read regardless of body style.
/// (#70) The hull's tactical [`Facing`] as a flat GROUND-PLANE yaw offset
/// (degrees), composed on the chase-cam up-lane stern-on base in the loft render.
/// This is what makes the hull SHOW its orientation — the core hook (bow-on vs
/// broadside drives firing arcs). Relative to facing-N (bow up-lane toward the
/// VP = 0): `Bow(N)` toward the VP/up-lane → 0; `Bow(E)` toward higher col
/// (screen right) → +90 (bow to the right flank); `Bow(S)` toward the camera →
/// 180 (bow at the viewer); `Bow(W)` toward lower col (screen left) → −90 (bow to
/// the left flank); `Broadside(NorthSouth)` (hull along the lane) → 0 (reads
/// bow-on-ish up-lane); `Broadside(EastWest)` (hull across the lane) → +90
/// (flanks bear up-lane = the broadside/perpendicular stance).
///
/// All FLAT (a Y-axis ground yaw). The exact ± signs are CALIBRATED by capture
/// (all 4 cardinals must be DISTINCT + correct: N→VP, S→camera, E/W→perpendicular).
fn loft_facing_ground_yaw(facing: Facing) -> f32 {
    match facing {
        Facing::Bow(Dir4::N) => 0.0,
        Facing::Bow(Dir4::E) => 90.0,
        Facing::Bow(Dir4::S) => 180.0,
        Facing::Bow(Dir4::W) => -90.0,
        Facing::Broadside(Axis::NorthSouth) => 0.0,
        Facing::Broadside(Axis::EastWest) => 90.0,
    }
}

/// (#79) SHORTEST-PATH interpolate the ground-plane facing yaw from `from`→`to`
/// by `t∈[0,1]`, so a Q/E quarter-turn ROTATES the hull smoothly instead of
/// snapping ±90. Both endpoints are [`loft_facing_ground_yaw`]; the delta is
/// wrapped into `(−180, 180]` so e.g. a turn that numerically reads −270 takes
/// the +90 short way. `pub` so the bin (which owns the turn timer) builds the
/// interpolated [`VisualShip2d::facing_yaw_deg`]. Returns a yaw in degrees that
/// `chase_cam_ground_yaw_deg` consumes exactly like a snapped facing yaw.
pub fn lerp_facing_yaw_deg(from: Facing, to: Facing, t: f32) -> f32 {
    let a = loft_facing_ground_yaw(from);
    let b = loft_facing_ground_yaw(to);
    // Wrap (b - a) into (−180, 180] for the shortest arc.
    let mut delta = (b - a) % 360.0;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta <= -180.0 {
        delta += 360.0;
    }
    a + delta * t.clamp(0.0, 1.0)
}

fn push_ship_2d(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    cfg: &ProjectorConfig,
    sprites: &dyn SpriteRegistry,
    vis: Option<&VisualShip2d>,
) {
    let q = grid_cell_quad(ship.pos, cfg);
    let (fill, stroke) = if ship.faction == Faction::Player {
        (PLAYER_HULL_FILL, PLAYER_HULL_STROKE)
    } else {
        (ENEMY_HULL_FILL, ENEMY_HULL_STROKE)
    };
    // (#79) Mid-move/turn: use the bin's interpolated render position/facing so
    // the ship SLIDES + ROTATES; absent ⇒ snap to the logical cell.
    let center = vis.map(|v| v.center).unwrap_or(q.center);
    let near_edge_width = vis.map(|v| v.near_edge_width).unwrap_or_else(|| q.near_edge_width());
    let depth_scale = vis.map(|v| v.depth_scale).unwrap_or(q.depth_scale);
    let facing_yaw_deg = vis
        .map(|v| v.facing_yaw_deg)
        .unwrap_or_else(|| loft_facing_ground_yaw(ship.facing));
    // (#80) The cell's NEAR (bottom) edge screen-y — the loft hero hull seats its
    // base here so it FOLLOWS the cell up-lane on a move (was pinned to the HUD
    // band, which made the hull "drop off the grid" on any forward step).
    // `q.corners[3]` is the bottom-left (near edge); interpolated mid-slide.
    let near_edge_y = vis.map(|v| v.near_edge_y).unwrap_or(q.corners[3][1]);

    let is_player = ship.faction == Faction::Player;

    // (#67) v2 15-FACING WHEEL per `BROADSIDE_RENDER_CONTRACT_v2.md` §5: a ship's
    // visual orientation is one of 15 PRE-LIT baked PNG frames (3 hull fans × 5 lane
    // aims), shot at the fixed pitch-20 chase camera. The engine SWAPS to the frame
    // the wheel selects and draws it UNLIT — it never rotates the pixels (lights are
    // baked in world space) and never re-lights (contract §3). This REPLACES the old
    // 4-stance side/top path: Bruce confirmed the on-disk `aegis_bowOn*` 4-set is the
    // WRONG ship (a 1D-era bake, not his v2-json Aegis), so a clean placeholder beats
    // it until the correct `<class>_f00..f14` frames drop — at which point they
    // auto-render here with no further wiring. Runtime glows still layer ON TOP (§1).
    //
    // PLAYER ONLY: the 15-set is player-centric (hull pointing up-lane / banked
    // left-right) and has NO toward-camera view, so enemies (bow-on, oncoming) are
    // NOT routed through the wheel — they fall to the flat-box placeholder below
    // pending a separate enemy bake (lead escalated to Bruce). Do NOT route enemies
    // here; do NOT re-add the removed runtime loft.
    let class = ship.klass.as_deref().unwrap_or("frigate");

    // (#70) LIVE-3D PLAYER: if a loft mesh is installed for the player (the Aegis
    // GLB via mesh_import — the faithful render render_aegis proved), emit a
    // LoftShip at the player's projected cell. gfx's loft pre-pass renders the
    // real 3D hull LIT (chase-cam posed: stern toward camera, bow up-lane, engine
    // glow bright) and blits it into the lane; HUD overlays + the engine-glow cue
    // still layer on top below. This takes precedence over the sprite/flat-box —
    // it's the "Aegis flies in-game" path. Enemies are NOT routed here (loft_kind
    // returns None for them: no enemy mesh installed — oncoming bake is a
    // follow-up), so they stay on the flat-box placeholder.
    if is_player {
        if let Some(loft_kind) = sprites.loft_kind(&ship.id, true) {
            // (#76b Bruce) FOREGROUND HERO: anchor the hull LOW — its BOTTOM seats
            // just above the bottom HUD band, so the ship sits in the chase-cam
            // foreground and the PLAYFIELD (grid + enemies) reads ABOVE it. Was
            // centred on the cell's mid-screen projection (`cy = center.y`), which
            // floated the big hull at mid-screen and OCCLUDED the grid + the back-row
            // enemies (Bruce: "sits too high off the grid ... can't see the enemy
            // ships"). The hull's tactical facing still lives in the loft texture;
            // only this dest-rect's screen anchor changes — `aim_at` stays the true
            // cell centre so the chase-cam lane-aim is unaffected.
            //
            // Trimmed 1.9 → 1.0 × the near-row cell width so the hull stays in the
            // lower band and its TOP clears the far/enemy rows (which project up
            // near the horizon ~y120-150): at 1.0 the hull top lands ~y155, BELOW
            // the back-row enemies (~y140), so the playfield reads above it (Bruce:
            // "can't see the enemy ships"). Height from the loft aspect (#74, no
            // squash). x stays centred on the cell column so lateral moves read.
            let w = (near_edge_width * 1.0).max(16.0);
            let h = w / LOFT_TEXTURE_ASPECT;
            // (#80 Bruce) SEAT THE HULL ON ITS CELL — base at the cell's near
            // (bottom) edge, extend UP by h, so the hero hull FOLLOWS its
            // projected cell: it rides up-lane + shrinks as it advances, like any
            // cell occupant. Was pinned to the bottom HUD band regardless of row
            // (#78): at the front-row spawn that ~matched the cell, but on ANY
            // forward move the cell went up-lane while the hull stayed at the
            // bottom — reading as "the ship dropped off the grid below the
            // playfield" (Bruce live: POS 2,2 yet the hull pinned at the band,
            // the threatened cell up-lane). Keep the 1.0× width (the GOOD half of
            // #78); x + bottom use the INTERPOLATED cell so the #79 slide follows
            // the lane in both axes.
            let b = near_edge_y;
            let t = b - h;
            let (l, r) = (center[0] - w * 0.5, center[0] + w * 0.5);
            out.push(DrawCommand::LoftShip(LoftShipInstance {
                p0: [l, t],
                p1: [r, t],
                p2: [r, b],
                p3: [l, b],
                ship_id: SpriteSlug::new(&ship.id),
                kind: loft_kind,
                // (#70) Aim the nose from the true CELL centre (not the dragged-
                // down hero quad) — keeps the chase-cam lane-aim small + correct.
                // (#79) Mid-slide this is the interpolated centre so the lane-aim
                // tracks the sliding ship.
                aim_at: center,
                // (#70) The hull SHOWS its facing as a flat ground-plane yaw (the
                // core hook): N→up-lane/VP, S→camera, E/W→broadside flanks. (#79)
                // Mid-turn this is the interpolated yaw so the hull rotates smoothly.
                facing_yaw_deg,
            }));
            // Pips/buffer cues on top (the lit hull + its baked engine glow own
            // the hull + stern read, so the player chevron stays dropped).
            push_ship_arrow_and_pips_2d(out, ship, center, 22.0 * depth_scale, cfg);
            return;
        }
        // aim lane = the ship's OWN board column; fan = its own-forward board dir
        // (see `facing_wheel::player_facing15`). One of 15.
        let facing = crate::facing_wheel::player_facing15(ship.facing, ship.pos.col);
        if sprites.has_facing(class, facing.index()) {
            // The PLAYER is the hero foreground element (big, bottom-centre). Quad
            // from the cell's near-edge width × a hero factor, clamped above the
            // bottom HUD band so the ship clears the status strip (#64). Baked
            // facing frames read ~2:1 (length:height) like the old side art.
            let w = (near_edge_width * 1.9).max(16.0);
            let h = w * 0.5;
            // (#76 scene-res) Clamp above the bottom HUD band of the LIVE scene.
            let band_top = crate::gfx::scene_h() as f32 - 40.0;
            // PIVOT (#67 / contract §5): the wheel registers each facing against the
            // hull's board-center. Until the bake ships per-facing trim metadata,
            // anchor on the quad center (the loader uploads untrimmed frames, so the
            // geometric center IS the board-center — revisit when the bake lands).
            let cy = center[1].min(band_top - h * 0.5 - 2.0);
            let (l, r) = (center[0] - w * 0.5, center[0] + w * 0.5);
            let (t, b) = (cy - h * 0.5, cy + h * 0.5);
            // ONE pre-lit frame — no side/top blend (each facing is already the final
            // pitch-20 view). Feed the same slug to both texture slots with blend_t=0
            // so the pipeline samples a single texture (the blend is a no-op).
            let slug = crate::facing_wheel::facing_slug(class, facing);
            out.push(DrawCommand::TexturedShip(TexturedShipInstance {
                p0: [l, t],
                p1: [r, t],
                p2: [r, b],
                p3: [l, b],
                blend_t: 0.0,
                side: SpriteSlug::new(&slug),
                top: SpriteSlug::new(&slug),
            }));
            // Runtime engine glow ON TOP (contract: additive runtime effect, NOT
            // baked) at the player's stern (the sprite's lower edge).
            let glow_y = (b - h * 0.10).min(band_top - 4.0);
            push_engine_glow_2d(out, [center[0], glow_y], w);
            // Pips/buffer cues on top; the baked frame owns the hull + bow read, so
            // the player chevron stays dropped (enemy chevrons still telegraph).
            push_ship_arrow_and_pips_2d(out, ship, center, 22.0 * depth_scale, cfg);
            return;
        }
        // else: no facing frame loaded yet (the correct bake hasn't dropped) → fall
        // through to the clean flat-box placeholder. Placeholder > wrong ship.
    }

    // (#89) LIVE-3D ENEMY: if an enemy loft mesh is installed (the RED-tinted Aegis
    // hull via install_enemy_glb), emit a LoftShip at the enemy's projected cell so
    // the fleet renders as the player's ship-class in a hostile colour instead of
    // the flat box (Bruce). Unlike the player's hero-foreground anchor, an enemy
    // seats ON its own cell quad and scales with depth (far enemies smaller), so
    // the back-row fleet reads in perspective. Enemies face the player (bow-on /
    // oncoming) — facing_yaw_deg carries the Bow(S)=180 toward-camera pose. The
    // loft pre-pass renders the hull lit + posed; HUD overlays layer on top below.
    if !is_player {
        if let Some(loft_kind) = sprites.loft_kind(&ship.id, false) {
            // Seat on the cell: width from the cell's near-edge (already perspective-
            // scaled), height from the loft aspect (#74 no squash). Centred on the
            // cell centre (not the bottom band — enemies aren't the foreground hero).
            let w = (near_edge_width * 0.92).max(10.0);
            let h = w / LOFT_TEXTURE_ASPECT;
            let (l, r) = (center[0] - w * 0.5, center[0] + w * 0.5);
            let (t, b) = (center[1] - h * 0.5, center[1] + h * 0.5);
            out.push(DrawCommand::LoftShip(LoftShipInstance {
                p0: [l, t],
                p1: [r, t],
                p2: [r, b],
                p3: [l, b],
                ship_id: SpriteSlug::new(&ship.id),
                kind: loft_kind,
                aim_at: center,
                // Enemy facing as the flat ground-plane yaw — Bow(S)=180 renders the
                // hull oncoming (bow toward the camera/player), the bow-to-bow read.
                facing_yaw_deg,
            }));
            // Bow chevron + shield pips still layer on top so the enemy's threat/
            // orientation telegraph reads over the lit hull.
            push_ship_arrow_and_pips_2d(out, ship, center, 22.0 * depth_scale, cfg);
            return;
        }
    }

    // Inset hull box, scaled by depth so far ships are smaller. Bow stance is
    // longer along the bow axis; broadside is wider across the hull axis — a
    // coarse stance read under the (always-present) arrow.
    let base = 22.0 * depth_scale;
    let (hx, hy) = match ship.facing {
        Facing::Bow(d) => match d.axis() {
            Axis::NorthSouth => (base * 0.62, base),
            Axis::EastWest => (base, base * 0.62),
        },
        Facing::Broadside(axis) => match axis {
            Axis::EastWest => (base, base * 0.5),
            Axis::NorthSouth => (base * 0.5, base),
        },
    };
    let hull = [
        [center[0] - hx, center[1] - hy],
        [center[0] + hx, center[1] - hy],
        [center[0] + hx, center[1] + hy],
        [center[0] - hx, center[1] + hy],
    ];
    push_polygon(
        out,
        PolygonInstance::flat(hull, fill, atlas::cell_uvs(atlas::SOLID_WHITE)),
    );
    for i in 0..4 {
        push_line(out, pt(hull[i]), pt(hull[(i + 1) % 4]), 1.0, stroke);
    }

    push_ship_arrow_and_pips_2d(out, ship, center, base, cfg);
}

/// Bow-direction arrow + per-zone shield pips for a ship, shared by both the
/// flat-box and the loft (#51) body paths so orientation + the shield buffer
/// read identically regardless of how the hull itself is drawn. `base` is the
/// hull half-extent (22 × depth_scale) the arrow reach + pip placement key off.
fn push_ship_arrow_and_pips_2d(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    center: [f32; 2],
    base: f32,
    cfg: &ProjectorConfig,
) {
    // (#99) The arrow direction follows the GROUND-PLANE lane (perspective), not a
    // pure screen axis — so it lies FLAT in the grid (Bruce). Derive it from the
    // projected step to the next cell in the ship's facing; falls back to the
    // screen-axis dir at the grid edge (no neighbour to project).
    let (dx, dy) = ground_facing_dir(ship, center, cfg).unwrap_or_else(|| bow_screen_dir(ship.facing));
    // Bow-direction chevron — ENEMY ONLY in the chase view. The enemy's facing
    // varies and matters (telegraph read), so it keeps the chevron; the PLAYER's is
    // dropped (the hero hull + its motion carry the player's facing). Pips both.
    if ship.faction != Faction::Player {
        let rot = dy.atan2(dx);
        // (#99 Bruce) SMALLER than before (was base*0.7, floor 7) — it was bulky.
        let arrow_sz = (base * 0.5).max(5.0);
        // (#99 Bruce) On the ship's CENTRE LINE: seat it a short reach along the
        // facing dir from the hull centre (closer than the old base+sz+5 push), so
        // it reads as the ship's heading marker, not a far-flung floating arrow.
        let reach = base * 0.55 + arrow_sz;
        let tip = [center[0] + dx * reach, center[1] + dy * reach];
        let (uv0, uv1) = atlas::cell_uvs(atlas::BOW_CHEVRON);
        // Shadow first (pulled back toward the hull by ~1px so it haloes the mark).
        push_sprite(
            out,
            SpriteInstance {
                pos: [tip[0] - dx * 1.0, tip[1] - dy * 1.0],
                half_size: [arrow_sz * 1.18, arrow_sz * 1.18],
                color: BOW_MARK_SHADOW,
                uv_min: uv0,
                uv_max: uv1,
                rotation_rad: rot,
                _pad: [0.0; 3],
            },
        );
        push_sprite(
            out,
            SpriteInstance {
                pos: tip,
                half_size: [arrow_sz, arrow_sz],
                color: BOW_MARK_ENEMY,
                uv_min: uv0,
                uv_max: uv1,
                rotation_rad: rot,
                _pad: [0.0; 3],
            },
        );
    }

    push_shield_pips_2d(out, ship, center, base, (dx, dy));
}

/// (#99) The ship's facing direction as a SCREEN unit vector that lies FLAT in the
/// perspective grid — the projected step from the ship's cell to the adjacent cell
/// in its `Dir4` facing. Returns `None` at the grid edge (the neighbour is
/// off-board), where the caller falls back to the pure screen-axis direction.
fn ground_facing_dir(ship: &Ship, center: [f32; 2], cfg: &ProjectorConfig) -> Option<(f32, f32)> {
    use crate::grid::Dir4;
    let dir4 = match ship.facing {
        Facing::Bow(d) => d,
        // A broadside hull's "forward" for the marker = its axis's canonical dir.
        Facing::Broadside(axis) => match axis {
            Axis::NorthSouth => Dir4::S,
            Axis::EastWest => Dir4::E,
        },
    };
    let next = crate::grid::offset(ship.pos, dir4.to_dir8(), 1)?; // None at the board edge
    let nc = grid_cell_quad(next, cfg).center;
    let (vx, vy) = (nc[0] - center[0], nc[1] - center[1]);
    let len = (vx * vx + vy * vy).sqrt();
    if len < 1e-3 {
        return None;
    }
    Some((vx / len, vy / len))
}

/// (#62) The player's stern ENGINE-GLOW cluster — the reference ship's signature
/// "it's a ship from behind" read. Drawn at `centre` (the hull's lower edge,
/// toward the camera) sized to the hull width `hull_w`: a wider bottom row of
/// thrusters plus a couple above, each a bright cyan CORE over a larger soft
/// HALO so the cluster glows. Player-only (enemies are seen bow-on, up-lane).
fn push_engine_glow_2d(out: &mut Vec<DrawCommand>, center: [f32; 2], hull_w: f32) {
    // Thruster cluster sized to a COMPACT fraction of the hull width but CLAMPED so
    // it stays a tidy engine bank, not a giant pale-cyan wash (the hero hull is
    // huge, so scaling 1:1 with hull_w ballooned the halos into the "pale slab" —
    // #62). The whole cluster spans ~`r` px; dots are small.
    let r = (hull_w * 0.20).clamp(10.0, 26.0);
    let step = r * 0.42;
    let dots = [
        (-2.0_f32, 0.35_f32, 1.15_f32), // (x in steps, y in steps, size mult)
        (0.0, 0.6, 1.5),
        (2.0, 0.35, 1.15),
        (-1.0, -0.5, 1.0),
        (1.0, -0.5, 1.0),
    ];
    let (uv0, uv1) = atlas::cell_uvs(atlas::SOLID_WHITE);
    let base = (r * 0.22).max(1.6);
    for (sx, sy, sz) in dots {
        let pos = [center[0] + sx * step, center[1] + sy * step];
        let core = base * sz;
        // Halo first (bigger, dim), then the bright core on top.
        push_sprite(
            out,
            SpriteInstance {
                pos,
                half_size: [core * 1.9, core * 1.9],
                color: ENGINE_GLOW_HALO,
                uv_min: uv0,
                uv_max: uv1,
                rotation_rad: 0.0,
                _pad: [0.0; 3],
            },
        );
        push_sprite(
            out,
            SpriteInstance {
                pos,
                half_size: [core, core],
                color: ENGINE_GLOW_CORE,
                uv_min: uv0,
                uv_max: uv1,
                rotation_rad: 0.0,
                _pad: [0.0; 3],
            },
        );
    }
}

/// The screen-space forward unit vector `(dx, dy)` the bow arrow points along,
/// from `Facing::forward_axis()` — the SAME forward axis the resolver's
/// `facing_zone` table uses (blueprint: "the renderer's bow-arrow MUST encode
/// the SAME forward axis"). `Bow(dir)` points its cardinal; `Broadside(axis)`
/// points along the axis's positive cardinal (a render choice — both flanks are
/// symmetric). y is screen-down: `N` (toward row 0 / up-board) is `-y`.
fn bow_screen_dir(facing: Facing) -> (f32, f32) {
    let dir4 = match facing {
        Facing::Bow(d) => d,
        Facing::Broadside(axis) => match axis {
            Axis::NorthSouth => Dir4::S,
            Axis::EastWest => Dir4::E,
        },
    };
    match dir4 {
        Dir4::N => (0.0, -1.0),
        Dir4::S => (0.0, 1.0),
        Dir4::E => (1.0, 0.0),
        Dir4::W => (-1.0, 0.0),
    }
}

/// Gold shield pips per zone (one per held charge): bow/stern along the forward
/// axis, port/starboard perpendicular to it, stacked ALONG each face so multiple
/// charges read as a row. Reads `ship.shield_profile.face(zone).charge`. `fwd`
/// is the screen forward unit vector from [`bow_screen_dir`].
fn push_shield_pips_2d(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    center: [f32; 2],
    base: f32,
    fwd: (f32, f32),
) {
    let (fx, fy) = fwd;
    let (px, py) = (-fy, fx); // perpendicular
    let pip = 1.8;
    let edge = base + 3.0;
    let step = pip * 2.0 + 1.0;
    let faces = [
        (HullZone::Bow, (fx, fy)),
        (HullZone::Stern, (-fx, -fy)),
        (HullZone::Starboard, (px, py)),
        (HullZone::Port, (-px, -py)),
    ];
    for (zone, (ox, oy)) in faces {
        let n = ship.shield_profile.face(zone).charge;
        if n <= 0 {
            continue;
        }
        let (sx, sy) = (-oy, ox); // stack along the face
        let bx = center[0] + ox * edge;
        let by = center[1] + oy * edge;
        let start = -(n as f32 - 1.0) * 0.5;
        for i in 0..n {
            let k = start + i as f32;
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [bx + sx * step * k, by + sy * step * k],
                    [pip, pip],
                    SHIELD_PIP_CHARGE,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
        }
    }
}

/// On-screen silhouette bounding box for a ship at the current view angle.
/// Returns `(width, total_h)` so overlay helpers (heat bar, shield pips,
/// queue glyphs, status badges) can position consistently against the
/// current silhouette regardless of stance or angle.
fn ship_bbox(_ship: &Ship, _view_angle_rad: f32) -> (f32, f32) {
    // Ships now render via the loft pipeline into a FIXED lane dest-rect
    // (`loft_dest_rect`), so the overlay HUD (heat / shield / queue / status)
    // must anchor to THAT footprint, not the old per-stance 2D
    // `scaled_ship_extent` (which the #44 loft seating decoupled from — the
    // cause of the floating overlays in #45). One uniform box per ship: the
    // loft quad's full width × height, centred on the lane. Overlays offset
    // from these edges sit just outside the hull, consistent across stances.
    (
        LOFT_SHIP_HEIGHT_PX * LOFT_TEXTURE_ASPECT,
        LOFT_SHIP_HEIGHT_PX,
    )
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

        // Nebula patches at VARIED depths (#14) — not a single row. Each entry
        // is (x-fraction, y-fraction-down-the-wall, half-width, half-height,
        // alpha). Far clouds sit higher / smaller / dimmer; near clouds lower /
        // bigger / brighter, so the field reads layered in depth.
        let wall_top = horizon - back_wall_h;
        let nebulae = [
            // (xf,   yf,    hw,    hh,   alpha)  — far, mid, near
            (0.14_f32, 0.16_f32, 38.0_f32, 16.0_f32, 0.42_f32), // far: high, small, dim
            (0.50, 0.40, 64.0, 26.0, 0.55),                     // mid
            (0.78, 0.62, 88.0, 34.0, 0.66),                     // near: low, big, bright
        ];
        for (xf, yf, hw, hh, alpha) in nebulae {
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [w * xf, wall_top + back_wall_h * yf],
                    [hw, hh],
                    [1.0, 1.0, 1.0, alpha],
                    atlas::cell_uvs(atlas::PARALLAX_NEBULA),
                ),
            );
        }

        // Distant planet — sits HIGH and small (far depth), offset right and
        // clear of the nebula row so the background reads layered, not lined-up.
        let planet_size = 40.0;
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [w * 0.66, wall_top + back_wall_h * 0.20],
                [planet_size, planet_size],
                WHITE,
                atlas::cell_uvs(atlas::PARALLAX_DISTANT_PLANET),
            ),
        );

        // Far stars — 60 single-pixel sprites scattered across the wall.
        for i in 0..60u32 {
            let (sx, sy) = lcg_canvas_pos(i ^ 0xA53F_C1B5, sky_band);
            let alpha = 0.35 + 0.25 * lcg_unit(i ^ 0x1234_5678);
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [sx, sy],
                    [0.5, 0.5],
                    [1.0, 1.0, 1.0, alpha],
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
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
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [sx, sy],
                    [1.0, 1.0],
                    [1.0, 1.0, 1.0, alpha],
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
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
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [sx, sy],
                    [1.0, 1.0],
                    [0.85, 0.85, 1.0, alpha],
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
        }
        // Foreground dust tile sample at low-center of the floor for a
        // subtle near-camera detail. Hidden at low angles where the floor
        // is edge-on.
        if sin_a > 0.2 {
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [w * 0.40, horizon + floor_h * 0.75],
                    [32.0, 32.0],
                    [1.0, 1.0, 1.0, 0.55 * sin_a],
                    atlas::cell_uvs(atlas::PARALLAX_FOREGROUND_DUST),
                ),
            );
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
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [w / 2.0, lane.center_y],
            [w / 2.0, 0.75],
            LANE_STROKE,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    // Per-cell ticks — short vertical marks under the lane at each cell x.
    for c in 0..lane.cell_count {
        let p = cell_to_screen(c, lane);
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [p.x, lane.center_y + 5.0],
                [0.75, 4.0],
                LANE_TICK,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
}

/* =============================================================================
 * Range-band tick marks — short vertical ticks above the lane at each
 * cell, colored by the band that cell sits in relative to the player.
 * ============================================================================= */

fn push_range_band_ticks(out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
    let Some(player) = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
    else {
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
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [p.x, lane.center_y + 14.0],
                [1.25, 6.0],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
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
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [p.x, lane.center_y - 8.0],
                    [5.0, 5.0],
                    color,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
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

    // Loft path: if this ship has a live 3D asset (player demo dagger, or the
    // vendored CAD hull for enemies), emit a LoftShip blit quad at the
    // silhouette bbox and skip the 2D draw — "loft if the ship has a 3D asset,
    // else 2D", dispatched per-ship via the registry. The bbox is the same one
    // the 2D silhouette would occupy, so the 3D ship sits exactly where the 2D
    // one did. HUD overlays (heat/pips/glyphs) still draw on top below via the
    // caller. The quad carries the ship id (keys its animated pose in the
    // renderer) and the mesh kind.
    let is_player = ship.faction == Faction::Player;
    if let Some(loft_kind) = sprites.loft_kind(&ship.id, is_player) {
        // Loft ships use a dedicated lane-seated dest-rect (fixed height × the
        // loft texture aspect, centred on the lane) rather than the per-stance
        // 2D `scaled_ship_extent` bbox: the loft texture is content-centred, so
        // a lane-centred quad seats the hull ON the lane and keeps a consistent
        // size across stances (the 2D bbox's tall broadside quad dipped the ship
        // below the lane). The 3D pose/foreshortening lives inside the texture.
        let (left, top, right, bottom) = loft_dest_rect(cx, p.y);
        out.push(DrawCommand::LoftShip(LoftShipInstance {
            p0: [left, top],
            p1: [right, top],
            p2: [right, bottom],
            p3: [left, bottom],
            ship_id: SpriteSlug::new(&ship.id),
            kind: loft_kind,
            // Legacy 1-D path (not the live 2-D chase cam) — no VP convergence
            // here; aim from the lane point so the chase-cam yaw is a no-op.
            aim_at: [cx, p.y],
            // Legacy 1-D: keep the up-lane stern-on orientation (no facing yaw).
            facing_yaw_deg: 0.0,
        }));
        return;
    }

    // If the artist has painted both side + top PNGs for this ship's
    // class/stance, draw the textured quad instead of the procedural
    // silhouette. The bbox is the same — the shader samples both PNGs
    // and blends by sin(view_angle).
    let class = ship.klass.as_deref().unwrap_or("frigate");
    let sprite_stance = match ship.orientation {
        Orientation::BowOn { bow: LaneEnd::Fore } => SpriteStance::BowOnFore,
        Orientation::BowOn { bow: LaneEnd::Aft } => SpriteStance::BowOnAft,
        Orientation::Broadside => SpriteStance::Broadside,
    };
    if sprites.has_pair(class, sprite_stance) {
        let left = cx - width / 2.0;
        let right = cx + width / 2.0;
        let side_slug = format!(
            "{}_{}_{}",
            class,
            sprite_stance.slug(),
            SpriteView::Side.slug()
        );
        let top_slug = format!(
            "{}_{}_{}",
            class,
            sprite_stance.slug(),
            SpriteView::Top.slug()
        );
        out.push(DrawCommand::TexturedShip(TexturedShipInstance {
            p0: [left, top_y],
            p1: [right, top_y],
            p2: [right, base_y],
            p3: [left, base_y],
            blend_t: sin_a,
            side: SpriteSlug::new(&side_slug),
            top: SpriteSlug::new(&top_slug),
        }));
        // Skip chevron + procedural-silhouette art: the painted PNGs
        // own bow direction and outline. Heat bars / shield pips /
        // queue glyphs / status badges still draw on top.
        return;
    }

    match stance {
        Stance::BowOn => {
            push_bow_on_silhouette(out, cx, base_y, top_y, width, cos_a, bow_fore, fill, stroke)
        }
        Stance::Broadside => {
            push_broadside_silhouette(out, cx, base_y, top_y, width, cos_a, fill, stroke)
        }
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
        push_sprite(
            out,
            SpriteInstance {
                pos: [chx, chy],
                half_size: [chevron_size, chevron_size],
                color: chev_color,
                uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
                uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
                rotation_rad: chrot,
                _pad: [0.0; 3],
            },
        );
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
    push_polygon(
        out,
        PolygonInstance {
            p0: [left, top_y],
            p1: [right, top_y],
            p2: [right, base_y],
            p3: [left, base_y],
            color: fill,
            uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
            uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        },
    );
    // Bow triangle (degenerate-quad with two coincident vertices at tip).
    push_polygon(
        out,
        PolygonInstance {
            p0: [bow_corner_x, top_y],
            p1: [bow_tip_x, mid_y],
            p2: [bow_tip_x, mid_y],
            p3: [bow_corner_x, base_y],
            color: fill,
            uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
            uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        },
    );

    // Outline strokes around the full silhouette (no internal seam).
    // Stern edge.
    push_line(
        out,
        Point2 {
            x: stern_edge_x,
            y: top_y,
        },
        Point2 {
            x: stern_edge_x,
            y: base_y,
        },
        1.0,
        stroke,
    );
    // Top edge (stern_edge_x -> bow_corner_x).
    push_line(
        out,
        Point2 {
            x: stern_edge_x,
            y: top_y,
        },
        Point2 {
            x: bow_corner_x,
            y: top_y,
        },
        1.0,
        stroke,
    );
    // Bottom edge.
    push_line(
        out,
        Point2 {
            x: stern_edge_x,
            y: base_y,
        },
        Point2 {
            x: bow_corner_x,
            y: base_y,
        },
        1.0,
        stroke,
    );
    // Bow taper edges. When cos_a is near 0 these collapse to a vertical
    // line at bow_corner_x; that's fine — no visible seam because they
    // coincide.
    push_line(
        out,
        Point2 {
            x: bow_corner_x,
            y: top_y,
        },
        Point2 {
            x: bow_tip_x,
            y: mid_y,
        },
        1.0,
        stroke,
    );
    push_line(
        out,
        Point2 {
            x: bow_corner_x,
            y: base_y,
        },
        Point2 {
            x: bow_tip_x,
            y: mid_y,
        },
        1.0,
        stroke,
    );
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
    push_polygon(
        out,
        PolygonInstance {
            p0: [cx - half_w, top_y],
            p1: [cx + half_w, top_y],
            p2: [cx + half_w, base_y],
            p3: [cx - half_w, base_y],
            color: fill,
            uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
            uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        },
    );
    // Superstructure bump: short rectangle perched on top, centered.
    // Height scales with cos(angle) so it reads strongly at side view and
    // recedes at top-down (where the bump would be foreshortened away).
    let bump_w = width * 0.4;
    let bump_h = height * 0.30 * cos_a.max(0.1);
    push_polygon(
        out,
        PolygonInstance {
            p0: [cx - bump_w / 2.0, top_y - bump_h],
            p1: [cx + bump_w / 2.0, top_y - bump_h],
            p2: [cx + bump_w / 2.0, top_y],
            p3: [cx - bump_w / 2.0, top_y],
            color: fill,
            uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
            uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        },
    );

    // Outlines.
    let main = [
        Point2 {
            x: cx - half_w,
            y: top_y,
        },
        Point2 {
            x: cx + half_w,
            y: top_y,
        },
        Point2 {
            x: cx + half_w,
            y: base_y,
        },
        Point2 {
            x: cx - half_w,
            y: base_y,
        },
    ];
    for i in 0..4 {
        push_line(out, main[i], main[(i + 1) % 4], 1.0, stroke);
    }
    let bump = [
        Point2 {
            x: cx - bump_w / 2.0,
            y: top_y - bump_h,
        },
        Point2 {
            x: cx + bump_w / 2.0,
            y: top_y - bump_h,
        },
        Point2 {
            x: cx + bump_w / 2.0,
            y: top_y,
        },
        Point2 {
            x: cx - bump_w / 2.0,
            y: top_y,
        },
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
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [x_right - max_w / 2.0, y],
            [max_w / 2.0, bar_h / 2.0],
            [0.08, 0.12, 0.18, 0.85],
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    // Fill.
    if cur_w > 0.5 {
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [x_right - max_w + cur_w / 2.0, y],
                [cur_w / 2.0, bar_h / 2.0],
                [0.33, 0.81, 0.79, 1.0],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
    // Tick marks at each fixed angle (0, 15, 30, 45, 60, 75, 90).
    for i in 0..=6 {
        let tick_x = (x_right - max_w) + (i as f32 / 6.0) * max_w;
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [tick_x, y + bar_h + 2.0],
                [0.5, 2.0],
                [0.55, 0.50, 0.45, 1.0],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
}

/// Thin line segment from `a` to `b` as a rotated rectangle of width `thickness`.
fn push_line(out: &mut Vec<DrawCommand>, a: Point2, b: Point2, thickness: f32, color: [f32; 4]) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    let cx = (a.x + b.x) / 2.0;
    let cy = (a.y + b.y) / 2.0;
    push_sprite(
        out,
        SpriteInstance {
            pos: [cx, cy],
            half_size: [len / 2.0, thickness / 2.0],
            color,
            uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
            uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
            rotation_rad: dy.atan2(dx),
            _pad: [0.0; 3],
        },
    );
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
    let rot = if proj.heading == LaneEnd::Aft {
        std::f32::consts::PI
    } else {
        0.0
    };
    push_sprite(
        out,
        SpriteInstance {
            pos: [pos.x, lane.center_y - 18.0],
            half_size: [16.0, 8.0],
            color: WHITE,
            uv_min: atlas::cell_uvs(cell).0,
            uv_max: atlas::cell_uvs(cell).1,
            rotation_rad: rot,
            _pad: [0.0; 3],
        },
    );
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
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [bar_x, bar_y - max_h / 2.0],
            [bar_w / 2.0, max_h / 2.0],
            HEAT_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    let ratio = (ship.heat as f32 / ship.heat_max.max(1) as f32).clamp(0.0, 1.0);
    if ratio > 0.0 {
        let fill_h = max_h * ratio;
        let color = if ship.locked_out {
            HEAT_LOCKOUT
        } else {
            HEAT_FILL
        };
        // Bottom-aligned: fill grows upward from the bar's bottom edge.
        let bottom_y = bar_y - max_h / 2.0 + max_h; // = bar_y + max_h/2
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [bar_x, bottom_y - fill_h / 2.0],
                [bar_w / 2.0, fill_h / 2.0],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
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
    let bow_sign = if bow_fore || stance_broadside {
        1.0
    } else {
        -1.0
    };

    let zones = [
        // (zone, base position, stacking direction)
        (
            HullZone::Bow,
            Point2 {
                x: p.x + bow_sign * (width / 2.0 + pad),
                y: lane.center_y,
            },
            Point2 {
                x: bow_sign * (pip * 2.0 + 1.0),
                y: 0.0,
            },
        ),
        (
            HullZone::Stern,
            Point2 {
                x: p.x - bow_sign * (width / 2.0 + pad),
                y: lane.center_y,
            },
            Point2 {
                x: -bow_sign * (pip * 2.0 + 1.0),
                y: 0.0,
            },
        ),
        (
            HullZone::Starboard,
            Point2 {
                x: p.x,
                y: lane.center_y + total_h / 2.0 + pad,
            },
            Point2 {
                x: 0.0,
                y: pip * 2.0 + 1.0,
            },
        ),
        (
            HullZone::Port,
            Point2 {
                x: p.x,
                y: lane.center_y - total_h / 2.0 - pad,
            },
            Point2 {
                x: 0.0,
                y: -(pip * 2.0 + 1.0),
            },
        ),
    ];
    for (zone, base, step) in zones {
        let face = ship.shield_profile.face(zone);
        if face.charge <= 0 {
            continue;
        }
        for i in 0..face.charge {
            let px = base.x + step.x * (i as f32);
            let py = base.y + step.y * (i as f32);
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [px, py],
                    [pip, pip],
                    SHIELD_PIP_CHARGE,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
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
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [start_x + (i as f32) * spacing, glyph_y],
                [glyph_size, glyph_size],
                WHITE,
                atlas::cell_uvs(cell_uv),
            ),
        );
    }
}

fn archetype_of_mount(ship: &Ship, action_id: &str) -> Option<WeaponArchetype> {
    let _ = ship
        .mounts
        .iter()
        .find(|m: &&Mount| m.weapon == action_id)?;
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
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [start_x + (i as f32) * spacing, y],
                [size, size],
                WHITE,
                atlas::cell_uvs(cell_uv),
            ),
        );
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
            Faction::Enemy => any_enemy = true,
        }
    }
    if !any_player {
        WinState::Defeat
    } else if !any_enemy {
        WinState::Victory
    } else {
        WinState::Playing
    }
}

pub fn push_end_state_overlay(out: &mut Vec<DrawCommand>, state: WinState) {
    let (tint, banner) = match state {
        WinState::Playing => return,
        WinState::Defeat => (DEFEAT_TINT, "DEFEATED - PRESS ENTER TO RESTART"),
        WinState::Victory => (VICTORY_TINT, "VICTORY - PRESS ENTER TO RESTART"),
    };
    // (#76 scene-res) Full-canvas tinted overlay over the LIVE scene.
    let (cx, cy) = (
        crate::gfx::scene_w() as f32 / 2.0,
        crate::gfx::scene_h() as f32 / 2.0,
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [cx, cy],
            [cx, cy],
            tint,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_centered_banner(out, banner, cy, 4.0);
}

/* =============================================================================
 * Player danger legibility (#67) — hull readout + hit flash.
 *
 * The player needs to read their own peril at a glance: a prominent hull bar
 * (not just the tiny per-ship heat strip) plus a brief full-screen damage
 * flash when they take a hit, so a loss never comes out of nowhere.
 * ============================================================================= */

const PLAYER_HULL_BAR_BG: [f32; 4] = [0.08, 0.10, 0.14, 0.9];
const PLAYER_HULL_OK: [f32; 4] = [0.33, 0.81, 0.79, 1.0]; // teal, healthy
const PLAYER_HULL_HURT: [f32; 4] = [0.95, 0.62, 0.30, 1.0]; // amber, wounded
const PLAYER_HULL_CRIT: [f32; 4] = [0.95, 0.24, 0.22, 1.0]; // red, critical

/// A prominent player hull bar anchored bottom-left of the canvas. Reads
/// teal when healthy, amber under half, red at/under one-third, so the player
/// always knows how close to death they are. `hull`/`max_hull` come straight
/// off the player ship.
pub fn push_player_hull_bar(out: &mut Vec<DrawCommand>, hull: i32, max_hull: i32) {
    let max = max_hull.max(1) as f32;
    let cur = hull.clamp(0, max_hull) as f32;
    let ratio = (cur / max).clamp(0.0, 1.0);
    let bar_w = 140.0;
    let bar_h = 12.0;
    let x_left = 20.0;
    // (#76 scene-res) Anchor to the bottom of the LIVE scene.
    let y = crate::gfx::scene_h() as f32 - 28.0;
    // Track.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [x_left + bar_w / 2.0, y],
            [bar_w / 2.0, bar_h / 2.0],
            PLAYER_HULL_BAR_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    let color = if ratio <= 0.34 {
        PLAYER_HULL_CRIT
    } else if ratio <= 0.5 {
        PLAYER_HULL_HURT
    } else {
        PLAYER_HULL_OK
    };
    let fill_w = bar_w * ratio;
    if fill_w > 0.5 {
        push_sprite(
            out,
            SpriteInstance::axis_aligned(
                [x_left + fill_w / 2.0, y],
                [fill_w / 2.0, bar_h / 2.0],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ),
        );
    }
    // "HULL N/M" label above the bar.
    push_text_left(
        out,
        &format!("HULL {}/{}", hull.max(0), max_hull),
        x_left,
        y - bar_h / 2.0 - 12.0,
        1.5,
        WHITE,
    );
}

/// A brief full-canvas red flash when the player takes a hit. `intensity`
/// (0..1) is driven by the bin's hit-flash timer (decays after a hull drop).
/// No-op at zero so it costs nothing on quiet frames.
pub fn push_player_hit_flash(out: &mut Vec<DrawCommand>, intensity: f32) {
    if intensity <= 0.01 {
        return;
    }
    let alpha = 0.45 * intensity.clamp(0.0, 1.0);
    // (#76 scene-res) Cover the full LIVE scene.
    let (cx, cy) = (
        crate::gfx::scene_w() as f32 / 2.0,
        crate::gfx::scene_h() as f32 / 2.0,
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [cx, cy],
            [cx, cy],
            [0.95, 0.18, 0.16, alpha],
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
}

/// Run-end overlay used by the bin's `DemoState::RunDefeated` arm.
/// Like the Phase-1 [`push_end_state_overlay`] `Defeat` variant but
/// also surfaces the run's earned-salvage total so the player sees
/// what their meta-progression contribution was before dying.
pub fn push_run_defeated_overlay(out: &mut Vec<DrawCommand>, salvage: u32) {
    push_run_defeated_overlay_with_cause(out, salvage, None)
}

/// Like [`push_run_defeated_overlay`] but surfaces WHAT killed the player —
/// e.g. "DESTROYED BY GUNBOAT" — so a defeat reads as a comprehensible event
/// rather than just a red screen. `cause` is a short upper-case phrase the bin
/// derives from the killing blow (or `None` to omit the line).
pub fn push_run_defeated_overlay_with_cause(
    out: &mut Vec<DrawCommand>,
    salvage: u32,
    cause: Option<&str>,
) {
    // (#76 scene-res) Full-canvas defeat overlay over the LIVE scene.
    let center_x = crate::gfx::scene_w() as f32 / 2.0;
    let center_y = crate::gfx::scene_h() as f32 / 2.0;
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [center_x, center_y],
            [center_x, center_y],
            DEFEAT_TINT,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_centered_banner(out, "DEFEATED", center_y - 60.0, 5.0);
    // What killed you — so a loss is a readable event, not just a red screen.
    if let Some(cause) = cause {
        push_centered_banner(out, cause, center_y - 22.0, 2.5);
    }
    push_centered_banner(
        out,
        &format!("TOTAL SALVAGE: {}", salvage),
        center_y + 10.0,
        3.0,
    );
    push_centered_banner(out, "PRESS ENTER TO RESTART", center_y + 60.0, 2.5);
}

/// Top-right in-game salvage counter. Small inline-font readout that
/// stays present during `Playing` state so the player can verify the
/// counter ticks up on each encounter win. Pushes a single row of
/// 5×7 glyphs ~16px from the top-right canvas edge.
pub fn push_salvage_hud(out: &mut Vec<DrawCommand>, salvage: u32) {
    let banner = format!("SALVAGE: {}", salvage);
    let pixel = 2.0;
    let glyph_w_px = 5.0 * pixel;
    let space_px = pixel;
    let advance = glyph_w_px + space_px;
    let total_w: f32 = banner.len() as f32 * advance - space_px;
    let right_pad = 20.0;
    // (#76 scene-res) Right-align to the LIVE canvas edge.
    let start_x = crate::gfx::scene_w() as f32 - total_w - right_pad;
    let y = 8.0;
    for (i, ch) in banner.chars().enumerate() {
        let x = start_x + i as f32 * advance;
        push_glyph_5x7(out, ch, x, y, pixel, WHITE);
    }
}

/// (#70) Live player POS + FACING readout, top-right just under SALVAGE — the
/// ground-truth Bruce + the lead read instead of guessing from a capture. Shows
/// the board cell `(col,row)` and the cardinal facing the strafe/reorient
/// controls produce, so "press Right → col+1, facing unchanged" is verifiable
/// on screen. Small dim text (pixel=1) so it doesn't crowd the salvage banner.
pub fn push_player_readout(out: &mut Vec<DrawCommand>, pos: crate::grid::Pos, facing: Facing) {
    const DIM: [f32; 4] = [0.62, 0.70, 0.80, 0.85];
    let face = match facing {
        Facing::Bow(Dir4::N) => "N",
        Facing::Bow(Dir4::E) => "E",
        Facing::Bow(Dir4::S) => "S",
        Facing::Bow(Dir4::W) => "W",
        Facing::Broadside(Axis::NorthSouth) => "BNS",
        Facing::Broadside(Axis::EastWest) => "BEW",
    };
    let text = format!("POS {},{}  FACE {}", pos.col, pos.row, face);
    let pixel = 1.0;
    let advance = 5.0 * pixel + pixel; // glyph + 1px space (matches push_text_left)
    let total_w = text.len() as f32 * advance - pixel;
    let right_pad = 20.0;
    // (#76 scene-res) Right-align to the LIVE canvas edge.
    let start_x = crate::gfx::scene_w() as f32 - total_w - right_pad;
    // Just below the SALVAGE banner (salvage sits at y=8 with pixel=2 → ~14px
    // tall; place this at y=26).
    push_text_left(out, &text, start_x, 26.0, pixel, DIM);
}

/// (#76) Top-right resolution readout under the POS/FACE line: `SHIP <w>x<h>`
/// (the loft-render pixel size, cycled with `,`/`.`) and `SCENE <w>x<h>` (the
/// whole-scene offscreen size, cycled with `;`/`'`). Right-aligned, dim, so Bruce
/// sees the live res while tuning. Stacked at y=38 / y=48 (the POS/FACE line sits
/// at y=26).
pub fn push_res_readout(out: &mut Vec<DrawCommand>, ship: (u32, u32), scene: (u32, u32)) {
    const DIM: [f32; 4] = [0.62, 0.70, 0.80, 0.85];
    let pixel = 1.0;
    let advance = 5.0 * pixel + pixel;
    let right_pad = 20.0;
    // (#76 scene-res) Right-align to the LIVE canvas edge.
    let canvas_w = crate::gfx::scene_w() as f32;
    let right_align = |out: &mut Vec<DrawCommand>, text: &str, y: f32| {
        let total_w = text.len() as f32 * advance - pixel;
        let start_x = canvas_w - total_w - right_pad;
        push_text_left(out, text, start_x, y, pixel, DIM);
    };
    right_align(out, &format!("SHIP {}x{}", ship.0, ship.1), 38.0);
    right_align(out, &format!("SCENE {}x{}", scene.0, scene.1), 48.0);
}

/// Minimalist controls legend, bottom-left corner. Dim single-column text,
/// no background panel — just a quiet reminder of the keybindings that
/// doesn't crowd the lane. Labels mirror the bin's `keycode_to_key` /
/// `Intent` map (1/2/3 queue, arrows move, Tab reorient, V vent, Space fire).
pub fn push_controls_overlay(out: &mut Vec<DrawCommand>) {
    use crate::gfx::VIRTUAL_H;
    const DIM: [f32; 4] = [0.62, 0.70, 0.80, 0.55];
    // (#48) Shrunk to `pixel = 1.0` (one font-pixel = one virtual pixel) and
    // tucked tight into the bottom-left corner. At pixel = 2.0 the 5-line legend
    // ate ~1/3 of the screen (Bruce); at 1.0 each glyph is 5x7 px, the longest
    // line ("TAB REORIENT") spans ~72 px of the 480 frame (~15% width) and the
    // five lines ~45 px of 270 (~17% height) — a compact corner readout.
    let pixel = 1.0;
    let line_h = 7.0 * pixel + 2.0; // glyph height + tight inter-line gap
    let lines = ["1 2 3  QUEUE", "ARROWS MOVE", "TAB REORIENT", "V  VENT", "SPACE FIRE"];
    let left_pad = 4.0;
    let bottom_pad = 4.0;
    let start_y = VIRTUAL_H as f32 - line_h * lines.len() as f32 - bottom_pad;
    for (i, line) in lines.iter().enumerate() {
        push_text_left(out, line, left_pad, start_y + i as f32 * line_h, pixel, DIM);
    }
}

/* =============================================================================
 * Ability tiles (#53 redesign / #64) — square icon tiles.
 *
 * Each tile is a SQUARE: a placeholder archetype icon (reusing the atlas
 * GLYPH_* cells until real ability art lands), a damage pip-count along the
 * bottom, and a cooldown overlay (dim + remaining-turns) when cooling.
 *
 * PLAYER: a resting horizontal row BELOW the lane (always visible, cooldown
 * state). When an ability is QUEUED it animates UP into a vertical stack above
 * the player ship, in queue order; on dequeue it animates back down. The
 * below↔above position is tweened by [`AbilityHud`] (stateful, advanced by the
 * bin each frame), keyed by slot.
 *
 * ENEMY: a vertical stack ABOVE the enemy of the abilities it's readying
 * (telegraphed intent = its queued action). No below-lane row; tiles on
 * cooldown are hidden (they appear only when readying). Enemy tiles are
 * stateless — emitted directly from the enemy's current queue.
 *
 * The bin assembles the [`AbilityTile`] list (Content for icon/damage/cooldown,
 * the ship for live cooldown + queue order); hud lays them out + animates.
 * ============================================================================= */

/// Placeholder ability icon — maps to an atlas archetype glyph cell until real
/// per-ability art exists. Keeping it an enum (not a raw cell) makes the
/// real-art swap a one-spot change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbilityIcon {
    Beam,
    Ordnance,
    Broadside,
    Displacement,
    Control,
    Movement,
    Defensive,
}

impl AbilityIcon {
    fn atlas_cell(self) -> (u32, u32) {
        match self {
            AbilityIcon::Beam => atlas::GLYPH_BEAM,
            AbilityIcon::Ordnance => atlas::GLYPH_ORDNANCE,
            AbilityIcon::Broadside => atlas::GLYPH_BROADSIDE,
            AbilityIcon::Displacement => atlas::GLYPH_DISPLACEMENT,
            AbilityIcon::Control => atlas::GLYPH_CONTROL,
            AbilityIcon::Movement => atlas::GLYPH_MOVEMENT,
            AbilityIcon::Defensive => atlas::GLYPH_DEFENSIVE,
        }
    }
}

/// One ability, flattened for display. The bin fills this from a ship's mounts
/// and field-kit cards: icon/damage/cooldown-max via the catalog action defs,
/// `cooldown` via `Ship::cooldowns`, `queued_index` from the ship's queue.
#[derive(Clone, Debug)]
pub struct AbilityTile {
    /// Input key (`'1'`..`'3'`, `'5'`..`'7'`) — drawn small in a corner.
    pub slot: char,
    /// Placeholder archetype icon.
    pub icon: AbilityIcon,
    /// Damage figure for the damage indicator (`0` = non-damage ability).
    pub damage: i32,
    /// (#98) RANGE in cells = the weapon's max reach (Adjacent=1, Near=2, Far=3),
    /// shown top-right. `0` = no meaningful range (non-targeted / self) — blank.
    pub range: i32,
    /// Turns remaining on cooldown (`0` = ready).
    pub cooldown: i32,
    /// Cooldown length when fired; `0` = no cooldown.
    pub cooldown_max: i32,
    /// `Some(i)` when this ability is queued at position `i` (0-based); `None`
    /// when resting. Drives the below-lane ↔ above-ship animation target.
    pub queued_index: Option<usize>,
}

const TILE_READY: [f32; 4] = [0.329, 0.812, 0.788, 1.0]; // teal (1-D emit_tile path)
// (#98) Ready 2-D tile = WHITE border ("queue me"); charging tile = dim violet.
const TILE_BORDER_READY: [f32; 4] = [0.96, 0.98, 1.0, 1.0]; // white = ready to queue
const TILE_COOLDOWN: [f32; 4] = [0.42, 0.40, 0.50, 1.0]; // dim violet = on CD
const TILE_BG: [f32; 4] = [0.094, 0.110, 0.149, 0.92];
const TILE_DAMAGE: [f32; 4] = [0.95, 0.62, 0.30, 1.0]; // orange damage pips
const TILE_RANGE: [f32; 4] = [0.62, 0.82, 0.95, 1.0]; // cool blue = range cells
const TILE_ICON: [f32; 4] = [0.92, 0.94, 0.98, 1.0];
const TILE_ENEMY: [f32; 4] = [0.90, 0.34, 0.30, 1.0]; // enemy-intent red frame
// (#98) Cooldown TICKS along a tile's bottom edge: white = elapsed/ready round,
// grey = a round still remaining.
const TILE_TICK_ELAPSED: [f32; 4] = [0.92, 0.94, 0.98, 1.0]; // white = ready/elapsed
const TILE_TICK_REMAIN: [f32; 4] = [0.40, 0.42, 0.50, 1.0]; // grey = round remaining

/// Square tile edge (virtual px).
const TILE_SIZE: f32 = 30.0;
const TILE_GAP: f32 = 6.0;
/// How fast a tile slides below↔above on queue/dequeue (seconds for the full
/// trip). Snappy — Shogun tiles pop.
const TILE_TWEEN_SECS: f32 = 0.18;

/// Stateful player ability-tile layout + animation. Holds a per-slot lerp
/// (`0.0` = resting below the lane, `1.0` = docked in the above-ship queue
/// stack) so queue/dequeue animate. The bin advances it each frame and emits.
#[derive(Default)]
pub struct AbilityHud {
    /// slot char → current animated position fraction (0 below ↔ 1 above).
    phase: std::collections::HashMap<char, f32>,
}

impl AbilityHud {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance each slot's position toward its target (queued → 1, resting → 0)
    /// by `dt`. `tiles` gives the current targets. Returns `true` while any tile
    /// is mid-transition (redraw-keepalive).
    pub fn advance(&mut self, tiles: &[AbilityTile], dt: f32) -> bool {
        let step = if TILE_TWEEN_SECS > 0.0 {
            dt / TILE_TWEEN_SECS
        } else {
            1.0
        };
        let mut animating = false;
        for t in tiles {
            let target = if t.queued_index.is_some() { 1.0 } else { 0.0 };
            let cur = self.phase.entry(t.slot).or_insert(target);
            if (*cur - target).abs() > 1e-3 {
                let dir = (target - *cur).signum();
                *cur = (*cur + dir * step).clamp(0.0, 1.0);
                if (*cur - target).abs() > 1e-3 {
                    animating = true;
                }
            } else {
                *cur = target;
            }
        }
        animating
    }

    /// Emit the player's ability tiles, each interpolated between its resting
    /// below-lane slot and its above-ship queue-stack slot. `anchor_x` is the
    /// player ship's screen x.
    pub fn emit_player(
        &self,
        out: &mut Vec<DrawCommand>,
        tiles: &[AbilityTile],
        anchor_x: f32,
        lane: &LaneGeometry,
    ) {
        // Resting row: centred below the lane.
        let resting_y = lane.center_y + 80.0;
        let row_w = tiles.len() as f32 * TILE_SIZE + (tiles.len() as f32 - 1.0) * TILE_GAP;
        let row_left = anchor_x - row_w / 2.0;
        // Above-ship queue stack: vertical, climbing up from just above the ship.
        let stack_top = lane.center_y - 120.0;
        for (i, t) in tiles.iter().enumerate() {
            let rest_x = row_left + i as f32 * (TILE_SIZE + TILE_GAP) + TILE_SIZE / 2.0;
            let rest = [rest_x, resting_y];
            // Queue slot (if queued): stacked above the ship in queue order.
            let above = match t.queued_index {
                Some(qi) => [anchor_x, stack_top - qi as f32 * (TILE_SIZE + TILE_GAP)],
                None => rest, // not queued → target is its resting slot
            };
            let ph = self.phase.get(&t.slot).copied().unwrap_or(0.0);
            let pos = [lerp(rest[0], above[0], ph), lerp(rest[1], above[1], ph)];
            emit_tile(out, t, pos, false);
        }
    }
}

/// Emit an ENEMY's telegraph stack: a vertical column ABOVE the enemy of the
/// abilities it's readying (`queued_index.is_some()`), skipping any on cooldown
/// (hidden until ready). Stateless — straight from the enemy's current queue.
pub fn push_enemy_telegraph(
    out: &mut Vec<DrawCommand>,
    tiles: &[AbilityTile],
    enemy_x: f32,
    lane: &LaneGeometry,
) {
    let stack_top = lane.center_y - 96.0;
    let mut shown = 0usize;
    for t in tiles {
        // Only abilities the enemy is readying, and not on cooldown.
        if t.queued_index.is_none() || t.cooldown > 0 {
            continue;
        }
        let pos = [enemy_x, stack_top - shown as f32 * (TILE_SIZE + TILE_GAP)];
        emit_tile(out, t, pos, true);
        shown += 1;
    }
}

/// Draw one square tile centred at `pos`: background, archetype icon, damage
/// pips, slot key, and the cooldown overlay when cooling. `enemy` tints the
/// frame red (telegraph) vs the teal player frame.
fn emit_tile(out: &mut Vec<DrawCommand>, t: &AbilityTile, pos: [f32; 2], enemy: bool) {
    let half = TILE_SIZE / 2.0;
    let ready = t.cooldown <= 0;
    let frame = if enemy {
        TILE_ENEMY
    } else if ready {
        TILE_READY
    } else {
        TILE_COOLDOWN
    };
    // Frame (slightly larger) + inner background.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half + 1.5, half + 1.5],
            frame,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half, half],
            TILE_BG,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    // Archetype icon, centred, dimmed when on cooldown.
    let icon_color = if ready {
        TILE_ICON
    } else {
        [0.5, 0.5, 0.58, 1.0]
    };
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [pos[0], pos[1] - 2.0],
            [half * 0.7, half * 0.7],
            icon_color,
            atlas::cell_uvs(t.icon.atlas_cell()),
        ),
    );
    // Damage pips along the bottom edge (cap at 5 so the row fits).
    if t.damage > 0 {
        let pips = t.damage.min(5);
        let pip = 2.5;
        let total = pips as f32 * pip + (pips as f32 - 1.0) * 1.5;
        let mut px = pos[0] - total / 2.0 + pip / 2.0;
        let py = pos[1] + half - 4.0;
        for _ in 0..pips {
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [px, py],
                    [pip / 2.0, pip / 2.0],
                    TILE_DAMAGE,
                    atlas::cell_uvs(atlas::SOLID_WHITE),
                ),
            );
            px += pip + 1.5;
        }
    }
    // Slot key, top-left corner.
    push_text_left(
        out,
        &t.slot.to_string(),
        pos[0] - half + 2.0,
        pos[1] - half + 2.0,
        1.0,
        frame,
    );
    // Cooldown remaining number, centred, when cooling.
    if !ready {
        push_text_left(
            out,
            &t.cooldown.to_string(),
            pos[0] - 3.0,
            pos[1] - 3.0,
            1.6,
            TILE_COOLDOWN,
        );
    }
}

/* =============================================================================
 * Enemy telegraph cue + incoming-attack viz (#67).
 *
 * Now that resolver persists each enemy's NEXT action in `enemy.queue`
 * (b9268c4), render a CLEAR per-enemy cue of what it's about to do, so the
 * player can read the threat and plan. The bin categorises the queued action
 * id (via Content → effects) into a [`TelegraphKind`]; hud draws it above the
 * enemy. An ABILITY aimed at the player also gets an incoming-attack line.
 * ============================================================================= */

/// What an enemy's next queued action is, for the telegraph cue. The bin maps
/// the queued action id → effects → this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TelegraphKind {
    /// A weapon/ability that will deal `damage` (an incoming attack).
    Ability { icon: AbilityIcon, damage: i32 },
    /// A self-move along the lane in `dir` (Fore = +x / right).
    Move { dir: LaneEnd },
    /// A reorient (turn).
    Reorient,
}

const TELEGRAPH_FRAME: [f32; 4] = [0.90, 0.34, 0.30, 1.0]; // enemy-intent red
const TELEGRAPH_DIM: [f32; 4] = [0.094, 0.110, 0.149, 0.92];
const TELEGRAPH_INK: [f32; 4] = [0.96, 0.84, 0.40, 1.0]; // amber cue mark

/// Draw the telegraph cue for one enemy directly above it at `enemy_x`. Square
/// red-framed badge: ABILITY → icon + damage pips, MOVE → a direction arrow,
/// REORIENT → a turn glyph. Stateless — straight from the enemy's next action.
pub fn push_telegraph_cue(
    out: &mut Vec<DrawCommand>,
    kind: TelegraphKind,
    enemy_x: f32,
    lane: &LaneGeometry,
) {
    let half = TILE_SIZE / 2.0;
    let pos = [enemy_x, lane.center_y - 96.0];
    // Red frame + dark inner (the "this ship will act" badge).
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half + 1.5, half + 1.5],
            TELEGRAPH_FRAME,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half, half],
            TELEGRAPH_DIM,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    match kind {
        TelegraphKind::Ability { icon, damage } => {
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [pos[0], pos[1] - 2.0],
                    [half * 0.7, half * 0.7],
                    TILE_ICON,
                    atlas::cell_uvs(icon.atlas_cell()),
                ),
            );
            // Damage as "N" centred near the bottom so the threat magnitude reads.
            if damage > 0 {
                push_text_left(
                    out,
                    &damage.to_string(),
                    pos[0] - 3.0,
                    pos[1] + half - 8.0,
                    1.4,
                    TILE_DAMAGE,
                );
            }
        }
        TelegraphKind::Move { dir } => emit_arrow(out, pos, dir, half * 0.6, TELEGRAPH_INK),
        TelegraphKind::Reorient => emit_turn_glyph(out, pos, half * 0.55, TELEGRAPH_INK),
    }
}

/// A line from a telegraphing enemy toward the player — the shot that's aimed at
/// you this turn. Pulses (alpha by `pulse` 0..1) so it reads as "incoming, not
/// yet fired". Drawn along the lane between the two cells.
pub fn push_incoming_attack(
    out: &mut Vec<DrawCommand>,
    enemy_x: f32,
    player_x: f32,
    lane: &LaneGeometry,
    pulse: f32,
) {
    let y = lane.center_y;
    let dx = player_x - enemy_x;
    let len = dx.abs().max(1.0);
    let cx = (enemy_x + player_x) / 2.0;
    let alpha = 0.30 + 0.45 * pulse;
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: [cx, y],
        half_size: [len / 2.0, 1.5],
        color: [0.95, 0.30, 0.28, alpha],
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        rotation_rad: 0.0,
        _pad: [0.0; 3],
    }));
}

/// A small direction arrow (triangle-ish) centred at `pos`, pointing Fore
/// (+x/right) or Aft (−x/left), size `r`.
fn emit_arrow(out: &mut Vec<DrawCommand>, pos: [f32; 2], dir: LaneEnd, r: f32, color: [f32; 4]) {
    let sign = match dir {
        LaneEnd::Fore => 1.0,
        LaneEnd::Aft => -1.0,
    };
    // Shaft + a chunky tip block — readable at tile scale without a custom mesh.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [pos[0], pos[1]],
            [r, 1.5],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [pos[0] + sign * r, pos[1]],
            [r * 0.4, r * 0.5],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
}

/// A turn cue glyph centred at `pos` (two offset arcs-ish blocks suggesting
/// rotation), size `r`.
fn emit_turn_glyph(out: &mut Vec<DrawCommand>, pos: [f32; 2], r: f32, color: [f32; 4]) {
    // Two short bars at right angles — a minimal "rotate" mark.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [pos[0] - r * 0.3, pos[1]],
            [r, 1.5],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [pos[0] + r * 0.5, pos[1] - r * 0.3],
            [1.5, r * 0.6],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
}

/* =============================================================================
 * Enemy telegraph — bruce's refined spec (#67):
 *
 *   1. QUEUED ABILITY ICONS stacked above the enemy, in queue order. The
 *      persistent `enemy.queue` (resolver telegraph, b9268c4) feeds this.
 *   2. A SPINNY "pending" placeholder in the slot where the NEXT ability will
 *      land — a pre-resolution cue that an action is being readied THERE,
 *      which then resolves into the real ability icon.
 *   3. A MOVEMENT ARROW encircling the ship pointing the way it will move,
 *      rather than an icon in the stack.
 *
 * `push_telegraph_cue` above is the single-badge form; these are the richer
 * stacked / encircling forms bruce asked for. The bin categorises each queued
 * action id into a [`TelegraphKind`] and feeds the list here.
 * ============================================================================= */

/// One readied entry in an enemy's telegraph stack. `Pending` is the spinny
/// "winding up" placeholder occupying the slot where the next action will
/// resolve; the others are the resolved cues (ability icon / move arrow / turn).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TelegraphSlot {
    /// The next-to-resolve slot, not yet committed to a concrete cue. Drawn as
    /// the animated spinner so the player sees the enemy "winding up".
    Pending,
    /// A resolved, readied cue (its kind is known from the queued action id).
    Ready(TelegraphKind),
}

/// Draw an enemy's full telegraph above it at `enemy_x`, bottom-of-stack first.
/// ABILITY/REORIENT slots render as stacked red badges; the `Pending` slot
/// renders as the spinny placeholder. A `Move` slot is NOT stacked — it is
/// drawn as a lane-direction arrow ahead of the ship (see
/// [`push_move_arrow_around`]), so the caller should pull moves out and route
/// them there; any `Move` that reaches here still draws its in-badge arrow as a
/// fallback.
///
/// `spin` is a free-running phase (radians) the bin advances each frame; it
/// drives the pending-slot animation.
pub fn push_enemy_telegraph_stack(
    out: &mut Vec<DrawCommand>,
    slots: &[TelegraphSlot],
    enemy_x: f32,
    lane: &LaneGeometry,
    spin: f32,
) {
    let stack_base = lane.center_y - 96.0;
    for (i, slot) in slots.iter().enumerate() {
        let pos = [enemy_x, stack_base - i as f32 * (TILE_SIZE + TILE_GAP)];
        match slot {
            TelegraphSlot::Pending => emit_pending_spinner(out, pos, spin),
            TelegraphSlot::Ready(kind) => emit_telegraph_badge(out, *kind, pos),
        }
    }
}

/// Draw one resolved telegraph badge centred at `pos`: red frame + dark inner,
/// then the kind's cue (ability icon + damage / direction arrow / turn glyph).
/// Factored out of [`push_telegraph_cue`] so the stack and the single-badge
/// form share one look.
fn emit_telegraph_badge(out: &mut Vec<DrawCommand>, kind: TelegraphKind, pos: [f32; 2]) {
    let half = TILE_SIZE / 2.0;
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half + 1.5, half + 1.5],
            TELEGRAPH_FRAME,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half, half],
            TELEGRAPH_DIM,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    match kind {
        TelegraphKind::Ability { icon, damage } => {
            push_sprite(
                out,
                SpriteInstance::axis_aligned(
                    [pos[0], pos[1] - 2.0],
                    [half * 0.7, half * 0.7],
                    TILE_ICON,
                    atlas::cell_uvs(icon.atlas_cell()),
                ),
            );
            if damage > 0 {
                push_text_left(
                    out,
                    &damage.to_string(),
                    pos[0] - 3.0,
                    pos[1] + half - 8.0,
                    1.4,
                    TILE_DAMAGE,
                );
            }
        }
        TelegraphKind::Move { dir } => emit_arrow(out, pos, dir, half * 0.6, TELEGRAPH_INK),
        TelegraphKind::Reorient => emit_turn_glyph(out, pos, half * 0.55, TELEGRAPH_INK),
    }
}

/// The "pending" / winding-up placeholder: a dim badge with a rotating
/// four-spoke mark and an orbiting tick, so the slot reads as "an action is
/// being readied here" before it resolves into a concrete cue. `spin` is the
/// running phase (radians).
fn emit_pending_spinner(out: &mut Vec<DrawCommand>, pos: [f32; 2], spin: f32) {
    let half = TILE_SIZE / 2.0;
    // Dimmer frame than a resolved badge — it's not committed yet.
    let pending_frame = [
        TELEGRAPH_FRAME[0],
        TELEGRAPH_FRAME[1],
        TELEGRAPH_FRAME[2],
        0.6,
    ];
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half + 1.5, half + 1.5],
            pending_frame,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            pos,
            [half, half],
            TELEGRAPH_DIM,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    // Two crossed bars rotating around the badge centre = a spinner. Brightness
    // pulses with the phase so it visibly "churns".
    let arm = half * 0.6;
    let glow = 0.55 + 0.45 * (spin * 1.7).sin().abs();
    let ink = [TELEGRAPH_INK[0], TELEGRAPH_INK[1], TELEGRAPH_INK[2], glow];
    for k in 0..2 {
        let rot = spin + k as f32 * std::f32::consts::FRAC_PI_2;
        push_sprite(
            out,
            SpriteInstance {
                pos,
                half_size: [arm, 1.5],
                color: ink,
                uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
                uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
                rotation_rad: rot,
                _pad: [0.0; 3],
            },
        );
    }
    // An orbiting tick that circles the centre — a clear "still spinning" cue.
    let orbit_r = half * 0.7;
    let tick = [pos[0] + spin.cos() * orbit_r, pos[1] + spin.sin() * orbit_r];
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            tick,
            [2.0, 2.0],
            TELEGRAPH_INK,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
}

/// A LEGIBLE lane-direction move cue (#70 — bruce: "what does the arc signify?").
/// A bold horizontal arrow on the lane just AHEAD of the enemy in the direction
/// it will step (Fore = +x / right): a thick shaft + a big chevron head.
/// Unmistakably "this ship moves THIS way" — green to read as movement (vs the
/// red attack telegraph), brighter as `pulse` rises so it reads as imminent.
/// Replaces the old encircling-hull arc, which didn't read as motion.
pub fn push_move_arrow_around(
    out: &mut Vec<DrawCommand>,
    enemy_x: f32,
    dir: LaneEnd,
    lane: &LaneGeometry,
    pulse: f32,
) {
    let sign = match dir {
        LaneEnd::Fore => 1.0,
        LaneEnd::Aft => -1.0,
    };
    let bright = 0.6 + 0.4 * pulse;
    // Green ink reads as movement, distinct from the red attack telegraph.
    let ink = [0.40, 0.92, 0.55, bright];
    // Sit just above the lane line so it clears the hull + lane ticks, offset
    // toward the move direction so the arrow LEADS the ship.
    let y = lane.center_y - 26.0;
    let gap = 40.0; // start clear of the hull
    let shaft_len = 34.0;
    let head = 11.0;
    let shaft_near_x = enemy_x + sign * gap;
    let shaft_far_x = shaft_near_x + sign * shaft_len;
    let shaft_cx = (shaft_near_x + shaft_far_x) / 2.0;
    // Shaft.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [shaft_cx, y],
            [shaft_len / 2.0, 3.0],
            ink,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );
    // Chevron head, rotated to point along the lane (0 = +x/right, PI = left).
    let head_x = shaft_far_x + sign * head * 0.6;
    let rot = if sign > 0.0 {
        0.0
    } else {
        std::f32::consts::PI
    };
    push_sprite(
        out,
        SpriteInstance {
            pos: [head_x, y],
            half_size: [head, head],
            color: ink,
            uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
            uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
            rotation_rad: rot,
            _pad: [0.0; 3],
        },
    );
}

/// Linear interpolation, `t` in 0..1.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Left-aligned single-line text using the inline 5×7 font, starting at
/// `(x, y)` (top-left). Unsupported glyphs render blank (advance preserved).
fn push_text_left(
    out: &mut Vec<DrawCommand>,
    text: &str,
    x: f32,
    y: f32,
    pixel: f32,
    color: [f32; 4],
) {
    let advance = 5.0 * pixel + pixel;
    for (i, ch) in text.chars().enumerate() {
        push_glyph_5x7(out, ch, x + i as f32 * advance, y, pixel, color);
    }
}

/// Centered single-line banner using the inline 5×7 font. `pixel` is
/// the size of one font "pixel" in virtual pixels (typically 4 for
/// title-style banners, 2 for body text). `y` is the vertical center
/// of the rendered glyph row.
fn push_centered_banner(out: &mut Vec<DrawCommand>, banner: &str, y_center: f32, pixel: f32) {
    let glyph_w_px = 5.0 * pixel;
    let glyph_h_px = 7.0 * pixel;
    let space_px = pixel;
    let advance = glyph_w_px + space_px;
    let total_w: f32 = banner.len() as f32 * advance - space_px;
    // (#76 scene-res) Centre on the LIVE canvas width.
    let start_x = (crate::gfx::scene_w() as f32 - total_w) / 2.0;
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
pub fn push_between_encounter_overlay(out: &mut Vec<DrawCommand>, choice: BetweenEncounterChoice) {
    // (#76 scene-res) Full-canvas overlay over the LIVE scene.
    let center_x = crate::gfx::scene_w() as f32 / 2.0;
    let center_y = crate::gfx::scene_h() as f32 / 2.0;
    let tint = match choice {
        BetweenEncounterChoice::EncounterComplete { .. } => [0.10, 0.20, 0.35, 0.65],
        BetweenEncounterChoice::RunComplete { .. } => VICTORY_TINT,
    };
    // Full-canvas tinted overlay.
    push_sprite(
        out,
        SpriteInstance::axis_aligned(
            [center_x, center_y],
            [center_x, center_y],
            tint,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ),
    );

    match choice {
        BetweenEncounterChoice::EncounterComplete {
            sector_idx,
            salvage,
        } => {
            // Banner row: "ENCOUNTER COMPLETE - SECTOR N" at y_center - 60.
            let pixel = 3.0;
            let sector_num = sector_idx + 1;
            let banner = format!("ENCOUNTER COMPLETE - SECTOR {}", sector_num);
            push_centered_banner(out, &banner, center_y - 60.0, pixel);
            // Salvage row: "SALVAGE: N" between banner and choices.
            push_centered_banner(
                out,
                &format!("SALVAGE: {}", salvage),
                center_y - 15.0,
                pixel,
            );
            // Choice row: "1 REPAIR    2 UPGRADE    3 CONTINUE" at y_center + 35.
            push_centered_banner(
                out,
                "1 REPAIR  2 UPGRADE  3 CONTINUE",
                center_y + 35.0,
                pixel,
            );
        }
        BetweenEncounterChoice::RunComplete { salvage } => {
            push_centered_banner(out, "RUN COMPLETE", center_y - 50.0, 5.0);
            push_centered_banner(
                out,
                &format!("TOTAL SALVAGE: {}", salvage),
                center_y + 15.0,
                3.0,
            );
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
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        ' ' => return,
        _ => return, // unknown char = blank glyph
    };
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..5 {
            if (bits >> (4 - col)) & 1 == 1 {
                let px = x + col as f32 * pixel;
                let py = y + row as f32 * pixel;
                push_sprite(
                    out,
                    SpriteInstance::axis_aligned(
                        [px + pixel / 2.0, py + pixel / 2.0],
                        [pixel / 2.0, pixel / 2.0],
                        color,
                        atlas::cell_uvs(atlas::SOLID_WHITE),
                    ),
                );
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
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        }
    }

    fn frigate_at(cell: usize, faction: Faction, orientation: Orientation) -> Ship {
        Ship {
            id: format!("ship-{}", cell),
            faction,
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation,
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
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
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        let scene = compose_scene_with(
            &board,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
            &EmptySpriteRegistry,
        );
        let textured_count = scene
            .iter()
            .filter(|c| matches!(c, DrawCommand::TexturedShip(_)))
            .count();
        assert_eq!(
            textured_count, 0,
            "empty registry should not emit textured-ship draws"
        );
    }

    #[test]
    fn loaded_registry_emits_textured_ship_per_ship() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        let scene = compose_scene_with(
            &board,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
            &AlwaysLoaded,
        );
        let textured: Vec<_> = scene
            .iter()
            .filter_map(|c| {
                if let DrawCommand::TexturedShip(t) = c {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            textured.len(),
            2,
            "expected one textured-ship draw per ship"
        );
        // sin(45deg) ≈ 0.7071
        for t in &textured {
            assert!((t.blend_t - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        }
        // Each ship's slug pair encodes its stance.
        assert_eq!(textured[0].side.as_str(), "frigate_bowOnFore_side");
        assert_eq!(textured[0].top.as_str(), "frigate_bowOnFore_top");
        assert_eq!(textured[1].side.as_str(), "frigate_broadside_side");
        assert_eq!(textured[1].top.as_str(), "frigate_broadside_top");
    }

    /// Stub registry that lofts every ship: the player as the grey dagger,
    /// enemies as the CAD hull. Mirrors the live `Gfx::loft_kind` dispatch.
    struct LoftAll;
    impl SpriteRegistry for LoftAll {
        fn has(&self, _class: &str, _stance: SpriteStance, _view: SpriteView) -> bool {
            false
        }
        fn loft_kind(
            &self,
            _ship_id: &str,
            is_player: bool,
        ) -> Option<crate::sprites::LoftMeshKind> {
            Some(if is_player {
                crate::sprites::LoftMeshKind::PlayerCad
            } else {
                crate::sprites::LoftMeshKind::EnemyCad
            })
        }
    }

    #[test]
    fn loft_registry_emits_loftship_per_ship_with_kind_and_id() {
        use crate::sprites::LoftMeshKind;
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        let scene =
            compose_scene_with(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4, &LoftAll);
        let lofts: Vec<_> = scene
            .iter()
            .filter_map(|c| match c {
                DrawCommand::LoftShip(l) => Some(*l),
                _ => None,
            })
            .collect();
        assert_eq!(lofts.len(), 2, "one LoftShip per ship");
        // No 2D silhouette / textured draws when every ship lofts.
        assert!(scene
            .iter()
            .all(|c| !matches!(c, DrawCommand::TexturedShip(_))));
        // Player (cell 0, id "ship-0") → grey dagger; enemy (cell 2, "ship-2")
        // → CAD hull. The ship id is carried so the renderer keys its pose.
        let player = lofts
            .iter()
            .find(|l| l.ship_id.as_str() == "ship-0")
            .unwrap();
        assert_eq!(player.kind, LoftMeshKind::PlayerCad);
        let enemy = lofts
            .iter()
            .find(|l| l.ship_id.as_str() == "ship-2")
            .unwrap();
        assert_eq!(enemy.kind, LoftMeshKind::EnemyCad);
    }

    /// (#74) The player loft hero-quad's aspect MUST equal the loft texture's
    /// aspect ([`LOFT_TEXTURE_ASPECT`]). The loft blit STRETCHES the offscreen
    /// texture to fill this quad, so any other quad aspect squashes the hull —
    /// the round engine-glow discs rendered as ovals (the 2:1 quad stretched the
    /// 1.6:1 texture 1.25× horizontally). Asserting quad-aspect == texture-aspect
    /// proves the blit applies ZERO stretch ⇒ a round texture-disc stays round,
    /// deterministically (no eyeball / no pixel archaeology). Checked at a centre
    /// and an edge column so the (column-dependent) `near_edge_width` can't
    /// reintroduce a stretch.
    #[test]
    fn player_loft_quad_matches_texture_aspect_no_squash() {
        use crate::grid::{Dir4, Facing, Pos, COLS, ROWS};
        use crate::projector::ProjectorConfig;
        let cfg = ProjectorConfig::default();
        for col in [COLS / 2, COLS - 1] {
            let mut board = empty_board(crate::grid::CELLS);
            // Player on the front row at `col`, facing up-lane, with a class so
            // the loft path engages (mirrors the live player).
            let mut player =
                frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
            player.pos = Pos::new(col, ROWS - 1);
            player.facing = Facing::Bow(Dir4::N);
            player.klass = Some("aegis".to_string());
            let idx = player.pos.to_index();
            board.cells[idx] = Some(player);

            let scene = compose_scene_2d_with(&board, &cfg, &LoftAll);
            let loft = scene
                .iter()
                .find_map(|c| match c {
                    DrawCommand::LoftShip(l) => Some(*l),
                    _ => None,
                })
                .expect("player emits a LoftShip");
            // Quad corners: p0 top-left, p1 top-right, p2 bot-right, p3 bot-left.
            let w = loft.p1[0] - loft.p0[0];
            let h = loft.p3[1] - loft.p0[1];
            assert!(w > 0.0 && h > 0.0, "col {col}: degenerate quad {w}x{h}");
            let aspect = w / h;
            assert!(
                (aspect - LOFT_TEXTURE_ASPECT).abs() < 1e-3,
                "col {col}: hero-quad aspect {aspect:.4} must equal the loft texture aspect \
                 {LOFT_TEXTURE_ASPECT:.4} (else the blit squashes the hull / ovals the discs)"
            );
        }
    }

    /// (#79) `lerp_facing_yaw_deg` takes the SHORTEST arc: a Q/E quarter-turn is
    /// always ±90 even across the ±180 wrap, and endpoints are exact at t=0/1.
    #[test]
    fn facing_yaw_lerp_is_shortest_path() {
        use crate::grid::{Axis, Dir4, Facing};
        let n = Facing::Bow(Dir4::N); // 0
        let e = Facing::Bow(Dir4::E); // +90
        let w = Facing::Bow(Dir4::W); // -90
        let s = Facing::Bow(Dir4::S); // 180
        // Endpoints exact.
        assert!((lerp_facing_yaw_deg(n, e, 0.0) - 0.0).abs() < 1e-4);
        assert!((lerp_facing_yaw_deg(n, e, 1.0) - 90.0).abs() < 1e-4);
        // N->E half = +45.
        assert!((lerp_facing_yaw_deg(n, e, 0.5) - 45.0).abs() < 1e-4);
        // S(180)->W(-90): naive delta -270, shortest is +90 -> half lands 225
        // (180 + 45), NOT 45. Proves the wrap.
        assert!((lerp_facing_yaw_deg(s, w, 0.5) - 225.0).abs() < 1e-4, "S->W must wrap +90");
        // W(-90)->N(0): +90, half = -45.
        assert!((lerp_facing_yaw_deg(w, n, 0.5) - (-45.0)).abs() < 1e-4);
        // A no-op (same facing) is flat.
        let _ = Axis::EastWest;
        assert!((lerp_facing_yaw_deg(n, n, 0.5) - 0.0).abs() < 1e-4);
    }

    /// (#79) `lerp_cell_quad` returns `a` at t=0, `b` at t=1, and the midpoint
    /// centre is the mean of the two cell centres (slides along the grid).
    #[test]
    fn cell_quad_lerp_endpoints_and_midpoint() {
        use crate::grid::Pos;
        use crate::projector::{grid_cell_quad, ProjectorConfig};
        let cfg = ProjectorConfig::default();
        let a = grid_cell_quad(Pos::new(1, 3), &cfg);
        let b = grid_cell_quad(Pos::new(3, 3), &cfg);
        let at0 = lerp_cell_quad(&a, &b, 0.0);
        let at1 = lerp_cell_quad(&a, &b, 1.0);
        assert!((at0.center[0] - a.center[0]).abs() < 1e-3 && (at0.center[1] - a.center[1]).abs() < 1e-3);
        assert!((at1.center[0] - b.center[0]).abs() < 1e-3 && (at1.center[1] - b.center[1]).abs() < 1e-3);
        let mid = lerp_cell_quad(&a, &b, 0.5);
        assert!((mid.center[0] - 0.5 * (a.center[0] + b.center[0])).abs() < 1e-3);
        assert!((mid.center[1] - 0.5 * (a.center[1] + b.center[1])).abs() < 1e-3);
    }

    /// (#79) An empty [`Tween2d`] makes `compose_scene_2d_tweened` byte-identical
    /// to `compose_scene_2d_with` (the tween is a strict opt-in superset).
    #[test]
    fn empty_tween2d_matches_untweened_2d() {
        use crate::projector::ProjectorConfig;
        let cfg = ProjectorConfig::default();
        let mut board = empty_board(crate::grid::CELLS);
        board.cells[crate::grid::Pos::new(2, 3).to_index()] = Some({
            let mut s = frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
            s.pos = crate::grid::Pos::new(2, 3);
            s.facing = crate::grid::Facing::Bow(crate::grid::Dir4::N);
            s
        });
        let plain = compose_scene_2d_with(&board, &cfg, &EmptySpriteRegistry);
        let tweened = compose_scene_2d_tweened(&board, &cfg, &EmptySpriteRegistry, &Tween2d::default());
        assert_eq!(plain.len(), tweened.len(), "empty Tween2d must not change the draw list");
    }

    #[test]
    fn tween_state_default_is_identity_with_compose_scene_with() {
        // A default TweenState (empty visual_cells map) should produce
        // the same scene as compose_scene_with — the tweened path is a
        // strict superset.
        let mut board = empty_board(7);
        board.cells[2] = Some(frigate_at(
            2,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        let untweened = compose_scene_with(
            &board,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
            &EmptySpriteRegistry,
        );
        let tweened = compose_scene_tweened(
            &board,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
            &EmptySpriteRegistry,
            &TweenState::default(),
        );
        assert_eq!(
            untweened.len(),
            tweened.len(),
            "default TweenState must produce identical draw count"
        );
    }

    #[test]
    fn tween_state_shifts_ship_polygon_left_when_visual_cell_is_lower() {
        // Same board, two compose calls: one with no tween (ship at
        // logical cell 4), one with the tween anchoring the ship at
        // cell 2.0 (mid-flight from cell 2 → cell 4). The second pass
        // should emit ship polygons whose x coords are shifted LEFT
        // because visual_cell < logical_cell on a left-to-right lane.
        let mut board = empty_board(7);
        board.cells[4] = Some(frigate_at(
            4,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        let logical_scene = compose_scene_with(&board, &DEFAULT_LANE, 0.0, &EmptySpriteRegistry);

        let mut tween = TweenState::default();
        tween.visual_cells.insert("ship-4".to_string(), 2.0);
        let tweened =
            compose_scene_tweened(&board, &DEFAULT_LANE, 0.0, &EmptySpriteRegistry, &tween);

        // Find the first ship polygon in each (the stern body
        // rectangle is the first Polygon emitted after parallax /
        // lane / range bands).
        let logical_x = first_ship_polygon_left_x(&logical_scene)
            .expect("logical scene must have a ship polygon");
        let tweened_x =
            first_ship_polygon_left_x(&tweened).expect("tweened scene must have a ship polygon");
        assert!(
            tweened_x < logical_x,
            "tweened ship (visual_cell=2) should be drawn LEFT of logical ship (cell=4); \
             got logical_x={} tweened_x={}",
            logical_x,
            tweened_x
        );
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
        let ship = frigate_at(
            3,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        );
        let empty = TweenState::default();
        assert_eq!(
            empty.cell_for(&ship),
            3.0,
            "empty TweenState should fall back to ship.cell"
        );

        let mut populated = TweenState::default();
        populated.visual_cells.insert(ship.id.clone(), 1.5);
        assert_eq!(
            populated.cell_for(&ship),
            1.5,
            "TweenState entry should override the logical cell"
        );
    }

    #[test]
    fn win_state_classifies_factions_correctly() {
        // Pure backdrop / lane / no ships → board is technically both
        // "no player" and "no enemy"; we resolve to Defeat (player isn't
        // present so they can't have won).
        assert_eq!(win_state(&empty_board(7)), WinState::Defeat);

        let mut b = empty_board(7);
        b.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        assert_eq!(win_state(&b), WinState::Victory, "player alone = victory");

        let mut b = empty_board(7);
        b.cells[3] = Some(frigate_at(3, Faction::Enemy, Orientation::Broadside));
        assert_eq!(win_state(&b), WinState::Defeat, "enemy alone = defeat");

        let mut b = empty_board(7);
        b.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
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
        assert!(
            out.len() > 50,
            "defeat overlay should emit tint + banner glyphs, got {}",
            out.len()
        );

        let mut v_out: Vec<DrawCommand> = Vec::new();
        push_end_state_overlay(&mut v_out, WinState::Victory);
        assert!(
            v_out.len() > 50,
            "victory overlay should emit tint + banner glyphs, got {}",
            v_out.len()
        );
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
        let baseline = compose_scene_with(
            &board,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
            &EmptySpriteRegistry,
        );
        let has_overlay_quad = baseline.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.half_size[0] >= crate::gfx::VIRTUAL_W as f32 / 2.0
                && s.half_size[1] >= crate::gfx::VIRTUAL_H as f32 / 2.0)
        });
        assert!(
            !has_overlay_quad,
            "compose_scene must NOT auto-push the end-state overlay anymore; \
             the bin owns that decision since #77"
        );
    }

    #[test]
    fn push_between_encounter_overlay_emits_tint_plus_text() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_between_encounter_overlay(
            &mut out,
            BetweenEncounterChoice::EncounterComplete {
                sector_idx: 0,
                salvage: 7,
            },
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
        assert!(
            out.len() > 20,
            "salvage HUD should emit a row of font glyph quads, got {}",
            out.len()
        );
        // No full-canvas overlay quad — this is an in-game indicator,
        // not a modal screen.
        let has_overlay_quad = out.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.half_size[0] >= crate::gfx::VIRTUAL_W as f32 / 2.0
                && s.half_size[1] >= crate::gfx::VIRTUAL_H as f32 / 2.0)
        });
        assert!(
            !has_overlay_quad,
            "salvage HUD must NOT emit a full-canvas tint quad"
        );
    }

    #[test]
    fn push_salvage_hud_scales_with_value() {
        // Multi-digit salvage values should emit MORE glyph quads than
        // single-digit values — verifies the counter actually
        // renders the number (not just the "SALVAGE:" prefix).
        let mut small: Vec<DrawCommand> = Vec::new();
        let mut large: Vec<DrawCommand> = Vec::new();
        push_salvage_hud(&mut small, 7); // 1 digit
        push_salvage_hud(&mut large, 12345); // 5 digits
        assert!(
            large.len() > small.len(),
            "5-digit salvage HUD ({}) should emit more glyphs than 1-digit ({})",
            large.len(),
            small.len()
        );
    }

    #[test]
    fn push_run_defeated_overlay_emits_total_salvage_line() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_run_defeated_overlay(&mut out, 42);
        assert!(
            out.len() > 50,
            "run-defeated overlay should emit tint + banner + salvage + restart glyphs, got {}",
            out.len()
        );
    }

    #[test]
    fn run_defeated_overlay_with_cause_adds_a_line() {
        // The cause variant should emit MORE draws than the bare overlay
        // (the extra "DESTROYED BY …" banner), and None should match the bare.
        let mut bare: Vec<DrawCommand> = Vec::new();
        push_run_defeated_overlay_with_cause(&mut bare, 5, None);
        let mut with_cause: Vec<DrawCommand> = Vec::new();
        push_run_defeated_overlay_with_cause(&mut with_cause, 5, Some("DESTROYED BY GUNBOAT"));
        assert!(
            with_cause.len() > bare.len(),
            "cause line should add glyph draws: bare={} with={}",
            bare.len(),
            with_cause.len()
        );
    }

    #[test]
    fn telegraph_stack_emits_pending_spinner_and_badges() {
        // A stack of [Pending, Ability, Reorient] should produce draws for
        // each slot. We don't pin exact counts — just that it's non-trivial
        // and scales with slot count vs a single pending slot.
        let mut one: Vec<DrawCommand> = Vec::new();
        push_enemy_telegraph_stack(
            &mut one,
            &[TelegraphSlot::Pending],
            100.0,
            &DEFAULT_LANE,
            0.3,
        );
        let mut many: Vec<DrawCommand> = Vec::new();
        push_enemy_telegraph_stack(
            &mut many,
            &[
                TelegraphSlot::Pending,
                TelegraphSlot::Ready(TelegraphKind::Ability {
                    icon: AbilityIcon::Beam,
                    damage: 3,
                }),
                TelegraphSlot::Ready(TelegraphKind::Reorient),
            ],
            100.0,
            &DEFAULT_LANE,
            0.3,
        );
        assert!(!one.is_empty(), "a pending slot must draw the spinner");
        assert!(
            many.len() > one.len(),
            "more slots should draw more: one={} many={}",
            one.len(),
            many.len()
        );
    }

    #[test]
    fn move_arrow_around_emits_for_both_directions() {
        let mut fore: Vec<DrawCommand> = Vec::new();
        push_move_arrow_around(&mut fore, 100.0, LaneEnd::Fore, &DEFAULT_LANE, 0.5);
        let mut aft: Vec<DrawCommand> = Vec::new();
        push_move_arrow_around(&mut aft, 100.0, LaneEnd::Aft, &DEFAULT_LANE, 0.5);
        assert!(!fore.is_empty(), "fore move arrow must draw arc + head");
        assert!(!aft.is_empty(), "aft move arrow must draw arc + head");
        // The leading arrowhead sits on opposite sides of the ship x for the
        // two directions, so the rightmost sprite x differs.
        let max_x = |v: &[DrawCommand]| {
            v.iter()
                .filter_map(|c| match c {
                    DrawCommand::Sprite(s) => Some(s.pos[0]),
                    _ => None,
                })
                .fold(f32::MIN, f32::max)
        };
        assert!(
            max_x(&fore) > max_x(&aft),
            "fore arrowhead should reach further right than aft"
        );
    }

    #[test]
    fn player_hull_bar_fill_color_tracks_health() {
        // The fill quad's colour should switch from teal (healthy) to red
        // (critical). Pull the widest fill-coloured sprite (the bar fill) and
        // check its tint flips. The track BG is a distinct dark colour.
        let fill_color = |hull: i32| -> Option<[f32; 4]> {
            let mut out: Vec<DrawCommand> = Vec::new();
            push_player_hull_bar(&mut out, hull, 6);
            // The fill quad is the one matching one of the three health tints.
            out.iter().find_map(|c| match c {
                DrawCommand::Sprite(s)
                    if s.color == PLAYER_HULL_OK
                        || s.color == PLAYER_HULL_HURT
                        || s.color == PLAYER_HULL_CRIT =>
                {
                    Some(s.color)
                }
                _ => None,
            })
        };
        assert_eq!(fill_color(6), Some(PLAYER_HULL_OK), "full hull = teal");
        assert_eq!(fill_color(2), Some(PLAYER_HULL_CRIT), "1/3 hull = red");
        assert_eq!(fill_color(0), None, "zero hull draws no fill quad");
    }

    #[test]
    fn hit_flash_is_noop_at_zero() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_player_hit_flash(&mut out, 0.0);
        assert!(out.is_empty(), "zero-intensity flash must draw nothing");
        push_player_hit_flash(&mut out, 1.0);
        assert_eq!(out.len(), 1, "full flash draws one full-canvas quad");
    }

    #[test]
    fn push_between_encounter_overlay_run_complete_variant_renders() {
        let mut out: Vec<DrawCommand> = Vec::new();
        push_between_encounter_overlay(
            &mut out,
            BetweenEncounterChoice::RunComplete { salvage: 17 },
        );
        assert!(
            out.len() > 50,
            "run-complete overlay should emit tint + banner glyphs, got {}",
            out.len()
        );
    }

    #[test]
    fn empty_board_still_produces_backdrop_and_lane() {
        let board = empty_board(7);
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(
            scene.len() > 20,
            "expected backdrop + lane, got {}",
            scene.len()
        );
    }

    #[test]
    fn one_player_ship_produces_visible_sprites() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(
            scene.len() > 30,
            "expected backdrop + ship sprites, got {}",
            scene.len()
        );
    }

    #[test]
    fn ship_with_shield_charges_draws_pips() {
        let mut ship = frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        );
        ship.shield_profile = ShieldProfile {
            bow: ShieldFace {
                armour: 2,
                charge: 2,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 1,
                charge: 1,
            },
            starboard: ShieldFace {
                armour: 1,
                charge: 0,
            },
        };
        // Test the emitter directly (not via the compose delta): the overlay
        // HUD is re-anchored to the loft footprint and ON
        // (SHOW_PLACEHOLDER_HUD = true), so `push_shield_pips` is the precise
        // unit under test regardless of compose-level gating.
        let mut with = Vec::new();
        push_shield_pips(
            &mut with,
            &ship,
            0.0,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
        );
        // 2 bow pips + 1 port pip = 3 sprites.
        assert_eq!(with.len(), 3);
    }

    #[test]
    fn ship_with_heat_draws_a_filled_bar() {
        let mut ship = frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        );
        ship.heat = 3;
        // Emitter-direct (compose gates the overlay HUD off for the showcase).
        let mut with = Vec::new();
        push_heat_bar(
            &mut with,
            &ship,
            0.0,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
        );
        let mut bare = Vec::new();
        let bare_ship = frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        );
        push_heat_bar(
            &mut bare,
            &bare_ship,
            0.0,
            &DEFAULT_LANE,
            std::f32::consts::FRAC_PI_4,
        );
        // Heated ship draws the bar BG + a fill quad; cold ship draws only BG.
        assert_eq!(with.len() - bare.len(), 1);
    }

    #[test]
    fn projectiles_render_after_ships_in_z_order() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.ordnance.push(Projectile {
            id: "t1".into(),
            kind: "torpedo".into(),
            cell: 3,
            pos: crate::grid::Pos::new(0, 0),
            heading: LaneEnd::Fore,
            heading8: crate::grid::Dir8::N,
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
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        board.cells[3] = Some(frigate_at(
            3,
            Faction::Enemy,
            Orientation::BowOn { bow: LaneEnd::Aft },
        ));
        board.cells[5] = Some(frigate_at(
            5,
            Faction::Enemy,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.cells[6] = Some(frigate_at(
            6,
            Faction::Enemy,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.ordnance.push(Projectile {
            id: "ord".into(),
            kind: "torpedo".into(),
            cell: 4,
            pos: crate::grid::Pos::new(0, 0),
            heading: LaneEnd::Fore,
            heading8: crate::grid::Dir8::N,
            speed: 1,
            hull: 1,
            payload: Vec::new(),
            owner_faction: Faction::Player,
        });
        let scene = compose_scene(&board, &DEFAULT_LANE, std::f32::consts::FRAC_PI_4);
        assert!(
            scene.len() > 60,
            "expected a populated scene, got {}",
            scene.len()
        );
    }

    #[test]
    fn every_view_angle_produces_finite_vertices() {
        // Crash-guard: walk every fixed scrub angle (0, 15, 30, 45, 60, 75,
        // 90 deg) and assert no NaN/inf reaches the GPU. wgpu rejects
        // non-finite vertex positions on some drivers; this catches a
        // regression in the ship-rotation math before bruce sees it.
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(
            0,
            Faction::Player,
            Orientation::BowOn { bow: LaneEnd::Fore },
        ));
        board.cells[2] = Some(frigate_at(2, Faction::Enemy, Orientation::Broadside));
        board.cells[3] = Some(frigate_at(
            3,
            Faction::Enemy,
            Orientation::BowOn { bow: LaneEnd::Aft },
        ));
        for d in [0.0_f32, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0] {
            let scene = compose_scene(&board, &DEFAULT_LANE, d.to_radians());
            for (i, cmd) in scene.iter().enumerate() {
                match cmd {
                    DrawCommand::Sprite(s) => {
                        for v in [s.pos, s.half_size, s.uv_min, s.uv_max] {
                            for c in v {
                                assert!(
                                    c.is_finite(),
                                    "non-finite sprite coord at angle {}° idx {}: {:?}",
                                    d,
                                    i,
                                    s
                                );
                            }
                        }
                    }
                    DrawCommand::Polygon(p) => {
                        for v in [p.p0, p.p1, p.p2, p.p3, p.uv_min, p.uv_max] {
                            for c in v {
                                assert!(
                                    c.is_finite(),
                                    "non-finite polygon coord at angle {}° idx {}: {:?}",
                                    d,
                                    i,
                                    p
                                );
                            }
                        }
                    }
                    DrawCommand::TexturedShip(t) => {
                        for v in [t.p0, t.p1, t.p2, t.p3] {
                            for c in v {
                                assert!(
                                    c.is_finite(),
                                    "non-finite textured-ship coord at angle {}° idx {}: {:?}",
                                    d,
                                    i,
                                    t
                                );
                            }
                        }
                        assert!(
                            t.blend_t.is_finite(),
                            "non-finite blend_t at angle {}° idx {}: {:?}",
                            d,
                            i,
                            t
                        );
                    }
                    DrawCommand::LoftShip(l) => {
                        for v in [l.p0, l.p1, l.p2, l.p3] {
                            for c in v {
                                assert!(
                                    c.is_finite(),
                                    "non-finite loft-ship coord at angle {}° idx {}: {:?}",
                                    d,
                                    i,
                                    l
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /* ---- v2 2-D compositor (D3) smoke tests ------------------------------ */

    use crate::grid::{Axis, Dir4, Facing, Pos};
    use crate::projector::ProjectorConfig;

    /// `compose_scene_2d` draws the grid wireframe even with no ships, and adds
    /// strictly more commands once ships are present — the basic "it composes"
    /// guard for the perspective path.
    #[test]
    fn compose_scene_2d_draws_grid_then_ships() {
        let cfg = ProjectorConfig::default();

        let empty = empty_board(crate::grid::CELLS);
        let grid_only = compose_scene_2d(&empty, &cfg);
        // 5×4 cells × 4 edge lines = 80 line sprites minimum.
        assert!(
            grid_only.len() >= 80,
            "grid wireframe should emit ≥80 commands, got {}",
            grid_only.len()
        );

        let mut board = empty_board(crate::grid::CELLS);
        let mut player = frigate_at(0, Faction::Player, Orientation::Broadside);
        player.pos = Pos::new(2, ROWS_LOCAL - 1);
        player.facing = Facing::Bow(Dir4::N);
        let idx = player.pos.to_index();
        board.cells[idx] = Some(player);
        let with_ship = compose_scene_2d(&board, &cfg);
        assert!(
            with_ship.len() > grid_only.len(),
            "a ship should add commands ({} vs {})",
            with_ship.len(),
            grid_only.len()
        );
    }

    /// The bow arrow encodes `Facing::forward_axis()`: a ship facing N puts its
    /// chevron ABOVE the hull center (smaller y, screen-down), facing S puts it
    /// BELOW — the orientation-obvious contract shared with `facing_zone`.
    #[test]
    fn bow_arrow_points_along_forward_axis() {
        let cfg = ProjectorConfig::default();
        let center = grid_cell_quad(Pos::new(2, ROWS_LOCAL - 1), &cfg).center;

        // Extract the BOW_CHEVRON sprite y for a given facing.
        let chevron_y = |facing: Facing| -> f32 {
            let mut s = frigate_at(0, Faction::Enemy, Orientation::Broadside);
            s.pos = Pos::new(2, ROWS_LOCAL - 1);
            s.facing = facing;
            let mut out = Vec::new();
            push_ship_2d(&mut out, &s, &cfg, &EmptySpriteRegistry, None);
            let (mn, mx) = atlas::cell_uvs(atlas::BOW_CHEVRON);
            out.iter()
                .find_map(|c| match c {
                    DrawCommand::Sprite(sp) if sp.uv_min == mn && sp.uv_max == mx => Some(sp.pos[1]),
                    _ => None,
                })
                .expect("a bow chevron sprite should be emitted")
        };

        // N (toward row 0 / up the board) ⇒ chevron above center (smaller y).
        assert!(chevron_y(Facing::Bow(Dir4::N)) < center[1]);
        // S (toward the player / down) ⇒ chevron below center (larger y).
        assert!(chevron_y(Facing::Bow(Dir4::S)) > center[1]);
        // Broadside(EastWest) points along +E ⇒ chevron to the right, same y as
        // center (no vertical offset).
        let mut s = frigate_at(0, Faction::Enemy, Orientation::Broadside);
        s.pos = Pos::new(2, ROWS_LOCAL - 1);
        s.facing = Facing::Broadside(Axis::EastWest);
        let mut out = Vec::new();
        push_ship_2d(&mut out, &s, &cfg, &EmptySpriteRegistry, None);
        let (mn, mx) = atlas::cell_uvs(atlas::BOW_CHEVRON);
        let chev = out
            .iter()
            .find_map(|c| match c {
                DrawCommand::Sprite(sp) if sp.uv_min == mn && sp.uv_max == mx => Some(sp.pos),
                _ => None,
            })
            .expect("chevron present");
        assert!(chev[0] > center[0], "EastWest broadside arrow points +E (right)");
    }

    /// Shield pips: a ship with bow+port charges emits exactly that many pip
    /// sprites in the 2-D path (one per held charge, by zone).
    #[test]
    fn shield_pips_2d_one_per_charge() {
        let cfg = ProjectorConfig::default();
        let mut ship = frigate_at(0, Faction::Player, Orientation::Broadside);
        ship.pos = Pos::new(1, 1);
        ship.facing = Facing::Bow(Dir4::N);
        ship.shield_profile = ShieldProfile {
            bow: ShieldFace { armour: 2, charge: 2 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 1 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        };
        let center = grid_cell_quad(ship.pos, &cfg).center;
        let base = 22.0 * grid_cell_quad(ship.pos, &cfg).depth_scale;
        let fwd = bow_screen_dir(ship.facing);
        let mut out = Vec::new();
        push_shield_pips_2d(&mut out, &ship, center, base, fwd);
        // 2 bow + 1 port = 3 pip sprites (SOLID_WHITE in the pip color).
        assert_eq!(out.len(), 3, "expected 3 pips (2 bow + 1 port)");
    }

    /// `ROWS` aliased locally so the tests read clearly without colliding with
    /// the module-wide `use` set.
    const ROWS_LOCAL: usize = crate::grid::ROWS;
}
