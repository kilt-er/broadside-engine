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
//! — the SAME 2-D targeting path the shot fires through and the `ThreatMap` (R8)
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
use crate::resolve::{enemy_initiative, resolve_targeting_2d, Content};
use crate::types::{Board, Effect, Faction, Trait};

/// C2 (#35) threat-spread tie-breaker weight: score penalty PER target cell that
/// overlaps an already-threatened (by an earlier-committed ally) cell. Kept
/// SMALL — one overlapping cell (`-1`) cannot flip the choice away from hitting
/// the player (`+10`) or a higher-damage shot; it only separates otherwise
/// comparable shots so the squad fans its threat. NEVER gates firing. Tunable;
/// any change here is balance-touching → pre-propose.
const SPREAD_OVERLAP_PENALTY: i32 = 1;

/// Choose and queue the enemy at `enemy_cell`'s action for this world phase
/// (the 2-D C1 ladder). `enemy_cell` is the flat board index the resolver's
/// world-phase loop holds; under the board's slot==pos invariant (A) it equals
/// the enemy's [`Pos::to_index`], so we recover the 2-D position with
/// [`Pos::from_index`] and work in `Pos`/`Dir8` from here. Keeping the `usize`
/// entry point means the resolver's call site in `run_world_phase` is unchanged
/// (no `resolve.rs` co-edit).
pub fn decide_enemy_action(enemy_cell: usize, board: &mut Board, content: &dyn Content) {
    // Recover the enemy's 2-D position (invariant A: slot == pos.to_index()).
    // Width migration: from_index_in(board.dims()) == from_index() on a 5x4 board.
    let Some(enemy_pos) = Pos::from_index_in(enemy_cell, board.dims()) else {
        return; // out-of-grid index — nothing to decide
    };

    // 1. Locate the player's 2-D position.
    let Some(player_pos) = board.cells.iter().find_map(|c| {
        c.as_ref()
            .and_then(|s| (s.faction == Faction::Player).then_some(s.pos))
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
    // (#166) The enemy's current facing — snapshotted here so RUNG 2's
    // rotate-then-forward maneuver knows which way is "forward" (the bow). Same
    // read-only block as the gating state above (borrow released before the
    // scoring loop re-borrows the board).
    let facing = enemy.facing;

    let has_trait = |t: Trait| traits.contains(&t);
    let burn_hard = has_trait(Trait::BurnHard);
    let pursuit = has_trait(Trait::Pursuit);
    let anchored = has_trait(Trait::Anchored);

    /* -- RUNG 1: FIRE (commit when able) --------------------------------- */
    // Score every affordable, in-arc, in-band, hostile-targeting weapon and fire
    // the best. The gate IS resolve_targeting_2d (the single source — V4); empty
    // result = can't fire (off-arc, out-of-band, or the #7 deadzone when the
    // player closed on a long-range gun).
    //
    // C2 (#35, the #74 threat-SPREAD): the cells already threatened by allies who
    // committed EARLIER in this decision pass. Used as a TIE-BREAKER in the score
    // below — a candidate shot whose target cells overlap the spread set is mildly
    // penalised, so the squad fans its threats across DISTINCT cells (the player
    // can't sidestep one cell to dodge everything). It is NEVER a gate: spread
    // only chooses AMONG viable shots, never whether to shoot (the #41/#71 lesson
    // — diversity must not cause "march, don't shoot"). See
    // [`allies_threatened_cells`].
    let spread_set = allies_threatened_cells(enemy_pos, board, content);
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
            board
                .ship_at(p)
                .is_some_and(|s| s.faction != Faction::Enemy)
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
        score -= if burn_hard {
            action.cost.heat / 2
        } else {
            action.cost.heat
        };
        if pursuit && hits_player {
            score += 2;
        }
        // C2 spread tie-breaker: penalise overlap with cells already threatened
        // by earlier-committed allies, so the squad spreads its threat. SMALL
        // and dominated by the +10 player-hit / raw-damage terms — it breaks
        // ties between comparable shots, never overrides "hit the player hard"
        // and never suppresses a shot (the action is already a viable fire here;
        // this only nudges WHICH viable shot). Counts each target cell once.
        let overlap = cells.iter().filter(|c| spread_set.contains(c)).count() as i32;
        score -= SPREAD_OVERLAP_PENALTY * overlap;
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
    //
    // (#166 rotate-then-forward, Bruce's no-strafe ruling) Ships do NOT slide
    // sideways. `choose_maneuver_dir` still owns the BAND decision (close vs open
    // vs hold) — but instead of emitting the absolute slide it returned, we:
    //   1. derive the TARGET cardinal (the dominant-component Dir4 of the raw
    //      enemy->player delta, flipped to its opposite when opening range), and
    //   2. if that cardinal lies on the bow AXIS (== forward, or == its reverse),
    //      queue the on-axis forward/reverse step (`synthetic_move_for_dir`);
    //   3. otherwise (the cardinal is perpendicular to the bow) queue a ROTATE
    //      toward it — next phase the bow points the right way and the enemy
    //      advances forward. Rotate-then-forward, never a free lateral step.
    // This mirrors the resolver's forward-only self-move gate (#167, landing
    // AFTER this) — but the AI must already STOP emitting laterals on its own, so
    // it never relies on the gate to bounce an illegal move.
    if !locked_out && !anchored {
        if let Some(dir) = choose_maneuver_dir(&mount_weapons, enemy_pos, player_pos, content) {
            // `choose_maneuver_dir` returns exactly `from_to(enemy, player)`
            // (CLOSE) or its `.opposite()` (OPEN). So we recover the sense by
            // comparing against the toward-direction; `from_to` is `Some` here
            // (co-located enemies make it `None`, and then the call above is
            // `None` too, so we wouldn't be inside this block).
            let toward8 = grid::from_to(enemy_pos, player_pos);
            let opening = toward8 != Some(dir);
            if let Some(toward_card) = dominant_cardinal(enemy_pos, player_pos) {
                let target = if opening {
                    toward_card.opposite()
                } else {
                    toward_card
                };
                let forward = crate::input::forward_dir4(facing);
                let synth_id = if target == forward || target == forward.opposite() {
                    // On the bow axis: advance forward / reverse straight.
                    synthetic_move_for_dir(target.to_dir8())
                } else {
                    // Perpendicular to the bow: turn toward the approach first.
                    rotate_toward_cardinal(forward, target)
                };
                if let Some(synth_id) = synth_id {
                    if let Some(s) = board.ship_at_mut(enemy_pos) {
                        s.queue.push(synth_id.to_string());
                        return;
                    }
                }
            }
        }
    }

    /* -- RUNG 3: REORIENT ------------------------------------------------- */
    // Turning the bow/broadside may bring the player into a forward/broadside
    // arc next phase. The AI just picks the reorient; arc math is the resolver's.
    // 3a: a mounted weapon that ITSELF reorients (e.g. a sweep) — fire it.
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
        if action
            .effects
            .iter()
            .any(|e| matches!(e, Effect::REORIENT { .. }))
        {
            if let Some(s) = board.ship_at_mut(enemy_pos) {
                s.queue.push(weapon_id.clone());
            }
            return;
        }
    }
    // 3b (Q3 rotate-to-bear, ARC-AGNOSTIC #92): reaching here means we couldn't
    // FIRE, Rung 2 held (in band but off-ARC) or declined, and no weapon
    // self-reorients. A mis-ORIENTED hull would otherwise fall to Rung 3.5 and
    // CLOSE forever, mashing the player's cell without ever turning its gun to
    // bear — the "camp + never fire" bug. So queue a synthetic ROTATE toward the
    // facing that makes the enemy's own weapon BEAR on the player — bow-on for a
    // Forward gun, SIDE-on for a BroadsideArc gun, etc. The bearing test IS
    // `resolve_targeting_2d` (the single source — `rotate_to_make_weapon_bear`
    // probes each candidate facing through it), so it's arc-agnostic: no
    // bow-vs-broadside hardcode. Resolver-served rotate (`resolver_ai_move`), so
    // no Content-action dependency. Skipped for locked-out (prefers VENT) +
    // Anchored (hold + vent, don't spin). Next phase the new facing bears ->
    // Rung 1 fires.
    if !locked_out && !anchored {
        if let Some(rot_id) =
            rotate_to_make_weapon_bear(&mount_weapons, enemy_pos, player_pos, board, content)
        {
            if let Some(s) = board.ship_at_mut(enemy_pos) {
                s.queue.push(rot_id.to_string());
                return;
            }
        }
    }

    /* -- RUNG 3.5: FALLBACK CLOSE (never camp) ---------------------------- */
    // Reached when the enemy couldn't FIRE, choose_maneuver_dir held (Rung 2
    // returns None for "in band but blocked by ARC/HEAT/COOLDOWN" — it expects a
    // reorient), AND no REORIENT action exists (Rung 3 found none). Pre-fix the
    // ladder fell through to empty here and the enemy CAMPED — bruce's "enemies
    // just sit there" + the 3 red ai_2d maneuver tests. v1 always closed in this
    // case. So: if not locked-out / anchored, close one step toward the player so
    // the enemy keeps applying pressure (and likely brings its arc to bear next
    // phase) rather than queuing nothing. Over-extension is unharmed — this only
    // fires when Rung 2's band-aware open/close already declined (i.e. the weapon
    // is in band; closing keeps it in/below band, never strands a Far gun's
    // deadzone open). A locked-out enemy still prefers VENT (below).
    //
    // (#166) Same rotate-then-forward discipline as RUNG 2: the close is along
    // the bow axis when the player's dominant cardinal lies on it, else a rotate
    // toward that cardinal (no lateral slide). The CLOSE sense is always "toward"
    // here, so the target IS the dominant cardinal (no open-flip).
    //
    // (#166) Gated on having at least one mount: a MOUNTLESS hull has no weapon to
    // bring to bear, so neither closing nor rotating serves any purpose — it would
    // just wander. Under the old strafe model a mountless ship slid sideways toward
    // the player (a latent oddity that happened to keep the winnability canary
    // converging — it lined a stray target up under the player's column); the
    // forward-only model can't line up off-column without strafing, so a wandering
    // mountless ship instead chase-livelocked that canary. Holding it still is both
    // more correct (no gun = no maneuver intent) AND restores convergence (the
    // player closes onto the now-stationary target). No live enemy is mountless, so
    // this is invisible in real combat — only the test harness builds gun-less
    // "target" hulls. (RUNG 2 is already mount-gated: `choose_maneuver_dir` returns
    // None when there is no dominant weapon.)
    if !locked_out && !anchored && !mount_weapons.is_empty() {
        if let Some(target) = dominant_cardinal(enemy_pos, player_pos) {
            let forward = crate::input::forward_dir4(facing);
            let synth_id = if target == forward || target == forward.opposite() {
                synthetic_move_for_dir(target.to_dir8())
            } else {
                rotate_toward_cardinal(forward, target)
            };
            if let Some(synth_id) = synth_id {
                if let Some(s) = board.ship_at_mut(enemy_pos) {
                    s.queue.push(synth_id.to_string());
                    return;
                }
            }
        }
    }

    /* -- RUNG 4: VENT ----------------------------------------------------- */
    for weapon_id in &mount_weapons {
        let Some(action) = content.action(weapon_id) else {
            continue;
        };
        if action
            .effects
            .iter()
            .any(|e| matches!(e, Effect::VENT_HEAT { .. }))
        {
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

/// C2 (#35): the set of cells already threatened by allies who committed EARLIER
/// in THIS decision pass — the "threat-spread" context for the current enemy's
/// FIRE-rung tie-breaker.
///
/// ## Why "earlier in initiative" specifically (the interleaved-loop subtlety)
///
/// `run_world_phase` processes enemies in [`enemy_initiative`] order,
/// fire-THEN-decide INTERLEAVED per enemy: each enemy fires (clearing its queue),
/// then `decide_enemy_action` re-populates it. So at the moment enemy *E* decides:
/// - enemies BEFORE *E* in initiative have already fired+re-decided THIS pass →
///   their `queue` holds this-pass intent (fresh). ✓ count these.
/// - enemies AFTER *E* have NOT been reached this iteration → their `queue` still
///   holds LAST phase's intent (stale). ✗ skip — spreading against stale intent
///   would be wrong.
///
/// So we take `enemy_initiative`, find `self_pos`'s index, and union the
/// threatened cells of the strictly-earlier enemies, computing each via
/// [`resolve_targeting_2d`] on that ally's queued action (the SAME single source
/// the shot + `ThreatMap` use — no parallel targeting path). Only DAMAGE-bearing
/// queued actions threaten cells (a queued move/reorient/vent threatens nothing).
/// Pure read; no new board state.
fn allies_threatened_cells(self_pos: Pos, board: &Board, content: &dyn Content) -> Vec<Pos> {
    let order = enemy_initiative(board);
    let dims = board.dims();
    // Index of the deciding enemy in the initiative order (by cell == pos index
    // under invariant A). If absent (shouldn't happen — it's a live enemy),
    // treat as "no earlier allies".
    let self_idx = order.iter().position(|&c| c == self_pos.to_index_in(dims));
    let Some(self_idx) = self_idx else {
        return Vec::new();
    };
    let mut out: Vec<Pos> = Vec::new();
    for &cell in &order[..self_idx] {
        let Some(ally_pos) = Pos::from_index_in(cell, dims) else {
            continue;
        };
        let Some(ally) = board.ship_at(ally_pos) else {
            continue;
        };
        // The ally's queued (this-pass) action ids. Usually one.
        let queued: Vec<String> = ally.queue.clone();
        for action_id in &queued {
            let Some(action) = content.action(action_id) else {
                continue;
            };
            // Only damaging actions threaten cells (move/reorient/vent don't).
            let deals_damage = action
                .effects
                .iter()
                .any(|e| matches!(e, Effect::DAMAGE { .. }));
            if !deals_damage {
                continue;
            }
            for c in resolve_targeting_2d(action, board, ally_pos) {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
    }
    out
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
    let min_allowed = bands
        .iter()
        .map(|b| band_ordinal(*b))
        .min()
        .unwrap_or(cur_o);
    let max_allowed = bands
        .iter()
        .map(|b| band_ordinal(*b))
        .max()
        .unwrap_or(cur_o);
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
const fn band_ordinal(r: Range) -> u8 {
    match r {
        Range::Adjacent => 0,
        Range::Near => 1,
        Range::Far => 2,
    }
}

/// Map a chosen [`Dir8`] step to the resolver-served synthetic move id. The AI
/// closes/opens via the SAME synthetic moves the player uses (resolver-served
/// through `resolver_ai_move`, so no `Content` dependency). All four cardinals
/// are served as of R6.
///
/// (#166 no-strafe) Since Bruce's rotate-then-forward ruling, the AI only ever
/// passes a CARDINAL here — the on-axis forward/reverse step it has already
/// committed to (perpendicular approaches become a ROTATE instead, never a slide;
/// see RUNG 2 / RUNG 3.5). The diagonal arms below are kept only so the mapping
/// stays total over `Dir8`; the move model no longer produces diagonal closes.
/// A diagonal would resolve its column component first. `None` only for the zero
/// vector (co-located, already excluded upstream by `from_to`).
const fn synthetic_move_for_dir(dir: Dir8) -> Option<&'static str> {
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

/// (#166) The dominant-component cardinal [`Dir4`] pointing from `from` toward
/// `to` — the "which way do I want to face to approach" used by the
/// rotate-then-forward maneuver. Resolves a diagonal to a SINGLE cardinal by the
/// larger axis delta (ties → the column axis E/W, the dodge axis), so a ship
/// rotates onto one approach heading and advances straight rather than sliding.
/// `None` only for the zero vector (co-located, excluded upstream).
///
/// Note this is intentionally distinct from [`grid::from_to`] (which yields the
/// 8-way octant, diagonals included): the no-strafe model needs a cardinal the
/// hull can FACE, so we collapse to 4-way here.
const fn dominant_cardinal(from: Pos, to: Pos) -> Option<crate::grid::Dir4> {
    use crate::grid::Dir4;
    let dc = (to.col as i32) - (from.col as i32);
    let dr = (to.row as i32) - (from.row as i32);
    if dc == 0 && dr == 0 {
        return None;
    }
    // |dc| >= |dr| -> horizontal dominates (ties pick the column/dodge axis).
    if dc.abs() >= dr.abs() {
        Some(if dc >= 0 { Dir4::E } else { Dir4::W })
    } else {
        Some(if dr >= 0 { Dir4::S } else { Dir4::N })
    }
}

/// (#166) The synthetic ROTATE id that turns `current` the SHORTEST quarter-turn
/// toward `target`, or `None` if already aligned. A 180°-opposite target picks
/// `__rotate_right` (either turn closes the gap equally; the next phase finishes
/// the about-face) — the SAME turn-choice convention as
/// [`rotate_to_make_weapon_bear`], so the maneuver and the rotate-to-bear rungs
/// spin a hull the same way. Resolver-served id (`resolver_ai_move`), no
/// `Content`-action dependency.
fn rotate_toward_cardinal(
    current: crate::grid::Dir4,
    target: crate::grid::Dir4,
) -> Option<&'static str> {
    use crate::input::{SYNTHETIC_ROTATE_LEFT, SYNTHETIC_ROTATE_RIGHT};
    if current == target {
        None // already facing the approach — caller should advance, not spin
    } else if current.rotate_cw() == target {
        Some(SYNTHETIC_ROTATE_RIGHT)
    } else if current.rotate_ccw() == target {
        Some(SYNTHETIC_ROTATE_LEFT)
    } else {
        // 180° off: either quarter-turn reduces the gap; finish next phase.
        Some(SYNTHETIC_ROTATE_RIGHT)
    }
}

/// Q3 (#86) generalized to ARC-AGNOSTIC rotate-to-bear (#92): pick the synthetic
/// ROTATE id that turns the enemy the SHORTEST way to a `Bow` facing from which
/// its OWN weapon BEARS on the player — bow-on for a Forward gun, SIDE-on for a
/// `BroadsideArc` gun, etc. `None` if it already bears (let the close/hold rungs
/// act) or no facing bears (no point spinning).
///
/// ## Single source of truth (no bow-vs-broadside hardcode)
///
/// "Does my weapon bear from facing F?" is answered by the SAME
/// `resolve_targeting_2d` the shot fires through (V4): we probe each candidate
/// [`Dir4`] `Bow(F)` by temporarily setting the enemy's `facing`, running
/// `resolve_targeting_2d` for its dominant weapon, and checking whether the
/// player's cell is in the result — then RESTORE the facing (pure probe, no
/// lasting board mutation). This is why a `BroadsideArc` enemy will orient
/// PERPENDICULAR to the player (its `arc_bears` only returns true for a
/// `Broadside` stance whose axis is across the bearing) while a Forward enemy
/// orients bow-on — all from the one targeting path, never a hardcoded stance
/// rule. (We only rotate among the four `Bow` cardinals; the resolver's
/// `REORIENT::RotateLeft/Right` cycle the bow, and `orientation_from_facing` maps
/// Bow(E)/Bow(W) to a Broadside `orientation`, so a quarter-turn into the E/W
/// bow IS the broadside stance the arc test wants.)
///
/// Turn choice: shortest quarter-turn (CW=`__rotate_right`, CCW=`__rotate_left`)
/// onto the nearest bearing facing; a 180°-only target picks `__rotate_right`
/// (next phase finishes the turn). Resolver-served id (`resolver_ai_move`), so no
/// Content-action dependency for the rotate itself.
fn rotate_to_make_weapon_bear(
    mount_weapons: &[String],
    enemy_pos: Pos,
    player_pos: Pos,
    board: &mut Board,
    content: &dyn Content,
) -> Option<&'static str> {
    use crate::grid::{Dir4, Facing};
    use crate::input::{SYNTHETIC_ROTATE_LEFT, SYNTHETIC_ROTATE_RIGHT};

    // Dominant weapon (highest summed DAMAGE) — the one we want to bring to bear.
    // Cloned so we don't hold a `content` borrow across the board mutation below.
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
        })?
        .clone();

    // Current bow cardinal (only meaningful for a Bow stance; a Broadside stance
    // is reached via the E/W bow cardinals, so treat its forward axis cardinal as
    // the current bow-equivalent for shortest-turn math).
    let current: Dir4 = match board.ship_at(enemy_pos)?.facing {
        Facing::Bow(d) => d,
        Facing::Broadside(axis) => axis.dirs().0,
    };

    // Probe each of the 4 Bow facings: does `dominant` bear on the player from it?
    // Temporarily set facing, test via the single source, restore.
    let saved = board.ship_at(enemy_pos)?.facing;
    let bears_from = |board: &mut Board, dir: Dir4| -> bool {
        if let Some(s) = board.ship_at_mut(enemy_pos) {
            s.facing = Facing::Bow(dir);
        }
        let hits = resolve_targeting_2d(&dominant, board, enemy_pos).contains(&player_pos);
        if let Some(s) = board.ship_at_mut(enemy_pos) {
            s.facing = saved;
        }
        hits
    };

    // If the current facing already bears, don't spin (shouldn't reach here —
    // Rung 1 would have fired — but guard anyway).
    if bears_from(board, current) {
        return None;
    }

    // Among the bearing facings, pick the one needing the FEWEST quarter-turns.
    let cw1 = current.rotate_cw();
    let ccw1 = current.rotate_ccw();
    let opp = current.opposite();
    if bears_from(board, cw1) {
        Some(SYNTHETIC_ROTATE_RIGHT)
    } else if bears_from(board, ccw1) {
        Some(SYNTHETIC_ROTATE_LEFT)
    } else if bears_from(board, opp) {
        // 180° off — either quarter-turn reduces the gap; finish next phase.
        Some(SYNTHETIC_ROTATE_RIGHT)
    } else {
        None // no facing brings this weapon to bear (e.g. out of band) — hold
    }
}
