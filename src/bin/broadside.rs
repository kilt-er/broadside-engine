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
//! | `Enter` | `Restart` | Reset the board to its initial state (also the only key accepted while a run-end overlay is showing) |
//! | `1` / `2` / `3` (overloaded) | Path choice | While the EncounterComplete overlay is up: 1 = repair (+2 hull), 2 = upgrade (placeholder), 3 = continue to next encounter |
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
use std::time::Instant;

#[cfg(feature = "audio")]
use std::cell::RefCell;
#[cfg(feature = "audio")]
use std::rc::Rc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::cards::PlayResult;
use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::{Gfx, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::hud::{
    self, push_between_encounter_overlay, push_run_defeated_overlay, push_salvage_hud,
    win_state, BetweenEncounterChoice, TweenState, WinState,
};
use broadside_engine::runs::{
    advance_after_win, build_encounter_board, current_encounter, encounter_outcome, fallback_ship_for_spawn,
    mark_defeated, placeholder_sectors, AdvanceResult, EncounterOutcome,
};
use broadside_engine::input::{
    intent_to_action_id, key_to_intent, synthetic_card_action_id, DemoContent, Intent, Key,
};
use broadside_engine::perspective::{LaneGeometry, DEFAULT_LANE};
use broadside_engine::resolve::{
    apply_instant_action, find_player_id, fire_player_queue, run_world_phase, Content,
};
use broadside_engine::subsystems::{HEAT_SINK, POINT_BLANK_DOCTRINE};
use broadside_engine::types::{
    Arc as TArc, Board, EventBus, Faction, LaneEnd, Mount, Orientation, Run, Sector, ShieldFace,
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
 * renderer requests a redraw on true). Implements Shogun-Showdown turn
 * semantics: every input advances time (i.e. runs phases 2-4 via
 * [`run_world_phase`]). Move / Reorient / Vent / PlayCard apply
 * instantly via [`apply_instant_action`]; `QueueAction` pushes to the
 * player's queue (NOT fired until `CommitTurn`); `CommitTurn` fires the
 * queue via [`fire_player_queue`]; `Restart` rebuilds the board.
 * ========================================================================== */

/// Apply an [`Intent`] to the board under Shogun-Showdown turn rules.
/// `initial_board` produces a fresh starting state for `Restart`. Returns
/// true if the board changed.
///
/// `content` is `&mut` because [`Intent::PlayCard`] needs to validate +
/// decrement card charges via [`Content::try_play_card`].
pub fn apply_intent(
    intent: Intent,
    board: &mut Board,
    content: &mut dyn Content,
    initial_board: &dyn Fn() -> Board,
) -> bool {
    // Restart never advances time — it discards the whole board.
    if matches!(intent, Intent::Restart) {
        *board = initial_board();
        return true;
    }

    // Every other intent needs the player. If the player is gone (defeat
    // state), the only legal intent is Restart; everything else no-ops.
    let Some(player_id) = find_player_id(board) else {
        return false;
    };

    match intent {
        // --- Instant intents: apply the synthetic action atomically, then
        // advance the world phase. ---
        Intent::MoveLeft | Intent::MoveRight | Intent::ReorientFlip | Intent::Vent => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            // The synthetic Action is registered with DemoContent (see
            // `register_synthetics`). Clone so we don't hold a borrow on
            // content while we mutate the board.
            let Some(action) = content.action(id).cloned() else {
                return false;
            };
            apply_instant_action(&player_id, &action, board, content);
            run_world_phase(board, content);
            true
        }

        // --- PlayCard: validate + decrement charges via try_play_card,
        // then run the synthetic `__card_<id>` Action instantly. World
        // phase runs after. ---
        Intent::PlayCard(card_id) => {
            match content.try_play_card(&player_id, &card_id) {
                PlayResult::Played => {
                    let synth_id = synthetic_card_action_id(&card_id);
                    let Some(action) = content.action(&synth_id).cloned() else {
                        // Charges were decremented but the synthetic isn't
                        // registered. Best we can do is still advance time
                        // so the player isn't stuck.
                        run_world_phase(board, content);
                        return true;
                    };
                    apply_instant_action(&player_id, &action, board, content);
                    run_world_phase(board, content);
                    true
                }
                PlayResult::UnknownCard
                | PlayResult::NotCarried
                | PlayResult::InsufficientCharges => false,
            }
        }

        // --- QueueAction: push the action id to the player's queue. The
        // queue is NOT fired here — the player commits later via Enter /
        // Space. Time still advances. ---
        Intent::QueueAction(_) => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            let pushed = append_to_player_queue(board, id.to_string());
            run_world_phase(board, content);
            pushed
        }

        // --- CommitTurn: fire whatever is in the queue (empty queue =
        // Wait), then world phase. ---
        Intent::CommitTurn => {
            fire_player_queue(&player_id, board, content);
            run_world_phase(board, content);
            true
        }

        // Restart was handled at the top; this arm is unreachable but
        // keeps the match exhaustive without an `_` wildcard.
        Intent::Restart => unreachable!("Restart handled before player lookup"),
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
    // "aegis" is the first broadside-native player class (bruce's
    // hand-painted PNGs under assets/sprites/aegis_*.png). The
    // sprite loader picks them up via class-slug match; the renderer
    // emits TexturedShip draws via the side/top blend pipeline when
    // both views are present.
    //
    // TODO(broadside-content): once the canonical class roster
    // lands and replaces the Shogun-Showdown-derived placeholders
    // (wanderer / ronin / shadow / jujitsuka / chainmaster), wire a
    // real ClassDef for "aegis" into the catalog and look up the
    // player's loadout from there. For now this is a sprite-only
    // hook — combat math doesn't depend on the slug.
    player.klass = Some("aegis".into());
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

/// Duration of the per-ship snap → smooth lerp after a turn-advancing
/// input. ~200ms reads as crisp without feeling laggy at 60Hz.
const TWEEN_DURATION_MS: u32 = 200;

/// Per-ship "where did this ship visually start the tween from + when did
/// the tween begin?" anchor. Recorded by `App::record_tween_anchors`
/// after each input mutation; consumed by `App::tween_state` each frame
/// to compute the eased visual cell.
struct TweenAnchor {
    /// Where the ship was rendering when the input arrived. Fractional
    /// so an already-in-flight tween can be re-anchored mid-flight
    /// without an extra snap.
    from_cell: f32,
    /// When the input fired. Elapsed > TWEEN_DURATION_MS means the
    /// anchor is fully resolved and can be evicted.
    started_at: Instant,
}

/// Phase 3 demo state machine. The bin transitions between these on
/// every `apply_intent` call. `Playing` is the normal turn-by-turn
/// state; the other three are modal overlays that gate input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoState {
    /// Live encounter — normal apply_intent flow.
    Playing,
    /// Last encounter cleared. 1/2/3 chooses repair / upgrade /
    /// continue. Everything else is swallowed except Esc.
    EncounterComplete,
    /// Final encounter of final sector cleared. Enter restarts the
    /// run from sector 0.
    RunComplete,
    /// Player ship destroyed (and not at the encounter-clear screen).
    /// Enter restarts the run from sector 0. Distinct from
    /// `WinState::Defeat` (which is per-encounter) — this flips on
    /// `mark_defeated` and the Run carries the flag forward.
    RunDefeated,
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    board: Board,
    lane: LaneGeometry,
    content: DemoContent,
    /// Index into `CAMERA_ANGLE_STEPS_DEG`. Cycled by `[` and `]`.
    camera_angle_idx: usize,
    /// Per-ship tween anchors keyed by `Ship::id`. Populated whenever an
    /// input mutates the board; the redraw path interpolates `from_cell`
    /// → `ship.cell` over `TWEEN_DURATION_MS` using ease-out quad.
    tween_anchors: HashMap<String, TweenAnchor>,
    /// The campaign — list of sectors the run progresses through.
    /// Built once at startup from [`placeholder_sectors`] and not
    /// rebuilt on Restart.
    sectors: Vec<Sector>,
    /// Cross-encounter run state. Defeats / victories flip the
    /// `defeated` / `victorious` flags. Restart rebuilds a fresh Run
    /// at sector 0, encounters 0.
    run: Run,
    /// Modal-overlay state. `Playing` for the normal turn loop; the
    /// other variants gate input until the player presses the
    /// matching exit key.
    demo_state: DemoState,
    /// Shared audio state. `None` if the `audio` feature is off OR the
    /// audio backend failed to open on startup (headless CI, missing
    /// driver). When present, the bus is re-installed on every
    /// Restart so the fresh board's bus gets the same closures.
    #[cfg(feature = "audio")]
    audio: Option<Rc<RefCell<broadside_engine::audio::AudioState>>>,
}

