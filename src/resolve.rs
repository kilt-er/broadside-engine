//! Combat resolver: one execution path serves player, enemy, and ordnance.
//!
//! Port of `broadside-engine/engine/resolve.ts`. The TypeScript engine is the
//! canonical reference; when this port and the TS disagree, the TS is right.
//!
//! ## What is implemented
//!
//! - The four-phase round ([`resolve_round`]).
//! - The arc + heat + cooldown gate ([`execute_queue`]).
//! - The eight targeting patterns ([`resolve_targeting`]).
//! - The full damage pipeline ([`apply_damage`]) in the canonical order:
//!   `band falloff -> modifiers -> target-lock 2x -> directional shield ->
//!   hull`.
//! - Effect dispatch ([`apply_effect`]) for DAMAGE, `APPLY_STATUS`, `VENT_HEAT`,
//!   REORIENT, `SPAWN_ORDNANCE`, DEPLOY.
//! - Ordnance advance and end-of-turn.
//!
//! ## What is stubbed (content slice owns these)
//!
//! These functions exist and have a working — if narrow — body so the rest of
//! the pipeline can run; the body is the TS body verbatim. Each is marked
//! `// TODO(broadside-content):` for the next teammate.
//!
//! - [`apply_modifiers`] — subsystem damage bonuses (Marksman, Point-Blank
//!   Doctrine, ...). Currently returns `dmg` unchanged.
//! - [`resolve_self_move`] — full `THRUST/BURN/SLIP/JUMP/TRACTOR_SWAP` with
//!   occupancy + collision. Currently a simple step-loop in the bow direction.
//! - [`resolve_target_move`] — push/pull/swap with collision damage. Currently
//!   a no-op.
//! - [`decide_enemy_action`] — AI decision layer. Currently a no-op.
//! - The `BOARD` effect arm in [`apply_effect`] — currently a no-op.

use crate::geometry::{absorb_shield, bears, direction_to, facing_zone, opposite, range_band};
use crate::grid::{Dir4, Dir8, Facing, Pos};
use crate::types::{
    Action, ActionCost, Arc, Board, DeployHazardKind, Effect, Faction, Hazard, HazardKind, Hook,
    HookContext, LaneEnd, MovementMode, Orientation, Projectile, RangeBand, ReorientTo, Ship,
    Status, StatusKind, Targeting, TargetingPattern, WeaponArchetype,
};

/// Shield-pool regen per turn (#103 Model A, Bruce-tunable). Each face's `charge`
/// pool refills by this much toward its capacity (`armour`, repurposed) at the
/// end of a turn — but ONLY on faces that did NOT take fire this round (the
/// under-fire pause in [`end_of_turn`]; "stop getting shot to recover"). So
/// sustained fire on one face keeps it pinned and the target eventually dies,
/// while a disengaged ship recharges. Bruce tunes this rate.
const SHIELD_REGEN_PER_TURN: i32 = 1;

/* =============================================================================
 * Content trait — the resolver's view of the catalog + spawn table.
 * ========================================================================== */

/// Resolver-facing slice of the content layer. Mirrors the TS
/// `Content { actions, spawnProjectile }` interface; using a trait keeps the
/// resolver decoupled from the concrete catalog wiring.
pub trait Content {
    /// Action lookup by id. `None` for unknown ids — `execute_queue` skips
    /// missing actions silently, matching the TS `if (!a) continue`.
    fn action(&self, id: &str) -> Option<&Action>;

    /// Spawn a projectile of a given kind owned by `owner`. The TS signature
    /// is `(kind, owner, board) => Projectile`; in Rust the call site
    /// (`SPAWN_ORDNANCE`) already holds `&Board`, so we pass `&Ship` and let
    /// the implementation reach for board state through whatever closure it
    /// captures. Boards aren't borrowed here to keep the resolver's call
    /// patterns straightforward.
    fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile;

    /// Total additive subsystem damage modifier emitted by `attacker`
    /// at `band`. Called by [`apply_modifiers`] inside the canonical
    /// damage pipeline **after** band falloff and **before** target-lock
    /// doubling. Concrete `Content` impls scan the **attacker's**
    /// installed subsystem list and sum each match's contribution —
    /// Point-Blank Doctrine is `+2` at [`crate::grid::Range::Adjacent`],
    /// Marksman is `+1` at [`crate::grid::Range::Far`], and so on.
    ///
    /// **2-D band (#34):** `band` is the 2-D [`crate::grid::Range`] (the
    /// 3-band Chebyshev bucket — `Adjacent`/`Near`/`Far`), NOT the legacy
    /// 1-D `RangeBand`. The live damage pipeline ([`apply_damage_2d`])
    /// passes the actual 2-D band straight through — no lossy collapse.
    /// (The 1-D 5-band subsystem flavour mapped onto the 3 bands: the
    /// "point-blank" subsystem keys `Adjacent`, the "long-range" one keys
    /// `Far`. Before #34 the 2-D path collapsed `Far -> Mid` through a
    /// `Range -> RangeBand` shim, so a `Long`-keyed subsystem like Marksman
    /// could NEVER fire in 2-D — that latent bug is what this migration
    /// fixes.)
    ///
    /// **Direction (audit #67):** modifiers fire from the attacker's
    /// fittings, NOT the target's. The analysis HTML's catalog descs
    /// for these subsystems read "+1 damage **when firing** at long
    /// range" (Marksman), "+2 damage **at point-blank**" (Point-Blank
    /// Doctrine) — all attacker-frame verbs and pronouns. The pre-audit
    /// implementation consulted the target's subsystems, which was
    /// backwards; tests passed because each Phase 2 demo installed the
    /// same subsystem set on both sides.
    ///
    /// Default impl returns `0` so existing test / demo `Content` impls
    /// don't need to be updated. The runtime subsystem registry lives on
    /// the concrete `Content` type (not on [`Board`]) because:
    ///
    /// - `Board` is a runtime-state struct and architect deliberately kept
    ///   it free of content-shaped fields; `SubsystemDef` is catalog-only.
    /// - The bus path (subscribe to `OnDamageDealt`) doesn't work because
    ///   that hook fires only at the END of `execute_queue`, AFTER
    ///   `apply_damage` already ran — too late to influence the modifier
    ///   step inside the pipeline.
    ///
    /// Team-lead approved this trait extension; architect notified.
    fn damage_modifier(&self, _attacker: &Ship, _band: crate::grid::Range, _board: &Board) -> i32 {
        0
    }

    /// End-of-turn subsystem pass. Called by [`end_of_turn`] **after** the
    /// base passive heat dissipation and **before** the `OnTurnEnd`
    /// event-bus emit, so any bus subscribers see the post-subsystem
    /// state. Default impl is a no-op.
    ///
    /// Concrete impls (today: [`crate::input::DemoContent`]) walk their
    /// installed-subsystem registry and apply OnTurnEnd-shaped effects
    /// (e.g. `HeatSink` subtracting an extra heat from the owning ship).
    /// The runtime layer lives in [`crate::subsystems`]; see that module
    /// for why the registry isn't on `Board`.
    ///
    /// Task #61 (Phase 2). Same pre-approval scope as `damage_modifier`.
    fn on_turn_end(&self, _board: &mut Board) {}

    /// Dispatch a `BOARD` effect by its `note` string. Used by field-kit
    /// Cards (`mass_lock`, `mass_breach`, `sensor_pulse`) which encode their
    /// behavior as `Effect::BOARD { note: "mass_lock" }` and let the
    /// content layer decide what each note string actually does.
    ///
    /// `source_cell` is the cell of the ship that played the card.
    /// Default impl is a no-op so the BOARD arm of `apply_effect` stays
    /// safe for Content impls that don't carry cards.
    ///
    /// Task #63 (Phase 2). Same pre-approval scope as `damage_modifier`.
    fn apply_board_effect(&self, _note: &str, _source_cell: usize, _board: &mut Board) {}

    /// Look up the card id at slot `idx` (0-indexed) in the named ship's
    /// field-kit inventory. Returns `None` if the ship has no kit, the
    /// slot is past the end, or the entry has zero charges remaining.
    ///
    /// The "zero charges => slot is empty" rule keeps the key binding
    /// honest: a spent card is still in inventory (for tracking), but
    /// pressing the key shouldn't queue a play that can't pay its cost.
    ///
    /// Default impl returns `None` (no cards). [`crate::input::DemoContent`]
    /// overrides to read its [`crate::cards::FieldKitRegistry`].
    ///
    /// Task #63.
    fn card_at(&self, _ship_id: &str, _idx: usize) -> Option<String> {
        None
    }

    /// Validate and consume one play of `card_id` from `ship_id`'s
    /// field-kit. Returns [`crate::cards::PlayResult`] so the caller can
    /// distinguish "succeed → push synthetic action" from the various
    /// "no-op silently" failure modes.
    ///
    /// Mutates the content's per-ship card inventory; charges decrement
    /// on success. The actual board-wide effect is applied LATER, when
    /// the synthetic `__card_<id>` action runs through `execute_queue`
    /// and reaches the BOARD arm of `apply_effect`.
    ///
    /// Default impl returns `UnknownCard` (no cards). `DemoContent` overrides.
    ///
    /// Task #63.
    fn try_play_card(&mut self, _ship_id: &str, _card_id: &str) -> crate::cards::PlayResult {
        crate::cards::PlayResult::UnknownCard
    }
}

/* =============================================================================
 * Bus emit helper — temporary-detach pattern.
 *
 * `Board` owns its `bus`; emitting a hook needs `&mut bus` AND `&mut Board`
 * (because `HookContext` carries the board). We resolve the conflict by
 * `mem::take`-ing the bus, emitting, then putting it back. Closures registered
 * by subsystems can reach back into the board through `ctx.board` without
 * tripping Rust's aliasing rules.
 *
 * This mirrors how `EventBus::emit` itself swaps the per-hook subscriber Vec
 * out and back in; the two swaps compose cleanly.
 * ========================================================================== */

fn emit(board: &mut Board, hook: Hook, build: impl FnOnce(&mut HookContext)) {
    let mut bus = std::mem::take(&mut board.bus);
    let mut ctx = HookContext::new(board);
    build(&mut ctx);
    bus.emit(hook, &mut ctx);
    board.bus = bus;
}

/* =============================================================================
 * Phase 0 — the round itself.
 * ========================================================================== */

/// One full round. Mirrors `resolveRound` in `resolve.ts`. Composed of
/// [`fire_player_queue`] (phase 1) and [`run_world_phase`] (phases 2-4) so
/// the Shogun-Showdown-style turn dispatch in `input.rs::apply_intent` can
/// reuse the two halves independently: instant intents call
/// [`apply_instant_action`] + [`run_world_phase`]; queueing intents push to
/// the player's queue and call [`run_world_phase`] alone (queue not fired);
/// commit calls [`fire_player_queue`] + [`run_world_phase`].
pub fn resolve_round(board: &mut Board, content: &dyn Content) {
    // #59: a turn's worth of FireEvents accumulates across the WHOLE round —
    // the player's fired queue AND every enemy's — so the renderer draws one
    // beam per shot. Clear ONCE here, at turn start, BEFORE anyone fires.
    // `fire_player_queue` must NOT clear: it runs once per enemy inside
    // `run_world_phase`, so clearing there would wipe all-but-the-last ship's
    // beams. The in-game SS path doesn't call `resolve_round` — the bin clears
    // `board.fire_events` at the top of its `apply_intent` (renderer's lane);
    // this clear covers the resolve_round-driven (headless / test) path.
    board.fire_events.clear();

    let player_id: Option<String> = find_player_id(board);
    if let Some(id) = player_id {
        fire_player_queue(&id, board, content);
    }
    run_world_phase(board, content);
}

/// Find the id of the (single) player ship on the board, if any. Mirrors
/// the TS `board.cells.find(s => s?.faction === "player")` pattern.
pub fn find_player_id(board: &Board) -> Option<String> {
    ships_of(board)
        .iter()
        .find(|s| s.faction == Faction::Player)
        .map(|s| s.id.clone())
}

/* =============================================================================
 * Phase 1 — fire_player_queue (formerly the body of executeQueue).
 * ========================================================================== */

/// Phase 1 of a round: fire every action in `player_id`'s queue, in order,
/// through the arc + heat + cooldown gate. Clears the queue at the end.
/// Identical semantics to the TS `executeQueue(player, ...)` plus the
/// `onChainKill` emit. Exposed as its own seam so SS turn dispatch can call
/// it on `Intent::CommitTurn` without also running the world phase.
///
/// Also used by [`run_world_phase`] to fire each enemy's queue, so the same
/// per-ship loop body covers player and AI alike.
pub fn fire_player_queue(ship_id: &str, board: &mut Board, content: &dyn Content) {
    // Chain-kill window starts here. `destroy()` increments
    // `destroys_this_window`; `detect_chain` reads it after the queue runs.
    // Each queue-firing pass is one window, and so is each ordnance-phase
    // pass — both reset to 0 on entry.
    board.destroys_this_window = 0;

    // Snapshot the queue up front. Matches the TS `for (const actionId of
    // ship.queue)` which iterates a stable list across mutations to the
    // ship object — even if the ship moves, the action-id strings are still
    // consumed in order.
    let queue: Vec<String> = match find_cell_by_id(board, ship_id) {
        Some(c) => board.cells[c]
            .as_ref()
            .map(|s| s.queue.clone())
            .unwrap_or_default(),
        None => return,
    };

    for action_id in &queue {
        // Clone the Action so we don't hold a borrow on `content` while we
        // mutate the board.
        let action = match content.action(action_id) {
            Some(a) => a.clone(),
            // RESOLVER-SERVED FALLBACK (#68): the AI's closing maneuver queues
            // the synthetic lane-relative move ids (`__move_left` /
            // `__move_right`). The live bin's `Content` serves them, but the
            // resolver must NOT depend on that — a loader / test `Content`
            // that doesn't register them would otherwise leave enemies unable
            // to close (the original "enemies never move" failure mode). So we
            // fall back to a resolver-owned move action for those ids. Any
            // OTHER unknown id is skipped silently (TS `if (!a) continue`).
            None => match resolver_ai_move(action_id) {
                Some(a) => a,
                None => continue,
            },
        };
        // The action is identified by its id so heat / cooldown bookkeeping
        // can look it up in `ship.cooldowns`.
        run_action(ship_id, action_id, &action, board, content);
    }

    // Chain-kill check uses the ship's final cell, if it survived.
    if detect_chain(board) {
        let final_cell = find_cell_by_id(board, ship_id);
        emit(board, Hook::OnChainKill, |ctx| {
            ctx.source_cell = final_cell;
        });
    }

    // Clear the queue. The ship may have been destroyed during effect
    // application; only clear if it still exists.
    if let Some(post_cell) = find_cell_by_id(board, ship_id) {
        if let Some(ship) = board.cells[post_cell].as_mut() {
            ship.queue.clear();
        }
    }
}

/* =============================================================================
 * Phases 2-4 — run_world_phase: ordnance, enemy queues, end-of-turn.
 * ========================================================================== */

/// Phases 2-4 of a round: advance every live projectile, run each enemy
/// (FIRE its previously-telegraphed queue, THEN decide + telegraph its next
/// action), then end-of-turn bookkeeping. Every player input in the SS turn
/// model runs this after its instant / queue-mutation effect lands, so a
/// single keystroke always advances time.
///
/// The enemy step is **fire-then-decide** (telegraph-one-turn-ahead, #67):
/// each enemy resolves the action it telegraphed last phase, then chooses
/// and *displays* its next action without firing it. This is what makes the
/// enemy's intent visible to the player before it lands.
pub fn run_world_phase(board: &mut Board, content: &dyn Content) {
    // TURN-BASED (chess) model, per docs/design/CORE_GAMEPLAY_LOOP.md: one call =
    // ONE world turn, driven by each of the player's four turn-actions (move /
    // queue / dequeue-fire / wait). It is COMPOSED of three reusable seams in the
    // historical phase 2->3->4 ORDER: ordnance ([`advance_ordnance`]), then every
    // enemy takes one action ([`tick_enemy`], fire-then-decide), then end-of-turn
    // bookkeeping ([`end_of_turn`]: cooldown / heat / shield-regen / statuses).
    // (#124 built a real-time bin that drove tick_enemy/tick_world on independent
    // timers; #126 REVERTED it. tick_enemy + tick_world survive as composable
    // helpers but nothing real-time drives them - the live bin calls THIS once per
    // turn-action.) NOTE: run_world_phase does NOT clear `board.fire_events` (the
    // resolve_round-top clear owns that window for the headless path; the bin
    // clears at the top of `apply_intent`).
    advance_ordnance(board, content);

    // Enemy phase, in telegraphed initiative order. Snapshot ids up front so
    // movement / destroys during one enemy's turn can't reshuffle the remaining
    // enemies' identification; a since-destroyed enemy no-ops inside tick_enemy.
    for enemy_id in live_enemy_ids(board) {
        tick_enemy(&enemy_id, board, content);
    }
    // Final telegraph paint — identical to the historical end-of-loop paint.
    // tick_enemy already repaints per enemy, but this also covers the zero-enemy
    // case (no tick_enemy ran) so a stale threat set is always cleared.
    paint_threats(board, content);

    // 4 - end of turn (cooldowns / heat / shield-regen / statuses).
    end_of_turn(board, content);
}

/// Phase 2 (extracted, #124): advance every live projectile by its speed and
/// resolve impacts. Its own chain-kill window — resets `destroys_this_window` so
/// ordnance-impact kills are scored separately from the player's queue (the TS
/// emits `onChainKill` only from executeQueue, not the ordnance phase). Snapshots
/// the projectile ids first because an impact may remove its projectile.
pub fn advance_ordnance(board: &mut Board, content: &dyn Content) {
    board.destroys_this_window = 0;
    let projectile_ids: Vec<String> = board.ordnance.iter().map(|p| p.id.clone()).collect();
    for id in projectile_ids {
        // R5: live ordnance phase steps across the 2-D grid (invariant A).
        advance_projectile_2d(&id, board, content);
    }
}

/// Live ids of every enemy ship, in initiative order (#124). The bin's real-time
/// loop iterates these to drive [`tick_enemy`] per enemy without reaching into
/// `board.cells` itself; `run_world_phase` uses the same list so the composed
/// (headless) path and the decoupled (live) path enumerate enemies identically.
pub fn live_enemy_ids(board: &Board) -> Vec<String> {
    enemy_initiative(board)
        .into_iter()
        .filter_map(|c| board.cells[c].as_ref().map(|s| s.id.clone()))
        .collect()
}

/// Tick ONE enemy: the fire-then-decide step for a single enemy. Called once
/// per enemy by [`run_world_phase`] (in `enemy_initiative` order) so that each
/// world turn every enemy takes exactly one action - the turn-based model
/// (`docs/design/CORE_GAMEPLAY_LOOP.md`), "enemies queue before they fire."
/// (Extracted in #124 for a real-time bin that was reverted in #126; it remains
/// the per-enemy seam `run_world_phase` composes.)
///
/// TELEGRAPH-ONE-TURN-AHEAD (#67), per enemy:
///   a. FIRE the queue it telegraphed on its PREVIOUS tick — [`fire_player_queue`]
///      runs and CLEARS it (a no-op on the first tick, queue empty). Gated by
///      `skips_turn` (`SystemsOffline`) — a stunned enemy still DECIDES (shows
///      intent) but does not fire this tick.
///   b. RE-LOCATE (firing may have moved/destroyed it) and DECIDE its next
///      action, left queued + un-fired so the renderer's telegraph shows it.
///
/// Then REPAINT `board.threats` so the telegraph can't desync from where shots
/// land, regardless of tick cadence (the same `resolve_targeting_2d` single
/// source the AI elected with and the firing will resolve with — V4-at-R8). With
/// independent ticks this updates the threat set incrementally as each enemy
/// decides (a superset of the old all-at-once end-of-loop paint; identical final
/// state when run for every enemy in `run_world_phase`). Touches ONLY this enemy
/// and the cells it shoots — never the player's or another enemy's queue.
pub fn tick_enemy(enemy_id: &str, board: &mut Board, content: &dyn Content) {
    let Some(enemy_cell) = find_cell_by_id(board, enemy_id) else {
        return; // already destroyed
    };
    // a. Fire the previously-telegraphed queue (skipped while stunned).
    if !skips_turn(board, enemy_cell) {
        fire_player_queue(enemy_id, board, content);
    }
    // b. Re-locate (firing may have moved/destroyed the enemy) and decide the
    //    NEXT telegraph, left un-fired for the renderer to show.
    if let Some(enemy_cell) = find_cell_by_id(board, enemy_id) {
        crate::ai::decide_enemy_action(enemy_cell, board, content);
    }
    // Keep the telegraph truthful after this enemy's decision.
    paint_threats(board, content);
}

/// Everything in a world phase EXCEPT the per-enemy loop - ordnance advance +
/// end-of-turn bookkeeping (cooldown decrement, heat dissipation, shield regen,
/// statuses) - plus a `fire_events` clear. Built in #124 as a real-time global
/// clock tick; that bin was REVERTED in #126, so NOTHING drives this live today
/// (the live turn-based path uses [`run_world_phase`]; the headless path uses
/// `resolve_round`). RETAINED as a composable seam. NOTE the difference from
/// `run_world_phase`: this CLEARS `board.fire_events` (it owned the real-time
/// render window), whereas `run_world_phase` does not.
///
/// FIRE-EVENTS WINDOW (the lead's re-windowing requirement): the under-fire-pause
/// shield regen in [`end_of_turn`] reads `board.fire_events` to know which faces
/// took fire "this window". To keep the pause correct under the global tick, the
/// window is "since the last `tick_world`": `end_of_turn` reads the accumulated
/// `fire_events`, then this fn CLEARS them so the next window starts fresh. (The
/// renderer draws beams from `fire_events` on its faster frame cadence, so it
/// must draw BEFORE each `tick_world` — flagged to render. The headless
/// `run_world_phase` does NOT call `tick_world`; its window boundary is the
/// `resolve_round`-top clear, unchanged.)
pub fn tick_world(board: &mut Board, content: &dyn Content) {
    advance_ordnance(board, content);
    end_of_turn(board, content);
    // Window boundary for the next under-fire-pause read + the renderer's beams.
    board.fire_events.clear();
}

/// R8 — paint [`Board::threats`] from the enemies' currently-queued actions.
///
/// The telegraph "single best idea" (blueprint): a `Threat` is one cell the
/// player will be hit on next turn, and it is computed by running the **REAL**
/// [`resolve_targeting_2d`] against each enemy's QUEUED action — the identical
/// cell-selection spine used by the AI's fire election ([`crate::ai::decide_enemy_action`])
/// and by the firing phase ([`fire_player_queue`]). Reusing that one path is the
/// V4-at-R8 invariant: AI elects -> paints -> fires, all through `_2d`, so the
/// painted set provably equals the fired set (correctness from reuse, never a
/// second telegraph selection).
///
/// Rebuilds the whole list (clear + repopulate). Each enemy's queue holds at
/// most one telegraphed action (every `decide_enemy_action` rung pushes one id
/// then returns). The queued id is resolved to its [`Action`] the SAME way
/// `fire_player_queue` does — `content.action(id)` with the resolver-served
/// `resolver_ai_move` fallback for the synthetic move ids — so a queued maneuver
/// is handled identically here and at fire time. A move/vent/reorient produces
/// no hostile-cell footprint (its `resolve_targeting_2d` is empty or self), so
/// it paints nothing; only cell-targeting effects telegraph a threat.
///
/// `Threat.source` is the enemy's own [`Pos`] (invariant A: `cell.to_index() ==
/// pos`), so the renderer can draw the telegraph beam from the right ship and
/// R7 knows whose shot whiffs if the player vacates the threatened cell.
pub fn paint_threats(board: &mut Board, content: &dyn Content) {
    board.threats.clear();

    // Snapshot (enemy_pos, queued action ids) up front so we don't hold a board
    // borrow while resolving/ pushing threats. Only enemies telegraph.
    let queued: Vec<(Pos, Vec<String>)> = board
        .cells
        .iter()
        .filter_map(|c| c.as_ref())
        .filter(|s| s.faction == Faction::Enemy && !s.queue.is_empty())
        .map(|s| (s.pos, s.queue.clone()))
        .collect();

    for (enemy_pos, queue) in queued {
        for action_id in &queue {
            // Resolve the queued id -> Action exactly as fire_player_queue does:
            // the catalog first, then the resolver-owned synthetic-move fallback
            // (#68). An id neither serves is skipped (matches the firing path).
            let action = match content.action(action_id) {
                Some(a) => a.clone(),
                None => match resolver_ai_move(action_id) {
                    Some(a) => a,
                    None => continue,
                },
            };
            // SAME cell-selection spine as election + firing.
            let cells = resolve_targeting_2d(&action, board, enemy_pos);
            let kind = threat_kind(&action);
            for pos in cells {
                // A SELF-targeting action (move/vent/reorient resolves to the
                // firer's own cell) is not a threat against another cell — skip
                // the self-paint so a queued maneuver doesn't flag its own cell.
                if pos == enemy_pos {
                    continue;
                }
                board.threats.push(crate::types::Threat {
                    pos,
                    kind,
                    source: enemy_pos,
                });
            }
        }
    }
}

/// Classify a queued [`Action`] into the renderer's [`crate::types::ThreatKind`]
/// by its effect family (blueprint: "styled by `ThreatKind` + lethal flash").
/// `Damage` carries the projected PRE-mitigation total so the renderer can flash
/// cells where the hit would be lethal; the falloff/shield mitigation is NOT
/// applied here (the telegraph shows the raw threat, and the player's defensive
/// facing is exactly what they reposition to change). Precedence Damage >
/// Displace > Status > Other: a shot that also pushes reads as Damage (the
/// dangerous part), matching how the AI scores it.
fn threat_kind(action: &Action) -> crate::types::ThreatKind {
    use crate::types::ThreatKind;
    let raw_damage: i32 = action
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::DAMAGE { amount, .. } => Some(*amount),
            _ => None,
        })
        .sum();
    if raw_damage > 0 {
        return ThreatKind::Damage { amount: raw_damage };
    }
    if action
        .effects
        .iter()
        .any(|e| matches!(e, Effect::DISPLACE_TARGET { .. }))
    {
        return ThreatKind::Displace;
    }
    if action
        .effects
        .iter()
        .any(|e| matches!(e, Effect::APPLY_STATUS { .. }))
    {
        return ThreatKind::Status;
    }
    ThreatKind::Other
}

/* =============================================================================
 * Instant action — bypass the queue, run one Action atomically.
 * ========================================================================== */

/// Run a single action through the full gate + effect + bookkeeping
/// pipeline WITHOUT touching `ship.queue`. Used by the SS turn dispatch in
/// `input.rs` for Move / Reorient / Vent intents, which apply instantly to
/// board state and then yield to [`run_world_phase`].
///
/// Semantics match a single iteration of [`fire_player_queue`]'s loop:
/// heat / cooldown gate, the "nothing bore" arc gate, effect application
/// in declaration order, post-effect heat / cooldown bookkeeping, and an
/// `onDamageDealt` emit. Opens its own chain-kill window so an instant
/// action's destroys are scored independently of any later world-phase
/// damage.
pub fn apply_instant_action(
    ship_id: &str,
    action: &Action,
    board: &mut Board,
    content: &dyn Content,
) {
    board.destroys_this_window = 0;
    run_action(ship_id, &action.id, action, board, content);
    if detect_chain(board) {
        let final_cell = find_cell_by_id(board, ship_id);
        emit(board, Hook::OnChainKill, |ctx| {
            ctx.source_cell = final_cell;
        });
    }
}

