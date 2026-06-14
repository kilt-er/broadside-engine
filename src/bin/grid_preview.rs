//! Grid preview — a standalone visual harness for the v2 5×4 board geometry.
//!
//! Run: `cargo run --bin grid_preview --features render,runtime`
//!
//! This renders the **empty 5×4 grid** through the D2 perspective projector
//! ([`broadside_engine::projector`]) over the parallax-style space backdrop, plus
//! a set of **mock ships** and **mock threats** drawn with the D3 / D4 draw logic
//! — so Bruce can SEE the new board (recession, the column fan, ship orientation
//! arrows, the red move-threat fill, the gold shield pips) before the combat
//! resolver's 2D types land.
//!
//! ## Why it exists / what it is NOT
//!
//! It is a SEPARATE bin (not `src/bin/broadside.rs`, which is mid-migration in
//! A3's blast radius) so it can't collide with the type rewrite. It depends only
//! on the renderer modules that are already green over the frozen `grid.rs`:
//! [`broadside_engine::projector`], [`broadside_engine::gfx`],
//! [`broadside_engine::atlas`], [`broadside_engine::background`], and
//! [`broadside_engine::grid`]. It does **not** touch `types` / `resolve` /
//! `hud` (all mid-A3).
//!
//! The mock board here ([`MockShip`] / [`MockThreat`]) stands in for the real
//! `Ship.pos` / `Ship.facing` / `Board.threats` that arrive in A3.1. The draw
//! functions ([`ship_draw_commands`], [`threat_draw_commands`]) are written to
//! be **lifted into `hud.rs` essentially verbatim** once A3.1 lands — swap
//! `MockShip` → `types::Ship`, `grid::Facing` → `ship.facing`, `MockThreat` →
//! the real `Board.threats` entries. That is the D3 / D4 staging this harness
//! delivers.
//!
//! ## Coordinate conventions (shared with the whole renderer)
//!
//! 480×270 virtual-pixel frame, origin top-left, y-down; grid `row 0` is the
//! far/back row (small, high), `row ROWS-1` the front row (large, low); `col`
//! increases left→right. See [`broadside_engine::projector`] for the full
//! contract.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::atlas;
use broadside_engine::background::{visible_layers, ParallaxParams};
use broadside_engine::gfx::{DrawCommand, Gfx, PolygonInstance, SpriteInstance, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::grid::{Axis, Dir4, Facing, Pos, COLS, ROWS};
use broadside_engine::projector::{grid_cell_quad, CellQuad, ProjectorConfig};

/* =============================================================================
 * Palette — the analysis-HTML tokens (mirrors hud.rs), 0..1 sRGB.
 * ============================================================================= */

const PLAYER_STROKE: [f32; 4] = [0.329, 0.812, 0.788, 1.0];
const PLAYER_FILL: [f32; 4] = [0.102, 0.165, 0.243, 1.0];
const ENEMY_STROKE: [f32; 4] = [0.878, 0.478, 0.235, 1.0];
const ENEMY_FILL: [f32; 4] = [0.227, 0.122, 0.145, 1.0];

const GRID_LINE: [f32; 4] = [0.20, 0.28, 0.36, 0.85];
const GRID_LINE_PLAYER_ROW: [f32; 4] = [0.33, 0.41, 0.51, 0.95];

/// Red positional-threat fill UNDER a ship (blueprint §defense channel 1 —
/// "move out of this cell"). Semi-transparent so the grid + ship read through.
const THREAT_FILL: [f32; 4] = [0.878, 0.235, 0.235, 0.42];
/// Brighter red for a LETHAL threat (the would-kill flash channel).
const THREAT_FILL_LETHAL: [f32; 4] = [0.961, 0.341, 0.286, 0.62];
/// Gold shield pip ON a ship (blueprint §defense channel 2 — the absorb buffer,
/// one pip per held charge, positioned by zone).
const SHIELD_PIP: [f32; 4] = [1.0, 0.847, 0.420, 1.0];

/// Queued-move telegraph arrow (blueprint §defense: the "queued reposition"
/// channel, distinct from the red positional-threat fill). A cool, deliberate
/// hue so a queued move — including a long-range enemy's INTENDED back-off to
/// re-open firing range (content C1) — reads as planned movement, not danger.
const MOVE_ARROW: [f32; 4] = [0.475, 0.675, 0.945, 0.92];

/* =============================================================================
 * Mock board — stands in for the A3.1 `Ship` / `Board.threats`. Local to this
 * harness; the real draw path reads `types::Ship` + `Board.threats`.
 * ============================================================================= */

#[derive(Clone, Copy, PartialEq, Eq)]
enum Faction {
    Player,
    Enemy,
}

/// The four hull zones, mirroring `types::HullZone` so the lift is a rename.
#[derive(Clone, Copy)]
enum Zone {
    Bow,
    Stern,
    Port,
    Starboard,
}

/// A mock ship: just the fields D3 reads (position, facing, faction) plus a
/// per-zone shield-charge count for the D4 pip channel.
#[derive(Clone, Copy)]
struct MockShip {
    pos: Pos,
    facing: Facing,
    faction: Faction,
    /// Held shield charges per zone `[bow, stern, port, starboard]` — drives the
    /// gold pip count. Stands in for `ship.shield_profile.face(zone).charge`.
    charges: [i32; 4],
    /// The cell this ship has QUEUED a move to, if any — drives the move-arrow
    /// telegraph channel. `None` = no queued move. Stands in for reading the
    /// ship's queued move action (incl. a long-range enemy's intended BACK-OFF
    /// to re-open firing range, content C1 — rendered like any queued move).
    queued_move: Option<Pos>,
}

/// A mock threatened cell: a board position + whether the queued hit is lethal.
/// Stands in for a `Board.threats` entry (R8 populates the real one from
/// `resolve_targeting` against each enemy's queued action).
#[derive(Clone, Copy)]
struct MockThreat {
    pos: Pos,
    lethal: bool,
}

/// A small demo scene exercising every readability cue: a player frigate at the
/// front-center, three enemies at the back rows in different stances, threatened
/// cells under the player's neighbours, and shield charges on the player.
fn demo_board() -> (Vec<MockShip>, Vec<MockThreat>) {
    let ships = vec![
        // Player: front row, center column, bow pointing N (up the board, toward
        // the enemies) — the natural "advancing" stance.
        MockShip {
            pos: Pos::new(2, ROWS - 1),
            facing: Facing::Bow(Dir4::N),
            faction: Faction::Player,
            charges: [2, 0, 1, 1], // bow-heavy: presenting the strong face forward
            queued_move: None,
        },
        // Enemy 1: back row, left, bow pointing S (down the board, AT the player).
        MockShip {
            pos: Pos::new(0, 0),
            facing: Facing::Bow(Dir4::S),
            faction: Faction::Enemy,
            charges: [0, 0, 0, 0],
            queued_move: None,
        },
        // Enemy 2: back row, right, broadside along the E-W axis (flanks face
        // N/S — i.e. presenting a broadside DOWN the board at the player).
        MockShip {
            pos: Pos::new(4, 0),
            facing: Facing::Broadside(Axis::EastWest),
            faction: Faction::Enemy,
            charges: [0, 0, 0, 0],
            queued_move: None,
        },
        // Enemy 3: second-from-back, center, bow pointing W (turned sideways) —
        // shows a non-toward-player stance so the arrow direction is unmistakable.
        // It has CLOSED past its optimal range and queues a BACK-OFF (move toward
        // row 0, away from the player) to re-open firing distance — the visible
        // payoff of the over-extension mechanic (content C1). Rendered as a normal
        // queued move-arrow.
        MockShip {
            pos: Pos::new(2, 1),
            facing: Facing::Bow(Dir4::W),
            faction: Faction::Enemy,
            charges: [0, 0, 0, 0],
            queued_move: Some(Pos::new(2, 0)), // back off one row, away from player
        },
    ];
    // Threats: the two cells flanking the player (so the "dodge" read is obvious)
    // plus a lethal one directly ahead.
    let threats = vec![
        MockThreat { pos: Pos::new(1, ROWS - 1), lethal: false },
        MockThreat { pos: Pos::new(3, ROWS - 1), lethal: false },
        MockThreat { pos: Pos::new(2, ROWS - 2), lethal: true },
    ];
    (ships, threats)
}

/* =============================================================================
 * Draw helpers — emit DrawCommands in 480×270 frame space. These are the D3/D4
 * draw logic; they take the projector's CellQuad + the mock board and are the
 * code that lifts into hud.rs.
 * ============================================================================= */

/// A thin line segment as a rotated 1px-thick sprite quad (mirrors hud::push_line).
fn line(out: &mut Vec<DrawCommand>, a: [f32; 2], b: [f32; 2], thickness: f32, color: [f32; 4]) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
        half_size: [len * 0.5, thickness * 0.5],
        color,
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        rotation_rad: dy.atan2(dx),
        _pad: [0.0; 3],
    }));
}

