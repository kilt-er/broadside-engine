//! Enemy AI decision layer.
//!
//! Extracted from [`crate::resolve`] (blueprint R-setup) so the resolver lane
//! (R4/R5/R7/R8 in `resolve.rs`) and the content lane (the C1 2-D AI ladder
//! rewrite, here) don't edit `resolve.rs` at the same time. This is a
//! **mechanical move** of `decide_enemy_action` + `queue_purposeful_maneuver`
//! out of `resolve.rs` — NO behaviour change. The four-phase round in
//! `resolve.rs` calls [`decide_enemy_action`] once per living enemy.
//!
//! ## v2 status (the C1 rewrite lands here)
//!
//! These bodies are still the **1-D** AI (they read `Ship::cell` as a lane
//! index and gate firing via the 1-D `crate::resolve::resolve_targeting`). On a
//! 2-D board that geometry is wrong — the AI *picks* via the 1-D gate while the
//! shot *fires* via the 2-D `resolve_targeting_2d` (the reviewer's V4 caveat).
//! Content's C1 rewrites these for the 2-D ladder and, critically, routes the
//! fire-gate through `crate::resolve::resolve_targeting_2d` (the single-source
//! targeting path) so the AI's intent matches where the shot lands and the
//! ThreatMap (R8) paints. This module exists so that rewrite is content's lane,
//! not a `resolve.rs` co-edit.

use crate::resolve::{resolve_targeting, Content};
use crate::types::{Board, Effect, Faction, LaneEnd};

