//! Procedural sprite atlas. Generates a single RGBA8 texture at startup with
//! placeholder pixel-art for every Broadside sprite.
//!
//! The atlas is a fixed 8x8 grid of 32x32 cells, packed into a 256x256 RGBA8
//! texture. A cell is referenced by `(col, row)`; [`cell_uvs`] converts a
//! cell coord into the normalized UV rectangle used by the sprite shader.
//!
//! The atlas is **decorative**: ship hulls are drawn as tinted rectangles by
//! the renderer using polygon vertices from [`crate::perspective::ship_sprite`],
//! so the atlas does not carry ship art. Cells here carry direction-specific
//! detail the polygon math can't supply (bow chevron, torpedo silhouette),
//! parallax-layer pixel-art, and HUD glyphs.
//!
//! Palette is sampled from the analysis HTML's CSS tokens (`--ink`, `--gold`,
//! `--vermillion`, `--c-beam`, `--c-ord`, …); each cell function picks a few
//! to stay on-brand.

pub const ATLAS_SIZE: u32 = 256;
pub const CELL_SIZE: u32 = 32;
pub const CELLS_PER_ROW: u32 = ATLAS_SIZE / CELL_SIZE; // 8

/* =============================================================================
 * Cell map — keep grouped by row so the atlas image reads as related strips.
 *
 *   Row 0 — projectiles + chevron
 *   Row 1 — action-queue glyphs (one per WeaponArchetype, 7 total)
 *   Row 2 — telegraph intent icons (6)
 *   Row 3 — status badges (4)
 *   Row 4 — parallax layer art (far stars, nebula, distant planet, mid stars,
 *           foreground dust)
 *   Row 5 / 6 — reserved for future ship-class detail / decals
 *   Row 7 — SOLID_WHITE at (7, 7) for all flat-color tinted quads
 * ============================================================================= */

pub const BOW_CHEVRON: (u32, u32) = (0, 0);
pub const TORPEDO: (u32, u32) = (1, 0);
pub const MISSILE: (u32, u32) = (2, 0);

pub const GLYPH_BEAM: (u32, u32) = (0, 1);
pub const GLYPH_ORDNANCE: (u32, u32) = (1, 1);
pub const GLYPH_BROADSIDE: (u32, u32) = (2, 1);
pub const GLYPH_DISPLACEMENT: (u32, u32) = (3, 1);
pub const GLYPH_CONTROL: (u32, u32) = (4, 1);
pub const GLYPH_MOVEMENT: (u32, u32) = (5, 1);
pub const GLYPH_DEFENSIVE: (u32, u32) = (6, 1);

pub const TELEGRAPH_FIRE: (u32, u32) = (0, 2);
pub const TELEGRAPH_LOCK: (u32, u32) = (1, 2);
pub const TELEGRAPH_PUSH: (u32, u32) = (2, 2);
pub const TELEGRAPH_PULL: (u32, u32) = (3, 2);
pub const TELEGRAPH_REORIENT: (u32, u32) = (4, 2);
pub const TELEGRAPH_DEPLOY: (u32, u32) = (5, 2);

pub const STATUS_HULL_BREACH: (u32, u32) = (0, 3);
pub const STATUS_SYSTEMS_OFFLINE: (u32, u32) = (1, 3);
pub const STATUS_TARGET_LOCK: (u32, u32) = (2, 3);
pub const STATUS_SHIELDS_UP: (u32, u32) = (3, 3);

pub const PARALLAX_FAR_STARS: (u32, u32) = (0, 4);
pub const PARALLAX_NEBULA: (u32, u32) = (1, 4);
pub const PARALLAX_DISTANT_PLANET: (u32, u32) = (2, 4);
pub const PARALLAX_MID_STARS: (u32, u32) = (3, 4);
pub const PARALLAX_FOREGROUND_DUST: (u32, u32) = (4, 4);

/// Solid white cell. Multiply by the instance color tint to render a flat
/// colored quad — the workhorse for heat bars, range-band ticks, ship faces,
/// the lane plate, and end-state overlays.
pub const SOLID_WHITE: (u32, u32) = (7, 7);

/// Convert (col, row) cell coordinates to a `(uv_min, uv_max)` tuple, each
/// in normalized [0, 1] texture space.
pub fn cell_uvs(cell: (u32, u32)) -> ([f32; 2], [f32; 2]) {
    let s = CELL_SIZE as f32 / ATLAS_SIZE as f32;
    let (c, r) = cell;
    (
        [c as f32 * s, r as f32 * s],
        [(c + 1) as f32 * s, (r + 1) as f32 * s],
    )
}

