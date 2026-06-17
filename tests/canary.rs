//! #41 winnability CANARY — the 2-D campaign is winnable against enemies that
//! shoot back.
//!
//! ## Why this file exists (the gap it closes)
//!
//! `tests/run_loop.rs` already proves the generated campaign plays through to
//! `Victorious` (`generated_spawn_pool_campaign_plays_through_to_victory`) — but
//! against **mountless target** enemies (`weak_enemy`: no mounts, no AI threat).
//! That confirms the loop/spawn/advance machinery and the player's 2-D
//! fire-gate, and it was the right call for a deterministic campaign-shape test.
//! It does NOT, however, prove the harder thing the word *winnable* implies: that
//! the player can out-trade enemies **that fire back** — a campaign of inert
//! targets is winnable by definition.
//!
//! This file fills that gap. It drives the REAL resolver round-loop
//! ([`resolve_round`], which runs `fire_player_queue` then the world phase:
//! ordnance advance + each enemy's AI [`decide_enemy_action`] + its fire +
//! end-of-turn) on a 2-D invariant-A board where the enemies are **armed and
//! AI-driven**, and asserts the player clears them and survives within a bound.
//!
//! ## The stalemate diagnosis this confirms (memory: the generated_spawn_pool
//! "stalemate" was a 1-D test-driver artifact, NOT the engine)
//!
//! The earlier cap-timeout came from the 1-D test player-driver, which read
//! `cell`/`LaneEnd` and could not aim or close on a 2-D board, so the player
//! never connected and the fight ran to the round cap. With a 2-D-aware driver
//! (fire-gated on the SAME `resolve_targeting_2d` the shot fires through, strafe
//! to line up the column, close to enter band) the player wins — against live
//! fire, not just inert targets. If THIS is green, the engine resolves a real
//! two-sided 2-D fight to a player win; a future cap-timeout would be a harness
//! or balance issue, not an unwinnable engine.
//!
//! Content's lane: combat winnability (#41). Kept in its own file (not
//! `run_loop.rs`, the tester's) to avoid a shared-file edit race.

use broadside_engine::grid::{self, Dir4, Facing, Pos};
use broadside_engine::resolve::{resolve_round, resolve_targeting_2d, Content};
use broadside_engine::runs::{encounter_outcome, EncounterOutcome};
use broadside_engine::types::{
    Action, ActionCost, Arc, Effect, Faction, Projectile, RangeBand, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};

mod common;
use common::{board_2d, naked_shields, ship_2d};

/* =========================================================================
 * Content — a player beam + an enemy beam, both real-shaped (2-D bands).
 * ====================================================================== */

/// A forward beam. `raw` damage, all three 2-D bands so range never gates the
/// shot, Forward-arc, no falloff so the landed number is legible.
fn beam(id: &str, raw: i32) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![grid::Range::Adjacent, grid::Range::Near, grid::Range::Far],
            optimal_range: grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: raw, band_falloff: Some(false) }],
        r#mod: None,
        icon: None,
    }
}

/// Serves the player's heavy beam ("pc_beam") and the enemies' lighter beam
/// ("ai_beam"). The enemy AI (`decide_enemy_action`) reaches for the mount's
/// weapon id and fire-gates it through `resolve_targeting_2d`, so the enemies
/// genuinely shoot back when they bear.
struct CanaryContent {
    pc_beam: Action,
    ai_beam: Action,
}
impl CanaryContent {
    fn new(pc_raw: i32, ai_raw: i32) -> Self {
        CanaryContent { pc_beam: beam("pc_beam", pc_raw), ai_beam: beam("ai_beam", ai_raw) }
    }
}
impl Content for CanaryContent {
    fn action(&self, id: &str) -> Option<&Action> {
        match id {
            "pc_beam" => Some(&self.pc_beam),
            "ai_beam" => Some(&self.ai_beam),
            _ => None,
        }
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("canary scenarios fire beams, not ordnance");
    }
}

/* =========================================================================
 * Player driver — a 2-D playstyle, fire-gated on the single source.
 * ====================================================================== */