/// A flat-filled quad over four explicit corners (SOLID_WHITE × tint).
fn fill_quad(out: &mut Vec<DrawCommand>, corners: [[f32; 2]; 4], color: [f32; 4]) {
    out.push(DrawCommand::Polygon(PolygonInstance::flat(
        corners,
        color,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// Outline a cell quad's four edges (the grid wireframe).
fn outline_cell(out: &mut Vec<DrawCommand>, q: &CellQuad, color: [f32; 4]) {
    let c = q.corners;
    line(out, c[0], c[1], 1.0, color); // far edge (top)
    line(out, c[1], c[2], 1.0, color); // right edge
    line(out, c[2], c[3], 1.0, color); // near edge (bottom)
    line(out, c[3], c[0], 1.0, color); // left edge
}

/// The parallax backdrop, drawn as depth-tinted band quads using the EXACT D5
/// slot math ([`visible_layers`]). This previews the parallax look (scale / slide
/// / fade) without needing the GPU `Background` resource or any `gfx.rs` change:
/// each visible layer becomes one flat polygon the size of the scaled 960×270
/// canvas, tinted by depth and faded by the layer's edge alpha. The real game
/// composites painted PNGs the same way (background.rs::draw).
fn push_backdrop(out: &mut Vec<DrawCommand>, focus: f32, player_pos: f32) {
    let p = ParallaxParams::default();
    let frame_w = VIRTUAL_W as f32;
    let frame_h = VIRTUAL_H as f32;
    let canvas_w = frame_w * 2.0; // spec §2: canvas is 2× frame wide
    let canvas_h = frame_h;
    let cx = frame_w * 0.5;
    let cy = frame_h * 0.5;
    let count = 20usize;

    for d in visible_layers(focus, player_pos, count, &p) {
        let half_w = canvas_w * d.scale * 0.5;
        let half_h = canvas_h * d.scale * 0.5;
        let lx = cx - d.shift_px; // layers slide opposite the player
        let (left, right) = (lx - half_w, lx + half_w);
        let (top, bottom) = (cy - half_h, cy + half_h);
        // Depth tint: near (s small) = cool slate, far (s large) = deep void.
        let t = (d.s / (p.visible - 1.0)).clamp(0.0, 1.0);
        let near = [0.227_f32, 0.275, 0.376];
        let far = [0.039_f32, 0.055, 0.110];
        let rgb = [
            near[0] + (far[0] - near[0]) * t,
            near[1] + (far[1] - near[1]) * t,
            near[2] + (far[2] - near[2]) * t,
        ];
        fill_quad(
            out,
            [[left, top], [right, top], [right, bottom], [left, bottom]],
            [rgb[0], rgb[1], rgb[2], d.alpha],
        );
    }
}

/// Draw the empty grid: every cell's wireframe trapezoid via the projector. The
/// player's front row is drawn brighter so "near = where you are" reads at once.
fn push_grid(out: &mut Vec<DrawCommand>, cfg: &ProjectorConfig) {
    for row in 0..ROWS {
        for col in 0..COLS {
            let q = grid_cell_quad(Pos::new(col, row), cfg);
            let color = if row == ROWS - 1 {
                GRID_LINE_PLAYER_ROW
            } else {
                GRID_LINE
            };
            outline_cell(out, &q, color);
        }
    }
}

/// D4 channel 1: red positional-threat fill UNDER a threatened cell. Drawn as the
/// cell's full trapezoid so it reads as "this whole cell is dangerous — move."
fn threat_draw_commands(out: &mut Vec<DrawCommand>, threats: &[MockThreat], cfg: &ProjectorConfig) {
    for t in threats {
        let q = grid_cell_quad(t.pos, cfg);
        let color = if t.lethal { THREAT_FILL_LETHAL } else { THREAT_FILL };
        fill_quad(out, q.corners, color);
    }
}

/// D4 channel 3 (the "queued reposition" telegraph): draw a move-arrow from a
/// ship's current cell toward the cell it has QUEUED a move to. A line in the
/// cool [`MOVE_ARROW`] hue with a chevron arrowhead at the destination, so the
/// queued move reads as deliberate repositioning — distinct from the red
/// positional-threat fill. A long-range enemy's INTENDED back-off to re-open
/// firing range (content C1) is exactly this: a normal queued move rendered the
/// same way (the lead's D4 note). No-op when the ship has no queued move.
///
/// Lift note: in hud.rs this reads the ship's queued move action (and the
/// projector cell centers) instead of `MockShip::queued_move`.
fn push_move_arrow(out: &mut Vec<DrawCommand>, ship: &MockShip, cfg: &ProjectorConfig) {
    let Some(dest) = ship.queued_move else {
        return;
    };
    let from = grid_cell_quad(ship.pos, cfg).center;
    let to = grid_cell_quad(dest, cfg).center;
    // Shorten the arrow slightly at both ends so it sits between the cell
    // centers without poking through the ship hulls.
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (ux, uy) = (dx / len, dy / len);
    let inset = 10.0_f32.min(len * 0.3);
    let a = [from[0] + ux * inset, from[1] + uy * inset];
    let b = [to[0] - ux * inset, to[1] - uy * inset];
    line(out, a, b, 1.5, MOVE_ARROW);
    // Arrowhead chevron at the destination end, rotated to the heading.
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: b,
        half_size: [5.0, 5.0],
        color: MOVE_ARROW,
        uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
        uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
        rotation_rad: uy.atan2(ux),
        _pad: [0.0; 3],
    }));
}

/// The screen-space forward vector (dx, dy) the bow arrow points along, derived
/// from `Facing::forward_axis()` — the SAME forward axis the resolver's
/// `facing_zone` table uses (blueprint: "the renderer's bow-arrow MUST encode
/// the SAME forward axis"). For a `Bow` stance the arrow points the bow `Dir4`;
/// for a `Broadside` stance it points along the hull axis (toward the axis's
/// positive cardinal — purely a render choice for the arrow, since both flanks
/// are symmetric). y is screen-down, so `N` (toward row 0 / up the board) is -y.
fn bow_screen_dir(facing: Facing) -> (f32, f32) {
    // Map a Dir4 to its screen unit vector (y-down): N up, S down, E right, W left.
    let dir4 = match facing {
        Facing::Bow(d) => d,
        // Broadside: point along the axis's positive cardinal for the arrow.
        Facing::Broadside(axis) => match axis {
            Axis::NorthSouth => Dir4::S, // positive row = toward player = down
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

/// D3: draw one ship at its projected cell. A faction-tinted filled hull quad
/// (inset from the cell so the grid + threat fill show around it) + outline +
/// a bow-direction arrow encoding `Facing::forward_axis()`. Broadside stance is
/// drawn as a wider/flatter inset; bow stance as a longer one oriented along the
/// bow axis — but the unmistakable cue is the arrow.
///
/// This is the D3 logic that lifts into `hud.rs`: swap `MockShip` for
/// `types::Ship` and read `ship.pos` / `ship.facing`. The hull fill here is a
/// flat placeholder; the real path emits the loft/silhouette draw scaled by
/// `q.depth_scale`.
fn ship_draw_commands(out: &mut Vec<DrawCommand>, ship: &MockShip, cfg: &ProjectorConfig) {
    let q = grid_cell_quad(ship.pos, cfg);
    let (fill, stroke) = match ship.faction {
        Faction::Player => (PLAYER_FILL, PLAYER_STROKE),
        Faction::Enemy => (ENEMY_FILL, ENEMY_STROKE),
    };
    let center = q.center;

    // Inset hull box: a fraction of the cell, scaled by depth so far ships are
    // smaller (depth_scale already encodes the recession). The hull is drawn as
    // an axis-aligned quad centred on the cell center; broadside is wider, bow
    // is taller (along the screen), keyed off the forward axis for a coarse
    // stance read under the (always-present) arrow.
    let base = 22.0 * q.depth_scale; // half-extent baseline in px
    let (hx, hy) = match ship.facing {
        Facing::Bow(d) => match d.axis() {
            Axis::NorthSouth => (base * 0.62, base), // long axis vertical on screen
            Axis::EastWest => (base, base * 0.62),   // long axis horizontal
        },
        Facing::Broadside(axis) => match axis {
            Axis::EastWest => (base, base * 0.5), // hull runs E-W: wide + flat
            Axis::NorthSouth => (base * 0.5, base),
        },
    };
    let hull = [
        [center[0] - hx, center[1] - hy],
        [center[0] + hx, center[1] - hy],
        [center[0] + hx, center[1] + hy],
        [center[0] - hx, center[1] + hy],
    ];
    fill_quad(out, hull, fill);
    for i in 0..4 {
        line(out, hull[i], hull[(i + 1) % 4], 1.0, stroke);
    }

    // Bow-direction arrow: a chevron sprite placed just past the hull edge along
    // the forward axis, rotated to point that way. The forward axis is
    // Facing::forward_axis() (encoded via bow_screen_dir) — matching facing_zone.
    let (dx, dy) = bow_screen_dir(ship.facing);
    let reach = base + 8.0;
    let ax = center[0] + dx * reach;
    let ay = center[1] + dy * reach;
    // BOW_CHEVRON points +x at rotation 0; rotate to the forward direction.
    let rot = dy.atan2(dx);
    let arrow_sz = 6.0 * q.depth_scale.max(0.5);
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: [ax, ay],
        half_size: [arrow_sz, arrow_sz],
        color: stroke,
        uv_min: atlas::cell_uvs(atlas::BOW_CHEVRON).0,
        uv_max: atlas::cell_uvs(atlas::BOW_CHEVRON).1,
        rotation_rad: rot,
        _pad: [0.0; 3],
    }));

    // D4 channel 2: gold shield pips ON the ship, one per held charge, positioned
    // by zone. Bow/stern pips sit fore/aft along the forward axis; port/starboard
    // perpendicular to it. (Lift note: read counts from shield_profile.face(zone)
    // and place against the corrected facing_zone mapping.)
    push_shield_pips(out, ship, center, base, (dx, dy));
}

/// Gold pips per zone (D4 channel 2). `fwd` is the screen forward unit vector;
/// the perpendicular is `(-fwd.y, fwd.x)`. Pips stack outward from each hull face.
fn push_shield_pips(
    out: &mut Vec<DrawCommand>,
    ship: &MockShip,
    center: [f32; 2],
    base: f32,
    fwd: (f32, f32),
) {
    let (fx, fy) = fwd;
    let (px, py) = (-fy, fx); // perpendicular (starboard-ish)
    let pip = 1.8;
    let edge = base + 3.0;
    let step = pip * 2.0 + 1.0;
    // (zone index, outward unit vector toward that face)
    let faces = [
        (Zone::Bow, (fx, fy)),
        (Zone::Stern, (-fx, -fy)),
        (Zone::Starboard, (px, py)),
        (Zone::Port, (-px, -py)),
    ];
    for (zone, (ox, oy)) in faces {
        let n = ship.charges[zone as usize];
        // Stack pips ALONG the face (perpendicular to the outward vector) so
        // multiple charges read as a row, not a pile.
        let (sx, sy) = (-oy, ox);
        let base_x = center[0] + ox * edge;
        let base_y = center[1] + oy * edge;
        let start = -(n as f32 - 1.0) * 0.5;
        for i in 0..n {
            let k = start + i as f32;
            out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
                [base_x + sx * step * k, base_y + sy * step * k],
                [pip, pip],
                SHIELD_PIP,
                atlas::cell_uvs(atlas::SOLID_WHITE),
            )));
        }
    }
}