impl App {
    fn new() -> Self {
        #[allow(unused_mut)]
        let mut app = Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane: demo_lane(),
            content: fresh_content(),
            camera_angle_idx: CAMERA_ANGLE_DEFAULT_INDEX,
            tween_anchors: HashMap::new(),
            sectors: placeholder_sectors(),
            run: Run::new(Self::fresh_player_ship()),
            demo_state: DemoState::Playing,
            #[cfg(feature = "audio")]
            audio: None,
        };
        #[cfg(feature = "audio")]
        {
            if let Some(state) = broadside_engine::audio::AudioState::new(std::path::Path::new("assets")) {
                let shared = Rc::new(RefCell::new(state));
                broadside_engine::audio::install_on_bus(&mut app.board, Rc::clone(&shared));
                app.audio = Some(shared);
                log::info!("audio enabled");
            } else {
                log::info!("audio disabled (no backend or device not available)");
            }
        }
        app
    }

    /// Build the player ship for the current run. Cloned from the
    /// existing demo player so loadout / shield_profile / mounts stay
    /// consistent across encounters. Subsystems live on `content`, not
    /// on the ship, so they carry over for free.
    fn fresh_player_ship() -> Ship {
        player_ship(0)
    }

    /// Build the [`Board`] for the current encounter. Uses
    /// [`build_encounter_board`] with [`fallback_ship_for_spawn`] for
    /// class-id resolution — content's class catalog isn't required
    /// for the demo to function. Returns `None` if the run has no
    /// current encounter (defeated, victorious, or sector index past
    /// the end of `self.sectors`).
    fn build_current_board(&self) -> Option<Board> {
        let enc = current_encounter(&self.run, &self.sectors)?;
        let player = Self::fresh_player_ship();
        Some(build_encounter_board(enc, player, |spawn| Some(fallback_ship_for_spawn(spawn))))
    }

    /// Reset run + content + board to a fresh sector-0 / encounter-0
    /// start. Called on Restart from `RunComplete` / `RunDefeated`
    /// overlays. Also re-installs audio on the new board's EventBus.
    fn restart_run(&mut self) {
        self.run = Run::new(Self::fresh_player_ship());
        self.content = fresh_content();
        self.board = self
            .build_current_board()
            .unwrap_or_else(render_example_board);
        self.demo_state = DemoState::Playing;
        self.tween_anchors.clear();
        self.reinstall_audio();
    }

    /// React to an `EncounterComplete` 1/2/3 choice. Repair applies
    /// a small hull-restore on the player; upgrade is a placeholder
    /// (no-op); continue advances the run. Returns true if the
    /// caller should request_redraw.
    fn apply_path_choice(&mut self, choice: Key) -> bool {
        match choice {
            Key::D1 => {
                // Repair: restore up to +2 hull on the player.
                if let Some(player) = self
                    .board
                    .cells
                    .iter_mut()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                {
                    let restored = (player.hull + 2).min(player.max_hull);
                    log::info!("repair: hull {} -> {}", player.hull, restored);
                    player.hull = restored;
                }
                // Stays in EncounterComplete — player picks again
                // or presses 3 to continue.
                true
            }
            Key::D2 => {
                // Upgrade: placeholder. Future: spend salvage on a
                // subsystem install. For now just log.
                log::info!("upgrade: placeholder (not yet wired)");
                true
            }
            Key::D3 => {
                // Continue: advance the run.
                match advance_after_win(&mut self.run, &self.sectors) {
                    AdvanceResult::NextEncounter | AdvanceResult::NextSector => {
                        if let Some(next) = self.build_current_board() {
                            self.board = next;
                            self.demo_state = DemoState::Playing;
                            self.tween_anchors.clear();
                            self.reinstall_audio();
                        } else {
                            // Shouldn't happen — advance_after_win
                            // said there's another encounter, but
                            // current_encounter says no. Defensive
                            // fall-back to RunComplete.
                            self.demo_state = DemoState::RunComplete;
                        }
                    }
                    AdvanceResult::Victorious => {
                        self.demo_state = DemoState::RunComplete;
                    }
                    AdvanceResult::AlreadyEnded => {
                        // No-op; redraw won't change anything.
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Re-install audio hook subscriptions on the current `self.board.bus`.
    /// Called after Restart since the board (and its bus) get rebuilt.
    /// No-op when the `audio` feature is off OR when the backend isn't
    /// initialized.
    #[cfg(feature = "audio")]
    fn reinstall_audio(&mut self) {
        if let Some(audio) = self.audio.as_ref() {
            broadside_engine::audio::install_on_bus(&mut self.board, Rc::clone(audio));
        }
    }

    #[cfg(not(feature = "audio"))]
    fn reinstall_audio(&mut self) {}

    /// Snapshot the current visual position of every ship. Called BEFORE
    /// `apply_intent` so we have a `from_cell` to anchor the tween from.
    /// The returned map is keyed by ship id; if a ship is mid-tween
    /// (anchor present and not yet expired) we capture its currently
    /// rendered fractional cell rather than its logical cell, so a
    /// rapid double-tap doesn't visibly stutter.
    fn snapshot_visual_cells(&self, now: Instant) -> HashMap<String, f32> {
        let snap = self.tween_state(now);
        let mut out = HashMap::with_capacity(self.board.cells.len());
        for ship in self.board.cells.iter().flatten() {
            let cell = snap
                .visual_cells
                .get(&ship.id)
                .copied()
                .unwrap_or(ship.cell as f32);
            out.insert(ship.id.clone(), cell);
        }
        out
    }

    /// Record fresh tween anchors after `apply_intent` ran: for every
    /// ship currently on the board whose logical cell differs from its
    /// pre-mutation visual cell, plant an anchor at that visual cell so
    /// the next frame interpolates from there.
    fn record_tween_anchors(&mut self, prev_visual: HashMap<String, f32>, now: Instant) {
        // Drop anchors for ships that no longer exist (destroyed or
        // replaced after Restart).
        self.tween_anchors
            .retain(|id, _| self.board.cells.iter().flatten().any(|s| &s.id == id));
        for ship in self.board.cells.iter().flatten() {
            let target = ship.cell as f32;
            let Some(&from) = prev_visual.get(&ship.id) else { continue };
            if (from - target).abs() < 0.001 {
                // Visual position already matches logical — no tween needed.
                self.tween_anchors.remove(&ship.id);
                continue;
            }
            self.tween_anchors.insert(
                ship.id.clone(),
                TweenAnchor { from_cell: from, started_at: now },
            );
        }
    }

    /// Compute the per-ship visual cells for this frame. Anchors past
    /// `TWEEN_DURATION_MS` are dropped; remaining anchors apply
    /// ease-out quad to interpolate `from_cell` → `ship.cell`.
    fn tween_state(&self, now: Instant) -> TweenState {
        let dur_ms = TWEEN_DURATION_MS as f32;
        let mut state = TweenState::default();
        for ship in self.board.cells.iter().flatten() {
            let Some(anchor) = self.tween_anchors.get(&ship.id) else { continue };
            let elapsed = now.duration_since(anchor.started_at).as_secs_f32() * 1000.0;
            let t = (elapsed / dur_ms).clamp(0.0, 1.0);
            // Ease-out quad: 1 - (1 - t)^2. Crisp departure, soft arrival.
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            let target = ship.cell as f32;
            let visual = anchor.from_cell + (target - anchor.from_cell) * eased;
            state.visual_cells.insert(ship.id.clone(), visual);
        }
        state
    }

    /// True if any ship has a tween anchor that hasn't yet expired,
    /// meaning the next frame will still need to redraw to advance the
    /// interpolation. The redraw loop polls this at end-of-frame to
    /// keep requesting redraws while a tween is in flight.
    fn has_active_tween(&self, now: Instant) -> bool {
        let dur = std::time::Duration::from_millis(TWEEN_DURATION_MS as u64);
        self.tween_anchors
            .values()
            .any(|a| now.duration_since(a.started_at) < dur)
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
        if self.gfx.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size);
                }
            }
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

                // Modal-overlay states gate input. Phase 3 introduced
                // EncounterComplete / RunComplete / RunDefeated; each
                // accepts only a small key set, everything else is
                // swallowed.
                match self.demo_state {
                    DemoState::EncounterComplete => {
                        if matches!(key, Key::D1 | Key::D2 | Key::D3) {
                            let changed = self.apply_path_choice(key);
                            if changed {
                                if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                            }
                        }
                        return;
                    }
                    DemoState::RunComplete | DemoState::RunDefeated => {
                        if key == Key::Enter {
                            self.restart_run();
                            if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                        }
                        return;
                    }
                    DemoState::Playing => {}
                }

                // Defeat-mid-encounter still goes through the existing
                // Phase 1 path (apply_intent's Restart route). When the
                // player ship is gone but demo_state is still Playing,
                // we promote to RunDefeated here so the overlay path
                // takes over.
                if win_state(&self.board) == WinState::Defeat {
                    mark_defeated(&mut self.run);
                    self.demo_state = DemoState::RunDefeated;
                    if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                    return;
                }

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
                // Restart resets both the board AND the content so card
                // charges + subsystems come back as a fresh game.
                let is_restart = matches!(intent, Intent::Restart);
                // Snapshot per-ship visual positions BEFORE mutating, so
                // the tween anchor points at where each ship was already
                // rendering (not its logical pre-mutation cell, which
                // may itself be mid-tween).
                let now = Instant::now();
                let prev_visual = self.snapshot_visual_cells(now);
                let changed = apply_intent(intent, &mut self.board, &mut self.content, &render_example_board);
                if is_restart {
                    self.restart_run();
                } else if changed {
                    self.record_tween_anchors(prev_visual, now);
                    // Post-mutation: did this turn end an encounter?
                    match encounter_outcome(&self.board) {
                        EncounterOutcome::Won => {
                            self.demo_state = DemoState::EncounterComplete;
                        }
                        EncounterOutcome::Lost => {
                            mark_defeated(&mut self.run);
                            self.demo_state = DemoState::RunDefeated;
                        }
                        EncounterOutcome::InProgress => {}
                    }
                }
                if changed {
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let angle = CAMERA_ANGLE_STEPS_DEG[self.camera_angle_idx].to_radians();
                let now = Instant::now();
                // Build the tween + draw list FIRST, while we hold
                // only `&self`. Then borrow gfx mutably to render.
                let tween = self.tween_state(now);
                let active_tween = self.has_active_tween(now);
                let demo_state = self.demo_state;
                let sector_idx = self.run.current_sector_idx;
                let salvage = self.run.salvage;
                let Some(gfx) = self.gfx.as_mut() else { return };
                let mut instances = hud::compose_scene_tweened(&self.board, &self.lane, angle, gfx, &tween);
                // In-game salvage counter (top-right) — only shown
                // during Playing state. The modal overlays surface
                // salvage in their own banners.
                if matches!(demo_state, DemoState::Playing) {
                    push_salvage_hud(&mut instances, salvage);
                }
                // Push the appropriate demo-state overlay on top.
                // Compose no longer auto-pushes — the bin owns the
                // overlay decision since #77.
                match demo_state {
                    DemoState::Playing => {}
                    DemoState::EncounterComplete => {
                        push_between_encounter_overlay(
                            &mut instances,
                            BetweenEncounterChoice::EncounterComplete { sector_idx, salvage },
                        );
                    }
                    DemoState::RunComplete => {
                        push_between_encounter_overlay(
                            &mut instances,
                            BetweenEncounterChoice::RunComplete { salvage },
                        );
                    }
                    DemoState::RunDefeated => {
                        push_run_defeated_overlay(&mut instances, salvage);
                    }
                }
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
                // Keep redrawing only while a tween is in flight. Once
                // every anchor expires the scene is static and we let
                // the event loop sleep until the next input.
                if active_tween {
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
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
    fn move_intent_advances_ship_instantly() {
        // Under SS turn semantics MoveRight is instant — the ship moves
        // one cell on the press, the queue is NOT touched, and the
        // world phase runs after. Pre-SS the queue would contain the
        // synthetic id; post-SS the queue stays empty.
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        // Player starts at cell 0 with bow=Fore in the demo board.
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        let player_at_1 = board.cells[1]
            .as_ref()
            .is_some_and(|s| s.faction == Faction::Player);
        assert!(player_at_1, "MoveRight should advance the player to cell 1");
        assert!(board.cells[0].is_none(), "cell 0 should be empty after the move");
        let player = board.cells[1].as_ref().unwrap();
        assert!(player.queue.is_empty(), "instant intent must NOT push to queue");
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
    fn play_card_intent_fires_instantly_and_decrements_charges() {
        // Under SS turn semantics PlayCard is instant: try_play_card
        // validates + decrements, then the synthetic `__card_<id>` Action
        // runs through apply_instant_action immediately, then the world
        // phase advances. The queue is NOT touched. Pre-SS the synthetic
        // was queued and fired only on CommitTurn; the new behavior
        // matches the renderer tutorial's `(instant)` tag for cards.
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
        // Charges decremented.
        let charges_after = content
            .field_kits
            .for_ship("player")
            .and_then(|k| k.find(&card_id))
            .map(|c| c.charges)
            .unwrap_or(0);
        assert_eq!(charges_after, charges_before - 1, "play should decrement charges");
        // Queue NOT touched — card fired instantly, the synthetic id
        // never lands in `player.queue`.
        let synth_id = synthetic_card_action_id(&card_id);
        let player = find_player_id(&board)
            .and_then(|id| board.cells.iter().flatten().find(|s| s.id == id))
            .expect("player still on the board after card play");
        assert!(
            !player.queue.contains(&synth_id),
            "instant card play must NOT queue the synthetic id",
        );
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
