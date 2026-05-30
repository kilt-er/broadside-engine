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

    /// Total additive subsystem damage modifier against `target` at `band`.
    /// Called by [`apply_modifiers`] inside the canonical damage pipeline
    /// **after** band falloff and **before** target-lock doubling. Concrete
    /// `Content` impls scan their installed subsystem list and sum each
    /// match's contribution — Marksman is `+1` flat, Point-Blank Doctrine
    /// is `+2` when `band == PointBlank`, and so on.
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
    fn damage_modifier(&self, _target: &Ship, _band: RangeBand, _board: &Board) -> i32 {
        0
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
    //    onto Board.
    dmg = apply_modifiers(dmg, target_cell, band, board, content);

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
            for &c in cells {
                if board.cells[c].is_some() {
                    apply_damage(c, *amount, source_cell, a, board, content);
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
            resolve_self_move(source_cell, *mode, *distance, board, content);
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

        Effect::BOARD { .. } => {
            // Intentional no-op. There is no concrete catalog Action that
            // emits a BOARD effect today.
            //
            // The mass-* board-wide effects (mass_lock, mass_breach,
            // mass_emp, sensor_pulse) are **field-kit Cards** in the
            // analysis doc, not Actions — they live under
            // `Catalog::fieldkit`, not `Catalog::actions`, and field-kit
            // items are resolved by the (future) field-kit handler, not
            // through `applyEffect`. See the analysis HTML's "Ordnance &
            // Field Kit" section.
            //
            // When a real Action carrying a BOARD effect lands (e.g. a
            // class signature or capital-ship ability), this arm gets
            // wired then. The TS body at `resolve.ts:226-227` is also
            // empty, so leaving this stubbed matches the canonical
            // reference exactly.
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
fn apply_modifiers(
    dmg: i32,
    target_cell: usize,
    band: RangeBand,
    board: &Board,
    content: &dyn Content,
) -> i32 {
    let Some(target) = board.cells[target_cell].as_ref() else {
        return dmg;
    };
    let bonus = content.damage_modifier(target, band, board);
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
/// Movement runs in the ship's "bow" direction:
///   `BowOn { bow: Fore }` -> step +1
///   `BowOn { bow: Aft }`  -> step -1
///   `Broadside`           -> step +1 (arbitrary; broadside ships rarely
///                            queue a DISPLACE_SELF effect, and the design
///                            doc gives no preference; matches TS).
fn resolve_self_move(
    ship_cell: usize,
    mode: MovementMode,
    distance: i32,
    board: &mut Board,
    content: &dyn Content,
) {
    let Some(ship) = board.cells[ship_cell].as_ref() else {
        return;
    };
    let step: i32 = match ship.orientation {
        Orientation::BowOn { bow: LaneEnd::Aft } => -1,
        _ => 1,
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
///       already-queued enemy on this enemy's turn (diversity bonus)
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

    let has_trait = |t: crate::types::Trait| traits.iter().any(|x| *x == t);
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

        if best.as_ref().map_or(true, |(s, _)| score > *s) {
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
        super::resolve_self_move(2, MovementMode::THRUST, 1, &mut board, &NoContent);
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
        super::resolve_self_move(2, MovementMode::THRUST, 1, &mut board, &NoContent);
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
        super::resolve_self_move(6, MovementMode::THRUST, 1, &mut board, &NoContent);
        assert_eq!(board.cells[6].as_ref().unwrap().hull, 4);
    }

    /// BURN advances up to `distance` cells when clear.
    #[test]
    fn self_move_burn_advances_full_distance_when_clear() {
        let ship = make_ship("s", Faction::Player, 1, 10, LaneEnd::Fore);
        let mut board = make_board(7, vec![
            None, Some(ship), None, None, None, None, None,
        ]);
        super::resolve_self_move(1, MovementMode::BURN, 3, &mut board, &NoContent);
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
        super::resolve_self_move(1, MovementMode::BURN, 5, &mut board, &NoContent);
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
        super::resolve_self_move(0, MovementMode::SLIP, 2, &mut board, &NoContent);
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
        super::resolve_self_move(0, MovementMode::JUMP, 4, &mut board, &NoContent);
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
        super::resolve_self_move(0, MovementMode::JUMP, 4, &mut board, &NoContent);
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
        super::resolve_self_move(4, MovementMode::JUMP, 5, &mut board, &NoContent);
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
        super::resolve_self_move(2, MovementMode::TRACTOR_SWAP, 1, &mut board, &NoContent);
        assert_eq!(board.cells[2].as_ref().map(|s| s.id.clone()), Some("o".into()));
        assert_eq!(board.cells[3].as_ref().map(|s| s.id.clone()), Some("s".into()));
        // Cells updated to match new positions.
        assert_eq!(board.cells[2].as_ref().unwrap().cell, 2);
        assert_eq!(board.cells[3].as_ref().unwrap().cell, 3);
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
    struct FixedModifier(i32);
    impl Content for FixedModifier {
        fn action(&self, _: &str) -> Option<&Action> { None }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile { unreachable!() }
        fn damage_modifier(&self, _t: &Ship, _b: RangeBand, _board: &Board) -> i32 {
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
            effects: vec![Effect::DISPLACE_SELF { mode: MovementMode::BURN, distance: 3 }],
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
