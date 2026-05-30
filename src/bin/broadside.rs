//! Runnable demo: opens a window and renders an interactive Broadside scene.
//!
//! Initial state mirrors `_drive_pull/ship-view-decision/render-example.ts`
//! (7-cell lane, player at cell 0, four enemies). Keyboard input is routed
//! through [`broadside_engine::input`] so the engine library can run the
//! same key→intent→queue→resolve flow without winit.
//!
//! ## Controls (canonical map in `input::key_to_intent`)
//!
//! | Key | Intent | Effect |
//! |-----|--------|--------|
//! | `1` / `2` / `3` | `QueueAction` from `mounts[0/1/2]` | Append weapon action id to `player.queue` |
//! | `←` | `MoveLeft` | Queue synthetic `__move_left` |
//! | `→` | `MoveRight` | Queue synthetic `__move_right` |
//! | `Tab` | `ReorientFlip` | Queue synthetic `__reorient_flip` |
//! | `V` | `Vent` | Queue synthetic `__vent` |
//! | `R` / `Space` | `CommitTurn` | Run `resolve_round`; re-renders next frame |
//! | `Enter` | `Restart` | Reset the board to its initial state |
//! | `Esc` | exit | Close the window |
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin broadside --features render,runtime
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::{Gfx, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::hud;
use broadside_engine::input::{
    intent_to_action_id, key_to_intent, DemoContent, Intent, Key,
};
use broadside_engine::perspective::{LaneGeometry, Point2, DEFAULT_LANE, FRIGATE_DIMS};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::types::{
    Arc as TArc, Board, EventBus, Faction, LaneEnd, Mount, Orientation, ShieldFace,
    ShieldProfile, Ship,
};

/* =============================================================================
 * winit::KeyCode -> input::Key translation. Lives in the bin so the lib
 * never imports winit. One arm per binding the tutorial advertises;
 * everything else returns None and the key is ignored.
 * ========================================================================== */

fn keycode_to_key(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::ArrowLeft => Key::Left,
        KeyCode::ArrowRight => Key::Right,
        KeyCode::Tab => Key::Tab,
        KeyCode::KeyV => Key::V,
        KeyCode::Digit1 => Key::D1,
        KeyCode::Digit2 => Key::D2,
        KeyCode::Digit3 => Key::D3,
        KeyCode::KeyR => Key::R,
        KeyCode::Space => Key::Space,
        KeyCode::Enter => Key::Enter,
        _ => return None,
    })
}

/* =============================================================================
 * Applying an Intent to the board.
 *
 * Mutates `board` in place; returns true if the visible state changed (the
 * renderer requests a redraw on true). `CommitTurn` invokes the resolver;
 * `Restart` rebuilds the board via the supplied factory; everything else
 * gets translated to an action id via `input::intent_to_action_id` and
 * appended to the player's queue.
 * ========================================================================== */

/// Apply an [`Intent`] to the board. `initial_board` produces a fresh
/// starting state for `Restart`. Returns true if the board changed.
pub fn apply_intent(
    intent: Intent,
    board: &mut Board,
    content: &dyn Content,
    initial_board: &dyn Fn() -> Board,
) -> bool {
    match intent {
        Intent::CommitTurn => {
            resolve_round(board, content);
            true
        }
        Intent::Restart => {
            *board = initial_board();
            true
        }
        _ => {
            // QueueAction / MoveLeft / MoveRight / ReorientFlip / Vent
            // all map to a single action id that gets appended to the
            // player's queue.
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            append_to_player_queue(board, id.to_string())
        }
    }
}

fn append_to_player_queue(board: &mut Board, action_id: String) -> bool {
    let Some(player_cell) = board
        .cells
        .iter()
        .position(|c| matches!(c, Some(s) if s.faction == Faction::Player))
    else {
        return false;
    };
    if let Some(ship) = board.cells[player_cell].as_mut() {
        ship.queue.push(action_id);
        true
    } else {
        false
    }
}

