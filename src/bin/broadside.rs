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
    self, push_between_encounter_overlay, push_salvage_hud,
    win_state, BetweenEncounterChoice, TweenState, WinState,
};
use broadside_engine::runs::{
    advance_after_win, boss_ship_for_spawn, build_encounter_board, capital_boss_ship_for_spawn,
    current_encounter, encounter_outcome, fallback_ship_for_spawn, generate_campaign,
    is_capital_spawn, mark_defeated, placeholder_sectors, AdvanceResult, EncounterOutcome,
};
use broadside_engine::input::{
    intent_to_action_id, key_to_intent, synthetic_card_action_id, DemoContent, Intent, Key,
};
use broadside_engine::catalog::{enemy_ship_from_catalog_at_tier, load_from_path};
use broadside_engine::meta::{salvage_for_capital_encounter, salvage_for_encounter_win};
use broadside_engine::perspective::{fractional_cell_to_screen, LaneGeometry, DEFAULT_LANE};
use broadside_engine::projector::ProjectorConfig;
use broadside_engine::resolve::{
    apply_instant_action, find_player_id, fire_player_queue, run_world_phase, Content,
};
use broadside_engine::subsystems::{HEAT_SINK, POINT_BLANK_DOCTRINE};
use broadside_engine::types::{
    Arc as TArc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation, ReorientTo, Run,
    Sector, ShieldFace, ShieldProfile, Ship, WeaponArchetype,
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
        KeyCode::ArrowUp => Key::Up,
        KeyCode::ArrowDown => Key::Down,
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

    // Turn-start clear of the exact-shot FireEvent set (#59). The resolver clears
    // `fire_events` at the top of `resolve_round`, but the bin drives combat via
    // apply_instant_action / fire_player_queue / run_world_phase directly and
    // never calls resolve_round — so without this the in-game beams would
    // ACCUMULATE across every turn forever. Clearing here resets the set each
    // acting turn; the renderer (CombatVfx) has already latched the previous
    // turn's events into its own fading copy, so dropping them from the board is
    // safe — the fade keeps playing on the renderer's side.
    board.fire_events.clear();

    // Every other intent needs the player. If the player is gone (defeat
    // state), the only legal intent is Restart; everything else no-ops.
    let Some(player_id) = find_player_id(board) else {
        return false;
    };

    match intent {
        // --- Reorient: a 90° turn that TOGGLES bow-on ↔ broadside and stops
        // perpendicular — NOT the 180° bow Fore↔Aft about-face the static
        // `__reorient_flip` synthetic encodes (#52, bruce). We read the
        // player's current orientation here and pick the target stance: bow-on
        // → broadside, broadside → bow-on. The synthetic supplies the action's
        // name/cost/targeting; we override only its REORIENT effect. Stays
        // bin-local — no resolve.rs / AI change (enemy reorient uses its own
        // action def). Reaching bow-Aft via control is a deferred follow-up.
        Intent::ReorientFlip => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            let Some(mut action) = content.action(id).cloned() else {
                return false;
            };
            let bow_on = board
                .cells
                .iter()
                .flatten()
                .find(|s| s.id == player_id)
                .map(|s| matches!(s.orientation, Orientation::BowOn { .. }))
                .unwrap_or(true);
            let to = if bow_on {
                ReorientTo::Broadside
            } else {
                ReorientTo::BowOn
            };
            action.effects = vec![Effect::REORIENT { to }];
            apply_instant_action(&player_id, &action, board, content);
            run_world_phase(board, content);
            true
        }

        // --- Instant intents: apply the synthetic action atomically, then
        // advance the world phase. ---
        // v2 (#18): MoveUp/MoveDown are the 2-D depth moves; they flow through
        // the same instant-synthetic path as the lateral pair (intent_to_action_id
        // maps them to __move_up/__move_down, registered in DemoContent). The
        // Key->Intent BINDING for the depth keys is #18's bin/renderer half; this
        // arm just makes the Intents resolvable so the surface compiles + works.
        Intent::MoveLeft | Intent::MoveRight | Intent::MoveUp | Intent::MoveDown | Intent::Vent => {
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
///
/// (#43 pass-1) Unused now that the render path is the 2-D
/// [`hud::compose_scene_2d`]; retained — with the `App::lane` field,
/// [`player_lane_x`] and [`enemy_telegraph_kind`] — for the lane-keyed overlays
/// (telegraph / ability tiles / hull bar) that return as 2-D overlays on the
/// projector. Delete once those are all reborn on the 2-D path.
#[allow(dead_code)]
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

/// Load the canonical catalog from `assets/broadside.catalog.json` for
/// catalog-driven enemy synthesis. Logs and returns `None` on a missing or
/// unparseable asset — the demo then falls back to placeholder enemy
/// synthesis, so it still runs without the file (headless CI, fresh
/// checkout). Loaded once at startup; the catalog is immutable for the run.
fn load_catalog() -> Option<broadside_engine::types::Catalog> {
    let path = std::path::Path::new("assets/broadside.catalog.json");
    match load_from_path(path) {
        Ok(cat) => {
            log::info!("catalog loaded: {} enemies for catalog-driven synthesis", cat.enemies.len());
            Some(cat)
        }
        Err(e) => {
            log::warn!("catalog load failed ({e}); falling back to placeholder enemy synthesis");
            None
        }
    }
}

/* =============================================================================
 * Ability-tile assembly (#53 redesign / #64).
 *
 * The renderer (hud) lays out + animates the square icon tiles, but only the
 * bin has the Content registry to resolve a mount/card action's archetype
 * (→ icon), damage, and cooldown-max. So we flatten a ship's abilities into
 * `hud::AbilityTile`s here: mounts → slots 1/2/3, field-kit cards → 5/6/7.
 * `queued_index` is the action's position in the ship's `queue` (drives the
 * below↔above animation for the player and the readying-stack for enemies);
 * live cooldown comes from `Ship::cooldowns`. Reused for the player (animated
 * below↔above) and for each enemy (telegraph stack from its queue).
 * ============================================================================= */

/// Screen-x of a faction's first ship on the lane, or `None`.
///
/// (#43 pass-1) Unused on the 2-D render path; see [`demo_lane`].
#[allow(dead_code)]
fn player_lane_x(board: &Board, lane: &LaneGeometry) -> Option<f32> {
    board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
        .map(|s| fractional_cell_to_screen(s.cell as f32, lane).x)
}

/// Archetype → placeholder icon (until real per-ability art lands).
fn archetype_icon(a: WeaponArchetype) -> hud::AbilityIcon {
    match a {
        WeaponArchetype::Beam => hud::AbilityIcon::Beam,
        WeaponArchetype::Ordnance => hud::AbilityIcon::Ordnance,
        WeaponArchetype::Broadside => hud::AbilityIcon::Broadside,
        WeaponArchetype::Displacement => hud::AbilityIcon::Displacement,
        WeaponArchetype::Control => hud::AbilityIcon::Control,
        WeaponArchetype::Movement => hud::AbilityIcon::Movement,
        WeaponArchetype::Defensive => hud::AbilityIcon::Defensive,
    }
}

/// First `DAMAGE` effect amount of an action (`0` = non-damage), for the tile's
/// damage pips.
fn action_damage(action: &broadside_engine::types::Action) -> i32 {
    action
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::DAMAGE { amount, .. } => Some(*amount),
            _ => None,
        })
        .unwrap_or(0)
}