/// Choose + queue the player's action for one round in 2-D. Mirrors the
/// run_loop harness driver (#25): FIRE when the gun already bears on a hostile
/// (gated by the SAME `resolve_targeting_2d` the shot fires through, so "decided
/// to fire" == "connects"), else strafe to line up the nearest enemy's column,
/// else close toward the back rows. A mountless/idle player is left alone.
fn queue_player_action(board: &mut broadside_engine::types::Board, content: &dyn Content) {
    use broadside_engine::input::{SYNTHETIC_MOVE_LEFT, SYNTHETIC_MOVE_RIGHT, SYNTHETIC_MOVE_UP};

    let Some((ppos, weapon_id)) = board.cells.iter().flatten().find_map(|s| {
        (s.faction == Faction::Player)
            .then(|| s.mounts.first().map(|m| (s.pos, m.weapon.clone())))
            .flatten()
    }) else {
        return;
    };

    // Nearest live enemy by Chebyshev distance.
    let Some(epos) = board
        .cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .map(|s| s.pos)
        .min_by_key(|&e| grid::distance(ppos, e))
    else {
        return; // no enemies left
    };

    // FIRE-GATE (single source): does the weapon already bear on a hostile?
    let bears_on_hostile = content
        .action(&weapon_id)
        .map(|w| {
            resolve_targeting_2d(w, board, ppos).iter().any(|&p| {
                board
                    .cells
                    .get(p.to_index())
                    .and_then(|c| c.as_ref())
                    .map(|s| s.faction != Faction::Player)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    let action = if bears_on_hostile {
        weapon_id
    } else if epos.col != ppos.col {
        // Wrong column → strafe to line up the enemy's column.
        if epos.col < ppos.col { SYNTHETIC_MOVE_LEFT } else { SYNTHETIC_MOVE_RIGHT }.to_string()
    } else {
        // Same column but can't fire (out of band / facing away) → close N
        // toward the back-row enemies.
        SYNTHETIC_MOVE_UP.to_string()
    };

    if let Some(p) = board.cells[ppos.to_index()].as_mut() {
        p.queue = vec![action];
    }
}

fn enemies_left(b: &broadside_engine::types::Board) -> usize {
    b.cells.iter().flatten().filter(|s| s.faction == Faction::Enemy).count()
}

fn player_alive(b: &broadside_engine::types::Board) -> bool {
    b.cells.iter().flatten().any(|s| s.faction == Faction::Player)
}

/* =========================================================================
 * The canary.
 * ====================================================================== */

/// The headline #41 proof: a 2-D-driven player clears TWO armed, AI-driven
/// enemies that fire back, end-to-end through the real `resolve_round`, and
/// survives. This is winnability against live fire — not inert targets.
#[test]
fn player_clears_armed_ai_enemies_in_2d_and_survives() {
    // Player front-centre (2,3) bow N (Forward gun bears up its column toward
    // the back rows), tanky + naked so every landed shot is observable and it
    // out-lasts return fire. Heavy beam: 6 raw, one shot kills a naked hull-3
    // enemy at any band (falloff off).
    let mut player = ship_2d("p", Faction::Player, Pos::new(2, 3), 40, Facing::Bow(Dir4::N), Arc::Forward, "pc_beam");
    player.shield_profile = naked_shields();

    // Two armed enemies in the back rows, bow S (facing the player → their
    // Forward gun bears DOWN-column toward the player). They fire back via the
    // AI. Light beam (2 raw) + low hull (3) so the player out-trades them. One
    // sits in the player's column (2,0) — an immediate fire target; the other
    // off-column (4,1) so the driver must strafe to line it up (exercising the
    // lateral-aim path, not just a stationary duel).
    let mut e1 = ship_2d("e1", Faction::Enemy, Pos::new(2, 0), 3, Facing::Bow(Dir4::S), Arc::Forward, "ai_beam");
    let mut e2 = ship_2d("e2", Faction::Enemy, Pos::new(4, 1), 3, Facing::Bow(Dir4::S), Arc::Forward, "ai_beam");
    e1.shield_profile = naked_shields();
    e2.shield_profile = naked_shields();

    let mut board = board_2d(vec![player, e1, e2]);
    let content = CanaryContent::new(6, 2);

    // Drive the real round-loop: queue the player's 2-D action, then
    // resolve_round fires it + runs the world phase (enemy AI fires back +
    // end-of-turn). Bounded — hitting the cap is itself a failure.
    let cap = 40;
    let mut rounds = 0;
    while enemies_left(&board) > 0 && player_alive(&board) && rounds < cap {
        queue_player_action(&mut board, &content);
        resolve_round(&mut board, &content);
        rounds += 1;
    }

    assert!(rounds < cap, "#41: the armed 2-D fight must terminate, not run to the cap");
    assert_eq!(
        enemies_left(&board),
        0,
        "#41: the player clears BOTH armed AI enemies (winnable vs live fire), got {} left after {rounds} rounds",
        enemies_left(&board),
    );
    assert!(player_alive(&board), "#41: the player survives the win");
    // The run-loop's own win predicate agrees (the signal the bin's win-branch
    // keys off), so this is a real campaign-terminating win.
    assert_eq!(
        encounter_outcome(&board),
        EncounterOutcome::Won,
        "#41: encounter_outcome reports Won — the campaign-terminating signal fires",
    );
}

/// Sibling guard: a single in-column duel resolves to a win in a small bound.
/// Keeps a tight regression on the "player fires + connects + kills, vs a
/// shooter" core in case the multi-enemy strafing test is ever perturbed by a
/// tuning change — this one has no lateral-aim dependency.
#[test]
fn player_out_trades_a_single_armed_enemy_in_column() {
    let mut player = ship_2d("p", Faction::Player, Pos::new(1, 3), 30, Facing::Bow(Dir4::N), Arc::Forward, "pc_beam");
    player.shield_profile = naked_shields();
    let mut e = ship_2d("e", Faction::Enemy, Pos::new(1, 0), 4, Facing::Bow(Dir4::S), Arc::Forward, "ai_beam");
    e.shield_profile = naked_shields();
    let mut board = board_2d(vec![player, e]);
    let content = CanaryContent::new(6, 1);

    let cap = 16;
    let mut rounds = 0;
    while enemies_left(&board) > 0 && player_alive(&board) && rounds < cap {
        queue_player_action(&mut board, &content);
        resolve_round(&mut board, &content);
        rounds += 1;
    }

    assert!(rounds < cap, "the in-column duel terminates");
    assert_eq!(enemies_left(&board), 0, "player kills the in-column shooter");
    assert!(player_alive(&board), "player survives a one-on-one against a weaker shooter");
}