/// Generate the entire atlas as a tight RGBA8 byte buffer
/// (ATLAS_SIZE * ATLAS_SIZE * 4 bytes).
pub fn generate_atlas() -> Vec<u8> {
    let mut buf = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];

    // SOLID_WHITE first so every tinted-quad path works even if the rest of
    // the atlas hasn't run yet.
    fill_cell(&mut buf, SOLID_WHITE, [255, 255, 255, 255]);

    draw_bow_chevron(&mut buf, BOW_CHEVRON);
    draw_torpedo(&mut buf, TORPEDO);
    draw_missile(&mut buf, MISSILE);

    draw_glyph_beam(&mut buf, GLYPH_BEAM);
    draw_glyph_ordnance(&mut buf, GLYPH_ORDNANCE);
    draw_glyph_broadside(&mut buf, GLYPH_BROADSIDE);
    draw_glyph_displacement(&mut buf, GLYPH_DISPLACEMENT);
    draw_glyph_control(&mut buf, GLYPH_CONTROL);
    draw_glyph_movement(&mut buf, GLYPH_MOVEMENT);
    draw_glyph_defensive(&mut buf, GLYPH_DEFENSIVE);

    draw_telegraph_fire(&mut buf, TELEGRAPH_FIRE);
    draw_telegraph_lock(&mut buf, TELEGRAPH_LOCK);
    draw_telegraph_push(&mut buf, TELEGRAPH_PUSH);
    draw_telegraph_pull(&mut buf, TELEGRAPH_PULL);
    draw_telegraph_reorient(&mut buf, TELEGRAPH_REORIENT);
    draw_telegraph_deploy(&mut buf, TELEGRAPH_DEPLOY);

    draw_status_hull_breach(&mut buf, STATUS_HULL_BREACH);
    draw_status_systems_offline(&mut buf, STATUS_SYSTEMS_OFFLINE);
    draw_status_target_lock(&mut buf, STATUS_TARGET_LOCK);
    draw_status_shields_up(&mut buf, STATUS_SHIELDS_UP);

    draw_parallax_far_stars(&mut buf, PARALLAX_FAR_STARS);
    draw_parallax_nebula(&mut buf, PARALLAX_NEBULA);
    draw_parallax_distant_planet(&mut buf, PARALLAX_DISTANT_PLANET);
    draw_parallax_mid_stars(&mut buf, PARALLAX_MID_STARS);
    draw_parallax_foreground_dust(&mut buf, PARALLAX_FOREGROUND_DUST);

    buf
}

/* ---- low-level primitives ------------------------------------------------- */

pub(crate) fn put_pixel(buf: &mut [u8], x: u32, y: u32, rgba: [u8; 4]) {
    if x >= ATLAS_SIZE || y >= ATLAS_SIZE {
        return;
    }
    let i = ((y * ATLAS_SIZE + x) * 4) as usize;
    buf[i] = rgba[0];
    buf[i + 1] = rgba[1];
    buf[i + 2] = rgba[2];
    buf[i + 3] = rgba[3];
}

pub(crate) fn fill_rect(buf: &mut [u8], x: u32, y: u32, w: u32, h: u32, rgba: [u8; 4]) {
    for dy in 0..h {
        for dx in 0..w {
            put_pixel(buf, x + dx, y + dy, rgba);
        }
    }
}

pub(crate) fn fill_cell(buf: &mut [u8], cell: (u32, u32), rgba: [u8; 4]) {
    let cx = cell.0 * CELL_SIZE;
    let cy = cell.1 * CELL_SIZE;
    fill_rect(buf, cx, cy, CELL_SIZE, CELL_SIZE, rgba);
}

pub(crate) fn cell_origin(cell: (u32, u32)) -> (u32, u32) {
    (cell.0 * CELL_SIZE, cell.1 * CELL_SIZE)
}

/* ---- palette --------------------------------------------------------------
 *
 * Analysis HTML CSS tokens, transcribed to RGBA. Used throughout below.
 * ------------------------------------------------------------------------ */

const GOLD: [u8; 4] = [0x54, 0xcf, 0xc9, 255]; // --gold (teal-leaning)
const VERMILLION: [u8; 4] = [0xe0, 0x7a, 0x3c, 255]; // --vermillion
const C_BEAM: [u8; 4] = [0x5a, 0xd1, 0xcb, 255]; // beam archetype
const C_ORD: [u8; 4] = [0xe0, 0xa2, 0x3c, 255]; // ordnance archetype
const C_BROAD: [u8; 4] = [0xe0, 0x66, 0x4a, 255]; // broadside archetype
const C_DISP: [u8; 4] = [0x9b, 0x8c, 0xdb, 255]; // displacement archetype
const C_CTRL: [u8; 4] = [0x6f, 0xbf, 0x7a, 255]; // control archetype
const C_MOVE: [u8; 4] = [0x5a, 0x9f, 0xe0, 255]; // movement archetype
const C_DEF: [u8; 4] = [0x8a, 0xa0, 0xb8, 255]; // defensive archetype
const PAPER_DIM: [u8; 4] = [0x93, 0xa6, 0xbd, 255];

/* ---- projectiles ---------------------------------------------------------- */

