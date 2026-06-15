//! Enemy AI decision layer — the 2-D ladder (blueprint C1).
//!
//! Extracted from [`crate::resolve`] (blueprint R-setup, commit 1654e67) so the
//! resolver lane (R4/R5/R7/R8 in `resolve.rs`) and the content lane (this 2-D AI
//! rewrite) don't edit `resolve.rs` at the same time. The four-phase round in
//! `resolve.rs` calls [`decide_enemy_action`] once per living enemy, in
//! `enemy_initiative` order, during the world phase.
//!
//! ## The ladder (C1, `docs/design/C1_AI_LADDER_2D.md`)
//!
//! Per enemy, first match wins: **FIRE → CLOSE/HOLD-RANGE → REORIENT → VENT →
//! empty**. The AI is a *decision layer that only builds the enemy's queue* —
//! it never bypasses `execute_queue` / the damage pipeline (hard boundary). It
//! queues actions (real mounts or the resolver-served synthetic moves) and lets
//! the resolver run them next world phase.
//!
//! ### Single-source fire-gate (V4-at-C1)
//!
//! The FIRE rung's "can I fire?" test IS [`crate::resolve::resolve_targeting_2d`]
//! — the SAME 2-D targeting path the shot fires through and the ThreatMap (R8)
//! caches. So what the AI elects to fire == what the telegraph paints == where
//! the shot lands; there is NO second targeting path (the 1-D
//! `resolve_targeting` is never called here — reviewer V4 greps this file for
//! `resolve_targeting(` and must find zero). This also gives over-extension for
//! free: a Far weapon's `range_band` excludes `Adjacent`, so when the player has
//! closed to distance 1 the gate returns empty → the enemy is correctly inert
//! (blueprint decision #7); Rung 2 then backs it off to re-open range rather
//! than charging in.
//!
//! ### Telegraph / cross-turn
//!
//! Whatever this queues stays in `enemy.queue` until the NEXT world phase fires
//! it, so on the player's turn the renderer's per-enemy telegraph always has the
//! enemy's next intent to show (a pending shot, a close/back-off arrow, a
//! reorient, or a vent) — the read-and-react loop.

use crate::grid::{self, Dir8, Pos, Range};
use crate::resolve::{resolve_targeting_2d, Content};
use crate::types::{Board, Effect, Faction, Trait};

