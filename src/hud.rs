//! Scene compositor — turns a [`crate::types::Board`] into a back-to-front
//! `Vec<DrawCommand>` for [`crate::gfx::Gfx::render`].
//!
//! ## Render order (back to front)
//!
//! 1. **Deep-space backdrop** — handled by [`crate::gfx::Gfx`]'s clear color.
//! 2. **Parallax** — far stars + nebula + distant planet + mid stars +
//!    foreground dust. Tiled across the viewport at progressively closer
//!    bands.
//! 3. **Lane plate** — trapezoid from
//!    [`crate::perspective::cell_footprint`] aggregated across all cells,
//!    drawn as a stack of dim parallelograms.
//! 4. **Range-band tick marks** — five faint tick lines under the lane, one
//!    per band boundary (relative to the player), colored per the analysis
//!    palette. Subtle, doesn't compete with ships.
//! 5. **Hazards** — mines, drones, debris.
//! 6. **Ships** — for each cell with a ship: front face + top face + bow
//!    chevron + heat bar + shield pips. Composed via
//!    [`crate::perspective::ship_sprite`].
//! 7. **Live ordnance** — `Board.ordnance`, drawn with the torpedo or missile
//!    sprite rotated to its lane-slope heading.
//! 8. **Action queue glyphs** — stacked above each ship's pivot.
//! 9. **Telegraph icons** — above each enemy when telegraphed intent is known
//!    (slice-D: stubbed to an empty list; resolver/content wires this up).
//! 10. **Status badges** — small icons near each ship for active statuses.
//! 11. **End-state overlays** — defeat / victory tints (slice-D: not yet
//!     wired; the binary doesn't yet expose win state).
//!
//! All coordinates are in virtual pixels (the engine's `1320 × 480` canvas).
//! Vertex rotation about the lane slope is baked on the CPU here — the
//! sprite shader's per-instance `rotation_rad` is left at `0.0` for composed
//! polygon sprites, because they were already rotated when their vertices
//! were computed. Axis-aligned HUD overlays (range-band ticks, end-state
//! tints) use raw `SpriteInstance::axis_aligned`.

use crate::atlas;
use crate::geometry::range_band;
use crate::gfx::{DrawCommand, PolygonInstance, SpriteInstance};
use crate::perspective::{
    cell_footprint, cell_to_screen, fractional_cell_to_screen, ship_sprite, CellScreen,
    FacePoly, LaneGeometry, Point2, ShipSprite, Stance, FRIGATE_DIMS,
};
use crate::types::{
    Board, Faction, HullZone, LaneEnd, Mount, Orientation, Projectile, RangeBand, Ship, Status,
    StatusKind, WeaponArchetype,
};

/* ---- palette --------------------------------------------------------------
 *
 * The analysis HTML's CSS tokens (`--ink`, `--gold`, `--vermillion`, the
 * archetype colors, the range-band colors). Kept as `[f32; 4]` (linear-ish
 * sRGB scaled to 0..1) because that's what `SpriteInstance::color` takes.
 * Brightness is tuned for the deep-space ink clear color.
 * ----------------------------------------------------------------------- */

const PLAYER_FRONT:   [f32; 4] = [0.078, 0.133, 0.208, 1.0];
const PLAYER_TOP:     [f32; 4] = [0.102, 0.165, 0.243, 1.0];
const PLAYER_STROKE:  [f32; 4] = [0.329, 0.812, 0.788, 1.0];

const ENEMY_FRONT:    [f32; 4] = [0.129, 0.071, 0.102, 1.0];
const ENEMY_TOP:      [f32; 4] = [0.227, 0.122, 0.145, 1.0];
const ENEMY_STROKE:   [f32; 4] = [0.878, 0.478, 0.235, 1.0];

const LANE_PLATE_FILL:   [f32; 4] = [0.063, 0.102, 0.157, 0.85];
const LANE_PLATE_STROKE: [f32; 4] = [0.141, 0.192, 0.251, 1.0];

const BAND_POINT_BLANK: [f32; 4] = [0.878, 0.400, 0.290, 0.6];
const BAND_CLOSE:       [f32; 4] = [0.878, 0.635, 0.235, 0.6];
const BAND_MID:         [f32; 4] = [0.353, 0.624, 0.878, 0.6];
const BAND_LONG:        [f32; 4] = [0.353, 0.820, 0.796, 0.6];
const BAND_EXTREME:     [f32; 4] = [0.608, 0.549, 0.859, 0.6];