/// Right-pointing chevron. Three diagonal pixel runs converging on the tip.
/// The renderer rotates this around its center by the lane slope + bow-on/aft
/// direction.
fn draw_bow_chevron(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let stroke = GOLD;
    let glow = [GOLD[0] / 2, GOLD[1] / 2, GOLD[2] / 2, 200];
    // Two stacked strokes form a > pointing right (tip at the right edge).
    for i in 0..10u32 {
        // upper diagonal
        put_pixel(buf, cx + 6 + i, cy + 8 + i, stroke);
        if i > 0 {
            put_pixel(buf, cx + 6 + i - 1, cy + 8 + i, glow);
        }
        // lower diagonal
        put_pixel(buf, cx + 6 + i, cy + 22 - i, stroke);
        if i > 0 {
            put_pixel(buf, cx + 6 + i - 1, cy + 22 - i, glow);
        }
    }
    // Glow tip
    put_pixel(buf, cx + 17, cy + 15, stroke);
    put_pixel(buf, cx + 17, cy + 16, stroke);
}

/// Torpedo: a horizontal capsule with a bright nose and a tapering tail
/// flame. Points right in the unrotated cell; renderer rotates by heading.
fn draw_torpedo(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let body = [180, 170, 160, 255];
    let nose = [240, 220, 120, 255];
    let flame_hot = [255, 180, 60, 255];
    let flame_cool = [200, 80, 40, 200];

    // Capsule body (rows 14..18, cols 8..24).
    fill_rect(buf, cx + 8, cy + 14, 16, 4, body);
    // Nose taper (cols 24..27, rounding).
    put_pixel(buf, cx + 24, cy + 14, body);
    put_pixel(buf, cx + 24, cy + 17, body);
    fill_rect(buf, cx + 24, cy + 15, 2, 2, body);
    put_pixel(buf, cx + 26, cy + 15, nose);
    put_pixel(buf, cx + 26, cy + 16, nose);

    // Tail flame (cols 1..8, narrowing).
    for i in 0..7u32 {
        let x = cx + 7 - i;
        let h = if i < 3 { 4 } else { 4 - (i - 2) };
        let y = cy + 16 - h / 2;
        let c = if i < 2 { flame_hot } else { flame_cool };
        fill_rect(buf, x, y, 1, h, c);
    }
}

/// Missile: smaller, faster-looking — three thin pixels in a row with a
/// short flame and pointy nose. Same orientation convention as torpedo.
fn draw_missile(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let body = [200, 200, 200, 255];
    let nose = [255, 100, 80, 255];
    let flame = [255, 200, 80, 255];

    fill_rect(buf, cx + 10, cy + 15, 12, 2, body);
    put_pixel(buf, cx + 22, cy + 14, body);
    put_pixel(buf, cx + 22, cy + 17, body);
    put_pixel(buf, cx + 23, cy + 15, nose);
    put_pixel(buf, cx + 23, cy + 16, nose);

    // Two-pixel flame.
    put_pixel(buf, cx + 9, cy + 15, flame);
    put_pixel(buf, cx + 9, cy + 16, flame);
    put_pixel(buf, cx + 8, cy + 15, [180, 80, 40, 180]);
    put_pixel(buf, cx + 8, cy + 16, [180, 80, 40, 180]);
}

/* ---- action-queue glyphs --------------------------------------------------
 *
 * One per `WeaponArchetype`. Each is a centered 16x16-ish pictogram in the
 * archetype's palette color from the analysis HTML. Stacked above the player
 * by `hud::compose_scene` to show the queue contents.
 * ------------------------------------------------------------------------ */

/// Beam — a horizontal lightning bolt zig-zag.
fn draw_glyph_beam(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_BEAM;
    let bg = [c[0] / 3, c[1] / 3, c[2] / 3, 160];
    // Three connected diagonal segments.
    for x in 8..14u32 {
        put_pixel(buf, cx + x, cy + 20 - (x - 8), c);
        put_pixel(buf, cx + x, cy + 21 - (x - 8), bg);
    }
    for x in 14..20u32 {
        put_pixel(buf, cx + x, cy + 12 + (x - 14), c);
        put_pixel(buf, cx + x, cy + 13 + (x - 14), bg);
    }
    for x in 20..26u32 {
        put_pixel(buf, cx + x, cy + 18 - (x - 20), c);
        put_pixel(buf, cx + x, cy + 19 - (x - 20), bg);
    }
}

/// Ordnance — a small filled circle (torpedo head profile).
fn draw_glyph_ordnance(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_ORD;
    filled_circle(buf, cx + 16, cy + 16, 7, c);
    // Trail dots behind.
    put_pixel(buf, cx + 6, cy + 16, c);
    put_pixel(buf, cx + 4, cy + 16, [c[0] / 2, c[1] / 2, c[2] / 2, 180]);
}