/// Choose and queue the enemy at `enemy_cell`'s action for this world phase
/// (the 2-D C1 ladder). `enemy_cell` is the flat board index the resolver's
/// world-phase loop holds; under the board's slot==pos invariant (A) it equals
/// the enemy's [`Pos::to_index`], so we recover the 2-D position with
/// [`Pos::from_index`] and work in `Pos`/`Dir8` from here. Keeping the `usize`
/// entry point means the resolver's call site in `run_world_phase` is unchanged
/// (no `resolve.rs` co-edit).
pub fn decide_enemy_action(enemy_cell: usize, board: &mut Board, content: &dyn Content) {
    // Recover the enemy's 2-D position (invariant A: slot == pos.to_index()).
    let Some(enemy_pos) = Pos::from_index(enemy_cell) else {
        return; // out-of-grid index — nothing to decide
    };

    // 1. Locate the player's 2-D position.
    let Some(player_pos) = board.cells.iter().find_map(|c| {
        c.as_ref().and_then(|s| (s.faction == Faction::Player).then_some(s.pos))
    }) else {
        return;
    };

    // Snapshot the enemy's gating state (read-only borrow released before the
    // scoring loop, which also borrows the board for resolve_targeting_2d).
    let Some(enemy) = board.ship_at(enemy_pos) else {
        return;
    };
    let heat = enemy.heat;
    let heat_max = enemy.heat_max;
    let locked_out = enemy.locked_out;
    let cooldowns = enemy.cooldowns.clone();
    let mount_weapons: Vec<String> = enemy.mounts.iter().map(|m| m.weapon.clone()).collect();
    let traits: Vec<Trait> = enemy.traits.clone();

    let has_trait = |t: Trait| traits.contains(&t);
    let burn_hard = has_trait(Trait::BurnHard);
    let pursuit = has_trait(Trait::Pursuit);
    let anchored = has_trait(Trait::Anchored);

    /* -- RUNG 1: FIRE (commit when able) --------------------------------- */
    // Score every affordable, in-arc, in-band, hostile-targeting weapon and fire
    // the best. The gate IS resolve_targeting_2d (the single source — V4); empty
    // result = can't fire (off-arc, out-of-band, or the #7 deadzone when the
    // player closed on a long-range gun).
    let mut best: Option<(i32, String)> = None;
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        // Cooldown gate.
        if cooldowns.get(weapon_id).copied().unwrap_or(0) > 0 {
            continue;
        }
        // Heat / lockout gate: locked out -> only zero-heat; else don't push
        // more than 1 past heat_max (a whole turn lost to venting otherwise).
        if locked_out && action.cost.heat > 0 {
            continue;
        }
        if heat + action.cost.heat > heat_max + 1 {
            continue;
        }
        // Arc + band + deadzone gate, via the 2-D single source (V4-at-C1).
        let cells = resolve_targeting_2d(action, board, enemy_pos);
        if cells.is_empty() {
            continue;
        }
        // Friendly-fire filter (#49): the target set must contain >=1 non-enemy.
        let any_hostile = cells.iter().any(|&p| {
            board.ship_at(p).map(|s| s.faction != Faction::Enemy).unwrap_or(false)
        });
        if !any_hostile {
            continue;
        }
        // Score: player-hit + raw damage - heat (halved for BurnHard) + Pursuit.
        let raw_damage: i32 = action
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::DAMAGE { amount, .. } => Some(*amount),
                _ => None,
            })
            .sum();
        let hits_player = cells.contains(&player_pos);
        let mut score: i32 = 0;
        if hits_player {
            score += 10;
        }
        score += raw_damage;
        score -= if burn_hard { action.cost.heat / 2 } else { action.cost.heat };
        if pursuit && hits_player {
            score += 2;
        }
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, weapon_id.clone()));
        }
    }
    // FIRE when we can — full stop (#71: "fire when in position" beats holding).
    if let Some((_, id)) = best {
        if let Some(s) = board.ship_at_mut(enemy_pos) {
            s.queue.push(id);
        }
        return;
    }

    /* -- RUNG 2: CLOSE / HOLD-RANGE (the 2-D over-extension decision) ----- */
    // Only when we couldn't fire AND aren't locked out (a locked-out enemy
    // prefers to VENT below so it can fire again, not maneuver uselessly).
    // Anchored ships skip this rung (immune to self-displacement).
    if !locked_out && !anchored {
        if let Some(dir) = choose_maneuver_dir(&mount_weapons, enemy_pos, player_pos, content) {
            if let Some(synth_id) = synthetic_move_for_dir(dir) {
                if let Some(s) = board.ship_at_mut(enemy_pos) {
                    s.queue.push(synth_id.to_string());
                    return;
                }
            }
        }
    }

    /* -- RUNG 3: REORIENT ------------------------------------------------- */
    // Turning the bow/broadside may bring the player into a forward/broadside
    // arc next phase. The AI just picks the reorient; arc math is the resolver's.
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
            if let Some(s) = board.ship_at_mut(enemy_pos) {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }

    /* -- RUNG 4: VENT ----------------------------------------------------- */
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        if action.effects.iter().any(|e| matches!(e, Effect::VENT_HEAT { .. })) {
            if let Some(s) = board.ship_at_mut(enemy_pos) {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }

    /* -- RUNG 5: empty queue (misconfigured enemy with no valid mount) ---- */
    // The world phase no-ops the turn. A correctly-configured enemy never
    // reaches here. (Liveness holds for the other rungs: a queued move /
    // reorient / vent is a visible non-damage telegraph.)
}