/// `Some(position)` of `action_id` in `ship.queue`, else `None`.
fn queue_index(ship: &Ship, action_id: &str) -> Option<usize> {
    ship.queue.iter().position(|q| q == action_id)
}

/// Build one ship's ability tiles (mounts → 1/2/3, cards → 5/6/7). `icon` /
/// `damage` / `cooldown_max` come from the action def; `cooldown` from the
/// ship; `queued_index` from the ship's queue order.
fn build_ship_tiles(ship: &Ship, content: &dyn Content) -> Vec<hud::AbilityTile> {
    let mut tiles = Vec::new();
    for (i, mount) in ship.mounts.iter().take(3).enumerate() {
        if let Some(action) = content.action(&mount.weapon) {
            tiles.push(hud::AbilityTile {
                slot: (b'1' + i as u8) as char,
                icon: archetype_icon(action.archetype),
                damage: action_damage(action),
                cooldown: ship.cooldowns.get(&mount.weapon).copied().unwrap_or(0).max(0),
                cooldown_max: action.cost.cooldown_max.max(0),
                queued_index: queue_index(ship, &mount.weapon),
            });
        }
    }
    // Field-kit cards → slots '5'..'7'. Cards gate on charges, not cooldown;
    // their queued form is the synthetic `__card_<id>` action.
    for (i, slot) in ['5', '6', '7'].iter().enumerate() {
        if let Some(card_id) = content.card_at(&ship.id, i) {
            let synth = synthetic_card_action_id(&card_id);
            if let Some(action) = content.action(&synth) {
                tiles.push(hud::AbilityTile {
                    slot: *slot,
                    icon: archetype_icon(action.archetype),
                    damage: action_damage(action),
                    cooldown: 0,
                    cooldown_max: 0,
                    queued_index: queue_index(ship, &synth),
                });
            }
        }
    }
    tiles
}