/* =============================================================================
 * Initial scene + lane geometry.
 * ========================================================================== */

/// Build the demo's LaneGeometry from `DEFAULT_LANE`, scaled to the engine
/// virtual canvas and inset on the near side so a cell-0 ship at scaleNear
/// doesn't clip past the left edge.
fn demo_lane() -> LaneGeometry {
    let base = DEFAULT_LANE.scaled((VIRTUAL_W as f32) / 660.0);
    let half_len_near = FRIGATE_DIMS.length / 2.0;
    let target_near_x = half_len_near + 8.0;
    let inset = (target_near_x - base.front_start.x).max(0.0);
    LaneGeometry {
        front_start: Point2 { x: base.front_start.x + inset, y: base.front_start.y },
        back_start:  Point2 { x: base.back_start.x  + inset, y: base.back_start.y },
        ..base
    }
}

/// Mirrors the board state hard-coded in `render-example.ts`. Used as both
/// the startup scene and the Restart target.
fn render_example_board() -> Board {
    let size = 7usize;
    let mut cells: Vec<Option<Ship>> = (0..size).map(|_| None).collect();

    cells[0] = Some(player_ship(0));
    // Each enemy gets one Forward pulse_laser so the AI has something to
    // score and queue. Without a mount, decide_enemy_action correctly
    // returns nothing and the enemy looks inert; per bruce's playtest the
    // demo needs visible enemy behaviour to read as a live game.
    cells[2] = Some(enemy_ship("enemy-2", 2, Orientation::Broadside));
    cells[3] = Some(enemy_ship("enemy-3", 3, Orientation::BowOn { bow: LaneEnd::Aft }));
    cells[5] = Some(enemy_ship("enemy-5", 5, Orientation::BowOn { bow: LaneEnd::Fore }));
    cells[6] = Some(enemy_ship("enemy-6", 6, Orientation::BowOn { bow: LaneEnd::Fore }));

    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
    }
}

fn player_ship(cell: usize) -> Ship {
    let mut player = make_ship("player", Faction::Player, cell, Orientation::BowOn { bow: LaneEnd::Fore });
    player.shield_profile = ShieldProfile {
        bow: ShieldFace { armour: 2, charge: 1 },
        ..default_shield_profile()
    };
    player.mounts = vec![
        Mount { id: "m1".into(), arc: TArc::Forward, weapon: "pulse_laser".into() },
        Mount { id: "m2".into(), arc: TArc::Forward, weapon: "torpedo".into() },
    ];
    player
}

/// Enemy frigate: one Forward pulse_laser so the AI can actually queue an
/// action. Without a mount, decide_enemy_action returns nothing and the
/// enemy looks inert.
fn enemy_ship(id: &str, cell: usize, orientation: Orientation) -> Ship {
    let mut e = make_ship(id, Faction::Enemy, cell, orientation);
    e.mounts = vec![Mount {
        id: "m1".into(),
        arc: TArc::Forward,
        weapon: "pulse_laser".into(),
    }];
    e
}