/// Broadside — two opposing arrows in a vertical band.
fn draw_glyph_broadside(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_BROAD;
    // Horizontal band across the cell.
    fill_rect(
        buf,
        cx + 4,
        cy + 14,
        24,
        4,
        [c[0] / 4, c[1] / 4, c[2] / 4, 180],
    );
    // Left-pointing arrow.
    for i in 0..6u32 {
        let half = i.min(3);
        for dy in 0..=2 * half {
            put_pixel(buf, cx + 4 + i, cy + 13 + half + dy, c);
        }
    }
    // Right-pointing arrow.
    for i in 0..6u32 {
        let half = i.min(3);
        for dy in 0..=2 * half {
            put_pixel(buf, cx + 27 - i, cy + 13 + half + dy, c);
        }
    }
}

/// Displacement — a tractor-beam-style ⇄ arrow pair.
fn draw_glyph_displacement(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_DISP;
    // Two stacked arrows pointing opposite directions.
    // Top arrow points right.
    fill_rect(buf, cx + 6, cy + 10, 18, 2, c);
    put_pixel(buf, cx + 22, cy + 8, c);
    put_pixel(buf, cx + 23, cy + 9, c);
    put_pixel(buf, cx + 22, cy + 13, c);
    put_pixel(buf, cx + 23, cy + 12, c);
    // Bottom arrow points left.
    fill_rect(buf, cx + 8, cy + 20, 18, 2, c);
    put_pixel(buf, cx + 10, cy + 18, c);
    put_pixel(buf, cx + 9, cy + 19, c);
    put_pixel(buf, cx + 10, cy + 23, c);
    put_pixel(buf, cx + 9, cy + 22, c);
}

/// Control — a small spider-web (status-effect feel).
fn draw_glyph_control(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_CTRL;
    // Cross hairs + diagonals through the center.
    for i in 0..14u32 {
        put_pixel(buf, cx + 9 + i, cy + 16, c);
        put_pixel(buf, cx + 16, cy + 9 + i, c);
        if i < 10 {
            put_pixel(buf, cx + 11 + i, cy + 11 + i, c);
            put_pixel(buf, cx + 21 - i, cy + 11 + i, c);
        }
    }
    // Dot at the intersection.
    fill_rect(buf, cx + 15, cy + 15, 3, 3, c);
}

/// Movement — a forward chevron in the movement-archetype color.
fn draw_glyph_movement(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_MOVE;
    for i in 0..10u32 {
        put_pixel(buf, cx + 10 + i, cy + 9 + i, c);
        put_pixel(buf, cx + 10 + i, cy + 23 - i, c);
        put_pixel(buf, cx + 11 + i, cy + 9 + i, c);
        put_pixel(buf, cx + 11 + i, cy + 23 - i, c);
    }
}

/// Defensive — a shield outline.
fn draw_glyph_defensive(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_DEF;
    let dim = [c[0] / 2, c[1] / 2, c[2] / 2, 200];
    // Shield body.
    fill_rect(buf, cx + 11, cy + 9, 10, 14, dim);
    // Outline.
    for y in 9..23u32 {
        put_pixel(buf, cx + 11, cy + y, c);
        put_pixel(buf, cx + 20, cy + y, c);
    }
    for x in 11..21u32 {
        put_pixel(buf, cx + x, cy + 9, c);
    }
    // Pointed bottom.
    for i in 0..4u32 {
        put_pixel(buf, cx + 11 + i, cy + 23 + i, c);
        put_pixel(buf, cx + 20 - i, cy + 23 + i, c);
    }
}

/* ---- telegraph icons ------------------------------------------------------ */

/// Fire intent — an explosive starburst.
fn draw_telegraph_fire(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = VERMILLION;
    let inner = [c[0] + 20, c[1], c[2], 255];
    filled_circle(buf, cx + 16, cy + 16, 5, inner);
    // Eight rays.
    for ang in 0..8u32 {
        let (dx, dy) = match ang {
            0 => (0_i32, -10_i32),
            1 => (7, -7),
            2 => (10, 0),
            3 => (7, 7),
            4 => (0, 10),
            5 => (-7, 7),
            6 => (-10, 0),
            _ => (-7, -7),
        };
        let nx = 16 + dx;
        let ny = 16 + dy;
        if (0..32).contains(&nx) && (0..32).contains(&ny) {
            put_pixel(buf, cx + nx as u32, cy + ny as u32, c);
        }
    }
}

/// Target lock — a square reticle with a center dot.
fn draw_telegraph_lock(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = VERMILLION;
    // Outer corners only (open square).
    for i in 0..6u32 {
        // top-left
        put_pixel(buf, cx + 8 + i, cy + 8, c);
        put_pixel(buf, cx + 8, cy + 8 + i, c);
        // top-right
        put_pixel(buf, cx + 23 - i, cy + 8, c);
        put_pixel(buf, cx + 23, cy + 8 + i, c);
        // bottom-left
        put_pixel(buf, cx + 8 + i, cy + 23, c);
        put_pixel(buf, cx + 8, cy + 23 - i, c);
        // bottom-right
        put_pixel(buf, cx + 23 - i, cy + 23, c);
        put_pixel(buf, cx + 23, cy + 23 - i, c);
    }
    // Center dot.
    fill_rect(buf, cx + 15, cy + 15, 3, 3, c);
}