/// Categorise an enemy's NEXT queued action (resolver telegraph, b9268c4) into
/// a [`hud::TelegraphKind`] for the readout: a DAMAGE effect → an incoming
/// `Ability` (icon + amount), a DISPLACE_SELF → a `Move` (its lane direction),
/// a REORIENT → a turn cue. Returns `None` for actions with none of those
/// (nothing worth telegraphing). Read-only over the ship + content.
///
/// (#43 pass-1) Unused on the 2-D render path; see [`demo_lane`]. Returns with
/// the 2-D enemy-telegraph overlay (D4's staged channels).
#[allow(dead_code)]
fn enemy_telegraph_kind(action_id: &str, content: &dyn Content) -> Option<hud::TelegraphKind> {
    let action = content.action(action_id)?;
    // Damage takes precedence (it's the threat the player most needs to read).
    if let Some(amount) = action.effects.iter().find_map(|e| match e {
        Effect::DAMAGE { amount, .. } => Some(*amount),
        _ => None,
    }) {
        return Some(hud::TelegraphKind::Ability {
            icon: archetype_icon(action.archetype),
            damage: amount,
        });
    }
    // A self-displacement → a directional move cue. `direction` is lane-relative
    // (Some) when the AI queued a lane-relative close (#68); fall back to Fore.
    if let Some(dir) = action.effects.iter().find_map(|e| match e {
        Effect::DISPLACE_SELF { direction, .. } => Some(direction.unwrap_or(LaneEnd::Fore)),
        _ => None,
    }) {
        return Some(hud::TelegraphKind::Move { dir });
    }
    if action
        .effects
        .iter()
        .any(|e| matches!(e, Effect::REORIENT { .. }))
    {
        return Some(hud::TelegraphKind::Reorient);
    }
    None
}

/// Best-effort "what killed you" phrase for the defeat overlay. The player ship
/// is already gone by the time defeat resolves, so we name the dominant
/// surviving enemy (highest hull = the threat that finished the fight). Read
/// from the board's enemy ships; `None` if there are none to name.
fn defeat_cause(board: &Board) -> Option<String> {
    let killer = board
        .cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .max_by_key(|s| s.hull)?;
    let name = killer
        .klass
        .as_deref()
        .unwrap_or("ENEMY")
        .to_ascii_uppercase();
    Some(format!("DESTROYED BY {}", name))
}