fn make_ship(id: &str, faction: Faction, cell: usize, orientation: Orientation) -> Ship {
    Ship {
        id: id.into(),
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

/* =============================================================================
 * App + event loop.
 * ========================================================================== */

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    board: Board,
    lane: LaneGeometry,
    content: DemoContent,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane: demo_lane(),
            content: DemoContent::default(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Broadside")
            .with_inner_size(winit::dpi::LogicalSize::new(
                VIRTUAL_W as f64,
                VIRTUAL_H as f64,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let gfx = pollster::block_on(Gfx::new(window.clone()));
        self.window = Some(window);
        self.gfx = Some(gfx);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gfx) = self.gfx.as_mut() else { return; };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gfx.resize(size),
            WindowEvent::KeyboardInput { event, .. } => {
                // Edge-trigger: only on key down, ignore repeats.
                if event.state != ElementState::Pressed || event.repeat {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if code == KeyCode::Escape {
                    event_loop.exit();
                    return;
                }
                let Some(key) = keycode_to_key(code) else { return };
                // key_to_intent needs the player ship for digit-key mount
                // resolution; clone the snapshot to keep the borrow short.
                let player_snapshot = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .cloned();
                let Some(player) = player_snapshot else { return };
                let Some(intent) = key_to_intent(key, &player, &self.content) else { return };
                let changed = apply_intent(intent, &mut self.board, &self.content, &render_example_board);
                if changed {
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let instances = hud::compose_scene(&self.board, &self.lane);
                match gfx.render(&instances) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gfx.reconfigure();
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        log::error!("wgpu surface OOM, exiting");
                        event_loop.exit();
                    }
                    Err(e) => log::warn!("surface error: {e:?}"),
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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop");
}

/* =============================================================================
 * Tests — drive `apply_intent` without winit. The pure key→intent mapping is
 * tested in `broadside_engine::input::tests`; here we cover the bin-side
 * `apply_intent` + `keycode_to_key` translation + Restart wiring.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_board() -> Board {
        render_example_board()
    }

    #[test]
    fn keycode_translation_covers_every_binding() {
        assert_eq!(keycode_to_key(KeyCode::ArrowLeft), Some(Key::Left));
        assert_eq!(keycode_to_key(KeyCode::ArrowRight), Some(Key::Right));
        assert_eq!(keycode_to_key(KeyCode::Tab), Some(Key::Tab));
        assert_eq!(keycode_to_key(KeyCode::KeyV), Some(Key::V));
        assert_eq!(keycode_to_key(KeyCode::Digit1), Some(Key::D1));
        assert_eq!(keycode_to_key(KeyCode::Digit2), Some(Key::D2));
        assert_eq!(keycode_to_key(KeyCode::Digit3), Some(Key::D3));
        assert_eq!(keycode_to_key(KeyCode::KeyR), Some(Key::R));
        assert_eq!(keycode_to_key(KeyCode::Space), Some(Key::Space));
        assert_eq!(keycode_to_key(KeyCode::Enter), Some(Key::Enter));
    }

    #[test]
    fn keycode_translation_returns_none_for_unbound() {
        assert_eq!(keycode_to_key(KeyCode::KeyA), None);
        assert_eq!(keycode_to_key(KeyCode::F1), None);
    }

    #[test]
    fn queue_action_intent_appends_to_player_queue() {
        let mut board = fresh_board();
        let content = DemoContent::default();
        apply_intent(
            Intent::QueueAction("pulse_laser".into()),
            &mut board,
            &content,
            &fresh_board,
        );
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(player.queue.last(), Some(&"pulse_laser".to_string()));
    }

    #[test]
    fn move_intent_appends_synthetic_move_id() {
        let mut board = fresh_board();
        let content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &content, &fresh_board);
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(
            player.queue.last(),
            Some(&broadside_engine::input::SYNTHETIC_MOVE_RIGHT.to_string()),
        );
    }

    #[test]
    fn commit_turn_runs_resolve_round() {
        // Queue a thrust-fore and commit. The player ship should move from
        // cell 0 to cell 1 once the resolver runs the queue.
        let mut board = fresh_board();
        let content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &content, &fresh_board);
        let player_at_1 = board.cells[1]
            .as_ref()
            .is_some_and(|s| s.faction == Faction::Player);
        assert!(player_at_1, "player should have moved to cell 1 after thrust+commit");
        assert!(board.cells[0].is_none(), "cell 0 should be empty");
    }

    #[test]
    fn restart_resets_the_board() {
        let mut board = fresh_board();
        let content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &content, &fresh_board);
        apply_intent(Intent::Restart, &mut board, &content, &fresh_board);
        assert!(board.cells[0].as_ref().is_some_and(|s| s.faction == Faction::Player));
        assert!(board.cells[1].is_none());
    }
}
