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
//! | `←` / `→` | `MoveLeft` / `MoveRight` | Queue synthetic `__move_left` / `__move_right` |
//! | `↑` / `↓` | `MoveUp` / `MoveDown` | Queue synthetic `__move_up` / `__move_down` (N / S) |
//! | `Q` / `E` | `RotateLeft` / `RotateRight` (#75) | Turn the player's FACING a quarter-turn ccw / cw (`__rotate_left` / `__rotate_right`); render + firing arcs follow |
//! | `Tab` | `ReorientFlip` (#75) | 180° about-face: the bin overrides the synthetic to two `RotateRight` (reverses the bow N↔S / E↔W) so the hull visibly turns |
//! | `V` | `Vent` | Queue synthetic `__vent` |
//! | `R` / `Space` | `CommitTurn` | Run `resolve_round`; re-renders next frame |
//! | `Enter` | `Restart` (end-state ONLY) | Restart the run — accepted ONLY on a run-end overlay (defeat / victory). A NO-OP during active combat (#97: it used to rebuild the board mid-fight) |
//! | `1` / `2` / `3` (overloaded) | Path choice | While the EncounterComplete overlay is up: 1 = repair (+2 hull), 2 = upgrade (placeholder), 3 = continue to next encounter |
//! | `,` / `.` | ship-render res (#76) | Cycle the loft offscreen size `160×100 → 220×138 → 320×200 → 480×300` (live) |
//! | `;` / `'` | scene res (#76) | Cycle the whole-scene offscreen size `480×270 → 640×360 → 960×540` (live; 480 is the min + default; everything scales together) |
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
    win_state, BetweenEncounterChoice, WinState,
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
use broadside_engine::grid::{Facing, Pos}; // (#79) TweenAnchor records pre-move pos/facing
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
        KeyCode::KeyQ => Key::Q,
        KeyCode::KeyE => Key::E,
        KeyCode::KeyV => Key::V,
        KeyCode::KeyW => Key::W, // (#126) WAIT — pass the turn (turn-based model)
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

/// (#139/#140/#142) The live scene projector: `for_scene` at the current scene size,
/// re-pitched by the live grid-pitch step (`G`). The `T` key cycles THREE modes the
/// pitch feeds: 0 = with_pitch (constant-footprint drawbridge); 1 = with_stretch (grid
/// stretches to a uniform top-down square, curved column edges mid-arc); 2 =
/// with_stretch_straight (same stretch, STRAIGHT column edges). At pitch step 0 ALL
/// THREE are byte-identical to the chase-cam (each == base at t==0) — the no-regression
/// invariant. ONE place builds it so every projected element (grid, cells, movement,
/// threats, ordnance) shares the identical projection (single spatial source).
fn scene_projector() -> ProjectorConfig {
    let base = ProjectorConfig::for_scene(
        broadside_engine::gfx::scene_w() as f32,
        broadside_engine::gfx::scene_h() as f32,
    );
    let t = broadside_engine::gfx::grid_pitch_t();
    match broadside_engine::gfx::grid_mode() {
        1 => base.with_stretch(t),
        2 => base.with_stretch_straight(t),
        _ => base.with_pitch(t),
    }
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
        // --- Tab: a 180° ABOUT-FACE that visibly turns the hull (#75). Pre-#75
        // this toggled orientation BowOn↔Broadside only, which — now that render +
        // the fire-gate key off FACING — left the hull motionless (Bruce: "Tab does
        // nothing to the ship"). Tab now reverses the bow by rotating FACING 180°
        // (two RotateRight quarter-turns: N↔S, E↔W), reusing the same facing-driven
        // REORIENT path as the Q/E quarter-turn rotates; orientation is re-derived
        // from the new facing inside the resolver. Distinct from Q/E (90°) — Tab is
        // the quick reverse. Bin-local; the static `__reorient_flip` synthetic's
        // own effect is overridden here (its REORIENT::Flip stays orientation-only
        // for the class Signatures that use it).
        Intent::ReorientFlip => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            let Some(mut action) = content.action(id).cloned() else {
                return false;
            };
            // Two clockwise quarter-turns = a 180° about-face of facing.
            action.effects = vec![
                Effect::REORIENT { to: ReorientTo::RotateRight },
                Effect::REORIENT { to: ReorientTo::RotateRight },
            ];
            apply_instant_action(&player_id, &action, board, content);
            // (#126) TURN-BASED (chess): a player turn-action advances the world
            // EXACTLY ONE turn. The 180° about-face is one such action, so the
            // world phase runs once after it lands.
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
        // (#75) RotateLeft/RotateRight join this arm: they map to the registered
        // __rotate_left / __rotate_right synthetics (REORIENT::RotateLeft/Right),
        // which turn the player's FACING ±90 and re-derive orientation — the hull
        // rotates on screen + the firing arcs follow (both key off facing).
        Intent::MoveLeft
        | Intent::MoveRight
        | Intent::MoveUp
        | Intent::MoveDown
        | Intent::RotateLeft
        | Intent::RotateRight
        | Intent::Vent => {
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
            // (#126) TURN-BASED: a move / rotate / vent is one turn-action, so the
            // world advances exactly one turn after the player's action lands.
            run_world_phase(board, content);
            true
        }

        // --- PlayCard (#126): FIELD-KIT CARDS ARE FREE. Validate + decrement charges
        // via try_play_card, then run the synthetic `__card_<id>` Action INSTANTLY —
        // and do NOT advance the world. Playing a field-kit card (keys 5/6/7) is a
        // free action in the turn-based model: it costs no turn, ticks no cooldowns,
        // and gives the enemies no action. This is the one EXCEPTION to "every player
        // action advances one turn"; all other inputs (move/queue/commit/wait) call
        // run_world_phase exactly once. ---
        Intent::PlayCard(card_id) => {
            match content.try_play_card(&player_id, &card_id) {
                PlayResult::Played => {
                    let synth_id = synthetic_card_action_id(&card_id);
                    let Some(action) = content.action(&synth_id).cloned() else {
                        // Charges decremented but the synthetic isn't registered —
                        // nothing more to do (cards are free: no world phase either way).
                        return true;
                    };
                    apply_instant_action(&player_id, &action, board, content);
                    true
                }
                PlayResult::UnknownCard
                | PlayResult::NotCarried
                | PlayResult::InsufficientCharges => false,
            }
        }

        // --- QueueAction (#126): push the action id to the player's queue, then
        // advance the world one turn. In the turn-based (chess) model, lining up a
        // weapon (keys 1/2/3) IS a turn-action — it costs a turn (reverting #97's
        // "queue is free"). The shot fires when the player COMMITS (Space) on a
        // later turn; queuing just spends this turn to load it. A failed queue
        // (no such mount) advances nothing. ---
        Intent::QueueAction(_) => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            if append_to_player_queue(board, id.to_string()) {
                run_world_phase(board, content);
                true
            } else {
                false
            }
        }

        // --- Wait (#126): pass the turn. The player takes no action of their own;
        // the world simply advances one turn (ordnance steps, every enemy takes its
        // one action, cooldowns/shield-regen tick). "Hold position, let them move." ---
        Intent::Wait => {
            run_world_phase(board, content);
            true
        }

        // --- CommitTurn (#126): fire the player's queued shots, THEN advance the
        // world one turn. Space = "fire what I lined up + end my turn." An empty
        // queue still spends the turn (a no-op "hold" that lets the world move) —
        // identical to Wait when nothing is queued. The player's queue fires FIRST,
        // then run_world_phase advances ordnance + every enemy's one action + the
        // end-of-turn cooldown/shield-regen tick, matching resolve_round's order
        // (fire_player_queue then run_world_phase). ---
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