/// Mirrors the board state hard-coded in `render-example.ts`. Used as both
/// the startup scene and the Restart target.
fn render_example_board() -> Board {
    let size = 7usize;
    // v2 (A3 Board EXPAND): len-CELLS backing Vecs so Board::ship_at works over
    // the 5×4 grid; the 1-D demo placement below only touches cells[0..size].
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();

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
        hazards: (0..broadside_engine::grid::CELLS).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
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
        // m3 (#49): a BROADSIDE-arc gun so key 3 is live AND it only bears when
        // the player turns broadside — teaching the REORIENT mechanic (the point
        // of a game called Broadside: forward guns for the bow-on approach, a
        // broadside that rewards the turn). `broadside_battery` is an existing
        // catalog gun (Arc::BroadsideArc, band close → 2D Near via #28); no
        // invented numbers. A legibility cue ("this weapon needs you turned") is
        // a renderer follow-up — for now the gun is wired.
        Mount { id: "m3".into(), arc: TArc::BroadsideArc, weapon: "broadside_battery".into() },
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

/// Materialize an enemy [`Ship`] from a `ShipSpawn`, in dispatch priority:
///
/// 1. **Final boss** (`class_id == "warlord"`, task #83) → the hand-tuned
///    [`boss_ship_for_spawn`] (hull 14, ReactorBreach, 3 mounts).
/// 2. **Sector-end capitals** (#69) → [`capital_boss_ship_for_spawn`]: an armed
///    boss baseline, NOT the hull-3 fallback. Before this, every named capital
///    except the warlord degraded to a popgun because `capital_spawn` writes
///    the capital's DISPLAY name into `class_id` (not in `enemies[]`) and only
///    `"warlord"` routed to a boss. [`is_capital_spawn`] matches the spawn's
///    `class_id` against `catalog.capitals` by name.
/// 3. **Regular enemies** → catalog synthesis via
///    [`enemy_ship_from_catalog_at_tier`] (real hull, mounts, traits).
/// 4. **Fallback** → [`fallback_ship_for_spawn`] when the catalog is absent or
///    the id isn't a known enemy/capital (graceful degrade, no crash).
///
/// Shared by `build_current_board` (the live board) and
/// `award_encounter_salvage` (the reward path) so both agree on what a spawn
/// becomes — a capital that's a real boss on the board must also be a real
/// boss when its salvage is computed.
fn synth_enemy_for_spawn(
    spawn: &broadside_engine::types::ShipSpawn,
    catalog: Option<&broadside_engine::types::Catalog>,
    patrol_tier: u8,
) -> Ship {
    if spawn.class_id == "warlord" {
        return boss_ship_for_spawn(spawn);
    }
    if let Some(cat) = catalog {
        if is_capital_spawn(&spawn.class_id, cat) {
            return capital_boss_ship_for_spawn(spawn, cat);
        }
        if let Some(ship) = enemy_ship_from_catalog_at_tier(cat, spawn, patrol_tier) {
            return ship;
        }
    }
    fallback_ship_for_spawn(spawn)
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
        // v2 (A3 EXPAND): transitional 2-D defaults. The 1-D lane index and a
        // 2-D grid Pos are different spaces with no valid bijection (lead
        // ruling) — don't derive pos from cell. The renderer's D-series rebuilds
        // this demo scene natively on the 5×4 grid.
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation,
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
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
    /// (#43 pass-1) The legacy 1-D flat lane. No longer read by the render path
    /// (now [`hud::compose_scene_2d`]); kept (still constructed in `App::new`)
    /// for the lane-keyed overlays that return as 2-D overlays. See [`demo_lane`].
    #[allow(dead_code)]
    lane: LaneGeometry,
    content: DemoContent,
    /// Canonical catalog (assets/broadside.catalog.json), loaded once at
    /// startup. `None` if the asset is missing or fails to parse — the
    /// spawn closure then falls back to the placeholder synthesizers, so
    /// the demo still runs headless / without the asset. Drives
    /// catalog-backed enemy synthesis (real hull / mounts / traits per
    /// the canonical `enemies[]`).
    catalog: Option<broadside_engine::types::Catalog>,
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
    /// Combat-juice VFX (#51): observes board state-diffs each frame and emits
    /// transient weapon-fire / hit / explosion / trail effects + the live
    /// telegraph cue. Read-only over the board; never touches the resolver.
    vfx: broadside_engine::vfx::CombatVfx,
    /// Player ability-tile layout/animation state (#64): tweens each tile
    /// between its resting below-lane slot and its above-ship queue-stack slot
    /// as abilities are queued/dequeued.
    ability_hud: broadside_engine::hud::AbilityHud,
    /// Free-running animation clock (seconds), advanced ~1/60 each redraw.
    /// Drives the #67 telegraph spinner + move-arrow / incoming-attack pulse.
    /// Wraps so it never loses precision over a long session.
    frame_clock: f32,
    /// Player danger legibility (#67): last observed player hull, and a
    /// decaying hit-flash intensity (0..1). When the player's hull drops
    /// between frames we bump the flash to 1.0; the redraw fades it.
    player_hull_prev: Option<i32>,
    hit_flash: f32,
    /// Shared audio state. `None` if the `audio` feature is off OR the
    /// audio backend failed to open on startup (headless CI, missing
    /// driver). When present, the bus is re-installed on every
    /// Restart so the fresh board's bus gets the same closures.
    #[cfg(feature = "audio")]
    audio: Option<Rc<RefCell<broadside_engine::audio::AudioState>>>,
}

impl App {
    fn new() -> Self {
        // #62: drive the campaign off the canonical catalog when it loaded —
        // generate_campaign turns the catalog's SectorDef[] into runtime
        // sectors via the #60 spawn-pool generator. Falls back to the
        // hand-authored placeholder_sectors() if the catalog is absent
        // (headless / missing asset), so the demo still runs. Patrol tier
        // starts at 1 (the run's global difficulty; meta/run state will
        // drive it later).
        let catalog = load_catalog();
        let sectors = match catalog.as_ref() {
            Some(cat) if !cat.sectors.is_empty() => generate_campaign(cat, 1),
            _ => placeholder_sectors(),
        };
        #[allow(unused_mut)]
        let mut app = Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane: demo_lane(),
            content: fresh_content(),
            catalog,
            camera_angle_idx: CAMERA_ANGLE_DEFAULT_INDEX,
            tween_anchors: HashMap::new(),
            sectors,
            run: Run::new(Self::fresh_player_ship()),
            demo_state: DemoState::Playing,
            vfx: broadside_engine::vfx::CombatVfx::new(),
            ability_hud: broadside_engine::hud::AbilityHud::new(),
            frame_clock: 0.0,
            player_hull_prev: None,
            hit_flash: 0.0,
            #[cfg(feature = "audio")]
            audio: None,
        };
        // #83: boot into the campaign's FIRST generated encounter (player
        // mid-lane, pincered catalog enemies that bear + fire) instead of the
        // showcase demo board the struct literal seeded above. Mirrors
        // restart_run's board build; falls back to render_example_board only if
        // the run has no current encounter. Done BEFORE the audio install below
        // so the EventBus is wired on the board the player actually plays.
        if let Some(first) = app.build_current_board() {
            app.board = first;
        }
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

    /// Build the [`Board`] for the current encounter. Returns `None` if
    /// the run has no current encounter (defeated, victorious, or sector
    /// index past the end of `self.sectors`).
    ///
    /// Spawn → ship dispatch is handled by [`synth_enemy_for_spawn`]
    /// (warlord → hand-tuned boss; capital → armed boss #69; regular →
    /// catalog synthesis; else → fallback). The salvage path
    /// (`award_encounter_salvage`) uses the same fn so board + reward agree.
    fn build_current_board(&self) -> Option<Board> {
        let enc = current_encounter(&self.run, &self.sectors)?;
        // The current sector's patrol tier feeds the catalog synthesizer's
        // difficulty-tier seam (hull5-at-patrol-5 is dormant today, but the
        // tier is threaded so wiring it later is a one-line change).
        let patrol_tier = self
            .sectors
            .get(self.run.current_sector_idx)
            .map(|s| s.patrol_tier)
            .unwrap_or(1);
        let player = Self::fresh_player_ship();
        let catalog = self.catalog.as_ref();
        Some(build_encounter_board(enc, player, |spawn| {
            Some(synth_enemy_for_spawn(spawn, catalog, patrol_tier))
        }))
    }

    /// Award salvage for the just-cleared encounter into `self.run`. Called
    /// on the `EncounterOutcome::Won` transition, BEFORE the run advances
    /// (so `current_encounter` still points at the encounter that was won).
    ///
    /// Capital/boss encounters pay the doc-canonical tier-scaled
    /// [`CapitalDef`] salvage (#63) via `award_run_salvage_with_catalog`;
    /// other encounters fall back to per-enemy salvage. Only fires when a
    /// catalog loaded — the placeholder campaign has no capitals, so there's
    /// nothing to reward and we skip (the old flat-salvage path was never
    /// wired into the bin, so this is the first live salvage accrual).
    fn award_encounter_salvage(&mut self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return; // no catalog → placeholder campaign, no salvage source
        };
        let Some(enc) = current_encounter(&self.run, &self.sectors) else {
            return;
        };
        let patrol_tier = self
            .sectors
            .get(self.run.current_sector_idx)
            .map(|s| s.patrol_tier)
            .unwrap_or(1);
        // Compute the salvage with only IMMUTABLE borrows (catalog, enc,
        // sectors), then apply it to self.run with the mutable borrow —
        // avoids borrowing self.catalog and self.run simultaneously.
        let earned = salvage_for_capital_encounter(enc, catalog, patrol_tier).unwrap_or_else(|| {
            // Non-capital fallback: per-enemy salvage via the same
            // spawn→Ship builder build_current_board uses (shared
            // synth_enemy_for_spawn so board + reward agree on what each
            // spawn becomes — incl. the #69 armed-capital route).
            salvage_for_encounter_win(enc, |spawn| {
                Some(synth_enemy_for_spawn(spawn, Some(catalog), patrol_tier))
            })
        });
        self.run.salvage = self.run.salvage.saturating_add(earned);
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
        // Launch maximized so the continuous fit-scale blit fills the screen
        // by default (bruce no longer has to resize each run to get a large
        // render). `with_inner_size` stays as the un-maximized fallback size
        // (1 window pixel = 1 virtual pixel) for platforms / WMs that ignore
        // the maximize hint.
        let attrs = Window::default_attributes()
            .with_title("Broadside")
            .with_maximized(true)
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
        // Install the loft meshes the 3D render path draws: the vendored CAD
        // hull (assets/ships/broadside-ship.glb) shared by every ship — the
        // player gets a distinctly cool-tinted copy, the four enemies the
        // authored (orange-accented) colours. Meshes are uploaded once here;
        // per-ship poses are synced from board orientation each frame. hud emits
        // a LoftShip for any ship whose mesh is installed (skipping its 2D
        // silhouette). The glb is embedded via include_bytes! so it loads
        // regardless of the binary's run directory.
        const SHIP_GLB: &[u8] = include_bytes!("../../assets/ships/broadside-ship.glb");
        let player_ok = gfx.install_player_cad(SHIP_GLB).is_ok();
        let enemy_ok = gfx.install_enemy_cad(SHIP_GLB).is_ok();
        if player_ok && enemy_ok {
            log::info!(
                "loft: CAD hull installed for player (tinted) + enemies ({} bytes)",
                SHIP_GLB.len()
            );
        } else {
            log::warn!(
                "loft: CAD import failed (player_ok={player_ok}, enemy_ok={enemy_ok}); \
                 affected ships fall back to 2D silhouettes"
            );
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
                            // Award salvage for the just-cleared encounter
                            // BEFORE advancing the run (the run still points
                            // at this encounter). Capital bosses pay the
                            // doc-canonical tier-scaled CapitalDef salvage
                            // (#63); other encounters fall back to per-enemy
                            // salvage. Data-driven only when the catalog
                            // loaded; no-op otherwise (placeholder campaign
                            // has no capitals to reward).
                            self.award_encounter_salvage();
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
                let now = Instant::now();
                // (#43) `angle` (legacy camera-revolve) and the per-ship `tween`
                // fed the 1-D compose_scene_tweened; the 2-D path
                // (compose_scene_2d) takes neither in pass 1. `active_tween` is
                // still read below to keep the redraw loop alive while a logical
                // turn-tween is in flight (re-applied visually with the 2-D tween
                // layer follow-up).
                let active_tween = self.has_active_tween(now);
                let demo_state = self.demo_state;
                let sector_idx = self.run.current_sector_idx;
                let salvage = self.run.salvage;
                // Sync every loft ship's pose to its current board orientation
                // (sync_loft_pose creates a pose on first sight and reorients on
                // a bow-on↔broadside flip — a no-op when unchanged, so flips
                // auto-tween), prune poses for ships that have left the board,
                // then advance all idle + tweens by a fixed ~60 Hz dt.
                let loft_ships: Vec<(String, Orientation)> = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .map(|s| (s.id.clone(), s.orientation))
                    .collect();
                // Combat juice (#51): diff the board for this frame (spawns
                // hit/explosion/trail/beam effects), then advance lifetimes by a
                // fixed ~60 Hz dt. observe() is read-only over the board and
                // idempotent on unchanged frames, so running it every redraw is
                // safe — it only spawns on an actual state change.
                self.vfx.observe(&self.board);
                let vfx_active = self.vfx.advance(1.0 / 60.0);
                // Free-running animation clock kept advancing for the #67
                // telegraph spinner / move-arrow / incoming pulse — consumed by
                // the lane-keyed overlays that are dropped in the #43 pass-1 2-D
                // switch and return as 2-D overlays (the `spin`/`pulse` readers
                // come back with them). Wrap at TAU so it stays precise.
                self.frame_clock = (self.frame_clock + 1.0 / 60.0) % std::f32::consts::TAU;
                // Player danger legibility (#67): read the player's current hull
                // and flash the screen red when it drops. The flash decays ~2/s.
                let player_hull = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .map(|s| (s.hull, s.max_hull));
                if let Some((hull, _)) = player_hull {
                    if let Some(prev) = self.player_hull_prev {
                        if hull < prev {
                            self.hit_flash = 1.0;
                        }
                    }
                    self.player_hull_prev = Some(hull);
                }
                self.hit_flash = (self.hit_flash - (1.0 / 60.0) * 2.0).max(0.0);
                let flash_active = self.hit_flash > 0.01;
                // Ability tiles (#64): build the player's tiles and advance the
                // below↔above queue animation (~60 Hz). Built before the gfx
                // borrow (needs &board/&content); emitted into the draw list in
                // the Playing block below.
                let player_tiles = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .map(|p| build_ship_tiles(p, &self.content))
                    .unwrap_or_default();
                let ability_active = self.ability_hud.advance(&player_tiles, 1.0 / 60.0);
                let Some(gfx) = self.gfx.as_mut() else { return };
                for (id, orient) in &loft_ships {
                    gfx.sync_loft_pose(id, *orient);
                }
                let live_ids: Vec<String> = loft_ships.iter().map(|(id, _)| id.clone()).collect();
                gfx.retain_loft_poses(&live_ids);
                let loft_animating = gfx.advance_loft_poses(1.0 / 60.0);
                // v2 render path (#43): the playable bin now composes the 2-D
                // perspective scene (the SAME hud::compose_scene_2d encounter_
                // preview uses) instead of the legacy 1-D flat-lane
                // compose_scene_tweened — so it renders the real ship.pos/facing
                // board, not 1-D noise. Pass-1 trade-off (lead-approved): a
                // static-but-correct 2-D board beats an animated-but-wrong legacy
                // one, so per-ship turn TWEENING and the 3-D LOFT ships drop here
                // for now (compose_scene_2d takes board+cfg, no tween/gfx) — both
                // return as a follow-up (a 2-D tween layer + loft seating driven
                // by CellQuad::depth_scale, D4/D6). The lane-keyed overlays
                // (vfx, the enemy-telegraph badges/arrows, ability tiles, hull
                // bar) are ALSO dropped this pass: they position by
                // fractional_cell_to_screen / &self.lane, which don't match the
                // 2-D board (they were the source of the overlapping-label mess);
                // they come back as 2-D overlays on the projector (telegraph =
                // D4's staged channels). Screen-space HUD (salvage, legend, the
                // hit-flash, the modal state overlays) is unaffected and stays.
                let mut instances =
                    hud::compose_scene_2d(&self.board, &ProjectorConfig::default());
                // In-game salvage counter (top-right) + controls legend
                // (bottom-left) — both screen-space, independent of the board
                // projection. The modal overlays surface salvage in their banners.
                if matches!(demo_state, DemoState::Playing) {
                    push_salvage_hud(&mut instances, salvage);
                    // Minimalist controls legend, bottom-left (#82).
                    hud::push_controls_overlay(&mut instances);
                    // Player danger legibility (#67): screen hit-flash on damage.
                    // (The lane-anchored hull bar returns as a 2-D overlay.)
                    hud::push_player_hit_flash(&mut instances, self.hit_flash);
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
                        // Surface WHAT killed the player so a loss reads as an
                        // event, not just a red screen (#67).
                        let cause = defeat_cause(&self.board);
                        hud::push_run_defeated_overlay_with_cause(
                            &mut instances,
                            salvage,
                            cause.as_deref(),
                        );
                    }
                }
                match gfx.render(&instances) {
                    Ok(()) => {}
                    Err(e @ (wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated)) => {
                        // (#43-followup diag) If this fires EVERY frame, the
                        // surface is going Outdated→reconfigure in a loop, which
                        // presents black frames between reconfigures = the
                        // "fade-to-black → snap-to-grey" pulse. A one-shot here
                        // is normal (resize); a per-frame flood is the bug.
                        log::warn!("surface {e:?} → reconfigure (per-frame flood = the pulse bug)");
                        gfx.reconfigure();
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        log::error!("wgpu surface OOM, exiting");
                        event_loop.exit();
                    }
                    Err(e) => log::warn!("surface error: {e:?}"),
                }
                // (#43-followup) Re-request a redraw ONLY while something that is
                // (#47) Drive a CONTINUOUS render loop: re-request a redraw every
                // frame. This is the standard game-loop cadence and is THROTTLED
                // by vsync (present_mode = AutoVsync ≈ 60 fps) — it is NOT an
                // uncapped spin. It MUST stay continuous: a winit window that
                // stops presenting leaves a stale swapchain that Windows' DWM
                // compositor degrades — pulsing the image through greys to black
                // and periodically force-repainting it. That stale-swapchain
                // degradation (not any draw content, not the surface, not monitor
                // sleep) was the "fade to black → snap to grey" Bruce saw after a
                // mistaken earlier attempt to gate redraws on
                // flash_active-only — which rendered exactly ONE frame then went
                // idle. Keeping the swapchain live every frame fixes it.
                //
                // The animation-liveness flags are consumed here only to advance
                // their state each frame; redraw cadence no longer depends on
                // them (we always redraw), so they no longer gate anything.
                let _ = (active_tween, vfx_active, ability_active, loft_animating, flash_active);
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// Keep the DISPLAY awake for the lifetime of the session (#47 follow-up).
///
/// A winit/wgpu app does NOT inhibit Windows display power-management by default,
/// and the monitor powers down on INPUT-idle (not GPU-idle) — so even a busy
/// render loop won't stop it. After a few idle minutes the screen does its slow
/// ~15 s analog fade to black, then snaps back the instant you touch a key: the
/// symptom Bruce hit. The standard game fix is to assert `ES_DISPLAY_REQUIRED`
/// while the app runs, so Windows treats the session as "display in use" and
/// never sleeps the monitor mid-game.
///
/// `ES_CONTINUOUS` makes the state persist until changed; OR-ed with
/// `ES_DISPLAY_REQUIRED` it holds the display on. We clear it on exit
/// ([`release_display_keep_awake`], `ES_CONTINUOUS` alone) so normal power
/// behaviour resumes once the game closes. No-op on non-Windows.
#[cfg(windows)]
fn keep_display_awake() {
    // `EXECUTION_STATE = u32`. SetThreadExecutionState lives in kernel32.
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
    extern "system" {
        fn SetThreadExecutionState(esFlags: u32) -> u32;
    }
    // SAFETY: a single FFI call to a documented kernel32 entry point with a valid
    // flag bitmask; it has no memory effects and returns the previous state
    // (ignored). Returns 0 only on an invalid flag, which these are not.
    let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) };
    if prev == 0 {
        log::warn!("SetThreadExecutionState(ES_DISPLAY_REQUIRED) failed; monitor may sleep");
    }
}

/// Restore default display power-management on exit (clears the
/// [`keep_display_awake`] hold). No-op on non-Windows.
#[cfg(windows)]
fn release_display_keep_awake() {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    extern "system" {
        fn SetThreadExecutionState(esFlags: u32) -> u32;
    }
    // SAFETY: same documented kernel32 call; ES_CONTINUOUS alone drops the
    // display-required assertion so the monitor can sleep normally again.
    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS);
    }
}

#[cfg(not(windows))]
fn keep_display_awake() {}
#[cfg(not(windows))]
fn release_display_keep_awake() {}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Stop Windows sleeping the monitor mid-session (#47): the display powers
    // down on input-idle regardless of render activity, producing the slow
    // fade-to-black Bruce saw. Held for the session, cleared on exit.
    keep_display_awake();

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop");

    release_display_keep_awake();
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
        assert_eq!(keycode_to_key(KeyCode::ArrowUp), Some(Key::Up)); // #18
        assert_eq!(keycode_to_key(KeyCode::ArrowDown), Some(Key::Down)); // #18
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
