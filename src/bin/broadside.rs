//! Runnable demo: opens a window and renders an interactive Broadside scene.
//!
//! Initial state mirrors `_drive_pull/ship-view-decision/render-example.ts`
//! (7-cell lane, player at cell 0, four enemies). Keyboard input mutates
//! the player ship's queue and commits turns through
//! [`broadside_engine::resolve::resolve_round`].
//!
//! ## Controls (defaults; coordinated with content task #43)
//!
//! | Key | Intent | Effect |
//! |-----|--------|--------|
//! | `1` / `2` / `3` | `QueueAction` from `mounts[0/1/2]` | Append weapon action id to `player.queue` |
//! | `←` | `MoveLeft` | Queue THRUST 1 cell aft |
//! | `→` | `MoveRight` | Queue THRUST 1 cell fore |
//! | `Tab` | `Reorient` | Queue REORIENT flip |
//! | `V` | `Vent` | Queue VENT_HEAT |
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
use broadside_engine::perspective::{LaneGeometry, Point2, DEFAULT_LANE, FRIGATE_DIMS};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc as TArc, Board, Effect, EventBus, Faction, LaneEnd, MovementMode,
    Mount, Orientation, Projectile, RangeBand, ReorientTo, ShieldFace, ShieldProfile, Ship,
    Targeting, TargetingPattern, WeaponArchetype,
};

/* =============================================================================
 * Intents and the pure input mapper.
 *
 * `Intent` is the small closed enum that lives between the raw winit
 * KeyCode and the engine's `Action` / `Effect` types. The mapper is a pure
 * function so the tester can drive it programmatically without winit.
 * ========================================================================== */

/// One player-input intent. Pure data — the binary applies it by mutating
/// the player's queue or calling `resolve_round`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Append this action id to `player.queue`.
    QueueAction(String),
    /// Shorthand: queue a one-cell THRUST move toward `aft` end.
    MoveLeft,
    /// Shorthand: queue a one-cell THRUST move toward `fore` end.
    MoveRight,
    /// Shorthand: queue a REORIENT flip on the player ship.
    Reorient,
    /// Shorthand: queue a heat vent.
    Vent,
    /// Run `resolve_round` on the board and re-render the new state.
    CommitTurn,
    /// Reset the board to its initial state.
    Restart,
}

/// Shortcut action ids the demo binary assumes content provides. Hardcoded
/// here for the Phase-1 controls sprint; content (task #43) will refine.
/// Each id maps to a single canonical action in `demo_content()`.
mod ids {
    pub const THRUST_FORE: &str = "thrust_fore";
    pub const THRUST_AFT: &str = "thrust_aft";
    pub const REORIENT_FLIP: &str = "reorient_flip";
    pub const VENT: &str = "vent";
}

/// Translate one KeyCode to an Intent. Pure — does not consult the board.
/// Selecting a mount weapon (`1`/`2`/`3`) needs the player ship's mounts
/// list, so [`key_to_intent_with_ship`] handles that variant; this version
/// handles every other binding.
pub fn key_to_intent(key: KeyCode) -> Option<Intent> {
    Some(match key {
        KeyCode::ArrowLeft => Intent::MoveLeft,
        KeyCode::ArrowRight => Intent::MoveRight,
        KeyCode::Tab => Intent::Reorient,
        KeyCode::KeyV => Intent::Vent,
        KeyCode::KeyR | KeyCode::Space => Intent::CommitTurn,
        KeyCode::Enter => Intent::Restart,
        _ => return None,
    })
}

/// Full intent mapper that also handles `1`/`2`/`3` mount-weapon selection.
/// Returns `None` if the key is unbound, OR if it picks a mount slot that
/// the ship doesn't have. Pure — `ship` is borrowed read-only.
pub fn key_to_intent_with_ship(key: KeyCode, ship: &Ship) -> Option<Intent> {
    let mount_idx = match key {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        _ => return key_to_intent(key),
    };
    let weapon_id = ship.mounts.get(mount_idx)?.weapon.clone();
    Some(Intent::QueueAction(weapon_id))
}