/// Displacement intent — push: arrow pointing right with a trailing pulse.
fn draw_telegraph_push(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_DISP;
    fill_rect(buf, cx + 6, cy + 14, 18, 4, c);
    // Arrowhead.
    for i in 0..5u32 {
        let half = i.min(3);
        for dy in 0..=2 * half {
            put_pixel(buf, cx + 24 + i, cy + 13 + half + dy, c);
        }
    }
}

/// Displacement intent — pull: arrow pointing left.
fn draw_telegraph_pull(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_DISP;
    fill_rect(buf, cx + 8, cy + 14, 18, 4, c);
    for i in 0..5u32 {
        let half = i.min(3);
        for dy in 0..=2 * half {
            put_pixel(buf, cx + 7 - i, cy + 13 + half + dy, c);
        }
    }
}

/// Reorient — a circular arrow.
fn draw_telegraph_reorient(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = GOLD;
    // Ring drawn as a circle with a small wedge missing on the right.
    for ang_deg in (0..360).step_by(8) {
        if (320..360).contains(&ang_deg) {
            continue;
        }
        let a = (ang_deg as f32).to_radians();
        let x = 16.0 + 10.0 * a.cos();
        let y = 16.0 + 10.0 * a.sin();
        put_pixel(buf, cx + x as u32, cy + y as u32, c);
        // Thicker ring
        let x2 = 16.0 + 11.0 * a.cos();
        let y2 = 16.0 + 11.0 * a.sin();
        put_pixel(buf, cx + x2 as u32, cy + y2 as u32, c);
    }
    // Arrowhead at the gap.
    put_pixel(buf, cx + 25, cy + 13, c);
    put_pixel(buf, cx + 26, cy + 14, c);
    put_pixel(buf, cx + 27, cy + 15, c);
    put_pixel(buf, cx + 26, cy + 16, c);
    put_pixel(buf, cx + 25, cy + 17, c);
}

/// Deploy intent — a downward arrow + small hazard square.
fn draw_telegraph_deploy(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = C_ORD;
    // Vertical shaft.
    fill_rect(buf, cx + 14, cy + 4, 4, 14, c);
    // Arrowhead down.
    for i in 0..5u32 {
        let half = i.min(3);
        for dx in 0..=2 * half {
            put_pixel(buf, cx + 13 + half + dx, cy + 18 + i, c);
        }
    }
    // Hazard square at the bottom.
    fill_rect(
        buf,
        cx + 10,
        cy + 25,
        12,
        4,
        [c[0] / 2, c[1] / 2, c[2] / 2, 220],
    );
}

/* ---- status badges -------------------------------------------------------- */

/// Hull breach — a small flame outline.
fn draw_status_hull_breach(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let outer = [255, 90, 40, 255];
    let inner = [255, 200, 80, 255];
    // Teardrop shape.
    for y in 8..24u32 {
        let half = ((24 - y) * (24 - y)) / 18;
        let half = half.min(7);
        for x in (16 - half)..=(16 + half) {
            put_pixel(buf, cx + x, cy + y, outer);
        }
    }
    // Inner highlight.
    fill_rect(buf, cx + 14, cy + 14, 4, 6, inner);
}

/// Systems offline — a power-off symbol (circle with a vertical break).
fn draw_status_systems_offline(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = PAPER_DIM;
    // Open ring.
    for ang_deg in (0..360).step_by(10) {
        if (260..280).contains(&ang_deg) {
            continue;
        }
        let a = (ang_deg as f32).to_radians();
        let x = 16.0 + 9.0 * a.cos();
        let y = 16.0 + 9.0 * a.sin();
        put_pixel(buf, cx + x as u32, cy + y as u32, c);
    }
    // Vertical line breaking the top.
    fill_rect(buf, cx + 15, cy + 5, 2, 10, c);
}

/// Target lock badge — a small reticle (slim version of the telegraph).
fn draw_status_target_lock(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = VERMILLION;
    // Slim cross-hairs through center.
    fill_rect(buf, cx + 7, cy + 15, 18, 2, c);
    fill_rect(buf, cx + 15, cy + 7, 2, 18, c);
    // Square dot.
    fill_rect(buf, cx + 14, cy + 14, 4, 4, [c[0], c[1] / 2, c[2] / 4, 255]);
}