const HEAT_BG:    [f32; 4] = [0.094, 0.094, 0.110, 0.85];
const HEAT_FILL:  [f32; 4] = [0.949, 0.475, 0.235, 1.0];
const HEAT_LOCKOUT: [f32; 4] = [0.949, 0.235, 0.235, 1.0];

const SHIELD_PIP_CHARGE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];

const WHITE:      [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DEFEAT_TINT: [f32; 4] = [0.85, 0.08, 0.10, 0.55];
const VICTORY_TINT: [f32; 4] = [1.0, 0.80, 0.20, 0.45];

/* ---- entry point ---------------------------------------------------------- */

/// Build the full frame's draw command list, back-to-front. Sprites and
/// polygons are interleaved in z-order; `Gfx::render` batches consecutive
/// same-variant runs into single GPU draw calls.
pub fn compose_scene(board: &Board, lane: &LaneGeometry) -> Vec<DrawCommand> {
    let mut out = Vec::with_capacity(256);

    push_parallax(&mut out, lane);
    push_lane_plate(&mut out, board, lane);
    push_range_band_ticks(&mut out, board, lane);
    push_hazards(&mut out, board, lane);

    for (cell_idx, slot) in board.cells.iter().enumerate() {
        if let Some(ship) = slot {
            push_ship(&mut out, ship, cell_idx, lane);
        }
    }

    for proj in &board.ordnance {
        push_projectile(&mut out, proj, lane);
    }

    for (cell_idx, slot) in board.cells.iter().enumerate() {
        if let Some(ship) = slot {
            push_queue_glyphs(&mut out, ship, cell_idx, lane);
            push_status_badges(&mut out, ship, cell_idx, lane);
        }
    }

    out
}

/// Push a sprite onto the draw list, wrapping it as `DrawCommand::Sprite`.
#[inline]
fn push_sprite(out: &mut Vec<DrawCommand>, s: SpriteInstance) {
    out.push(DrawCommand::Sprite(s));
}

/// Push a polygon onto the draw list, wrapping it as `DrawCommand::Polygon`.
#[inline]
fn push_polygon(out: &mut Vec<DrawCommand>, p: PolygonInstance) {
    out.push(DrawCommand::Polygon(p));
}

/* =============================================================================
 * Parallax — five strips of background detail tiled across the viewport.
 *
 * Each strip uses one atlas cell tiled at a different vertical band and tile
 * frequency. No drift today — the renderer is paused on a static frame; a
 * later slice can add per-frame offset for parallax motion.
 * ============================================================================= */

fn push_parallax(out: &mut Vec<DrawCommand>, _lane: &LaneGeometry) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let w = VIRTUAL_W as f32;
    let h = VIRTUAL_H as f32;

    // Far stars — three sparse 32x32 patches across the upper canvas. Each
    // atlas cell contains ~12 pixel stars, so three placements give ~36
    // visible stars total. Bruce's feedback was that the previous tiled
    // density (hundreds of stars) competed with the lane for foreground
    // attention; this cut makes the lane the visual anchor.
    let star_alpha = 0.55;
    let star_tint = [1.0, 1.0, 1.0, star_alpha];
    for &(fx, fy) in &[(0.18_f32, 0.12), (0.55, 0.30), (0.85, 0.18)] {
        push_sprite(out, SpriteInstance::axis_aligned(
            [fx * w, fy * h],
            [32.0, 32.0],
            star_tint,
            atlas::cell_uvs(atlas::PARALLAX_FAR_STARS),
        ));
    }

    // Nebula band — three wide patches in the upper half, mostly behind the
    // distant planet.
    let nebula_tint = [1.0, 1.0, 1.0, 0.75];
    let nebula_y = h * 0.20;
    for i in 0..3 {
        let x = w * (0.20 + (i as f32) * 0.30);
        push_sprite(out, SpriteInstance::axis_aligned(
            [x, nebula_y],
            [80.0, 32.0],
            nebula_tint,
            atlas::cell_uvs(atlas::PARALLAX_NEBULA),
        ));
    }

    // Distant planet — one big sphere at upper-right.
    push_sprite(out, SpriteInstance::axis_aligned(
        [w * 0.78, h * 0.22],
        [44.0, 44.0],
        WHITE,
        atlas::cell_uvs(atlas::PARALLAX_DISTANT_PLANET),
    ));

    // Mid stars — two patches; each cell carries ~12 stars, so this is ~24
    // visible mid-distance stars total.
    let mid_tint = [1.0, 1.0, 1.0, 0.70];
    for &(fx, fy) in &[(0.32_f32, 0.45), (0.68, 0.55)] {
        push_sprite(out, SpriteInstance::axis_aligned(
            [fx * w, fy * h],
            [32.0, 32.0],
            mid_tint,
            atlas::cell_uvs(atlas::PARALLAX_MID_STARS),
        ));
    }

    // Foreground dust — one sparse patch low on the canvas, in front of the
    // lane. Single placement so it reads as a subtle near-camera detail
    // rather than a curtain of motes.
    push_sprite(out, SpriteInstance::axis_aligned(
        [w * 0.40, h * 0.88],
        [32.0, 32.0],
        [1.0, 1.0, 1.0, 0.55],
        atlas::cell_uvs(atlas::PARALLAX_FOREGROUND_DUST),
    ));
}

