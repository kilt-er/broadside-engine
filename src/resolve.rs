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
    Action, ActionCost, Arc, Board, DeployHazardKind, Effect, Faction, Hazard, HazardKind, Hook,
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

/// One full round. Mirrors `resolveRound` in `resolve.ts`.
pub fn resolve_round(board: &mut Board, content: &dyn Content) {
    // 1 - player queue, bottom -> top.
    if let Some(player_cell) = ships_of(board).iter().find_map(|s| (s.faction == Faction::Player).then_some(s.cell)) {
        execute_queue(player_cell, board, content);
    }

    // 2 - advance every live projectile by its speed, resolve impacts. This
    // is its own chain-kill window — reset the counter so kills caused by
    // ordnance impacts (e.g. multi-projectile torpedoes piercing low-hull
    // enemies) are scored separately from the player's queue. The TS does not
    // emit `onChainKill` from the ordnance phase itself (only `executeQueue`
    // does); we match that and leave the emit gate to `executeQueue`.
    //
    // TS iterates a SHALLOW COPY of `board.ordnance` because each
    // `advanceProjectile` may remove its projectile from the live list. We do
    // the same: snapshot the ids, then advance each by id-lookup.
    board.destroys_this_window = 0;
    let projectile_ids: Vec<String> = board.ordnance.iter().map(|p| p.id.clone()).collect();
    for id in projectile_ids {
        advance_projectile(&id, board, content);
    }

    // 3 - enemy phase, in telegraphed initiative order.
    for enemy_cell in enemy_initiative(board) {
        if skips_turn(board, enemy_cell) {
            continue;
        }
        decide_enemy_action(enemy_cell, board, content); // TODO(broadside-content): AI fills the queue
        execute_queue(enemy_cell, board, content);
    }

    // 4 - end of turn.
    end_of_turn(board, content);
}

/* =============================================================================
 * Phase 1 / 3 — executeQueue: the arc + heat + cooldown gate.
 * ========================================================================== */

/// Execute a ship's queued actions in order. Mirrors `executeQueue` in
/// `resolve.ts`. The ship is identified by lane `cell` rather than a borrow
/// because effects (movement, destroys) can mutate the board's cell vector
/// underneath us.
pub fn execute_queue(ship_cell: usize, board: &mut Board, content: &dyn Content) {
    // Chain-kill window starts here. `destroy()` increments
    // `destroys_this_window`; `detect_chain` reads it after the queue runs.
    // Each `execute_queue` call is one window, and so is each ordnance-phase
    // pass — both reset to 0 on entry.
    board.destroys_this_window = 0;

    // The queue is copied out up front because applying an effect can mutate
    // the ship (e.g. clearing `lockedOut`, repositioning), and we need a
    // stable iteration order. Matches the TS `for (const actionId of
    // ship.queue)` which is also stable across mutations to the ship object.
    let queue: Vec<String> = match board.cells[ship_cell].as_ref() {
        Some(s) => s.queue.clone(),
        None => return,
    };

    for action_id in &queue {
        // Clone the Action so we don't hold a borrow on `content` while we
        // mutate the board.
        let action = match content.action(action_id) {
            Some(a) => a.clone(),
            None => continue, // TS: `if (!a) continue` — unknown action ids are skipped silently.
        };

        // Read the gating state from the ship without holding a borrow.
        let Some(ship) = board.cells[ship_cell].as_ref() else {
            // The ship was destroyed by a prior effect in this very queue.
            // TS happens to silently no-op the rest because `ship.queue` was
            // already iterated; matching that here.
            return;
        };
        // Overheated: only free / zero-heat actions can fire.
        if ship.locked_out && action.cost.heat > 0 {
            continue;
        }
        // Not charged yet.
        if ship.cooldowns.get(action_id).copied().unwrap_or(0) > 0 {
            continue;
        }

        // Resolve targeting (uses the board to walk lane cells).
        let cells = resolve_targeting(&action, board, ship_cell);
        // The "nothing bore" gate: arc-required actions with no targets eat
        // nothing — cooldown is NOT reset and heat is NOT spent.
        if action.targeting.requires_arc.is_some() && cells.is_empty() {
            continue;
        }

        // Apply each effect. `apply_effect` may mutate cells / ordnance / etc.
        for fx in &action.effects {
            apply_effect(fx, &action, ship_cell, &cells, board, content);
        }

        // Heat + cooldown bookkeeping. The TS resets `cooldowns[actionId]`
        // unconditionally once the action passed the arc gate — hit or miss
        // on individual effects. We match that exactly.
        if let Some(ship) = board.cells[ship_cell].as_mut() {
            ship.heat += action.cost.heat;
            if ship.heat >= ship.heat_max {
                ship.locked_out = true; // overheat -> lockout until vent
            }
            ship.cooldowns.insert(action_id.clone(), action.cost.cooldown_max);
        }

        emit(board, Hook::OnDamageDealt, |ctx| {
            ctx.source_cell = Some(ship_cell);
        });
    }

    if detect_chain(board) {
        emit(board, Hook::OnChainKill, |ctx| {
            ctx.source_cell = Some(ship_cell);
        });
    }

    // Clear the queue. The ship may have been destroyed during effect
    // application; only clear if it still exists.
    if let Some(ship) = board.cells[ship_cell].as_mut() {
        ship.queue.clear();
    }
}