/* =============================================================================
 * Demo Content — minimal `Content` impl that provides the shortcut actions
 * (thrust_fore, thrust_aft, reorient_flip, vent) plus the player's two
 * mounted weapons. Content (task #43) will replace this with the full
 * catalog-driven `Content` impl when ready.
 * ========================================================================== */

struct DemoContent {
    actions: HashMap<String, Action>,
}

impl Content for DemoContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile {
        let heading = match owner.orientation {
            Orientation::BowOn { bow } => bow,
            Orientation::Broadside => LaneEnd::Fore,
        };
        let spawn_cell = match heading {
            LaneEnd::Fore => owner.cell.saturating_add(1),
            LaneEnd::Aft => owner.cell.saturating_sub(1),
        };
        Projectile {
            id: format!("{}-{}", kind, owner.id),
            kind: kind.into(),
            cell: spawn_cell,
            heading,
            speed: 1,
            hull: 1,
            payload: vec![Effect::DAMAGE { amount: 2, band_falloff: Some(false) }],
            owner_faction: owner.faction,
        }
    }
}

fn demo_content() -> DemoContent {
    let mut actions: HashMap<String, Action> = HashMap::new();
    actions.insert(ids::THRUST_FORE.into(), thrust_action(ids::THRUST_FORE));
    actions.insert(ids::THRUST_AFT.into(), thrust_action(ids::THRUST_AFT));
    actions.insert(ids::REORIENT_FLIP.into(), reorient_flip_action());
    actions.insert(ids::VENT.into(), vent_action());
    actions.insert("pulse_laser".into(), pulse_laser_action());
    actions.insert("torpedo".into(), torpedo_action());
    DemoContent { actions }
}

fn thrust_action(id: &str) -> Action {
    Action {
        id: id.into(),
        name: if id == ids::THRUST_FORE { "Thrust Fore".into() } else { "Thrust Aft".into() },
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DISPLACE_SELF { mode: MovementMode::THRUST, distance: 1 }],
        r#mod: None,
        icon: None,
    }
}

fn reorient_flip_action() -> Action {
    Action {
        id: ids::REORIENT_FLIP.into(),
        name: "Reorient".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::REORIENT { to: ReorientTo::Flip }],
        r#mod: None,
        icon: None,
    }
}

fn vent_action() -> Action {
    Action {
        id: ids::VENT.into(),
        name: "Vent".into(),
        archetype: WeaponArchetype::Defensive,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::VENT_HEAT { amount: 4, recharge_cooldowns: Some(true) }],
        r#mod: None,
        icon: None,
    }
}

fn pulse_laser_action() -> Action {
    Action {
        id: "pulse_laser".into(),
        name: "Pulse Laser".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(TArc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 2, band_falloff: None }],
        r#mod: None,
        icon: None,
    }
}