/// Shields up — a small shield outline filled with the gold tone.
fn draw_status_shields_up(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let c = GOLD;
    let dim = [c[0] / 3, c[1] / 3, c[2] / 3, 220];
    fill_rect(buf, cx + 11, cy + 9, 10, 12, dim);
    for y in 9..21u32 {
        put_pixel(buf, cx + 11, cy + y, c);
        put_pixel(buf, cx + 20, cy + y, c);
    }
    for x in 11..21u32 {
        put_pixel(buf, cx + x, cy + 9, c);
    }
    for i in 0..4u32 {
        put_pixel(buf, cx + 11 + i, cy + 21 + i, c);
        put_pixel(buf, cx + 20 - i, cy + 21 + i, c);
    }
}

/* ---- parallax layer art -------------------------------------------------- */

/// Far stars — sparse 1-pixel pinpricks on a transparent background. Tiled
/// across the backdrop by the renderer for an even starfield.
fn draw_parallax_far_stars(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    // Deterministic pseudo-random scatter.
    let stars = [
        (3_u32, 5_u32),
        (7, 11),
        (12, 3),
        (19, 17),
        (26, 7),
        (4, 23),
        (29, 26),
        (15, 28),
        (22, 12),
        (9, 19),
        (28, 18),
        (1, 14),
    ];
    let dim = [180, 200, 220, 160];
    let bright = [240, 250, 255, 220];
    for (i, (x, y)) in stars.iter().enumerate() {
        let c = if i % 4 == 0 { bright } else { dim };
        put_pixel(buf, cx + x, cy + y, c);
    }
}

/// Nebula — a soft cloud of two complementary tints on transparent.
fn draw_parallax_nebula(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    // Three overlapping gaussian-ish lobes (purple + blue) with a SMOOTH alpha
    // falloff by distance, so the cloud feathers at its edges instead of the
    // two hard-edged ovals the threshold version drew (#46). Alpha sums across
    // lobes where they overlap, giving a denser core.
    let lobes: [(f32, f32, f32, [u8; 3]); 3] = [
        (9.0, 13.0, 7.5, [120, 80, 180]),  // purple, left
        (20.0, 15.0, 6.5, [80, 120, 200]), // blue, right
        (14.0, 18.0, 5.0, [100, 90, 190]), // violet, lower-mid blend
    ];
    for y in 0..CELL_SIZE as i32 {
        for x in 0..CELL_SIZE as i32 {
            let mut a = 0.0_f32;
            let mut r = 0.0_f32;
            let mut g = 0.0_f32;
            let mut b = 0.0_f32;
            for (lx, ly, rad, col) in &lobes {
                let dx = x as f32 - lx;
                let dy = y as f32 - ly;
                let d2 = dx * dx + dy * dy;
                // Smooth falloff: 1 at centre → 0 at radius, clamped.
                let f = (1.0 - d2 / (rad * rad)).clamp(0.0, 1.0);
                if f > 0.0 {
                    let wa = f * 0.55; // peak per-lobe alpha
                    a += wa;
                    r += col[0] as f32 * wa;
                    g += col[1] as f32 * wa;
                    b += col[2] as f32 * wa;
                }
            }
            if a > 0.01 {
                let inv = 1.0 / a;
                let alpha = (a * 160.0).min(150.0) as u8; // cap so it stays a wisp
                put_pixel(
                    buf,
                    cx + x as u32,
                    cy + y as u32,
                    [(r * inv) as u8, (g * inv) as u8, (b * inv) as u8, alpha],
                );
            }
        }
    }
}

/// Distant planet — a single shaded sphere in the cell center. One cell ==
/// one whole planet on screen; the renderer places it at a specific lane
/// background position.
fn draw_parallax_distant_planet(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    // Four tones dark→light. Shaded by a SMOOTH terminator (dot of the surface
    // normal with an upper-left light), bucketed into bands — no hard diagonal
    // seam (the old `dx+dy<0` half-plane split read as a straight line across
    // the sphere; #46).
    let tones = [
        [38, 46, 66, 255],    // night side
        [60, 70, 92, 255],    // terminator
        [92, 102, 124, 255],  // lit body
        [150, 160, 180, 255], // highlight
    ];
    let r = 12.0_f32; // sphere radius (px); cell is 32, centre 16
                      // Light direction (upper-left, toward viewer): normalized.
    let lx = -0.55_f32;
    let ly = -0.55_f32;
    let lz = 0.63_f32;
    for y in 0..CELL_SIZE as i32 {
        for x in 0..CELL_SIZE as i32 {
            let dx = (x - 16) as f32;
            let dy = (y - 16) as f32;
            let d2 = dx * dx + dy * dy;
            if d2 > r * r {
                continue;
            }
            // Surface normal of a sphere at this pixel: (dx, dy, z)/r.
            let nz = (r * r - d2).max(0.0).sqrt();
            let ndotl = ((dx * lx + dy * ly + nz * lz) / r).clamp(-1.0, 1.0);
            // Map [-1,1] lambert → one of the 4 tones.
            let t = ((ndotl + 1.0) * 0.5 * tones.len() as f32) as usize;
            let c = tones[t.min(tones.len() - 1)];
            put_pixel(buf, cx + x as u32, cy + y as u32, c);
        }
    }
}

