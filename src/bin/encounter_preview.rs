//! Encounter preview — renders the REAL v2 5×4 formation (the campaign's first
//! encounter layout) through the production `hud::compose_scene_2d` path, so
//! Bruce can see the actual board, not mock data.
//!
//! Run: `cargo run --bin encounter_preview --features render,runtime`
//!
//! ## What it shows
//!
//! A real `types::Board` populated to match content C4's spawn layout (commit
//! 1314b17): the player at front-centre `(2,3)` facing `Bow(N)` (into the board),
//! and enemies fanned centre-out across the back row `(2,0),(1,0),(3,0),(0,0)`
//! all facing `Bow(S)` (toward the player). It is drawn by the SAME
//! [`broadside_engine::hud::compose_scene_2d`] the game uses — real `ship.pos` /
//! `ship.facing` / `shield_profile`, the D2 perspective projector, bow-direction
//! arrows on `Facing::forward_axis()`, and gold shield pips — over the
//! parallax-style space backdrop.
//!
//! ## Why synthetic-but-faithful rather than `runs::build_encounter_board`
//!
//! `build_encounter_board` needs a live `EncounterDef` + `Run` + a catalog-backed
//! spawn→ship closure (run state machine + `DemoContent`), which is heavy to
//! stand up in a preview bin. The lead's fallback is "a synthetic board matching
//! C4's exact layout" — which is what this builds, keyed off the SAME public
//! placement helpers `runs::player_start_pos` / `player_spawn_facing` /
//! `enemy_spawn_facing` so it tracks any layout change in those. The formation is
//! identical to what `build_encounter_board` produces for a 3–4-enemy encounter.
//!
//! ## Relationship to `grid_preview`
//!
//! `grid_preview` exercises the projector + telegraph channels with MOCK data
//! (so it can show threats / the move-arrow before the resolver populates them).
//! This bin is the REAL-data counterpart: a true `types::Board` through
//! `compose_scene_2d`. Bin-wiring of `compose_scene_2d` into `broadside.rs`
//! proper stays held; this preview is the interim "see the real board" path.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::atlas;
use broadside_engine::background::{visible_layers, ParallaxParams};
use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::{DrawCommand, Gfx, PolygonInstance, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::grid::{Facing, Pos, COLS};
use broadside_engine::hud::compose_scene_2d;
use broadside_engine::projector::ProjectorConfig;
use broadside_engine::runs::{enemy_spawn_facing, player_spawn_facing, player_start_pos};
use broadside_engine::types::{Board, EventBus, Faction, Orientation, Ship};

/// Build a `types::Ship` at `pos`/`facing` for `faction`. Mirrors the field set
/// `runs::build_encounter_board` produces (legacy 1-D `cell`/`orientation` kept
/// consistent with the 2-D `pos`/`facing` for the transition window), with a
/// `default_shield_profile` so shield pips render realistically.
fn make_ship(id: &str, faction: Faction, pos: Pos, facing: Facing) -> Ship {
    // Keep the legacy 1-D orientation roughly consistent with the 2-D facing for
    // the EXPAND window: bow-toward-N (into board) reads as Fore, S as Aft.
    let orientation = match facing {
        Facing::Bow(broadside_engine::grid::Dir4::N) => Orientation::BowOn {
            bow: broadside_engine::types::LaneEnd::Fore,
        },
        Facing::Bow(broadside_engine::grid::Dir4::S) => Orientation::BowOn {
            bow: broadside_engine::types::LaneEnd::Aft,
        },
        _ => Orientation::Broadside,
    };
    Ship {
        id: id.to_string(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation,
        facing,
        hull: 5,
        max_hull: 5,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: std::collections::HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// Centre-out back-row column order (`2,1,3,0,4` for `COLS == 5`), matching
/// `runs::back_row_column_order` (private there, replicated here — it's the
/// documented centre-out fan).
fn back_row_columns() -> Vec<usize> {
    let mid = COLS / 2;
    let mut cols = vec![mid];
    let mut k = 1usize;
    while cols.len() < COLS {
        if mid >= k {
            cols.push(mid - k);
        }
        if mid + k < COLS {
            cols.push(mid + k);
        }
        k += 1;
    }
    cols
}

/// A real `types::Board` matching C4's first-encounter formation: player at
/// `player_start_pos()` facing `player_spawn_facing()`, and `enemy_count`
/// enemies fanned centre-out across the back row facing `enemy_spawn_facing()`.
/// Built through the SAME public placement helpers `build_encounter_board` uses,
/// so it stays faithful if those change.
fn build_demo_board(enemy_count: usize) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    let hazards: Vec<Vec<broadside_engine::types::Hazard>> = (0..broadside_engine::grid::CELLS)
        .map(|_| Vec::new())
        .collect();

    // Player: front-centre, bow into the board. Give it a couple of bow shield
    // charges so the gold pips read on the strong (forward) face.
    let p_pos = player_start_pos();
    let mut player = make_ship("player", Faction::Player, p_pos, player_spawn_facing());
    player.shield_profile.bow.charge = 2;
    player.shield_profile.port.charge = 1;
    player.shield_profile.starboard.charge = 1;
    cells[p_pos.to_index()] = Some(player);

    // Enemies across the back row, centre-out, bow toward the player.
    let cols = back_row_columns();
    for (i, &col) in cols.iter().take(enemy_count).enumerate() {
        let pos = Pos::new(col, 0);
        if pos == p_pos {
            continue;
        }
        let ship = make_ship(
            &format!("enemy-{i}"),
            Faction::Enemy,
            pos,
            enemy_spawn_facing(),
        );
        cells[pos.to_index()] = Some(ship);
    }

    Board {
        size: COLS,
        cells,
        ordnance: Vec::new(),
        hazards,
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/// The parallax backdrop as depth-tinted band quads, reusing the D5 slot math
/// ([`visible_layers`]) — identical technique to `grid_preview`, so the preview
/// shows the parallax look without the GPU `Background` resource.
fn push_backdrop(out: &mut Vec<DrawCommand>, focus: f32, player_pos: f32) {
    let p = ParallaxParams::default();
    let frame_w = VIRTUAL_W as f32;
    let frame_h = VIRTUAL_H as f32;
    let canvas_w = frame_w * 2.0;
    let canvas_h = frame_h;
    let cx = frame_w * 0.5;
    let cy = frame_h * 0.5;

    for d in visible_layers(focus, player_pos, 20, &p) {
        let half_w = canvas_w * d.scale * 0.5;
        let half_h = canvas_h * d.scale * 0.5;
        let lx = cx - d.shift_px;
        let (left, right) = (lx - half_w, lx + half_w);
        let (top, bottom) = (cy - half_h, cy + half_h);
        let t = (d.s / (p.visible - 1.0)).clamp(0.0, 1.0);
        let near = [0.227_f32, 0.275, 0.376];
        let far = [0.039_f32, 0.055, 0.110];
        let rgb = [
            near[0] + (far[0] - near[0]) * t,
            near[1] + (far[1] - near[1]) * t,
            near[2] + (far[2] - near[2]) * t,
        ];
        out.push(DrawCommand::Polygon(PolygonInstance::flat(
            [[left, top], [right, top], [right, bottom], [left, bottom]],
            [rgb[0], rgb[1], rgb[2], d.alpha],
            atlas::cell_uvs(atlas::SOLID_WHITE),
        )));
    }
}

/// Compose the full frame: parallax backdrop, then the real board via the
/// production `compose_scene_2d`.
fn compose(board: &Board, cfg: &ProjectorConfig, focus: f32, player_pos: f32) -> Vec<DrawCommand> {
    let mut out = Vec::with_capacity(512);
    push_backdrop(&mut out, focus, player_pos);
    out.extend(compose_scene_2d(board, cfg));
    out
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    cfg: ProjectorConfig,
    board: Board,
    focus: f32,
    drift: bool,
    last_frame: Instant,
}

impl Default for App {
    fn default() -> Self {
        Self {
            window: None,
            gfx: None,
            cfg: ProjectorConfig::default(),
            // 3 enemies (centre-out: (2,0),(1,0),(3,0)) — the lead's reference
            // formation; bump to fan wider.
            board: build_demo_board(3),
            focus: 0.0,
            drift: true,
            last_frame: Instant::now(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Broadside encounter preview (real board) — Space pause drift · 1-5 enemies · Esc quit")
            .with_inner_size(winit::dpi::LogicalSize::new(
                f64::from(VIRTUAL_W * 3),
                f64::from(VIRTUAL_H * 3),
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
                // 1-5 rebuild the board with that many enemies (fan the back row).
                KeyCode::Digit1 => self.board = build_demo_board(1),
                KeyCode::Digit2 => self.board = build_demo_board(2),
                KeyCode::Digit3 => self.board = build_demo_board(3),
                KeyCode::Digit4 => self.board = build_demo_board(4),
                KeyCode::Digit5 => self.board = build_demo_board(5),
                _ => {}
            },
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;
                if self.drift {
                    self.focus = (self.focus + dt * 0.4).rem_euclid(20.0);
                }
                // Backdrop horizontal parallax tracks the player's column.
                let player_col = player_start_pos().col as f32;
                let commands = compose(&self.board, &self.cfg, self.focus, player_col);
                if let Some(gfx) = self.gfx.as_mut() {
                    match gfx.render(&commands) {
                        Ok(()) => {}
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            gfx.reconfigure();
                        }
                        Err(e) => eprintln!("[encounter_preview] surface error: {e:?}"),
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