fn torpedo_action() -> Action {
    Action {
        id: "torpedo".into(),
        name: "Torpedo".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost { heat: 3, cooldown_max: 5, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::ORDNANCE,
            band: vec![RangeBand::Mid, RangeBand::Long],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(TArc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::SPAWN_ORDNANCE { projectile: "torpedo".into() }],
        r#mod: None,
        icon: None,
    }
}

/* =============================================================================
 * Applying an Intent to the board.
 *
 * Pure-ish: mutates `board` directly. Returns `true` if the intent did
 * anything visible (so the renderer can request a redraw). `CommitTurn`
 * invokes the resolver; everything else just edits the player's queue.
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
        Intent::QueueAction(action_id) => append_to_player_queue(board, action_id),
        Intent::MoveLeft => append_to_player_queue(board, ids::THRUST_AFT.into()),
        Intent::MoveRight => append_to_player_queue(board, ids::THRUST_FORE.into()),
        Intent::Reorient => append_to_player_queue(board, ids::REORIENT_FLIP.into()),
        Intent::Vent => append_to_player_queue(board, ids::VENT.into()),
        Intent::CommitTurn => {
            resolve_round(board, content);
            true
        }
        Intent::Restart => {
            *board = initial_board();
            true
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
    cells[2] = Some(make_ship("enemy-2", Faction::Enemy, 2, Orientation::Broadside));
    cells[3] = Some(make_ship("enemy-3", Faction::Enemy, 3, Orientation::BowOn { bow: LaneEnd::Aft }));
    cells[5] = Some(make_ship("enemy-5", Faction::Enemy, 5, Orientation::BowOn { bow: LaneEnd::Fore }));
    cells[6] = Some(make_ship("enemy-6", Faction::Enemy, 6, Orientation::BowOn { bow: LaneEnd::Fore }));

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
            content: demo_content(),
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
                // Edge-trigger: only on key down, ignore repeats. winit fires
                // events for every key transition; one action per press.
                if event.state != ElementState::Pressed || event.repeat {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if code == KeyCode::Escape {
                    event_loop.exit();
                    return;
                }
                let player_snapshot = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .cloned();
                let intent = match &player_snapshot {
                    Some(ship) => key_to_intent_with_ship(code, ship),
                    None => key_to_intent(code),
                };
                if let Some(intent) = intent {
                    let changed = apply_intent(
                        intent,
                        &mut self.board,
                        &self.content,
                        &render_example_board,
                    );
                    if changed {
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
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
 * Tests — drive the input layer without winit. The tester (task #44) can
 * extend these into a full input-replay suite.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_board() -> Board {
        render_example_board()
    }

    #[test]
    fn key_to_intent_maps_movement_keys() {
        assert_eq!(key_to_intent(KeyCode::ArrowLeft), Some(Intent::MoveLeft));
        assert_eq!(key_to_intent(KeyCode::ArrowRight), Some(Intent::MoveRight));
        assert_eq!(key_to_intent(KeyCode::Tab), Some(Intent::Reorient));
        assert_eq!(key_to_intent(KeyCode::KeyV), Some(Intent::Vent));
    }

    #[test]
    fn key_to_intent_commit_and_restart() {
        assert_eq!(key_to_intent(KeyCode::KeyR), Some(Intent::CommitTurn));
        assert_eq!(key_to_intent(KeyCode::Space), Some(Intent::CommitTurn));
        assert_eq!(key_to_intent(KeyCode::Enter), Some(Intent::Restart));
    }

    #[test]
    fn key_to_intent_ignores_unbound_keys() {
        assert_eq!(key_to_intent(KeyCode::KeyA), None);
        assert_eq!(key_to_intent(KeyCode::F1), None);
    }

    #[test]
    fn digit_keys_pick_mount_weapons() {
        let board = fresh_board();
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(
            key_to_intent_with_ship(KeyCode::Digit1, player),
            Some(Intent::QueueAction("pulse_laser".into())),
        );
        assert_eq!(
            key_to_intent_with_ship(KeyCode::Digit2, player),
            Some(Intent::QueueAction("torpedo".into())),
        );
    }

    #[test]
    fn digit_keys_for_missing_mounts_return_none() {
        let board = fresh_board();
        let player = board.cells[0].as_ref().unwrap();
        // Mount 3 doesn't exist on the demo player ship.
        assert_eq!(key_to_intent_with_ship(KeyCode::Digit3, player), None);
    }

    #[test]
    fn queueing_action_appends_to_player_queue() {
        let mut board = fresh_board();
        let content = demo_content();
        apply_intent(Intent::QueueAction("pulse_laser".into()), &mut board, &content, &fresh_board);
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(player.queue.last(), Some(&"pulse_laser".to_string()));
    }

    #[test]
    fn move_left_queues_thrust_aft_action() {
        let mut board = fresh_board();
        let content = demo_content();
        apply_intent(Intent::MoveLeft, &mut board, &content, &fresh_board);
        let player = board.cells[0].as_ref().unwrap();
        assert_eq!(player.queue.last(), Some(&ids::THRUST_AFT.to_string()));
    }

    #[test]
    fn commit_turn_runs_resolve_round() {
        // Queue a thrust-fore and commit. The player ship should move from
        // cell 0 to cell 1 once the resolver runs the queue.
        let mut board = fresh_board();
        let content = demo_content();
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
        let content = demo_content();
        apply_intent(Intent::MoveRight, &mut board, &content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &content, &fresh_board);
        apply_intent(Intent::Restart, &mut board, &content, &fresh_board);
        assert!(board.cells[0].as_ref().is_some_and(|s| s.faction == Faction::Player));
        assert!(board.cells[1].is_none());
    }
}