/// Build the whole frame's draw list, back to front (matches the blueprint render
/// order): backdrop → grid → threat fill → queued move-arrows → ships (with bow
/// arrows + shield pips).
fn compose(
    cfg: &ProjectorConfig,
    ships: &[MockShip],
    threats: &[MockThreat],
    focus: f32,
    player_pos: f32,
) -> Vec<DrawCommand> {
    let mut out = Vec::with_capacity(512);
    push_backdrop(&mut out, focus, player_pos);
    push_grid(&mut out, cfg);
    threat_draw_commands(&mut out, threats, cfg);
    // Queued move-arrows under the ships (so a ship hull sits on top of its
    // arrow's tail) — incl. enemy back-off repositioning.
    for s in ships {
        push_move_arrow(&mut out, s, cfg);
    }
    // Draw ships far row first (row 0) → near last, so nearer ships overlap
    // farther ones correctly.
    let mut sorted: Vec<&MockShip> = ships.iter().collect();
    sorted.sort_by_key(|s| s.pos.row); // ascending row = far → near
    for s in sorted {
        ship_draw_commands(&mut out, s, cfg);
    }
    out
}

/* =============================================================================
 * winit / Gfx harness (mirrors loft_poc's ApplicationHandler scaffold).
 * ============================================================================= */

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    cfg: ProjectorConfig,
    ships: Vec<MockShip>,
    threats: Vec<MockThreat>,
    /// Backdrop depth cursor, slowly drifting so the parallax recession is
    /// visible; SPACE pauses it.
    focus: f32,
    drift: bool,
    /// Player column 0..4 (drives the backdrop horizontal parallax); ←/→ move it.
    player_col: f32,
    last_frame: Instant,
}