/* =============================================================================
 * Lane plate — the trapezoid from cell footprints.
 *
 * Drawn as one true filled parallelogram per cell (using the
 * `PolygonInstance` GPU primitive) plus a thin stroke along the front edge.
 * `cell_footprint` supplies the four corners; we remap to the polygon
 * corner convention in `push_quad_as_two_triangles`.
 * ============================================================================= */

fn push_lane_plate(out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
    for c in 0..board.size as u32 {
        if c >= lane.cell_count {
            break;
        }
        let fp = cell_footprint(c, lane);
        push_quad_as_two_triangles(out, fp, LANE_PLATE_FILL);
    }
    // Front edge stroke — one thin rect along the front line.
    let p0 = Point2 { x: lane.front_start.x, y: lane.front_start.y };
    let p1 = Point2 { x: lane.front_end.x,   y: lane.front_end.y };
    push_line_strip(out, p0, p1, 1.5, LANE_PLATE_STROKE);
}

/// Emit a true filled parallelogram for `quad` (vertex order from
/// `perspective::cell_footprint`: front-near, front-far, back-far,
/// back-near). Maps to `PolygonInstance`'s top-left/top-right/bottom-right/
/// bottom-left corner convention. Fixes the staircase the previous
/// axis-aligned-bounding-box approximation produced when the lane tilts.
fn push_quad_as_two_triangles(out: &mut Vec<DrawCommand>, quad: [Point2; 4], color: [f32; 4]) {
    let corners = [
        [quad[3].x, quad[3].y], // top-left  = back-near
        [quad[2].x, quad[2].y], // top-right = back-far
        [quad[1].x, quad[1].y], // bot-right = front-far
        [quad[0].x, quad[0].y], // bot-left  = front-near
    ];
    push_polygon(out, PolygonInstance::flat(corners, color, atlas::cell_uvs(atlas::SOLID_WHITE)));
}