/// Mid stars — slightly denser, slightly brighter than far stars.
fn draw_parallax_mid_stars(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let stars = [
        (2_u32, 8_u32),
        (6, 20),
        (11, 14),
        (16, 6),
        (21, 23),
        (24, 11),
        (27, 19),
        (3, 27),
        (13, 29),
        (18, 16),
        (25, 4),
        (8, 3),
    ];
    let bright = [255, 250, 240, 240];
    let dim = [200, 215, 230, 180];
    for (i, (x, y)) in stars.iter().enumerate() {
        let c = if i % 3 == 0 { bright } else { dim };
        put_pixel(buf, cx + x, cy + y, c);
        // Cross-shaped sparkle on the brightest stars.
        if i % 3 == 0 {
            put_pixel(buf, cx + x - 1, cy + y, [c[0], c[1], c[2], 100]);
            put_pixel(buf, cx + x + 1, cy + y, [c[0], c[1], c[2], 100]);
            put_pixel(buf, cx + x, cy + y - 1, [c[0], c[1], c[2], 100]);
            put_pixel(buf, cx + x, cy + y + 1, [c[0], c[1], c[2], 100]);
        }
    }
}

/// Foreground dust — drifting bright motes near the camera. Higher alpha,
/// fewer points; tiled in the renderer's foreground parallax band.
fn draw_parallax_foreground_dust(buf: &mut [u8], cell: (u32, u32)) {
    let (cx, cy) = cell_origin(cell);
    let motes = [(5_u32, 18_u32), (14, 8), (23, 22), (28, 14)];
    for (x, y) in motes {
        // Each mote is a 2x2 highlight + a 1px halo.
        fill_rect(buf, cx + x, cy + y, 2, 2, [255, 240, 200, 220]);
        put_pixel(buf, cx + x - 1, cy + y, [200, 180, 140, 90]);
        put_pixel(buf, cx + x + 2, cy + y, [200, 180, 140, 90]);
        put_pixel(buf, cx + x, cy + y - 1, [200, 180, 140, 90]);
        put_pixel(buf, cx + x, cy + y + 2, [200, 180, 140, 90]);
    }
}

/* ---- shared helpers ------------------------------------------------------- */

/// Filled circle of `radius` around `(cx, cy)` in atlas-pixel space.
fn filled_circle(buf: &mut [u8], cx: u32, cy: u32, radius: i32, rgba: [u8; 4]) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x >= 0 && y >= 0 {
                    put_pixel(buf, x as u32, y as u32, rgba);
                }
            }
        }
    }
}