/* =============================================================================
 * Phase 2 — advanceProjectile.
 * ========================================================================== */

/// Step a single projectile by its speed, resolving impacts. Mirrors
/// `advanceProjectile` in `resolve.ts`. Identified by id rather than `&mut`
/// because the projectile may remove itself from `board.ordnance` on impact.
pub fn advance_projectile(projectile_id: &str, board: &mut Board, _content: &dyn Content) {
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
                            apply_damage(impact_cell, *amount, impact_cell, &dummy, board);
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
pub fn end_of_turn(board: &mut Board, _content: &dyn Content) {
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
        tick_statuses(*c, board);
    }
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
pub fn apply_damage(
    target_cell: usize,
    raw: i32,
    atk_cell: usize,
    weapon: &Action,
    board: &mut Board,
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

    // 2. Subsystem modifiers. Stubbed — content slice owns it.
    dmg = apply_modifiers(dmg, target_cell, band, board);

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
        destroy(target_cell, board);
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
            for &c in cells {
                if board.cells[c].is_some() {
                    apply_damage(c, *amount, source_cell, a, board);
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

        Effect::DISPLACE_SELF { mode, distance } => {
            // TODO(broadside-content): implement THRUST/BURN/SLIP/JUMP/
            // TRACTOR_SWAP with proper collision rules. Body below is the TS
            // stub verbatim — simple step-loop in the bow direction with
            // occupancy checks.
            resolve_self_move(source_cell, *mode, *distance, board);
        }

        Effect::DISPLACE_TARGET { mode, distance } => {
            // TODO(broadside-content): push/pull/swap with collision damage.
            // Body below is the TS no-op stub.
            for &c in cells {
                resolve_target_move(c, *mode, *distance, board);
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

        Effect::BOARD { .. } => {
            // TODO(broadside-content): board-wide effects (mass card items,
            // lightning analogs). TS has an empty body here.
        }
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
        // TS probes `ship.cell ± 1`; we do the same, signed to avoid
        // underflow at cell 0.
        let probe = match end {
            LaneEnd::Fore => ship_cell as i32 + 1,
            LaneEnd::Aft => ship_cell as i32 - 1,
        };
        if probe < 0 {
            // Probe would be off-lane in the aft direction; still ask `bears`
            // — the arc gate cares about ORIENTATION, not lane bounds, and
            // matching TS means passing through. Treat as cell 0 (which TS
            // does implicitly via `-1` becoming a negative number that
            // `bears`->`direction_to` interprets as aft).
            if bears(ship, arc, 0) && matches!(arc, Some(Arc::Rear) | Some(Arc::Turret) | Some(Arc::BroadsideArc)) {
                // Only return aft for arcs that meaningfully fire aft.
                if matches!(end, LaneEnd::Aft) {
                    return Some(end);
                }
            }
            continue;
        }
        if bears(ship, arc, probe as usize) {
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
/// duration reaches 0. Mirrors `tickStatuses` in `resolve.ts`.
fn tick_statuses(cell: usize, board: &mut Board) {
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
        destroy(cell, board);
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
pub fn destroy(cell: usize, board: &mut Board) {
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
                apply_damage(nc, 2, owner_cell, &dummy, board);
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

/// TODO(broadside-content): sum subsystem damage bonuses (Marksman,
/// Point-Blank Doctrine, ...). TS body: returns `dmg` unchanged.
fn apply_modifiers(dmg: i32, _target_cell: usize, _band: RangeBand, _board: &Board) -> i32 {
    dmg
}

/// TODO(broadside-content): full THRUST/BURN/SLIP/JUMP/TRACTOR_SWAP with
/// occupancy + collision rules. TS body kept verbatim: simple step-loop in
/// the bow direction with occupancy checks.
fn resolve_self_move(ship_cell: usize, _mode: MovementMode, distance: i32, board: &mut Board) {
    let Some(ship) = board.cells[ship_cell].as_ref() else {
        return;
    };
    let step: i32 = match ship.orientation {
        Orientation::BowOn { bow: LaneEnd::Aft } => -1,
        _ => 1,
    };
    let mut c = ship_cell as i32;
    for _ in 0..distance {
        let next = c + step;
        if next < 0 || (next as usize) >= board.size {
            break;
        }
        if board.cells[next as usize].is_some() {
            break;
        }
        c = next;
    }
    let final_cell = c as usize;
    if final_cell == ship_cell {
        return;
    }
    // Move the ship: take from old cell, update `cell`, place in new cell.
    let mut ship = board.cells[ship_cell].take().expect("source still occupied at start");
    ship.cell = final_cell;
    board.cells[final_cell] = Some(ship);
}

/// TODO(broadside-content): push/pull/swap with collision damage; mirror
/// `resolve_self_move`. TS body is a no-op stub.
fn resolve_target_move(_target_cell: usize, _mode: crate::types::DisplaceMode, _distance: i32, _board: &mut Board) {
    // intentionally empty — matches TS stub
}

/// TODO(broadside-content): AI decision layer. Pick actions to maximise
/// threatened lane-ends (the flanking objective), then reuse `execute_queue`
/// unchanged. TS body is a no-op stub.
fn decide_enemy_action(_enemy_cell: usize, _board: &mut Board, _content: &dyn Content) {
    // intentionally empty — matches TS stub
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
    use crate::types::{ActionCost, EventBus, Mount, Orientation, ShieldProfile};
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
        apply_damage(1, 4, 0, &weapon, &mut board);
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
        apply_damage(1, 4, 0, &weapon, &mut board);
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
        apply_damage(1, 4, 0, &weapon, &mut board);
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
        apply_damage(1, 4, 0, &weapon, &mut board);
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
        execute_queue(0, &mut board, &content);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 6, "heat should be 5 + 1");
        assert!(p.locked_out, "heat at heat_max triggers lockout");
        assert_eq!(p.cooldowns.get("pulse_laser").copied(), Some(0));
        assert!(p.queue.is_empty(), "queue should be cleared after execution");
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
        execute_queue(0, &mut board, &content);
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
        apply_damage(5, 4, 0, &weapon, &mut board);
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
        apply_damage(5, 4, 0, &weapon, &mut board);
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
        execute_queue(0, &mut board, &Empty);
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
        execute_queue(0, &mut board, &content);

        // Both ships should be gone, and OnChainKill should have fired once.
        assert!(board.cells[2].is_none(), "scout was killed");
        assert!(board.cells[4].is_none(), "gunboat was killed");
        assert_eq!(count.get(), 1, "OnChainKill fires once for the window");
    }
}
