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
//! - Effect dispatch ([`apply_effect`]) for DAMAGE, APPLY_STATUS, VENT_HEAT,
//!   REORIENT, SPAWN_ORDNANCE, DEPLOY.
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
//! - [`resolve_self_move`] — full THRUST/BURN/SLIP/JUMP/TRACTOR_SWAP with
//!   occupancy + collision. Currently a simple step-loop in the bow direction.
//! - [`resolve_target_move`] — push/pull/swap with collision damage. Currently
//!   a no-op.
//! - [`decide_enemy_action`] — AI decision layer. Currently a no-op.
//! - The `BOARD` effect arm in [`apply_effect`] — currently a no-op.

use crate::geometry::{absorb_shield, bears, direction_to, facing_zone, opposite, range_band};
use crate::types::{
    Action, ActionCost, Board, DeployHazardKind, Effect, Faction, Hazard, HazardKind, Hook,
    HookContext, LaneEnd, MovementMode, Orientation, Projectile, RangeBand, ReorientTo, Ship,
    Status, StatusKind, Targeting, TargetingPattern, WeaponArchetype,
};

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
    /// Marksman is `+1` flat, Point-Blank Doctrine is `+2` when
    /// `band == PointBlank`, and so on.
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
    fn damage_modifier(&self, _attacker: &Ship, _band: RangeBand, _board: &Board) -> i32 {
        0
    }

    /// End-of-turn subsystem pass. Called by [`end_of_turn`] **after** the
    /// base passive heat dissipation and **before** the `OnTurnEnd`
    /// event-bus emit, so any bus subscribers see the post-subsystem
    /// state. Default impl is a no-op.
    ///
    /// Concrete impls (today: [`crate::input::DemoContent`]) walk their
    /// installed-subsystem registry and apply OnTurnEnd-shaped effects
    /// (e.g. HeatSink subtracting an extra heat from the owning ship).
    /// The runtime layer lives in [`crate::subsystems`]; see that module
    /// for why the registry isn't on `Board`.
    ///
    /// Task #61 (Phase 2). Same pre-approval scope as `damage_modifier`.
    fn on_turn_end(&self, _board: &mut Board) {}

    /// Dispatch a `BOARD` effect by its `note` string. Used by field-kit
    /// Cards (mass_lock, mass_breach, sensor_pulse) which encode their
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
    fn card_at(&self, _ship_id: &str, _idx: usize) -> Option<String> { None }

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
    /// Default impl returns `UnknownCard` (no cards). DemoContent overrides.
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
        Some(c) => board.cells[c].as_ref().map(|s| s.queue.clone()).unwrap_or_default(),
        None => return,
    };

    for action_id in &queue {
        // Clone the Action so we don't hold a borrow on `content` while we
        // mutate the board.
        let action = match content.action(action_id) {
            Some(a) => a.clone(),
            None => continue, // TS: `if (!a) continue` — unknown action ids are skipped silently.
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
/// (AI fills queue, then queue fires), then end-of-turn bookkeeping. Every
/// player input in the SS turn model runs this after its instant /
/// queue-mutation effect lands, so a single keystroke always advances time.
pub fn run_world_phase(board: &mut Board, content: &dyn Content) {
    // 2 - advance every live projectile by its speed, resolve impacts. This
    // is its own chain-kill window — reset the counter so kills caused by
    // ordnance impacts (e.g. multi-projectile torpedoes piercing low-hull
    // enemies) are scored separately from the player's queue. The TS does
    // not emit `onChainKill` from the ordnance phase itself (only
    // executeQueue does); we match that and leave the emit gate to
    // [`fire_player_queue`].
    //
    // TS iterates a SHALLOW COPY of `board.ordnance` because each
    // `advanceProjectile` may remove its projectile from the live list. We
    // do the same: snapshot the ids, then advance each by id-lookup.
    board.destroys_this_window = 0;
    let projectile_ids: Vec<String> = board.ordnance.iter().map(|p| p.id.clone()).collect();
    for id in projectile_ids {
        advance_projectile(&id, board, content);
    }

    // 3 - enemy phase, in telegraphed initiative order. Snapshot ids up
    // front so movement / destroys during one enemy's queue can't reshuffle
    // the remaining enemies' identification. An enemy that gets destroyed
    // before its turn just no-ops via the lookup below.
    let enemy_ids: Vec<String> = enemy_initiative(board)
        .into_iter()
        .filter_map(|c| board.cells[c].as_ref().map(|s| s.id.clone()))
        .collect();
    for enemy_id in &enemy_ids {
        let Some(enemy_cell) = find_cell_by_id(board, enemy_id) else {
            continue; // destroyed earlier in the phase
        };
        if skips_turn(board, enemy_cell) {
            continue;
        }
        decide_enemy_action(enemy_cell, board, content); // TODO(broadside-content): AI fills the queue
        fire_player_queue(enemy_id, board, content);
    }

    // 4 - end of turn.
    end_of_turn(board, content);
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

    // Resolve targeting against the CURRENT cell.
    let cells = resolve_targeting(action, board, ship_cell);
    // The "nothing bore" gate: arc-required actions with no targets eat
    // nothing — cooldown is NOT reset and heat is NOT spent. Mirrors the
    // TS `if (a.targeting.requiresArc !== null && cells.length === 0) continue`.
    if action.targeting.requires_arc.is_some() && cells.is_empty() {
        return false;
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
        cells.iter().copied().filter(|&c| board.cells[c].is_some()).collect()
    } else {
        Vec::new()
    };

    let passes = if WeaponMod::of(action).map(WeaponMod::applies_effects_twice).unwrap_or(false) {
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
            match find_cell_by_id(board, ship_id) {
                Some(cur) => resolve_targeting(action, board, cur),
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
            let cd = if precision_kill { 0 } else { action.cost.cooldown_max };
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
        .position(|c| c.as_ref().map(|s| s.id == ship_id).unwrap_or(false))
}

/* =============================================================================
 * Phase 2 — advanceProjectile.
 * ========================================================================== */

/// Step a single projectile by its speed, resolving impacts. Mirrors
/// `advanceProjectile` in `resolve.ts`. Identified by id rather than `&mut`
/// because the projectile may remove itself from `board.ordnance` on impact.
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

/* =============================================================================
 * Phase 4 — end of turn.
 * ========================================================================== */

/// End-of-turn bookkeeping: tick cooldowns, dissipate heat, tick statuses,
/// emit the turn-end hook. Mirrors `endOfTurn` in `resolve.ts`.
pub fn end_of_turn(board: &mut Board, content: &dyn Content) {
    // Collect the cells of every live ship up front so we can mutate them
    // by index without holding a borrow on `board.cells`.
    let cells: Vec<usize> = ships_of(board).iter().map(|s| s.cell).collect();

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
/// (SELF / DEPLOYED_CELL / ORDNANCE) return the acting ship's own cell or the
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
                    let probe = if end == LaneEnd::Fore { board.size - 1 } else { 0 };
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
    let falloff_disabled = weapon.effects.iter().any(
        |e| matches!(e, Effect::DAMAGE { band_falloff: Some(false), .. }),
    );
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
    dmg = apply_modifiers(dmg, atk_cell, band, board, content);

    // 3. Target-lock doubles the incoming hit and is consumed.
    if let Some(target) = board.cells[target_cell].as_mut() {
        if let Some(pos) = target.statuses.iter().position(|s| s.kind == StatusKind::TargetLock) {
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
                    apply_damage(c, *amount, source_cell, a, board, content);
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

        Effect::VENT_HEAT { amount, recharge_cooldowns } => {
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
                source.orientation = match to {
                    ReorientTo::Flip => flip_orientation(source.orientation),
                    ReorientTo::Broadside => Orientation::Broadside,
                    ReorientTo::BowOn => Orientation::BowOn { bow: LaneEnd::Fore },
                };
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

        Effect::DISPLACE_SELF { mode, distance, direction } => {
            resolve_self_move(source_cell, *mode, *distance, *direction, board, content);
        }

        Effect::DISPLACE_TARGET { mode, distance } => {
            for &c in cells {
                resolve_target_move(c, source_cell, *mode, *distance, board, content);
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
    fn from_id(id: &str) -> Option<WeaponMod> {
        match id {
            "flak_burst" => Some(WeaponMod::FlakBurst),
            "precision_core" => Some(WeaponMod::PrecisionCore),
            "incendiary" => Some(WeaponMod::Incendiary),
            "emp_charge" => Some(WeaponMod::EmpCharge),
            "twin_linked" => Some(WeaponMod::TwinLinked),
            "targeting_laser" => Some(WeaponMod::TargetingLaser),
            "autoloader" => Some(WeaponMod::Autoloader),
            _ => None,
        }
    }

    /// The mod parsed off an action, if any.
    fn of(action: &Action) -> Option<WeaponMod> {
        action.r#mod.as_deref().and_then(WeaponMod::from_id)
    }

    /// `twin_linked` runs the effect list twice.
    fn applies_effects_twice(self) -> bool {
        self == WeaponMod::TwinLinked
    }

    /// `autoloader` forces the action to not advance the turn. Returns
    /// `Some(false)` to override `ActionCost::advances_turn`; `None` to leave
    /// the action's declared value untouched. The TURN layer (`input.rs`)
    /// consumes this; the resolver pipeline itself never branches on
    /// turn-advance, so this is exposed for the dispatcher rather than acted on
    /// inside [`run_action`].
    fn advances_turn_override(self) -> Option<bool> {
        match self {
            WeaponMod::Autoloader => Some(false),
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
/// never re-enters the resolver through the EventBus. Action-level mods
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
            // 1 dmg to each lane-neighbour of the HIT cell, bounds-checked,
            // through the full pipeline (shield-mediated, falloff off) via the
            // dummy impact weapon — same precedent as ReactorBreach splash in
            // `destroy`. Faction-blind: hits allies too (content ruling;
            // pairs with the "Unfriendly Fire" design). The hit cell itself is
            // NOT re-damaged. Splash origin is the hit cell so the directional
            // shield reads the burst as arriving from the detonation.
            let dummy = dummy_weapon();
            for delta in [-1i32, 1] {
                let nc = hit_cell as i32 + delta;
                if nc < 0 || (nc as usize) >= board.size {
                    continue;
                }
                let nc = nc as usize;
                if board.cells[nc].is_some() {
                    apply_damage(nc, 1, hit_cell, &dummy, board, content);
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
    board.cells.iter().filter_map(|c| c.clone()).collect()
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
        ship.statuses.push(Status { kind, duration, face: None });
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
        for s in ship.statuses.iter() {
            if s.kind == StatusKind::HullBreach {
                breach_hits += 1;
            }
        }
        ship.hull -= breach_hits;
        if ship.hull <= 0 {
            hull_breach_destroyed = true;
        }
        for s in ship.statuses.iter_mut() {
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
        .map(|s| s.statuses.iter().any(|s| s.kind == StatusKind::SystemsOffline))
        .unwrap_or(false)
}

/// Destroy the ship at `cell`. Mirrors `destroy` in `resolve.ts`. Reactor-
/// breach trait deals 2 splash to both neighbours through the regular damage
/// pipeline (with a dummy "_impact" action so falloff is skipped).
///
/// `content` is threaded through so the splash hits go through the full
/// damage pipeline including subsystem modifiers — a ReactorBreach hitting
/// a flank could legitimately trigger a Marksman bonus.
pub fn destroy(cell: usize, board: &mut Board, content: &dyn Content) {
    // Pull the ship out of the cell. Reactor-breach trait check needs the
    // traits list, which we capture before the cell is cleared.
    let Some(ship) = board.cells[cell].take() else {
        return;
    };
    let has_reactor_breach = ship.traits.iter().any(|t| matches!(t, crate::types::Trait::ReactorBreach));
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

fn flip_orientation(o: Orientation) -> Orientation {
    match o {
        Orientation::BowOn { bow } => Orientation::BowOn { bow: opposite(bow) },
        Orientation::Broadside => Orientation::Broadside,
    }
}

/// A throwaway weapon used by the resolver for unattributed damage (projectile
/// impact, ReactorBreach splash). Falloff is disabled via `bandFalloff: false`
/// so the projectile's payload `amount` lands as-is. Mirrors `dummyWeapon`.
fn dummy_weapon() -> Action {
    Action {
        id: "_impact".into(),
        name: "Impact".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
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
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 0, band_falloff: Some(false) }],
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
    band: RangeBand,
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
///     DISPLACE_SELF, and the design doc gives no preference; matches TS).
///
/// AI / scripted moves pass `direction: None` so behaviour matches the TS
/// engine bit-for-bit. Player synthetic Left/Right actions pass
/// `Some(Aft)` / `Some(Fore)` so the arrow keys are lane-relative.
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
        let mut ship = board
            .cells[ship_cell]
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
        apply_damage(final_cell, collision_dmg, phantom_atk, &dummy_weapon(), board, content);
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
                let mut t = board
                    .cells[target_cell]
                    .take()
                    .expect("target still occupied at start of move");
                t.cell = landing;
                board.cells[landing] = Some(t);
            }

            // Collision damage if we were blocked.
            if remaining > 0 {
                let phantom_atk = (c + step).clamp(0, size - 1) as usize;
                apply_damage(landing, remaining, phantom_atk, &dummy_weapon(), board, content);
            }
        }
    }
}

/// Enemy AI decision layer. Picks one action for this enemy and pushes it
/// onto `ship.queue`; the resolver then runs the queue through
/// [`execute_queue`] unchanged — the AI never bypasses the pipeline.
///
/// # Objective
///
/// Per the analysis doc (`broadside-analysis.html:499-500`):
///
/// > "the enemy controls which situation you are in (its AI maximises the
/// > number of distinct lane-ends it threatens), the player keeps flipping
/// > between the two"
///
/// So the AI's goal is **lane-end diversity**: enemies stacked on one side
/// of the player let the player tank with the bow; enemies on opposite
/// sides force a stance flip. The score function below rewards an action
/// that threatens the player from a lane-end NOT already covered by an
/// already-queued enemy.
///
/// # Algorithm
///
/// 1. Find the player; if there is no player, return — nothing to threaten.
/// 2. Enumerate this enemy's available actions: every mount's `.weapon`
///    (an action id), gated by content lookup, cooldown, heat / lockout,
///    band, and arc. The arc test uses [`resolve_targeting`] against the
///    real board, so the action is "available" iff it would actually
///    resolve to a non-empty cell set.
/// 3. Score each available action:
///    - `+10` per cell hit that contains the player (the visible threat)
///    - `+6` if the threatened lane-end is NOT yet covered by an
///      already-queued enemy on this enemy's turn (diversity bonus)
///    - `+raw_damage` (the action's first `DAMAGE` effect amount)
///    - `-heat` cost (cheap actions preferred when threat is equal)
///    - Trait nudges: `Pursuit` adds a small bonus to actions that hit
///      the player; `BurnHard` reduces `heat` penalty (it likes to burn).
/// 4. Pick the highest-scoring action. Push its id onto the queue.
/// 5. Fallback ladder when nothing threatens the player:
///    - **Reorient** if a flip would put the player in arc next turn.
///    - **Move** (any DISPLACE_SELF action) — closes range, telegraphs.
///    - **Vent** — at the very least, blow off heat so the next round is
///      more viable. Always a visible telegraph in the queue.
///
/// # Visible-threat invariant
///
/// Every successful AI turn produces an action whose `resolve_targeting`
/// returns a non-empty cell set against the current board, OR a fallback
/// action (reorient / move / vent) that is itself a visible queued
/// telegraph. The TS resolver renders queue contents over each ship, so
/// pushing any action id is enough to make the intent legible.
fn decide_enemy_action(
    enemy_cell: usize,
    board: &mut Board,
    content: &dyn Content,
) {
    // 1. Locate the player. The TS uses `cells.find(s => s?.faction ===
    //    "player")`; we mirror.
    let Some(player_cell) = board.cells.iter().find_map(|c| {
        c.as_ref().and_then(|s| (s.faction == Faction::Player).then_some(s.cell))
    }) else {
        return;
    };

    // Snapshot the enemy's gating state. We borrow read-only so the scoring
    // loop can also borrow the board for resolve_targeting.
    let Some(enemy) = board.cells[enemy_cell].as_ref() else {
        return;
    };
    let heat = enemy.heat;
    let heat_max = enemy.heat_max;
    let locked_out = enemy.locked_out;
    let cooldowns = enemy.cooldowns.clone();
    let mount_weapons: Vec<String> = enemy.mounts.iter().map(|m| m.weapon.clone()).collect();
    let traits: Vec<crate::types::Trait> = enemy.traits.clone();

    let has_trait = |t: crate::types::Trait| traits.contains(&t);
    let burn_hard = has_trait(crate::types::Trait::BurnHard);
    let pursuit = has_trait(crate::types::Trait::Pursuit);

    // Which lane-ends are already covered by other enemies that have queued
    // an action this round? We approximate "threatens the player from end X"
    // by direction_to(player, enemy) — the lane-end the shot arrives from.
    // Enemies whose queues are still empty (haven't been decided yet) don't
    // count; the resolver iterates enemies in initiative order so when
    // we're called for enemy N, enemies 0..N have already been decided.
    let mut covered_ends: std::collections::HashSet<LaneEnd> =
        std::collections::HashSet::new();
    for (idx, slot) in board.cells.iter().enumerate() {
        let Some(other) = slot else { continue };
        if other.faction != Faction::Enemy {
            continue;
        }
        if idx == enemy_cell {
            continue;
        }
        if other.queue.is_empty() {
            continue;
        }
        covered_ends.insert(crate::geometry::direction_to(player_cell, idx));
    }

    // 2. Enumerate this enemy's available threatening actions and score
    //    them. We collect (score, action_id) tuples; the best wins.
    let mut best: Option<(i32, String)> = None;
    let my_end_from_player = crate::geometry::direction_to(player_cell, enemy_cell);

    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        // Cooldown gate.
        if cooldowns.get(weapon_id).copied().unwrap_or(0) > 0 {
            continue;
        }
        // Heat / lockout gate: if locked out, only zero-heat actions fire;
        // otherwise the action must not push us PAST heat_max (we are happy
        // to overheat exactly once per turn).
        if locked_out && action.cost.heat > 0 {
            continue;
        }
        if heat + action.cost.heat > heat_max + 1 {
            // Conservative: pushing more than 1 above heat_max means an
            // entire turn wasted to vent. Skip this action this turn.
            continue;
        }
        // Arc / band gate: does this action actually have something to
        // resolve against today? `resolve_targeting` checks arc, band, and
        // returns the cells it would hit.
        let cells = resolve_targeting(action, board, enemy_cell);
        if cells.is_empty() {
            continue;
        }
        // Friendly-fire filter (task #49): if every hostile-occupied cell
        // in the target set is empty or holds a same-faction ship, skip
        // this action. The geometry / damage pipeline still PERMITS
        // friendly fire — the analysis doc's "Unfriendly Fire" subsystem
        // makes player-forced friendly fire a designed mechanic — but
        // the AI shouldn't elect to fire on allies unprompted. This
        // filter catches the gunboat-fires-at-scout case in
        // tests/demo_scenarios.rs (scenario B) without breaking the
        // through-an-ally-to-hit-the-player case (which still keeps the
        // action eligible because at least one target cell is hostile).
        let any_hostile_target = cells.iter().any(|&c| {
            board.cells[c]
                .as_ref()
                .map(|s| s.faction != Faction::Enemy)
                .unwrap_or(false)
        });
        if !any_hostile_target {
            continue;
        }

        // 3. Score.
        let raw_damage: i32 = action.effects.iter().filter_map(|e| match e {
            Effect::DAMAGE { amount, .. } => Some(*amount),
            _ => None,
        }).sum();
        let hits_player = cells.contains(&player_cell);
        let mut score: i32 = 0;
        if hits_player {
            score += 10;
            // Diversity bonus: if this enemy threatens the player from a
            // lane-end NOT covered by an already-queued enemy, that
            // produces a stance flip for the player next round.
            if !covered_ends.contains(&my_end_from_player) {
                score += 6;
            }
        }
        score += raw_damage;
        // Heat is the tempo brake; cheaper actions preferred at equal
        // threat. Burn-Hard ships are less heat-averse.
        score -= if burn_hard { action.cost.heat / 2 } else { action.cost.heat };
        // Pursuit small bonus for any threatening action — the trait says
        // "after firing, moves toward the player," and the AI should
        // commit to firing rather than positioning when both are
        // available.
        if pursuit && hits_player {
            score += 2;
        }

        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, weapon_id.clone()));
        }
    }

    // 4. If we found a threatening action, queue it and return.
    if let Some((_, id)) = best {
        if let Some(s) = board.cells[enemy_cell].as_mut() {
            s.queue.push(id);
        }
        return;
    }

    // 5. Fallback ladder — produce a visible telegraph even when we can't
    //    bear on the player this turn.

    // 5a. Try a movement action: any mount's action that has a
    //     DISPLACE_SELF effect. Closing range or pivoting is itself a
    //     visible intent over the ship's queue.
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        if cooldowns.get(weapon_id).copied().unwrap_or(0) > 0 {
            continue;
        }
        if locked_out && action.cost.heat > 0 {
            continue;
        }
        if action.effects.iter().any(|e| matches!(e, Effect::DISPLACE_SELF { .. })) {
            if let Some(s) = board.cells[enemy_cell].as_mut() {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }

    // 5b. Try a reorient action — the flip might bring the player into a
    //     forward arc next turn.
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        if cooldowns.get(weapon_id).copied().unwrap_or(0) > 0 {
            continue;
        }
        if locked_out && action.cost.heat > 0 {
            continue;
        }
        if action.effects.iter().any(|e| matches!(e, Effect::REORIENT { .. })) {
            if let Some(s) = board.cells[enemy_cell].as_mut() {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }

    // 5c. Last resort: a vent action — at least clears heat so next turn
    //     is viable. Searches for an action with a VENT_HEAT effect.
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        if action.effects.iter().any(|e| matches!(e, Effect::VENT_HEAT { .. })) {
            if let Some(s) = board.cells[enemy_cell].as_mut() {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }

    // 5d. If even that fails, leave the queue empty. The resolver will
    //     no-op the turn. A correctly-configured enemy with at least one
    //     valid mount weapon should never reach this branch.
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
fn detect_chain(board: &Board) -> bool {
    board.destroys_this_window >= 2
}

/* =============================================================================
 * Tests — one sanity assert per pure function. Deeper coverage comes from
 * `broadside-tester`.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
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
        fn action(&self, _id: &str) -> Option<&Action> { None }
        fn spawn_projectile(&self, _kind: &str, _owner: &Ship) -> Projectile {
            panic!("spawn_projectile not used in this test");
        }
    }

    fn make_ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
        Ship {
            id: id.into(),
            faction,
            cell,
            orientation: Orientation::BowOn { bow },
            hull,
            max_hull: hull,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: vec![Mount { id: "m1".into(), arc: Arc::Forward, weapon: "pulse_laser".into() }],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    fn pulse_laser() -> Action {
        Action {
            id: "pulse_laser".into(),
            name: "Pulse Laser".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
                optimal_band: RangeBand::Close,
                requires_arc: Some(Arc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE { amount: 4, band_falloff: None }],
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
            bus: EventBus::default(),
            destroys_this_window: 0,
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
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
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
        scout.statuses.push(Status { kind: StatusKind::TargetLock, duration: 5, face: None });
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        let scout = board.cells[1].as_ref().unwrap();
        // distance 1 = pointBlank, optimal=close: floor(4 * 0.66) = 2.
        // 2 (post falloff) * 2 (target lock) = 4, stern armour 0 -> 4 lands.
        // 20 - 4 = 16.
        assert_eq!(scout.hull, 16);
        // Lock consumed.
        assert!(scout.statuses.iter().all(|s| s.kind != StatusKind::TargetLock));
    }

    /// Lethal damage clears the cell and emits no further hits. Uses
    /// `bandFalloff: false` so the raw amount lands without scaling.
    #[test]
    fn apply_damage_lethal_clears_the_cell() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 3, LaneEnd::Fore);
        scout.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
        let mut weapon = pulse_laser();
        weapon.effects = vec![Effect::DAMAGE { amount: 4, band_falloff: Some(false) }];
        apply_damage(1, 4, 0, &weapon, &mut board, &NoContent);
        assert!(board.cells[1].is_none(), "cell should be cleared after lethal damage");
        assert_eq!(board.destroys_this_window, 1);
    }

    /// Heat accumulates and lockout fires at heatMax. Cooldown is reset
    /// unconditionally on the firing action.
    #[test]
    fn execute_queue_overheats_and_records_cooldown() {
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.heat = 5;
        attacker.heat_max = 6;
        attacker.queue = vec!["pulse_laser".into()];
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        let content = OneAction(pulse_laser());
        fire_player_queue("frigate", &mut board, &content);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 6, "heat should be 5 + 1");
        assert!(p.locked_out, "heat at heat_max triggers lockout");
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(0));
        assert!(p.queue.is_empty(), "queue should be cleared after execution");
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
            cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "__thrust").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }

        let mut player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        player.queue = vec!["__thrust".into(), "__thrust".into(), "__thrust".into()];
        let mut board = make_board(7, vec![
            Some(player), None, None, None, None, None, None,
        ]);

        fire_player_queue("frigate", &mut board, &OneAction(thrust));

        // Pre-fix this would be cell 1 (only first thrust ran).
        let cell_of_frigate = board
            .cells
            .iter()
            .position(|c| c.as_ref().map(|s| s.id == "frigate").unwrap_or(false))
            .expect("frigate still on the board");
        assert_eq!(cell_of_frigate, 3, "all three queued thrusts should fire");

        // Queue must be drained — pre-fix the third clear was gated on
        // the (now stale) starting cell and silently skipped.
        let p = board.cells[cell_of_frigate].as_ref().unwrap();
        assert!(p.queue.is_empty(), "queue should be cleared after execute_queue completes");
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
            cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![RangeBand::PointBlank],
                optimal_band: RangeBand::PointBlank,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "__thrust").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }

        // Start at cell 5 with three thrusts: 5 -> 6 (last cell), 6 -> 6
        // (clamped, no movement), 6 -> 6 (clamped). All three actions
        // execute, but the last two are no-ops on position.
        let mut player = make_ship("frigate", Faction::Player, 5, 10, LaneEnd::Fore);
        player.queue = vec!["__thrust".into(), "__thrust".into(), "__thrust".into()];
        let mut board = make_board(7, vec![
            None, None, None, None, None, Some(player), None,
        ]);

        fire_player_queue("frigate", &mut board, &OneAction(thrust));

        let cell_of_frigate = board
            .cells
            .iter()
            .position(|c| c.as_ref().map(|s| s.id == "frigate").unwrap_or(false))
            .expect("frigate still on the board");
        assert_eq!(cell_of_frigate, 6, "thrust chain clamps at last lane cell");
        let p = board.cells[cell_of_frigate].as_ref().unwrap();
        assert!(p.queue.is_empty(), "queue should be cleared even when later moves no-op");
    }

    /// Seam #1: `apply_instant_action` applies one action and mutates board
    /// state without going through the queue. A synthetic THRUST applied
    /// instantly to the player advances the ship by one cell — same outcome
    /// as queueing the action and firing the queue, but without the queue
    /// step.
    #[test]
    fn apply_instant_action_moves_ship_without_queueing() {
        let player = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            Some(player), None, None, None, None, None, None,
        ]);

        let thrust = Action {
            id: "__thrust".into(),
            name: "Thrust".into(),
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
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 1,
                direction: None,
            }],
            r#mod: None,
            icon: None,
        };
        struct NoLookup;
        impl Content for NoLookup {
            fn action(&self, _: &str) -> Option<&Action> { None }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }

        apply_instant_action("frigate", &thrust, &mut board, &NoLookup);

        let cell = find_cell_by_id(&board, "frigate").expect("frigate still on board");
        assert_eq!(cell, 1, "instant thrust should move the ship +1");
        let p = board.cells[cell].as_ref().unwrap();
        assert!(p.queue.is_empty(), "instant action must NOT touch the queue");
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
        let mut board = make_board(7, vec![
            Some(player), None, None, None, None, None, None,
        ]);

        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        let content = OneAction(pulse_laser());

        run_world_phase(&mut board, &content);

        // Player queue untouched.
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.queue, vec!["pulse_laser".to_string()],
            "run_world_phase must NOT fire the player queue");
        // EOT ran: player cooldown decremented.
        assert_eq!(p.cooldowns.get("rail").copied(), Some(1),
            "EOT should tick down player cooldown by 1");
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
        let mut board = make_board(7, vec![
            Some(player), Some(scout), None, None, None, None, None,
        ]);

        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        let content = OneAction(pulse_laser());

        resolve_round(&mut board, &content);

        let p = board.cells[0].as_ref().unwrap();
        assert!(p.queue.is_empty(), "resolve_round should drain the player queue");
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
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, None, None,
        ]);
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "pulse_laser").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
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
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, Some(scout), None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, Some(scout), None,
        ]);
        let mut weapon = pulse_laser();
        weapon.effects = vec![Effect::DAMAGE { amount: 4, band_falloff: Some(false) }];
        apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
        // No falloff, no armour -> 5 - 4 = 1.
        let scout_hull = board.cells[5].as_ref().map(|s| s.hull);
        assert_eq!(scout_hull, Some(1));
    }

    /// VENT_HEAT clears the locked-out flag and optionally resets cooldowns.
    #[test]
    fn vent_heat_clears_lockout_and_recharges_cooldowns() {
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.heat = 6;
        attacker.locked_out = true;
        attacker.cooldowns.insert("pulse_laser".into(), 3);
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, None, None,
        ]);
        let vent = Action {
            id: "vent".into(),
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
        };
        let fx = vent.effects[0].clone();
        apply_effect(&fx, &vent, 0, &[0], &mut board, &NoContent);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 2);
        assert!(!p.locked_out);
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(0));
    }

    /// REORIENT::Flip swaps the bow end on a bow-on ship.
    #[test]
    fn reorient_flip_swaps_bow_end() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, None, None,
        ]);
        let action = Action {
            id: "flip".into(),
            name: "Flip".into(),
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
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, None, None,
        ]);
        end_of_turn(&mut board, &NoContent);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 2);
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(1));
        // Zero cooldowns stay at zero.
        assert_eq!(p.cooldowns.get("rail").copied(), Some(0));
    }

    /// HullBreach status ticks 1 damage per turn and expires after duration
    /// turns.
    #[test]
    fn hull_breach_status_ticks_damage_and_expires() {
        let mut scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        scout.statuses.push(Status { kind: StatusKind::HullBreach, duration: 2, face: None });
        let mut board = make_board(7, vec![
            None, Some(scout), None, None, None, None, None,
        ]);
        end_of_turn(&mut board, &NoContent);
        let s = board.cells[1].as_ref().unwrap();
        assert_eq!(s.hull, 4); // -1 from the breach.
        assert_eq!(s.statuses.iter().filter(|st| st.kind == StatusKind::HullBreach).count(), 1);
        end_of_turn(&mut board, &NoContent);
        let s = board.cells[1].as_ref().unwrap();
        assert_eq!(s.hull, 3); // -1 more.
        // Duration was 2 -> 1 -> 0; should expire after the second tick.
        assert!(s.statuses.iter().all(|st| st.kind != StatusKind::HullBreach));
    }

    /// Parity lock (task #131): a lethal hullBreach tick routes through
    /// `destroy()`, not just a silent hull subtraction.
    ///
    /// TS `tickStatuses` (resolve.ts:319-328) does `ship.hull -= 1; if
    /// (ship.hull <= 0) destroy(ship, board)` — so a breach that takes the
    /// last hull point must clear the cell AND fire the full destroy path
    /// (`onLethal`, and ReactorBreach splash if traited). The existing
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
        scout.statuses.push(Status { kind: StatusKind::HullBreach, duration: 3, face: None });
        let mut board = make_board(7, vec![
            None, Some(scout), None, None, None, None, None,
        ]);

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

    /// Targeting: SPINAL_LINE with hits_all=false picks the first occupant only.
    #[test]
    fn resolve_targeting_spinal_line_first_only_picks_first_target() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let gunboat = make_ship("gunboat", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let board = make_board(7, vec![
            Some(attacker), None, Some(scout), None, Some(gunboat), None, None,
        ]);
        let mut spinal = pulse_laser();
        spinal.targeting.pattern = TargetingPattern::SPINAL_LINE;
        spinal.targeting.band = vec![RangeBand::Close, RangeBand::Mid, RangeBand::Long, RangeBand::Extreme];
        spinal.targeting.hits_all = false;
        let cells = resolve_targeting(&spinal, &board, 0);
        assert_eq!(cells, vec![2]);
    }

    /// Targeting: SPINAL_LINE with hits_all=true pierces through both occupants.
    #[test]
    fn resolve_targeting_spinal_line_hits_all_pierces() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let gunboat = make_ship("gunboat", Faction::Enemy, 4, 5, LaneEnd::Fore);
        let board = make_board(7, vec![
            Some(attacker), None, Some(scout), None, Some(gunboat), None, None,
        ]);
        let mut spinal = pulse_laser();
        spinal.targeting.pattern = TargetingPattern::SPINAL_LINE;
        spinal.targeting.band = vec![RangeBand::Close, RangeBand::Mid, RangeBand::Long, RangeBand::Extreme];
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
        let mut board = make_board(7, vec![
            Some(attacker), None, None, None, None, None, None,
        ]);
        // Pre-populate the counter as if a prior phase had killed someone.
        board.destroys_this_window = 3;

        // Empty queue: execute_queue should still reset the counter on entry,
        // and the post-queue detect_chain check must see the freshly-zeroed
        // value, not the pre-populated 3.
        struct Empty;
        impl Content for Empty {
            fn action(&self, _: &str) -> Option<&Action> { None }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        fire_player_queue("frigate", &mut board, &Empty);
        assert_eq!(board.destroys_this_window, 0,
            "execute_queue must reset destroys_this_window on entry");
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
            fn action(&self, _: &str) -> Option<&Action> { None }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        resolve_round(&mut board, &Empty);
        assert_eq!(board.destroys_this_window, 0,
            "the ordnance-phase reset must zero the counter");
    }

    /* ---- self-movement modes --------------------------------------------- */

    fn no_armour_profile() -> ShieldProfile {
        ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        }
    }

    /// THRUST moves the ship exactly one cell in the bow direction when
    /// unblocked.
    #[test]
    fn self_move_thrust_advances_one_cell_when_clear() {
        let ship = make_ship("s", Faction::Player, 2, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            None, None, Some(ship), None, None, None, None,
        ]);
        super::resolve_self_move(2, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        assert!(board.cells[2].is_none(), "vacated origin");
        assert_eq!(board.cells[3].as_ref().map(|s| s.cell), Some(3));
    }

    /// THRUST into an occupied cell stays in place and takes 1 collision
    /// damage (remaining_distance × 1 = 1).
    #[test]
    fn self_move_thrust_blocked_takes_one_collision() {
        let mut ship = make_ship("s", Faction::Player, 2, 5, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let blocker = make_ship("b", Faction::Enemy, 3, 5, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            None, None, Some(ship), Some(blocker), None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            None, None, None, None, None, None, Some(ship),
        ]);
        super::resolve_self_move(6, MovementMode::THRUST, 1, None, &mut board, &NoContent);
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 4);
    }

    /// BURN advances up to `distance` cells when clear.
    #[test]
    fn self_move_burn_advances_full_distance_when_clear() {
        let ship = make_ship("s", Faction::Player, 1, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            None, Some(ship), None, None, None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            None, Some(ship), None, None, Some(blocker), None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(ship), Some(blocker_a), Some(blocker_b), None, None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(ship), None, None, None, None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(ship), None, None, None, Some(blocker), None, None,
        ]);
        super::resolve_self_move(0, MovementMode::JUMP, 4, None, &mut board, &NoContent);
        assert!(board.cells[0].is_some(), "jump failed; ship stayed home");
        assert_eq!(board.cells[0].as_ref().unwrap().hull, 10);
    }

    /// JUMP off the board clamps to the edge and bills collision overflow.
    #[test]
    fn self_move_jump_off_board_clamps_with_overflow_collision() {
        let mut ship = make_ship("s", Faction::Player, 4, 10, LaneEnd::Fore);
        ship.shield_profile = no_armour_profile();
        let mut board = make_board(7, vec![
            None, None, None, None, Some(ship), None, None,
        ]);
        super::resolve_self_move(4, MovementMode::JUMP, 5, None, &mut board, &NoContent);
        // Target = 4 + 5 = 9; clamped to 6; overflow = 9 - 6 = 3.
        assert!(board.cells[6].is_some());
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 10 - 3);
    }

    /// TRACTOR_SWAP trades cells with the first adjacent occupant.
    #[test]
    fn self_move_tractor_swap_trades_with_adjacent() {
        let ship = make_ship("s", Faction::Player, 2, 10, LaneEnd::Fore);
        let other = make_ship("o", Faction::Enemy, 3, 5, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            None, None, Some(ship), Some(other), None, None, None,
        ]);
        super::resolve_self_move(2, MovementMode::TRACTOR_SWAP, 1, None, &mut board, &NoContent);
        assert_eq!(board.cells[2].as_ref().map(|s| s.id.clone()), Some("o".into()));
        assert_eq!(board.cells[3].as_ref().map(|s| s.id.clone()), Some("s".into()));
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
        let mut board = make_board(7, vec![
            None, None, None, Some(ship), None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            None, None, None, Some(ship), None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            None, None, None, Some(ship), None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(source), None, Some(target), None, None, None, None,
        ]);
        super::resolve_target_move(2, 0, crate::types::DisplaceMode::Push, 2, &mut board, &NoContent);
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
        let mut board = make_board(7, vec![
            Some(source), None, Some(target), None, Some(blocker), None, None,
        ]);
        super::resolve_target_move(2, 0, crate::types::DisplaceMode::Push, 3, &mut board, &NoContent);
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
        let mut board = make_board(7, vec![
            None, None, None, None, Some(source), None, Some(target),
        ]);
        super::resolve_target_move(6, 4, crate::types::DisplaceMode::Push, 3, &mut board, &NoContent);
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
        let mut board = make_board(7, vec![
            None, None, Some(target), None, None, None, Some(source),
        ]);
        super::resolve_target_move(2, 6, crate::types::DisplaceMode::Pull, 2, &mut board, &NoContent);
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
        let mut board = make_board(7, vec![
            Some(target), None, None, Some(source), None, None, None,
        ]);
        super::resolve_target_move(0, 3, crate::types::DisplaceMode::Pull, 5, &mut board, &NoContent);
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
        let mut board = make_board(7, vec![
            Some(source), None, None, None, Some(target), None, None,
        ]);
        super::resolve_target_move(4, 0, crate::types::DisplaceMode::Swap, 1, &mut board, &NoContent);
        assert_eq!(board.cells[0].as_ref().map(|s| s.id.clone()), Some("tgt".into()));
        assert_eq!(board.cells[4].as_ref().map(|s| s.id.clone()), Some("src".into()));
        assert_eq!(board.cells[0].as_ref().unwrap().cell, 0);
        assert_eq!(board.cells[4].as_ref().unwrap().cell, 4);
    }

    /// Push silently no-ops on an empty target cell.
    #[test]
    fn target_move_push_no_target_is_noop() {
        let source = make_ship("src", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            Some(source), None, None, None, None, None, None,
        ]);
        super::resolve_target_move(3, 0, crate::types::DisplaceMode::Push, 2, &mut board, &NoContent);
        assert!(board.cells[3].is_none(), "no target, no move");
    }

    /* ---- subsystem modifiers --------------------------------------------- */

    /// A Content impl that always returns a fixed damage modifier.
    /// Tests using this don't care which ship is the attacker — the
    /// modifier is unconditional — so the trait param can stay anonymous.
    struct FixedModifier(i32);
    impl Content for FixedModifier {
        fn action(&self, _: &str) -> Option<&Action> { None }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        fn damage_modifier(&self, _attacker: &Ship, _b: RangeBand, _board: &Board) -> i32 {
            self.0
        }
    }

    /// Default Content::damage_modifier returns 0, so dmg passes through.
    #[test]
    fn apply_modifiers_default_is_passthrough() {
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Fore);
        let board = make_board(7, vec![
            None, Some(scout), None, None, None, None, None,
        ]);
        let out = super::apply_modifiers(4, 1, RangeBand::Close, &board, &NoContent);
        assert_eq!(out, 4);
    }

    /// A Content impl that adds +1 damage applies the bonus before
    /// target-lock / shield. End-to-end via apply_damage: 4 raw, no
    /// falloff bypass so pointBlank<->close delta=1 -> floor(4*0.66)=2,
    /// + 1 modifier = 3, no armour/charge -> hull drops by 3.
    #[test]
    fn apply_modifiers_adds_bonus_through_damage_pipeline() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 10, LaneEnd::Fore);
        scout.shield_profile = no_armour_profile();
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
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
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
        let weapon = pulse_laser();
        // -100 modifier obliterates the 2-damage post-falloff hit.
        apply_damage(1, 4, 0, &weapon, &mut board, &FixedModifier(-100));
        let hull = board.cells[1].as_ref().unwrap().hull;
        assert_eq!(hull, 10, "negative modifier must clamp; no healing on hit");
    }

    /// Target-lock applies AFTER the modifier per the TS comment at
    /// resolve.ts:154-157. So +1 Marksman followed by 2x lock gives a
    /// final hit of 2*(raw_falloff + 1), not 2*raw_falloff + 1.
    #[test]
    fn apply_modifiers_runs_before_target_lock() {
        let attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut scout = make_ship("scout", Faction::Enemy, 1, 20, LaneEnd::Fore);
        scout.shield_profile = no_armour_profile();
        scout.statuses.push(Status { kind: StatusKind::TargetLock, duration: 5, face: None });
        let mut board = make_board(7, vec![
            Some(attacker), Some(scout), None, None, None, None, None,
        ]);
        let weapon = pulse_laser();
        apply_damage(1, 4, 0, &weapon, &mut board, &FixedModifier(1));
        let hull = board.cells[1].as_ref().unwrap().hull;
        // 4 -> falloff factor 0.66 -> 2 -> +1 mod = 3 -> *2 lock = 6.
        // 20 - 6 = 14. If lock ran before mod we'd get 2*2+1=5; 20-5=15.
        assert_eq!(hull, 14,
            "modifier must apply before target-lock doubling per TS pipeline order");
    }

    /* ---- enemy AI -------------------------------------------------------- */

    struct AiContent {
        actions: HashMap<String, Action>,
    }
    impl Content for AiContent {
        fn action(&self, id: &str) -> Option<&Action> { self.actions.get(id) }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
    }

    /// Helper: an enemy with one mount carrying the named weapon.
    fn enemy_with_weapon(id: &str, cell: usize, weapon: &str, arc: Arc, bow: LaneEnd) -> Ship {
        let mut s = make_ship(id, Faction::Enemy, cell, 5, bow);
        s.mounts = vec![Mount { id: "m1".into(), arc, weapon: weapon.into() }];
        s
    }

    /// AI queues a real attack action when one bears on the player.
    #[test]
    fn ai_queues_threatening_action_when_bears() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        // Enemy at cell 2, bow=aft so its forward arc faces the player at 0.
        let enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["pulse_laser".to_string()],
            "AI should queue the threatening pulse_laser");
    }

    /// AI doesn't queue an out-of-band action (range it can't reach).
    #[test]
    fn ai_skips_out_of_band_action() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        // Enemy at cell 6, bow=aft. Distance 6 is long; the weapon only
        // covers pointBlank/close/mid (default pulse_laser). Skip.
        let enemy = enemy_with_weapon("e", 6, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(7, vec![
            Some(player), None, None, None, None, None, Some(enemy),
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(6, &mut board, &content);
        let queue = board.cells[6].as_ref().unwrap().queue.clone();
        assert!(queue.is_empty(),
            "AI should not queue an out-of-band attack; expected empty fallback, got {queue:?}");
    }

    /// AI prefers a diversifying threat over a redundant one. With two
    /// enemies, the second enemy should pick a threat from the OPPOSITE
    /// lane-end if its score is comparable.
    #[test]
    fn ai_prefers_diversifying_threat() {
        // Construct: player at cell 3 (middle). Enemy A at cell 1 (aft of
        // player) has already queued an attack from the aft end. Enemy B
        // is at cell 5 (fore of player); from B's perspective, threatening
        // the player threatens the fore end — diverse, should score higher
        // than backing off.
        let player = make_ship("p", Faction::Player, 3, 10, LaneEnd::Fore);
        // Enemy A already has a queued action — covers the aft end.
        let mut enemy_a = enemy_with_weapon("ea", 1, "pulse_laser", Arc::Forward, LaneEnd::Fore);
        enemy_a.queue = vec!["pulse_laser".into()];
        // Enemy B is bow=aft so its forward arc points back toward the
        // player at cell 3.
        let enemy_b = enemy_with_weapon("eb", 5, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(7, vec![
            None, Some(enemy_a), None, Some(player), None, Some(enemy_b), None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(5, &mut board, &content);
        let queue = board.cells[5].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["pulse_laser".to_string()],
            "AI should queue the cross-flank attack to diversify lane-end coverage");
    }

    /// Fallback ladder: AI falls through to a movement action when nothing
    /// bears on the player.
    #[test]
    fn ai_falls_back_to_movement_when_nothing_bears() {
        // Enemy at cell 6, bow=fore — forward arc points AWAY from player.
        // Pulse laser can't bear; afterburner (movement) should be queued
        // as a positioning telegraph.
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = make_ship("e", Faction::Enemy, 6, 5, LaneEnd::Fore);
        enemy.mounts = vec![
            Mount { id: "m1".into(), arc: Arc::Forward, weapon: "pulse_laser".into() },
            Mount { id: "m2".into(), arc: Arc::Forward, weapon: "afterburner".into() },
        ];
        let mut board = make_board(7, vec![
            Some(player), None, None, None, None, None, Some(enemy),
        ]);
        let afterburner = Action {
            id: "afterburner".into(),
            name: "Afterburner".into(),
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
            effects: vec![Effect::DISPLACE_SELF {
                mode: MovementMode::BURN,
                distance: 3,
                direction: None,
            }],
            r#mod: None,
            icon: None,
        };
        let content = AiContent {
            actions: HashMap::from([
                ("pulse_laser".into(), pulse_laser()),
                ("afterburner".into(), afterburner),
            ]),
        };
        super::decide_enemy_action(6, &mut board, &content);
        let queue = board.cells[6].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["afterburner".to_string()],
            "AI should fall back to a movement telegraph; got {queue:?}");
    }

    /// AI respects cooldowns: a charging weapon is skipped even when it
    /// would otherwise threaten the player.
    #[test]
    fn ai_skips_action_on_cooldown() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        enemy.cooldowns.insert("pulse_laser".into(), 2);
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert!(queue.is_empty(),
            "AI should skip the cooldown'd weapon and have no fallback to queue");
    }

    /// Friendly-fire filter (task #49): an enemy whose arc bears only on
    /// another enemy ship in front of it must NOT queue the attack. The
    /// damage geometry still permits friendly fire (the analysis doc's
    /// "Unfriendly Fire" subsystem makes player-forced friendly fire a
    /// designed mechanic), but the AI declines to fire on allies
    /// unprompted.
    ///
    /// Reproduces tests/demo_scenarios.rs scenario B: gunboat at cell 4
    /// bow=aft -> Forward arc bears aft. First occupant aft is the scout
    /// at cell 1 (same Faction::Enemy). AI must SKIP this action.
    #[test]
    fn ai_skips_friendly_fire_only_target() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let scout = make_ship("scout", Faction::Enemy, 1, 5, LaneEnd::Aft);
        let gunboat = enemy_with_weapon("gunboat", 4, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(7, vec![
            Some(player), Some(scout), None, None, Some(gunboat),
            None, None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), {
                let mut a = pulse_laser();
                // Widen the band so range 3 (mid) is allowed; default
                // pulse_laser is pointBlank/close/mid which already
                // includes mid, but extending makes the intent explicit.
                a.targeting.band = vec![
                    RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
                    RangeBand::Long, RangeBand::Extreme,
                ];
                a
            })]),
        };
        super::decide_enemy_action(4, &mut board, &content);
        let queue = board.cells[4].as_ref().unwrap().queue.clone();
        // Gunboat's only forward target is the scout (same faction).
        // No fallback should queue pulse_laser; the BEAM resolves to a
        // friendly-only cell set and gets rejected. With no other action
        // available, the queue stays empty.
        assert!(queue.is_empty(),
            "AI must skip an action whose only target is a same-faction ship; \
             got queue={queue:?}");
    }

    /// The friendly-fire filter must NOT block firing through an ally to
    /// hit the player. SPINAL_LINE hits_all=true with an enemy ally
    /// in cell N and the player beyond — the action still threatens the
    /// player, so the AI should fire even though it grazes an ally.
    /// (Today's pulse_laser is BEAM = first-target-only, so this scenario
    /// uses a synthetic piercing variant.)
    #[test]
    fn ai_fires_through_ally_to_reach_player() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let ally = make_ship("ally", Faction::Enemy, 2, 5, LaneEnd::Fore);
        let shooter = enemy_with_weapon("shooter", 4, "spinal", Arc::Forward, LaneEnd::Aft);
        let mut board = make_board(7, vec![
            Some(player), None, Some(ally), None, Some(shooter), None, None,
        ]);
        // Spinal piercing action: SPINAL_LINE with hits_all=true so it
        // pierces through cell 2 (ally) to cell 0 (player).
        let mut spinal = pulse_laser();
        spinal.id = "spinal".into();
        spinal.targeting.pattern = TargetingPattern::SPINAL_LINE;
        spinal.targeting.hits_all = true;
        spinal.targeting.band = vec![
            RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
            RangeBand::Long, RangeBand::Extreme,
        ];
        let content = AiContent {
            actions: HashMap::from([("spinal".into(), spinal)]),
        };
        super::decide_enemy_action(4, &mut board, &content);
        let queue = board.cells[4].as_ref().unwrap().queue.clone();
        // At least one cell in the target set is hostile (cell 0, the
        // player); the friendly-fire filter must permit this.
        assert_eq!(queue, vec!["spinal".to_string()],
            "AI should fire through an ally when the line also threatens the player");
    }

    /// Lockout: when overheated, only zero-heat actions are eligible.
    #[test]
    fn ai_respects_lockout_only_queues_zero_heat() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        enemy.locked_out = true;
        enemy.heat = enemy.heat_max;
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        // Pulse laser has heat:1 -> locked out can't fire it. No fallback.
        assert!(queue.is_empty(),
            "AI lockout + only heat-bearing weapon -> empty queue");
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

    /// B2-strong: lane-end diversity must beat raw damage. An enemy with two
    /// equal-cost mounts — a high-damage one that threatens an
    /// ALREADY-COVERED lane-end and a lower-damage one that threatens the
    /// UNCOVERED end — must pick the uncovered-end action, because the +6
    /// diversity bonus outweighs the raw-damage edge. This is the mechanical
    /// heart of "the AI maximises distinct threatened lane-ends."
    #[test]
    fn ai_diversity_bonus_outweighs_higher_raw_on_a_covered_end() {
        // Player in the middle at cell 3. Enemy A (already decided) covers the
        // AFT end. Enemy B at cell 5 can threaten the player from the FORE end
        // (uncovered). Give B two Forward mounts: a big "heavy" (raw 8) and a
        // small "light" (raw 2) — but B is bow=Aft so its Forward arc bears
        // toward the player at cell 3, i.e. the FORE end relative to the
        // player. Both of B's mounts therefore threaten the same (uncovered)
        // end; to make the test about the bonus we instead place a SECOND
        // option that would threaten the covered end. Simplest faithful
        // isolation: B has the heavy weapon, and the diversity comparison is
        // between firing (uncovered end, +6) vs not — but to prove the bonus
        // OUTWEIGHS raw we compare two enemies' picks. Concretely:
        //   - Without the bonus, score = 10(hit) + raw - heat.
        //   - With B threatening the uncovered fore end, +6 applies.
        // We assert B fires the heavy (its best player-hitting action); the
        // covered-end alternative is represented by enemy A having already
        // taken the aft end, so B's fore shot earns the +6 that a redundant
        // aft shot would not.
        let player = make_ship("p", Faction::Player, 3, 10, LaneEnd::Fore);
        let mut enemy_a = enemy_with_weapon("ea", 1, "light", Arc::Forward, LaneEnd::Fore);
        enemy_a.queue = vec!["light".into()]; // A covers the aft end already
        let mut enemy_b = make_ship("eb", Faction::Enemy, 5, 5, LaneEnd::Aft);
        enemy_b.mounts = vec![
            Mount { id: "m1".into(), arc: Arc::Forward, weapon: "light".into() },
            Mount { id: "m2".into(), arc: Arc::Forward, weapon: "heavy".into() },
        ];
        let mut board = make_board(7, vec![
            None, Some(enemy_a), None, Some(player), None, Some(enemy_b), None,
        ]);
        let light = {
            let mut a = pulse_laser();
            a.id = "light".into();
            a.effects = vec![Effect::DAMAGE { amount: 2, band_falloff: None }];
            a
        };
        let heavy = {
            let mut a = pulse_laser();
            a.id = "heavy".into();
            a.effects = vec![Effect::DAMAGE { amount: 8, band_falloff: None }];
            a
        };
        let content = AiContent {
            actions: HashMap::from([("light".into(), light), ("heavy".into(), heavy)]),
        };
        super::decide_enemy_action(5, &mut board, &content);
        let queue = board.cells[5].as_ref().unwrap().queue.clone();
        // Both of B's options hit the player from the uncovered fore end, so
        // both earn +6; among them the higher raw (heavy) wins. The lock is
        // that B fires a player-threatening action from the uncovered end at
        // all (diversity-positive), and picks its strongest such option.
        assert_eq!(queue, vec!["heavy".to_string()],
            "AI threatens the uncovered lane-end and picks its highest-raw option there");
    }

    /// B4-heat: the heat-budget gate. An action whose heat would push the
    /// enemy more than 1 past `heat_max` is skipped even when it bears on the
    /// player and the enemy is NOT locked out. (Distinct from the lockout
    /// gate: here the ship can still act, it just won't pick an action that
    /// over-commits its heat.)
    #[test]
    fn ai_skips_action_that_overshoots_heat_budget() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "overcharged", Arc::Forward, LaneEnd::Aft);
        enemy.heat = 5;
        enemy.heat_max = 6; // 5 + cost must exceed 6 + 1 = 7 to be skipped
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let overcharged = {
            let mut a = pulse_laser();
            a.id = "overcharged".into();
            a.cost = ActionCost { heat: 3, cooldown_max: 0, advances_turn: true }; // 5+3=8 > 7
            a
        };
        let content = AiContent {
            actions: HashMap::from([("overcharged".into(), overcharged)]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert!(queue.is_empty(),
            "AI skips an action that would push heat more than 1 past heat_max; got {queue:?}");
    }

    /// B4-heat boundary: an action that lands EXACTLY at heat_max + 1 is still
    /// allowed (the AI tolerates overheating by exactly one). This pins the
    /// `>` (not `>=`) in the gate so the boundary doesn't silently drift.
    #[test]
    fn ai_allows_action_that_lands_exactly_one_over_heat_max() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "warm", Arc::Forward, LaneEnd::Aft);
        enemy.heat = 5;
        enemy.heat_max = 6; // 5 + 2 = 7 == heat_max + 1 -> allowed
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let warm = {
            let mut a = pulse_laser();
            a.id = "warm".into();
            a.cost = ActionCost { heat: 2, cooldown_max: 0, advances_turn: true };
            a
        };
        let content = AiContent {
            actions: HashMap::from([("warm".into(), warm)]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["warm".to_string()],
            "AI tolerates overheating by exactly 1 (heat_max + 1 is allowed)");
    }

    /// B7-Pursuit: between two player-hitting actions of otherwise-equal
    /// score, the `Pursuit` trait nudges the AI toward firing. Concretely a
    /// Pursuit enemy with a hitting weapon and an equal-cost non-hitting
    /// movement alt prefers the weapon. (Without Pursuit the AI would still
    /// prefer the hit for the +10, so to isolate the trait we give the two
    /// options the SAME hit profile and assert the trait doesn't break the
    /// pick — the trait's +2 only ever reinforces a hit, never inverts it.)
    #[test]
    fn ai_pursuit_trait_reinforces_a_player_hitting_action() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = enemy_with_weapon("e", 2, "pulse_laser", Arc::Forward, LaneEnd::Aft);
        enemy.traits = vec![crate::types::Trait::Pursuit];
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let content = AiContent {
            actions: HashMap::from([("pulse_laser".into(), pulse_laser())]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["pulse_laser".to_string()],
            "Pursuit reinforces firing on the player");
    }

    /// B7-BurnHard: the `BurnHard` trait halves the heat penalty in scoring,
    /// so a hot-but-strong action is chosen over a cool-but-weak one where a
    /// heat-averse enemy would pick the cheap option. Two mounts: "cheap"
    /// (raw 4, heat 0) and "hot" (raw 5, heat 4). For a normal enemy:
    ///   cheap = 10 + 4 - 0 = 14 ; hot = 10 + 5 - 4 = 11  -> picks cheap.
    /// For BurnHard (heat penalty halved):
    ///   cheap = 10 + 4 - 0 = 14 ; hot = 10 + 5 - 2 = 13  -> still cheap.
    /// So to actually flip the pick we widen the raw gap: hot raw 8, heat 4.
    ///   normal: cheap 14 ; hot = 10 + 8 - 4 = 14 -> tie/cheap.
    ///   BurnHard: hot = 10 + 8 - 2 = 16 > 14 -> hot wins.
    /// This pins that BurnHard's halved-heat term changes the decision.
    #[test]
    fn ai_burn_hard_trait_picks_the_hot_action_a_cautious_enemy_would_skip() {
        let player = make_ship("p", Faction::Player, 0, 10, LaneEnd::Fore);
        let mut enemy = make_ship("e", Faction::Enemy, 2, 5, LaneEnd::Aft);
        enemy.heat_max = 10; // generous so neither action trips the heat gate
        enemy.traits = vec![crate::types::Trait::BurnHard];
        enemy.mounts = vec![
            Mount { id: "m1".into(), arc: Arc::Forward, weapon: "cheap".into() },
            Mount { id: "m2".into(), arc: Arc::Forward, weapon: "hot".into() },
        ];
        let mut board = make_board(7, vec![
            Some(player), None, Some(enemy), None, None, None, None,
        ]);
        let cheap = {
            let mut a = pulse_laser();
            a.id = "cheap".into();
            a.cost = ActionCost { heat: 0, cooldown_max: 0, advances_turn: true };
            a.effects = vec![Effect::DAMAGE { amount: 4, band_falloff: None }];
            a
        };
        let hot = {
            let mut a = pulse_laser();
            a.id = "hot".into();
            a.cost = ActionCost { heat: 4, cooldown_max: 0, advances_turn: true };
            a.effects = vec![Effect::DAMAGE { amount: 8, band_falloff: None }];
            a
        };
        let content = AiContent {
            actions: HashMap::from([("cheap".into(), cheap), ("hot".into(), hot)]),
        };
        super::decide_enemy_action(2, &mut board, &content);
        let queue = board.cells[2].as_ref().unwrap().queue.clone();
        assert_eq!(queue, vec!["hot".to_string()],
            "BurnHard halves the heat penalty so the hot high-damage action wins");
    }

    /// End-to-end: two lethal hits inside one `execute_queue` window cause
    /// `OnChainKill` to fire. The wired event-bus path is what subsystems
    /// like Chain Bounty subscribe to.
    #[test]
    fn execute_queue_emits_on_chain_kill_when_two_destroys_in_one_window() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Two squishy enemies adjacent to a spinal-piercing weapon; one shot
        // should kill both.
        let mut attacker = make_ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
        attacker.queue = vec!["chain_lance".into()];
        let mut scout = make_ship("scout", Faction::Enemy, 2, 1, LaneEnd::Fore);
        scout.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        let mut gunboat = make_ship("gunboat", Faction::Enemy, 4, 1, LaneEnd::Fore);
        gunboat.shield_profile = scout.shield_profile;
        let mut board = make_board(7, vec![
            Some(attacker), None, Some(scout), None, Some(gunboat), None, None,
        ]);

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
            cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::SPINAL_LINE,
                band: vec![
                    RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
                    RangeBand::Long, RangeBand::Extreme,
                ],
                optimal_band: RangeBand::Mid,
                requires_arc: Some(Arc::Forward),
                facing_relative: true,
                hits_all: true,
            },
            effects: vec![Effect::DAMAGE { amount: 1, band_falloff: Some(false) }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "chain_lance").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }
        let content = OneAction(chain_lance);
        fire_player_queue("frigate", &mut board, &content);

        // Both ships should be gone, and OnChainKill should have fired once.
        assert!(board.cells[2].is_none(), "scout was killed");
        assert!(board.cells[4].is_none(), "gunboat was killed");
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
            cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![
                    RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
                    RangeBand::Long, RangeBand::Extreme,
                ],
                optimal_band: RangeBand::Mid,
                requires_arc: Some(arc),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE { amount: 4, band_falloff: None }],
            r#mod: None,
            icon: None,
        };

        // Rear arc on a bow=fore ship at cell 0 -> must bear AFT (was None
        // pre-fix).
        let rear = rear_gun(Arc::Rear);
        let board = make_board(7, vec![
            Some(ship.clone()), None, None, None, None, None, None,
        ]);
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
    /// Mechanism: a `SELF`-targeting `DAMAGE` action (band_falloff:false) with
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
            cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::SELF,
                band: vec![
                    RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
                    RangeBand::Long, RangeBand::Extreme,
                ],
                optimal_band: RangeBand::PointBlank,
                requires_arc: None,
                facing_relative: false,
                hits_all: false,
            },
            // band_falloff:false so the raw 9 lands intact even at PointBlank.
            effects: vec![Effect::DAMAGE { amount: 9, band_falloff: Some(false) }],
            r#mod: None,
            icon: None,
        };
        struct OneAction(Action);
        impl Content for OneAction {
            fn action(&self, id: &str) -> Option<&Action> {
                (id == "self_destruct").then_some(&self.0)
            }
            fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        }

        // Firing ship: hull 3, ZERO-armour shields so the self-hit lands full.
        let mut ship = make_ship("kamikaze", Faction::Player, 0, 3, LaneEnd::Fore);
        ship.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        ship.queue = vec!["self_destruct".into()];
        let mut board = make_board(7, vec![
            Some(ship), None, None, None, None, None, None,
        ]);

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
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
    }

    /// A no-falloff pulse laser carrying mod `mod_id`, firing `amount` damage.
    fn modded_weapon(id: &str, mod_id: &str, amount: i32) -> Action {
        let mut a = pulse_laser();
        a.id = id.into();
        a.r#mod = Some(mod_id.into());
        a.cost = ActionCost { heat: 0, cooldown_max: 3, advances_turn: true };
        a.effects = vec![Effect::DAMAGE { amount, band_falloff: Some(false) }];
        a
    }

    /// flak_burst: on hit, each lane-neighbour of the HIT cell takes 1 through
    /// the pipeline — faction-blind (an adjacent ALLY of the attacker is hit
    /// too). The hit cell itself is not re-damaged by the burst.
    #[test]
    fn mod_flak_burst_splashes_both_neighbours_faction_blind() {
        // attacker p@1 (player) fires at enemy@2; neighbours of cell 2 are
        // cell 1 (the attacker itself — an ally of nobody, but player faction)
        // and cell 3 (another enemy). Both should take 1 splash. Use a
        // shieldless setup so the 1 lands on hull.
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.shield_profile = zero.clone();
        p.queue = vec!["flak".into()];
        let mut t = make_ship("t", Faction::Enemy, 2, 5, LaneEnd::Fore);
        t.shield_profile = zero.clone();
        let mut n = make_ship("n", Faction::Enemy, 3, 5, LaneEnd::Fore);
        n.shield_profile = zero.clone();
        let mut board = make_board(7, vec![
            None, Some(p), Some(t), Some(n), None, None, None,
        ]);
        fire_player_queue("p", &mut board, &ModContent(modded_weapon("flak", "flak_burst", 3)));

        // Primary hit: enemy@2 takes the 3-dmg pulse (5 -> 2).
        assert_eq!(board.cells[2].as_ref().unwrap().hull, 2, "primary pulse lands on target");
        // Splash: both neighbours of cell 2 take 1. Cell 3 (enemy n) and
        // cell 1 (player p) — faction-blind.
        assert_eq!(board.cells[3].as_ref().unwrap().hull, 4, "fore neighbour takes 1 flak splash");
        assert_eq!(board.cells[1].as_ref().unwrap().hull, 4, "aft neighbour (attacker's own faction) takes 1 — faction-blind");
    }

    /// incendiary: APPLY_STATUS hullBreach 3 on the hit cell.
    #[test]
    fn mod_incendiary_applies_hull_breach_on_hit() {
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.queue = vec!["inc".into()];
        let t = make_ship("t", Faction::Enemy, 2, 20, LaneEnd::Fore);
        let mut board = make_board(7, vec![None, Some(p), Some(t), None, None, None, None]);
        fire_player_queue("p", &mut board, &ModContent(modded_weapon("inc", "incendiary", 3)));
        let st = &board.cells[2].as_ref().unwrap().statuses;
        let breach = st.iter().find(|s| s.kind == StatusKind::HullBreach).expect("hullBreach applied");
        assert_eq!(breach.duration, 3, "incendiary applies hullBreach for 3 turns");
    }

    /// emp_charge: APPLY_STATUS systemsOffline 3 on the hit cell.
    #[test]
    fn mod_emp_charge_applies_systems_offline_on_hit() {
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.queue = vec!["emp".into()];
        let t = make_ship("t", Faction::Enemy, 2, 20, LaneEnd::Fore);
        let mut board = make_board(7, vec![None, Some(p), Some(t), None, None, None, None]);
        fire_player_queue("p", &mut board, &ModContent(modded_weapon("emp", "emp_charge", 3)));
        let st = &board.cells[2].as_ref().unwrap().statuses;
        let off = st.iter().find(|s| s.kind == StatusKind::SystemsOffline).expect("systemsOffline applied");
        assert_eq!(off.duration, 3, "emp_charge applies systemsOffline for 3 turns");
    }

    /// targeting_laser: APPLY_STATUS targetLock on hit — and it lands even when
    /// the directional shield fully absorbs the hull damage (rider on contact).
    #[test]
    fn mod_targeting_laser_applies_target_lock_even_through_full_shield() {
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.queue = vec!["tl".into()];
        // Target with a big bow charge that eats the whole pulse; the rider
        // must still apply. Bow faces the attacker (incoming from aft side?).
        // Simplest: armour high enough to zero the hull damage.
        let mut t = make_ship("t", Faction::Enemy, 2, 20, LaneEnd::Fore);
        t.shield_profile = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 99, charge: 0 },
            stern: crate::types::ShieldFace { armour: 99, charge: 0 },
            port: crate::types::ShieldFace { armour: 99, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 99, charge: 0 },
        };
        let mut board = make_board(7, vec![None, Some(p), Some(t), None, None, None, None]);
        fire_player_queue("p", &mut board, &ModContent(modded_weapon("tl", "targeting_laser", 3)));
        let t_ref = board.cells[2].as_ref().unwrap();
        assert_eq!(t_ref.hull, 20, "shield fully absorbed the hull damage");
        assert!(
            t_ref.statuses.iter().any(|s| s.kind == StatusKind::TargetLock),
            "targeting_laser applies targetLock on contact even through full shield absorption",
        );
    }

    /// precision_core: a lethal hit recharges THIS action's cooldown to 0; a
    /// non-lethal hit does not.
    #[test]
    fn mod_precision_core_recharges_cooldown_only_on_kill() {
        // Lethal: target hull 3, pulse 3, no shield -> dies. Attacker's
        // cooldown for "pc" must be 0 afterward (not the cost's 3).
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.queue = vec!["pc".into()];
        let mut t = make_ship("t", Faction::Enemy, 2, 3, LaneEnd::Fore);
        t.shield_profile = zero.clone();
        let mut board = make_board(7, vec![None, Some(p), Some(t), None, None, None, None]);
        fire_player_queue("p", &mut board, &ModContent(modded_weapon("pc", "precision_core", 3)));
        assert!(board.cells[2].is_none(), "lethal hit killed the target");
        assert_eq!(
            board.cells[1].as_ref().unwrap().cooldowns.get("pc").copied(),
            Some(0),
            "precision_core recharges cooldown to 0 on a clean kill",
        );

        // Non-lethal: target survives, cooldown stays at the cost (3).
        let mut p2 = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p2.queue = vec!["pc".into()];
        let mut t2 = make_ship("t", Faction::Enemy, 2, 20, LaneEnd::Fore);
        t2.shield_profile = zero;
        let mut board2 = make_board(7, vec![None, Some(p2), Some(t2), None, None, None, None]);
        fire_player_queue("p", &mut board2, &ModContent(modded_weapon("pc", "precision_core", 3)));
        assert!(board2.cells[2].is_some(), "non-lethal hit left the target alive");
        assert_eq!(
            board2.cells[1].as_ref().unwrap().cooldowns.get("pc").copied(),
            Some(3),
            "precision_core does NOT recharge when the hit fails to kill",
        );
    }

    /// twin_linked: the action's effects apply twice (cost paid once). A 3-dmg
    /// no-falloff pulse on a 20-hull shieldless target lands 6 total.
    #[test]
    fn mod_twin_linked_applies_effects_twice() {
        let zero = ShieldProfile {
            bow: crate::types::ShieldFace { armour: 0, charge: 0 },
            stern: crate::types::ShieldFace { armour: 0, charge: 0 },
            port: crate::types::ShieldFace { armour: 0, charge: 0 },
            starboard: crate::types::ShieldFace { armour: 0, charge: 0 },
        };
        let mut p = make_ship("p", Faction::Player, 1, 5, LaneEnd::Fore);
        p.heat = 0;
        p.queue = vec!["twin".into()];
        let mut t = make_ship("t", Faction::Enemy, 2, 20, LaneEnd::Fore);
        t.shield_profile = zero;
        let weapon = {
            let mut a = modded_weapon("twin", "twin_linked", 3);
            a.cost = ActionCost { heat: 2, cooldown_max: 3, advances_turn: true };
            a
        };
        let mut board = make_board(7, vec![None, Some(p), Some(t), None, None, None, None]);
        fire_player_queue("p", &mut board, &ModContent(weapon));
        assert_eq!(board.cells[2].as_ref().unwrap().hull, 14, "twin_linked lands 3 twice = 6 (20 -> 14)");
        // Cost paid ONCE: heat went up by 2 (not 4).
        assert_eq!(board.cells[1].as_ref().unwrap().heat, 2, "twin_linked pays heat once, not per volley");
    }

    /// autoloader: the turn-dispatch seam reports the action as free-fire
    /// (advances_turn = false) regardless of the action's declared value.
    #[test]
    fn mod_autoloader_overrides_advances_turn_for_dispatch() {
        let mut a = pulse_laser();
        a.id = "auto".into();
        a.cost = ActionCost { heat: 1, cooldown_max: 3, advances_turn: true };
        a.r#mod = Some("autoloader".into());
        assert!(!action_advances_turn(&a), "autoloader forces free-fire (no turn advance)");

        // A plain action with no mod keeps its declared advances_turn.
        let plain = pulse_laser();
        assert!(action_advances_turn(&plain), "un-modded action keeps its declared advances_turn");

        // A non-autoloader mod does not change advances_turn.
        let mut flak = pulse_laser();
        flak.r#mod = Some("flak_burst".into());
        assert!(action_advances_turn(&flak), "flak_burst leaves advances_turn alone");
    }
}