/* ---- tests --------------------------------------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_uvs_at_origin_is_unit_cell() {
        let (mn, mx) = cell_uvs((0, 0));
        assert_eq!(mn, [0.0, 0.0]);
        let expected = CELL_SIZE as f32 / ATLAS_SIZE as f32;
        assert!((mx[0] - expected).abs() < 1e-6);
        assert!((mx[1] - expected).abs() < 1e-6);
    }

    #[test]
    fn cell_uvs_at_corner_is_inside_unit_square() {
        let (mn, mx) = cell_uvs((CELLS_PER_ROW - 1, CELLS_PER_ROW - 1));
        assert!(mn[0] >= 0.0 && mn[1] >= 0.0);
        assert!(mx[0] <= 1.0 && mx[1] <= 1.0);
    }

    #[test]
    fn generate_atlas_sized_correctly() {
        let buf = generate_atlas();
        assert_eq!(buf.len(), (ATLAS_SIZE * ATLAS_SIZE * 4) as usize);
    }

    #[test]
    fn solid_white_cell_is_white() {
        let buf = generate_atlas();
        let (cx, cy) = (SOLID_WHITE.0 * CELL_SIZE, SOLID_WHITE.1 * CELL_SIZE);
        let i = ((cy * ATLAS_SIZE + cx) * 4) as usize;
        assert_eq!(&buf[i..i + 4], &[255, 255, 255, 255]);
    }

    /// Every named cell sits within atlas bounds.
    #[test]
    fn every_cell_inside_atlas_bounds() {
        let cells: &[(u32, u32)] = &[
            BOW_CHEVRON,
            TORPEDO,
            MISSILE,
            GLYPH_BEAM,
            GLYPH_ORDNANCE,
            GLYPH_BROADSIDE,
            GLYPH_DISPLACEMENT,
            GLYPH_CONTROL,
            GLYPH_MOVEMENT,
            GLYPH_DEFENSIVE,
            TELEGRAPH_FIRE,
            TELEGRAPH_LOCK,
            TELEGRAPH_PUSH,
            TELEGRAPH_PULL,
            TELEGRAPH_REORIENT,
            TELEGRAPH_DEPLOY,
            STATUS_HULL_BREACH,
            STATUS_SYSTEMS_OFFLINE,
            STATUS_TARGET_LOCK,
            STATUS_SHIELDS_UP,
            PARALLAX_FAR_STARS,
            PARALLAX_NEBULA,
            PARALLAX_DISTANT_PLANET,
            PARALLAX_MID_STARS,
            PARALLAX_FOREGROUND_DUST,
            SOLID_WHITE,
        ];
        for (c, r) in cells {
            assert!(*c < CELLS_PER_ROW, "col {} out of bounds", c);
            assert!(*r < CELLS_PER_ROW, "row {} out of bounds", r);
        }
    }

    /// No two named cells collide.
    #[test]
    fn named_cells_are_distinct() {
        let cells: &[(u32, u32)] = &[
            BOW_CHEVRON,
            TORPEDO,
            MISSILE,
            GLYPH_BEAM,
            GLYPH_ORDNANCE,
            GLYPH_BROADSIDE,
            GLYPH_DISPLACEMENT,
            GLYPH_CONTROL,
            GLYPH_MOVEMENT,
            GLYPH_DEFENSIVE,
            TELEGRAPH_FIRE,
            TELEGRAPH_LOCK,
            TELEGRAPH_PUSH,
            TELEGRAPH_PULL,
            TELEGRAPH_REORIENT,
            TELEGRAPH_DEPLOY,
            STATUS_HULL_BREACH,
            STATUS_SYSTEMS_OFFLINE,
            STATUS_TARGET_LOCK,
            STATUS_SHIELDS_UP,
            PARALLAX_FAR_STARS,
            PARALLAX_NEBULA,
            PARALLAX_DISTANT_PLANET,
            PARALLAX_MID_STARS,
            PARALLAX_FOREGROUND_DUST,
            SOLID_WHITE,
        ];
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                assert_ne!(a, b, "cell collision at {:?}", a);
            }
        }
    }

    /// Generation completes without panicking; every cell has at least one
    /// non-transparent pixel (catches a forgotten draw_* call).
    #[test]
    fn every_cell_has_some_content() {
        let buf = generate_atlas();
        let cells: &[((u32, u32), &str)] = &[
            (BOW_CHEVRON, "BOW_CHEVRON"),
            (TORPEDO, "TORPEDO"),
            (MISSILE, "MISSILE"),
            (GLYPH_BEAM, "GLYPH_BEAM"),
            (GLYPH_ORDNANCE, "GLYPH_ORDNANCE"),
            (GLYPH_BROADSIDE, "GLYPH_BROADSIDE"),
            (GLYPH_DISPLACEMENT, "GLYPH_DISPLACEMENT"),
            (GLYPH_CONTROL, "GLYPH_CONTROL"),
            (GLYPH_MOVEMENT, "GLYPH_MOVEMENT"),
            (GLYPH_DEFENSIVE, "GLYPH_DEFENSIVE"),
            (TELEGRAPH_FIRE, "TELEGRAPH_FIRE"),
            (TELEGRAPH_LOCK, "TELEGRAPH_LOCK"),
            (TELEGRAPH_PUSH, "TELEGRAPH_PUSH"),
            (TELEGRAPH_PULL, "TELEGRAPH_PULL"),
            (TELEGRAPH_REORIENT, "TELEGRAPH_REORIENT"),
            (TELEGRAPH_DEPLOY, "TELEGRAPH_DEPLOY"),
            (STATUS_HULL_BREACH, "STATUS_HULL_BREACH"),
            (STATUS_SYSTEMS_OFFLINE, "STATUS_SYSTEMS_OFFLINE"),
            (STATUS_TARGET_LOCK, "STATUS_TARGET_LOCK"),
            (STATUS_SHIELDS_UP, "STATUS_SHIELDS_UP"),
            (PARALLAX_FAR_STARS, "PARALLAX_FAR_STARS"),
            (PARALLAX_NEBULA, "PARALLAX_NEBULA"),
            (PARALLAX_DISTANT_PLANET, "PARALLAX_DISTANT_PLANET"),
            (PARALLAX_MID_STARS, "PARALLAX_MID_STARS"),
            (PARALLAX_FOREGROUND_DUST, "PARALLAX_FOREGROUND_DUST"),
            (SOLID_WHITE, "SOLID_WHITE"),
        ];
        for ((c, r), name) in cells {
            let cx = c * CELL_SIZE;
            let cy = r * CELL_SIZE;
            let mut found = false;
            'outer: for dy in 0..CELL_SIZE {
                for dx in 0..CELL_SIZE {
                    let i = (((cy + dy) * ATLAS_SIZE + cx + dx) * 4 + 3) as usize;
                    if buf[i] > 0 {
                        found = true;
                        break 'outer;
                    }
                }
            }
            assert!(found, "cell {} ({:?}) has no opaque pixels", name, (c, r));
        }
    }
}