/// Shared kernel: drive one [`Action`] through the gate + effects +
/// bookkeeping. Used by both [`fire_player_queue`]'s per-queued-action
/// iteration and by [`apply_instant_action`]. Returns `true` if the action
/// actually fired (passed every gate, applied its effects, paid its cost),
/// `false` if it was filtered by lockout / cooldown / "nothing bore."
///
/// The `lookup_id` is the cooldown-map key — usually `action.id`, but
/// queued actions pass the queue entry's string so a mod that aliases an
/// action under a different id still resets the right cooldown slot.
fn run_action(
    ship_id: &str,
    lookup_id: &str,
    action: &Action,
    board: &mut Board,
    content: &dyn Content,
) -> bool {
    // Re-resolve the ship's current cell. A prior effect (DISPLACE_SELF,
    // push, swap) may have moved the ship before this action runs.
    let Some(ship_cell) = find_cell_by_id(board, ship_id) else {
        return false;
    };
    let ship = board.cells[ship_cell]
        .as_ref()
        .expect("find_cell_by_id located an occupant");
    // Overheated: only free / zero-heat actions can fire.
    if ship.locked_out && action.cost.heat > 0 {
        return false;
    }
    // Not charged yet.
    if ship.cooldowns.get(lookup_id).copied().unwrap_or(0) > 0 {
        return false;
    }

    // Resolve targeting against the CURRENT cell. R3: 2-D path —
    // `resolve_targeting_2d` returns `Vec<Pos>` over the real grid; we shim to
    // 1-D `cells` (`Pos::to_index`) to feed the still-1-D `apply_effect` /
    // FireEvent code below. The shim is CORRECT under the Board slot==pos
    // invariant (A): a ship at `cells[i]` has `pos.to_index() == i`, so 2-D
    // targeting + 1-D cell application address the same slots. Removed when R4
    // takes `apply_effect`/`apply_damage` to `Pos`.
    let ship_pos = ship.pos;
    // Capture the firer's faction here (releases the `ship` borrow before the
    // R7 whiff block mutates `board.fire_events`).
    let ship_faction = ship.faction;
    let target_positions = resolve_targeting_2d(action, board, ship_pos);
    let cells: Vec<usize> = target_positions.iter().map(|p| p.to_index()).collect();

    // R7 — DODGE WHIFF. Before the "nothing bore" gate can swallow a vacated
    // shot, draw the miss. This ship telegraphed a threat set last phase
    // (`board.threats` painted by R8, `source == ship_pos`); if the player has
    // since VACATED a threatened cell, the queued shot now finds it empty and
    // would otherwise vanish silently. We emit a `hit: false` FireEvent
    // (ship_pos -> the now-empty telegraphed cell) so the renderer draws the
    // beam firing into the space the target just left. A threatened cell that
    // is STILL occupied resolves normally below (the `hit: true` path), so we
    // whiff ONLY telegraphed cells that are now empty. Additive render state,
    // like the hit:true emit — it changes no mechanic and runs regardless of
    // the nothing-bore gate (the enemy visibly fires even when it connects with
    // nothing; the gate only governs heat/cooldown). Fires-only: a non-DAMAGE
    // queued action (move/vent/reorient) telegraphs no Damage threat to whiff.
    let fires_damage_whiff = action
        .effects
        .iter()
        .any(|e| matches!(e, Effect::DAMAGE { .. }));
    if fires_damage_whiff {
        // Telegraphed cells from THIS ship that are now empty (player vacated).
        let whiffed: Vec<Pos> = board
            .threats
            .iter()
            .filter(|th| th.source == ship_pos && board.ship_at(th.pos).is_none())
            .map(|th| th.pos)
            .collect();
        for pos in whiffed {
            board.fire_events.push(crate::types::FireEvent {
                from_cell: ship_pos.to_index(),
                to_cell: pos.to_index(),
                from_pos: ship_pos,
                to_pos: pos,
                archetype: action.archetype,
                attacker_faction: ship_faction,
                hit: false,
            });
        }
    }

    // The "nothing bore" gate: arc-required actions with no targets eat
    // nothing — cooldown is NOT reset and heat is NOT spent. Mirrors the
    // TS `if (a.targeting.requiresArc !== null && cells.length === 0) continue`.
    if action.targeting.requires_arc.is_some() && cells.is_empty() {
        return false;
    }

    // FireEvent production (#59): record the EXACT shot for the renderer's
    // beam, BEFORE effects run (so `ship_cell` is the gun's fire-time cell and
    // the target cells are pre-mutation). One [`crate::types::FireEvent`] per
    // CONNECTING target — a target cell that holds a ship — so a multi-target
    // shot (spinal / blast / broadside) fans out as N beams from one origin.
    // Fires-only: only DAMAGE-bearing actions emit (a move / vent / reorient
    // is not a "shot"). This is purely additive — it appends to the runtime
    // `board.fire_events` and changes no mechanic. `hit` is always `true`
    // here: we only emit for occupied target cells (a resolved target is a
    // ship), and shield-fully-absorbed hits still CONNECT (the exact case the
    // old hull-drop VFX guess missed). The `hit: false` miss path is the R7
    // dodge-whiff (#38), emitted in the separate block ABOVE for cells this ship
    // telegraphed last turn that the player has since vacated.
    let fires_damage = action
        .effects
        .iter()
        .any(|e| matches!(e, Effect::DAMAGE { .. }));
    if fires_damage {
        let attacker_faction = board.cells[ship_cell]
            .as_ref()
            .map_or(Faction::Enemy, |s| s.faction);
        for &target in &cells {
            // Only connecting shots (a ship sits at the target cell) and never
            // a self-targeting beam (SELF actions resolve to the firer's own
            // cell — those aren't an attacker->target line).
            if target != ship_cell && board.cells[target].is_some() {
                board.fire_events.push(crate::types::FireEvent {
                    from_cell: ship_cell,
                    to_cell: target,
                    // R3: real 2-D beam endpoints. `from_pos` is the gun's
                    // fire-time cell; `to_pos` recovers the target's Pos from
                    // its flat slot index (exact under invariant (A):
                    // `target == target_pos.to_index()`). The renderer's #59
                    // exact beam + the R7 dodge-whiff draw between these.
                    from_pos: ship_pos,
                    to_pos: Pos::from_index(target).unwrap_or(ship_pos),
                    archetype: action.archetype,
                    attacker_faction,
                    hit: true,
                });
            }
        }
    }

    // Apply each effect. `apply_effect` may mutate cells / ordnance / etc.
    // The `source_cell` for THIS action's effects is the iteration-start
    // cell (the gun's position at fire time). Movement triggered by this
    // action shifts the ship for the NEXT call's reads, not for the current
    // effect chain.
    //
    // twin_linked (#50): the action's effects run TWICE. Cost/heat/cooldown
    // are still paid once (below) — the mod only doubles effect application,
    // it is not a re-queued action. Targeting is RE-RESOLVED before the second
    // pass (content ruling) so the second volley re-aims at the board left by
    // the first (e.g. the first volley's kill clears a cell, so the spinal
    // line shortens). The re-resolve reads the ship's current cell, which a
    // DISPLACE effect in the first pass may have moved.
    // precision_core: snapshot which targeted cells hold a ship BEFORE the
    // effects run, so after bookkeeping we can tell whether this action killed
    // one (the cell is occupied now and empty after). Recorded only when the
    // mod is present, to avoid the scan otherwise.
    let precision_core = WeaponMod::of(action) == Some(WeaponMod::PrecisionCore);
    let precision_targets: Vec<usize> = if precision_core {
        cells
            .iter()
            .copied()
            .filter(|&c| board.cells[c].is_some())
            .collect()
    } else {
        Vec::new()
    };

    let passes = if WeaponMod::of(action)
        .is_some_and(WeaponMod::applies_effects_twice)
    {
        2
    } else {
        1
    };
    for pass in 0..passes {
        // Re-resolve targeting on the second pass against the (possibly moved)
        // ship and (possibly mutated) board.
        let pass_cells = if pass == 0 {
            cells.clone()
        } else {
            // R3: re-resolve via the 2-D path against the ship's CURRENT Pos
            // (it may have moved); shim Pos->usize for the 1-D apply_effect
            // (correct under invariant (A), as above).
            match board.find_pos_by_id(ship_id) {
                Some(cur_pos) => resolve_targeting_2d(action, board, cur_pos)
                    .iter()
                    .map(|p| p.to_index())
                    .collect(),
                None => break, // attacker gone after the first pass
            }
        };
        // The effect source is the ship's CURRENT cell for this pass.
        let pass_source = find_cell_by_id(board, ship_id).unwrap_or(ship_cell);
        for fx in &action.effects {
            apply_effect(fx, action, pass_source, &pass_cells, board, content);
        }
    }

    // Heat + cooldown bookkeeping happen AFTER effects, against the ship at
    // its post-effect cell. The TS resets `cooldowns[lookup_id]`
    // unconditionally once the action passed the arc gate (hit or miss on
    // individual effects); we match that, but only when the ship is still on
    // the board — a self-destruct (e.g. ReactorBreach) or reactor-breach
    // splash could have cleared its cell, and Rust (unlike TS) cannot write
    // fields on a ship that no longer occupies a cell.
    // precision_core (#50): did this action make a clean kill? Computed BEFORE
    // the mutable `ship` borrow below to avoid aliasing `board.cells`. "Clean
    // kill" = any targeted cell that held a ship before the effects is now
    // empty (any-lethal; overkill counts — content ruling).
    let precision_kill =
        precision_core && precision_targets.iter().any(|&c| board.cells[c].is_none());

    let post_cell = find_cell_by_id(board, ship_id);
    if let Some(post_cell) = post_cell {
        if let Some(ship) = board.cells[post_cell].as_mut() {
            ship.heat += action.cost.heat;
            if ship.heat >= ship.heat_max {
                ship.locked_out = true;
            }
            // The cooldown bookkeeping insert. precision_core overrides it to 0
            // on a clean kill — applied here (after the base insert) so the
            // recharge wins; doing it during effects would be clobbered by this
            // very insert. Keyed by `lookup_id`, the action's cooldown slot.
            let cd = if precision_kill {
                0
            } else {
                action.cost.cooldown_max
            };
            ship.cooldowns.insert(lookup_id.to_string(), cd);
        }
    }
    // `onDamageDealt` fires UNCONDITIONALLY — once per fired action — to match
    // the TS `executeQueue`, which emits `{ board, source: ship }` on every
    // loop iteration regardless of whether the firing ship survived (in TS
    // `ship` is an object reference that outlives its removal from the board;
    // the event is orthogonal to the attacker's fate). Reviewer divergence #1:
    // the pre-fix Rust nested this emit inside the `Some(post_cell)` guard, so
    // a self-destructing attacker silently skipped the hook. When the attacker
    // is gone, `source_cell` is `None` — the lane-index analog of "the source
    // ship is no longer on the board" — but subscribers still run.
    emit(board, Hook::OnDamageDealt, |ctx| {
        ctx.source_cell = post_cell;
    });
    true
}

/// Locate the lane cell occupied by the ship whose `id` matches. `None` if
/// no live ship on the board has that id (destroyed mid-queue, never
/// existed). The TS engine identifies ships by object reference; this is
/// the Rust analog — call it whenever a hand-off across a board-mutating
/// call needs to re-locate a known ship.
fn find_cell_by_id(board: &Board, ship_id: &str) -> Option<usize> {
    board
        .cells
        .iter()
        .position(|c| c.as_ref().is_some_and(|s| s.id == ship_id))
}

/* =============================================================================
 * Phase 2 — advanceProjectile.
 * ========================================================================== */

/// Step a single projectile by its speed, resolving impacts. Mirrors
/// `advanceProjectile` in `resolve.ts`. Identified by id rather than `&mut`
/// because the projectile may remove itself from `board.ordnance` on impact.
///
/// `#[allow(dead_code)]` after R5: the live ordnance phase calls
/// [`advance_projectile_2d`]; this 1-D version is retained for its fixture
/// tests until CONTRACT — superseded on the live path, not unused.
#[allow(dead_code)]
pub fn advance_projectile(projectile_id: &str, board: &mut Board, content: &dyn Content) {
    let Some(idx) = board.ordnance.iter().position(|p| p.id == projectile_id) else {
        return;
    };
    let speed = board.ordnance[idx].speed;
    for _ in 0..speed {
        // Re-find the projectile each step in case prior iterations moved
        // it within the vec (they don't today, but the snapshot of `idx` is
        // not load-bearing this way).
        let Some(idx) = board.ordnance.iter().position(|p| p.id == projectile_id) else {
            return;
        };
        // Step one cell in the heading direction.
        let new_cell = match board.ordnance[idx].heading {
            LaneEnd::Fore => board.ordnance[idx].cell.checked_add(1),
            LaneEnd::Aft => board.ordnance[idx].cell.checked_sub(1),
        };
        let Some(new_cell) = new_cell else {
            // Stepped off the lane (aft overflow).
            board.ordnance.retain(|p| p.id != projectile_id);
            return;
        };
        if new_cell >= board.size {
            // Stepped off the lane (fore overflow).
            board.ordnance.retain(|p| p.id != projectile_id);
            return;
        }
        board.ordnance[idx].cell = new_cell;

        // Did we hit a non-owner occupant?
        let occupant_faction = board.cells[new_cell].as_ref().map(|s| s.faction);
        let owner_faction = board.ordnance[idx].owner_faction;
        if let Some(occ_faction) = occupant_faction {
            if occ_faction != owner_faction {
                // Drop the payload through the damage pipeline / status apply.
                // Cloned out because we'll be mutating the board next.
                let payload = board.ordnance[idx].payload.clone();
                let impact_cell = new_cell;
                let dummy = dummy_weapon();
                for fx in &payload {
                    match fx {
                        Effect::DAMAGE { amount, .. } => {
                            apply_damage(impact_cell, *amount, impact_cell, &dummy, board, content);
                        }
                        Effect::APPLY_STATUS { status, duration } => {
                            add_status(impact_cell, *status, *duration, board);
                        }
                        _ => {} // TS only handles these two on impact.
                    }
                }
                board.ordnance.retain(|p| p.id != projectile_id);
                return;
            }
        }
    }
}

/// 2-D `advance_projectile` (blueprint R5). The v2 port of [`advance_projectile`]
/// above — steps a projectile `speed` cells along its [`Projectile::heading8`]
/// (a `Dir8`) via `grid::offset`, resolving the first non-owner impact. Same
/// expand-contract shape: the 1-D version stays (its fixture tests) until
/// CONTRACT; the live ordnance phase switches here.
///
/// Per step: `grid::offset(pos, heading8, 1)` — off-grid (`None`) removes the
/// projectile (flew off the board); otherwise update `pos`+`cell` (invariant A)
/// and, if a non-owner ship occupies the new cell, drop the payload
/// (DAMAGE -> [`apply_damage_2d`], `APPLY_STATUS` -> [`add_status`]) and remove
/// the projectile.
///
/// Impact direction: the projectile arrives FROM the cell behind it along its
/// heading, so the damage `incoming_from` is `opposite(heading8)` — i.e. the
/// phantom attacker is one cell back along the heading. This is more correct
/// than the 1-D version (which passed `impact_cell` as both target and
/// attacker, so `direction_to(c,c)` always read the bow); the 2-D shot now hits
/// the hull face the projectile actually came at.
pub fn advance_projectile_2d(projectile_id: &str, board: &mut Board, content: &dyn Content) {
    let Some(idx) = board.ordnance.iter().position(|p| p.id == projectile_id) else {
        return;
    };
    let speed = board.ordnance[idx].speed;
    for _ in 0..speed {
        // Re-find each step (the projectile may move within the vec).
        let Some(idx) = board.ordnance.iter().position(|p| p.id == projectile_id) else {
            return;
        };
        let heading = board.ordnance[idx].heading8;
        let cur = board.ordnance[idx].pos;
        // Step one cell along the heading; off-grid removes the projectile.
        let Some(new_pos) = crate::grid::offset(cur, heading, 1) else {
            board.ordnance.retain(|p| p.id != projectile_id);
            return;
        };
        board.ordnance[idx].pos = new_pos;
        board.ordnance[idx].cell = new_pos.to_index(); // keep 1-D mirror in sync (invariant A)

        // Hit a non-owner occupant?
        let owner_faction = board.ordnance[idx].owner_faction;
        let occ_faction = board.ship_at(new_pos).map(|s| s.faction);
        if let Some(occ_faction) = occ_faction {
            if occ_faction != owner_faction {
                // Drop the payload through the 2-D damage pipeline / status apply.
                // Cloned out because we mutate the board next. The shot arrives
                // from behind along the heading -> phantom attacker one cell back
                // (clamped to new_pos if that's off-grid).
                let payload = board.ordnance[idx].payload.clone();
                let from = crate::grid::offset(new_pos, heading.opposite(), 1).unwrap_or(new_pos);
                let dummy = dummy_weapon();
                for fx in &payload {
                    match fx {
                        Effect::DAMAGE { amount, .. } => {
                            apply_damage_2d(new_pos, *amount, from, &dummy, board, content);
                        }
                        Effect::APPLY_STATUS { status, duration } => {
                            add_status(new_pos.to_index(), *status, *duration, board);
                        }
                        _ => {} // TS only handles these two on impact.
                    }
                }
                board.ordnance.retain(|p| p.id != projectile_id);
                return;
            }
        }
    }
}

/* =============================================================================
 * Phase 4 — end of turn.
 * ========================================================================== */

/// End-of-turn bookkeeping: tick cooldowns, dissipate heat, tick statuses,
/// emit the turn-end hook. Mirrors `endOfTurn` in `resolve.ts`.
pub fn end_of_turn(board: &mut Board, content: &dyn Content) {
    // Collect the cells of every live ship up front so we can mutate them
    // by index without holding a borrow on `board.cells`.
    let cells: Vec<usize> = ships_of(board).iter().map(|s| s.cell).collect();

    // PROBE(option B): faces that took FIRE this round do NOT regen (under-fire
    // pause). Read it from `board.fire_events` (accumulated all round, cleared at
    // resolve_round start) BEFORE the mutate loop. Map each hit to its HullZone
    // via the SAME single-source the damage path uses (geometry2d::facing_zone of
    // direction_to(target,attacker) against the target's facing).
    let mut under_fire: std::collections::HashSet<(usize, crate::types::HullZone)> =
        std::collections::HashSet::new();
    for ev in &board.fire_events {
        if !ev.hit {
            continue;
        }
        let tcell = ev.to_pos.to_index();
        if let Some(target) = board.cells.get(tcell).and_then(|c| c.as_ref()) {
            if let Some(incoming_from) = crate::geometry2d::direction_to(ev.to_pos, ev.from_pos) {
                let zone = crate::geometry2d::facing_zone(target.facing, incoming_from);
                under_fire.insert((tcell, zone));
            }
        }
    }

    for c in &cells {
        if let Some(s) = board.cells[*c].as_mut() {
            // Decrement every positive cooldown.
            for v in s.cooldowns.values_mut() {
                if *v > 0 {
                    *v -= 1;
                }
            }
            // Passive heat dissipation, floored at 0.
            s.heat = (s.heat - 1).max(0);
            if s.heat < s.heat_max {
                s.locked_out = false;
            }
            // Shield regen (#103 Model A, option B): each face's pool (`charge`)
            // refills by SHIELD_REGEN_PER_TURN toward its capacity (`armour`,
            // repurposed) — but ONLY if that face did NOT take fire this round
            // (under-fire pause). Sustained fire on a face keeps it pinned ->
            // ships die -> campaign terminates; quiet faces recover. Integer (#104).
            // Zone order MUST match `faces_mut()`: [bow, stern, port, starboard].
            let zones = [
                crate::types::HullZone::Bow,
                crate::types::HullZone::Stern,
                crate::types::HullZone::Port,
                crate::types::HullZone::Starboard,
            ];
            for (zone, face) in zones.iter().zip(s.shield_profile.faces_mut()) {
                if under_fire.contains(&(*c, *zone)) {
                    continue;
                }
                if face.charge < face.armour {
                    face.charge = (face.charge + SHIELD_REGEN_PER_TURN).min(face.armour);
                }
            }
        }
        tick_statuses(*c, board, content);
    }
    // Subsystem OnTurnEnd pass (Phase 2 task #61). Runs AFTER base
    // dissipation so HeatSink stacks additively on the canonical -1, and
    // BEFORE the bus emit so subscribers see the final post-subsystem
    // state.
    content.on_turn_end(board);
    emit(board, Hook::OnTurnEnd, |_ctx| {});
}

/* =============================================================================
 * Targeting — eight patterns.
 * ========================================================================== */

/// Return the lane cells `a` resolves on, honouring arc + band. Mirrors
/// `resolveTargeting` in `resolve.ts`. Patterns that don't pick board cells
/// (SELF / `DEPLOYED_CELL` / ORDNANCE) return the acting ship's own cell or the
/// spawn cell as appropriate.
pub fn resolve_targeting(a: &Action, board: &Board, ship_cell: usize) -> Vec<usize> {
    let t = &a.targeting;
    let Some(ship) = board.cells[ship_cell].as_ref() else {
        return Vec::new();
    };
    match t.pattern {
        TargetingPattern::SELF => vec![ship_cell],

        TargetingPattern::BROADSIDE => {
            // Fires both lane directions if the broadside arc bears.
            let mut out = Vec::new();
            for &end in &[LaneEnd::Fore, LaneEnd::Aft] {
                if let Some(arc) = t.requires_arc {
                    let probe = if end == LaneEnd::Fore {
                        board.size - 1
                    } else {
                        0
                    };
                    if !bears(ship, Some(arc), probe) {
                        continue;
                    }
                }
                if let Some(c) = first_target_toward(board, ship_cell, end) {
                    if in_allowed_band(&t.band, ship_cell, c) {
                        out.push(c);
                    }
                }
            }
            out
        }

        TargetingPattern::BEAM | TargetingPattern::POINT_BLANK => {
            // First target in the bearing direction the mount can fire.
            let Some(end) = bearing_direction(ship, ship_cell, board, a) else {
                return Vec::new();
            };
            let Some(c) = first_target_toward(board, ship_cell, end) else {
                return Vec::new();
            };
            if !in_allowed_band(&t.band, ship_cell, c) {
                return Vec::new();
            }
            vec![c]
        }

        TargetingPattern::SPINAL_LINE => {
            let Some(end) = bearing_direction(ship, ship_cell, board, a) else {
                return Vec::new();
            };
            let line: Vec<usize> = cells_toward(board, ship_cell, end)
                .into_iter()
                .filter(|c| in_allowed_band(&t.band, ship_cell, *c) && board.cells[*c].is_some())
                .collect();
            if t.hits_all {
                line
            } else {
                line.into_iter().take(1).collect()
            }
        }

        TargetingPattern::BLAST => {
            let Some(end) = bearing_direction(ship, ship_cell, board, a) else {
                return Vec::new();
            };
            let Some(c) = first_target_toward(board, ship_cell, end) else {
                return Vec::new();
            };
            // c-1 may underflow at the fore edge; clamp inclusively via
            // signed math then re-bound against `board.size`.
            let mut out = Vec::with_capacity(3);
            for delta in [-1i32, 0, 1] {
                let x = c as i32 + delta;
                if x >= 0 && (x as usize) < board.size {
                    out.push(x as usize);
                }
            }
            out
        }

        TargetingPattern::DEPLOYED_CELL | TargetingPattern::ORDNANCE => {
            let Some(end) = bearing_direction(ship, ship_cell, board, a) else {
                return Vec::new();
            };
            let c = match end {
                LaneEnd::Fore => ship_cell.checked_add(1),
                LaneEnd::Aft => ship_cell.checked_sub(1),
            };
            match c {
                Some(c) if c < board.size => vec![c],
                _ => Vec::new(),
            }
        }
    }
}

/* =============================================================================
 * Targeting — the 2-D port (blueprint R3). `resolve_targeting_2d` is the v2
 * replacement for the 1-D `resolve_targeting` above, built on the frozen
 * `grid` + `geometry2d` and the Board EXPAND occupancy seam ([`Board::ship_at`]).
 *
 * EXPAND-CONTRACT (matching the geometry.rs/geometry2d.rs split): the 1-D
 * `resolve_targeting` stays here, compiling + passing its own 1-D fixture tests,
 * until CONTRACT deletes it and renames `_2d` -> canonical. Only the live call
 * sites switch to `_2d` (the 1-D one is dead-for-live once the board is
 * 2-D-native, but kept as a reference + for its tests).
 *
 * SINGLE SOURCE OF TRUTH: this is the ONLY cell-selection path — both firing
 * and the ThreatMap telegraph run it, so a painted threat cell can never desync
 * from where the shot lands (blueprint "single best idea"; reviewer V4).
 *
 * FIRING-DIRECTION CONTRACT (reviewer-confirmed, the V4/V5 authority): the
 * firing ray is **cardinal** (4-way) — `bearing_cardinals` yields cardinals,
 * `first_target_along` walks a cardinal, `arc_bears` gates a cardinal (decision
 * #9: 4-cardinal facing). For every DIRECT hit the target sits ON the cardinal
 * ray, so the damage-step `incoming_from` (an 8-way `direction_to`, R4) is the
 * exact opposite cardinal. The 8-way `direction_to` only ever yields a diagonal
 * for BLAST splash (off-ray neighbours) + ordnance impacts — intended, since
 * `facing_zone` is total over all 8 (an off-axis splash lands on whatever face
 * the diagonal presents).
 * ========================================================================== */

/// The set of **cardinal** [`Dir8`] directions a mount with firing `arc` fires
/// along, given the ship's [`Facing`]. The 2-D replacement for the 1-D
/// `bearing_direction` (which returned a single `Fore`/`Aft`); in 2-D an arc can
/// bear along more than one cardinal (a `BroadsideArc` fires out *both* flanks),
/// so this returns a `Vec`. Empty = the arc does not bear at all in this stance.
///
/// Cardinals only (decision #9): a weapon never fires along a diagonal. Each
/// direction here is exactly a direction [`geometry2d::arc_bears`] accepts, so
/// "where I fire" and "which arc gate passes" are one model.
fn bearing_cardinals(facing: Facing, arc: Option<Arc>) -> Vec<Dir8> {
    use crate::grid::{Axis, Facing as F};
    let Some(arc) = arc else {
        // Arc-less (SELF / DEPLOYED_CELL / ORDNANCE): fire along the hull's
        // forward cardinal. `Bow(dir)` -> the bow; `Broadside(axis)` -> the
        // axis's increasing-coordinate direction (`dirs().0`), a stable choice
        // for the spawn/deploy "ahead" with no real bow.
        return match facing {
            F::Bow(dir) => vec![dir.to_dir8()],
            F::Broadside(axis) => vec![axis.dirs().0.to_dir8()],
        };
    };
    match arc {
        // Turret bears every cardinal; single-ray patterns pick among them.
        Arc::Turret => Dir4::ALL.iter().map(|d| d.to_dir8()).collect(),
        Arc::Forward => match facing {
            F::Bow(dir) => vec![dir.to_dir8()],
            F::Broadside(_) => Vec::new(),
        },
        Arc::Rear => match facing {
            F::Bow(dir) => vec![dir.to_dir8().opposite()],
            F::Broadside(_) => Vec::new(),
        },
        // Broadside battery (Model D, #92): fires out the two flank cardinals
        // PERPENDICULAR to the bow — turning the bow E/W puts the flanks N/S,
        // which IS broadsiding (Bruce's bow-cardinal model; no separate
        // Facing::Broadside stance). MUST mirror geometry2d::arc_bears's
        // BroadsideArc arm exactly (gate == firing, "one model").
        Arc::BroadsideArc => {
            let axis = match facing {
                F::Bow(dir) => dir.axis(),
                F::Broadside(axis) => axis,
            };
            let off = match axis {
                Axis::NorthSouth => Axis::EastWest,
                Axis::EastWest => Axis::NorthSouth,
            };
            let (a, b) = off.dirs();
            vec![a.to_dir8(), b.to_dir8()]
        }
    }
}

/// Every in-bounds cell along the cardinal ray from `from` in direction `dir`,
/// nearest-first. The 2-D replacement for the 1-D `cells_toward`; walks via
/// [`crate::grid::offset`] so the bounds check (and the no-1-D-underflow-hack
/// property the reviewer asked for, gate #1) is the grid's, not ad-hoc signed
/// math.
fn cells_along(from: Pos, dir: Dir8) -> Vec<Pos> {
    let mut out = Vec::new();
    let mut k = 1;
    while let Some(p) = crate::grid::offset(from, dir, k) {
        out.push(p);
        k += 1;
    }
    out
}

/// First occupied cell along the cardinal ray from `from` in `dir`, or `None`.
/// 2-D replacement for the 1-D `first_target_toward` — RE-DERIVED as a clean
/// ray-walk over [`Board::ship_at`] (no negative-index probe).
fn first_target_along(board: &Board, from: Pos, dir: Dir8) -> Option<Pos> {
    cells_along(from, dir)
        .into_iter()
        .find(|p| board.ship_at(*p).is_some())
}

// NOTE: the 2-D `bears(ship, arc, target_pos)` convenience wrapper (the 1-D
// `geometry::bears` analog — `arc_bears(ship.facing, arc, direction_to(ship.pos,
// target))`, with a same-cell `None` rejected per reviewer gate #3) is deferred
// to its first caller. `resolve_targeting_2d` doesn't need it (the patterns gate
// via `bearing_cardinals` directly); it lands with the 2-D AI (C1) or a later
// R-task that needs an arbitrary-target bearing test, to avoid dead code now.

/// 2-D `resolve_targeting`: the cells `a` resolves on from `ship_pos`, honouring
/// arc + 3-band range. The SINGLE source for firing AND the `ThreatMap` telegraph.
/// See the module-level firing-direction contract above. Pure + deterministic.
pub fn resolve_targeting_2d(a: &Action, board: &Board, ship_pos: Pos) -> Vec<Pos> {
    let t = &a.targeting;
    let Some(ship) = board.ship_at(ship_pos) else {
        return Vec::new();
    };
    // 2-D allowed bands (the EXPAND `range_band` field; `band` is the dead 1-D
    // one). `in_band` realises the over-extension deadzone (decision #7).
    let in_band = |target: Pos| crate::geometry2d::in_band(&t.range_band, ship_pos, target);

    match t.pattern {
        TargetingPattern::SELF => vec![ship_pos],

        TargetingPattern::BROADSIDE => {
            // Fire along every bearing cardinal (both flanks for a broadside),
            // taking the first in-band occupant on each ray.
            let mut out = Vec::new();
            for dir in bearing_cardinals(ship.facing, t.requires_arc) {
                if let Some(p) = first_target_along(board, ship_pos, dir) {
                    if in_band(p) {
                        out.push(p);
                    }
                }
            }
            out
        }

        TargetingPattern::BEAM | TargetingPattern::POINT_BLANK => {
            // First in-band occupant along the first bearing cardinal that has
            // one (faithful 2-D analog of the 1-D fore-first scan: iterate the
            // bearing cardinals in order, take the first ray that yields a
            // legal target).
            for dir in bearing_cardinals(ship.facing, t.requires_arc) {
                if let Some(p) = first_target_along(board, ship_pos, dir) {
                    if in_band(p) {
                        return vec![p];
                    }
                }
            }
            Vec::new()
        }

        TargetingPattern::SPINAL_LINE => {
            // Pierce along ONE bearing cardinal: the first that yields any
            // in-band occupant. `hits_all` -> every in-band occupant on the
            // ray; else just the first.
            for dir in bearing_cardinals(ship.facing, t.requires_arc) {
                let line: Vec<Pos> = cells_along(ship_pos, dir)
                    .into_iter()
                    .filter(|p| in_band(*p) && board.ship_at(*p).is_some())
                    .collect();
                if line.is_empty() {
                    continue;
                }
                return if t.hits_all {
                    line
                } else {
                    line.into_iter().take(1).collect()
                };
            }
            Vec::new()
        }

        TargetingPattern::BLAST => {
            // First occupant along a bearing cardinal, then splash its
            // 8-neighbours. ±1 -> 8-NEIGHBOUR is the deliberate 2-D widening
            // (reviewer gate #2): the 1-D "first + the two lane neighbours"
            // becomes "first + its in-bounds 8-neighbours" — an area burst.
            // (Splash cells are off the firing ray, so a splashed neighbour's
            // damage-step `incoming_from` may be diagonal — intended per the
            // firing-direction contract; `facing_zone` is total over 8.)
            for dir in bearing_cardinals(ship.facing, t.requires_arc) {
                if let Some(center) = first_target_along(board, ship_pos, dir) {
                    let mut out = vec![center];
                    out.extend(crate::grid::neighbors(center));
                    return out;
                }
            }
            Vec::new()
        }

        TargetingPattern::DEPLOYED_CELL | TargetingPattern::ORDNANCE => {
            // The single adjacent cell one step along the forward cardinal (the
            // spawn/deploy cell). Arc-less -> bearing_cardinals returns the
            // forward cardinal; take one step via grid::offset.
            let Some(&dir) = bearing_cardinals(ship.facing, t.requires_arc).first() else {
                return Vec::new();
            };
            match crate::grid::offset(ship_pos, dir, 1) {
                Some(p) => vec![p],
                None => Vec::new(),
            }
        }
    }
}

/* =============================================================================
 * The damage pipeline.
 *
 * LOAD-BEARING ORDER:
 *   1. band falloff (unless ANY DAMAGE effect on the weapon disables it)
 *   2. subsystem modifiers
 *   3. target-lock 2x (consumes the status)
 *   4. directional shield (charge -> armour)
 *   5. hull subtraction + emit + destroy check
 *
 * Do not re-order. The TS shape of this function is the canonical reference.
 * ========================================================================== */

/// Apply `raw` damage from cell `atk_cell` to the ship at cell `target_cell`
/// through the canonical pipeline. Mirrors `applyDamage` in `resolve.ts`.
///
/// `content` is needed for step 2 (subsystem damage modifiers); other steps
/// only touch board state. The trait extension is documented at
/// [`Content::damage_modifier`].
pub fn apply_damage(
    target_cell: usize,
    raw: i32,
    atk_cell: usize,
    weapon: &Action,
    board: &mut Board,
    content: &dyn Content,
) {
    // 1. Range band + optional falloff. The TS predicate is
    //    `effects.some(e => e.kind === "DAMAGE" && e.bandFalloff === false)`
    //    — i.e. a single DAMAGE effect on the action with the field
    //    explicitly set to false disables falloff for the WHOLE call.
    //    `None` and `Some(true)` both keep falloff on. Architect documented
    //    this in `Effect::DAMAGE.band_falloff`.
    let target_cell_value = match board.cells[target_cell].as_ref() {
        Some(s) => s.cell,
        None => return,
    };
    let band = range_band(atk_cell, target_cell_value);
    let falloff_disabled = weapon.effects.iter().any(|e| {
        matches!(
            e,
            Effect::DAMAGE {
                band_falloff: Some(false),
                ..
            }
        )
    });
    let mut dmg = if falloff_disabled {
        raw
    } else {
        crate::geometry::band_falloff(raw, band, weapon.targeting.optimal_band)
    };

    // 2. Subsystem damage modifiers. Routed through Content so the runtime
    //    subsystem registry stays on the content layer and doesn't leak
    //    onto Board. Audit #67: modifiers are attacker-side (the
    //    attacker's installed subsystems fire), so we look up by
    //    `atk_cell`, not `target_cell`.
    //    #34: `damage_modifier`/`apply_modifiers` now take the 2-D `Range`.
    //    This 1-D `apply_damage` is dead-for-live (the live path is
    //    `apply_damage_2d`, which already has the real 2-D band); it stays
    //    only for its fixture tests until CONTRACT. Map its 1-D `RangeBand`
    //    up to the 2-D `Range` at THIS boundary — the shim now lives in the
    //    dead 1-D path, not the live 2-D one (that was the #34 point).
    dmg = apply_modifiers(dmg, atk_cell, rangeband_to_range(band), board, content);

    // 3. Target-lock doubles the incoming hit and is consumed.
    if let Some(target) = board.cells[target_cell].as_mut() {
        if let Some(pos) = target
            .statuses
            .iter()
            .position(|s| s.kind == StatusKind::TargetLock)
        {
            dmg *= 2;
            target.statuses.swap_remove(pos);
        }
    }

    // 4. Directional shield. The shot arrives FROM the attacker's side, so
    //    `incoming_from` points back at the gun. `direction_to(target,
    //    attacker)` is exactly that.
    let incoming_from: LaneEnd = direction_to(target_cell_value, atk_cell);
    let post_shield_dmg = if let Some(target) = board.cells[target_cell].as_mut() {
        let zone = facing_zone(target.orientation, incoming_from);
        let face = target.shield_profile.face_mut(zone);
        absorb_shield(face, dmg)
    } else {
        return;
    };
    let final_dmg = post_shield_dmg;

    // 5. Hull subtraction + emit + destroy check.
    let killed = if let Some(target) = board.cells[target_cell].as_mut() {
        target.hull -= final_dmg;
        target.hull <= 0
    } else {
        return;
    };
    if final_dmg > 0 {
        emit(board, Hook::OnDamageTaken, |ctx| {
            ctx.target_cell = Some(target_cell);
            ctx.amount = Some(final_dmg);
        });
    }
    if killed {
        destroy(target_cell, board, content);
    }
}

