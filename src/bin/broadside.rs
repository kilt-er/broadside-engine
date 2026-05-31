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
//! | `5` / `6` / `7` | `PlayCard` from `field_kit.cards[0/1/2]` | Decrement charge + queue synthetic card action |
//! | `←` | `MoveLeft` | Queue synthetic `__move_left` |
//! | `→` | `MoveRight` | Queue synthetic `__move_right` |
//! | `Tab` | `ReorientFlip` | Queue synthetic `__reorient_flip` |
//! | `V` | `Vent` | Queue synthetic `__vent` |
//! | `R` / `Space` | `CommitTurn` | Run `resolve_round`; re-renders next frame |
//! | `Enter` | `Restart` | Reset the board to its initial state (also the only key accepted while the win/lose overlay is showing) |
//! | `[` / `]` | rotate camera | Cycle through `[0, 15, 30, 45, 60, 75, 90]°` |
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

use broadside_engine::cards::PlayResult;
use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::{Gfx, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::hud::{self, win_state, WinState};
use broadside_engine::input::{
    intent_to_action_id, key_to_intent, synthetic_card_action_id, DemoContent, Intent, Key,
};
use broadside_engine::perspective::{LaneGeometry, DEFAULT_LANE};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::subsystems::{HEAT_SINK, POINT_BLANK_DOCTRINE};
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
        KeyCode::Digit5 => Key::D5,
        KeyCode::Digit6 => Key::D6,
        KeyCode::Digit7 => Key::D7,
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
///
/// `content` is `&mut` because [`Intent::PlayCard`] needs to validate +
/// decrement card charges via [`Content::try_play_card`].
pub fn apply_intent(
    intent: Intent,
    board: &mut Board,
    content: &mut dyn Content,
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
        Intent::PlayCard(card_id) => {
            // Resolve the player ship id, then validate + decrement
            // charges via Content::try_play_card. On success push the
            // synthetic `__card_<id>` action onto the player's queue;
            // execute_queue handles the BOARD-effect dispatch.
            let Some(player_id) = board
                .cells
                .iter()
                .flatten()
                .find(|s| s.faction == Faction::Player)
                .map(|s| s.id.clone())
            else {
                return false;
            };
            match content.try_play_card(&player_id, &card_id) {
                PlayResult::Played => {
                    append_to_player_queue(board, synthetic_card_action_id(&card_id))
                }
                PlayResult::UnknownCard
                | PlayResult::NotCarried
                | PlayResult::InsufficientCharges => false,
            }
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

/// Demo lane: just `DEFAULT_LANE`. The flat horizontal model has no
/// foreshortening so no per-binary tuning is needed; the lane spans the
/// canvas width centered vertically.
fn demo_lane() -> LaneGeometry {
    DEFAULT_LANE
}

/// Build the demo [`DemoContent`] with the player's Phase 2 loadout
/// pre-installed: HeatSink + Point-Blank Doctrine subsystems and one
/// charge of each placeholder field-kit card (mass_lock / mass_breach /
/// sensor_pulse). Called on startup and on every Restart so card
/// charges are refilled when the player restarts.
fn fresh_content() -> DemoContent {
    let mut c = DemoContent::default();
    c.install_subsystem("player", HEAT_SINK);
    c.install_subsystem("player", POINT_BLANK_DOCTRINE);
    c.grant_placeholder_kit("player");
    c
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

/// Seven fixed camera view angles in degrees, scrubbed via `[` / `]`. The
/// lane stays flat at every angle; ship silhouettes and parallax planes
/// foreshorten with the angle so the scene reads as a camera revolving
/// around the lane. Default index 3 = 45° — the natural isometric middle.
const CAMERA_ANGLE_STEPS_DEG: [f32; 7] = [0.0, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0];
const CAMERA_ANGLE_DEFAULT_INDEX: usize = 3;

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    board: Board,
    lane: LaneGeometry,
    content: DemoContent,
    /// Index into `CAMERA_ANGLE_STEPS_DEG`. Cycled by `[` and `]`.
    camera_angle_idx: usize,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane: demo_lane(),
            content: fresh_content(),
            camera_angle_idx: CAMERA_ANGLE_DEFAULT_INDEX,
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
        let mut gfx = pollster::block_on(Gfx::new(window.clone()));
        // Look for hand-painted ship sprites under `assets/sprites/`.
        // Missing PNGs are silently skipped; the renderer falls back to
        // the procedural silhouette. See docs/SPRITE_SPEC.md.
        let loaded = gfx.try_load_ship_sprites(std::path::Path::new("assets"));
        if loaded > 0 {
            log::info!("loaded {} ship sprite PNG(s) from assets/sprites/", loaded);
        }
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
                // `[` / `]` cycle the camera angle. Handled before the
                // canonical key-to-intent lookup so they remain a renderer-
                // owned binding, not part of the content key map.
                if code == KeyCode::BracketLeft {
                    self.camera_angle_idx = self.camera_angle_idx.saturating_sub(1);
                    log::info!("camera angle: {}°", CAMERA_ANGLE_STEPS_DEG[self.camera_angle_idx]);
                    if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                    return;
                }
                if code == KeyCode::BracketRight {
                    self.camera_angle_idx = (self.camera_angle_idx + 1).min(CAMERA_ANGLE_STEPS_DEG.len() - 1);
                    log::info!("camera angle: {}°", CAMERA_ANGLE_STEPS_DEG[self.camera_angle_idx]);
                    if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                    return;
                }
                let Some(key) = keycode_to_key(code) else { return };
                // When the game has ended (defeat/victory), only Enter is
                // accepted — restart. Every other key is swallowed so the
                // overlay reads as a modal screen.
                if win_state(&self.board) != WinState::Playing && key != Key::Enter {
                    return;
                }
                // key_to_intent needs the player ship for digit-key mount
                // resolution; clone the snapshot to keep the borrow short.
                // After defeat there's no player ship, so synthesize a
                // minimal one purely for the Enter -> Restart routing.
                let player_snapshot = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .cloned();
                let intent_opt = match player_snapshot {
                    Some(player) => key_to_intent(key, &player, &self.content),
                    None if key == Key::Enter => Some(Intent::Restart),
                    None => None,
                };
                let Some(intent) = intent_opt else { return };
                // Restart resets both the board AND the content so card
                // charges + subsystems come back as a fresh game.
                let is_restart = matches!(intent, Intent::Restart);
                let changed = apply_intent(intent, &mut self.board, &mut self.content, &render_example_board);
                if is_restart {
                    self.content = fresh_content();
                }
                if changed {
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Inline the angle read to avoid a &self borrow that
                // would conflict with the &mut self.gfx held above.
                let angle = CAMERA_ANGLE_STEPS_DEG[self.camera_angle_idx].to_radians();
                let instances = hud::compose_scene_with(&self.board, &self.lane, angle, gfx);
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
        assert_eq!(keycode_to_key(KeyCode::Digit5), Some(Key::D5));
        assert_eq!(keycode_to_key(KeyCode::Digit6), Some(Key::D6));
        assert_eq!(keycode_to_key(KeyCode::Digit7), Some(Key::D7));
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
        let mut content = DemoContent::default();
        apply_intent(
            Intent::QueueAction("pulse_laser".into()),
            &mut board,
            &mut content,
            &fresh_board,
        );
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(player.queue.last(), Some(&"pulse_laser".to_string()));
    }

    #[test]
    fn move_intent_appends_synthetic_move_id() {
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(
            player.queue.last(),
            Some(&broadside_engine::input::SYNTHETIC_MOVE_RIGHT.to_string()),
        );
    }

    #[test]
    fn commit_turn_runs_resolve_round() {
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &mut content, &fresh_board);
        let player_at_1 = board.cells[1]
            .as_ref()
            .is_some_and(|s| s.faction == Faction::Player);
        assert!(player_at_1, "player should have moved to cell 1 after thrust+commit");
        assert!(board.cells[0].is_none(), "cell 0 should be empty");
    }

    #[test]
    fn restart_intent_after_defeat_recreates_player() {
        let mut board = fresh_board();
        for slot in board.cells.iter_mut() {
            if matches!(slot, Some(s) if s.faction == Faction::Player) {
                *slot = None;
            }
        }
        assert_eq!(win_state(&board), WinState::Defeat, "precondition");
        let mut content = DemoContent::default();
        apply_intent(Intent::Restart, &mut board, &mut content, &fresh_board);
        assert_eq!(win_state(&board), WinState::Playing);
        assert!(board.cells[0].as_ref().is_some_and(|s| s.faction == Faction::Player));
    }

    #[test]
    fn restart_resets_the_board() {
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::Restart, &mut board, &mut content, &fresh_board);
        assert!(board.cells[0].as_ref().is_some_and(|s| s.faction == Faction::Player));
        assert!(board.cells[1].is_none());
    }

    #[test]
    fn fresh_content_grants_subsystems_and_cards() {
        // The demo loadout: HeatSink + Point-Blank Doctrine installed on
        // the player ship and one charge of each of the 3 placeholder
        // cards in their kit.
        let content = fresh_content();
        let installed = content.installations.for_ship("player");
        assert!(installed.contains(&HEAT_SINK.to_string()));
        assert!(installed.contains(&POINT_BLANK_DOCTRINE.to_string()));
        // card_at(0..3) should resolve to the 3 placeholder cards.
        for i in 0..3 {
            let card = <DemoContent as Content>::card_at(&content, "player", i);
            assert!(card.is_some(), "expected card at kit slot {} after fresh_content", i);
        }
    }

    #[test]
    fn play_card_intent_appends_synthetic_action_and_decrements_charges() {
        let mut board = fresh_board();
        let mut content = fresh_content();
        // First placeholder card is mass_lock (per content's
        // grant_placeholder_kit order).
        let card_id = <DemoContent as Content>::card_at(&content, "player", 0)
            .expect("kit should have a card at slot 0");
        let charges_before = content
            .field_kits
            .for_ship("player")
            .and_then(|k| k.find(&card_id))
            .map(|c| c.charges)
            .unwrap_or(0);
        let changed = apply_intent(
            Intent::PlayCard(card_id.clone()),
            &mut board,
            &mut content,
            &fresh_board,
        );
        assert!(changed, "PlayCard with sufficient charges should mutate board");
        let synth_id = synthetic_card_action_id(&card_id);
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(player.queue.last(), Some(&synth_id), "synthetic card action queued");
        let charges_after = content
            .field_kits
            .for_ship("player")
            .and_then(|k| k.find(&card_id))
            .map(|c| c.charges)
            .unwrap_or(0);
        assert_eq!(charges_after, charges_before - 1, "play should decrement charges");
    }

    #[test]
    fn play_card_intent_rejected_when_card_absent() {
        let mut board = fresh_board();
        let mut content = DemoContent::default(); // no kit granted
        let changed = apply_intent(
            Intent::PlayCard("mass_lock".into()),
            &mut board,
            &mut content,
            &fresh_board,
        );
        assert!(!changed, "PlayCard without inventory should be a no-op");
        let player = board.cells[0].as_ref().unwrap();
        assert!(player.queue.is_empty(), "no synthetic queued on rejected play");
    }
}