impl Default for App {
    fn default() -> Self {
        let (ships, threats) = demo_board();
        Self {
            window: None,
            gfx: None,
            cfg: ProjectorConfig::default(),
            ships,
            threats,
            focus: 0.0,
            drift: true,
            player_col: 2.0,
            last_frame: Instant::now(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Broadside grid preview — ←/→ parallax · Space pause drift · Esc quit")
            .with_inner_size(winit::dpi::LogicalSize::new(
                (VIRTUAL_W * 3) as f64,
                (VIRTUAL_H * 3) as f64,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        let gfx = pollster::block_on(Gfx::new(window.clone()));
        self.window = Some(window);
        self.gfx = Some(gfx);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match code {
                KeyCode::Escape => event_loop.exit(),
                KeyCode::Space => self.drift = !self.drift,
                KeyCode::ArrowLeft => self.player_col = (self.player_col - 1.0).max(0.0),
                KeyCode::ArrowRight => {
                    self.player_col = (self.player_col + 1.0).min((COLS - 1) as f32)
                }
                _ => {}
            },
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;
                if self.drift {
                    // Slow loop through the depth queue so the recession reads.
                    self.focus = (self.focus + dt * 0.4).rem_euclid(20.0);
                }
                let commands = compose(
                    &self.cfg,
                    &self.ships,
                    &self.threats,
                    self.focus,
                    self.player_col,
                );
                if let Some(gfx) = self.gfx.as_mut() {
                    match gfx.render(&commands) {
                        Ok(()) => {}
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            gfx.reconfigure()
                        }
                        Err(e) => eprintln!("[grid_preview] surface error: {e:?}"),
                    }
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run");
}