/// 2-D damage pipeline (blueprint R4). The v2 port of [`apply_damage`] above,
/// wiring the 2-D `geometry2d` Range falloff (step 1) + 2-D `facing_zone` (step
/// 4) into the **UNCHANGED, load-bearing ORDER**:
///
///   1. band falloff  ->  2. subsystem modifiers  ->  3. target-lock 2x  ->
///   4. directional shield  ->  5. hull + emit + destroy
///
/// Reviewer V5 guards this ORDER + the `direction_to -> incoming_from` wiring.
/// Expand-contract like the rest of the R-series: the 1-D [`apply_damage`] stays
/// (its fixture tests) until CONTRACT; only the live callers (the `DAMAGE`
/// effect arm + the R6 collision) switch to this.
///
/// `atk_pos` is the firing/colliding cell, `target_pos` the cell hit. For every
/// DIRECT fired hit the target sits ON the cardinal firing ray, so
/// `direction_to(target_pos, atk_pos)` is the exact opposite cardinal; BLAST
/// splash + ordnance can yield a diagonal `incoming_from`, which `facing_zone`
/// handles (it is total over all 8) — the documented arity seam. This also
/// makes the R6 collision shield-zone correct (it was provisional on the 1-D
/// path, which mis-read the flat index as a lane position).
pub fn apply_damage_2d(
    target_pos: Pos,
    raw: i32,
    atk_pos: Pos,
    weapon: &Action,
    board: &mut Board,
    content: &dyn Content,
) {
    let target_idx = target_pos.to_index();
    // Bail if the target cell is empty (matches the 1-D guard).
    if board.ship_at(target_pos).is_none() {
        return;
    }

    // 1. Range band (2-D Chebyshev) + optional falloff. Same disable predicate
    //    as 1-D (one DAMAGE effect with `band_falloff: Some(false)` disables it
    //    for the whole call). The 2-D `band_falloff` is the ABSOLUTE
    //    [1.0, 0.6, 0.3] curve (decision #6), keyed on the actual band — it does
    //    NOT take `optimal_band` (that was the 1-D distance-from-optimal model).
    let band = crate::geometry2d::range_band(atk_pos, target_pos);
    let falloff_disabled = weapon.effects.iter().any(|e| {
        matches!(
            e,
            Effect::DAMAGE {
                band_falloff: Some(false),
                ..
            }
        )
    });
    let mut dmg = if falloff_disabled {
        raw
    } else {
        crate::geometry2d::band_falloff(raw, band)
    };

    // 2. Subsystem damage modifiers (ATTACKER-side: look up by the attacker's
    //    cell). #34: `apply_modifiers`/`Content::damage_modifier` now take the
    //    2-D `Range` directly — the live path passes the ACTUAL 2-D band, no
    //    `Range -> RangeBand` collapse (the old shim dropped `Far -> Mid`, which
    //    silently disabled any `Far`-keyed subsystem like Marksman in 2-D).
    dmg = apply_modifiers(dmg, atk_pos.to_index(), band, board, content);

    // 3. Target-lock doubles the hit and is consumed.
    if let Some(target) = board.ship_at_mut(target_pos) {
        if let Some(p) = target
            .statuses
            .iter()
            .position(|s| s.kind == StatusKind::TargetLock)
        {
            dmg *= 2;
            target.statuses.swap_remove(p);
        }
    }

    // 4. Directional shield. `incoming_from` points back at the gun:
    //    `direction_to(target, attacker)`. A same-cell hit (`None`) has no
    //    meaningful incoming direction — treat as a zone-less hit on the bow
    //    (only a degenerate self-collision reaches this). Real hits give a
    //    cardinal (direct) or diagonal (splash/ordnance); `facing_zone` is total.
    let post_shield_dmg = if let Some(target) = board.ship_at_mut(target_pos) {
        let zone = match crate::geometry2d::direction_to(target_pos, atk_pos) {
            Some(incoming_from) => crate::geometry2d::facing_zone(target.facing, incoming_from),
            None => crate::types::HullZone::Bow,
        };
        let face = target.shield_profile.face_mut(zone);
        // #103 Model A: the LIVE 2-D path uses the geometry2d POOL soak (charge
        // depletes, overflow -> hull), NOT the 1-D `geometry::absorb_shield`
        // (charge-eats-whole-hit + flat-armour-subtract) that the unqualified
        // import at the top still binds for the dead 1-D `apply_damage`.
        crate::geometry2d::absorb_shield(face, dmg)
    } else {
        return;
    };
    let final_dmg = post_shield_dmg;

    // 5. Hull subtraction + emit + destroy check.
    let killed = if let Some(target) = board.ship_at_mut(target_pos) {
        target.hull -= final_dmg;
        target.hull <= 0
    } else {
        return;
    };
    if final_dmg > 0 {
        emit(board, Hook::OnDamageTaken, |ctx| {
            ctx.target_cell = Some(target_idx);
            ctx.amount = Some(final_dmg);
        });
    }
    if killed {
        destroy(target_idx, board, content);
    }
}

/// Map a 1-D [`RangeBand`] up to the 2-D [`crate::grid::Range`] for the
/// **dead-for-live** 1-D [`apply_damage`] path (#34). The live 2-D pipeline
/// ([`apply_damage_2d`]) already holds the real 2-D `Range` and no longer needs
/// any conversion — the `Content::damage_modifier` trait now takes `Range`
/// natively. This collapse lives ONLY here so the 1-D fixture path keeps
/// compiling until CONTRACT deletes [`apply_damage`]; it is the inverse-direction
/// successor of the removed `range_to_rangeband` shim.
///
/// The 5 v1 bands fold onto the 3 v2 bands the same way the catalog loader's
/// `normalize_2d_bands` and the canonical transformer do (blueprint decision #6):
/// `PointBlank -> Adjacent`, `Close -> Near`, `Mid|Long|Extreme -> Far`. So a 1-D
/// `Long` hit maps to `Far` — and a `Far`-keyed subsystem (Marksman) still fires
/// on the 1-D path, matching its 2-D behaviour.
const fn rangeband_to_range(b: RangeBand) -> crate::grid::Range {
    use crate::grid::Range;
    match b {
        RangeBand::PointBlank => Range::Adjacent,
        RangeBand::Close => Range::Near,
        RangeBand::Mid | RangeBand::Long | RangeBand::Extreme => Range::Far,
    }
}

/* =============================================================================
 * Effect dispatch.
 * ========================================================================== */

/// Apply one [`Effect`] from action `a`, sourced by the ship at `source_cell`,
/// against the cells previously chosen by `resolve_targeting`. Mirrors
/// `applyEffect` in `resolve.ts`.
pub fn apply_effect(
    fx: &Effect,
    a: &Action,
    source_cell: usize,
    cells: &[usize],
    board: &mut Board,
    content: &dyn Content,
) {
    match fx {
        Effect::DAMAGE { amount, .. } => {
            // The attacker's id, captured BEFORE any hit so the on-hit mod
            // dispatch (precision_core) can re-find it after the board mutates.
            // `source_cell` holds the attacker at effect-start.
            let attacker_id: Option<String> =
                board.cells[source_cell].as_ref().map(|s| s.id.clone());
            let has_on_hit_mod = WeaponMod::of(a).is_some();
            for &c in cells {
                if board.cells[c].is_some() {
                    // R4: live damage path -> 2-D pipeline. cells came from
                    // resolve_targeting_2d (Pos->to_index shim) + source_cell is
                    // the attacker's slot, so under invariant (A) both recover
                    // their Pos exactly. apply_damage_2d wires the 2-D Range
                    // falloff + facing_zone, KEEPING the ORDER.
                    if let (Some(tp), Some(ap)) = (Pos::from_index(c), Pos::from_index(source_cell))
                    {
                        apply_damage_2d(tp, *amount, ap, a, board, content);
                    }
                    // On-hit weapon mod (flak/incendiary/emp/targeting_laser/
                    // precision_core). The target was present pre-hit (the
                    // `is_some` gate above), so the shot CONNECTED — riders
                    // land on contact even if the shield absorbed the hull
                    // damage. `killed` = the cell is now empty.
                    if has_on_hit_mod {
                        let killed = board.cells[c].is_none();
                        if let Some(ref atk_id) = attacker_id {
                            apply_on_hit_mod(a, c, killed, source_cell, atk_id, board, content);
                        }
                    }
                }
            }
        }

        Effect::APPLY_STATUS { status, duration } => {
            for &c in cells {
                if board.cells[c].is_some() {
                    add_status(c, *status, *duration, board);
                }
            }
        }

        Effect::VENT_HEAT {
            amount,
            recharge_cooldowns,
        } => {
            if let Some(source) = board.cells[source_cell].as_mut() {
                source.heat = (source.heat - amount).max(0);
                source.locked_out = false;
                if matches!(recharge_cooldowns, Some(true)) {
                    for v in source.cooldowns.values_mut() {
                        *v = 0;
                    }
                }
            }
            emit(board, Hook::OnVent, |ctx| {
                ctx.source_cell = Some(source_cell);
            });
        }

        Effect::REORIENT { to } => {
            if let Some(source) = board.cells[source_cell].as_mut() {
                match to {
                    // (#75) Player rotation: turn the authoritative 2-D `facing`
                    // a quarter-turn, then re-derive `orientation` from it so the
                    // hull VISUALLY rotates and the firing arcs follow (render +
                    // the 2-D fire-gate both key off `facing`; `orientation` is
                    // the shadow the loft pose / HUD still read).
                    ReorientTo::RotateLeft => {
                        source.facing = rotate_facing_ccw(source.facing);
                        source.orientation = orientation_from_facing(source.facing);
                    }
                    ReorientTo::RotateRight => {
                        source.facing = rotate_facing_cw(source.facing);
                        source.orientation = orientation_from_facing(source.facing);
                    }
                    // The legacy orientation-only reorients (TS parity) — unchanged.
                    ReorientTo::Flip => source.orientation = flip_orientation(source.orientation),
                    ReorientTo::Broadside => source.orientation = Orientation::Broadside,
                    ReorientTo::BowOn => {
                        source.orientation = Orientation::BowOn { bow: LaneEnd::Fore }
                    }
                }
            }
            emit(board, Hook::OnReorient, |ctx| {
                ctx.source_cell = Some(source_cell);
            });
        }

        Effect::SPAWN_ORDNANCE { projectile } => {
            // Snapshot the source ship for the content callback (avoids
            // holding `board.cells` borrowed while calling content).
            let owner = match board.cells[source_cell].as_ref() {
                Some(s) => s.clone(),
                None => return,
            };
            let p = content.spawn_projectile(projectile, &owner);
            board.ordnance.push(p);
        }

        Effect::DISPLACE_SELF {
            mode,
            distance,
            direction,
            direction_2d,
        } => {
            // R6: the LIVE path is now 2-D. Convert source_cell -> Pos (exact
            // under Board invariant (A): source_cell == ship.pos.to_index()) and
            // move via resolve_self_move_2d. We pass BOTH the 2-D `direction_2d`
            // override AND the 1-D `direction`: throw-2d uses the canonical
            // `direction: Aft` to hurl the ship facing-RELATIVE aft (the 2-D
            // resolver computes facing.opposite at run time, since a static
            // `direction_2d` cardinal can't express "aft" for an arbitrary
            // facing). The legacy 1-D resolve_self_move + its fixture tests stay
            // until CONTRACT.
            if let Some(source_pos) = Pos::from_index(source_cell) {
                resolve_self_move_2d(
                    source_pos,
                    *mode,
                    *distance,
                    *direction_2d,
                    *direction,
                    board,
                    content,
                );
            }
        }

        Effect::DISPLACE_TARGET { mode, distance } => {
            // R6b: live path -> 2-D. cells came from resolve_targeting_2d
            // (Pos->to_index shim) + source_cell is the actor's slot, so under
            // invariant (A) both recover their Pos exactly.
            if let Some(source_pos) = Pos::from_index(source_cell) {
                for &c in cells {
                    if let Some(target_pos) = Pos::from_index(c) {
                        resolve_target_move_2d(
                            target_pos, source_pos, *mode, *distance, board, content,
                        );
                    }
                }
            }
        }

        Effect::DEPLOY { hazard } => {
            // TS uses an array of arrays keyed by cell; we mirror that shape
            // with `Vec<Vec<Hazard>>`. The TS distinguishes `DeployHazardKind`
            // (mine|drone) from `HazardKind` (mine|drone|debris); we widen.
            let kind = match hazard {
                DeployHazardKind::Mine => HazardKind::Mine,
                DeployHazardKind::Drone => HazardKind::Drone,
            };
            for &c in cells {
                board.hazards[c].push(Hazard {
                    id: format!("{}@{}", a.id, c),
                    kind,
                    cell: c,
                    // v2 (A3 EXPAND): 2-D pos left at the transitional default —
                    // the 1-D lane index `c` and a 2-D grid Pos are different
                    // spaces with no valid bijection (lead ruling), so we do NOT
                    // derive one from the other. The resolver sets a real Pos
                    // when DEPLOY migrates to 2-D (R-series).
                    pos: crate::grid::Pos::new(0, 0),
                    payload: Vec::new(),
                    ttl: None,
                });
            }
        }

        Effect::BOARD { note } => {
            // Field-kit Cards encode their board-wide behavior as
            // `Effect::BOARD { note: "mass_lock" }` etc. and the Content
            // layer dispatches by note string. See `Content::apply_board_effect`
            // and the `cards` module for the placeholder card set.
            //
            // Card plays reach this arm through the synthetic action
            // produced by `intent_to_action_id(Intent::PlayCard(id))` —
            // the resolver doesn't special-case cards, they flow through
            // `execute_queue` exactly like any other action.
            content.apply_board_effect(note, source_cell, board);
        }
    }
}

/* =============================================================================
 * Weapon mods (#50).
 *
 * A weapon mod attaches to ONE action via [`Action::r#mod`] (a single mod id;
 * the catalog raises that action's cooldown to pay for it). Mods split into two
 * timing classes:
 *
 *   - ACTION-LEVEL, applied in [`run_action`]:
 *       * `twin_linked` — apply the action's effects TWICE (cost/heat/cooldown
 *         paid once; targeting re-resolved between the two passes so the second
 *         volley re-aims at the post-first-volley board).
 *       * `autoloader`  — free-fire: the action does not advance the turn. This
 *         is a TURN-DISPATCH concern (the SS turn model in `input.rs` reads
 *         `ActionCost::advances_turn`); the resolver pipeline has no
 *         turn-advance gate to flip, so the resolver does not act on autoloader
 *         beyond recognising it. See [`WeaponMod::advances_turn_override`].
 *
 *   - ON-HIT, applied by [`apply_on_hit_mod`] right after a DAMAGE effect's
 *     [`apply_damage`] resolves (NOT via a bus subscriber — the EventBus
 *     "no resolver re-entry inside a callback" invariant forbids that):
 *       * `flak_burst`     — 1 dmg to each lane-neighbour (target±1) of the hit
 *         cell, through the full pipeline (dummy_weapon, falloff off,
 *         shield-mediated), faction-blind, same precedent as ReactorBreach.
 *       * `incendiary`     — APPLY_STATUS hullBreach 3 on the hit cell.
 *       * `emp_charge`     — APPLY_STATUS systemsOffline 3 on the hit cell.
 *       * `targeting_laser`— APPLY_STATUS targetLock on the hit cell.
 *       * `precision_core` — if the hit killed the target, recharge this
 *         action's cooldown to 0 (any-lethal; overkill counts).
 *
 * Content ruled the edge semantics (#50): on-hit riders land on CONTACT (the
 * shot connected with an occupied target cell), even if the directional shield
 * fully absorbs the hull damage — the shield stops hull, not the rider. They
 * apply to enemy weapons identically (faction-agnostic; mods are properties of
 * the `Action`). First pass is single-mod-only (`r#mod` is one id); the doc's
 * "autoloader alongside another" combo is a deferred follow-up needing a
 * `r#mod -> Vec` types change (architect), not wired here.
 * ========================================================================== */

/// A recognised weapon mod. Parsed from [`Action::r#mod`]'s id string. The
/// exhaustive match in [`WeaponMod::from_id`] is the drift guard: an unknown
/// id yields `None` and the action behaves as un-modded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WeaponMod {
    FlakBurst,
    PrecisionCore,
    Incendiary,
    EmpCharge,
    TwinLinked,
    TargetingLaser,
    Autoloader,
}

impl WeaponMod {
    /// Parse a catalog mod id. `None` for unknown ids — an action carrying an
    /// unrecognised mod simply fires un-modded (forward-compatible with mods
    /// the resolver doesn't implement yet).
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "flak_burst" => Some(Self::FlakBurst),
            "precision_core" => Some(Self::PrecisionCore),
            "incendiary" => Some(Self::Incendiary),
            "emp_charge" => Some(Self::EmpCharge),
            "twin_linked" => Some(Self::TwinLinked),
            "targeting_laser" => Some(Self::TargetingLaser),
            "autoloader" => Some(Self::Autoloader),
            _ => None,
        }
    }

    /// The mod parsed off an action, if any.
    fn of(action: &Action) -> Option<Self> {
        action.r#mod.as_deref().and_then(Self::from_id)
    }

    /// `twin_linked` runs the effect list twice.
    fn applies_effects_twice(self) -> bool {
        self == Self::TwinLinked
    }

    /// `autoloader` forces the action to not advance the turn. Returns
    /// `Some(false)` to override `ActionCost::advances_turn`; `None` to leave
    /// the action's declared value untouched. The TURN layer (`input.rs`)
    /// consumes this; the resolver pipeline itself never branches on
    /// turn-advance, so this is exposed for the dispatcher rather than acted on
    /// inside [`run_action`].
    const fn advances_turn_override(self) -> Option<bool> {
        match self {
            Self::Autoloader => Some(false),
            _ => None,
        }
    }
}

/// Whether `action`'s mod (if any) forces free-fire (no turn advance). Public
/// seam for the turn dispatcher in `input.rs`: an autoloader-modded action is
/// free-fire regardless of its declared `advances_turn`. Returns the effective
/// advances-turn value (the override when a mod sets one, else the action's
/// own `cost.advances_turn`). The resolver does not call this — turn
/// advancement is decided in the SS dispatch layer.
pub fn action_advances_turn(action: &Action) -> bool {
    match WeaponMod::of(action).and_then(WeaponMod::advances_turn_override) {
        Some(v) => v,
        None => action.cost.advances_turn,
    }
}

/// Apply `action`'s ON-HIT weapon mod against a target at `hit_cell` that an
/// immediately-preceding [`apply_damage`] just struck. `killed` is whether that
/// hit destroyed the target (its cell is now empty). `atk_cell` is the firing
/// ship's cell — for `precision_core`'s cooldown recharge and as the splash
/// origin.
///
/// Called from the DAMAGE arm of [`apply_effect`]; not a bus subscriber, so it
/// never re-enters the resolver through the `EventBus`. Action-level mods
/// (`twin_linked`, `autoloader`) are NOT handled here — see [`run_action`].
fn apply_on_hit_mod(
    action: &Action,
    hit_cell: usize,
    killed: bool,
    atk_cell: usize,
    attacker_id: &str,
    board: &mut Board,
    content: &dyn Content,
) {
    let Some(m) = WeaponMod::of(action) else {
        return;
    };
    match m {
        WeaponMod::FlakBurst => {
            // flak-2d: 1 dmg to each in-bounds 8-NEIGHBOUR of the HIT cell,
            // through the full 2-D pipeline (shield-mediated, falloff off) via the
            // dummy impact weapon — the area-burst analog of the BLAST targeting
            // pattern (which already splashes `grid::neighbors`). The pre-2-D arm
            // splashed only the two 1-D lane neighbours (`hit_cell ± 1`)
            // bounds-checked against `board.size` (== COLS = 5), so OFF row 0 the
            // neighbours culled as "off-board" and the burst hit nothing. Now keyed
            // on the real grid: `grid::neighbors(hit_pos)` is edge-clamped (3 at a
            // corner, 5 on an edge, 8 interior). Faction-blind (hits allies too —
            // the "Unfriendly Fire" ruling). The hit cell itself is NOT re-damaged.
            // Splash origin is the hit cell so the directional shield reads the
            // burst as arriving from the detonation; `apply_damage_2d`'s
            // `direction_to` handles the diagonal `incoming_from` an 8-neighbour
            // splash produces (facing_zone is total over 8).
            if let Some(hit_pos) = Pos::from_index(hit_cell) {
                let dummy = dummy_weapon();
                for n in crate::grid::neighbors(hit_pos) {
                    if board.ship_at(n).is_some() {
                        apply_damage_2d(n, 1, hit_pos, &dummy, board, content);
                    }
                }
            }
        }
        WeaponMod::Incendiary => {
            // Rider lands on contact even if the shield ate the hull damage.
            add_status(hit_cell, StatusKind::HullBreach, 3, board);
        }
        WeaponMod::EmpCharge => {
            add_status(hit_cell, StatusKind::SystemsOffline, 3, board);
        }
        WeaponMod::TargetingLaser => {
            // TargetLock has no inherent duration in the doc (it is consumed by
            // the next hit). Use the same long-ish duration the demo uses so it
            // persists until consumed or it times out; `add_status` coalesces.
            add_status(hit_cell, StatusKind::TargetLock, 5, board);
        }
        WeaponMod::PrecisionCore => {
            // precision_core's cooldown recharge is NOT applied here. run_action
            // resets the action's cooldown to `cooldown_max` AFTER the effects
            // loop (the canonical post-effect bookkeeping), which would clobber
            // a recharge written during effects. So run_action handles the
            // recharge itself, post-bookkeeping — see the
            // `precision_core_killed` path there. This arm is a no-op; the
            // params are accepted for a uniform on-hit signature.
            let _ = (killed, atk_cell, attacker_id);
        }
        // Action-level mods are handled in run_action, not on-hit.
        WeaponMod::TwinLinked | WeaponMod::Autoloader => {}
    }
}

/* =============================================================================
 * Helpers — real implementations.
 * ========================================================================== */

/// All live ships on the board, cloned for snapshot iteration.
pub fn ships_of(board: &Board) -> Vec<Ship> {
    board.cells.iter().filter_map(std::clone::Clone::clone).collect()
}

/// Cells of every enemy ship, in lane order. The TS `enemyInitiative` says
/// "telegraphed order; here simply lane order. Replace with explicit
/// initiative." — preserved as-is.
pub fn enemy_initiative(board: &Board) -> Vec<usize> {
    board
        .cells
        .iter()
        .filter_map(|c| c.as_ref())
        .filter(|s| s.faction == Faction::Enemy)
        .map(|s| s.cell)
        .collect()
}

/// Which lane direction does `action`'s mount bear toward from `ship_cell`?
/// `None` means no direction bears — caller should treat this as "the action
/// can't pick a target this round". Mirrors the private `bearingDirection`
/// helper in `resolve.ts`.
fn bearing_direction(ship: &Ship, ship_cell: usize, board: &Board, a: &Action) -> Option<LaneEnd> {
    if a.targeting.requires_arc.is_none() {
        // Arc-less: pick whichever direction has a target.
        //
        // The TS uses `firstTargetToward({ size: ship.cell + 99 } as Board, ship, end)`
        // to probe lane cells without a real board — a TS escape hatch. The
        // intent is "scan the lane for the first occupant in this direction
        // without enforcing the real `board.size`". We get the same result
        // here by scanning the real board's `cells` directly. If neither
        // direction yields an occupant, TS defaults to `"fore"`; we match.
        for &end in &[LaneEnd::Fore, LaneEnd::Aft] {
            if first_target_toward(board, ship_cell, end).is_some() {
                return Some(end);
            }
        }
        return Some(LaneEnd::Fore);
    }
    let arc = a.targeting.requires_arc;
    for &end in &[LaneEnd::Fore, LaneEnd::Aft] {
        // TS probes `ship.cell ± 1` then calls `bears(ship, arc, probe)`, and
        // the ONLY thing `bears` does with `probe` is feed it to
        // `directionTo(ship.cell, probe)` — a pure sign test
        // (`b >= a ? fore : aft`). So the probe's magnitude never matters
        // here; only whether it sits at/above or below `ship.cell` does.
        //
        // At `ship_cell == 0` the aft probe is `-1` in TS, and
        // `directionTo(0, -1)` is `"aft"`. Our `bears`/`direction_to` take a
        // `usize`, so we cannot pass `-1`. Instead we compute the lane
        // direction from the SIGNED probe directly (mirroring `direction_to`'s
        // rule) and hand it to `arc_bears`. This drops the old `probe < 0`
        // special-case AND its non-canonical arc allowlist — TS has neither.
        // A Rear-arc (or Turret) weapon on a bow=fore ship at cell 0 now
        // correctly bears aft and fires, exactly as the TS engine does.
        let probe: i32 = match end {
            LaneEnd::Fore => ship_cell as i32 + 1,
            LaneEnd::Aft => ship_cell as i32 - 1,
        };
        // Mirror `direction_to(ship_cell, probe)`: `probe >= ship_cell` -> fore,
        // else aft.
        let probe_dir = if probe >= ship_cell as i32 {
            LaneEnd::Fore
        } else {
            LaneEnd::Aft
        };
        let bears_here = match arc {
            None => true, // arc-less is handled above; kept for parity with `bears`.
            Some(a) => crate::geometry::arc_bears(ship.orientation, a, probe_dir),
        };
        if bears_here {
            return Some(end);
        }
    }
    None
}

/// All lane cells strictly in `end` direction from `ship_cell`. Mirrors
/// `cellsToward` in `resolve.ts`.
fn cells_toward(board: &Board, ship_cell: usize, end: LaneEnd) -> Vec<usize> {
    let mut out = Vec::new();
    match end {
        LaneEnd::Fore => {
            let mut c = ship_cell + 1;
            while c < board.size {
                out.push(c);
                c += 1;
            }
        }
        LaneEnd::Aft => {
            if ship_cell == 0 {
                return out;
            }
            let mut c = ship_cell - 1;
            loop {
                out.push(c);
                if c == 0 {
                    break;
                }
                c -= 1;
            }
        }
    }
    out
}

/// First occupied cell in `end` direction from `ship_cell`, if any. Mirrors
/// `firstTargetToward` in `resolve.ts`.
fn first_target_toward(board: &Board, ship_cell: usize, end: LaneEnd) -> Option<usize> {
    cells_toward(board, ship_cell, end)
        .into_iter()
        .find(|c| board.cells[*c].is_some())
}

fn in_allowed_band(band: &[RangeBand], a: usize, b: usize) -> bool {
    band.contains(&range_band(a, b))
}

/// Add or extend a status on the ship at `cell`. Mirrors `addStatus` in
/// `resolve.ts`. If an entry with the same `kind` already exists, the
/// duration becomes `max(existing, new)`.
pub fn add_status(cell: usize, kind: StatusKind, duration: i32, board: &mut Board) {
    let Some(ship) = board.cells[cell].as_mut() else {
        return;
    };
    if let Some(existing) = ship.statuses.iter_mut().find(|s| s.kind == kind) {
        existing.duration = existing.duration.max(duration);
    } else {
        ship.statuses.push(Status {
            kind,
            duration,
            face: None,
        });
    }
}

/// Tick every status on the ship at `cell` by one turn; expire those whose
/// duration reaches 0. Mirrors `tickStatuses` in `resolve.ts`. Takes content
/// because a hull-breach kill routes through [`destroy`].
fn tick_statuses(cell: usize, board: &mut Board, content: &dyn Content) {
    let mut hull_breach_destroyed = false;
    if let Some(ship) = board.cells[cell].as_mut() {
        // Pre-tick effects: hullBreach does 1 damage per turn before its
        // duration decrements (matches TS order).
        let mut breach_hits = 0;
        for s in &ship.statuses {
            if s.kind == StatusKind::HullBreach {
                breach_hits += 1;
            }
        }
        ship.hull -= breach_hits;
        if ship.hull <= 0 {
            hull_breach_destroyed = true;
        }
        for s in &mut ship.statuses {
            s.duration -= 1;
        }
        ship.statuses.retain(|s| s.duration > 0);
    }
    if hull_breach_destroyed {
        destroy(cell, board, content);
    }
}

/// Does the ship at `cell` skip its turn this round? Mirrors `skipsTurn` in
/// `resolve.ts`. Today that's just `SystemsOffline`.
pub fn skips_turn(board: &Board, cell: usize) -> bool {
    board.cells[cell]
        .as_ref()
        .is_some_and(|s| {
            s.statuses
                .iter()
                .any(|s| s.kind == StatusKind::SystemsOffline)
        })
}

/// Destroy the ship at `cell`. Mirrors `destroy` in `resolve.ts`. Reactor-
/// breach trait deals 2 splash to both neighbours through the regular damage
/// pipeline (with a dummy "_impact" action so falloff is skipped).
///
/// `content` is threaded through so the splash hits go through the full
/// damage pipeline including subsystem modifiers — a `ReactorBreach` hitting
/// a flank could legitimately trigger a Marksman bonus.
pub fn destroy(cell: usize, board: &mut Board, content: &dyn Content) {
    // Pull the ship out of the cell. Reactor-breach trait check needs the
    // traits list, which we capture before the cell is cleared.
    let Some(ship) = board.cells[cell].take() else {
        return;
    };
    let has_reactor_breach = ship
        .traits
        .iter()
        .any(|t| matches!(t, crate::types::Trait::ReactorBreach));
    let owner_cell = cell;
    board.destroys_this_window += 1;

    if has_reactor_breach {
        let dummy = dummy_weapon();
        for delta in [-1i32, 1] {
            let nc = owner_cell as i32 + delta;
            if nc < 0 || (nc as usize) >= board.size {
                continue;
            }
            let nc = nc as usize;
            if board.cells[nc].is_some() {
                apply_damage(nc, 2, owner_cell, &dummy, board, content);
            }
        }
    }

    emit(board, Hook::OnLethal, |ctx| {
        ctx.target_cell = Some(owner_cell);
    });
}

const fn flip_orientation(o: Orientation) -> Orientation {
    match o {
        Orientation::BowOn { bow } => Orientation::BowOn { bow: opposite(bow) },
        Orientation::Broadside => Orientation::Broadside,
    }
}

/// Rotate a [`Facing`] one quarter-turn **clockwise** (#75 player rotate-RIGHT).
/// A `Bow` stance turns its bow `Dir4` (`N→E→S→W`); a `Broadside` stance swaps
/// its axis (the hull pivots from across-lane to along-lane). Total + pure.
const fn rotate_facing_cw(facing: crate::grid::Facing) -> crate::grid::Facing {
    use crate::grid::{Axis, Facing};
    match facing {
        Facing::Bow(d) => Facing::Bow(d.rotate_cw()),
        Facing::Broadside(Axis::NorthSouth) => Facing::Broadside(Axis::EastWest),
        Facing::Broadside(Axis::EastWest) => Facing::Broadside(Axis::NorthSouth),
    }
}

/// Rotate a [`Facing`] one quarter-turn **counter-clockwise** (#75 rotate-LEFT).
const fn rotate_facing_ccw(facing: crate::grid::Facing) -> crate::grid::Facing {
    use crate::grid::{Axis, Facing};
    match facing {
        Facing::Bow(d) => Facing::Bow(d.rotate_ccw()),
        Facing::Broadside(Axis::NorthSouth) => Facing::Broadside(Axis::EastWest),
        Facing::Broadside(Axis::EastWest) => Facing::Broadside(Axis::NorthSouth),
    }
}

/// Derive the legacy [`Orientation`] from the authoritative 2-D [`Facing`], for
/// the player rotation control (#75): the live combat + render key off `facing`,
/// so `facing` is the source of truth and `orientation` is kept as a consistent
/// shadow (the loft pose + HUD sprite-stance still read it). Uses the live
/// `make_ship`/spawn convention (capture.rs / broadside.rs): bow up-lane (away,
/// `Dir4::N`) → `BowOn { Fore }`; bow toward the camera (`Dir4::S`) → `BowOn
/// { Aft }`; the two flanks (`E`/`W`) → `Broadside`. This is the INVERSE of the
/// enemy-spawn-oriented [`crate::types::facing_from_orientation`] (which maps
/// `Fore → Bow(S)`); the player path uses the bin's construction convention so a
/// rotated player's `orientation` matches how its ship was built.
#[allow(clippy::match_same_arms)] // deliberate facing->orientation table; arms kept explicit
const fn orientation_from_facing(facing: crate::grid::Facing) -> Orientation {
    use crate::grid::{Dir4, Facing};
    match facing {
        Facing::Bow(Dir4::N) => Orientation::BowOn { bow: LaneEnd::Fore },
        Facing::Bow(Dir4::S) => Orientation::BowOn { bow: LaneEnd::Aft },
        Facing::Bow(Dir4::E | Dir4::W) => Orientation::Broadside,
        Facing::Broadside(_) => Orientation::Broadside,
    }
}