/// Choose and queue the enemy at `enemy_cell`'s action for this world phase.
/// Mirrors what would be `decideEnemyAction` in `resolve.ts` (the TS body was a
/// stub). Fire-else-maneuver-else-reorient-else-vent ladder.
///
/// Under the fire-then-decide world phase (#67), whatever this function queues
/// stays in `enemy.queue` until the NEXT world phase fires it — so the
/// renderer's per-enemy telegraph always has something to show (a pending shot,
/// a close arrow, a reorient, or a vent).
pub fn decide_enemy_action(enemy_cell: usize, board: &mut Board, content: &dyn Content) {
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

    // 2. Enumerate this enemy's available threatening actions and score
    //    them. We collect (score, action_id) tuples; the best wins.
    //
    // NOTE (#74): there used to be a per-enemy "lane-end diversity" pass here
    // — a `covered_ends` set + a `+6` score bonus for threatening the player
    // from an end no earlier-queued ally already covered. It was removed as
    // VESTIGIAL: the +6 was provably a no-op on the QUEUED pick
    // (`my_end_from_player` is constant across one enemy's candidates, so the
    // bonus is added to all of them or none — argmax-preserving), and #71
    // dropped the only thing that ever made it behavioral (the
    // covered-end -> reposition-instead-of-fire suppression, which caused the
    // "march in a line, don't shoot, die" bug). True cross-enemy threat
    // coordination (an enemyInitiative pass assigning enemies to distinct
    // lane-ends) was never built; current lane-end diversity is emergent from
    // geometry. If explicit coordination is wanted later, it's a real
    // resolver feature, not a dead scoring term — so the term is gone rather
    // than left to mislead.
    let mut best: Option<(i32, String)> = None;

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
        //
        // v2 CAVEAT (V4): this is the 1-D `resolve_targeting` — wrong geometry
        // on a 2-D board. C1's rewrite must use `resolve_targeting_2d` here so
        // the AI's fire-gate matches the 2-D shot + the ThreatMap.
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
            // (#74: the +6 lane-end-diversity bonus that used to live here was
            // removed as vestigial — see the note at the top of the scoring
            // section.)
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

    // 4. FIRE when we can (#71). If the scoring loop found ANY in-band,
    //    bearing, affordable, hostile-targeting action, FIRE it — full stop.
    //    Firing from a good position is the point of the AI; it must actually
    //    happen.
    //
    //    This deliberately DROPS the old "if my end is already covered by an
    //    ally, reposition instead of firing" detour (#41 O1). That detour
    //    caused bruce's "march in a line, never shoot, die": with the live
    //    spawn shape (all enemies on ONE side of the player) every enemy but
    //    the first sees its end "covered", so every one of them maneuvered
    //    instead of firing — and since they're all on the same side, none
    //    ever reached an "uncovered" end, so they marched into the player and
    //    died without firing a shot. Repositioning to a fresh lane-end is
    //    rarely achievable on a 1-D lane, and "fire when in position" must
    //    win over "hold fire to maybe pressure a different end". The +6
    //    diversity term still shapes WHICH weapon an enemy picks (in the
    //    score above), it just no longer SUPPRESSES firing.
    if let Some((_, id)) = best.clone() {
        if let Some(s) = board.cells[enemy_cell].as_mut() {
            s.queue.push(id);
        }
        return;
    }

    // 5. Cannot fire effectively this turn -> maneuver toward an optimal
    //    firing position (#41 O1 "optimal position" / #68 anti-camp), then
    //    reorient, then vent. The close is a zero-heat synthetic move, so it
    //    is always "affordable" — but an OVERHEATED enemy must not mindlessly
    //    advance while it can't shoot; a locked-out ship prefers to VENT
    //    (handled below) so it can fire again. So only close when NOT locked
    //    out. (A heat-locked enemy that's also out of range will vent now and
    //    close once cool — still progresses, just not into a useless overheat.)
    if !locked_out && queue_purposeful_maneuver(enemy_cell, player_cell, board) {
        return;
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

/// Queue a PURPOSEFUL maneuver that CLOSES the enemy toward the player (#41
/// O1 "optimal position" / #68 anti-camp). Returns `true` and pushes the
/// closing-move id onto the enemy's queue; `false` only in the degenerate
/// case where the enemy is already on the player's cell.
///
/// # Why the SYNTHETIC lane-relative move
///
/// Live enemies carry NO movement action in their mounts — catalog enemies'
/// mounts are built purely from `def.weapons` (combat weapons) and the
/// fallback ship has a single `pulse_laser`. So an AI that only queued
/// *mounted* DISPLACE_SELF actions could never move — that was bruce's
/// "enemies never move" bug (the prior helper scanned mounts for a
/// DISPLACE_SELF that simply isn't there).
///
/// The PLAYER moves via SYNTHETIC lane-relative actions (`__move_left` /
/// `__move_right`); the AI now issues the SAME ids toward the player —
/// `__move_left` when the player is AFT of the enemy (lower cell),
/// `__move_right` when the player is FORE (higher cell). These carry
/// `direction: Some(LaneEnd::…)`, so `resolve_self_move` steps in the ABSOLUTE
/// lane direction independent of orientation — the enemy closes whichever way
/// its bow points, with no reorient dance and no "decline forever" trap.
///
/// The id is queued UNCONDITIONALLY (no `content.action` check): the resolver
/// serves these ids itself via `crate::resolve::resolver_ai_move` (used by
/// `fire_player_queue` when `content.action()` returns `None`), so the enemy
/// closes even when the running `Content` doesn't register the synthetic moves
/// — no DemoContent dependency.
///
/// v2 note: `direction_to`/the synthetic LEFT/RIGHT ids are the 1-D close;
/// C1's 2-D ladder will pick the cardinal (incl. the N/S depth-axis
/// `__move_up`/`__move_down` ids the resolver now serves) toward the player.
fn queue_purposeful_maneuver(enemy_cell: usize, player_cell: usize, board: &mut Board) -> bool {
    if enemy_cell == player_cell {
        return false; // degenerate; nothing to close
    }
    let synth_id = match crate::geometry::direction_to(enemy_cell, player_cell) {
        LaneEnd::Aft => crate::input::SYNTHETIC_MOVE_LEFT,
        LaneEnd::Fore => crate::input::SYNTHETIC_MOVE_RIGHT,
    };
    if let Some(s) = board.cells[enemy_cell].as_mut() {
        s.queue.push(synth_id.to_string());
        return true;
    }
    false
}