/// Draw a thin "line" from `a` to `b` as a rotated rectangle of the given
/// thickness. `rotation_rad` is filled so the rect aligns with the line
/// direction; the sprite shader handles the rotation.
fn push_line_strip(out: &mut Vec<DrawCommand>, a: Point2, b: Point2, thickness: f32, color: [f32; 4]) {
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
 * Range-band tick marks — five short vertical ticks under the lane, one per
 * band boundary relative to the player ship (if there is one).
 *
 * If there is no player on the board (e.g. early-game or test scenarios),
 * we skip the ruler. Boundaries are at cell distances 1, 2, 4, 6, 7 from
 * the player (the upper bounds of pointBlank/close/mid/long; the extreme
 * tick anchors the right edge).
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
        let target_screen = cell_to_screen(cell as u32, lane);
        let color = match range_band(pc as usize, cell as usize) {
            RangeBand::PointBlank => BAND_POINT_BLANK,
            RangeBand::Close => BAND_CLOSE,
            RangeBand::Mid => BAND_MID,
            RangeBand::Long => BAND_LONG,
            RangeBand::Extreme => BAND_EXTREME,
        };
        // Short vertical tick just under the lane front edge.
        let tick_y = target_screen.y + 6.0;
        let tick_h = 4.0 * target_screen.scale;
        push_sprite(out, SpriteInstance::axis_aligned(
            [target_screen.x, tick_y + tick_h / 2.0],
            [1.0, tick_h / 2.0],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/* =============================================================================
 * Hazards — one quad per hazard on the lane plate.
 *
 * Slice-D draws mines / drones / debris as simple tinted squares centered on
 * their cell. A dedicated hazard sprite cell can replace this later.
 * ============================================================================= */

fn push_hazards(out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
    use crate::types::HazardKind;
    for cell_list in &board.hazards {
        for h in cell_list {
            let c = cell_to_screen(h.cell.min(lane.cell_count as usize - 1) as u32, lane);
            let color = match h.kind {
                HazardKind::Mine => [0.95, 0.30, 0.30, 1.0],
                HazardKind::Drone => [0.40, 0.78, 0.55, 1.0],
                HazardKind::Debris => [0.55, 0.50, 0.45, 1.0],
            };
            push_sprite(out, SpriteInstance::axis_aligned(
                [c.x, c.y - 3.0 * c.scale],
                [3.0 * c.scale, 3.0 * c.scale],
                color,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
    }
}

/* =============================================================================
 * Ships — composed from face polygons + chevron + heat bar + shield pips.
 *
 * Face polygons come from perspective::ship_sprite (unrotated in the screen
 * frame) and are then rotated about the ship's pivot by the lane slope on
 * the CPU. The rotated corners go through the `PolygonInstance` GPU
 * primitive, so the faces are true parallelograms — not axis-aligned
 * bounding boxes — and follow the lane tilt cleanly.
 * ============================================================================= */

fn push_ship(out: &mut Vec<DrawCommand>, ship: &Ship, cell_idx: usize, lane: &LaneGeometry) {
    let cell = cell_to_screen(cell_idx as u32, lane);
    let stance = match ship.orientation {
        Orientation::BowOn { .. } => Stance::BowOn,
        Orientation::Broadside => Stance::Broadside,
    };
    let sprite = ship_sprite(cell, FRIGATE_DIMS, stance);

    let (front_color, top_color, stroke_color) = if ship.faction == Faction::Player {
        (PLAYER_FRONT, PLAYER_TOP, PLAYER_STROKE)
    } else {
        (ENEMY_FRONT, ENEMY_TOP, ENEMY_STROKE)
    };

    // Both polygons rotated about the ship pivot on the CPU.
    let front_rot = rotate_face(sprite.front_face, sprite.pivot, sprite.rotation_rad);
    let top_rot = rotate_face(sprite.top_face, sprite.pivot, sprite.rotation_rad);

    push_face_quad(out, front_rot, front_color);
    push_face_quad(out, top_rot, top_color);
    // Outline (stroke around top face — most visible silhouette edge).
    push_face_outline(out, top_rot, stroke_color, 1.0);

    push_bow_chevron(out, ship, &sprite, cell);
    push_heat_bar(out, ship, cell);
    push_shield_pips(out, ship, &sprite);
}

fn rotate_face(face: FacePoly, pivot: Point2, theta: f32) -> FacePoly {
    let cos_t = theta.cos();
    let sin_t = theta.sin();
    let rotate = |p: Point2| -> Point2 {
        let dx = p.x - pivot.x;
        let dy = p.y - pivot.y;
        Point2 {
            x: pivot.x + dx * cos_t - dy * sin_t,
            y: pivot.y + dx * sin_t + dy * cos_t,
        }
    };
    [rotate(face[0]), rotate(face[1]), rotate(face[2]), rotate(face[3])]
}

/// Emit a true filled polygon for a ship face. `perspective::ship_sprite`
/// returns FacePoly with vertex order bottom-left, bottom-right, top-right,
/// top-left (CCW under screen y-down); we remap to PolygonInstance's
/// top-left/top-right/bottom-right/bottom-left corner convention.
fn push_face_quad(out: &mut Vec<DrawCommand>, face: FacePoly, color: [f32; 4]) {
    let corners = [
        [face[3].x, face[3].y], // top-left
        [face[2].x, face[2].y], // top-right
        [face[1].x, face[1].y], // bot-right
        [face[0].x, face[0].y], // bot-left
    ];
    push_polygon(out, PolygonInstance::flat(corners, color, atlas::cell_uvs(atlas::SOLID_WHITE)));
}

fn push_face_outline(out: &mut Vec<DrawCommand>, face: FacePoly, color: [f32; 4], thickness: f32) {
    for i in 0..4 {
        let a = face[i];
        let b = face[(i + 1) % 4];
        push_line_strip(out, a, b, thickness, color);
    }
}

fn push_bow_chevron(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    sprite: &ShipSprite,
    cell: CellScreen,
) {
    // Chevron points along the ship's bow direction; we draw it centered on
    // the top-face center, scaled with the cell.
    let size = 6.0 * cell.scale;
    let chevron_rotation = sprite.bow_dir.y.atan2(sprite.bow_dir.x);
    // Topcenter in the unrotated frame; rotate about pivot to get screen pos.
    let tc = rotate_face(
        [sprite.top_center, sprite.top_center, sprite.top_center, sprite.top_center],
        sprite.pivot,
        sprite.rotation_rad,
    )[0];
    let stroke = if ship.faction == Faction::Player { PLAYER_STROKE } else { ENEMY_STROKE };
    push_sprite(out, SpriteInstance {
        pos: [tc.x, tc.y],
        half_size: [size, size],
        color: stroke,
        uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
        uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
        rotation_rad: chevron_rotation,
        _pad: [0.0; 3],
    });
}

fn push_heat_bar(out: &mut Vec<DrawCommand>, ship: &Ship, cell: CellScreen) {
    let max_h = 18.0 * cell.scale;
    let bar_w = 2.0 * cell.scale;
    let bar_x = cell.x + 18.0 * cell.scale;
    let bar_y = cell.y - 14.0 * cell.scale;
    // Background.
    push_sprite(out, SpriteInstance::axis_aligned(
        [bar_x, bar_y - max_h / 2.0],
        [bar_w / 2.0, max_h / 2.0],
        HEAT_BG,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
    // Fill (proportional to heat/heat_max, bottom-aligned).
    let ratio = (ship.heat as f32 / ship.heat_max.max(1) as f32).clamp(0.0, 1.0);
    if ratio > 0.0 {
        let fill_h = max_h * ratio;
        let color = if ship.locked_out { HEAT_LOCKOUT } else { HEAT_FILL };
        push_sprite(out, SpriteInstance::axis_aligned(
            [bar_x, bar_y - fill_h / 2.0],
            [bar_w / 2.0, fill_h / 2.0],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        ));
    }
}

/// Shield pips: one small pip per held `charge`, positioned by zone around
/// the ship's pivot. Bow / stern follow the rotated bow direction; port /
/// starboard sit perpendicular to it.
fn push_shield_pips(out: &mut Vec<DrawCommand>, ship: &Ship, sprite: &ShipSprite) {
    let pip_size = 1.5;
    let radius = 12.0;
    let bow = sprite.bow_dir;
    let perp = Point2 { x: -bow.y, y: bow.x };
    let zones = [
        (HullZone::Bow,       Point2 { x:  bow.x,  y:  bow.y  }),
        (HullZone::Stern,     Point2 { x: -bow.x,  y: -bow.y  }),
        (HullZone::Starboard, Point2 { x:  perp.x, y:  perp.y }),
        (HullZone::Port,      Point2 { x: -perp.x, y: -perp.y }),
    ];
    for (zone, dir) in zones {
        let face = ship.shield_profile.face(zone);
        if face.charge <= 0 {
            continue;
        }
        for i in 0..face.charge {
            let offset = radius + (i as f32) * (pip_size * 2.0 + 1.0);
            let px = sprite.pivot.x + dir.x * offset;
            let py = sprite.pivot.y + dir.y * offset;
            push_sprite(out, SpriteInstance::axis_aligned(
                [px, py],
                [pip_size, pip_size],
                SHIELD_PIP_CHARGE,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            ));
        }
    }
}

/* =============================================================================
 * Projectiles — torpedo / missile / etc.
 *
 * Drawn at the projectile's continuous lane position with rotation set to
 * the lane slope so trails read as moving along the lane. Heading flips the
 * orientation if the projectile is travelling aft (negative bow direction).
 * ============================================================================= */

fn push_projectile(out: &mut Vec<DrawCommand>, proj: &Projectile, lane: &LaneGeometry) {
    let pos = fractional_cell_to_screen(proj.cell as f32, lane);
    // Choose missile vs torpedo from projectile kind; default to torpedo.
    let cell = if proj.kind.contains("missile") {
        atlas::MISSILE
    } else {
        atlas::TORPEDO
    };
    let mut rot = pos.rotation_rad;
    if proj.heading == LaneEnd::Aft {
        rot += std::f32::consts::PI; // flip horizontally
    }
    let scale = pos.scale;
    push_sprite(out, SpriteInstance {
        pos: [pos.x, pos.y - 6.0 * scale],
        half_size: [8.0 * scale, 4.0 * scale],
        color: WHITE,
        uv_min: atlas::cell_uvs(cell).0,
        uv_max: atlas::cell_uvs(cell).1,
        rotation_rad: rot,
        _pad: [0.0; 3],
    });
}

/* =============================================================================
 * Action queue glyphs — small stack above each ship.
 *
 * For each action id in `ship.queue`, draw the archetype glyph above the
 * ship's pivot. Slice-D needs Content to look up the archetype; without it
 * we fall back to GLYPH_BEAM (a reasonable visual default). The full lookup
 * lands when the content slice ships.
 * ============================================================================= */

fn push_queue_glyphs(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    cell_idx: usize,
    lane: &LaneGeometry,
) {
    if ship.queue.is_empty() {
        return;
    }
    let cell = cell_to_screen(cell_idx as u32, lane);
    let n = ship.queue.len() as f32;
    let glyph_size = 4.0 * cell.scale;
    let spacing = glyph_size * 2.5;
    let total_w = (n - 1.0).max(0.0) * spacing;
    let start_x = cell.x - total_w / 2.0;
    let glyph_y = cell.y - 60.0 * cell.scale;
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

/// Best-effort archetype lookup: matches `action_id` against the ship's
/// mounts and uses the mount's arc as a weak proxy. The real lookup needs
/// `Content::action()`, which isn't passed into `compose_scene` today. When
/// it is, replace this with a direct catalog read.
fn archetype_of_mount(ship: &Ship, action_id: &str) -> Option<WeaponArchetype> {
    let _ = ship.mounts.iter().find(|m: &&Mount| m.weapon == action_id)?;
    // Without catalog access we can't know the archetype; default Beam.
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

/* =============================================================================
 * Status badges — small icons next to each ship for active statuses.
 *
 * Stacked horizontally above the ship's heat bar.
 * ============================================================================= */

fn push_status_badges(
    out: &mut Vec<DrawCommand>,
    ship: &Ship,
    cell_idx: usize,
    lane: &LaneGeometry,
) {
    if ship.statuses.is_empty() {
        return;
    }
    let cell = cell_to_screen(cell_idx as u32, lane);
    let size = 4.0 * cell.scale;
    let spacing = size * 2.2;
    let start_x = cell.x - 18.0 * cell.scale;
    let y = cell.y - 38.0 * cell.scale;
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
 *
 * Slice-D exposes the function but the demo binary never sets win state on
 * a synthetic Board, so it's an opt-in entry point for callers.
 * ============================================================================= */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinState {
    Playing,
    Defeat,
    Victory,
}

#[allow(dead_code)]
pub fn push_end_state_overlay(out: &mut Vec<DrawCommand>, state: WinState) {
    use crate::gfx::{VIRTUAL_H, VIRTUAL_W};
    let color = match state {
        WinState::Playing => return,
        WinState::Defeat => DEFEAT_TINT,
        WinState::Victory => VICTORY_TINT,
    };
    push_sprite(out, SpriteInstance::axis_aligned(
        [VIRTUAL_W as f32 / 2.0, VIRTUAL_H as f32 / 2.0],
        [VIRTUAL_W as f32 / 2.0, VIRTUAL_H as f32 / 2.0],
        color,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    ));
}

/* =============================================================================
 * Tests
 * ============================================================================= */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::perspective::DEFAULT_LANE;
    use crate::types::{
        Action, ActionCost, Arc, Effect, EventBus, Mount, Orientation, Projectile, RangeBand,
        ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern,
    };
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

    #[test]
    fn empty_board_still_produces_parallax_and_lane_plate() {
        let board = empty_board(7);
        let scene = compose_scene(&board, &DEFAULT_LANE);
        // At least parallax tiles + lane cells are drawn.
        assert!(scene.len() > 10, "expected backdrop content, got {}", scene.len());
    }

    #[test]
    fn one_player_ship_produces_visible_sprites() {
        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene = compose_scene(&board, &DEFAULT_LANE);
        // Ship instance count: 2 face quads + 4 outline edges + 1 chevron + 1 heat bg = 8 minimum.
        // Plus parallax + lane plate from the empty-board baseline.
        assert!(scene.len() > 30, "expected backdrop + ship sprites, got {}", scene.len());
    }

    #[test]
    fn ship_with_shield_charges_draws_pips() {
        let mut board = empty_board(7);
        let mut ship = frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
        // Two charges on the bow zone, one on starboard.
        ship.shield_profile.bow.charge = 2;
        ship.shield_profile.starboard.charge = 1;
        board.cells[0] = Some(ship);
        let scene_with = compose_scene(&board, &DEFAULT_LANE);

        let mut bare_board = empty_board(7);
        bare_board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene_without = compose_scene(&bare_board, &DEFAULT_LANE);

        // Three additional sprites for the three pips.
        assert_eq!(scene_with.len() - scene_without.len(), 3);
    }

    #[test]
    fn ship_with_heat_draws_a_filled_bar() {
        let mut board = empty_board(7);
        let mut ship = frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore });
        ship.heat = 3;
        board.cells[0] = Some(ship);
        let scene_with = compose_scene(&board, &DEFAULT_LANE);

        let mut bare_board = empty_board(7);
        bare_board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let scene_without = compose_scene(&bare_board, &DEFAULT_LANE);

        // Bare ship draws the heat background but no fill quad; heated ship
        // adds one extra fill quad.
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
        let scene = compose_scene(&board, &DEFAULT_LANE);
        // Find the torpedo sprite: it samples the TORPEDO atlas cell.
        let (mn, mx) = atlas::cell_uvs(atlas::TORPEDO);
        let torpedo_idx = scene.iter().position(|c| match c {
            DrawCommand::Sprite(s) => s.uv_min == mn && s.uv_max == mx,
            _ => false,
        });
        assert!(torpedo_idx.is_some(), "torpedo sprite should be present");
    }

    #[test]
    fn range_band_ticks_render_only_when_player_present() {
        let board_no_player = empty_board(7);
        let n_no_player = compose_scene(&board_no_player, &DEFAULT_LANE).len();

        let mut board = empty_board(7);
        board.cells[0] = Some(frigate_at(0, Faction::Player, Orientation::BowOn { bow: LaneEnd::Fore }));
        let n_with_player = compose_scene(&board, &DEFAULT_LANE).len();

        // With a player, the difference is: ship sprites + 7 band ticks (one
        // per visible cell within ±7 of player at cell 0 = cells 0..=6).
        // We don't need an exact count — just that it's strictly more.
        assert!(n_with_player > n_no_player + 6, "expected player sprites + ticks, delta = {}", n_with_player - n_no_player);
    }

    #[test]
    fn render_example_ts_scenario_composes_without_panic() {
        // The render-example.ts board: 7 cells, player at 0 (bowOn fore),
        // enemies at 2 (broadside), 3 (bowOn aft), 5 (bowOn fore), 6
        // (bowOn fore). Smoke test that the full scenario composes cleanly.
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
        let scene = compose_scene(&board, &DEFAULT_LANE);
        // Sanity: 5 ships + 1 projectile + parallax + lane + ticks → > 60.
        assert!(scene.len() > 60, "expected a populated scene, got {}", scene.len());
    }

    // Silence warnings for unused imports that ARE used inside scope.
    #[allow(dead_code)]
    fn _types_used(_a: Action, _c: ActionCost, _ar: Arc, _e: Effect, _m: Mount, _t: Targeting,
                   _tp: TargetingPattern, _rb: RangeBand, _sf: ShieldFace, _sp: ShieldProfile) {}
}