/// Resolver-owned fallback for the AI's synthetic lane-relative close-move
/// (#68). Returns a 1-cell lane-relative THRUST for the `__move_left` /
/// `__move_right` ids, `None` for anything else. Mirrors
/// [`crate::input::synthetic_move_left`] / `synthetic_move_right` (same ids,
/// same `direction: Some(LaneEnd::…)` lane-relative THRUST) so AI movement
/// resolves IDENTICALLY whether or not the running `Content` registers those
/// actions — the resolver does not depend on the demo/content layer to make
/// enemies close. Used by [`fire_player_queue`] when `content.action()`
/// returns `None` for one of these ids.
pub(crate) fn resolver_ai_move(action_id: &str) -> Option<Action> {
    // Shared SELF-targeted, zero-cost, all-bands shell for every resolver-served
    // AI synthetic (moves AND rotates) — same instant-apply shape the player's
    // input.rs synthetics use, so AI maneuvers resolve IDENTICALLY whether or not
    // the running `Content` registers these ids.
    let shell = |name: &str, effect: Effect| Action {
        id: action_id.to_string(),
        name: name.into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            range_band: vec![crate::grid::Range::Adjacent],
            optimal_range: crate::grid::Range::Adjacent,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![effect],
        r#mod: None,
        icon: None,
    };

    // ROTATE / FLIP synthetics (Q3 enemy rotate-to-bear): the AI turns a
    // mis-pointed hull to bring its arc onto the player. REORIENT effects,
    // mirroring input.rs `synthetic_rotate_left/right` / `synthetic_reorient_flip`
    // exactly so a resolver-served enemy rotate == the player's. WITHOUT these in
    // the serve-list the rotate ids the AI queues would resolve to `None` and the
    // enemy would never turn (the "camp + never fire" bug). The resolver's
    // REORIENT arm rotates `facing` (and re-derives `orientation`), so the firing
    // arc follows next phase.
    match action_id {
        crate::input::SYNTHETIC_ROTATE_LEFT => {
            return Some(shell(
                "Rotate Left",
                Effect::REORIENT {
                    to: ReorientTo::RotateLeft,
                },
            ));
        }
        crate::input::SYNTHETIC_ROTATE_RIGHT => {
            return Some(shell(
                "Rotate Right",
                Effect::REORIENT {
                    to: ReorientTo::RotateRight,
                },
            ));
        }
        crate::input::SYNTHETIC_REORIENT_FLIP => {
            return Some(shell(
                "Flip",
                Effect::REORIENT {
                    to: ReorientTo::Flip,
                },
            ));
        }
        _ => {}
    }

    // R6: the four cardinal synthetic-MOVE ids. The 1-D `direction` (LaneEnd)
    // stays for the dead-for-live 1-D path; `direction_2d` (Dir4) is the 2-D
    // override the live `resolve_self_move_2d` reads. self-derive both from the
    // id. The N/S ids have no 1-D LaneEnd analog (the 1-D lane had no depth
    // axis), so they map to the closest 1-D direction for the legacy fallback
    // while carrying the real Dir4 for 2-D.
    let (direction, direction_2d) = match action_id {
        crate::input::SYNTHETIC_MOVE_LEFT => (LaneEnd::Aft, Dir4::W),
        crate::input::SYNTHETIC_MOVE_RIGHT => (LaneEnd::Fore, Dir4::E),
        crate::input::SYNTHETIC_MOVE_UP => (LaneEnd::Aft, Dir4::N),
        crate::input::SYNTHETIC_MOVE_DOWN => (LaneEnd::Fore, Dir4::S),
        _ => return None,
    };
    Some(shell(
        "Move",
        Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(direction),
            // R6: the real 2-D cardinal, self-derived from the id; the live
            // resolve_self_move_2d moves 1 cell along it via grid::offset.
            direction_2d: Some(direction_2d),
        },
    ))
}