/// Pick a one-step [`Dir8`] for the CLOSE/HOLD-RANGE rung, or `None` to hold.
/// The 2-D over-extension decision (blueprint §3): reads the SAME
/// `range_band` band classification the fire-gate uses — never a parallel
/// targeting path — only to decide WHICH WAY to move, never whether to fire.
///
/// Keyed on the enemy's dominant (highest-raw-damage) weapon:
///   - inert because the player is TOO CLOSE (player at a band nearer than the
///     weapon's nearest firing band — the #7 deadzone): step AWAY to re-open
///     range so the gun re-arms next phase (the active back-off that keeps
///     over-extension a real threat; never a charge-in).
///   - inert because the player is TOO FAR: step TOWARD the player to close into
///     the weapon's band window.
///   - in band but couldn't fire (an ARC problem): hold (`None`) and let Rung 3
///     (reorient) try to bring the arc to bear.
///
/// `None` (hold) also when there is no dominant weapon or the enemy is on the
/// player's cell.
fn choose_maneuver_dir(
    mount_weapons: &[String],
    enemy_pos: Pos,
    player_pos: Pos,
    content: &dyn Content,
) -> Option<Dir8> {
    // Dominant weapon = the mount with the highest summed DAMAGE amount; its
    // band set drives close-vs-open (a long gun holds distance; a short gun
    // closes).
    let dominant = mount_weapons
        .iter()
        .filter_map(|id| content.action(id))
        .max_by_key(|a| {
            a.effects
                .iter()
                .filter_map(|e| match e {
                    Effect::DAMAGE { amount, .. } => Some(*amount),
                    _ => None,
                })
                .sum::<i32>()
        })?;

    let toward = grid::from_to(enemy_pos, player_pos)?; // None only if co-located
    let away = toward.opposite();

    // The dominant weapon's allowed 2-D bands. During the EXPAND window the
    // catalog may not have re-authored `range_band` yet (empty set); treat empty
    // as "no band preference" and close (the v1 behavior) so transition catalogs
    // still produce a sensible advance rather than freezing.
    let bands = &dominant.targeting.range_band;
    if bands.is_empty() {
        return Some(toward);
    }
    let cur = grid::range_band(enemy_pos, player_pos);
    if bands.contains(&cur) {
        // In band but Rung 1 couldn't fire -> almost certainly an arc problem;
        // hold here (don't wander out of band) and fall through to reorient.
        return None;
    }
    // Out of band: open if the player is nearer than the nearest firing band,
    // else close.
    let cur_o = band_ordinal(cur);
    let min_allowed = bands.iter().map(|b| band_ordinal(*b)).min().unwrap_or(cur_o);
    let max_allowed = bands.iter().map(|b| band_ordinal(*b)).max().unwrap_or(cur_o);
    if cur_o < min_allowed {
        Some(away) // player too close -> back off
    } else if cur_o > max_allowed {
        Some(toward) // player too far -> close
    } else {
        None // gapped band set; hold rather than guess
    }
}

/// Ordinal of a [`Range`] band for near/far comparison (`Adjacent` < `Near` <
/// `Far`). Local to the AI's maneuver heuristic — not a geometry seam.
fn band_ordinal(r: Range) -> u8 {
    match r {
        Range::Adjacent => 0,
        Range::Near => 1,
        Range::Far => 2,
    }
}

/// Map a chosen [`Dir8`] step to the resolver-served synthetic move id. The AI
/// closes/opens via the SAME synthetic moves the player uses (resolver-served
/// through `resolver_ai_move`, so no `Content` dependency). All four cardinals
/// are served as of R6. A diagonal step prefers its LATERAL (column) component —
/// lateral is the dodge axis and the move resolves one cell, so the next phase
/// re-decides and the depth component follows. `None` only for the zero vector
/// (co-located, already excluded upstream by `from_to`).
fn synthetic_move_for_dir(dir: Dir8) -> Option<&'static str> {
    use crate::input::{
        SYNTHETIC_MOVE_DOWN, SYNTHETIC_MOVE_LEFT, SYNTHETIC_MOVE_RIGHT, SYNTHETIC_MOVE_UP,
    };
    let (dc, dr) = dir.delta();
    if dc < 0 {
        Some(SYNTHETIC_MOVE_LEFT) // W: decreasing col
    } else if dc > 0 {
        Some(SYNTHETIC_MOVE_RIGHT) // E: increasing col
    } else if dr < 0 {
        Some(SYNTHETIC_MOVE_UP) // N: toward row 0 / away from the player (back off)
    } else if dr > 0 {
        Some(SYNTHETIC_MOVE_DOWN) // S: toward the player (close)
    } else {
        None
    }
}