/// [`fresh_content`] PLUS the loaded catalog's actions merged in (#49a), so the
/// resolver's fire path can resolve the catalog weapon ids that
/// catalog-synthesized enemies mount (beam_cannon, railgun_broadside, …) —
/// otherwise `content.action(id)` is `None` and enemies never fire. The catalog
/// actions already carry 2-D bands (derived at load); merge is insert-if-absent
/// so the hand-tuned player weapons keep precedence. `None` catalog (load
/// failed / headless) → just the demo content, unchanged.
fn build_content(catalog: Option<&broadside_engine::types::Catalog>) -> DemoContent {
    let mut c = fresh_content();
    if let Some(cat) = catalog {
        c.install_catalog_actions(cat);
    }
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

/// The damage figure shown on an action's tile (`0` = non-damage).
///
/// Reads the action's own first `DAMAGE` effect; but an ORDNANCE action
/// (`SPAWN_ORDNANCE`, e.g. torpedo / missile) carries NO direct DAMAGE — its
/// damage lives on the spawned projectile's payload (#117 bug: tile 2 read 0
/// because action_damage only looked at the action's own effects). So when the
/// action has no direct DAMAGE but DOES spawn ordnance, ask `content` to build
/// the projectile (`spawn_projectile`, the same call the resolver uses) and read
/// the DAMAGE amount off its `payload`. `ship` is the owner the spawn keys off.
fn action_damage(
    action: &broadside_engine::types::Action,
    content: &dyn Content,
    ship: &Ship,
) -> i32 {
    // Direct DAMAGE on the action itself (beams, broadsides, …).
    if let Some(amount) = action.effects.iter().find_map(|e| match e {
        Effect::DAMAGE { amount, .. } => Some(*amount),
        _ => None,
    }) {
        return amount;
    }
    // Otherwise, an ordnance action: its damage is on the spawned projectile.
    if let Some(kind) = action.effects.iter().find_map(|e| match e {
        Effect::SPAWN_ORDNANCE { projectile } => Some(projectile.as_str()),
        _ => None,
    }) {
        let proj = content.spawn_projectile(kind, ship);
        return proj
            .payload
            .iter()
            .find_map(|e| match e {
                Effect::DAMAGE { amount, .. } => Some(*amount),
                _ => None,
            })
            .unwrap_or(0);
    }
    0
}

/// (#98) The action's RANGE in cells for the tile readout = the MAX band it can
/// fire at, mapped Adjacent=1 / Near=2 / Far=3. `0` when the action has no range
/// band (non-targeted / self) → the tile shows no range number.
fn action_range(action: &broadside_engine::types::Action) -> i32 {
    use broadside_engine::grid::Range;
    action
        .targeting
        .range_band
        .iter()
        .map(|r| match r {
            Range::Adjacent => 1,
            Range::Near => 2,
            Range::Far => 3,
        })
        .max()
        .unwrap_or(0)
}

/// `Some(position)` of `action_id` in `ship.queue`, else `None`.
fn queue_index(ship: &Ship, action_id: &str) -> Option<usize> {
    ship.queue.iter().position(|q| q == action_id)
}

/// (#100/#102) Whether `action`, fired by `ship` from its CURRENT pos/facing,
/// would hit anything — the fire-gate single-source `resolve_targeting_2d`.
/// Drives the tile's "no target / can't bear" cue.
///
/// (#102 fix) The can't-bear cue only makes sense for an AIMED weapon — one that
/// bears on a target cell via an arc/line. A non-aimed action (a SELF buff or a
/// DEPLOYED_CELL placement, i.e. the field-kit utility cards mass_lock /
/// mass_breach / sensor_pulse) has no "does it bear" concept; `resolve_targeting_2d`
/// returns empty for it by construction, which previously veiled + slashed those
/// card tiles ("what is the slash through 5?"). So such actions ALWAYS read as
/// fireable — the veil never applies to a utility/self ability, only to a weapon
/// that genuinely can't bring its arc onto an enemy from here.
fn action_can_fire(action: &broadside_engine::types::Action, board: &Board, ship: &Ship) -> bool {
    use broadside_engine::types::TargetingPattern;
    if matches!(action.targeting.pattern, TargetingPattern::SELF | TargetingPattern::DEPLOYED_CELL) {
        return true;
    }
    !broadside_engine::resolve::resolve_targeting_2d(action, board, ship.pos).is_empty()
}

/// (#108) One-letter firing-arc tag for a mount's [`Arc`], drawn on its ability
/// tile so the player can tell a SIDE weapon from a forward one without firing:
/// `F` Forward, `B` Broadside, `T` Turret, `R` Rear.
fn arc_letter(arc: broadside_engine::types::Arc) -> char {
    use broadside_engine::types::Arc;
    match arc {
        Arc::Forward => 'F',
        Arc::BroadsideArc => 'B',
        Arc::Turret => 'T',
        Arc::Rear => 'R',
    }
}

/// Build one ship's ability tiles (mounts → 1/2/3, cards → 5/6/7). `icon` /
/// `damage` / `range` / `cooldown_max` come from the action def; `cooldown` from
/// the ship; `queued_index` from the ship's queue; `can_fire` from the fire-gate
/// against `board` at the ship's current pos/facing (#100).
fn build_ship_tiles(ship: &Ship, content: &dyn Content, board: &Board) -> Vec<hud::AbilityTile> {
    let mut tiles = Vec::new();
    for (i, mount) in ship.mounts.iter().take(3).enumerate() {
        if let Some(action) = content.action(&mount.weapon) {
            tiles.push(hud::AbilityTile {
                slot: (b'1' + i as u8) as char,
                icon: archetype_icon(action.archetype),
                damage: action_damage(action, content, ship),
                range: action_range(action),
                cooldown: ship.cooldowns.get(&mount.weapon).copied().unwrap_or(0).max(0),
                cooldown_max: action.cost.cooldown_max.max(0),
                queued_index: queue_index(ship, &mount.weapon),
                can_fire: action_can_fire(action, board, ship),
                // (#108) Firing-arc letter from the mount so the player can tell a
                // SIDE weapon (key 3) from a forward one at a glance.
                arc: Some(arc_letter(mount.arc)),
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
                    damage: action_damage(action, content, ship),
                    range: action_range(action),
                    cooldown: 0,
                    cooldown_max: 0,
                    queued_index: queue_index(ship, &synth),
                    // Cards (SELF/support) aren't position-gated — always "fireable".
                    can_fire: true,
                    // Cards have no firing arc — skip the side indicator.
                    arc: None,
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

    // (#54/#55) Lay the demo fleet out on the REAL 5×4 grid, bow-to-bow, matching
    // the live runs::build_encounter_board layout: player front-centre (2,3)
    // facing N (into the board, toward the enemies); enemies fanned centre-out
    // across the back row (2,0),(1,0),(3,0) facing S (toward the player). Each
    // ship's pos/facing now drive the 2-D render (was all-Pos(0,0)/Bow(S) →
    // stacked + uniform-broadside). Each enemy gets one pulse_laser so the AI has
    // something to queue (else decide_enemy_action returns nothing and it reads
    // inert).
    use broadside_engine::grid::{Dir4, Facing, Pos, COLS, ROWS};
    let bow_n = Facing::Bow(Dir4::N);
    let bow_s = Facing::Bow(Dir4::S);
    let mid = COLS / 2;
    let place = |cells: &mut Vec<Option<Ship>>, s: Ship| {
        let idx = s.pos.to_index();
        cells[idx] = Some(s);
    };
    place(&mut cells, player_ship(Pos::new(mid, ROWS - 1), bow_n));
    place(&mut cells, enemy_ship("enemy-2", Pos::new(mid, 0), bow_s));
    place(&mut cells, enemy_ship("enemy-3", Pos::new(mid - 1, 0), bow_s));
    place(&mut cells, enemy_ship("enemy-5", Pos::new(mid + 1, 0), bow_s));

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

fn player_ship(pos: broadside_engine::grid::Pos, facing: broadside_engine::grid::Facing) -> Ship {
    let mut player = make_ship("player", Faction::Player, pos, facing);
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
fn enemy_ship(id: &str, pos: broadside_engine::grid::Pos, facing: broadside_engine::grid::Facing) -> Ship {
    let mut e = make_ship(id, Faction::Enemy, pos, facing);
    e.mounts = vec![Mount {
        id: "m1".into(),
        arc: TArc::Forward,
        weapon: "pulse_laser".into(),
    }];
    e
}

fn make_ship(
    id: &str,
    faction: Faction,
    pos: broadside_engine::grid::Pos,
    facing: broadside_engine::grid::Facing,
) -> Ship {
    // (#54/#55) The startup/Restart demo board now places ships at REAL 2-D
    // positions + facings (like the live build_encounter_board) — previously
    // make_ship hardcoded Pos(0,0) + Bow(S) for every ship, so the whole demo
    // fleet stacked in the back-left cell with one uniform facing: the player
    // ship looked "gone" (jammed under the enemies) and everything read
    // "broadside". The legacy 1-D cell/orientation are derived to stay roughly
    // consistent for any remaining 1-D reader during the EXPAND→CONTRACT window.
    use broadside_engine::grid::{Dir4, Facing};
    let cell = pos.to_index();
    let orientation = match facing {
        Facing::Bow(Dir4::N) => Orientation::BowOn { bow: LaneEnd::Fore },
        Facing::Bow(Dir4::S) => Orientation::BowOn { bow: LaneEnd::Aft },
        _ => Orientation::Broadside,
    };
    Ship {
        id: id.into(),
        faction,
        cell,
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
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/* =============================================================================
 * App + event loop.
 * ========================================================================== */

/// Duration of the per-ship snap → smooth lerp after a turn-advancing
/// input. ~200ms reads as crisp without feeling laggy at 60Hz.
const TWEEN_DURATION_MS: u32 = 120;

/// (#90 kill-burst) How long a ship-destruction burst stays on screen after the
/// killing round. ~0.35s reads as a clear flash without lingering.
const KILL_BURST_SECS: f32 = 0.35;
/// (#101) Lifetime of a per-ship hull-drop flash on its lane bar. Short enough to
/// read as a "pop" on the round it happened, long enough to catch the eye.
const HULL_FLASH_SECS: f32 = 0.45;
/// (#119) Tint of the ship-death explosion particle burst — hot orange-white, so
/// the spray reads as fire/debris against the dark starfield.
const EXPLOSION_PARTICLE_COLOR: [f32; 4] = [1.0, 0.72, 0.32, 1.0];
/// (#133 Bruce) The "beat" between the player's queued ABILITIES landing on a
/// commit. When the player fires a multi-weapon volley, each weapon's beam +
/// impact + damage number is revealed ONE AT A TIME with this pause between, so
/// each hit reads distinctly instead of all landing on one frame. This is an
/// in-turn VISUAL playback pause — the turn-based model is preserved, input is
/// just locked for the brief playback. Tunable (Bruce dials the feel).
const BEAT_SECS: f32 = 0.5;

/// (#79) Per-ship "where + which way was this ship before the move, and when did
/// the slide/turn begin?" anchor. Recorded by `App::record_tween_anchors` after
/// each input mutation; consumed by `App::tween_2d` each frame to ease the
/// rendered position + facing-yaw from the pre-move state to the new logical
/// cell/facing over `TWEEN_DURATION_MS`, so the ship SLIDES + ROTATES instead of
/// snapping. (Was a 1-D fractional `from_cell` feeding the dead 1-D
/// `compose_scene_tweened`; now 2-D `Pos`/`Facing` feeding `compose_scene_2d_tweened`.)
struct TweenAnchor {
    /// The ship's grid cell BEFORE the move (the slide's start).
    from_pos: Pos,
    /// The ship's facing BEFORE the move (the turn's start) — for the rotation
    /// tween (shortest-path yaw lerp).
    from_facing: Facing,
    /// When the input fired. Elapsed > TWEEN_DURATION_MS ⇒ resolved + evictable.
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

/// (#133 Bruce) In-turn BEAT playback of the player's committed volley. On a
/// CommitTurn the resolver fires the whole queue atomically (all beams + hull
/// drops land at once); to make each ability read distinctly we DRAIN the player's
/// fire-events off the board into here and release them ONE AT A TIME, BEAT_SECS
/// apart, each re-pushed onto `board.fire_events` so both beam-render paths animate
/// it + an impact/number recorded at that moment. Input is LOCKED while this is
/// `Some` (the turn-based model holds — input is just suspended for the brief
/// playback). Enemy fire stays immediate (scope: player-volley-only).
struct BeatPlayback {
    /// Player fire-events not yet revealed, in fire (queue) order.
    pending: std::collections::VecDeque<broadside_engine::types::FireEvent>,
    /// When to release the next beat.
    next_at: Instant,
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
    /// (#90 kill-burst) Destruction bursts pending render: the BOARD cell a ship
    /// died on + when it died. Recorded ONLY on a combat-turn resolve (a ship id
    /// present pre-turn but gone after) — never on a board reset / restart / encounter
    /// rebuild, where all ids vanish at once (that would burst the whole board). The
    /// redraw prunes entries older than [`KILL_BURST_SECS`] and draws the rest via
    /// `hud::push_destruction_at`. Resolver removes a dead ship same-action
    /// (`destroy()` → `cells[c].take()`), so a hull<=0 ship never survives to a
    /// frame — this prev-vs-current diff is the renderer-side death signal (the 2-D
    /// analog of `vfx::CombatVfx`'s vanish detection).
    kill_bursts: Vec<(Pos, Instant)>,
    /// (#119) Procedural particle pool — screen-space sprays for combat juice.
    /// Phase 1: a ship-death EXPLOSION burst spawned at the projected kill cell
    /// when an id vanishes (same trigger as `kill_bursts`). Advanced each frame +
    /// emitted into the draw list; cleared on restart. Later phases (muzzle flash,
    /// impact debris) reuse the same pool.
    particles: broadside_engine::vfx::ParticlePool,
    /// (#101/#106) Per-ship hull-DROP record: ship id -> (amount lost, when).
    /// Recorded on a combat resolve by diffing pre-vs-current hull (the 2-D analog
    /// of the player's `hit_flash`, but for EVERY ship). The redraw fades each entry
    /// over `HULL_FLASH_SECS` and drives BOTH `hud::push_hull_flash_2d` (the bar
    /// flash, #101 — so even a 1-2 drop pops while damage balance is tuned) AND
    /// `hud::push_damage_number_2d` (the floating amount, #106). Pruned on expiry;
    /// cleared on restart so a fresh board shows no stale flashes/numbers.
    hull_flash: std::collections::HashMap<String, (i32, Instant)>,
    /// (#133) Active in-turn beat playback of the player's committed volley, or
    /// `None` when idle. While `Some`, gameplay input is locked and the redraw loop
    /// releases one queued beam per `BEAT_SECS` off the frame clock.
    beat_playback: Option<BeatPlayback>,
    /// (#136) "Can't queue — recharging" cue: the weapon id the player just tried to
    /// queue while it was ON COOLDOWN + when. Set in the keypress handler when a
    /// QueueAction is blocked; the redraw flashes that ability tile for a short fade
    /// so the block reads as "still cooling down", not a silent no-op. Pruned on
    /// expiry; cleared on restart.
    queue_blocked_flash: Option<(String, Instant)>,
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
        // #49a: merge the loaded catalog's (2-D-band-derived) actions into the
        // firing content so catalog-synthesized ENEMY weapons (beam_cannon,
        // railgun_broadside, …) resolve — otherwise the enemy AI's fire-gate
        // skips them (content.action(id) = None) and enemies never shoot.
        // insert-if-absent: the hand-tuned player weapons keep precedence.
        let content = build_content(catalog.as_ref());
        #[allow(unused_mut)]
        let mut app = Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane: demo_lane(),
            content,
            catalog,
            tween_anchors: HashMap::new(),
            sectors,
            run: Run::new(Self::fresh_player_ship()),
            demo_state: DemoState::Playing,
            vfx: broadside_engine::vfx::CombatVfx::new(),
            ability_hud: broadside_engine::hud::AbilityHud::new(),
            frame_clock: 0.0,
            player_hull_prev: None,
            hit_flash: 0.0,
            kill_bursts: Vec::new(),
            particles: broadside_engine::vfx::ParticlePool::new(),
            hull_flash: std::collections::HashMap::new(),
            beat_playback: None,
            queue_blocked_flash: None,
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
        // Front-centre, bow N — the campaign start pose. build_encounter_board
        // re-stamps pos/facing from player_start_pos()/player_spawn_facing()
        // anyway, but this keeps fresh_player_ship correct on its own.
        use broadside_engine::grid::{Dir4, Facing, Pos, COLS, ROWS};
        player_ship(Pos::new(COLS / 2, ROWS - 1), Facing::Bow(Dir4::N))
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
        // #49a: re-merge the catalog actions so enemy weapons resolve after a
        // restart too (same wiring as startup).
        self.content = build_content(self.catalog.as_ref());
        self.board = self
            .build_current_board()
            .unwrap_or_else(render_example_board);
        self.demo_state = DemoState::Playing;
        self.tween_anchors.clear();
        self.kill_bursts.clear(); // (#90) no stale bursts into the fresh board
        self.particles.clear(); // (#119) no stale explosion particles into the fresh board
        self.hull_flash.clear(); // (#101) no stale damage flashes into the fresh board
        self.beat_playback = None; // (#133) abort any in-flight volley playback on restart
        self.queue_blocked_flash = None; // (#136) clear any recharging cue on restart
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
    /// (#79) Snapshot each ship's pre-mutation grid pos + facing, so the tween
    /// anchor planted after `apply_intent` slides/turns FROM where the ship was.
    /// (A ship already mid-slide re-anchors from its logical pre-move cell — a
    /// negligible snap on a rare double-tap; moves are discrete turns.)
    fn snapshot_pos_facing(&self) -> HashMap<String, (Pos, Facing)> {
        let mut out = HashMap::with_capacity(self.board.cells.len());
        for ship in self.board.cells.iter().flatten() {
            out.insert(ship.id.clone(), (ship.pos, ship.facing));
        }
        out
    }

    /// Record fresh tween anchors after `apply_intent` ran: for every ship whose
    /// logical pos OR facing changed vs its pre-mutation snapshot, plant an
    /// anchor at the OLD pos/facing so the next frames interpolate from there.
    fn record_tween_anchors(&mut self, prev: &HashMap<String, (Pos, Facing)>, now: Instant) {
        // Drop anchors for ships that no longer exist (destroyed / Restart).
        self.tween_anchors
            .retain(|id, _| self.board.cells.iter().flatten().any(|s| &s.id == id));
        for ship in self.board.cells.iter().flatten() {
            let Some(&(from_pos, from_facing)) = prev.get(&ship.id) else { continue };
            if from_pos == ship.pos && from_facing == ship.facing {
                // Nothing moved/turned — no tween needed.
                self.tween_anchors.remove(&ship.id);
                continue;
            }
            self.tween_anchors.insert(
                ship.id.clone(),
                TweenAnchor { from_pos, from_facing, started_at: now },
            );
        }
    }

    /// (#79) Compute this frame's per-ship visual tween overrides. Each in-flight
    /// anchor eases `from`→`current` over `TWEEN_DURATION_MS` (ease-out quad):
    /// position = lerp of the two cells' projected `CellQuad`s (slides along the
    /// perspective), facing-yaw = shortest-path angular lerp (turns smoothly).
    /// Expired/absent ⇒ no entry ⇒ that ship snaps to its logical cell.
    fn tween_2d(&self, cfg: &broadside_engine::projector::ProjectorConfig, now: Instant) -> hud::Tween2d {
        use broadside_engine::projector::grid_cell_quad;
        let dur_ms = TWEEN_DURATION_MS as f32;
        let mut tw = hud::Tween2d::default();
        for ship in self.board.cells.iter().flatten() {
            let Some(anchor) = self.tween_anchors.get(&ship.id) else { continue };
            let elapsed = now.duration_since(anchor.started_at).as_secs_f32() * 1000.0;
            let t = (elapsed / dur_ms).clamp(0.0, 1.0);
            // Ease-out quad: 1 - (1 - t)^2 — crisp departure, soft arrival.
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            let from_q = grid_cell_quad(anchor.from_pos, cfg);
            let to_q = grid_cell_quad(ship.pos, cfg);
            let q = hud::lerp_cell_quad(&from_q, &to_q, eased);
            tw.visual.insert(
                ship.id.clone(),
                hud::VisualShip2d {
                    center: q.center,
                    // (#80) cell near (bottom) edge y — the loft hero seats here +
                    // follows the lane on a move. corners[3] = bottom-left.
                    near_edge_y: q.corners[3][1],
                    near_edge_width: q.near_edge_width(),
                    depth_scale: q.depth_scale,
                    facing_yaw_deg: hud::lerp_facing_yaw_deg(anchor.from_facing, ship.facing, eased),
                },
            );
        }
        tw
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
        // (#70) PLAYER = the faithful Aegis, live-3D. Install the GLB the tool
        // baked (Aegis.glb, per the v5 render contract) as the player loft mesh:
        // mesh_import → upload_imported keeps the authored materials + the unlit
        // cyan engine glow. push_ship_2d emits a LoftShip for the player when this
        // is installed (the loft 3D pass renders it lit, chase-cam posed, then
        // blits into the lane), else falls back to the sprite/flat-box. The HUD +
        // telegraph overlays are unaffected.
        const AEGIS_GLB: &[u8] = include_bytes!("../../assets/ships/Aegis.glb");
        match gfx.install_player_glb(AEGIS_GLB) {
            Ok(()) => log::info!("loft: player Aegis hull installed from Aegis.glb ({} bytes)", AEGIS_GLB.len()),
            Err(e) => log::warn!("loft: Aegis.glb import failed ({e}); player falls back to sprite/flat-box"),
        }
        // (#89) ENEMIES = the SAME Aegis hull, RED-tinted (Bruce): every enemy
        // renders as the player's ship-class in a hostile colour instead of the
        // flat CAD box. Enemies face the player (bow-on), so the hull renders
        // oncoming. loft_kind prefers EnemyLoft once this is installed.
        match gfx.install_enemy_glb(AEGIS_GLB) {
            Ok(()) => log::info!("loft: enemy Aegis hull (steel-grey) installed from Aegis.glb"),
            Err(e) => log::warn!("loft: enemy Aegis.glb import failed ({e}); enemies fall back to CAD/2D"),
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
                // (#76) `,` / `.` cycle the SHIP loft-render resolution LIVE
                // (chunky <-> crisp ship pixels); `;` / `'` cycle the WHOLE-SCENE
                // (offscreen) resolution LIVE (everything — background, lanes,
                // ships, HUD — gets chunkier/finer together). Both are renderer-
                // owned bindings, handled before the content key map as raw
                // KeyCodes (same pattern as the old `[`/`]` camera control).
                if let Some(gfx) = self.gfx.as_mut() {
                    if code == KeyCode::Comma {
                        let (w, h) = gfx.cycle_loft_res(false);
                        log::info!("ship res: {w}x{h}");
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                    if code == KeyCode::Period {
                        let (w, h) = gfx.cycle_loft_res(true);
                        log::info!("ship res: {w}x{h}");
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                    // `;` = previous scene res, `'` = next. The gfx side recreates
                    // the offscreen + view + blit; the render path below rebuilds the
                    // projector via `for_scene(scene_w, scene_h)` so the lane geometry
                    // reprojects to the new canvas.
                    if code == KeyCode::Semicolon {
                        let (w, h) = gfx.cycle_scene_res(false);
                        log::info!("scene res: {w}x{h}");
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                    if code == KeyCode::Quote {
                        let (w, h) = gfx.cycle_scene_res(true);
                        log::info!("scene res: {w}x{h}");
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                    // (#139) `G` cycles the GRID PITCH toward top-down (constant grid
                    // depth — the projector compensates the horizon). Renderer-owned
                    // raw binding like the res cycles; everything projector-derived
                    // (grid/cells/movement/threats/ordnance) reprojects via
                    // scene_projector(). (#140) The loft player + enemy hulls TILT with
                    // the plane via the live loft-camera pitch (gfx::loft_pitch_deg), so
                    // they follow the arc toward top-down.
                    if code == KeyCode::KeyG {
                        let step = broadside_engine::gfx::cycle_grid_pitch();
                        log::info!("grid pitch step: {step}/{}", broadside_engine::gfx::GRID_PITCH_STEPS);
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                    // (#140/#142) `T` cycles the GRID MODE: drawbridge (constant
                    // footprint) -> stretch-curved (uniform top-down square, bowed edges)
                    // -> stretch-straight (same stretch, STRAIGHT edges) -> back. The G
                    // pitch step drives the arc within the active mode.
                    if code == KeyCode::KeyT {
                        let mode = broadside_engine::gfx::cycle_grid_mode();
                        let name = match mode { 1 => "stretch-curved", 2 => "stretch-straight", _ => "drawbridge" };
                        log::info!("grid mode: {mode} ({name})");
                        if let Some(win) = self.window.as_ref() { win.request_redraw(); }
                        return;
                    }
                }
                let Some(key) = keycode_to_key(code) else { return };

                // (#133) Lock gameplay input while the player's committed volley is
                // playing out beat-by-beat. The turn-based model holds — input is just
                // suspended for the brief in-turn playback (Esc + res cycles above are
                // already handled, so they still work). Unlocks when the last beat lands.
                if self.beat_playback.is_some() {
                    return;
                }

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

                // (#97 Enter footgun) During active Playing combat, Enter is a NO-OP.
                // It used to map to Intent::Restart (via key_to_intent) and rebuild
                // the board MID-FIGHT — Bruce hit Enter expecting "commit" and nuked
                // his run (enemies reset to full hull = "health never goes down").
                // Restart is now reachable ONLY from the end-state overlays (the
                // RunComplete / RunDefeated arms above). Commit-turn stays on
                // Space / R; Enter does nothing while fighting.
                if key == Key::Enter {
                    return;
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
                // (#136 Bruce) COOLDOWN GATE on queuing. Per the core model, queuing a
                // weapon requires it OFF cooldown (the cooldown starts when it FIRES).
                // The player could previously re-queue an on-cooldown weapon (bug). Read
                // the cooldown straight off the player snapshot (no content dep): block
                // the QueueAction if cooldowns[weapon] > 0, and set a brief "recharging"
                // cue so the block reads (the redraw flashes that tile) instead of a
                // silent no-op. Blocking spends NO turn — it's an invalid input, not a
                // wasted move.
                if let Intent::QueueAction(ref weapon) = intent {
                    if player.cooldowns.get(weapon).copied().unwrap_or(0) > 0 {
                        self.queue_blocked_flash = Some((weapon.clone(), Instant::now()));
                        if let Some(w) = self.window.as_ref() { w.request_redraw(); }
                        return;
                    }
                }
                // Restart resets both the board AND the content so card
                // charges + subsystems come back as a fresh game.
                let is_restart = matches!(intent, Intent::Restart);
                // (#133) Only a CommitTurn fires the player's queued volley — the beat
                // playback applies to that, not to a move/queue/wait/card.
                let was_commit = matches!(intent, Intent::CommitTurn);
                // Snapshot per-ship visual positions BEFORE mutating, so
                // the tween anchor points at where each ship was already
                // rendering (not its logical pre-mutation cell, which
                // may itself be mid-tween).
                let now = Instant::now();
                let prev_visual = self.snapshot_pos_facing();
                // (#101) Pre-turn hull per ship id, so we can flash any ship whose
                // hull DROPS this resolve (the bar damage-flash). Cheap clone of
                // (id -> hull); diffed against the post-resolve board below.
                let prev_hulls: std::collections::HashMap<String, i32> = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .map(|s| (s.id.clone(), s.hull))
                    .collect();
                // (#132) Ordnance ids present BEFORE this turn, so we can spawn a LAUNCH
                // cue for any NEW player-owned projectile that appears (a torpedo just
                // left the tube). Diffed against the post-resolve ordnance below.
                let prev_ordnance: std::collections::HashSet<String> = self
                    .board
                    .ordnance
                    .iter()
                    .map(|p| p.id.clone())
                    .collect();
                let changed = apply_intent(intent, &mut self.board, &mut self.content, &render_example_board);
                if is_restart {
                    self.restart_run();
                } else if changed {
                    self.record_tween_anchors(&prev_visual, now);
                    // (#90 kill-burst) Any ship id present BEFORE this combat turn
                    // but gone AFTER was destroyed this round — record a burst at its
                    // last-known cell (the resolver removed it same-action, so it
                    // never survives to a frame for a hull<=0 check). GUARDED to the
                    // combat-turn path (`changed && !is_restart`): a board reset /
                    // restart / encounter rebuild vanishes every id at once and goes
                    // through `restart_run()` / a fresh board, NOT here — so the whole
                    // board can never burst at once (the lead's guard).
                    for id in prev_visual.keys() {
                        if !self.board.cells.iter().flatten().any(|s| &s.id == id) {
                            if let Some(&(pos, _)) = prev_visual.get(id) {
                                self.kill_bursts.push((pos, now));
                                // (#119) Spawn an EXPLOSION particle burst at the
                                // dead ship's PROJECTED screen position (same kill
                                // signal). Once per death (we're in the resolve
                                // branch, not per-frame), so it doesn't re-seed.
                                let pcfg = scene_projector();
                                let c = broadside_engine::projector::grid_cell_quad(pos, &pcfg).center;
                                self.particles.spawn_burst(c, 22, EXPLOSION_PARTICLE_COLOR, 0.55);
                            }
                        }
                    }
                    // (#101) Any SURVIVING ship whose hull fell this resolve gets a
                    // damage-flash on its lane bar (a destroyed ship already gets the
                    // kill burst above, so skip ids no longer on the board). Drives
                    // hud::push_hull_flash_2d in the overlay pass so even a 1-2 drop
                    // pops — the legibility win while damage balance gets tuned.
                    for ship in self.board.cells.iter().flatten() {
                        if let Some(&prev_hull) = prev_hulls.get(&ship.id) {
                            if ship.hull < prev_hull {
                                // Record the amount lost (#106) + timestamp (#101).
                                self.hull_flash.insert(ship.id.clone(), (prev_hull - ship.hull, now));
                            }
                        }
                    }
                    // (#132) LAUNCH CUE: any NEW player-owned projectile this turn (a
                    // torpedo just left the tube) gets a small burst at its current cell
                    // — so the player SEES the ordnance launch on the commit turn, not
                    // just a mystery hit when it lands a turn later. push_ordnance_2d
                    // then draws it travelling each subsequent turn.
                    let pcfg = scene_projector();
                    for proj in &self.board.ordnance {
                        if proj.owner_faction == Faction::Player && !prev_ordnance.contains(&proj.id) {
                            let c = broadside_engine::projector::grid_cell_quad(proj.pos, &pcfg).center;
                            self.particles.spawn_burst(c, 12, EXPLOSION_PARTICLE_COLOR, 0.30);
                        }
                    }
                    // (#133) BEAT PLAYBACK: when the player COMMITS a volley of 2+
                    // weapons, the resolver fired them all atomically — every beam landed
                    // on this frame. Stagger them: keep the FIRST player beam (+ all enemy
                    // beams) on the board to draw now, and DRAIN the rest into beat_playback
                    // to release one per BEAT_SECS (the redraw driver re-pushes each onto
                    // board.fire_events so both beam paths animate it). Input is locked
                    // while the playback runs. A single-beam (or no-beam) commit is left
                    // untouched — no beat needed. fire_player_queue produces fire_events in
                    // queue order, so draining preserves fire order.
                    if was_commit {
                        let player_beams: Vec<usize> = self
                            .board
                            .fire_events
                            .iter()
                            .enumerate()
                            .filter(|(_, fe)| fe.attacker_faction == Faction::Player)
                            .map(|(i, _)| i)
                            .collect();
                        if player_beams.len() >= 2 {
                            // Drain all player beams EXCEPT the first into the playback,
                            // in order; the first stays on the board to fire this frame.
                            let mut pending = std::collections::VecDeque::new();
                            // Walk from the back so removing by index stays valid; collect
                            // in reverse then restore order.
                            let mut to_remove: Vec<usize> = player_beams[1..].to_vec();
                            to_remove.sort_unstable_by(|a, b| b.cmp(a)); // descending
                            let mut drained: Vec<broadside_engine::types::FireEvent> = Vec::new();
                            for idx in to_remove {
                                drained.push(self.board.fire_events.remove(idx));
                            }
                            drained.reverse(); // back to fire order
                            for fe in drained {
                                pending.push_back(fe);
                            }
                            self.beat_playback = Some(BeatPlayback {
                                pending,
                                next_at: now + std::time::Duration::from_secs_f32(BEAT_SECS),
                            });
                        }
                    }
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
                // (#133) BEAT driver: while a committed volley is playing out, release
                // the next queued player beam every BEAT_SECS. Each released beam is
                // pushed onto board.fire_events (both beam-render paths animate the new
                // event, since its signature changes) + gets an impact spark and a
                // damage-number/flash on its target at THIS moment — so the volley reads
                // as distinct hits with the beat between. Input stays locked until the
                // queue empties (then beat_playback -> None unlocks it). Only fires while
                // Playing; an overlay freezes it.
                if self.demo_state == DemoState::Playing {
                    // Pop the next due beam WITHOUT holding a borrow across the
                    // self.particles / self.board accesses below (disjoint-field borrow
                    // would otherwise alias). due = the FireEvent to reveal this frame.
                    let due: Option<broadside_engine::types::FireEvent> = match self.beat_playback.as_mut() {
                        Some(pb) if now >= pb.next_at => {
                            let fe = pb.pending.pop_front();
                            pb.next_at = now + std::time::Duration::from_secs_f32(BEAT_SECS);
                            fe
                        }
                        _ => None,
                    };
                    if let Some(fe) = due {
                        // Impact spark on the struck cell, timed to the beat.
                        if fe.hit {
                            let pcfg = scene_projector();
                            let c = broadside_engine::projector::grid_cell_quad(fe.to_pos, &pcfg).center;
                            self.particles.spawn_burst(c, 8, EXPLOSION_PARTICLE_COLOR, 0.25);
                        }
                        // Re-push the beam so it draws + animates this frame.
                        self.board.fire_events.push(fe);
                    }
                    // Empty queue -> playback done, unlock input.
                    if self.beat_playback.as_ref().map(|p| p.pending.is_empty()).unwrap_or(false) {
                        self.beat_playback = None;
                    }
                }
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
                // Combat juice (#51): diff the board for this frame (spawns
                // hit/explosion/trail/beam effects), then advance lifetimes by a
                // fixed ~60 Hz dt. observe() is read-only over the board and
                // idempotent on unchanged frames, so running it every redraw is
                // safe — it only spawns on an actual state change.
                self.vfx.observe(&self.board);
                let vfx_active = self.vfx.advance(1.0 / 60.0);
                // (#119) Advance the explosion particle pool at the same fixed dt;
                // stays empty (cheap no-op) until a ship death seeds a burst.
                let particles_active = self.particles.advance(1.0 / 60.0);
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
                    .map(|p| build_ship_tiles(p, &self.content, &self.board))
                    .unwrap_or_default();
                let ability_active = self.ability_hud.advance(&player_tiles, 1.0 / 60.0);
                // (#122/#123) Player targeting telegraph: for each weapon the player
                // has QUEUED, resolve the cells it would strike from the current pose
                // (resolve_targeting_2d — the same single source the shot fires
                // through). Collect the union of target cells (the cyan preview) and
                // whether ANY queued weapon bears (else the commit will fizzle → the
                // "won't fire" cue). Computed before the gfx borrow; read-only.
                let (aim_pos, aim_cells, queued_any, queued_bears) = {
                    let mut cells: Vec<Pos> = Vec::new();
                    let mut any = false;
                    let mut bears = false;
                    let mut ppos = Pos::new(broadside_engine::grid::COLS / 2, 0);
                    if let Some(p) = self.board.cells.iter().flatten().find(|s| s.faction == Faction::Player) {
                        ppos = p.pos;
                        for qid in &p.queue {
                            if let Some(action) = self.content.action(qid) {
                                any = true;
                                let hits = broadside_engine::resolve::resolve_targeting_2d(action, &self.board, p.pos);
                                if !hits.is_empty() {
                                    bears = true;
                                    for h in hits {
                                        if !cells.contains(&h) {
                                            cells.push(h);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    (ppos, cells, any, bears)
                };
                // (#57) Player column + campaign level drive the parallax: the
                // background pans horizontally with the player's lateral position
                // and recedes with the campaign level. Read before the gfx borrow.
                let player_col = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .map(|s| s.pos.col)
                    .unwrap_or(broadside_engine::grid::COLS / 2);
                let bg_level = self.board.level;
                // (#76 scene-res) The projector for THIS frame, scaled to the LIVE
                // scene (offscreen) size so the lane geometry reprojects when `;`/`'`
                // change the resolution. At the 480×270 default `for_scene` ==
                // `default()`, so this is identical to the old fixed path until a
                // cycle. Built from the gfx scene-size globals (free fns, no borrow).
                let scene_cfg = scene_projector();
                // (#79) Per-ship slide/turn tween for THIS frame — computed BEFORE
                // the gfx mutable borrow (it reads &self). Empty when nothing is
                // mid-move, so the render is identical to the static path at rest.
                let scene_tween = self.tween_2d(&scene_cfg, now);
                // (#90 kill-burst) Prune bursts past their lifetime, then collect the
                // live ones' cells for this frame. Both BEFORE the gfx borrow (reads
                // &mut self). Only emitted in the Playing block below, so an overlay
                // frame never shows a burst.
                self.kill_bursts
                    .retain(|(_, t)| now.duration_since(*t).as_secs_f32() < KILL_BURST_SECS);
                let kill_cells: Vec<Pos> = self.kill_bursts.iter().map(|(p, _)| *p).collect();
                // (#101) Prune expired hull-drop flashes, then collect the live ones
                // as (ship clone, intensity) BEFORE the gfx borrow (same pattern as
                // kill_cells). Intensity fades 1->0 over HULL_FLASH_SECS. Cloning the
                // few flashing ships keeps push_hull_flash_2d able to take &Ship
                // without holding a board borrow across the gfx mutable borrow.
                self.hull_flash
                    .retain(|_, (_, t)| now.duration_since(*t).as_secs_f32() < HULL_FLASH_SECS);
                let hull_flashes: Vec<(Ship, i32, f32)> = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .filter_map(|s| {
                        self.hull_flash.get(&s.id).map(|(amount, t)| {
                            let intensity =
                                1.0 - (now.duration_since(*t).as_secs_f32() / HULL_FLASH_SECS);
                            (s.clone(), *amount, intensity.clamp(0.0, 1.0))
                        })
                    })
                    .collect();
                let Some(gfx) = self.gfx.as_mut() else { return };
                // (#57) Pan/recede the parallax background toward the player's
                // column + the campaign level, eased per frame.
                gfx.update_background(bg_level, player_col, 1.0 / 60.0);
                // (#70) Sync + advance loft poses so installed-mesh ships (the
                // player's Aegis GLB) have a live pose the loft pre-pass renders.
                // sync_loft_pose creates a pose on first sight + reorients on a
                // stance flip (no-op when unchanged); advance drives idle + any
                // tween. Only ships with an installed mesh actually loft; the rest
                // are a cheap no-op pose. The chase-cam camera yaw is applied in
                // the loft render itself — this just keeps the pose alive.
                let loft_ships: Vec<(String, Orientation)> = self
                    .board
                    .cells
                    .iter()
                    .flatten()
                    .map(|s| (s.id.clone(), s.orientation))
                    .collect();
                for (id, orient) in &loft_ships {
                    gfx.sync_loft_pose(id, *orient);
                }
                let live_ids: Vec<String> = loft_ships.iter().map(|(id, _)| id.clone()).collect();
                gfx.retain_loft_poses(&live_ids);
                let _ = gfx.advance_loft_poses(1.0 / 60.0);
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
                // (#51) Pass gfx as the SpriteRegistry so ships with an installed
                // loft mesh (the player's Aegis CAD hull via install_player_cad)
                // render as the real 3-D model, not the flat box. gfx already runs
                // the loft pre-pass + blit for any LoftShip command, and the
                // per-ship loft pose is synced/advanced above.
                let mut instances = hud::compose_scene_2d_tweened(
                    &self.board,
                    &scene_cfg,
                    &*gfx,
                    &scene_tween,
                    self.frame_clock,
                );
                // In-game salvage counter (top-right) + controls legend
                // (bottom-left) — both screen-space, independent of the board
                // projection. The modal overlays surface salvage in their banners.
                if matches!(demo_state, DemoState::Playing) {
                    push_salvage_hud(&mut instances, salvage);
                    // (#70) Live player POS + FACING readout (top-right under
                    // SALVAGE) — ground truth for the strafe/reorient controls so
                    // Bruce + lead read the real (col,row,facing), no capture
                    // guessing. Pulled fresh from the board each frame.
                    if let Some(p) = self
                        .board
                        .cells
                        .iter()
                        .flatten()
                        .find(|s| s.faction == Faction::Player)
                    {
                        hud::push_player_readout(&mut instances, p.pos, p.facing);
                    }
                    // (#76) Live resolution readout: SHIP <w>x<h> (cyclable via
                    // `,`/`.`) + SCENE <w>x<h> (cyclable via `;`/`'`), under the
                    // POS/FACE line.
                    hud::push_res_readout(&mut instances, gfx.loft_res(), gfx.scene_res());
                    // (#63) Controls legend removed — Bruce: the move-help text crowded
                    // the screen. Keybinds are discoverable in-game; no on-screen overlay.
                    // Player danger legibility (#67): screen hit-flash on damage.
                    // (The lane-anchored hull bar returns as a 2-D overlay.)
                    hud::push_player_hit_flash(&mut instances, self.hit_flash);
                    // (#90 kill-burst) Destruction bursts at the cells where ships
                    // died this/last round (~0.35s), projected onto the board via the
                    // live-scene projector. Gated to Playing so a board reset / overlay
                    // frame never bursts. Recorded only on a combat-turn resolve.
                    hud::push_destruction_at(&mut instances, &kill_cells, &scene_cfg);
                    // (#122) Player targeting telegraph — cyan preview of where each
                    // QUEUED weapon will strike from the current pose (mirrors the
                    // enemy threat overlay). No-op when nothing is queued/bears.
                    hud::push_player_targeting_2d(&mut instances, aim_pos, &aim_cells, &scene_cfg);
                    // (#123) "Won't fire" cue: a queued weapon exists but NONE bear
                    // from here → committing will fizzle. Loud red X over the player
                    // so a wasted commit isn't silent.
                    if queued_any && !queued_bears {
                        hud::push_fizzle_cue_2d(&mut instances, aim_pos, &scene_cfg);
                    }
                    // (#119) Procedural explosion particles seeded at ship death —
                    // one SOLID_WHITE sprite per live particle, fading + shrinking.
                    // Already in screen space (spawned via the projector), so it
                    // emits straight into the frame regardless of the live scene res.
                    self.particles.emit(&mut instances);
                    // (#101) Damage-flash on the lane hull bar of every ship that
                    // took a hit this round (fades over ~0.45s), so even a 1-2 hull
                    // drop visibly pops — paired with the min-size bar clamp so a
                    // back-row enemy's bar both stays readable AND flashes when hit.
                    // Drawn after the bars (compose_scene_2d_tweened) so it sits on
                    // top. Gated to Playing alongside the kill bursts.
                    for (ship, amount, intensity) in &hull_flashes {
                        hud::push_hull_flash_2d(&mut instances, ship, *intensity, &scene_cfg);
                        // (#106) Floating damage NUMBER above the ship, same timer.
                        hud::push_damage_number_2d(&mut instances, ship, *amount, *intensity, &scene_cfg);
                    }
                    // (#98) Player ability-tile row in the bottom HUD band — drawn
                    // from the real AbilityTile data (damage / cooldown_max), which the
                    // board alone doesn't carry. Damage # top-left, key # bottom-right,
                    // cooldown ticks along the bottom.
                    hud::push_ability_tiles_2d(&mut instances, &player_tiles);
                    // (#136 Bruce) "Recharging" cue: if the player just tried to queue
                    // an on-cooldown weapon, flash that tile for a short fade so the
                    // block reads. Map the blocked weapon id -> its slot char via the
                    // live player's mounts (slot '1'+mount_index, same as build_ship_tiles).
                    if let Some((weapon, t)) = self.queue_blocked_flash.clone() {
                        let age = now.duration_since(t).as_secs_f32();
                        if age < HULL_FLASH_SECS {
                            let slot = self
                                .board
                                .cells
                                .iter()
                                .flatten()
                                .find(|s| s.faction == Faction::Player)
                                .and_then(|p| p.mounts.iter().position(|m| m.weapon == weapon))
                                .map(|i| (b'1' + i as u8) as char);
                            if let Some(slot) = slot {
                                let intensity = 1.0 - age / HULL_FLASH_SECS;
                                hud::push_cooldown_block_cue_2d(&mut instances, &player_tiles, slot, intensity);
                            }
                        } else {
                            self.queue_blocked_flash = None;
                        }
                    }
                    // (#128 Bruce) Player QUEUE panel, TOP-RIGHT: the weapons lined up
                    // (1/2/3), in fire order. Built from the SAME player_tiles — a
                    // queued tile (queued_index = Some) leaves the hand (hollows out in
                    // push_ability_tiles_2d above) and its icon shows HERE. No-op when
                    // nothing is queued. Cards (5/6/7) are free + never queue, so they
                    // never appear in this panel.
                    hud::push_player_queue_panel_2d(&mut instances, &player_tiles);
                    // (#129 Bruce) Enemy INFO panel, TOP-LEFT: per live enemy, hull +
                    // shield + its REVEALED queue (enemy hand hidden — only what it has
                    // actually queued, read live from the board). No-op between encounters.
                    hud::push_enemy_info_panel_2d(&mut instances, &self.board);
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
                let _ = (active_tween, vfx_active, ability_active, flash_active, particles_active);
                // (#126) TURN-BASED: the world advances ONLY when the player takes a
                // turn-action (inside apply_intent, on a keypress) — there is no
                // per-frame world tick. RedrawRequested just keeps the swapchain live
                // (the continuous-redraw fix above) and re-presents the latched VFX
                // fade; the board state is unchanged between turns.
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

    /// (#70 strafe verify) REPLAY THE REAL INTENTS against the LIVE spawn board
    /// (player at Pos(2,3) Bow(N)), exactly as the running game does — do NOT set
    /// pos/facing directly (that bypass is what masked the bug). Pressing Right
    /// twice must STRAFE the player to col 4, SAME row, facing UNCHANGED (Bow N).
    /// This is the ground-truth for "arrows = lateral strafe, facing preserved"
    /// (Bruce's control model). If this passes, the live render of the moved
    /// player is bow-on toward the VP at col 4 (== the f4_c4_n capture).
    #[test]
    fn right_arrow_twice_strafes_to_col4_facing_unchanged() {
        use broadside_engine::grid::{Dir4, Facing};
        let mut board = fresh_board();
        let mut content = DemoContent::default();

        // Find the player's spawn pos/facing (the real board's, not assumed).
        let spawn = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| (s.pos, s.facing))
            .expect("player spawned");
        assert_eq!(spawn.0.col, 2, "spawn col (campaign mid)");
        assert_eq!(spawn.1, Facing::Bow(Dir4::N), "spawn facing bow-N up-lane");
        let spawn_row = spawn.0.row;

        // Replay Right, Right through the SAME apply_intent the keypress uses.
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);

        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player still on board");
        assert_eq!(player.pos.col, 4, "Right×2 strafes to col 4 (2→3→4)");
        assert_eq!(player.pos.row, spawn_row, "strafe keeps the SAME row (lateral only)");
        assert_eq!(
            player.facing,
            Facing::Bow(Dir4::N),
            "strafe must NOT change facing — ship stays bow-on toward the VP"
        );
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
        // (#70 2-D) player at its spawn cell (Pos(2,3)), not 1-D cell 0.
        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player on board");
        assert_eq!(player.queue.last(), Some(&"pulse_laser".to_string()));
    }

    #[test]
    fn move_intent_advances_ship_instantly() {
        // Under SS turn semantics MoveRight is instant — the ship strafes one
        // cell on the press, the queue is NOT touched, the world phase runs
        // after. (#70 2-D: the demo board spawns the player at Pos(2,3) facing N,
        // NOT 1-D cell 0; MoveRight = Dir4::E = col+1, SAME row, facing
        // unchanged.)
        use broadside_engine::grid::{Dir4, Facing};
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        let before = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
            .expect("player spawned");
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player still on board");
        assert_eq!(player.pos.col, before.col + 1, "MoveRight strafes col+1");
        assert_eq!(player.pos.row, before.row, "strafe keeps the row");
        assert_eq!(player.facing, Facing::Bow(Dir4::N), "facing unchanged by strafe");
        assert!(player.queue.is_empty(), "instant intent must NOT push to queue");
    }

    /// (#100 REGRESSION, was the #97 follow-up diagnostic) Bruce's exact live
    /// sequence headlessly: campaign spawn (player bow-N front) -> press 3 (queue
    /// broadside_battery) -> press Space (commit). This LOCKS the two #100 render
    /// cues at the data layer (the bottom-HUD tile renderer reads exactly these
    /// fields):
    ///   * pressing 3 populates the queue, so the queued tile reports
    ///     `queued_index == Some(_)` — the data the AMBER queued highlight + order
    ///     badge render from (the "no queue indicator" Bruce reported is a
    ///     regression iff this goes back to `None`);
    ///   * the queued tile's `can_fire` equals the fire-gate `resolve_targeting_2d`
    ///     verdict from the ship's current pose — the data the "NO TARGET /
    ///     can't bear" veil renders from (so the veil can never disagree with
    ///     whether a shot actually bears).
    /// It still prints the full repro (run with `-- --nocapture`) for eyeballing,
    /// but now FAILS on regression rather than only logging.
    #[test]
    fn combat_repro_3_space_diagnostic() {
        use broadside_engine::resolve::resolve_targeting_2d;

        let mut board = fresh_board();
        let mut content = fresh_content();
        let hulls = |b: &Board| -> Vec<(String, i32)> {
            b.cells.iter().flatten().map(|s| (s.id.clone(), s.hull)).collect()
        };
        let player = |b: &Board| b.cells.iter().flatten().find(|s| s.faction == Faction::Player).cloned().unwrap();

        eprintln!("=== COMBAT REPRO: spawn ===");
        let p0 = player(&board);
        eprintln!("player pos={:?} facing={:?} mounts={:?}", p0.pos, p0.facing,
            p0.mounts.iter().map(|m| (m.id.as_str(), format!("{:?}", m.arc), m.weapon.as_str())).collect::<Vec<_>>());
        eprintln!("hulls={:?}", hulls(&board));
        assert!(p0.mounts.len() >= 3, "player loadout has the 3 mount slots Bruce presses 1/2/3");

        // Independent fire-gate verdict for m3 from the spawn pose (the value the
        // tile's `can_fire` MUST mirror).
        let m3 = p0.mounts[2].weapon.clone();
        let m3_action = content.action(&m3).expect("broadside_battery is a real action").clone();
        let bears_at_spawn = !resolve_targeting_2d(&m3_action, &board, p0.pos).is_empty();
        eprintln!("broadside_battery bears from {:?} (spawn) = {bears_at_spawn}", p0.pos);

        // --- press 3: queue m3 (broadside_battery). The bin maps Key::D3 ->
        // QueueAction(mounts[2].weapon). ---
        eprintln!("=== press 3: QueueAction({m3}) ===");
        apply_intent(Intent::QueueAction(m3.clone()), &mut board, &mut content, &fresh_board);
        let p1 = player(&board);
        eprintln!("after press 3: player.queue={:?}", p1.queue);
        assert!(
            p1.queue.contains(&m3),
            "press 3 must QUEUE broadside_battery (so the queue indicator has data to render)"
        );

        // Build the tiles the bottom HUD would show + report each tile's state.
        let tiles = build_ship_tiles(&p1, &content, &board);
        for t in &tiles {
            eprintln!("  tile slot={} dmg={} range={} cd={}/{} queued_index={:?} can_fire={}",
                t.slot, t.damage, t.range, t.cooldown, t.cooldown_max, t.queued_index, t.can_fire);
        }
        // The m3 tile is the one Bruce queued: its slot is '3'.
        let m3_tile = tiles.iter().find(|t| t.slot == '3').expect("m3 tile present");
        assert!(
            m3_tile.queued_index.is_some(),
            "the queued tile must carry queued_index = Some (drives the amber highlight + order badge)"
        );
        assert_eq!(
            m3_tile.can_fire, bears_at_spawn,
            "the tile's can_fire must mirror the fire-gate (drives the NO-TARGET veil; never disagree with reality)"
        );

        // --- press Space: commit. ---
        eprintln!("=== press Space: CommitTurn ===");
        let before = hulls(&board);
        apply_intent(Intent::CommitTurn, &mut board, &mut content, &fresh_board);
        let after = hulls(&board);
        eprintln!("fire_events this round: {}", board.fire_events.len());
        for fe in &board.fire_events {
            eprintln!("  FireEvent {:?}->{:?} arch={:?} faction={:?} hit={}",
                fe.from_pos, fe.to_pos, fe.archetype, fe.attacker_faction, fe.hit);
        }
        eprintln!("hull BEFORE={before:?}");
        eprintln!("hull AFTER ={after:?}");
        eprintln!("player.queue after commit={:?}", player(&board).queue);
        // The commit consumes the queue (whether or not the shot bore): the
        // indicator clears, matching the round actually resolving.
        assert!(
            !player(&board).queue.contains(&m3),
            "committing the turn consumes the queued action (indicator clears)"
        );
        eprintln!("=== END REPRO ===");
    }

    /// (#102 REGRESSION) The #100 "no target / can't bear" cue must NEVER fire on a
    /// utility/self ability. The field-kit cards (mass_lock / mass_breach /
    /// sensor_pulse) are `TargetingPattern::SELF`, so `resolve_targeting_2d` is
    /// empty for them by construction — which used to veil + slash their tiles
    /// ("what is the slash through 5?"). `action_can_fire` now structurally returns
    /// `true` for SELF / DEPLOYED_CELL, so the veil can't apply. Lock that: every
    /// card action reads as fireable regardless of board state, while an aimed
    /// weapon out of bears still reads `false`.
    #[test]
    fn card_abilities_never_show_cant_bear_cue() {
        let content = fresh_content();
        let board = fresh_board();
        let player = board.cells.iter().flatten().find(|s| s.faction == Faction::Player).cloned().unwrap();

        for cid in ["mass_lock", "mass_breach", "sensor_pulse"] {
            let synth = synthetic_card_action_id(cid);
            let action = content
                .action(&synth)
                .unwrap_or_else(|| panic!("card {cid} synthetic action registered"));
            assert!(
                action_can_fire(action, &board, &player),
                "utility/self card {cid} must read as fireable (no can't-bear veil/slash)"
            );
        }

        // Sanity: an AIMED weapon that genuinely can't bear from the spawn pose
        // still reports false (the cue is preserved where it belongs). The player's
        // broadside_battery does not bear bow-N at spawn (proven in the #100 repro).
        let bb = content.action("broadside_battery").expect("broadside_battery exists");
        assert!(
            !action_can_fire(bb, &board, &player),
            "an aimed weapon out of bears must still read can't-fire (the cue is real for weapons)"
        );
    }

    /// (#108) The arc-letter mapping for the weapon-side tile indicator + that
    /// mount tiles carry it while utility cards don't. Locks the lookup Bruce reads
    /// to tell a SIDE weapon (B) from a forward one (F).
    #[test]
    fn ability_tile_carries_arc_letter_for_mounts_not_cards() {
        use broadside_engine::types::Arc;
        assert_eq!(arc_letter(Arc::Forward), 'F');
        assert_eq!(arc_letter(Arc::BroadsideArc), 'B');
        assert_eq!(arc_letter(Arc::Turret), 'T');
        assert_eq!(arc_letter(Arc::Rear), 'R');

        let content = fresh_content();
        let board = fresh_board();
        let player = board.cells.iter().flatten().find(|s| s.faction == Faction::Player).cloned().unwrap();
        let tiles = build_ship_tiles(&player, &content, &board);
        // Every mount tile (slots 1..3) carries an arc letter; card tiles (5..7) don't.
        for t in &tiles {
            if ('1'..='3').contains(&t.slot) {
                assert!(t.arc.is_some(), "mount tile slot {} must carry a firing-arc letter", t.slot);
            } else {
                assert!(t.arc.is_none(), "card tile slot {} has no firing arc", t.slot);
            }
        }
        // The player's m3 (broadside_battery) mounts on the BroadsideArc -> 'B'.
        let m3 = tiles.iter().find(|t| t.slot == '3').expect("m3 tile present");
        assert_eq!(m3.arc, Some('B'), "the broadside mount tile must read 'B' (side weapon)");
    }

    /// (#117) An ORDNANCE action (SPAWN_ORDNANCE, e.g. torpedo) reports the spawned
    /// PROJECTILE's damage on its tile, not 0. The damage lives on the projectile
    /// payload, not the action's own effects — action_damage now resolves it via
    /// content.spawn_projectile. Bruce's tile 2 (torpedo) read 0; must read its real
    /// damage (4 in the demo loadout).
    #[test]
    fn ordnance_tile_shows_spawned_projectile_damage() {
        let content = fresh_content();
        let board = fresh_board();
        let player = board.cells.iter().flatten().find(|s| s.faction == Faction::Player).cloned().unwrap();
        // m2 is the torpedo (SPAWN_ORDNANCE). Its tile damage must be > 0.
        let torp = content.action("torpedo").expect("torpedo action exists");
        let dmg = action_damage(torp, &content, &player);
        assert!(
            dmg > 0,
            "an ordnance (SPAWN_ORDNANCE) action must report its spawned projectile's damage, not 0; got {dmg}"
        );
        // And it flows through to the slot-2 tile.
        let tiles = build_ship_tiles(&player, &content, &board);
        let t2 = tiles.iter().find(|t| t.slot == '2').expect("m2 tile present");
        assert!(t2.damage > 0, "the torpedo tile (slot 2) must show nonzero damage; got {}", t2.damage);
    }

    #[test]
    fn commit_turn_runs_resolve_round() {
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        let before = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
            .expect("player spawned");
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &mut content, &fresh_board);
        // (#70 2-D) the player strafed col+1 (same row) before the round resolved.
        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player on board after thrust+commit");
        assert_eq!(player.pos.col, before.col + 1, "player strafed col+1 then committed");
        assert_eq!(player.pos.row, before.row);
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
        // (#70 2-D) Restart rebuilds render_example_board → the player is back
        // somewhere on the board (Pos(2,3)), not at 1-D cell 0.
        assert!(
            board.cells.iter().flatten().any(|s| s.faction == Faction::Player),
            "restart recreates the player"
        );
    }

    #[test]
    fn restart_resets_the_board() {
        let mut board = fresh_board();
        let mut content = DemoContent::default();
        apply_intent(Intent::MoveRight, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::CommitTurn, &mut board, &mut content, &fresh_board);
        apply_intent(Intent::Restart, &mut board, &mut content, &fresh_board);
        // (#70 2-D) Restart rebuilds the fresh board: the player is back at its
        // spawn Pos(2,3) (NOT moved to col+1) — i.e. the restart reset the strafe.
        let fresh = fresh_board();
        let spawn = fresh
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
            .expect("fresh player");
        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player after restart");
        assert_eq!(player.pos, spawn, "restart resets the player to its spawn cell");
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
        // (#70 2-D) player is at its spawn cell (Pos(2,3)), not 1-D cell 0.
        let player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .expect("player on board");
        assert!(player.queue.is_empty(), "no synthetic queued on rejected play");
    }
}