/// A throwaway weapon used by the resolver for unattributed damage (projectile
/// impact, `ReactorBreach` splash). Falloff is disabled via `bandFalloff: false`
/// so the projectile's payload `amount` lands as-is. Mirrors `dummyWeapon`.
fn dummy_weapon() -> Action {
    Action {
        id: "_impact".into(),
        name: "Impact".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: RangeBand::Mid,
            // v2 (A3 EXPAND): 2-D range mirror — all bands; falloff disabled via
            // the payload's bandFalloff:false, so optimal_range is nominal.
            range_band: vec![
                crate::grid::Range::Adjacent,
                crate::grid::Range::Near,
                crate::grid::Range::Far,
            ],
            optimal_range: crate::grid::Range::Near,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE {
            amount: 0,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/* =============================================================================
 * TODO stubs — content / AI layer.
 *
 * Each function is callable today; the body matches the TS reference's stub.
 * Content slice replaces the bodies atomically when the real rules land.
 * ========================================================================== */

/// Step 2 of the damage pipeline: add subsystem damage modifiers to `dmg`.
/// Mirrors the TS `applyModifiers` stub at `resolve.ts:371` ("sum subsystem
/// damage bonuses (Marksman, Point-Blank Doctrine, ...)"); now wired through
/// [`Content::damage_modifier`].
///
/// # Formula
///
/// `final_dmg = max(0, raw_falloff_dmg + Σ subsystem_bonus(target, band))`
///
/// - `raw_falloff_dmg` is the input from step 1 (post-falloff or raw if
///   `bandFalloff:false`).
/// - Subsystem bonuses are **additive** — there is no multiplicative tier —
///   matching the analysis doc's flat numeric bonuses (Marksman +1, Point-
///   Blank Doctrine +2 at pointBlank, etc.).
/// - Negative modifiers are allowed in principle (e.g. a future "stealth"
///   modifier might subtract); the result is clamped to `0` because step 4
///   (directional shield) and the band-falloff floor both already enforce
///   non-negative damage.
/// - Target-lock doubling (step 3) is applied to the AFTER-modifier value
///   per the TS comment at `resolve.ts:154-157`. So a +1 Marksman bonus
///   followed by a 2× lock makes the final hit `2*(raw_falloff + 1)`, not
///   `2*raw_falloff + 1`.
///
/// The default `Content::damage_modifier` impl returns 0, so this function
/// is a pass-through for all current test / demo content. Concrete Content
/// types that install subsystems (the real game loader, future tests)
/// override it to scan their registry.
///
/// **Audit #67:** `atk_cell` is the **attacker's** cell, not the target.
/// Subsystem damage bonuses are attacker-side per analysis HTML §VI
/// (the catalog descs all read "when firing" / "when striking" —
/// Marksman, PBD, Rear Gunner, Center Mass, Strafing Run). NOT the
/// target's subsystems. A future cross-cutting subsystem like Crossfire
/// (board-state predicate, owned by player, grants bonus to attacking
/// enemies) is out of scope for this trait; revisit when it lands.
fn apply_modifiers(
    dmg: i32,
    atk_cell: usize,
    band: crate::grid::Range,
    board: &Board,
    content: &dyn Content,
) -> i32 {
    let Some(attacker) = board.cells[atk_cell].as_ref() else {
        return dmg;
    };
    let bonus = content.damage_modifier(attacker, band, board);
    (dmg + bonus).max(0)
}

/// Resolve a `DISPLACE_SELF` effect: move the ship at `ship_cell` according
/// to `mode` and `distance`. Returns silently if the cell is empty.
///
/// # Mode semantics (analysis.html § Movement modes)
///
/// - `THRUST` — exactly 1 cell; blocked by occupancy. `distance` is ignored
///   beyond the first step (the TS catalog only ever uses `distance: 1` for
///   THRUST, but if a content author passes something larger we treat it as
///   THRUST is canonically one cell anyway).
/// - `BURN` — multi-cell; stops at first ship or wall. Collision damage on
///   stop.
/// - `SLIP` — passes through ships, lands in the **first free cell beyond**
///   the requested distance. If the lane runs out before a free cell is
///   found, the ship piles up at the edge and eats collision damage equal to
///   the overflow.
/// - `JUMP` — blink-drive; ignores the path entirely. Final landing cell is
///   `ship_cell + step * distance`. If that cell is off-board, the jump
///   clamps to the edge and eats collision damage equal to the overflow.
///   If the final cell is occupied, the jump fails (no-op) — telepathy
///   "ignores the path" so there is nothing to collide with mid-move.
/// - `TRACTOR_SWAP` — trades cells with the first adjacent occupant in the
///   bow direction. No collision damage (it is a tractor swap, not a ram).
///
/// # Collision rule
///
/// When `BURN` or `THRUST` is blocked, the ship stops one cell short of the
/// obstacle (which may be another ship OR the board edge) and takes
/// `remaining_distance × 1` collision damage, routed through the regular
/// damage pipeline so the directional shield still mediates. The damage is
/// attributed via [`dummy_weapon()`] with `bandFalloff: false`, so the raw
/// collision amount lands without range scaling.
///
/// # Direction
///
/// `direction` is the **Rust-port extension** added for player UX (see
/// [`Effect::DISPLACE_SELF`] doc). It overrides the bow-derived step:
///
/// - `Some(LaneEnd::Fore)` -> step +1
/// - `Some(LaneEnd::Aft)`  -> step -1
/// - `None` (the canonical TS semantics) — derive from `ship.orientation`:
///   - `BowOn { bow: Fore }` -> step +1
///   - `BowOn { bow: Aft }`  -> step -1
///   - `Broadside` -> step +1 (arbitrary; broadside ships rarely queue a
///     `DISPLACE_SELF`, and the design doc gives no preference; matches TS).
///
/// AI / scripted moves pass `direction: None` so behaviour matches the TS
/// engine bit-for-bit. Player synthetic Left/Right actions pass
/// `Some(Aft)` / `Some(Fore)` so the arrow keys are lane-relative.
///
/// **Dead-for-live (R6):** the live `apply_effect` `DISPLACE_SELF` arm now calls
/// [`resolve_self_move_2d`]; this 1-D version is retained only for its own 1-D
/// fixture tests (`self_move_*`) until CONTRACT deletes it (same expand-contract
/// shape as the 1-D `resolve_targeting`). `#[allow(dead_code)]` because it has no
/// non-test caller now — not unused, just superseded on the live path.
#[allow(dead_code)]
fn resolve_self_move(
    ship_cell: usize,
    mode: MovementMode,
    distance: i32,
    direction: Option<LaneEnd>,
    board: &mut Board,
    content: &dyn Content,
) {
    let Some(ship) = board.cells[ship_cell].as_ref() else {
        return;
    };
    let step: i32 = match direction {
        Some(LaneEnd::Fore) => 1,
        Some(LaneEnd::Aft) => -1,
        None => match ship.orientation {
            Orientation::BowOn { bow: LaneEnd::Aft } => -1,
            _ => 1,
        },
    };
    let size = board.size as i32;
    let start = ship_cell as i32;

    // Per-mode landing computation. `landing` is the cell to settle in;
    // `collision_dmg` is non-zero when the move was capped by a block.
    let (landing, collision_dmg): (i32, i32) = match mode {
        MovementMode::THRUST => {
            // Always one step. Distance argument is ignored; THRUST is
            // canonically 1.
            let next = start + step;
            if next < 0 || next >= size {
                // Wall blocks: stop in place, take 1 collision.
                (start, 1)
            } else if board.cells[next as usize].is_some() {
                // Occupant blocks: stop in place, take 1 collision.
                (start, 1)
            } else {
                (next, 0)
            }
        }

        MovementMode::BURN => {
            // Walk one cell at a time, stop at first occupant or wall.
            let mut c = start;
            let mut steps_taken = 0;
            for _ in 0..distance {
                let next = c + step;
                if next < 0 || next >= size {
                    break;
                }
                if board.cells[next as usize].is_some() {
                    break;
                }
                c = next;
                steps_taken += 1;
            }
            let remaining = distance - steps_taken;
            (c, remaining.max(0))
        }

        MovementMode::SLIP => {
            // Look distance cells ahead, then keep going until a free cell
            // is found OR we run off the lane. SLIP can land beyond
            // `start + step * distance` if every cell in that range is
            // occupied — that is what "lands in first free cell beyond"
            // means.
            let mut c = start;
            let mut scanned = 0;
            let mut found_free = false;
            // First pass: cover the `distance` cells we're slipping THROUGH.
            // We don't stop at occupants — we pass through them.
            while scanned < distance {
                let next = c + step;
                if next < 0 || next >= size {
                    break;
                }
                c = next;
                scanned += 1;
            }
            // Did the distance-cells-ahead landing happen to be free? If
            // not, keep walking until we find a free cell or hit the edge.
            loop {
                if c < 0 || c >= size {
                    break;
                }
                if board.cells[c as usize].is_none() {
                    found_free = true;
                    break;
                }
                let next = c + step;
                if next < 0 || next >= size {
                    break;
                }
                c = next;
            }
            if found_free {
                (c, 0)
            } else {
                // The lane ran out before a free cell appeared. Clamp to
                // the edge and bill collision damage equal to the cells
                // that DID NOT land — i.e. the requested distance minus
                // however many we actually advanced before getting stuck.
                let edge = if step > 0 { size - 1 } else { 0 };
                let advanced = (edge - start) * step; // always >= 0
                let remaining = (distance - advanced).max(1);
                (edge, remaining)
            }
        }

        MovementMode::JUMP => {
            // Blink-drive. Compute the target cell directly; no path scan.
            let raw_target = start + step * distance;
            if raw_target < 0 {
                // Off-board aft: clamp to 0, charge overflow as collision.
                (0, -raw_target)
            } else if raw_target >= size {
                // Off-board fore: clamp to edge, charge overflow.
                (size - 1, raw_target - (size - 1))
            } else if board.cells[raw_target as usize].is_some() {
                // Target cell occupied: jump fails. No move, no damage —
                // JUMP "ignores the path entirely" so there is nothing
                // physical to collide with.
                (start, 0)
            } else {
                (raw_target, 0)
            }
        }

        MovementMode::TRACTOR_SWAP => {
            // Swap with the first adjacent occupant in the bow direction.
            // Coordinated with team-lead: the analysis doc says
            // TRACTOR_SWAP "trades cells with a target" and the only
            // DISPLACE_SELF carrier today is the Frigate's Slip signature
            // / the Carrier's Swap-Toss, both of which target the ship
            // directly fore-of-bow. If there is no adjacent occupant, the
            // swap fails silently — there is nothing to trade with.
            let adj = start + step;
            if adj < 0 || adj >= size {
                return;
            }
            let adj = adj as usize;
            if board.cells[adj].is_none() {
                return;
            }
            // Perform the swap. Both ships' `cell` fields update; the cells
            // vector's contents swap by index.
            let source_ship = board.cells[ship_cell].take();
            let other_ship = board.cells[adj].take();
            if let Some(mut s) = source_ship {
                s.cell = adj;
                board.cells[adj] = Some(s);
            }
            if let Some(mut o) = other_ship {
                o.cell = ship_cell;
                board.cells[ship_cell] = Some(o);
            }
            return;
        }
    };

    let final_cell = landing as usize;
    // Move the ship into the landing cell. Skip the vec swap if we didn't
    // actually move — the cell value still needs to be updated on the ship
    // record, but since `cell == ship_cell` there's nothing to copy.
    if landing != start {
        let mut ship = board.cells[ship_cell]
            .take()
            .expect("source still occupied at start");
        ship.cell = final_cell;
        board.cells[final_cell] = Some(ship);
    }

    // Apply collision damage AFTER the move is committed, so the directional
    // shield reads against the ship's new (post-move) orientation. The
    // collision arrives from the direction we were travelling — i.e. from
    // beyond the landing cell — so `atk_cell` is one further in `step`.
    if collision_dmg > 0 {
        let phantom_atk = (landing + step).clamp(0, size - 1) as usize;
        apply_damage(
            final_cell,
            collision_dmg,
            phantom_atk,
            &dummy_weapon(),
            board,
            content,
        );
    }
}

/// 2-D `resolve_self_move` (blueprint R6). The v2 port of the 1-D
/// [`resolve_self_move`] above, over the real grid + the Board EXPAND occupancy
/// seam. Same expand-contract shape as `resolve_targeting_2d`: the 1-D fn stays
/// (its fixture tests untouched) until CONTRACT; only the live `apply_effect`
/// arm + `resolver_ai_move` switch to this.
///
/// ## Direction
///
/// `direction_2d` is the 2-D analog of the 1-D `direction: Option<LaneEnd>`
/// override:
/// - `Some(dir)` — move along that exact cardinal (player UX / the AI's
///   synthetic close-move set its cardinal here).
/// - `None` — derive from the ship's [`Facing`]: `Bow(d)` -> `d`;
///   `Broadside(axis)` -> the axis's increasing-coordinate direction
///   (`dirs().0`), mirroring the 1-D `Broadside -> step +1` default (a broadside
///   ship rarely self-moves; this is a stable convention, not a physical claim).
///
/// All movement is a **cardinal** `grid::offset` walk (decision #9; the
/// `Dir4 -> Dir8` is always a cardinal). Bounds = off-grid (`offset` -> `None`);
/// occupancy = [`Board::ship_at`]. On a real move the ship's slot AND `.pos` are
/// updated together to preserve Board invariant (A) (`slot == pos.to_index()`).
/// Collision damage routes through the unchanged 1-D `apply_damage` via
/// `to_index()` (correct under invariant (A); migrates to `Pos` with the
/// damage pipeline).
fn resolve_self_move_2d(
    ship_pos: Pos,
    mode: MovementMode,
    distance: i32,
    direction_2d: Option<Dir4>,
    direction_1d: Option<LaneEnd>,
    board: &mut Board,
    content: &dyn Content,
) {
    let Some(ship) = board.ship_at(ship_pos) else {
        return;
    };
    // Resolve the cardinal move direction, in precedence order:
    //   1. `direction_2d` — an explicit 2-D cardinal override (if a content
    //      author ever sets one; nothing does today).
    //   2. `direction_1d == Some(Aft)` — the facing-RELATIVE aft hurl (throw-2d).
    //      `direction_2d` can't encode "aft" as a static cardinal because aft
    //      depends on the ship's runtime facing; so `throw`'s canonical
    //      `direction: Aft` is honoured HERE, computing the opposite-of-bow
    //      cardinal at resolve time. `Some(Fore)` is the default forward step
    //      (no-op vs facing) and falls through.
    //   3. facing-forward (the bow, or a Broadside axis's positive cardinal).
    let forward: Dir4 = match ship.facing {
        Facing::Bow(d) => d,
        Facing::Broadside(axis) => axis.dirs().0,
    };
    let dir: Dir8 = match (direction_2d, direction_1d) {
        (Some(d), _) => d.to_dir8(),
        (None, Some(LaneEnd::Aft)) => forward.opposite().to_dir8(),
        _ => forward.to_dir8(),
    };

    // Per-mode landing computation. `landing` is the destination cell;
    // `collision_dmg` is non-zero when a wall/occupant capped the move.
    // (Mirrors the 1-D modes, with grid::offset walks replacing start+step*k.)
    let (landing, collision_dmg): (Pos, i32) = match mode {
        MovementMode::THRUST => {
            // Always one cardinal step; distance ignored (THRUST is 1).
            match crate::grid::offset(ship_pos, dir, 1) {
                None => (ship_pos, 1), // wall: stop, 1 collision
                Some(next) if board.ship_at(next).is_some() => (ship_pos, 1), // occupant: stop, 1
                Some(next) => (next, 0),
            }
        }

        MovementMode::BURN => {
            // Walk one cell at a time, stop at first occupant or wall.
            let mut cur = ship_pos;
            let mut steps_taken = 0;
            for _ in 0..distance {
                let Some(next) = crate::grid::offset(cur, dir, 1) else {
                    break;
                };
                if board.ship_at(next).is_some() {
                    break;
                }
                cur = next;
                steps_taken += 1;
            }
            (cur, (distance - steps_taken).max(0))
        }

        MovementMode::SLIP => {
            // Pass THROUGH `distance` cells, then keep going to the first free
            // cell (or the edge). Lands beyond start+distance if that range is
            // all occupied.
            let mut cur = ship_pos;
            let mut scanned = 0;
            while scanned < distance {
                let Some(next) = crate::grid::offset(cur, dir, 1) else {
                    break;
                };
                cur = next;
                scanned += 1;
            }
            // Now walk to the first free cell.
            loop {
                if board.ship_at(cur).is_none() {
                    return self_move_2d_commit(ship_pos, cur, 0, dir, board, content);
                }
                let Some(next) = crate::grid::offset(cur, dir, 1) else {
                    // Ran off the lane before a free cell — clamp at `cur` (last
                    // in-bounds, occupied) is wrong; the 1-D version clamps to the
                    // edge it reached. `cur` IS the last in-bounds cell; bill 1
                    // collision (no free landing). Settle on the last free cell
                    // we passed is not tracked, so stay put (no valid landing).
                    return self_move_2d_commit(ship_pos, ship_pos, 1, dir, board, content);
                };
                cur = next;
            }
        }

        MovementMode::JUMP => {
            // Blink: compute the target directly, no path scan.
            match crate::grid::offset(ship_pos, dir, distance) {
                None => {
                    // Off-board: 1-D clamps to the edge + bills overflow. In 2-D
                    // "the edge along `dir`" isn't a single cell; settle on the
                    // farthest in-bounds cell along `dir` and bill the shortfall.
                    let mut cur = ship_pos;
                    let mut adv = 0;
                    while let Some(next) = crate::grid::offset(cur, dir, 1) {
                        cur = next;
                        adv += 1;
                    }
                    (cur, (distance - adv).max(1))
                }
                Some(target) if board.ship_at(target).is_some() => (ship_pos, 0), // occupied: jump fails
                Some(target) => (target, 0),
            }
        }

        MovementMode::TRACTOR_SWAP => {
            // Swap with the first adjacent occupant along `dir`.
            let Some(adj) = crate::grid::offset(ship_pos, dir, 1) else {
                return;
            };
            if board.ship_at(adj).is_none() {
                return;
            }
            let i = ship_pos.to_index();
            let j = adj.to_index();
            let mut source_ship = board.cells[i].take();
            let mut other_ship = board.cells[j].take();
            if let Some(s) = source_ship.as_mut() {
                s.cell = j;
                s.pos = adj;
            }
            if let Some(o) = other_ship.as_mut() {
                o.cell = i;
                o.pos = ship_pos;
            }
            board.cells[j] = source_ship;
            board.cells[i] = other_ship;
            return;
        }
    };

    self_move_2d_commit(ship_pos, landing, collision_dmg, dir, board, content);
}

/// Commit a 2-D self-move: relocate the ship `from`->`to` (updating slot AND
/// `.pos`/`.cell` together for invariant (A)), then bill any collision damage
/// from one cell beyond `to` along `dir`. Factored out so the SLIP early-returns
/// share the move+collision tail with the fall-through modes.
fn self_move_2d_commit(
    from: Pos,
    to: Pos,
    collision_dmg: i32,
    dir: Dir8,
    board: &mut Board,
    content: &dyn Content,
) {
    if to != from {
        // Bounds-safe: a real board is len CELLS (grid::offset never escapes
        // 0..CELLS), but defensively never panic — if `to`'s slot is past the
        // board's actual `cells` length (a short legacy/test board), the move
        // can't land, so leave the ship in place rather than index OOB.
        let (fi, ti) = (from.to_index(), to.to_index());
        if ti >= board.cells.len() || fi >= board.cells.len() {
            return;
        }
        let mut ship = board.cells[fi]
            .take()
            .expect("source still occupied at move start");
        ship.cell = ti;
        ship.pos = to;
        board.cells[ti] = Some(ship);
    }
    if collision_dmg > 0 {
        // Collision arrives from beyond the landing cell along the travel
        // direction; the phantom attacker is one step further (clamped to `to`
        // if that is off-grid, so direction_to(to, phantom) still yields the
        // travel axis -> the collision hits the face toward `dir`).
        //
        // R4: now routes through the 2-D apply_damage_2d, so the directional-
        // shield ZONE is the TRUE 2-D collision face (was provisional on the 1-D
        // path, which mis-read the flat index as a lane position).
        let phantom = crate::grid::offset(to, dir, 1).unwrap_or(to);
        apply_damage_2d(to, collision_dmg, phantom, &dummy_weapon(), board, content);
    }
}

/// Resolve a `DISPLACE_TARGET` effect: move the ship at `target_cell` per
/// `mode`. The source ship (the action's owner) is at `source_cell` — its
/// position determines push/pull direction.
///
/// # Mode semantics
///
/// - `Push` — target moves AWAY from `source_cell`. Stops at first occupant
///   or wall and takes `remaining_distance × 1` collision damage routed
///   through [`dummy_weapon()`] so the directional shield mediates.
/// - `Pull` — target moves TOWARD `source_cell`, stopping one cell short of
///   the source itself OR at the first intervening occupant / wall.
///   Collision rule applies the same way.
/// - `Swap` — `target_cell` and `source_cell` exchange occupants. No
///   collision damage (it is a controlled trade, not a ram). If either cell
///   is empty the swap silently fails.
///
/// Mirrors what would have been `resolveTargetMove` in `resolve.ts` — the TS
/// body was a stub.
///
/// **Dead-for-live (R6b):** the live `apply_effect` `DISPLACE_TARGET` arm now calls
/// [`resolve_target_move_2d`]; this 1-D version is retained only for its own 1-D
/// fixture tests until CONTRACT (Shape 2, like `resolve_self_move`).
#[allow(dead_code)]
// The nested `match mode { Push.. Pull.. _ => unreachable!() }` triggers
// match_same_arms + match_wildcard_for_single_variants; the proper fix (match
// the two modes in the outer arm) is review #148 finding M2, owned by the
// resolver. Allowed here to keep the gate green without restructuring the
// displacement logic in this cleanup pass.
#[allow(clippy::match_same_arms, clippy::match_wildcard_for_single_variants)]
fn resolve_target_move(
    target_cell: usize,
    source_cell: usize,
    mode: crate::types::DisplaceMode,
    distance: i32,
    board: &mut Board,
    content: &dyn Content,
) {
    use crate::types::DisplaceMode;
    if board.cells[target_cell].is_none() {
        return;
    }

    let size = board.size as i32;
    let start = target_cell as i32;
    let src = source_cell as i32;

    match mode {
        DisplaceMode::Swap => {
            // Trade cells. If source == target (degenerate), no-op.
            if source_cell == target_cell {
                return;
            }
            let a = board.cells[source_cell].take();
            let b = board.cells[target_cell].take();
            if let Some(mut s) = a {
                s.cell = target_cell;
                board.cells[target_cell] = Some(s);
            }
            if let Some(mut t) = b {
                t.cell = source_cell;
                board.cells[source_cell] = Some(t);
            }
        }

        DisplaceMode::Push | DisplaceMode::Pull => {
            // Direction depends on mode:
            //   Push: target moves AWAY from source -> step = sign(target - source)
            //   Pull: target moves TOWARD source     -> step = sign(source - target)
            // Tie-breaker: if source == target (degenerate), pick +1 for both.
            let step: i32 = match mode {
                DisplaceMode::Push => match start.cmp(&src) {
                    std::cmp::Ordering::Greater => 1,
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 1,
                },
                DisplaceMode::Pull => match src.cmp(&start) {
                    std::cmp::Ordering::Greater => 1,
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 1,
                },
                _ => unreachable!(),
            };

            // Walk step-by-step. Stop at first occupant OR at the source
            // cell (you cannot pull a target onto the source — they would
            // share a cell). Pull therefore stops one cell short of source.
            let mut c = start;
            let mut steps_taken = 0;
            for _ in 0..distance {
                let next = c + step;
                if next < 0 || next >= size {
                    break;
                }
                if board.cells[next as usize].is_some() {
                    // The source ship counts as an occupant too — pull
                    // crashes the target into the operator, which is the
                    // canonical collision behaviour. So we don't special-
                    // case source here.
                    break;
                }
                c = next;
                steps_taken += 1;
            }
            let remaining = distance - steps_taken;
            let landing = c as usize;

            // Move the target if it actually moved.
            if landing != target_cell {
                let mut t = board.cells[target_cell]
                    .take()
                    .expect("target still occupied at start of move");
                t.cell = landing;
                board.cells[landing] = Some(t);
            }

            // Collision damage if we were blocked.
            if remaining > 0 {
                let phantom_atk = (c + step).clamp(0, size - 1) as usize;
                apply_damage(
                    landing,
                    remaining,
                    phantom_atk,
                    &dummy_weapon(),
                    board,
                    content,
                );
            }
        }
    }
}

/// 2-D `resolve_target_move` (blueprint R6b). The v2 port of [`resolve_target_move`]
/// above — the `DISPLACE_TARGET` (push / pull / swap) mover — over the grid + the
/// Board invariant (A). Same expand-contract shape as the rest of the R-series:
/// the 1-D version stays (its fixture tests) until CONTRACT; the live
/// `apply_effect` `DISPLACE_TARGET` arm switches here.
///
/// Direction is derived 2-D: PUSH moves the target AWAY from the source
/// (`direction_to(source_pos, target_pos)`), PULL TOWARD the source
/// (`direction_to(target_pos, source_pos)` = the opposite), via `grid::offset`
/// cardinal/diagonal walks. Stops at the first occupant ([`Board::ship_at`]) or
/// off-grid; Pull crashing the target into the operator is the canonical
/// collision (source counts as an occupant). Moves update the target's slot AND
/// `.pos`/`.cell` together (invariant A); bounds-safe (no OOB panic on short
/// boards). Collision routes through [`apply_damage`] for now (provisional
/// shield-zone, like R6's self-move was) — R4 switches it to `apply_damage_2d`
/// for the true 2-D collision face.
// Same nested displace-mode match as the 1-D version; the structural fix is
// review #148 M2 (resolver-owned). Allowed here so this cleanup pass keeps the
// gate green without touching the displacement step logic.
#[allow(clippy::match_same_arms, clippy::match_wildcard_for_single_variants)]
fn resolve_target_move_2d(
    target_pos: Pos,
    source_pos: Pos,
    mode: crate::types::DisplaceMode,
    distance: i32,
    board: &mut Board,
    content: &dyn Content,
) {
    use crate::types::DisplaceMode;
    if board.ship_at(target_pos).is_none() {
        return;
    }

    match mode {
        DisplaceMode::Swap => {
            // Trade cells. Degenerate (source == target) = no-op.
            if source_pos == target_pos {
                return;
            }
            let (i, j) = (target_pos.to_index(), source_pos.to_index());
            if i >= board.cells.len() || j >= board.cells.len() {
                return; // off-board (short test board) — can't swap
            }
            let mut t = board.cells[i].take();
            let mut s = board.cells[j].take();
            if let Some(t) = t.as_mut() {
                t.cell = j;
                t.pos = source_pos;
            }
            if let Some(s) = s.as_mut() {
                s.cell = i;
                s.pos = target_pos;
            }
            board.cells[j] = t;
            board.cells[i] = s;
        }

        DisplaceMode::Push | DisplaceMode::Pull => {
            // Push: away from source. Pull: toward source (opposite). Same-cell
            // source/target is degenerate -> no meaningful direction; bail.
            let dir = match mode {
                DisplaceMode::Push => crate::geometry2d::direction_to(source_pos, target_pos),
                DisplaceMode::Pull => crate::geometry2d::direction_to(target_pos, source_pos),
                _ => unreachable!(),
            };
            let Some(dir) = dir else {
                return; // source == target, no direction
            };

            // Walk step-by-step from the target; stop at first occupant or wall.
            // (For Pull, the source ship is an occupant -> the target crashes
            // into the operator, the canonical collision.)
            let mut cur = target_pos;
            let mut steps_taken = 0;
            for _ in 0..distance {
                let Some(next) = crate::grid::offset(cur, dir, 1) else {
                    break;
                };
                if board.ship_at(next).is_some() {
                    break;
                }
                cur = next;
                steps_taken += 1;
            }
            let remaining = distance - steps_taken;

            // Move the target if it actually moved (slot + pos together).
            if cur != target_pos {
                let (from_i, to_i) = (target_pos.to_index(), cur.to_index());
                if to_i < board.cells.len() && from_i < board.cells.len() {
                    let mut t = board.cells[from_i]
                        .take()
                        .expect("target still occupied at move start");
                    t.cell = to_i;
                    t.pos = cur;
                    board.cells[to_i] = Some(t);
                }
            }

            // Collision damage if blocked. The collision arrives from beyond the
            // landing cell along the travel direction; phantom attacker one step
            // further (clamped to `cur` if off-grid). R4: routes through the 2-D
            // apply_damage_2d, so the directional-shield ZONE is the true 2-D
            // collision face (direction_to(cur, phantom) yields the travel axis).
            if remaining > 0 {
                let phantom = crate::grid::offset(cur, dir, 1).unwrap_or(cur);
                apply_damage_2d(cur, remaining, phantom, &dummy_weapon(), board, content);
            }
        }
    }
}

/// Was the just-finished execution window a chain kill? A "window" is one
/// `execute_queue` call OR one ordnance-phase pass; both reset
/// [`Board::destroys_this_window`] to zero at their start, and `destroy()`
/// increments it. `>= 2` destroys in the same window means a chain.
///
/// Mirrors `detectChain` in `resolve.ts` (which is `TODO: count destroys
/// within this execution window; >=2 is a chain kill.`). The counter is the
/// runtime field architect added on `Board` for exactly this purpose; the
/// resets are inserted at the two window boundaries above.
const fn detect_chain(board: &Board) -> bool {
    board.destroys_this_window >= 2
}

/* =============================================================================
 * Tests — one sanity assert per pure function. Deeper coverage comes from
 * `broadside-tester`.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    // The shared 1-D-legacy helpers (`make_ship`, the dead `apply_damage`
    // fixtures) use the 1-D profile shape ({armour, charge:0}); the 2-D
    // pool-soak tests that need a CHARGED pool build it explicitly (see
    // `pooled_shield_profile` below).
    use crate::geometry::default_shield_profile;
    // `Arc` is imported here (not at module top) because every non-doc use of
    // it lives in this test module — importing it at the top would be an
    // `unused_imports` warning in the non-test build (the resolver's only
    // former top-level use was the cell-0 arc allowlist removed in #96).
    use crate::types::{ActionCost, Arc, EventBus, Mount, Orientation, ShieldProfile};
    use std::collections::HashMap;

    /// Empty content for tests that don't invoke action lookups or spawns.
    struct NoContent;
    impl Content for NoContent {
        fn action(&self, _id: &str) -> Option<&Action> {
            None
        }
        fn spawn_projectile(&self, _kind: &str, _owner: &Ship) -> Projectile {
            panic!("spawn_projectile not used in this test");
        }
    }

    fn make_ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
        // R3 green-keep: map the 1-D `cell`/`bow` onto a 2-D-coherent
        // pos/facing so these legacy lane fixtures stay green on the
        // now-2-D live firing path (run_action -> resolve_targeting_2d on
        // Board::ship_at). `Pos::from_index(cell)` lands the lane onto row 0's
        // E-W axis (cells 0..4 -> cols 0..4) and SATISFIES invariant (A)
        // (`pos.to_index() == cell`, the slot make_board places the ship at).
        // `bow` Fore/Aft -> Bow(E)/Bow(W) so a Forward-arc attacker at the low
        // cell bears E along the lane exactly as the 1-D tests expect.
        //
        // !! TEST-ONLY ROTATION — NOT the canonical orientation->facing map !!
        // Fore->Bow(E) here is a FIXTURE CONVENIENCE rotating the 1-D lane onto
        // the row-0 E-W axis so legacy Forward shots keep bearing. It is
        // DELIBERATELY DIFFERENT from the real spawn mapping
        // `types::facing_from_orientation` (Fore->Bow(S), toward the player down
        // the depth axis). Do NOT read Fore->E as canonical facing — the live
        // game uses facing_from_orientation / C4's position-derived facing.
        // (The proper multi-row 2-D fixtures + real-target asserts are the
        // tester's T-follow #20; this is the minimal green-keep, not the rewrite.)
        let pos = crate::grid::Pos::from_index(cell).unwrap_or(crate::grid::Pos::new(0, 0));
        let facing = match bow {
            LaneEnd::Fore => crate::grid::Facing::Bow(crate::grid::Dir4::E),
            LaneEnd::Aft => crate::grid::Facing::Bow(crate::grid::Dir4::W),
        };
        Ship {
            id: id.into(),
            faction,
            cell,
            pos,
            orientation: Orientation::BowOn { bow },
            facing,
            hull,
            max_hull: hull,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: vec![Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            }],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    /// The #103 Model A 2-D SHIELD POOL profile (bow {4,4} / flanks {3,3} /
    /// stern {1,1}, pools start FULL). Used by the 2-D tests that exercise the
    /// live pool-soak path (`crate::geometry2d::absorb_shield`); the 1-D-legacy
    /// `make_ship`/`apply_damage` fixtures keep the old `default_shield_profile`.
    fn pooled_shield_profile() -> ShieldProfile {
        crate::geometry2d::default_shield_profile()
    }

    fn pulse_laser() -> Action {
        Action {
            id: "pulse_laser".into(),
            name: "Pulse Laser".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost {
                heat: 1,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
                optimal_band: RangeBand::Close,
                range_band: vec![crate::grid::Range::Adjacent, crate::grid::Range::Near],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: Some(Arc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE {
                amount: 4,
                band_falloff: None,
            }],
            r#mod: None,
            icon: None,
        }
    }

    fn make_board(size: usize, cells: Vec<Option<Ship>>) -> Board {
        Board {
            size,
            cells,
            ordnance: Vec::new(),
            hazards: (0..size).map(|_| Vec::new()).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        }
    }

    /* =====================================================================
     * 2-D invariant-A fixtures (the #20 run_action rewrite).
     *
     * Mirror of the `tests/common` board_2d/ship_2d shape, ported inline
     * because a src `#[cfg(test)]` mod can't pull in the `tests/` integration
     * submodule. These place ships at real grid positions with explicit
     * bearing facings (invariant A: ship.cell == ship.pos.to_index()), so the
     * 2-D firing path (resolve_targeting_2d) + 2-D damage (apply_damage_2d)
     * the live `run_action`/`fire_player_queue` path runs actually bear and the
     * 2-D Range falloff / facing_zone apply. SUPERSEDES the make_ship row-0
     * green-keep (Fore->Bow(E)) for the run_action tests.
     * ================================================================== */

    /// A `Ship` at a real 2-D `pos` with bearing `facing`, one `arc`-mount
    /// loaded with `weapon`. `shield` lets a test route a hit onto a known
    /// face. Upholds invariant A. `heat_max` generous (12) so no accidental
    /// lockout; override fields on the returned ship as needed.
    ///
    /// Richer than the bare `ship_2d` in the `resolve_targeting_2d` sanity section
    /// below (which hardcodes hull/weapon) — the `run_action` tests need to set
    /// hull, weapon id, and a specific shield profile, so this is its own
    /// builder rather than overloading that one.
    #[allow(clippy::too_many_arguments)] // a fixture builder; explicit params mirror tests/common::ship_2d
    fn armed_ship_2d(
        id: &str,
        faction: Faction,
        pos: crate::grid::Pos,
        hull: i32,
        facing: crate::grid::Facing,
        arc: Arc,
        weapon: &str,
        shield: ShieldProfile,
    ) -> Ship {
        Ship {
            id: id.into(),
            faction,
            cell: pos.to_index(), // invariant A
            pos,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing,
            hull,
            max_hull: hull,
            heat: 0,
            heat_max: 12,
            locked_out: false,
            shield_profile: shield,
            mounts: vec![Mount {
                id: format!("{id}-m1"),
                arc,
                weapon: weapon.into(),
            }],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    /// All-zero shield faces, so a hit lands raw on hull (legible arithmetic).
    fn naked() -> ShieldProfile {
        ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        }
    }

    /// Build a `Board` over the fixed `CELLS`-length 5x4 grid, placing each
    /// ship at `cells[ship.pos.to_index()]` (invariant A). Panics on a
    /// double-booked cell or an out-of-bounds ship — a fixture authoring bug
    /// surfaced loudly. `size = COLS` (the grid width) so any residual 1-D
    /// `board.size` reader sees a sane lane.
    fn armed_board_2d(ships: Vec<Ship>) -> Board {
        let mut cells: Vec<Option<Ship>> = (0..crate::grid::CELLS).map(|_| None).collect();
        for s in ships {
            assert!(
                s.pos.in_bounds(),
                "ship {} pos {:?} out of bounds",
                s.id,
                s.pos
            );
            let idx = s.pos.to_index();
            assert_eq!(s.cell, idx, "ship {} breaks invariant A", s.id);
            assert!(cells[idx].is_none(), "two ships share cell {idx}");
            cells[idx] = Some(s);
        }
        Board {
            size: crate::grid::COLS,
            cells,
            ordnance: Vec::new(),
            hazards: (0..crate::grid::CELLS).map(|_| Vec::new()).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        }
    }

    /// Demo Scenario A: scout bow=fore -> weak STERN faces attacker.
    /// Distance 1 = pointBlank; weapon optimal=close (delta 1) -> factor
    /// 0.66 -> floor(4 * 0.66) = 2. Stern armour 0 -> 2 lands. 5 - 2 = 3.
    /// This is the exact math demo.ts exercises — the orientation contrast
    /// against scenario B is the load-bearing point, not "4 lands".
    #[test]
    fn apply_damage_weak_stern_takes_post_falloff_hit() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        let scout_hull = board.cells[1].as_ref().map(|s| s.hull);
        assert_eq!(scout_hull, Some(3));
    }

    /// Demo Scenario B: scout bow=aft -> strong BOW faces attacker.
    /// Post-falloff damage 2 (see scenario A); bow armour 2 -> max(0, 2-2)
    /// = 0 lands. Hull stays at 5.
    #[test]
    fn apply_damage_strong_bow_soaks_to_zero() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Aft);
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        let scout_hull = board.cells[1].as_ref().map(|s| s.hull);
        assert_eq!(scout_hull, Some(5));
    }

    /// Target-lock doubles the post-falloff, pre-shield damage and is
    /// consumed exactly once.
    #[test]
    fn apply_damage_target_lock_doubles_and_consumes() {
        let mut scout = make_ship("scout", Faction::Enemy, 1, 20, LaneEnd::Fore);
        scout.statuses.push(Status {
            kind: StatusKind::TargetLock,
            duration: 5,
            face: None,
        });
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        let scout = board.cells[1].as_ref().unwrap();
        // distance 1 = pointBlank, optimal=close: floor(4 * 0.66) = 2.
        // 2 (post falloff) * 2 (target lock) = 4, stern armour 0 -> 4 lands.
        // 20 - 4 = 16.
        assert_eq!(scout.hull, 16);
        // Lock consumed.
        assert!(scout
            .statuses
            .iter()
            .all(|s| s.kind != StatusKind::TargetLock));
    }

    /// Lethal damage clears the cell and emits no further hits. Uses
    /// `bandFalloff: false` so the raw amount lands without scaling.
    #[test]
    fn apply_damage_lethal_clears_the_cell() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 3, LaneEnd::Fore);
        scout.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let mut weapon = pulse_laser();
        weapon.effects = vec![Effect::DAMAGE {
            amount: 4,
            band_falloff: Some(false),
        }];
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        assert!(
            board.cells[1].is_none(),
            "cell should be cleared after lethal damage"
        );
        assert_eq!(board.destroys_this_window, 1);
    }

    /// Heat accumulates and lockout fires at heatMax. Cooldown is reset
    /// unconditionally on the firing action. (#20 2-D fixture: attacker at
    /// (2,1) Bow(S), scout directly ahead at (2,2) — distance 1 (Adjacent), in
    /// `pulse_laser`'s band, on the bearing ray, so the shot connects and the
    /// heat/lockout/cooldown bookkeeping runs through the live 2-D fire path.)
    #[test]
    fn execute_queue_overheats_and_records_cooldown() {
        let mut attacker = armed_ship_2d(
            "frigate",
            Faction::Player,
            crate::grid::Pos::new(2, 1),
            10,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "pulse_laser",
            default_shield_profile(),
        );
        attacker.heat = 5;
        attacker.heat_max = 6;
        attacker.queue = vec!["pulse_laser".into()];
        let scout = armed_ship_2d(
            "scout",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "pulse_laser",
            default_shield_profile(),
        );
        let mut board = armed_board_2d(vec![attacker, scout]);
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        let content = OneAction(pulse_laser());
        fire_player_queue("frigate", &mut board, &content);
        let p = board.cells[crate::grid::Pos::new(2, 1).to_index()]
            .as_ref()
            .unwrap();
        assert_eq!(p.heat, 6, "heat should be 5 + 1");
        assert!(p.locked_out, "heat at heat_max triggers lockout");
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(0));
        assert!(
            p.queue.is_empty(),
            "queue should be cleared after execution"
        );
    }

    /// Regression for task #52: three queued THRUST actions all execute,
    /// even though each one moves the ship to a different cell. Pre-fix,
    /// `execute_queue` keyed off a stale `ship_cell: usize` parameter,
    /// so the second iteration's gate read `board.cells[0]` after the
    /// ship had moved away and bailed via early-return — only the FIRST
    /// thrust ran. Post-fix the loop re-resolves the ship's current
    /// cell via `find_cell_by_id` at every read.
    ///
    /// Scenario mirrors tester's repro for `tests/controls.rs::three_thrusts_then_commit_moves_three_cells`:
    /// player at cell 0 (bow=fore, so THRUST moves +1 / fore), 7-cell lane,
    /// three `__thrust` actions queued. After one `execute_queue` call,
    /// the ship is at cell 3 and the queue is drained.
    #[test]
    fn execute_queue_keeps_executing_after_ship_moves() {
        // A bare-bones DISPLACE_SELF THRUST action with no falloff /
        // targeting concerns. `direction: None` -> resolver derives step
        // from bow (Fore), so cell advances by +1.
        let thrust = Action {
            id: "__thrust".into(),
            name: "Thrust".into(),
            archetype: WeaponArchetype::Movement,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![crate::grid::Range::Adjacent],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
                direction_2d: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "__thrust").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }

        let mut player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        player.queue = vec!["__thrust".into(), "__thrust".into(), "__thrust".into()];
        let mut board = make_board(7, vec![Some(player), None, None, None, None, None, None]);

        fire_player_queue("frigate", &mut board, &OneAction(thrust));

        // Pre-fix this would be cell 1 (only first thrust ran).
        let cell_of_frigate = board
            .cells
            .iter()
            .position(|c| c.as_ref().is_some_and(|s| s.id == "frigate"))
            .expect("frigate still on the board");
        assert_eq!(cell_of_frigate, 3, "all three queued thrusts should fire");

        // Queue must be drained — pre-fix the third clear was gated on
        // the (now stale) starting cell and silently skipped.
        let p = board.cells[cell_of_frigate].as_ref().unwrap();
        assert!(
            p.queue.is_empty(),
            "queue should be cleared after execute_queue completes"
        );
    }

    /// Edge-clamp companion to task #52's three-thrust regression: a
    /// short queue at the lane's fore edge bumps the ship to the last
    /// cell and stops. Without the ship-by-id fix, this test passed
    /// spuriously (only the first thrust ran, but the first thrust DID
    /// land at the clamp cell), so it didn't actually exercise the bug —
    /// adding it here as the explicit clamp check, distinct from the
    /// movement-counts check above.
    #[test]
    fn execute_queue_thrust_chain_clamps_at_lane_edge() {
        let thrust = Action {
            id: "__thrust".into(),
            name: "Thrust".into(),
            archetype: WeaponArchetype::Movement,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![crate::grid::Range::Adjacent],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
                direction_2d: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "__thrust").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }

        // Start at cell 5 with three thrusts: 5 -> 6 (last cell), 6 -> 6
        // (clamped, no movement), 6 -> 6 (clamped). All three actions
        // execute, but the last two are no-ops on position.
        let mut player = make_ship("frigate", Faction::Player, 5, 10, LaneEnd::Fore);
        player.queue = vec!["__thrust".into(), "__thrust".into(), "__thrust".into()];
        let mut board = make_board(7, vec![None, None, None, None, None, Some(player), None]);

        fire_player_queue("frigate", &mut board, &OneAction(thrust));

        let cell_of_frigate = board
            .cells
            .iter()
            .position(|c| c.as_ref().is_some_and(|s| s.id == "frigate"))
            .expect("frigate still on the board");
        assert_eq!(cell_of_frigate, 6, "thrust chain clamps at last lane cell");
        let p = board.cells[cell_of_frigate].as_ref().unwrap();
        assert!(
            p.queue.is_empty(),
            "queue should be cleared even when later moves no-op"
        );
    }

    /// Seam #1: `apply_instant_action` applies one action and mutates board
    /// state without going through the queue. A synthetic THRUST applied
    /// instantly to the player advances the ship by one cell — same outcome
    /// as queueing the action and firing the queue, but without the queue
    /// step.
    #[test]
    fn apply_instant_action_moves_ship_without_queueing() {
        let player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![Some(player), None, None, None, None, None, None]);

        let thrust = Action {
            id: "__thrust".into(),
            name: "Thrust".into(),
            archetype: WeaponArchetype::Movement,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![crate::grid::Range::Adjacent],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
                direction_2d: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct NoLookup;
        impl Content for NoLookup {
            fn action(&self, _: &str) -> Option<&Action> {
                None
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }

        apply_instant_action("frigate", &thrust, &mut board, &NoLookup);

        let cell = find_cell_by_id(&board, "frigate").expect("frigate still on board");
        assert_eq!(cell, 1, "instant thrust should move the ship +1");
        let p = board.cells[cell].as_ref().unwrap();
        assert!(
            p.queue.is_empty(),
            "instant action must NOT touch the queue"
        );
    }

    /// Seam #2: `run_world_phase` advances ordnance + runs enemy queues +
    /// EOT, but does NOT fire the player's queue. After one call, the
    /// player's queued action remains and the player's pre-loaded
    /// cooldown ticks down (proving EOT ran). Enemy state is not asserted
    /// because the AI may queue + fire its own actions during phase 3,
    /// which is intentional behavior outside this seam's contract.
    #[test]
    fn run_world_phase_does_not_fire_player_queue() {
        let mut player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        player.queue = vec!["pulse_laser".into()];
        // Pre-load a cooldown on the player to verify EOT decrements it.
        player.cooldowns.insert("rail".into(), 2);
        let mut board = make_board(7, vec![Some(player), None, None, None, None, None, None]);

        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        let content = OneAction(pulse_laser());

        run_world_phase(&mut board, &content);

        // Player queue untouched.
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(
            p.queue,
            vec!["pulse_laser".to_string()],
            "run_world_phase must NOT fire the player queue"
        );
        // EOT ran: player cooldown decremented.
        assert_eq!(
            p.cooldowns.get("rail").copied(),
            Some(1),
            "EOT should tick down player cooldown by 1"
        );
    }

    /// Seam #3: `resolve_round` composes `fire_player_queue` +
    /// `run_world_phase` — observable behavior unchanged from before the
    /// refactor. Queueing + resolving once drains the queue AND advances
    /// EOT (player cooldown ticks AFTER the queued action's reset).
    #[test]
    fn resolve_round_composes_phase1_and_world() {
        let mut player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        player.queue = vec!["pulse_laser".into()];
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(player), Some(scout), None, None, None, None, None],
        );

        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        let content = OneAction(pulse_laser());

        resolve_round(&mut board, &content);

        let p = board.cells[0].as_ref().unwrap();
        assert!(
            p.queue.is_empty(),
            "resolve_round should drain the player queue"
        );
        // pulse_laser sets cooldown to 0 (cooldown_max=0), EOT subtracts 1
        // floored at 0, so still 0. But the key is the queue drained AND
        // the world ran (i.e. heat dissipated by 1 too). Pulse_laser
        // costs 1 heat; EOT subtracts 1; final heat = 0.
        assert_eq!(p.heat, 0, "heat +1 from pulse_laser, -1 from EOT");
    }

    /// 'Nothing bore' gate: a forward arc looking at an empty lane does not
    /// reset the cooldown and does not spend heat.
    #[test]
    fn execute_queue_no_target_no_cost() {
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.heat = 0;
        attacker.queue = vec!["pulse_laser".into()];
        let mut board = make_board(7, vec![Some(attacker), None, None, None, None, None, None]);
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        let content = OneAction(pulse_laser());
        fire_player_queue("frigate", &mut board, &content);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 0, "heat must NOT advance when no target bears");
        assert!(!p.cooldowns.contains_key("pulse_laser"));
    }

    /// Range-band falloff: at distance 5 (long), with optimal=close (delta 2),
    /// raw 4 -> floor(4 * 0.5) = 2.
    #[test]
    fn apply_damage_applies_band_falloff_when_outside_optimal() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 5, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(attacker), None, None, None, None, Some(scout), None],
        );
        let weapon = pulse_laser();
        apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
        // distance 5 -> long; delta from close (idx 1) to long (idx 3) = 2 ->
        // factor 0.5; floor(4 * 0.5) = 2; stern armour 0; 5 - 2 = 3.
        let scout_hull = board.cells[5].as_ref().map(|s| s.hull);
        assert_eq!(scout_hull, Some(3));
    }

    /// `bandFalloff: false` on the weapon's DAMAGE effect bypasses falloff
    /// for the WHOLE call.
    #[test]
    fn apply_damage_band_falloff_disabled_lands_full_amount() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 5, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(attacker), None, None, None, None, Some(scout), None],
        );
        let mut weapon = pulse_laser();
        weapon.effects = vec![Effect::DAMAGE {
            amount: 4,
            band_falloff: Some(false),
        }];
        apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
        // No falloff, no armour -> 5 - 4 = 1.
        let scout_hull = board.cells[5].as_ref().map(|s| s.hull);
        assert_eq!(scout_hull, Some(1));
    }

    /// `VENT_HEAT` clears the locked-out flag and optionally resets cooldowns.
    #[test]
    fn vent_heat_clears_lockout_and_recharges_cooldowns() {
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.heat = 6;
        attacker.locked_out = true;
        attacker.cooldowns.insert("pulse_laser".into(), 3);
        let mut board = make_board(7, vec![Some(attacker), None, None, None, None, None, None]);
        let vent = Action {
            id: "vent".into(),
            name: "Vent".into(),
            archetype: WeaponArchetype::Defensive,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![crate::grid::Range::Adjacent],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::VENT_HEAT {
                amount: 4,
                recharge_cooldowns: Some(true),
            }],
            r#mod: None,
            icon: None,
        };
        let fx = vent.effects[0].clone();
        apply_effect(&fx, &vent, 0, &[0], &mut board, &NoContent);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 2);
        assert!(!p.locked_out);
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(0));
    }

    /// `REORIENT::Flip` swaps the bow end on a bow-on ship.
    #[test]
    fn reorient_flip_swaps_bow_end() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![Some(attacker), None, None, None, None, None, None]);
        let action = Action {
            id: "flip".into(),
            name: "Flip".into(),
            archetype: WeaponArchetype::Movement,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![crate::grid::Range::Adjacent],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::REORIENT {
                to: ReorientTo::Flip,
            }],
            r#mod: None,
            icon: None,
        };
        let fx = action.effects[0].clone();
        apply_effect(&fx, &action, 0, &[0], &mut board, &NoContent);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.orientation, Orientation::BowOn { bow: LaneEnd::Aft });
    }

    /// End of turn ticks cooldowns down and dissipates one heat.
    #[test]
    fn end_of_turn_ticks_cooldowns_and_dissipates_heat() {
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.heat = 3;
        attacker.cooldowns.insert("pulse_laser".into(), 2);
        attacker.cooldowns.insert("rail".into(), 0);
        let mut board = make_board(7, vec![Some(attacker), None, None, None, None, None, None]);
        end_of_turn(&mut board, &NoContent);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 2);
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(1));
        // Zero cooldowns stay at zero.
        assert_eq!(p.cooldowns.get("rail").copied(), Some(0));
    }

    /// `HullBreach` status ticks 1 damage per turn and expires after duration
    /// turns.
    #[test]
    fn hull_breach_status_ticks_damage_and_expires() {
        let mut scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        scout.statuses.push(Status {
            kind: StatusKind::HullBreach,
            duration: 2,
            face: None,
        });
        let mut board = make_board(7, vec![None, Some(scout), None, None, None, None, None]);
        end_of_turn(&mut board, &NoContent);
        let s = board.cells[1].as_ref().unwrap();
        assert_eq!(s.hull, 4); // -1 from the breach.
        assert_eq!(
            s.statuses
                .iter()
                .filter(|st| st.kind == StatusKind::HullBreach)
                .count(),
            1
        );
        end_of_turn(&mut board, &NoContent);
        let s = board.cells[1].as_ref().unwrap();
        assert_eq!(s.hull, 3); // -1 more.
                               // Duration was 2 -> 1 -> 0; should expire after the second tick.
        assert!(s
            .statuses
            .iter()
            .all(|st| st.kind != StatusKind::HullBreach));
    }

    /// Parity lock (task #131): a lethal hullBreach tick routes through
    /// `destroy()`, not just a silent hull subtraction.
    ///
    /// TS `tickStatuses` (resolve.ts:319-328) does `ship.hull -= 1; if
    /// (ship.hull <= 0) destroy(ship, board)` — so a breach that takes the
    /// last hull point must clear the cell AND fire the full destroy path
    /// (`onLethal`, and `ReactorBreach` splash if traited). The existing
    /// damage-tick test only covers the non-lethal case; this locks the
    /// lethal routing. (Note: `add_status` coalesces same-kind statuses by
    /// `max` duration, so at most one hullBreach is ever present — the Rust
    /// batched breach count is always 0 or 1, matching TS's per-status loop
    /// for every reachable state.)
    #[test]
    fn lethal_hull_breach_tick_routes_through_destroy() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Hull 1 + a hullBreach: the tick deals 1, hull -> 0, destroy fires.
        let mut scout = make_ship("scout", Faction::Enemy, 1, 1, LaneEnd::Fore);
        scout.statuses.push(Status {
            kind: StatusKind::HullBreach,
            duration: 3,
            face: None,
        });
        let mut board = make_board(7, vec![None, Some(scout), None, None, None, None, None]);

        let lethal = Rc::new(Cell::new(0u32));
        let l2 = lethal.clone();
        board.bus.on(Hook::OnLethal, move |_ctx| {
            l2.set(l2.get() + 1);
        });

        end_of_turn(&mut board, &NoContent);

        assert!(
            board.cells[1].is_none(),
            "a lethal hullBreach tick must clear the cell via destroy()",
        );
        assert_eq!(
            lethal.get(),
            1,
            "destroy() fires onLethal exactly once for the breach kill",
        );
    }

    /// Targeting: `SPINAL_LINE` with `hits_all=false` picks the first occupant only.
    #[test]
    fn resolve_targeting_spinal_line_first_only_picks_first_target() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let gunboat = make_ship("gunboat", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let board = make_board(
            7,
            vec![
                Some(attacker),
                None,
                Some(scout),
                None,
                Some(gunboat),
                None,
                None,
            ],
        );
        let mut spinal = pulse_laser();
        spinal.targeting.pattern = TargetingPattern::SPINAL_LINE;
        spinal.targeting.band = vec![
            RangeBand::Close,
            RangeBand::Mid,
            RangeBand::Long,
            RangeBand::Extreme,
        ];
        spinal.targeting.hits_all = false;
        let cells = resolve_targeting(&spinal, &board, 0);
        assert_eq!(cells, vec![2]);
    }

    /// Targeting: `SPINAL_LINE` with `hits_all=true` pierces through both occupants.
    #[test]
    fn resolve_targeting_spinal_line_hits_all_pierces() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let gunboat = make_ship("gunboat", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let board = make_board(
            7,
            vec![
                Some(attacker),
                None,
                Some(scout),
                None,
                Some(gunboat),
                None,
                None,
            ],
        );
        let mut spinal = pulse_laser();
        spinal.targeting.pattern = TargetingPattern::SPINAL_LINE;
        spinal.targeting.band = vec![
            RangeBand::Close,
            RangeBand::Mid,
            RangeBand::Long,
            RangeBand::Extreme,
        ];
        spinal.targeting.hits_all = true;
        let cells = resolve_targeting(&spinal, &board, 0);
        assert_eq!(cells, vec![2, 4]);
    }

    /* ---- chain-kill window ------------------------------------------------ */

    /// Two destroys in one window flips `detect_chain` to true. The counter is
    /// what `destroy()` increments; `execute_queue` resets it on entry, so a
    /// single window with two kills counts.
    #[test]
    fn detect_chain_fires_at_two_destroys_in_one_window() {
        let mut board = make_board(7, vec![None, None, None, None, None, None, None]);
        assert!(!detect_chain(&board));
        board.destroys_this_window = 1;
        assert!(!detect_chain(&board), "one destroy is not a chain");
        board.destroys_this_window = 2;
        assert!(detect_chain(&board));
        board.destroys_this_window = 5;
        assert!(detect_chain(&board), ">2 destroys still a chain");
    }

    /// `execute_queue` zeros the chain counter on entry, so a kill carried
    /// over from a prior phase does NOT pollute the current window.
    #[test]
    fn execute_queue_resets_chain_window_on_entry() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![Some(attacker), None, None, None, None, None, None]);
        // Pre-populate the counter as if a prior phase had killed someone.
        board.destroys_this_window = 3;

        // Empty queue: execute_queue should still reset the counter on entry,
        // and the post-queue detect_chain check must see the freshly-zeroed
        // value, not the pre-populated 3.
        struct Empty;
        impl Content for Empty {
            fn action(&self, _: &str) -> Option<&Action> {
                None
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        fire_player_queue("frigate", &mut board, &Empty);
        assert_eq!(
            board.destroys_this_window, 0,
            "execute_queue must reset destroys_this_window on entry"
        );
    }

    /// `resolve_round`'s ordnance phase also resets the window so an ordnance
    /// chain is its own scoring epoch.
    #[test]
    fn resolve_round_resets_chain_window_for_ordnance_phase() {
        // No ships, no ordnance — round runs cleanly. The ordnance phase
        // reset still runs; the player-queue reset doesn't (no player ship).
        let mut board = make_board(7, vec![None, None, None, None, None, None, None]);
        board.destroys_this_window = 4;
        struct Empty;
        impl Content for Empty {
            fn action(&self, _: &str) -> Option<&Action> {
                None
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        resolve_round(&mut board, &Empty);
        assert_eq!(
            board.destroys_this_window, 0,
            "the ordnance-phase reset must zero the counter"
        );
    }

    /* ---- self-movement modes --------------------------------------------- */

    fn no_armour_profile() -> ShieldProfile {
        ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        }
    }

    /// THRUST moves the ship exactly one cell in the bow direction when
    /// unblocked.
    #[test]
    fn self_move_thrust_advances_one_cell_when_clear() {
        let ship = make_ship("s", Faction::Player, 2, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![None, None, Some(ship), None, None, None, None]);
        super::resolve_self_move(2, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        assert!(board.cells[2].is_none(), "vacated origin");
        assert_eq!(board.cells[3].as_ref().map(|s| s.cell), Some(3));
    }

    /// THRUST into an occupied cell stays in place and takes 1 collision
    /// damage (`remaining_distance` × 1 = 1).
    #[test]
    fn self_move_thrust_blocked_takes_one_collision() {
        let mut ship = make_ship("s", Faction::Player, 2, 5, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let blocker = make_ship("b", Faction::Enemy, 3, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![None, None, Some(ship), Some(blocker), None, None, None],
        );
        super::resolve_self_move(2, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        // Did not move.
        assert!(board.cells[2].is_some());
        // Hull: 5 - 1 = 4 (collision damage routed through dummy_weapon, no
        // falloff, no armour on the test profile).
        assert_eq!(board.cells[2].as_ref().unwrap().hull, 4);
    }

    /// THRUST into the wall stays in place and takes 1 collision damage.
    #[test]
    fn self_move_thrust_at_wall_takes_one_collision() {
        let mut ship = make_ship("s", Faction::Player, 6, 5, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let mut board = make_board(7, vec![None, None, None, None, None, None, Some(ship)]);
        super::resolve_self_move(6, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 4);
    }

    /// BURN advances up to `distance` cells when clear.
    #[test]
    fn self_move_burn_advances_full_distance_when_clear() {
        let ship = make_ship("s", Faction::Player, 1, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![None, Some(ship), None, None, None, None, None]);
        super::resolve_self_move(1, MovementMode::BURN, 3, None, &mut board, &NoContent);
        assert!(board.cells[1].is_none());
        assert_eq!(board.cells[4].as_ref().map(|s| s.cell), Some(4));
        // No collision: hull intact.
        assert_eq!(board.cells[4].as_ref().unwrap().hull, 10);
    }

    /// BURN stops at the first occupant, eats remaining-distance collision.
    #[test]
    fn self_move_burn_stops_at_blocker_and_takes_collision() {
        let mut ship = make_ship("s", Faction::Player, 1, 10, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let blocker = make_ship("b", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![None, Some(ship), None, None, Some(blocker), None, None],
        );
        super::resolve_self_move(1, MovementMode::BURN, 5, None, &mut board, &NoContent);
        // Stopped at cell 3 (one short of the blocker at 4).
        // Steps taken: 2 (1->2, 2->3). Requested: 5. Remaining: 3.
        assert!(board.cells[3].is_some());
        assert_eq!(board.cells[3].as_ref().unwrap().hull, 10 - 3);
    }

    /// SLIP passes through ships and lands in the first free cell.
    #[test]
    fn self_move_slip_passes_through_to_first_free_cell() {
        let ship = make_ship("s", Faction::Player, 0, 10, LaneEnd::Fore);
        let blocker_a = make_ship("a", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let blocker_b = make_ship("b", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![
                Some(ship),
                Some(blocker_a),
                Some(blocker_b),
                None,
                None,
                None,
                None,
            ],
        );
        super::resolve_self_move(0, MovementMode::SLIP, 2, None, &mut board, &NoContent);
        // SLIP scans 2 cells (lands at 2), finds it occupied, walks forward
        // to 3 which is free.
        assert!(board.cells[0].is_none());
        assert_eq!(board.cells[3].as_ref().map(|s| s.cell), Some(3));
        assert_eq!(board.cells[3].as_ref().unwrap().hull, 10);
    }

    /// JUMP teleports to the target cell when free; no collision.
    #[test]
    fn self_move_jump_teleports_to_free_target() {
        let ship = make_ship("s", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![Some(ship), None, None, None, None, None, None]);
        super::resolve_self_move(0, MovementMode::JUMP, 4, None, &mut board, &NoContent);
        assert!(board.cells[0].is_none());
        assert_eq!(board.cells[4].as_ref().map(|s| s.cell), Some(4));
        assert_eq!(board.cells[4].as_ref().unwrap().hull, 10);
    }

    /// JUMP onto an occupied cell silently fails (no move, no damage).
    #[test]
    fn self_move_jump_onto_occupied_is_noop() {
        let ship = make_ship("s", Faction::Player, 0, 10, LaneEnd::Fore);
        let blocker = make_ship("b", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(ship), None, None, None, Some(blocker), None, None],
        );
        super::resolve_self_move(0, MovementMode::JUMP, 4, None, &mut board, &NoContent);
        assert!(board.cells[0].is_some(), "jump failed; ship stayed home");
        assert_eq!(board.cells[0].as_ref().unwrap().hull, 10);
    }

    /// JUMP off the board clamps to the edge and bills collision overflow.
    #[test]
    fn self_move_jump_off_board_clamps_with_overflow_collision() {
        let mut ship = make_ship("s", Faction::Player, 4, 10, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let mut board = make_board(7, vec![None, None, None, None, Some(ship), None, None]);
        super::resolve_self_move(4, MovementMode::JUMP, 5, None, &mut board, &NoContent);
        // Target = 4 + 5 = 9; clamped to 6; overflow = 9 - 6 = 3.
        assert!(board.cells[6].is_some());
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 10 - 3);
    }

    /// `TRACTOR_SWAP` trades cells with the first adjacent occupant.
    #[test]
    fn self_move_tractor_swap_trades_with_adjacent() {
        let ship = make_ship("s", Faction::Player, 2, 10, LaneEnd::Fore);
        let other = make_ship("o", Faction::Enemy, 3, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![None, None, Some(ship), Some(other), None, None, None],
        );
        super::resolve_self_move(
            2,
            MovementMode::TRACTOR_SWAP,
            1,
            None,
            &mut board,
            &NoContent,
        );
        assert_eq!(
            board.cells[2].as_ref().map(|s| s.id.clone()),
            Some("o".into())
        );
        assert_eq!(
            board.cells[3].as_ref().map(|s| s.id.clone()),
            Some("s".into())
        );
        // Cells updated to match new positions.
        assert_eq!(board.cells[2].as_ref().unwrap().cell, 2);
        assert_eq!(board.cells[3].as_ref().unwrap().cell, 3);
    }

    /// Direction override (Rust-port extension): a ship pointing bow=Fore
    /// with `direction: Some(Aft)` moves toward lower cell indices, opposite
    /// to its bow. Mirrors the synthetic Left arrow case from `input.rs`.
    #[test]
    fn self_move_thrust_honours_direction_override_aft() {
        let ship = make_ship("p", Faction::Player, 3, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![None, None, None, Some(ship), None, None, None]);
        super::resolve_self_move(
            3,
            MovementMode::THRUST,
            1,
            Some(LaneEnd::Aft),
            &mut board,
            &NoContent,
        );
        assert!(board.cells[3].is_none(), "ship left cell 3");
        assert_eq!(
            board.cells[2].as_ref().map(|s| s.cell),
            Some(2),
            "ship moved aft despite bow=Fore",
        );
    }

    /// Mirror: bow=Aft + override `Some(Fore)` moves toward higher indices.
    /// Confirms the override fully replaces the bow-derived step, not merely
    /// XORs with it.
    #[test]
    fn self_move_thrust_honours_direction_override_fore() {
        let ship = make_ship("p", Faction::Player, 3, 10, LaneEnd::Aft);
        let mut board = make_board(7, vec![None, None, None, Some(ship), None, None, None]);
        super::resolve_self_move(
            3,
            MovementMode::THRUST,
            1,
            Some(LaneEnd::Fore),
            &mut board,
            &NoContent,
        );
        assert!(board.cells[3].is_none());
        assert_eq!(
            board.cells[4].as_ref().map(|s| s.cell),
            Some(4),
            "ship moved fore despite bow=Aft",
        );
    }

    /// `direction: None` preserves the canonical TS bow-derived semantics —
    /// existing AI / scripted moves are unaffected.
    #[test]
    fn self_move_thrust_no_direction_uses_bow() {
        let ship = make_ship("p", Faction::Player, 3, 10, LaneEnd::Aft);
        let mut board = make_board(7, vec![None, None, None, Some(ship), None, None, None]);
        super::resolve_self_move(3, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        // bow=Aft -> step -1.
        assert_eq!(board.cells[2].as_ref().map(|s| s.cell), Some(2));
    }

    /* ---- target displacement --------------------------------------------- */

    /// Push moves the target AWAY from the source by `distance` when clear.
    #[test]
    fn target_move_push_advances_when_clear() {
        let source = make_ship("src", Faction::Player, 0, 10, LaneEnd::Fore);
        let target = make_ship("tgt", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(source), None, Some(target), None, None, None, None],
        );
        super::resolve_target_move(
            2,
            0,
            crate::types::DisplaceMode::Push,
            2,
            &mut board,
            &NoContent,
        );
        // Target was at 2, source at 0, push direction is +1 (away from
        // source). Should land at cell 4.
        assert!(board.cells[2].is_none());
        assert_eq!(board.cells[4].as_ref().map(|s| s.cell), Some(4));
        assert_eq!(board.cells[4].as_ref().unwrap().hull, 5);
    }

    /// Push blocked by another ship: target stops one short, takes
    /// remaining-distance collision.
    #[test]
    fn target_move_push_collides_with_intervening_ship() {
        let source = make_ship("src", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut target = make_ship("tgt", Faction::Enemy, 2, 5, LaneEnd::Fore);
        target.shield_profile = no_armour_profile();
        let blocker = make_ship("blk", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![
                Some(source),
                None,
                Some(target),
                None,
                Some(blocker),
                None,
                None,
            ],
        );
        super::resolve_target_move(
            2,
            0,
            crate::types::DisplaceMode::Push,
            3,
            &mut board,
            &NoContent,
        );
        // Step is +1. Cells walked: 3 (free) -> at 3. Next would be 4, occupied.
        // steps_taken=1, remaining=2 -> 2 collision damage.
        assert!(board.cells[3].is_some());
        assert_eq!(board.cells[3].as_ref().unwrap().hull, 5 - 2);
    }

    /// Push into a wall stops at the edge and takes overflow collision.
    #[test]
    fn target_move_push_at_wall_takes_collision() {
        let source = make_ship("src", Faction::Player, 4, 10, LaneEnd::Fore);
        let mut target = make_ship("tgt", Faction::Enemy, 6, 5, LaneEnd::Fore);
        target.shield_profile = no_armour_profile();
        let mut board = make_board(
            7,
            vec![None, None, None, None, Some(source), None, Some(target)],
        );
        super::resolve_target_move(
            6,
            4,
            crate::types::DisplaceMode::Push,
            3,
            &mut board,
            &NoContent,
        );
        // Target at 6, push +1 (away from source at 4). Cannot move
        // (cell 7 off-board). steps_taken=0, remaining=3.
        assert!(board.cells[6].is_some());
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 5 - 3);
    }

    /// Pull moves the target TOWARD the source by `distance` when clear.
    #[test]
    fn target_move_pull_advances_toward_source() {
        let source = make_ship("src", Faction::Player, 6, 10, LaneEnd::Fore);
        let target = make_ship("tgt", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![None, None, Some(target), None, None, None, Some(source)],
        );
        super::resolve_target_move(
            2,
            6,
            crate::types::DisplaceMode::Pull,
            2,
            &mut board,
            &NoContent,
        );
        // Target at 2, source at 6, pull direction is +1 (toward source).
        // Lands at cell 4.
        assert!(board.cells[2].is_none());
        assert_eq!(board.cells[4].as_ref().map(|s| s.cell), Some(4));
        assert_eq!(board.cells[4].as_ref().unwrap().hull, 5);
    }

    /// Pull that overshoots into the source: target collides with source.
    #[test]
    fn target_move_pull_collides_with_source() {
        let source = make_ship("src", Faction::Player, 3, 10, LaneEnd::Fore);
        let mut target = make_ship("tgt", Faction::Enemy, 0, 5, LaneEnd::Fore);
        target.shield_profile = no_armour_profile();
        let mut board = make_board(
            7,
            vec![Some(target), None, None, Some(source), None, None, None],
        );
        super::resolve_target_move(
            0,
            3,
            crate::types::DisplaceMode::Pull,
            5,
            &mut board,
            &NoContent,
        );
        // Pull direction +1 toward source at 3. Steps: 0->1, 1->2 (both free).
        // 2->3 is source, blocks. steps_taken=2, remaining=3.
        assert!(board.cells[2].is_some());
        assert_eq!(board.cells[2].as_ref().unwrap().hull, 5 - 3);
    }

    /// Swap trades source and target cells.
    #[test]
    fn target_move_swap_trades_cells() {
        let source = make_ship("src", Faction::Player, 0, 10, LaneEnd::Fore);
        let target = make_ship("tgt", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![Some(source), None, None, None, Some(target), None, None],
        );
        super::resolve_target_move(
            4,
            0,
            crate::types::DisplaceMode::Swap,
            1,
            &mut board,
            &NoContent,
        );
        assert_eq!(
            board.cells[0].as_ref().map(|s| s.id.clone()),
            Some("tgt".into())
        );
        assert_eq!(
            board.cells[4].as_ref().map(|s| s.id.clone()),
            Some("src".into())
        );
        assert_eq!(board.cells[0].as_ref().unwrap().cell, 0);
        assert_eq!(board.cells[4].as_ref().unwrap().cell, 4);
    }

    /// Push silently no-ops on an empty target cell.
    #[test]
    fn target_move_push_no_target_is_noop() {
        let source = make_ship("src", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![Some(source), None, None, None, None, None, None]);
        super::resolve_target_move(
            3,
            0,
            crate::types::DisplaceMode::Push,
            2,
            &mut board,
            &NoContent,
        );
        assert!(board.cells[3].is_none(), "no target, no move");
    }

    /* ---- subsystem modifiers --------------------------------------------- */

    /// A Content impl that always returns a fixed damage modifier.
    /// Tests using this don't care which ship is the attacker — the
    /// modifier is unconditional — so the trait param can stay anonymous.
    struct FixedModifier(i32);
    impl Content for FixedModifier {
        fn action(&self, _: &str) -> Option<&Action> {
            None
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            unreachable!()
        }
        fn damage_modifier(&self, _attacker: &Ship, _b: crate::grid::Range, _board: &Board) -> i32 {
            self.0
        }
    }

    /// Default `Content::damage_modifier` returns 0, so dmg passes through.
    #[test]
    fn apply_modifiers_default_is_passthrough() {
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let board = make_board(7, vec![None, Some(scout), None, None, None, None, None]);
        let out = super::apply_modifiers(4, 1, crate::grid::Range::Near, &board, &NoContent);
        assert_eq!(out, 4);
    }

    /// A Content impl that adds +1 damage applies the bonus before
    /// target-lock / shield. End-to-end via `apply_damage`: 4 raw, no
    /// falloff bypass so pointBlank<->close delta=1 -> floor(4*0.66)=2,
    /// + 1 modifier = 3, no armour/charge -> hull drops by 3.
    #[test]
    fn apply_modifiers_adds_bonus_through_damage_pipeline() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 10, LaneEnd::Fore);
        scout.shield_profile = no_armour_profile();
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &FixedModifier(1));
        let hull = board.cells[1].as_ref().unwrap().hull;
        // 4 raw -> falloff close vs pointBlank delta=1 factor 0.66 -> floor(4*0.66)=2
        // + 1 modifier = 3 -> stern armour 0 -> 3 lands. 10 - 3 = 7.
        assert_eq!(hull, 7);
    }

    /// Negative modifiers clamp to 0 — no underflow into "heals on hit".
    #[test]
    fn apply_modifiers_negative_clamps_at_zero() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 10, LaneEnd::Fore);
        scout.shield_profile = no_armour_profile();
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        // -100 modifier obliterates the 2-damage post-falloff hit.
        apply_damage(1, 4, 0, &weapon, &mut board, &FixedModifier(-100));
        let hull = board.cells[1].as_ref().unwrap().hull;
        assert_eq!(hull, 10, "negative modifier must clamp; no healing on hit");
    }

    /// Target-lock applies AFTER the modifier per the TS comment at
    /// resolve.ts:154-157. So +1 Marksman followed by 2x lock gives a
    /// final hit of 2*(`raw_falloff` + 1), not 2*`raw_falloff` + 1.
    #[test]
    fn apply_modifiers_runs_before_target_lock() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 20, LaneEnd::Fore);
        scout.shield_profile = no_armour_profile();
        scout.statuses.push(Status {
            kind: StatusKind::TargetLock,
            duration: 5,
            face: None,
        });
        let mut board = make_board(
            7,
            vec![Some(attacker), Some(scout), None, None, None, None, None],
        );
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &FixedModifier(1));
        let hull = board.cells[1].as_ref().unwrap().hull;
        // 4 -> falloff factor 0.66 -> 2 -> +1 mod = 3 -> *2 lock = 6.
        // 20 - 6 = 14. If lock ran before mod we'd get 2*2+1=5; 20-5=15.
        assert_eq!(
            hull, 14,
            "modifier must apply before target-lock doubling per TS pipeline order"
        );
    }

    /* ---- enemy AI -------------------------------------------------------- */

    struct AiContent {
        actions: HashMap<String, Action>,
    }
    impl Content for AiContent {
        fn action(&self, id: &str) -> Option<&Action> {
            self.actions.get(id)
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            unreachable!()
        }
    }

    /// Helper: an enemy with one mount carrying the named weapon.
    fn enemy_with_weapon(id: &str, cell: usize, weapon: &str, arc: Arc, bow: LaneEnd) -> Ship {
        let mut s = make_ship(id, Faction::Enemy, cell, 5, bow);
        s.mounts = vec![Mount {
            id: "m1".into(),
            arc,
            weapon: weapon.into(),
        }];
        s
    }

    /// AI queues a real attack action when one bears on the player.
    #[test]
    fn ai_queues_threatening_action_when_bears() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        // Enemy at cell 2, bow=aft so its forward arc faces the player at 0.
        let enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(
            7,
            vec![Some(player), None, Some(enemy), None, None, None, None],
        );
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(
            queue,
            vec!["pulse_laser".to_string()],
            "AI should queue the threatening pulse_laser"
        );
    }

    // (#30/#33) The 1-D make_ship stub of `ai_skips_out_of_band_action` was
    // DELETED here — its 2-D version lives + green in tests/ai_2d.rs
    // (ai_skips_out_of_band_action_and_closes).

    // (#76 audit) `ai_prefers_diversifying_threat` was DELETED here. It
    // claimed to lock "the AI prefers a diversifying (uncovered-end) threat,"
    // but its premise was the +6 lane-end-diversity bonus, which #74 proved
    // VESTIGIAL and removed (see the scoring-section note). With the bonus
    // gone the test was a pure duplicate of
    // `ai_queues_threatening_action_when_bears` — "a bearing enemy fires" —
    // passing for a different reason than its name claimed. Deleted rather
    // than relabelled because the honest behavior it would assert is already
    // locked by that test. (lead call, #76.)

    /// #71: an enemy whose arc BEARS on the player FIRES even when its
    /// lane-end is already covered by an ally — the covered-end "reposition
    /// instead of fire" suppression was DROPPED in #71 (it caused the
    /// "march in a line, never shoot, die" bug). The +6 diversity term still
    /// scores into the pick but no longer SUPPRESSES firing. This locks the
    /// fire-when-you-can behavior against the suppression detour creeping back.
    ///
    /// (Supersedes the former `ai_o1_repositions_instead_of_redundant_fire_on`
    /// _`covered_end`, which #71 made stale: that test asserted the dropped
    /// suppression but passed only coincidentally — its enemy's arc didn't
    /// bear, so it fell through to a maneuver. reviewer-2 flagged the
    /// mislabel; this is the corrected, behavior-true lock.)
    #[test]
    fn ai_fires_on_a_covered_end_when_it_bears_post71() {
        // Player at cell 3. Enemy A at cell 1 (aft of the player) ALREADY
        // queued — so the AFT end is covered (direction_to(player=3, A=1) =
        // Aft). Enemy B at cell 2 is ALSO aft of the player
        // (direction_to(3, 2) = Aft), so B's shot only RE-covers the aft end.
        // B is bow=Fore so its Forward arc points toward higher cells → it
        // genuinely BEARS on the player at cell 3 (distance 1 = PointBlank,
        // in band). Pre-#71 B would have repositioned (covered end); post-#71
        // it FIRES, because firing-when-in-position beats holding fire to
        // maybe pressure a different end.
        let player = make_ship("p", Faction::Player, 3, 10, LaneEnd::Fore);
        let mut enemy_a = enemy_with_weapon("ea", 1, "pulse_laser", Arc::Forward, LaneEnd::Fore);
        enemy_a.queue = vec!["pulse_laser".into()]; // covers the aft end
        let enemy_b = enemy_with_weapon("eb", 2, "pulse_laser", Arc::Forward, LaneEnd::Fore);
        let mut board = make_board(
            7,
            vec![
                None,
                Some(enemy_a),
                Some(enemy_b),
                Some(player),
                None,
                None,
                None,
            ],
        );
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(
            queue,
            vec!["pulse_laser".to_string()],
            "#71: B's arc bears on the player, so it FIRES even though the aft end is \
             already covered — the covered-end suppression was dropped; got {queue:?}",
        );
    }

    // (#30/#33) The 1-D make_ship stub of `ai_closes_via_synthetic_move_when_cannot_fire`
    // was DELETED here — its 2-D version lives + green in tests/ai_2d.rs
    // (same name).

    // (#20/#33) The 1-D make_ship stub of `ai_falls_back_to_movement_when_nothing_bears`
    // was DELETED here — migrated to tests/ai_2d.rs on 2-D invariant-A fixtures.

    // (#20/#33) The 1-D make_ship stub of `ai_skips_action_on_cooldown` was
    // DELETED here — migrated to tests/ai_2d.rs on 2-D invariant-A fixtures.

    /// Friendly-fire filter (task #49): an enemy whose arc bears only on
    /// another enemy ship in front of it must NOT queue the attack. The
    /// damage geometry still permits friendly fire (the analysis doc's
    /// "Unfriendly Fire" subsystem makes player-forced friendly fire a
    /// designed mechanic), but the AI declines to fire on allies
    /// unprompted.
    ///
    /// Reproduces `tests/demo_scenarios.rs` scenario B: gunboat at cell 4
    /// bow=aft -> Forward arc bears aft. First occupant aft is the scout
    /// at cell 1 (same `Faction::Enemy`). AI must SKIP this action.
    #[test]
    fn ai_skips_friendly_fire_only_target() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Aft);
        let gunboat = enemy_with_weapon("gunboat", 4, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(
            7,
            vec![
                Some(player),
                Some(scout),
                None,
                None,
                Some(gunboat),
                None,
                None,
            ],
        );
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), {
                let mut a = pulse_laser();
                // Widen the band so range 3 (mid) is allowed; default
                // pulse_laser is pointBlank/close/mid which already
                // includes mid, but extending makes the intent explicit.
                a.targeting.band = vec![
                    RangeBand::PointBlank,
                    RangeBand::Close,
                    RangeBand::Mid,
                    RangeBand::Long,
                    RangeBand::Extreme,
                ];
                a
            })]),
        };
        crate::ai::decide_enemy_action(4, &mut board, &content);
        let queue = board.cells[4].as_ref().unwrap().queue.clone();
        // Gunboat's only forward target is the scout (same faction) -> the
        // friendly-fire filter rejects firing pulse_laser. It must NOT queue
        // the friendly-only shot; instead (#68) it CLOSES toward the player
        // (cell 0 is aft of cell 4 => __move_left).
        assert!(
            !queue.contains(&"pulse_laser".to_string()),
            "AI must skip an action whose only target is a same-faction ship; got {queue:?}"
        );
        assert_eq!(queue, vec![crate::input::SYNTHETIC_MOVE_LEFT.to_string()],
            "#68: friendly-fire-blocked enemy closes toward the player instead of camping; got {queue:?}");
    }

    // (#20/#33) The 1-D make_ship stub of `ai_fires_through_ally_to_reach_player`
    // was DELETED here — migrated to tests/ai_2d.rs on 2-D invariant-A fixtures
    // (and reconciled with the resolver: pierce is SPINAL_LINE, not BEAM).

    /// Lockout: when overheated, only zero-heat actions are eligible.
    #[test]
    fn ai_respects_lockout_only_queues_zero_heat() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        enemy.locked_out = true;
        enemy.heat = enemy.heat_max;
        let mut board = make_board(
            7,
            vec![Some(player), None, Some(enemy), None, None, None, None],
        );
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        // Pulse laser has heat:1 -> locked out can't fire it. No fallback.
        assert!(
            queue.is_empty(),
            "AI lockout + only heat-bearing weapon -> empty queue"
        );
    }

    /* ---- content invariant spec, series B (net-new locks) ----------------
     *
     * These extend the AI tests above with the assertions from content's
     * invariant spec (#35) that were NOT already pinned:
     *   B2-strong — the +6 uncovered-end bonus FLIPS the pick even when a
     *               higher-raw-damage option threatens an already-covered end.
     *   B4-heat   — the heat-budget gate (heat + cost > heat_max + 1 -> skip),
     *               distinct from the lockout gate already covered above.
     *   B7        — trait nudges: Pursuit prefers a player-hitting action;
     *               BurnHard halves the heat penalty so a hot action wins.
     * The simpler B1/B3/B5/B6 locks are already covered by the tests above
     * and are deliberately not duplicated here.
     * ------------------------------------------------------------------- */

    /// Raw-damage selection: among several mounts that all bear on the player,
    /// the AI picks the highest-raw-damage one. (#76 relabel — this was
    /// `ai_diversity_bonus_outweighs_higher_raw_on_a_covered_end`, whose name
    /// claimed the +6 lane-end bonus beats raw damage. But its own setup gave
    /// BOTH candidates the same threatened end, so the +6 — now removed as
    /// vestigial, #74 — applied equally and CANCELLED; the test only ever
    /// proved raw-selection. Renamed to the behavior it actually locks:
    /// among bearing options the strongest raw wins.)
    #[test]
    fn ai_picks_highest_raw_bearing_weapon() {
        // Enemy at cell 2, bow=Aft so its Forward arc bears down-lane on the
        // player at cell 0 (distance 2 = Close, in band). Two equal-cost
        // Forward mounts both bear on the player: "light" (raw 2) and "heavy"
        // (raw 8). Score = 10(hit) + raw - heat for both, so the heavy wins on
        // raw alone. (Delete-the-+6 leaves this green — it never depended on
        // the bonus; that's the whole point of the relabel.)
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = make_ship("e", Faction::Enemy, 2, 5, LaneEnd::Aft);
        enemy.mounts = vec![
            Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "light".into(),
            },
            Mount {
                id: "m2".into(),
                arc: Arc::Forward,
                weapon: "heavy".into(),
            },
        ];
        let mut board = make_board(
            7,
            vec![Some(player), None, Some(enemy), None, None, None, None],
        );
        let light = {
            let mut a = pulse_laser();
            a.id = "light".into();
            a.effects = vec![Effect::DAMAGE {
                amount: 2,
                band_falloff: None,
            }];
            a
        };
        let heavy = {
            let mut a = pulse_laser();
            a.id = "heavy".into();
            a.effects = vec![Effect::DAMAGE {
                amount: 8,
                band_falloff: None,
            }];
            a
        };
        let content = AiContent {
            actions: HashMap::from([("light".into(), light), ("heavy".into(), heavy)]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(
            queue,
            vec!["heavy".to_string()],
            "among bearing options the AI picks the highest raw damage"
        );
    }

    // (#20/#33) The 1-D make_ship stub of `ai_skips_action_that_overshoots_heat_budget`
    // was DELETED here — migrated to tests/ai_2d.rs on 2-D invariant-A fixtures.

    /// B4-heat boundary: an action that lands EXACTLY at `heat_max` + 1 is still
    /// allowed (the AI tolerates overheating by exactly one). This pins the
    /// `>` (not `>=`) in the gate so the boundary doesn't silently drift.
    #[test]
    fn ai_allows_action_that_lands_exactly_one_over_heat_max() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "warm", Arc::Forward, LaneEnd::Aft);
        enemy.heat = 5;
        enemy.heat_max = 6; // 5 + 2 = 7 == heat_max + 1 -> allowed
        let mut board = make_board(
            7,
            vec![Some(player), None, Some(enemy), None, None, None, None],
        );
        let warm = {
            let mut a = pulse_laser();
            a.id = "warm".into();
            a.cost = ActionCost {
                heat: 2,
                cooldown_max: 0,
                advances_turn: true,
            };
            a
        };
        let content = AiContent {
            actions: HashMap::from([("warm".into(), warm)]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(
            queue,
            vec!["warm".to_string()],
            "AI tolerates overheating by exactly 1 (heat_max + 1 is allowed)"
        );
    }

    // (#20/#33) The 1-D make_ship stub of `ai_pursuit_bonus_flips_pick_toward_the_player_hitting_action`
    // was DELETED here — migrated to tests/ai_2d.rs on 2-D invariant-A fixtures
    // (the same Pursuit-+2 isolation, ported to opposed Rear/Forward arcs).

    /// B7-BurnHard: the `BurnHard` trait halves the heat penalty in scoring,
    /// so a hot-but-strong action is chosen over a cool-but-weak one where a
    /// heat-averse enemy would pick the cheap option. Two mounts: "cheap"
    /// (raw 4, heat 0) and "hot" (raw 5, heat 4). For a normal enemy:
    ///   cheap = 10 + 4 - 0 = 14 ; hot = 10 + 5 - 4 = 11  -> picks cheap.
    /// For `BurnHard` (heat penalty halved):
    ///   cheap = 10 + 4 - 0 = 14 ; hot = 10 + 5 - 2 = 13  -> still cheap.
    /// So to actually flip the pick we widen the raw gap: hot raw 8, heat 4.
    ///   normal: cheap 14 ; hot = 10 + 8 - 4 = 14 -> tie/cheap.
    ///   `BurnHard`: hot = 10 + 8 - 2 = 16 > 14 -> hot wins.
    /// This pins that `BurnHard`'s halved-heat term changes the decision.
    #[test]
    fn ai_burn_hard_trait_picks_the_hot_action_a_cautious_enemy_would_skip() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = make_ship("e", Faction::Enemy, 2, 5, LaneEnd::Aft);
        enemy.heat_max = 10; // generous so neither action trips the heat gate
        enemy.traits = vec![crate::types::Trait::BurnHard];
        enemy.mounts = vec![
            Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "cheap".into(),
            },
            Mount {
                id: "m2".into(),
                arc: Arc::Forward,
                weapon: "hot".into(),
            },
        ];
        let mut board = make_board(
            7,
            vec![Some(player), None, Some(enemy), None, None, None, None],
        );
        let cheap = {
            let mut a = pulse_laser();
            a.id = "cheap".into();
            a.cost = ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            };
            a.effects = vec![Effect::DAMAGE {
                amount: 4,
                band_falloff: None,
            }];
            a
        };
        let hot = {
            let mut a = pulse_laser();
            a.id = "hot".into();
            a.cost = ActionCost {
                heat: 4,
                cooldown_max: 0,
                advances_turn: true,
            };
            a.effects = vec![Effect::DAMAGE {
                amount: 8,
                band_falloff: None,
            }];
            a
        };
        let content = AiContent {
            actions: HashMap::from([("cheap".into(), cheap), ("hot".into(), hot)]),
        };
        crate::ai::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(
            queue,
            vec!["hot".to_string()],
            "BurnHard halves the heat penalty so the hot high-damage action wins"
        );
    }

    /// End-to-end: two lethal hits inside one `execute_queue` window cause
    /// `OnChainKill` to fire. The wired event-bus path is what subsystems
    /// like Chain Bounty subscribe to.
    #[test]
    fn execute_queue_emits_on_chain_kill_when_two_destroys_in_one_window() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Two squishy enemies on the attacker's column ahead of a spinal-piercing
        // weapon; one shot should pierce and kill both. (#20 2-D fixture:
        // attacker at (2,3) Bow(N) fires N up column 2; scout at (2,2) and gunboat
        // at (2,1) are both on the ray, so the SPINAL_LINE hits_all pierces both.)
        let mut attacker = armed_ship_2d(
            "frigate",
            Faction::Player,
            crate::grid::Pos::new(2, 3),
            10,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "chain_lance",
            default_shield_profile(),
        );
        attacker.queue = vec!["chain_lance".into()];
        let scout = armed_ship_2d(
            "scout",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            1,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "chain_lance",
            naked(),
        );
        let gunboat = armed_ship_2d(
            "gunboat",
            Faction::Enemy,
            crate::grid::Pos::new(2, 1),
            1,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "chain_lance",
            naked(),
        );
        let mut board = armed_board_2d(vec![attacker, scout, gunboat]);

        // Subscribe to OnChainKill BEFORE we lose the bus mutability into
        // execute_queue. Use Rc<Cell<u32>> to side-channel the count out.
        let count = Rc::new(Cell::new(0u32));
        let c2 = count.clone();
        board.bus.on(Hook::OnChainKill, move |_ctx| {
            c2.set(c2.get() + 1);
        });

        // Spinal-piercing weapon with bandFalloff:false so 1 damage lands raw
        // on both targets, killing each (hull 1, armour 0, no charge).
        let chain_lance = Action {
            id: "chain_lance".into(),
            name: "Chain Lance".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SPINAL_LINE,
                band: vec![
                    RangeBand::PointBlank,
                    RangeBand::Close,
                    RangeBand::Mid,
                    RangeBand::Long,
                    RangeBand::Extreme,
                ],
                optimal_band: RangeBand::Mid,
                range_band: vec![
                    crate::grid::Range::Adjacent,
                    crate::grid::Range::Near,
                    crate::grid::Range::Far,
                ],
                optimal_range: crate::grid::Range::Near,
                requires_arc: Some(Arc::Forward),
                facing_relative: true,
                hits_all: true,
            },
            effects: vec![Effect::DAMAGE {
                amount: 1,
                band_falloff: Some(false),
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "chain_lance").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }
        let content = OneAction(chain_lance);
        fire_player_queue("frigate", &mut board, &content);

        // Both ships should be gone, and OnChainKill should have fired once.
        assert!(
            board.cells[crate::grid::Pos::new(2, 2).to_index()].is_none(),
            "scout was killed"
        );
        assert!(
            board.cells[crate::grid::Pos::new(2, 1).to_index()].is_none(),
            "gunboat was killed"
        );
        assert_eq!(count.get(), 1, "OnChainKill fires once for the window");
    }

    /// Regression (task #96): `bearing_direction` at the aft lane edge.
    ///
    /// A ship at cell 0 with bow=fore firing a Rear-arc weapon must bear AFT
    /// — the rear gun points astern, which is the opposite end from the bow.
    /// TS probes `ship.cell - 1 = -1` and `directionTo(0, -1) = "aft"`, so the
    /// rear arc legally bears and the action fires. The pre-fix Rust port hit
    /// a `probe < 0` special-case that called `bears(ship, arc, 0)` ->
    /// `direction_to(0, 0)` -> Fore, asking the aft branch "does this bear
    /// FORE" (it doesn't), then gated on a non-canonical arc allowlist — so
    /// `bearing_direction` returned `None` and the action silently no-opped.
    ///
    /// A Turret-arc weapon at cell 0 must also resolve a direction (turrets
    /// always bear); the fix returns Fore for it (the first end probed that
    /// bears), which is fine — the point is it is not `None`.
    #[test]
    fn bearing_direction_rear_arc_at_cell_zero_bears_aft() {
        // bow=fore ship sitting at the aft lane edge.
        let ship = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);

        let rear_gun = |arc: Arc| Action {
            id: "rear_gun".into(),
            name: "Rear Gun".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost {
                heat: 1,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![
                    RangeBand::PointBlank,
                    RangeBand::Close,
                    RangeBand::Mid,
                    RangeBand::Long,
                    RangeBand::Extreme,
                ],
                optimal_band: RangeBand::Mid,
                range_band: vec![
                    crate::grid::Range::Adjacent,
                    crate::grid::Range::Near,
                    crate::grid::Range::Far,
                ],
                optimal_range: crate::grid::Range::Near,
                requires_arc: Some(arc),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE {
                amount: 4,
                band_falloff: None,
            }],
            r#mod: None,
            icon: None,
        };

        // Rear arc on a bow=fore ship at cell 0 -> must bear AFT (was None
        // pre-fix).
        let rear = rear_gun(Arc::Rear);
        let board = make_board(
            7,
            vec![Some(ship.clone()), None, None, None, None, None, None],
        );
        assert_eq!(
            bearing_direction(&ship, 0, &board, &rear),
            Some(LaneEnd::Aft),
            "rear arc at the aft edge must bear aft, matching TS directionTo(0,-1)=aft",
        );

        // Turret always bears; at cell 0 the fore probe is checked first and
        // bears, so the direction resolves (the regression is `None`, not a
        // specific end).
        let turret = rear_gun(Arc::Turret);
        assert!(
            bearing_direction(&ship, 0, &board, &turret).is_some(),
            "turret arc at cell 0 must resolve a bearing direction, not None",
        );
    }

    /// Regression (task #112 / reviewer divergence #1): a firing ship that
    /// self-destructs in its own action STILL emits `onDamageDealt`.
    ///
    /// TS `executeQueue` emits `onDamageDealt` once per fired action,
    /// unconditionally — `source: ship` is an object reference that survives
    /// the ship's removal from the board, so the event is orthogonal to the
    /// attacker's fate. The pre-fix Rust nested the emit inside the
    /// `Some(post_cell)` guard, so a self-destructing attacker skipped it.
    ///
    /// Mechanism: a `SELF`-targeting `DAMAGE` action (`band_falloff:false`) with
    /// amount >= the firing ship's hull, against a zero-armour shield, drops
    /// the ship's own hull to <=0 and `destroy()`s it during effect
    /// application. After the queue runs, the ship's cell is empty AND the
    /// `OnDamageDealt` subscriber must have fired exactly once.
    #[test]
    fn run_action_emits_on_damage_dealt_even_when_attacker_self_destructs() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Self-destruct action: targets SELF, lands 9 raw on a hull-3 ship.
        let self_destruct = Action {
            id: "self_destruct".into(),
            name: "Reactor Overload".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost {
                heat: 0,
                cooldown_max: 0,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![
                    RangeBand::PointBlank,
                    RangeBand::Close,
                    RangeBand::Mid,
                    RangeBand::Long,
                    RangeBand::Extreme,
                ],
                optimal_band: RangeBand::PointBlank,
                range_band: vec![
                    crate::grid::Range::Adjacent,
                    crate::grid::Range::Near,
                    crate::grid::Range::Far,
                ],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            // band_falloff:false so the raw 9 lands intact even at PointBlank.
            effects: vec![Effect::DAMAGE {
                amount: 9,
                band_falloff: Some(false),
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "self_destruct").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
                unreachable!()
            }
        }

        // Firing ship: hull 3, ZERO-armour shields so the self-hit lands full.
        let mut ship = make_ship("kamikaze", Faction::Player, 0, 3, LaneEnd::Fore);
        ship.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        ship.queue = vec!["self_destruct".into()];
        let mut board = make_board(7, vec![Some(ship), None, None, None, None, None, None]);

        // Count OnDamageDealt emits via a side-channel before the bus is
        // borrowed into the resolver.
        let damage_dealt = Rc::new(Cell::new(0u32));
        let c2 = damage_dealt.clone();
        board.bus.on(Hook::OnDamageDealt, move |_ctx| {
            c2.set(c2.get() + 1);
        });

        fire_player_queue("kamikaze", &mut board, &OneAction(self_destruct));

        // The attacker self-destructed: its cell is empty.
        assert!(
            board.cells[0].is_none(),
            "the firing ship should have destroyed itself with its own SELF damage",
        );
        // ...and `onDamageDealt` still fired exactly once, matching TS's
        // unconditional emit. Pre-fix this was 0 (emit was skipped because the
        // attacker was gone from the board).
        assert_eq!(
            damage_dealt.get(),
            1,
            "onDamageDealt fires unconditionally per action, even on self-destruct",
        );
    }

    /* =====================================================================
     * Weapon mods (#50). One-action Content harness reused across the mod
     * tests: serves a single modded weapon by id.
     * ================================================================== */

    struct ModContent(Action);
    impl Content for ModContent {
        fn action(&self, id: &str) -> Option<&Action> {
            (id == self.0.id).then_some(&self.0)
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            unreachable!()
        }
    }

    /// A no-falloff pulse laser carrying mod `mod_id`, firing `amount` damage.
    fn modded_weapon(id: &str, mod_id: &str, amount: i32) -> Action {
        let mut a = pulse_laser();
        a.id = id.into();
        a.r#mod = Some(mod_id.into());
        a.cost = ActionCost {
            heat: 0,
            cooldown_max: 3,
            advances_turn: true,
        };
        a.effects = vec![Effect::DAMAGE {
            amount,
            band_falloff: Some(false),
        }];
        a
    }

    /// `flak_burst`: on hit, each lane-neighbour of the HIT cell takes 1 through
    /// the pipeline — faction-blind (an adjacent ALLY of the attacker is hit
    /// too). The hit cell itself is not re-damaged by the burst.
    ///
    /// !! REAL 2-D GAP (flagged to lead — flak-2d): the `flak_burst` ON-HIT MOD
    /// splash (`apply_on_hit_mod`, resolve.rs `FlakBurst` arm) is still 1-D and is
    /// effectively BROKEN on a real 2-D board. It splashes the HIT CELL's
    /// flat-index neighbours `hit_cell +/- 1`, bounds-checked against
    /// `board.size`, via the 1-D `apply_damage` — NOT `grid::neighbors` +
    /// `apply_damage_2d`. Two failures result on the grid: (1) flat `+/- 1` is only
    /// the spatial E-W neighbour mid-row (it crosses rows at a column edge), and
    /// (2) `board.size` is the grid WIDTH (COLS=5), so any neighbour index >= 5
    /// (i.e. anything off row 0) is wrongly culled as "off-board" — so on a
    /// real board the splash usually lands on NOTHING. (Distinct from the BLAST
    /// *targeting pattern*, which WAS widened to 8-neighbours.)
    ///
    /// This test is the SPEC of the intended 2-D behaviour (center's spatial
    /// neighbours each take 1, faction-blind). flak-2d FIXED: the mod now splashes
    /// `grid::neighbors(hit_pos)` via `apply_damage_2d`, so the splash lands on a
    /// real 2-D board (off row 0 too). The other 8 `run_action` tests (#20) don't
    /// touch this mod.
    #[test]
    fn mod_flak_burst_splashes_both_neighbours_faction_blind() {
        // Attacker at (2,3) Bow(N) fires N up column 2 ((2,2) empty) onto the
        // target at (2,1). The flak mod splashes the hit cell's spatial
        // neighbours: (1,1) [a player-faction ally] and (3,1) [an enemy] — both
        // take 1, proving the splash is faction-blind. Naked shields so the 1
        // lands on hull. The hit cell itself is not re-damaged.
        let attacker = {
            let mut a = armed_ship_2d(
                "p",
                Faction::Player,
                crate::grid::Pos::new(2, 3),
                5,
                crate::grid::Facing::Bow(crate::grid::Dir4::N),
                Arc::Forward,
                "flak",
                naked(),
            );
            a.queue = vec!["flak".into()];
            a
        };
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            crate::grid::Pos::new(2, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "flak",
            naked(),
        );
        let ally = armed_ship_2d(
            "ally",
            Faction::Player,
            crate::grid::Pos::new(1, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "flak",
            naked(),
        );
        let enemy_n = armed_ship_2d(
            "n",
            Faction::Enemy,
            crate::grid::Pos::new(3, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "flak",
            naked(),
        );
        let mut board = armed_board_2d(vec![attacker, t, ally, enemy_n]);
        fire_player_queue(
            "p",
            &mut board,
            &ModContent(modded_weapon("flak", "flak_burst", 3)),
        );

        // Primary hit: target at (2,1) takes the 3-dmg pulse (5 -> 2).
        assert_eq!(
            board.cells[crate::grid::Pos::new(2, 1).to_index()]
                .as_ref()
                .unwrap()
                .hull,
            2,
            "primary pulse lands on target"
        );
        // Splash: the hit cell's E-W neighbours each take 1 — faction-blind.
        assert_eq!(
            board.cells[crate::grid::Pos::new(3, 1).to_index()]
                .as_ref()
                .unwrap()
                .hull,
            4,
            "E neighbour (enemy) takes 1 flak splash"
        );
        assert_eq!(
            board.cells[crate::grid::Pos::new(1, 1).to_index()]
                .as_ref()
                .unwrap()
                .hull,
            4,
            "W neighbour (player-faction ally) takes 1 — faction-blind"
        );
    }

    /// incendiary: `APPLY_STATUS` hullBreach 3 on the hit cell. (#20 2-D fixture:
    /// p at (2,1) Bow(S) fires S onto t directly ahead at (2,2), Adjacent.)
    #[test]
    fn mod_incendiary_applies_hull_breach_on_hit() {
        let mut p = armed_ship_2d(
            "p",
            Faction::Player,
            crate::grid::Pos::new(2, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "inc",
            default_shield_profile(),
        );
        p.queue = vec!["inc".into()];
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            20,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "inc",
            default_shield_profile(),
        );
        let mut board = armed_board_2d(vec![p, t]);
        fire_player_queue(
            "p",
            &mut board,
            &ModContent(modded_weapon("inc", "incendiary", 3)),
        );
        let st = &board.cells[crate::grid::Pos::new(2, 2).to_index()]
            .as_ref()
            .unwrap()
            .statuses;
        let breach = st
            .iter()
            .find(|s| s.kind == StatusKind::HullBreach)
            .expect("hullBreach applied");
        assert_eq!(
            breach.duration, 3,
            "incendiary applies hullBreach for 3 turns"
        );
    }

    /// `emp_charge`: `APPLY_STATUS` systemsOffline 3 on the hit cell. (#20 2-D fixture.)
    #[test]
    fn mod_emp_charge_applies_systems_offline_on_hit() {
        let mut p = armed_ship_2d(
            "p",
            Faction::Player,
            crate::grid::Pos::new(2, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "emp",
            default_shield_profile(),
        );
        p.queue = vec!["emp".into()];
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            20,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "emp",
            default_shield_profile(),
        );
        let mut board = armed_board_2d(vec![p, t]);
        fire_player_queue(
            "p",
            &mut board,
            &ModContent(modded_weapon("emp", "emp_charge", 3)),
        );
        let st = &board.cells[crate::grid::Pos::new(2, 2).to_index()]
            .as_ref()
            .unwrap()
            .statuses;
        let off = st
            .iter()
            .find(|s| s.kind == StatusKind::SystemsOffline)
            .expect("systemsOffline applied");
        assert_eq!(
            off.duration, 3,
            "emp_charge applies systemsOffline for 3 turns"
        );
    }

    /// `targeting_laser`: `APPLY_STATUS` targetLock on hit — and it lands even when
    /// the directional shield fully absorbs the hull damage (rider on contact).
    /// (#20 2-D fixture: t carries armour 99 on every face, so whichever zone
    /// the southward shot presents absorbs the full pulse — the rider still lands.)
    #[test]
    fn mod_targeting_laser_applies_target_lock_even_through_full_shield() {
        let mut p = armed_ship_2d(
            "p",
            Faction::Player,
            crate::grid::Pos::new(2, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "tl",
            default_shield_profile(),
        );
        p.queue = vec!["tl".into()];
        // #103 Model A: a FULL shield pool on every face (charge 99) soaks the
        // whole hit on whichever zone the southward shot presents — so the hull
        // damage is fully absorbed and we can prove the targetLock rider still
        // lands. (`armour` is the pool CAPACITY now; `charge` is what absorbs.)
        let armoured = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 99,
                charge: 99,
            },
            stern: crate::types::ShieldFace {
                armour: 99,
                charge: 99,
            },
            port: crate::types::ShieldFace {
                armour: 99,
                charge: 99,
            },
            starboard: crate::types::ShieldFace {
                armour: 99,
                charge: 99,
            },
        };
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            20,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "tl",
            armoured,
        );
        let mut board = armed_board_2d(vec![p, t]);
        fire_player_queue(
            "p",
            &mut board,
            &ModContent(modded_weapon("tl", "targeting_laser", 3)),
        );
        let t_ref = board.cells[crate::grid::Pos::new(2, 2).to_index()]
            .as_ref()
            .unwrap();
        assert_eq!(t_ref.hull, 20, "shield fully absorbed the hull damage");
        assert!(
            t_ref
                .statuses
                .iter()
                .any(|s| s.kind == StatusKind::TargetLock),
            "targeting_laser applies targetLock on contact even through full shield absorption",
        );
    }

    /// `precision_core`: a lethal hit recharges THIS action's cooldown to 0; a
    /// non-lethal hit does not.
    #[test]
    fn mod_precision_core_recharges_cooldown_only_on_kill() {
        // (#20 2-D fixture: p at (2,1) Bow(S) fires S onto t at (2,2).)
        // Lethal: target hull 3, pulse 3 (no-falloff), naked -> dies. Attacker's
        // cooldown for "pc" must be 0 afterward (not the cost's 3).
        let p_pos = crate::grid::Pos::new(2, 1);
        let t_pos = crate::grid::Pos::new(2, 2);
        let mut p = armed_ship_2d(
            "p",
            Faction::Player,
            p_pos,
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "pc",
            default_shield_profile(),
        );
        p.queue = vec!["pc".into()];
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            t_pos,
            3,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "pc",
            naked(),
        );
        let mut board = armed_board_2d(vec![p, t]);
        fire_player_queue(
            "p",
            &mut board,
            &ModContent(modded_weapon("pc", "precision_core", 3)),
        );
        assert!(
            board.cells[t_pos.to_index()].is_none(),
            "lethal hit killed the target"
        );
        assert_eq!(
            board.cells[p_pos.to_index()]
                .as_ref()
                .unwrap()
                .cooldowns
                .get("pc")
                .copied(),
            Some(0),
            "precision_core recharges cooldown to 0 on a clean kill",
        );

        // Non-lethal: target survives, cooldown stays at the cost (3).
        let mut p2 = armed_ship_2d(
            "p",
            Faction::Player,
            p_pos,
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "pc",
            default_shield_profile(),
        );
        p2.queue = vec!["pc".into()];
        let t2 = armed_ship_2d(
            "t",
            Faction::Enemy,
            t_pos,
            20,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "pc",
            naked(),
        );
        let mut board2 = armed_board_2d(vec![p2, t2]);
        fire_player_queue(
            "p",
            &mut board2,
            &ModContent(modded_weapon("pc", "precision_core", 3)),
        );
        assert!(
            board2.cells[t_pos.to_index()].is_some(),
            "non-lethal hit left the target alive"
        );
        assert_eq!(
            board2.cells[p_pos.to_index()]
                .as_ref()
                .unwrap()
                .cooldowns
                .get("pc")
                .copied(),
            Some(3),
            "precision_core does NOT recharge when the hit fails to kill",
        );
    }

    /// `twin_linked`: the action's effects apply twice (cost paid once). A 3-dmg
    /// no-falloff pulse on a 20-hull shieldless target lands 6 total. (#20 2-D
    /// fixture: p at (2,1) Bow(S) fires S onto t at (2,2), naked so 6 lands raw.)
    #[test]
    fn mod_twin_linked_applies_effects_twice() {
        let mut p = armed_ship_2d(
            "p",
            Faction::Player,
            crate::grid::Pos::new(2, 1),
            5,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "twin",
            default_shield_profile(),
        );
        p.heat = 0;
        p.queue = vec!["twin".into()];
        let t = armed_ship_2d(
            "t",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            20,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "twin",
            naked(),
        );
        let weapon = {
            let mut a = modded_weapon("twin", "twin_linked", 3);
            a.cost = ActionCost {
                heat: 2,
                cooldown_max: 3,
                advances_turn: true,
            };
            a
        };
        let mut board = armed_board_2d(vec![p, t]);
        fire_player_queue("p", &mut board, &ModContent(weapon));
        assert_eq!(
            board.cells[crate::grid::Pos::new(2, 2).to_index()]
                .as_ref()
                .unwrap()
                .hull,
            14,
            "twin_linked lands 3 twice = 6 (20 -> 14)"
        );
        // Cost paid ONCE: heat went up by 2 (not 4).
        assert_eq!(
            board.cells[crate::grid::Pos::new(2, 1).to_index()]
                .as_ref()
                .unwrap()
                .heat,
            2,
            "twin_linked pays heat once, not per volley"
        );
    }

    /// autoloader: the turn-dispatch seam reports the action as free-fire
    /// (`advances_turn` = false) regardless of the action's declared value.
    #[test]
    fn mod_autoloader_overrides_advances_turn_for_dispatch() {
        let mut a = pulse_laser();
        a.id = "auto".into();
        a.cost = ActionCost {
            heat: 1,
            cooldown_max: 3,
            advances_turn: true,
        };
        a.r#mod = Some("autoloader".into());
        assert!(
            !action_advances_turn(&a),
            "autoloader forces free-fire (no turn advance)"
        );

        // A plain action with no mod keeps its declared advances_turn.
        let plain = pulse_laser();
        assert!(
            action_advances_turn(&plain),
            "un-modded action keeps its declared advances_turn"
        );

        // A non-autoloader mod does not change advances_turn.
        let mut flak = pulse_laser();
        flak.r#mod = Some("flak_burst".into());
        assert!(
            action_advances_turn(&flak),
            "flak_burst leaves advances_turn alone"
        );
    }

    /// #59: `FireEvents` accumulate across the WHOLE round — the player's fired
    /// shot AND every enemy's — and are cleared once at `resolve_round` start.
    /// This is the regression that proves `fire_player_queue` does NOT clear
    /// per-enemy (which would wipe all-but-the-last ship's beams).
    #[test]
    fn fire_events_accumulate_across_a_multi_ship_round() {
        // (#20 2-D fixture) Player front-centre at (2,3) Bow(N) with a queued
        // pulse up its column. Two enemies on column 2 ahead, Bow(S) so their
        // forward guns bear back down-column on the player and telegraph a shot.
        let p_idx = crate::grid::Pos::new(2, 3).to_index();
        let mut player = armed_ship_2d(
            "p",
            Faction::Player,
            crate::grid::Pos::new(2, 3),
            40,
            crate::grid::Facing::Bow(crate::grid::Dir4::N),
            Arc::Forward,
            "pulse_laser",
            default_shield_profile(),
        );
        player.heat_max = 99; // never lock out across the round
        player.queue = vec!["pulse_laser".into()];
        // e1 at (2,2): player's pulse (Adjacent) bears + in band -> player shot
        // lands here. e1 (Bow S) also bears on the player down the column.
        let mut e1 = armed_ship_2d(
            "e1",
            Faction::Enemy,
            crate::grid::Pos::new(2, 2),
            40,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "pulse_laser",
            default_shield_profile(),
        );
        e1.heat_max = 99;
        // e2 at (2,1) (Near), Bow S, also bears on the player.
        let mut e2 = armed_ship_2d(
            "e2",
            Faction::Enemy,
            crate::grid::Pos::new(2, 1),
            40,
            crate::grid::Facing::Bow(crate::grid::Dir4::S),
            Arc::Forward,
            "pulse_laser",
            default_shield_profile(),
        );
        e2.heat_max = 99;
        let mut board = armed_board_2d(vec![player, e1, e2]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };

        // First resolve_round: enemies fire-then-decide, so this round they
        // only TELEGRAPH (their queues were empty). The player fires its queued
        // pulse at e1. So fire_events should hold the player's shot this round.
        resolve_round(&mut board, &content);
        let after_first: Vec<_> = board.fire_events.clone();
        assert!(
            after_first.iter().any(|f| f.from_cell == p_idx),
            "player's fired pulse produced a FireEvent from the player's cell; got {after_first:?}",
        );

        // Re-arm the player and run a SECOND round: now the enemies have a
        // telegraphed shot to fire (from round 1's decide). The player fires
        // again. fire_events must contain the player's shot AND BOTH enemies'
        // shots — proving accumulation (no per-enemy wipe) and the
        // start-of-round clear (no carryover of round-1 events).
        if let Some(c) = board
            .cells
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.id == "p"))
        {
            if let Some(s) = board.cells[c].as_mut() {
                s.queue.push("pulse_laser".into());
            }
        }
        resolve_round(&mut board, &content);
        let after_second = &board.fire_events;

        // No round-1 carryover: every event's attacker still exists this round
        // (we only assert the count grew to include multiple distinct shooters).
        let distinct_shooters: std::collections::HashSet<usize> =
            after_second.iter().map(|f| f.from_cell).collect();
        assert!(
            distinct_shooters.len() >= 2,
            "a multi-ship-fire round records beams from >=2 distinct attackers (player + enemies), not just the last; \
             got {} distinct from {:?}",
            distinct_shooters.len(),
            after_second,
        );
        // And the player is among them — its shot wasn't wiped by the enemies'
        // subsequent fire_player_queue calls.
        assert!(
            after_second.iter().any(|f| f.from_cell == p_idx),
            "the player's beam survives the enemies' fires in the same round; got {after_second:?}",
        );
    }

    /* =====================================================================
     * resolve_targeting_2d (R3) — sanity coverage over a real 2-D board.
     * One+ per pattern; exhaustive per-pattern coverage is the tester's T3.
     * Ships are placed at cells[pos.to_index()] (invariant A) so Board::ship_at
     * finds them — the same shape C4's build_encounter_board produces.
     * ================================================================== */

    use crate::grid::{Axis, Dir4, Facing, Pos};

    /// A ship at a real 2-D `pos` with a real `facing` and one mount of `arc`.
    fn ship_2d(id: &str, faction: Faction, pos: Pos, facing: Facing, arc: Arc) -> Ship {
        let mut s = make_ship(id, faction, pos.to_index(), 10, LaneEnd::Fore);
        s.pos = pos;
        s.facing = facing;
        s.mounts = vec![Mount {
            id: "m".into(),
            arc,
            weapon: "w".into(),
        }];
        s
    }

    /// A len-CELLS board with each ship slotted at `pos.to_index()` (invariant A).
    fn board_2d(ships: Vec<Ship>) -> Board {
        let mut cells: Vec<Option<Ship>> = (0..crate::grid::CELLS).map(|_| None).collect();
        for s in ships {
            let idx = s.pos.to_index();
            cells[idx] = Some(s);
        }
        let mut b = make_board(crate::grid::CELLS, cells);
        b.hazards = (0..crate::grid::CELLS).map(|_| Vec::new()).collect();
        b
    }

    /// An action with a given pattern/arc and the full 3-band range allowed.
    fn action_2d(pattern: TargetingPattern, arc: Option<Arc>, hits_all: bool) -> Action {
        let mut a = pulse_laser();
        a.targeting.pattern = pattern;
        a.targeting.requires_arc = arc;
        a.targeting.hits_all = hits_all;
        a.targeting.range_band = vec![Range::Adjacent, Range::Near, Range::Far];
        a
    }
    use crate::grid::Range;

    #[test]
    fn rt2d_self_returns_own_pos() {
        let p = Pos::new(2, 3);
        let board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            p,
            Facing::Bow(Dir4::N),
            Arc::Turret,
        )]);
        let a = action_2d(TargetingPattern::SELF, None, false);
        assert_eq!(resolve_targeting_2d(&a, &board, p), vec![p]);
    }

    #[test]
    fn rt2d_beam_forward_hits_first_occupant_up_the_bow_column() {
        // Player at (2,3) facing Bow(N) (up-board). Forward beam walks N (row
        // decreasing) along column 2; the enemy at (2,1) is the first occupant.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let near = ship_2d(
            "e1",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let far = ship_2d(
            "e2",
            Faction::Enemy,
            Pos::new(2, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![player, near, far]);
        let a = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        assert_eq!(
            resolve_targeting_2d(&a, &board, Pos::new(2, 3)),
            vec![Pos::new(2, 1)]
        );
    }

    #[test]
    fn rt2d_beam_does_not_bear_off_the_bow_column() {
        // Same player, but the only enemy is in a different column (off the N
        // ray) — a Forward beam fires straight N and finds nothing.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let off = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(0, 1),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![player, off]);
        let a = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        assert!(resolve_targeting_2d(&a, &board, Pos::new(2, 3)).is_empty());
    }

    #[test]
    fn rt2d_broadside_fires_both_flanks() {
        // Broadside(EastWest) hull at (2,2): flanks face N and S. Enemies due N
        // (2,0) and due S (2,3) both get hit; a third off-flank (0,2) does not.
        let ship = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Broadside(Axis::EastWest),
            Arc::BroadsideArc,
        );
        let n = ship_2d(
            "n",
            Faction::Enemy,
            Pos::new(2, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let s = ship_2d(
            "s",
            Faction::Enemy,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let w = ship_2d(
            "w",
            Faction::Enemy,
            Pos::new(0, 2),
            Facing::Bow(Dir4::E),
            Arc::Turret,
        );
        let board = board_2d(vec![ship, n, s, w]);
        let a = action_2d(TargetingPattern::BROADSIDE, Some(Arc::BroadsideArc), false);
        let mut hit = resolve_targeting_2d(&a, &board, Pos::new(2, 2));
        hit.sort_by_key(|p| p.to_index());
        assert_eq!(hit, vec![Pos::new(2, 0), Pos::new(2, 3)]);
    }

    #[test]
    fn rt2d_spinal_pierces_all_when_hits_all() {
        // Forward spinal up column 2 from (2,3): both (2,1) and (2,0) pierce.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let a1 = ship_2d(
            "a",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let a0 = ship_2d(
            "b",
            Faction::Enemy,
            Pos::new(2, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![player, a1, a0]);
        let pierce = action_2d(TargetingPattern::SPINAL_LINE, Some(Arc::Forward), true);
        assert_eq!(
            resolve_targeting_2d(&pierce, &board, Pos::new(2, 3)),
            vec![Pos::new(2, 1), Pos::new(2, 0)]
        );
        // hits_all=false -> just the first.
        let first = action_2d(TargetingPattern::SPINAL_LINE, Some(Arc::Forward), false);
        assert_eq!(
            resolve_targeting_2d(&first, &board, Pos::new(2, 3)),
            vec![Pos::new(2, 1)]
        );
    }

    #[test]
    fn rt2d_blast_hits_center_plus_eight_neighbours() {
        // Forward blast up column 2 from (3,3): center is the first occupant
        // (3,1); splash is its in-bounds 8-neighbours. (The ±1->8-neighbour
        // widening, reviewer gate #2.)
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(3, 3),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let center = ship_2d(
            "c",
            Faction::Enemy,
            Pos::new(3, 1),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![player, center]);
        let a = action_2d(TargetingPattern::BLAST, Some(Arc::Forward), false);
        let hit = resolve_targeting_2d(&a, &board, Pos::new(3, 3));
        assert!(hit.contains(&Pos::new(3, 1)), "center");
        // 8-neighbours of (3,1) that are in-bounds, e.g. (2,1),(4,1),(3,0),(3,2),(2,0)...
        assert!(hit.contains(&Pos::new(2, 1)) && hit.contains(&Pos::new(4, 1)));
        assert!(hit.contains(&Pos::new(3, 0)) && hit.contains(&Pos::new(3, 2)));
        // center + 8 neighbours, all in-bounds for an interior cell.
        assert_eq!(hit.len(), 1 + crate::grid::neighbors(Pos::new(3, 1)).len());
    }

    #[test]
    fn rt2d_far_only_weapon_cannot_hit_adjacent_deadzone() {
        // Decision #7 over-extension deadzone: a Far-only weapon does NOT bear
        // on an adjacent target. Player (2,3) Bow(N); enemy adjacent at (2,2).
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let adj = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 2),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![player, adj]);
        let mut a = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        a.targeting.range_band = vec![Range::Far]; // Far only
        assert!(
            resolve_targeting_2d(&a, &board, Pos::new(2, 3)).is_empty(),
            "Far-only weapon must not hit an Adjacent (dist-1) target"
        );
    }

    #[test]
    fn rt2d_broadside_bow_stance_does_not_bear() {
        // A BroadsideArc weapon on a Bow stance never bears (wrong stance).
        let ship = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::BroadsideArc,
        );
        let n = ship_2d(
            "n",
            Faction::Enemy,
            Pos::new(2, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let board = board_2d(vec![ship, n]);
        let a = action_2d(TargetingPattern::BROADSIDE, Some(Arc::BroadsideArc), false);
        assert!(resolve_targeting_2d(&a, &board, Pos::new(2, 2)).is_empty());
    }

    /// (#75) THE rotation gate: a `REORIENT::RotateRight` changes the player's
    /// FACING by +90 (N→E), re-derives orientation, AND the fire-gate follows
    /// end to end. A Forward beam that bore NORTH (hitting the enemy due N)
    /// must, after one rotate-right, bear EAST (hit the enemy due E and NOT the
    /// one due N) — proving render-facing and combat-facing rotate together
    /// (the bug was Tab moving orientation while facing/arcs stood still).
    #[test]
    fn rotate_right_turns_facing_and_the_fire_gate_follows() {
        // Player at the interior cell (2,2) so it has occupants due N and due E.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        );
        let north = ship_2d(
            "n",
            Faction::Enemy,
            Pos::new(2, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        );
        let east = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(4, 2),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        let mut board = board_2d(vec![player, north, east]);
        let beam = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);

        // Before: Forward beam bears NORTH up column 2 → hits the enemy at (2,0).
        assert_eq!(
            resolve_targeting_2d(&beam, &board, Pos::new(2, 2)),
            vec![Pos::new(2, 0)],
            "pre-rotate: Forward beam bears N"
        );

        // Apply the live rotate-right REORIENT effect (the queued-action path).
        let action = synthetic_rotate_right_action();
        let fx = action.effects[0].clone();
        apply_effect(
            &fx,
            &action,
            Pos::new(2, 2).to_index(),
            &[],
            &mut board,
            &NoContent,
        );

        // Facing turned N→E; orientation re-derived to Broadside (E/W flank).
        let p = board
            .ship_at(Pos::new(2, 2))
            .expect("player still at (2,2)");
        assert_eq!(p.facing, Facing::Bow(Dir4::E), "rotate-right: N→E");
        assert_eq!(
            p.orientation,
            Orientation::Broadside,
            "orientation re-derived from facing"
        );

        // After: the SAME Forward beam now bears EAST along row 2 → hits (4,2),
        // and no longer the northern enemy. The fire-gate followed the facing.
        assert_eq!(
            resolve_targeting_2d(&beam, &board, Pos::new(2, 2)),
            vec![Pos::new(4, 2)],
            "post-rotate: Forward beam bears E (arc followed the rotated facing)"
        );
    }

    /// Rotate-LEFT is the inverse: four rotate-lefts return the facing to start,
    /// and one rotate-left from Bow(N) is Bow(W).
    #[test]
    fn rotate_left_is_ccw_and_four_turns_round_trip() {
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        )]);
        let action = synthetic_rotate_left_action();
        let fx = action.effects[0].clone();
        apply_effect(
            &fx,
            &action,
            Pos::new(2, 2).to_index(),
            &[],
            &mut board,
            &NoContent,
        );
        assert_eq!(
            board.ship_at(Pos::new(2, 2)).unwrap().facing,
            Facing::Bow(Dir4::W),
            "N→W (ccw)"
        );
        for _ in 0..3 {
            apply_effect(
                &fx,
                &action,
                Pos::new(2, 2).to_index(),
                &[],
                &mut board,
                &NoContent,
            );
        }
        assert_eq!(
            board.ship_at(Pos::new(2, 2)).unwrap().facing,
            Facing::Bow(Dir4::N),
            "four ccw turns round-trip"
        );
    }

    /// (#75) Tab's 180° about-face: two `RotateRight` effects (the bin's Tab
    /// applies exactly this) reverse the bow (N->S), so the hull visibly turns
    /// around — the fix for "Tab does nothing to the ship" (it used to toggle
    /// orientation only, which no longer moves the facing-driven render/arcs).
    #[test]
    fn two_rotate_rights_are_a_180_about_face() {
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::Forward,
        )]);
        let action = synthetic_rotate_right_action();
        let fx = action.effects[0].clone();
        apply_effect(
            &fx,
            &action,
            Pos::new(2, 2).to_index(),
            &[],
            &mut board,
            &NoContent,
        );
        apply_effect(
            &fx,
            &action,
            Pos::new(2, 2).to_index(),
            &[],
            &mut board,
            &NoContent,
        );
        let p = board.ship_at(Pos::new(2, 2)).unwrap();
        assert_eq!(
            p.facing,
            Facing::Bow(Dir4::S),
            "N + 2x rotate-right = S (about-face)"
        );
        assert_eq!(
            p.orientation,
            Orientation::BowOn { bow: LaneEnd::Aft },
            "orientation re-derived: S -> Aft"
        );
    }

    /// Local builders for the rotate REORIENT effects (mirror
    /// `input::synthetic_rotate_*` without depending on the input module here).
    fn synthetic_rotate_right_action() -> Action {
        let mut a = action_2d(TargetingPattern::SELF, None, false);
        a.effects = vec![Effect::REORIENT {
            to: ReorientTo::RotateRight,
        }];
        a
    }
    fn synthetic_rotate_left_action() -> Action {
        let mut a = action_2d(TargetingPattern::SELF, None, false);
        a.effects = vec![Effect::REORIENT {
            to: ReorientTo::RotateLeft,
        }];
        a
    }

    /* =====================================================================
     * resolve_self_move_2d (R6) — sanity coverage over a real 2-D board.
     * One+ per mode + the invariant-(A) slot==pos maintenance. Deeper
     * coverage is the tester's lane.
     * ================================================================== */

    /// Assert the ship `id` is at `pos` AND invariant (A) holds for it
    /// (slot == `pos.to_index()`, pos == cell-as-index).
    fn assert_ship_at(board: &Board, id: &str, pos: Pos) {
        let s = board
            .ship_at(pos)
            .unwrap_or_else(|| panic!("{id} not at {pos:?}"));
        assert_eq!(s.id, id, "wrong ship at {pos:?}");
        assert_eq!(s.pos, pos, "{id}.pos mismatch");
        assert_eq!(
            s.cell,
            pos.to_index(),
            "{id}.cell != pos.to_index (invariant A)"
        );
    }

    #[test]
    fn rsm2d_thrust_moves_one_cell_along_direction_2d_override() {
        // Override N from (2,2): move to (2,1), slot+pos updated, old cell empty.
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        )]);
        resolve_self_move_2d(
            Pos::new(2, 2),
            MovementMode::THRUST,
            1,
            Some(Dir4::N),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(2, 1));
        assert!(board.ship_at(Pos::new(2, 2)).is_none(), "old cell vacated");
    }

    #[test]
    fn rsm2d_thrust_none_derives_direction_from_facing() {
        // No override: a Bow(N) ship thrusts N (toward row 0). From (2,2)->(2,1).
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        )]);
        resolve_self_move_2d(
            Pos::new(2, 2),
            MovementMode::THRUST,
            1,
            None,
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(2, 1));
    }

    #[test]
    fn rsm2d_thrust_wall_blocks_stays_and_takes_collision() {
        // Bow(N) at (2,0) (back row): N is off-grid -> stay, take 1 collision.
        // Use a SHIELDLESS hull so the 1 collision reaches hull (the directional
        // shield would otherwise mediate it — a bow-first wall hit lands on the
        // strong bow armour, which is its own behavior, not what this asserts).
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        let mut p = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 0),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        p.shield_profile = zero;
        let mut board = board_2d(vec![p]);
        let hull_before = board.ship_at(Pos::new(2, 0)).unwrap().hull;
        resolve_self_move_2d(
            Pos::new(2, 0),
            MovementMode::THRUST,
            1,
            None,
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(2, 0)); // didn't move
        assert_eq!(
            board.ship_at(Pos::new(2, 0)).unwrap().hull,
            hull_before - 1,
            "wall collision = 1 (shieldless)"
        );
    }

    #[test]
    fn rsm2d_thrust_occupant_blocks_stays_and_takes_collision() {
        // Mover at (2,2) Bow(N); blocker at (2,1). Mover stays, takes 1.
        // Shieldless mover so the collision reaches hull regardless of which
        // zone absorbs it (the collision DIRECTION->zone is provisional until
        // R4 migrates apply_damage to 2D; this asserts the collision happened +
        // the block, not the zone).
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        let mut p = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 2),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        p.shield_profile = zero;
        let mut board = board_2d(vec![
            p,
            ship_2d(
                "b",
                Faction::Enemy,
                Pos::new(2, 1),
                Facing::Bow(Dir4::S),
                Arc::Turret,
            ),
        ]);
        let hull_before = board.ship_at(Pos::new(2, 2)).unwrap().hull;
        resolve_self_move_2d(
            Pos::new(2, 2),
            MovementMode::THRUST,
            1,
            None,
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(2, 2)); // blocked
        assert_ship_at(&board, "b", Pos::new(2, 1)); // blocker unmoved
        assert_eq!(board.ship_at(Pos::new(2, 2)).unwrap().hull, hull_before - 1);
    }

    #[test]
    fn rsm2d_burn_walks_until_occupant() {
        // BURN W distance 3 from (4,1): walks (3,1),(2,1) then stops before
        // blocker at (1,1). Lands (2,1).
        let mut board = board_2d(vec![
            ship_2d(
                "p",
                Faction::Player,
                Pos::new(4, 1),
                Facing::Bow(Dir4::W),
                Arc::Turret,
            ),
            ship_2d(
                "b",
                Faction::Enemy,
                Pos::new(1, 1),
                Facing::Bow(Dir4::E),
                Arc::Turret,
            ),
        ]);
        resolve_self_move_2d(
            Pos::new(4, 1),
            MovementMode::BURN,
            3,
            Some(Dir4::W),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(2, 1));
        assert_ship_at(&board, "b", Pos::new(1, 1));
    }

    #[test]
    fn rsm2d_jump_blinks_to_target_cell() {
        // JUMP S distance 2 from (0,0): direct to (0,2), no path scan.
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(0, 0),
            Facing::Bow(Dir4::S),
            Arc::Turret,
        )]);
        resolve_self_move_2d(
            Pos::new(0, 0),
            MovementMode::JUMP,
            2,
            Some(Dir4::S),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(0, 2));
    }

    #[test]
    fn rsm2d_jump_fails_on_occupied_target() {
        // JUMP S dist 2 from (0,0) but (0,2) occupied -> jump fails, stay.
        let mut board = board_2d(vec![
            ship_2d(
                "p",
                Faction::Player,
                Pos::new(0, 0),
                Facing::Bow(Dir4::S),
                Arc::Turret,
            ),
            ship_2d(
                "x",
                Faction::Enemy,
                Pos::new(0, 2),
                Facing::Bow(Dir4::N),
                Arc::Turret,
            ),
        ]);
        resolve_self_move_2d(
            Pos::new(0, 0),
            MovementMode::JUMP,
            2,
            Some(Dir4::S),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(0, 0)); // stayed
        assert_ship_at(&board, "x", Pos::new(0, 2));
    }

    #[test]
    fn rsm2d_tractor_swap_trades_with_adjacent_and_keeps_invariant() {
        // SWAP E from (1,1): trade with occupant at (2,1). Both pos+slot swap.
        let mut board = board_2d(vec![
            ship_2d(
                "p",
                Faction::Player,
                Pos::new(1, 1),
                Facing::Bow(Dir4::E),
                Arc::Turret,
            ),
            ship_2d(
                "o",
                Faction::Enemy,
                Pos::new(2, 1),
                Facing::Bow(Dir4::W),
                Arc::Turret,
            ),
        ]);
        resolve_self_move_2d(
            Pos::new(1, 1),
            MovementMode::TRACTOR_SWAP,
            1,
            Some(Dir4::E),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "o", Pos::new(1, 1)); // other now where p was
        assert_ship_at(&board, "p", Pos::new(2, 1)); // p now where other was
    }

    #[test]
    fn rsm2d_tractor_swap_no_adjacent_is_noop() {
        // SWAP E from (4,3) (col 4 = E edge): nothing adjacent -> no-op.
        let mut board = board_2d(vec![ship_2d(
            "p",
            Faction::Player,
            Pos::new(4, 3),
            Facing::Bow(Dir4::E),
            Arc::Turret,
        )]);
        resolve_self_move_2d(
            Pos::new(4, 3),
            MovementMode::TRACTOR_SWAP,
            1,
            Some(Dir4::E),
            None,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "p", Pos::new(4, 3)); // unmoved
    }

    #[test]
    fn resolver_ai_move_serves_all_four_cardinals_with_real_dir4() {
        // The resolver-owned fallback maps each synthetic id to a 1-cell
        // DISPLACE_SELF carrying the right Dir4 in direction_2d.
        for (id, want) in [
            (crate::input::SYNTHETIC_MOVE_UP, Dir4::N),
            (crate::input::SYNTHETIC_MOVE_DOWN, Dir4::S),
            (crate::input::SYNTHETIC_MOVE_LEFT, Dir4::W),
            (crate::input::SYNTHETIC_MOVE_RIGHT, Dir4::E),
        ] {
            let a = resolver_ai_move(id).unwrap_or_else(|| panic!("no action for {id}"));
            match &a.effects[0] {
                Effect::DISPLACE_SELF {
                    direction_2d,
                    mode,
                    distance,
                    ..
                } => {
                    assert_eq!(*direction_2d, Some(want), "{id} dir");
                    assert_eq!(*mode, MovementMode::THRUST);
                    assert_eq!(*distance, 1);
                }
                other => panic!("expected DISPLACE_SELF, got {other:?}"),
            }
        }
        assert!(resolver_ai_move("not_a_move").is_none());
    }

    /* =====================================================================
     * resolve_target_move_2d (R6b) — push / pull / swap over a real 2-D board.
     * ================================================================== */

    #[test]
    fn rtm2d_push_moves_target_away_from_source() {
        // Source at (1,1), target at (2,1) (E of source). Push 2 -> target
        // moves E (away) to (4,1)? offset E by 2 from (2,1) = (4,1) (cols 3,4
        // free). Lands (4,1).
        let mut board = board_2d(vec![
            ship_2d(
                "src",
                Faction::Player,
                Pos::new(1, 1),
                Facing::Bow(Dir4::E),
                Arc::Turret,
            ),
            ship_2d(
                "tgt",
                Faction::Enemy,
                Pos::new(2, 1),
                Facing::Bow(Dir4::W),
                Arc::Turret,
            ),
        ]);
        resolve_target_move_2d(
            Pos::new(2, 1),
            Pos::new(1, 1),
            crate::types::DisplaceMode::Push,
            2,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "tgt", Pos::new(4, 1)); // pushed E by 2
        assert_ship_at(&board, "src", Pos::new(1, 1)); // source unmoved
    }

    #[test]
    fn rtm2d_push_blocked_by_occupant_stops_and_collides() {
        // Source (0,1), target (1,1), blocker (3,1). Push 3 E: target walks
        // (2,1) then stops before blocker (3,1). Lands (2,1). Shieldless target
        // so the collision (remaining 2) is observable on hull.
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        let mut tgt = ship_2d(
            "tgt",
            Faction::Enemy,
            Pos::new(1, 1),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        tgt.shield_profile = zero;
        let mut board = board_2d(vec![
            ship_2d(
                "src",
                Faction::Player,
                Pos::new(0, 1),
                Facing::Bow(Dir4::E),
                Arc::Turret,
            ),
            tgt,
            ship_2d(
                "blk",
                Faction::Enemy,
                Pos::new(3, 1),
                Facing::Bow(Dir4::W),
                Arc::Turret,
            ),
        ]);
        let hull_before = board.ship_at(Pos::new(1, 1)).unwrap().hull;
        resolve_target_move_2d(
            Pos::new(1, 1),
            Pos::new(0, 1),
            crate::types::DisplaceMode::Push,
            3,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "tgt", Pos::new(2, 1)); // stopped before blocker
        assert_ship_at(&board, "blk", Pos::new(3, 1));
        assert_eq!(
            board.ship_at(Pos::new(2, 1)).unwrap().hull,
            hull_before - 2,
            "remaining-2 collision"
        );
    }

    #[test]
    fn rtm2d_pull_moves_target_toward_source() {
        // Source (0,1), target (3,1). Pull 2 -> target moves W (toward source)
        // to (1,1) (cols 2,1 free), stopping short of source.
        let mut board = board_2d(vec![
            ship_2d(
                "src",
                Faction::Player,
                Pos::new(0, 1),
                Facing::Bow(Dir4::E),
                Arc::Turret,
            ),
            ship_2d(
                "tgt",
                Faction::Enemy,
                Pos::new(3, 1),
                Facing::Bow(Dir4::W),
                Arc::Turret,
            ),
        ]);
        resolve_target_move_2d(
            Pos::new(3, 1),
            Pos::new(0, 1),
            crate::types::DisplaceMode::Pull,
            2,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "tgt", Pos::new(1, 1)); // pulled W by 2
        assert_ship_at(&board, "src", Pos::new(0, 1));
    }

    #[test]
    fn rtm2d_swap_trades_source_and_target_cells() {
        // Swap source (1,2) <-> target (3,0): both pos+slot swap, invariant kept.
        let mut board = board_2d(vec![
            ship_2d(
                "src",
                Faction::Player,
                Pos::new(1, 2),
                Facing::Bow(Dir4::N),
                Arc::Turret,
            ),
            ship_2d(
                "tgt",
                Faction::Enemy,
                Pos::new(3, 0),
                Facing::Bow(Dir4::S),
                Arc::Turret,
            ),
        ]);
        resolve_target_move_2d(
            Pos::new(3, 0),
            Pos::new(1, 2),
            crate::types::DisplaceMode::Swap,
            1,
            &mut board,
            &NoContent,
        );
        assert_ship_at(&board, "src", Pos::new(3, 0)); // source now where target was
        assert_ship_at(&board, "tgt", Pos::new(1, 2)); // target now where source was
    }

    #[test]
    fn rtm2d_empty_target_cell_is_noop() {
        // No ship at the target pos -> no-op (no panic).
        let mut board = board_2d(vec![ship_2d(
            "src",
            Faction::Player,
            Pos::new(0, 0),
            Facing::Bow(Dir4::E),
            Arc::Turret,
        )]);
        resolve_target_move_2d(
            Pos::new(2, 2),
            Pos::new(0, 0),
            crate::types::DisplaceMode::Push,
            1,
            &mut board,
            &NoContent,
        );
        assert!(board.ship_at(Pos::new(2, 2)).is_none());
        assert_ship_at(&board, "src", Pos::new(0, 0));
    }

    /* =====================================================================
     * advance_projectile_2d (R5) — ordnance steps across the 2-D grid.
     * Sanity coverage: clean travel + off-grid removal, non-owner impact
     * through the 2-D damage pipeline (directional!), owner pass-through,
     * APPLY_STATUS payload. Deeper coverage is the tester's lane.
     * ================================================================== */

    /// A projectile at `pos` heading `heading8` with `speed` and a payload.
    /// `cell` mirrors `pos.to_index()` (invariant A); `heading` (1-D) is set
    /// to a coherent `LaneEnd` but is unused on the 2-D path.
    fn proj_2d(
        id: &str,
        pos: Pos,
        heading8: crate::grid::Dir8,
        speed: u32,
        owner: Faction,
        payload: Vec<Effect>,
    ) -> Projectile {
        Projectile {
            id: id.into(),
            kind: "torpedo".into(),
            cell: pos.to_index(),
            pos,
            heading: LaneEnd::Fore,
            heading8,
            speed,
            hull: 1,
            payload,
            owner_faction: owner,
        }
    }

    #[test]
    fn ap2d_travels_speed_cells_and_keeps_invariant() {
        // A torpedo at (0,1) heading E, speed 2, no occupant in its path:
        // walks to (2,1) and stays live with pos+cell in sync (invariant A).
        let mut board = board_2d(vec![]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(0, 1),
            crate::grid::Dir8::E,
            2,
            Faction::Player,
            vec![Effect::DAMAGE {
                amount: 3,
                band_falloff: None,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        let p = board
            .ordnance
            .iter()
            .find(|p| p.id == "t")
            .expect("still in flight");
        assert_eq!(p.pos, Pos::new(2, 1), "advanced 2 cells E");
        assert_eq!(
            p.cell,
            Pos::new(2, 1).to_index(),
            "cell mirror in sync (invariant A)"
        );
    }

    #[test]
    fn ap2d_off_grid_removes_projectile() {
        // At (4,1) (east edge) heading E: the next step is off-grid -> the
        // projectile flies off the board and is removed.
        let mut board = board_2d(vec![]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(4, 1),
            crate::grid::Dir8::E,
            1,
            Faction::Player,
            vec![Effect::DAMAGE {
                amount: 3,
                band_falloff: None,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        assert!(
            board.ordnance.iter().all(|p| p.id != "t"),
            "off-grid projectile removed"
        );
    }

    #[test]
    fn ap2d_impact_on_enemy_applies_damage_and_removes() {
        // Player torpedo at (0,1) heading E, speed 3; a shieldless enemy sits
        // at (2,1). The projectile reaches (2,1), drops its DAMAGE payload
        // through apply_damage_2d, and is consumed. Shieldless so the hit is
        // observable on hull regardless of which face it lands on.
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: crate::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        };
        let mut tgt = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        tgt.shield_profile = zero;
        let hull_before = tgt.hull;
        let mut board = board_2d(vec![tgt]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(0, 1),
            crate::grid::Dir8::E,
            3,
            Faction::Player,
            vec![Effect::DAMAGE {
                amount: 5,
                band_falloff: None,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        // Adjacent-band falloff factor 1.0 on the (1,1)->(2,1) impact step:
        // floor(5 * 1.0) = 5 lands on a shieldless hull.
        assert_eq!(
            board.ship_at(Pos::new(2, 1)).unwrap().hull,
            hull_before - 5,
            "payload landed"
        );
        assert!(
            board.ordnance.iter().all(|p| p.id != "t"),
            "consumed on impact"
        );
    }

    #[test]
    fn ap2d_impact_hits_the_face_the_shot_came_at() {
        // The 2-D improvement over the 1-D path: incoming_from is opposite the
        // heading, so the projectile strikes the hull face it actually flew at.
        // Torpedo heading E impacts a ship whose BOW faces W (toward the
        // incoming shot) -> the bow armour soaks it. Same ship, same payload,
        // but the strong bow now absorbs where a stern would not.
        let mut tgt = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        // #103 Model A pool profile: bow pool {4,4}. Shot from W hits the bow ->
        // its full pool soaks the falloff-2 torpedo, 0 reaches hull.
        tgt.shield_profile = pooled_shield_profile();
        let hull_before = tgt.hull;
        let mut board = board_2d(vec![tgt]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(0, 1),
            crate::grid::Dir8::E,
            3,
            Faction::Player,
            vec![Effect::DAMAGE {
                amount: 2,
                band_falloff: None,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        // incoming_from = opposite(E) = W; facing Bow(W) -> bow zone, pool {4,4}.
        // falloff at Adjacent = 2 raw; the bow pool soaks all 2, 0 reaches hull.
        assert_eq!(
            board.ship_at(Pos::new(2, 1)).unwrap().hull,
            hull_before,
            "bow pool soaked the head-on shot"
        );
    }

    #[test]
    fn ap2d_passes_through_own_faction_occupant() {
        // A projectile does NOT detonate on a same-faction ship. Player torpedo
        // heading E over a friendly Player ship at (2,1): it keeps going (here
        // speed 3 carries it past to (3,1)) rather than impacting.
        let friendly = ship_2d(
            "f",
            Faction::Player,
            Pos::new(2, 1),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        let mut board = board_2d(vec![friendly]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(0, 1),
            crate::grid::Dir8::E,
            3,
            Faction::Player,
            vec![Effect::DAMAGE {
                amount: 5,
                band_falloff: None,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        // Friendly unharmed; projectile flew past to (3,1).
        assert_eq!(
            board.ship_at(Pos::new(2, 1)).unwrap().hull,
            10,
            "friendly untouched"
        );
        let p = board
            .ordnance
            .iter()
            .find(|p| p.id == "t")
            .expect("still flying past friendly");
        assert_eq!(p.pos, Pos::new(3, 1), "passed through to (3,1)");
    }

    #[test]
    fn ap2d_apply_status_payload_lands_on_impact() {
        // An APPLY_STATUS payload is applied to the impacted enemy on contact.
        let tgt = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(1, 1),
            Facing::Bow(Dir4::W),
            Arc::Turret,
        );
        let mut board = board_2d(vec![tgt]);
        board.ordnance.push(proj_2d(
            "t",
            Pos::new(0, 1),
            crate::grid::Dir8::E,
            1,
            Faction::Player,
            vec![Effect::APPLY_STATUS {
                status: StatusKind::TargetLock,
                duration: 2,
            }],
        ));
        advance_projectile_2d("t", &mut board, &NoContent);
        let e = board.ship_at(Pos::new(1, 1)).expect("enemy present");
        assert!(
            e.statuses.iter().any(|s| s.kind == StatusKind::TargetLock),
            "status applied on impact"
        );
        assert!(
            board.ordnance.iter().all(|p| p.id != "t"),
            "consumed on impact"
        );
    }

    /* =====================================================================
     * paint_threats (R8) — ThreatMap painted via the SAME resolve_targeting_2d
     * spine the AI elects + fires with (single-source, V4-at-R8). Sanity
     * coverage; deeper coverage is the tester's lane.
     * ================================================================== */

    /// Content serving one named action (for queued-threat resolution).
    struct OneAction(String, Action);
    impl Content for OneAction {
        fn action(&self, id: &str) -> Option<&Action> {
            (id == self.0).then_some(&self.1)
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            panic!("spawn_projectile not used in this test");
        }
    }

    /// An enemy at `pos`/`facing` with one mount + a single queued action id.
    fn enemy_queued_2d(id: &str, pos: Pos, facing: Facing, arc: Arc, queued: &str) -> Ship {
        let mut s = ship_2d(id, Faction::Enemy, pos, facing, arc);
        s.queue = vec![queued.to_string()];
        s
    }

    #[test]
    fn pt2d_paints_damage_threat_on_the_targeted_cell() {
        // Enemy at (2,1) Bow(S) with a queued forward beam; player at (2,3) is
        // down the S column. The threat paints on the player's cell, kind
        // Damage{4} (pulse_laser's raw), source = the enemy's pos.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let enemy = enemy_queued_2d(
            "e",
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
            "pulse_laser",
        );
        let mut board = board_2d(vec![player, enemy]);
        // pulse_laser with the full band so it bears at this range.
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("pulse_laser".into(), weapon);
        paint_threats(&mut board, &content);
        assert_eq!(board.threats.len(), 1, "exactly one threatened cell");
        let t = board.threats[0];
        assert_eq!(t.pos, Pos::new(2, 3), "threat on the player's cell");
        assert_eq!(t.source, Pos::new(2, 1), "source is the firing enemy");
        assert_eq!(
            t.kind,
            crate::types::ThreatKind::Damage { amount: 4 },
            "raw pulse damage"
        );
    }

    #[test]
    fn pt2d_threat_set_equals_resolve_targeting_2d_single_source() {
        // THE V4-at-R8 invariant: the painted cell set is exactly what
        // resolve_targeting_2d returns for the same queued action + enemy pos.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let enemy = enemy_queued_2d(
            "e",
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
            "pulse_laser",
        );
        let mut board = board_2d(vec![player, enemy]);
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("pulse_laser".into(), weapon.clone());
        // The spine, called directly.
        let fired = resolve_targeting_2d(&weapon, &board, Pos::new(2, 1));
        paint_threats(&mut board, &content);
        let painted: Vec<Pos> = board.threats.iter().map(|t| t.pos).collect();
        assert_eq!(
            painted, fired,
            "painted threats must equal the fired cell set"
        );
    }

    #[test]
    fn pt2d_empty_queue_paints_nothing() {
        // An enemy with no queued action telegraphs no threat.
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let enemy = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let mut board = board_2d(vec![player, enemy]);
        let content = OneAction("pulse_laser".into(), pulse_laser());
        paint_threats(&mut board, &content);
        assert!(board.threats.is_empty(), "no queue -> no threat");
    }

    #[test]
    fn pt2d_clears_stale_threats_each_pass() {
        // paint_threats rebuilds: a pre-populated stale threat is cleared even
        // when the current queues paint nothing.
        let enemy = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let mut board = board_2d(vec![enemy]);
        board.threats.push(crate::types::Threat {
            pos: Pos::new(0, 0),
            kind: crate::types::ThreatKind::Other,
            source: Pos::new(2, 1),
        });
        let content = OneAction("pulse_laser".into(), pulse_laser());
        paint_threats(&mut board, &content);
        assert!(board.threats.is_empty(), "stale threat cleared on rebuild");
    }

    #[test]
    fn pt2d_does_not_paint_when_player_off_the_ray() {
        // Player not on the enemy's forward column -> the queued beam bears on
        // nothing -> no threat (the deterministic basis for R7's whiff: the
        // shot will find an empty cell).
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(0, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let enemy = enemy_queued_2d(
            "e",
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
            "pulse_laser",
        );
        let mut board = board_2d(vec![player, enemy]);
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("pulse_laser".into(), weapon);
        paint_threats(&mut board, &content);
        assert!(
            board.threats.is_empty(),
            "off-ray queued shot paints no threat"
        );
    }

    #[test]
    fn threat_kind_classifies_by_effect_family() {
        use crate::types::ThreatKind;
        // Damage wins even if combined with displace.
        let mut dmg = pulse_laser();
        dmg.effects = vec![
            Effect::DISPLACE_TARGET {
                mode: crate::types::DisplaceMode::Push,
                distance: 1,
            },
            Effect::DAMAGE {
                amount: 3,
                band_falloff: None,
            },
        ];
        assert_eq!(threat_kind(&dmg), ThreatKind::Damage { amount: 3 });

        let mut disp = pulse_laser();
        disp.effects = vec![Effect::DISPLACE_TARGET {
            mode: crate::types::DisplaceMode::Pull,
            distance: 2,
        }];
        assert_eq!(threat_kind(&disp), ThreatKind::Displace);

        let mut status = pulse_laser();
        status.effects = vec![Effect::APPLY_STATUS {
            status: StatusKind::TargetLock,
            duration: 3,
        }];
        assert_eq!(threat_kind(&status), ThreatKind::Status);

        let mut other = pulse_laser();
        other.effects = vec![Effect::VENT_HEAT {
            amount: 2,
            recharge_cooldowns: None,
        }];
        assert_eq!(threat_kind(&other), ThreatKind::Other);
    }

    /* =====================================================================
     * R7 — dodge whiff (hit:false). A telegraphed shot whose target cell the
     * player has VACATED emits a hit:false FireEvent so the renderer draws the
     * beam firing into the now-empty cell. Reads the R8 telegraph
     * (board.threats, source == firer). Sanity coverage.
     * ================================================================== */

    #[test]
    fn r7_whiff_emitted_when_player_vacated_a_telegraphed_cell() {
        // Enemy at (2,1) Bow(S) telegraphed a hit on (2,3) last phase. The
        // player has since moved off (2,3) — that cell is now empty. Firing the
        // queued beam this phase: no target on the S ray -> nothing-bore, but
        // the whiff draws first: a hit:false FireEvent (2,1) -> (2,3).
        let enemy = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let mut board = board_2d(vec![enemy]); // (2,3) deliberately EMPTY now
        board.threats.push(crate::types::Threat {
            pos: Pos::new(2, 3),
            kind: crate::types::ThreatKind::Damage { amount: 4 },
            source: Pos::new(2, 1),
        });
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("w".into(), weapon.clone());
        run_action("e", "w", &weapon, &mut board, &content);
        let whiffs: Vec<_> = board.fire_events.iter().filter(|f| !f.hit).collect();
        assert_eq!(whiffs.len(), 1, "exactly one whiff");
        assert_eq!(whiffs[0].from_pos, Pos::new(2, 1));
        assert_eq!(whiffs[0].to_pos, Pos::new(2, 3));
        assert!(!whiffs[0].hit, "hit:false");
    }

    #[test]
    fn r7_no_whiff_when_telegraphed_cell_still_occupied() {
        // Same telegraph, but the player is STILL on (2,3). The shot connects
        // normally (hit:true) and there is NO whiff.
        let enemy = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let player = ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            Facing::Bow(Dir4::N),
            Arc::Turret,
        );
        let mut board = board_2d(vec![enemy, player]);
        board.threats.push(crate::types::Threat {
            pos: Pos::new(2, 3),
            kind: crate::types::ThreatKind::Damage { amount: 4 },
            source: Pos::new(2, 1),
        });
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("w".into(), weapon.clone());
        run_action("e", "w", &weapon, &mut board, &content);
        assert!(
            board.fire_events.iter().all(|f| f.hit),
            "occupied target -> no whiff, only hit:true"
        );
        assert!(
            board
                .fire_events
                .iter()
                .any(|f| f.hit && f.to_pos == Pos::new(2, 3)),
            "the connecting hit is recorded"
        );
    }

    #[test]
    fn r7_no_whiff_for_a_non_damage_action() {
        // A queued non-DAMAGE action (here a SELF vent) does not whiff even if a
        // stale threat exists — only DAMAGE-bearing shots draw a miss.
        let mut enemy = ship_2d(
            "e",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        enemy.heat = 3; // so the vent does something; irrelevant to the whiff gate
        let mut board = board_2d(vec![enemy]);
        board.threats.push(crate::types::Threat {
            pos: Pos::new(2, 3),
            kind: crate::types::ThreatKind::Damage { amount: 4 },
            source: Pos::new(2, 1),
        });
        let mut vent = pulse_laser();
        vent.targeting.pattern = TargetingPattern::SELF;
        vent.targeting.requires_arc = None;
        vent.effects = vec![Effect::VENT_HEAT {
            amount: 2,
            recharge_cooldowns: None,
        }];
        let content = OneAction("vent".into(), vent.clone());
        run_action("e", "vent", &vent, &mut board, &content);
        assert!(
            board.fire_events.is_empty(),
            "a non-DAMAGE action emits no FireEvent (hit or whiff)"
        );
    }

    #[test]
    fn r7_whiff_only_for_this_firers_telegraph() {
        // Two enemies telegraphed different cells; firing enemy A must only
        // whiff A's vacated cell, not B's (filtered by Threat.source).
        let a = ship_2d(
            "a",
            Faction::Enemy,
            Pos::new(2, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let b = ship_2d(
            "b",
            Faction::Enemy,
            Pos::new(0, 1),
            Facing::Bow(Dir4::S),
            Arc::Forward,
        );
        let mut board = board_2d(vec![a, b]); // (2,3) and (0,3) both empty
        board.threats.push(crate::types::Threat {
            pos: Pos::new(2, 3),
            kind: crate::types::ThreatKind::Damage { amount: 4 },
            source: Pos::new(2, 1),
        });
        board.threats.push(crate::types::Threat {
            pos: Pos::new(0, 3),
            kind: crate::types::ThreatKind::Damage { amount: 4 },
            source: Pos::new(0, 1),
        });
        let weapon = action_2d(TargetingPattern::BEAM, Some(Arc::Forward), false);
        let content = OneAction("w".into(), weapon.clone());
        run_action("a", "w", &weapon, &mut board, &content);
        let whiffs: Vec<_> = board.fire_events.iter().filter(|f| !f.hit).collect();
        assert_eq!(whiffs.len(), 1, "only A's telegraph whiffs");
        assert_eq!(
            whiffs[0].to_pos,
            Pos::new(2, 3),
            "A's vacated cell, not B's"
        );
    }
}
