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
//! ## The stalemate diagnosis this confirms (memory: the `generated_spawn_pool`
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
use broadside_engine::resolve::{
    live_enemy_ids, resolve_round, resolve_targeting_2d, tick_enemy, tick_world, Content,
};
use broadside_engine::runs::{encounter_outcome, EncounterOutcome};
use broadside_engine::types::{
    Action, ActionCost, Arc, Effect, Faction, Projectile, RangeBand, Ship, Targeting,
    TargetingPattern, Trait, WeaponArchetype,
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
        cost: ActionCost {
            heat: 1,
            cooldown_max: 0,
            advances_turn: true,
        },
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
        effects: vec![Effect::DAMAGE {
            amount: raw,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Serves the player's heavy beam ("`pc_beam`") and the enemies' lighter beam
/// ("`ai_beam`"). The enemy AI (`decide_enemy_action`) reaches for the mount's
/// weapon id and fire-gates it through `resolve_targeting_2d`, so the enemies
/// genuinely shoot back when they bear.
struct CanaryContent {
    pc_beam: Action,
    ai_beam: Action,
}
impl CanaryContent {
    fn new(pc_raw: i32, ai_raw: i32) -> Self {
        Self {
            pc_beam: beam("pc_beam", pc_raw),
            ai_beam: beam("ai_beam", ai_raw),
        }
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
/// `run_loop` harness driver (#25): FIRE when the gun already bears on a hostile
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
        .is_some_and(|w| {
            resolve_targeting_2d(w, board, ppos).iter().any(|&p| {
                board
                    .cells
                    .get(p.to_index())
                    .and_then(|c| c.as_ref())
                    .is_some_and(|s| s.faction != Faction::Player)
            })
        });

    let action = if bears_on_hostile {
        weapon_id
    } else if epos.col != ppos.col {
        // Wrong column → strafe to line up the enemy's column.
        if epos.col < ppos.col {
            SYNTHETIC_MOVE_LEFT
        } else {
            SYNTHETIC_MOVE_RIGHT
        }
        .to_string()
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
    b.cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .count()
}

fn player_alive(b: &broadside_engine::types::Board) -> bool {
    b.cells
        .iter()
        .flatten()
        .any(|s| s.faction == Faction::Player)
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
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields();

    // Two armed enemies in the back rows, bow S (facing the player → their
    // Forward gun bears DOWN-column toward the player). They fire back via the
    // AI. Light beam (2 raw) + low hull (3) so the player out-trades them. One
    // sits in the player's column (2,0) — an immediate fire target; the other
    // off-column (4,1) so the driver must strafe to line it up (exercising the
    // lateral-aim path, not just a stationary duel).
    let mut e1 = ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 0),
        3,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let mut e2 = ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(4, 1),
        3,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
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

    assert!(
        rounds < cap,
        "#41: the armed 2-D fight must terminate, not run to the cap"
    );
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
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(1, 3),
        30,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields();
    let mut e = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(1, 0),
        4,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
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
    assert_eq!(
        enemies_left(&board),
        0,
        "player kills the in-column shooter"
    );
    assert!(
        player_alive(&board),
        "player survives a one-on-one against a weaker shooter"
    );
}

/// Q3 PAYOFF (#86): a Forward-arc enemy whose bow points the WRONG WAY (away from
/// the player) must ROTATE to bring its gun to bear and THEN FIRE — not spin in
/// place or mash the player's cell forever (the pre-#86 "camp + never shoot"
/// bug). Drives a stationary player (never fires) so the ONLY way its hull drops
/// is the enemy turning to bear and shooting. Bounded; the enemy must connect.
#[test]
fn q3_misfacing_enemy_rotates_to_bear_then_fires() {
    // Enemy at (2,1) Bow(N) — bow points UP/away from the player at (2,3) due
    // SOUTH, same column, distance 2 (Near, in the ai_beam's band). A Forward gun
    // bears ONLY out the bow (N), so it does NOT bear south on the player. To
    // engage, the enemy must rotate its bow to S (two quarter-turns) — strafing
    // can't help (already the player's column). Once Bow(S), the gun bears down
    // the column and fires.
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        9,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "ai_beam",
    );
    enemy.shield_profile = naked_shields();
    // Player: tanky, naked (so a landed shot shows in hull), and it NEVER fires
    // (we don't queue it) — isolates "did the enemy turn + shoot?".
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields();
    let mut board = board_2d(vec![enemy, player]);
    let content = CanaryContent::new(6, 2);

    let player_hull_0 = 40;
    let mut rotated_seen = false;
    let cap = 12;
    let mut rounds = 0;
    while rounds < cap {
        // Player does NOT queue — only the world phase (enemy AI) acts.
        resolve_round(&mut board, &content);
        rounds += 1;
        // Observe the enemy's facing: it should turn toward S (toward the player).
        if let Some(e) = board.cells.iter().flatten().find(|s| s.id == "e") {
            if matches!(e.facing, Facing::Bow(Dir4::S)) {
                rotated_seen = true;
            }
        }
        // Stop once the enemy has connected.
        let p_hull = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.id == "p")
            .map_or(0, |s| s.hull);
        if p_hull < player_hull_0 {
            break;
        }
    }

    assert!(
        rotated_seen,
        "#86: the enemy must ROTATE its bow toward the player (saw Bow(S))"
    );
    let p_hull = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == "p")
        .map_or(0, |s| s.hull);
    assert!(
        p_hull < player_hull_0,
        "#86: after rotating to bear, the enemy must FIRE + connect (player hull {p_hull} < {player_hull_0}); it must not spin/mash forever",
    );
    // And it didn't waste the whole window: connected within the bound.
    assert!(
        rounds < cap,
        "#86: enemy rotates + fires within {cap} rounds, not stuck"
    );
}

/* =========================================================================
 * #92 BROADSIDE payoff (Model D): a BroadsideArc weapon bears off the bow's
 * PERPENDICULAR flanks. Verify (1) a broadside-armed enemy orients flank-to-
 * player + lands a shot (rotates side-on, doesn't bow-rush/spin), and (2) the
 * player at bow-E fires a broadside up-lane.
 * ====================================================================== */

/// A `BroadsideArc` beam. `raw` damage, all 2-D bands (range never gates), no
/// falloff. Bears out the flanks perpendicular to the bow (Model D).
fn broadside_beam(id: &str, raw: i32) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
        archetype: WeaponArchetype::Broadside,
        cost: ActionCost {
            heat: 1,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            range_band: vec![grid::Range::Adjacent, grid::Range::Near, grid::Range::Far],
            optimal_range: grid::Range::Adjacent,
            pattern: TargetingPattern::BROADSIDE,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::BroadsideArc),
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE {
            amount: raw,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Serves one broadside weapon by id for the #92 payoff scenarios.
struct BroadsideContent(Action);
impl Content for BroadsideContent {
    fn action(&self, id: &str) -> Option<&Action> {
        (id == self.0.id).then_some(&self.0)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("broadside scenarios fire a beam, not ordnance");
    }
}

#[test]
fn q92_broadside_enemy_orients_flank_to_player_then_fires() {
    // Enemy at (2,1) Bow(E) — its bow points E, straight AT the player at (4,1)
    // due east (distance 2 = Near). A BroadsideArc bears off the PERPENDICULAR
    // flanks (N/S here), NOT the bow axis (E/W), so it does NOT bear on the
    // eastward player yet. To fire, the enemy must rotate its bow to N or S so a
    // FLANK faces east — i.e. orient SIDE-on, not bow-on. Then the broadside
    // bears down the row and fires. Stationary player (never queues) isolates
    // "enemy turned side-on + shot".
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        9,
        Facing::Bow(Dir4::E),
        Arc::BroadsideArc,
        "bcannon",
    );
    enemy.shield_profile = naked_shields();
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(4, 1),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "noop",
    );
    player.shield_profile = naked_shields();
    player.mounts.clear(); // can't fire back; pure target
    let mut board = board_2d(vec![enemy, player]);
    let content = BroadsideContent(broadside_beam("bcannon", 5));

    let player_hull_0 = 40;
    let mut side_on_seen = false;
    let cap = 12;
    let mut rounds = 0;
    while rounds < cap {
        resolve_round(&mut board, &content);
        rounds += 1;
        if let Some(e) = board.cells.iter().flatten().find(|s| s.id == "e") {
            // SIDE-on to an east player = bow turned off the E/W axis, i.e. N or S.
            if matches!(e.facing, Facing::Bow(Dir4::N | Dir4::S)) {
                side_on_seen = true;
            }
        }
        if board
            .cells
            .iter()
            .flatten()
            .find(|s| s.id == "p")
            .map_or(0, |s| s.hull)
            < player_hull_0
        {
            break;
        }
    }

    assert!(
        side_on_seen,
        "#92: the broadside enemy must orient SIDE-on (bow N/S) so a flank faces the east player"
    );
    let p_hull = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == "p")
        .map_or(0, |s| s.hull);
    assert!(
        p_hull < player_hull_0,
        "#92: after going side-on, the broadside enemy FIRES + connects (player hull {p_hull} < {player_hull_0}); not spin/bow-rush",
    );
    assert!(
        rounds < cap,
        "#92: broadside enemy orients + fires within {cap} rounds"
    );
}

#[test]
fn q92_player_bow_ew_fires_broadside_up_lane() {
    // Direct firing check (Model D): a player in bow-E stance fires a BroadsideArc
    // weapon out its N/S flanks. Player at (2,3) Bow(E); a target due NORTH up the
    // column at (2,1) (distance 2). The broadside bears N (a flank) → the shot
    // resolves on the target. Proves the player gets the broadside hook from a bow
    // cardinal (no separate stance needed).
    use broadside_engine::resolve::resolve_targeting_2d;
    let player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        30,
        Facing::Bow(Dir4::E),
        Arc::BroadsideArc,
        "bcannon",
    );
    let target = ship_2d(
        "t",
        Faction::Enemy,
        Pos::new(2, 1),
        5,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    let board = board_2d(vec![player, target]);
    let content = BroadsideContent(broadside_beam("bcannon", 5));

    let cells = resolve_targeting_2d(content.action("bcannon").unwrap(), &board, Pos::new(2, 3));
    assert!(
        cells.contains(&Pos::new(2, 1)),
        "#92: a bow-E player's broadside bears N (a flank) on the up-column target; got {cells:?}",
    );
}

/* =========================================================================
 * #103/#104 SHIELD-POOL payoff: the headline of the combat-model overhaul —
 * a per-face shield POOL that DEPLETES on the hit face, overflows to hull once
 * empty, and RECHARGES only on faces that did NOT take fire this round (the
 * under-fire pause). Integer throughout. Locks the live behavior the way the
 * demo_scenarios pin the directional claim, so a future edit that re-breaks
 * the live path (e.g. reverting apply_damage_2d back to the 1-D absorb) fails
 * here, not silently in-game.
 * ====================================================================== */

/// A bow shield pool with `cap` capacity, full; the other faces empty. Lets a
/// southward shot land on a Bow(S) ship's bow pool and watch it deplete.
const fn bow_pool(cap: i32) -> broadside_engine::types::ShieldProfile {
    use broadside_engine::types::{ShieldFace, ShieldProfile};
    ShieldProfile {
        bow: ShieldFace {
            armour: cap,
            charge: cap,
        },
        stern: ShieldFace {
            armour: 0,
            charge: 0,
        },
        port: ShieldFace {
            armour: 0,
            charge: 0,
        },
        starboard: ShieldFace {
            armour: 0,
            charge: 0,
        },
    }
}

#[test]
fn shield_pool_depletes_then_overflows_to_hull_under_sustained_fire() {
    // Player at (2,3) Bow(N) fires pc_beam (raw 4, falloff OFF) straight up the
    // column at an enemy at (2,1) Bow(S) — the shot lands on the enemy's BOW.
    // The enemy carries a bow POOL of 3 (other faces empty) and hull 10. The
    // player NEVER takes fire (enemy is mountless), so only the enemy's shields
    // matter.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        10,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    enemy.shield_profile = bow_pool(3);
    enemy.mounts.clear(); // pure target; never fires back
    enemy.traits = vec![Trait::Anchored]; // hold position so the shot keeps hitting the bow (stable geometry)
    let mut board = board_2d(vec![player, enemy]);
    let content = CanaryContent::new(4, 1);

    let bow_charge = |b: &broadside_engine::types::Board| -> i32 {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .unwrap()
            .shield_profile
            .bow
            .charge
    };
    let hull = |b: &broadside_engine::types::Board| -> i32 {
        b.cells.iter().flatten().find(|s| s.id == "e").unwrap().hull
    };

    // Round 1: the 4-raw hit lands on the bow pool (3) -> soak 3, pool 0, overflow
    // 1 -> hull 9. The bow took fire this round so the under-fire pause holds it
    // at 0 (no end-of-turn regen).
    queue_player_action(&mut board, &content);
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        0,
        "bow pool drained by the hit; under-fire pause = no regen"
    );
    assert_eq!(
        hull(&board),
        9,
        "1 overflow past the emptied pool reaches hull"
    );

    // Round 2: pool 0, the full 4 overflows -> hull 5. Bow still under fire, still
    // pinned at 0 (this is the regression the pause must hold — a hit on an empty
    // pool still counts as "under fire").
    queue_player_action(&mut board, &content);
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        0,
        "empty pool stays empty under continued fire (pause holds even with 0 to absorb)"
    );
    assert_eq!(
        hull(&board),
        5,
        "full hit reaches hull once the pool is gone (4 -> hull 5)"
    );
}

#[test]
fn shield_pool_recharges_when_a_face_stops_taking_fire() {
    // Same enemy bow pool (3), but the player only fires ONCE then stops, so the
    // bow face goes quiet and recharges +1/turn back toward its capacity. Proves
    // the "recharge" half of the model (the under-fire pause lifts on a quiet
    // turn). Enemy is mountless so the player is never hit.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        10,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    enemy.shield_profile = bow_pool(3);
    enemy.mounts.clear();
    enemy.traits = vec![Trait::Anchored]; // hold position (stable geometry across the quiet turns)
    let mut board = board_2d(vec![player, enemy]);
    let content = CanaryContent::new(2, 1); // raw 2: drains the pool but leaves hull

    let bow_charge = |b: &broadside_engine::types::Board| -> i32 {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .unwrap()
            .shield_profile
            .bow
            .charge
    };

    // Round 1: fire raw 2 onto the bow pool (3) -> soak 2 -> pool 1, 0 overflow.
    // Bow took fire -> no regen this round. Pool ends at 1.
    queue_player_action(&mut board, &content);
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        1,
        "raw-2 hit drains the 3-pool to 1 (no regen the turn it's hit)"
    );

    // Rounds 2-3: the player does NOT fire (we don't queue it), so the bow face
    // is quiet and recharges +1/turn toward its capacity (3): 1 -> 2 -> 3.
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        2,
        "quiet bow face recharges +1 (1 -> 2)"
    );
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        3,
        "recharge continues up to capacity (2 -> 3)"
    );

    // Round 4: at capacity, regen clamps (does not overfill past 3).
    resolve_round(&mut board, &content);
    assert_eq!(
        bow_charge(&board),
        3,
        "recharge clamps at capacity, never overfills"
    );
}

/* =========================================================================
 * "WEAPONS DO NOTHING" GUARD: the REAL catalog pulse_laser, fired through the
 * LIVE content path (DemoContent::install_catalog_actions — the exact wiring
 * the bin's build_content uses) + the resolver, MUST damage and kill an
 * in-range enemy. Plus the spawn-range fact that explains the player's
 * perception: pulse is a CLOSE weapon (bands Adjacent/Near, max reach dist 2),
 * so at the spawn distance (front row -> back row = 3 = Far) it does NOT reach
 * and the player must close first. This is the regression guard for Bruce's
 * "my weapons do nothing to enemies" report — if catalog damage ever stops
 * landing through the live path, this fails loudly.
 * ====================================================================== */
#[test]
fn real_pulse_laser_kills_an_in_range_enemy_via_the_live_catalog_path() {
    use broadside_engine::input::DemoContent;
    // Build the LIVE content: load the real catalog asset + merge its actions,
    // exactly like the bin's build_content does (#49a wiring).
    let bytes = std::fs::read("assets/broadside.catalog.json").expect("catalog asset present");
    let catalog = broadside_engine::catalog::load_from_bytes(&bytes).expect("catalog parses");
    let mut content = DemoContent::default();
    content.install_catalog_actions(&catalog);

    // The real pulse_laser: beam, raw 4 (inflate_effect: heat 2 -> heat+2), 2-D
    // bands [Adjacent, Near], Forward arc.
    let pulse = content
        .action("pulse_laser")
        .expect("pulse_laser is a real catalog action")
        .clone();
    let raw: i32 = pulse
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::DAMAGE { amount, .. } => Some(*amount),
            _ => None,
        })
        .sum();
    assert_eq!(
        raw, 4,
        "real catalog pulse_laser deals raw 4 (beam: heat 2 + 2)"
    );
    assert!(
        pulse.targeting.range_band.contains(&grid::Range::Adjacent)
            && pulse.targeting.range_band.contains(&grid::Range::Near),
        "pulse_laser is a CLOSE weapon: bands Adjacent + Near; got {:?}",
        pulse.targeting.range_band,
    );

    // Player at (2,2) Bow(N) with pulse_laser; light enemy at (2,1) Bow(S) one
    // cell N = ADJACENT (dist 1), bearing in the Forward arc. Enemy bow pool 2,
    // hull 3 (the light-enemy matchup). Anchored so the geometry is stable.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 2),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pulse_laser",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        3,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    enemy.shield_profile = bow_pool(2);
    enemy.mounts.clear();
    enemy.traits = vec![Trait::Anchored];
    let mut board = board_2d(vec![player, enemy]);

    let estate = |b: &broadside_engine::types::Board| -> Option<(i32, i32)> {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .map(|s| (s.hull, s.shield_profile.bow.charge))
    };

    // Commit 1 (Adjacent, -0 falloff): 4 raw -> bow pool 2 soaks 2 -> 2 overflow
    // -> enemy hull 3 -> 1, pool 0.
    board
        .cells
        .iter_mut()
        .flatten()
        .find(|s| s.id == "p")
        .unwrap()
        .queue = vec!["pulse_laser".into()];
    resolve_round(&mut board, &content);
    assert_eq!(
        estate(&board),
        Some((1, 0)),
        "commit 1: pool 2 soaks 2 of the 4, 2 overflows to hull (3 -> 1)"
    );

    // Commit 2: pool 0 -> full 4 to hull -> enemy dies.
    board
        .cells
        .iter_mut()
        .flatten()
        .find(|s| s.id == "p")
        .unwrap()
        .queue = vec!["pulse_laser".into()];
    resolve_round(&mut board, &content);
    assert!(estate(&board).is_none(), "commit 2: the empty-pool hull takes the full 4 -> enemy destroyed (live path damage is REAL)");

    // The spawn-range fact: front-row player -> back-row enemy is Chebyshev 3 =
    // Far, which is OUTSIDE pulse's [Adjacent, Near] band set, so the player must
    // CLOSE before pulse_laser connects (it is NOT a damage bug). Both the player
    // and the first-encounter enemies (skiff/lancer) mount Pulse Laser, so the
    // SAME must-close gate applies to the enemy AI — it closes via its ladder.
    assert_eq!(
        grid::distance(Pos::new(2, 3), Pos::new(2, 0)),
        3,
        "spawn front->back row is dist 3"
    );
    assert!(
        !pulse.targeting.range_band.contains(&grid::Range::Far),
        "pulse does NOT reach Far: at spawn distance the player must close first (the perceived 'weapons do nothing')",
    );
}

/* =========================================================================
 * #124/#125 REAL-TIME DECOUPLE seam: tick_enemy fires ONE enemy independent of
 * the player + other enemies; tick_world runs the global bookkeeping. These lock
 * the decoupling contract render's bin clock wires to.
 * ====================================================================== */

#[test]
fn tick_enemy_fires_only_that_enemy_and_leaves_the_player_alone() {
    use broadside_engine::input::DemoContent;
    let bytes = std::fs::read("assets/broadside.catalog.json").expect("catalog asset");
    let catalog = broadside_engine::catalog::load_from_bytes(&bytes).expect("catalog parses");
    let mut content = DemoContent::default();
    content.install_catalog_actions(&catalog);

    // Player at (2,2) Bow(N), naked, with a QUEUED pulse_laser it has NOT fired.
    // Two enemies down-column both mounting pulse_laser, bearing on the player at
    // Adjacent/Near (so they CAN fire). e1 at (2,1) dist 1, e2 at (2,0) dist 2.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 2),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pulse_laser",
    );
    player.shield_profile = naked_shields();
    player.queue = vec!["pulse_laser".into()]; // queued, must NOT be fired by tick_enemy
    let mut e1 = ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 1),
        9,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "pulse_laser",
    );
    let mut e2 = ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(2, 0),
        9,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "pulse_laser",
    );
    e1.shield_profile = naked_shields();
    e2.shield_profile = naked_shields();
    let mut board = board_2d(vec![player, e1, e2]);

    let hull = |b: &broadside_engine::types::Board, id: &str| -> i32 {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == id)
            .map_or(-1, |s| s.hull)
    };
    let queue_len = |b: &broadside_engine::types::Board, id: &str| -> usize {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == id)
            .map_or(0, |s| s.queue.len())
    };

    // First tick e1: telegraph-one-turn-ahead means tick 1 only DECIDES (its
    // queue was empty), no fire yet. Tick e1 AGAIN: now it fires its telegraph.
    tick_enemy("e1", &mut board, &content);
    assert_eq!(
        hull(&board, "p"),
        40,
        "tick 1 only telegraphs (empty queue) -> player untouched yet"
    );
    assert!(
        queue_len(&board, "e1") >= 1,
        "e1 telegraphed its next action"
    );
    // e2 has NOT been ticked: it must be completely untouched (no decide, no fire).
    assert_eq!(
        queue_len(&board, "e2"),
        0,
        "ticking e1 did NOT decide for e2 (per-enemy isolation)"
    );

    let p_before = hull(&board, "p");
    tick_enemy("e1", &mut board, &content);
    assert!(hull(&board, "p") < p_before, "second e1 tick FIRES its telegraph -> player hull drops (enemy acts independent of player commit)");

    // Through all of this the PLAYER's queued pulse_laser was never fired by
    // tick_enemy -> both enemies still alive at full hull.
    assert_eq!(
        hull(&board, "e1"),
        9,
        "player's queued shot was NOT fired by tick_enemy (e1 full)"
    );
    assert_eq!(
        hull(&board, "e2"),
        9,
        "player's queued shot was NOT fired by tick_enemy (e2 full)"
    );
    assert!(
        queue_len(&board, "p") >= 1,
        "the player's queued action is still pending (only the player's commit fires it)"
    );
}

#[test]
fn tick_world_runs_global_bookkeeping_without_firing_queues() {
    use broadside_engine::input::DemoContent;
    let bytes = std::fs::read("assets/broadside.catalog.json").expect("catalog asset");
    let catalog = broadside_engine::catalog::load_from_bytes(&bytes).expect("catalog parses");
    let mut content = DemoContent::default();
    content.install_catalog_actions(&catalog);

    // A player with heat to dissipate + a queued shot; an enemy with a queued
    // shot. tick_world must NOT fire either queue (it's the global clock, not the
    // fire step) but MUST tick the per-turn bookkeeping (heat dissipates).
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 2),
        40,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pulse_laser",
    );
    player.shield_profile = naked_shields();
    player.heat = 3;
    player.queue = vec!["pulse_laser".into()];
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        9,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "pulse_laser",
    );
    enemy.shield_profile = naked_shields();
    enemy.queue = vec!["pulse_laser".into()];
    let mut board = board_2d(vec![player, enemy]);

    let find = |b: &broadside_engine::types::Board, id: &str| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == id)
            .cloned()
            .unwrap()
    };

    tick_world(&mut board, &content);

    // Bookkeeping ran: player heat dissipated by 1 (3 -> 2).
    assert_eq!(
        find(&board, "p").heat,
        2,
        "tick_world dissipates heat (the global per-turn bookkeeping)"
    );
    // Neither queue was fired: both ships full hull, queues intact.
    assert_eq!(
        find(&board, "p").hull,
        40,
        "tick_world did not fire the enemy's queue at the player"
    );
    assert_eq!(
        find(&board, "e").hull,
        9,
        "tick_world did not fire the player's queue at the enemy"
    );
    assert_eq!(
        find(&board, "p").queue.len(),
        1,
        "player's queue untouched by tick_world"
    );
    assert_eq!(
        find(&board, "e").queue.len(),
        1,
        "enemy's queue untouched by tick_world"
    );
}

#[test]
fn live_enemy_ids_lists_enemies_only() {
    let player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        10,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "noop",
    );
    let e1 = ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 1),
        5,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    let e2 = ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(1, 0),
        5,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "noop",
    );
    let board = board_2d(vec![player, e1, e2]);
    let mut ids = live_enemy_ids(&board);
    ids.sort();
    assert_eq!(
        ids,
        vec!["e1".to_string(), "e2".to_string()],
        "live_enemy_ids lists enemies only, not the player"
    );
}

/* =========================================================================
 * TURN-BASED enemy rhythm (#126, Bruce's chess-like model): one run_world_phase
 * = ONE turn (the bin calls it once per player action). Locks the per-turn enemy
 * rhythm against the REAL catalog with a STATIONARY player (only enemy behavior
 * shows): an enemy that starts OUT of pulse range MOVES to close (reposition),
 * THEN telegraphs, THEN fires — and CANNOT fire on turn 1 (#67 telegraph-one-
 * ahead + out of range). Observed rhythm (verified): T1 telegraph `__move_down`
 * (no fire); T2 moved to dist-2, telegraph `pulse_laser` (no fire); T3 fires
 * (player hull drops); T4+ re-fires (cd-free pulse).
 * ====================================================================== */
#[test]
fn turn_based_enemy_moves_then_telegraphs_then_fires() {
    use broadside_engine::input::DemoContent;
    let bytes = std::fs::read("assets/broadside.catalog.json").expect("catalog asset");
    let catalog = broadside_engine::catalog::load_from_bytes(&bytes).expect("catalog parses");
    let mut content = DemoContent::default();
    content.register_synthetics(); // the AI's close maneuver resolves the synthetic moves
    content.install_catalog_actions(&catalog);

    // Stationary player at (2,3) Bow(N), naked, NO queue (never fires) so the ONLY
    // actor is the enemy. Enemy at (2,0) Bow(S) + pulse_laser — same column, dist
    // 3 = Far = OUT of pulse's [Adjacent,Near] band. It must CLOSE to dist <= 2,
    // then telegraph, then fire.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        60,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pulse_laser",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 0),
        9,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "pulse_laser",
    );
    enemy.shield_profile = naked_shields();
    let mut board = board_2d(vec![player, enemy]);

    let epos = |b: &broadside_engine::types::Board| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .map(|s| s.pos)
    };
    let phull = |b: &broadside_engine::types::Board| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "p")
            .map_or(-1, |s| s.hull)
    };

    let spawn_row = epos(&board).unwrap().row;
    let mut moved = false;
    let mut first_hit_turn: Option<usize> = None;
    for turn in 1..=8 {
        resolve_round(&mut board, &content); // ONE turn
        if epos(&board).is_some_and(|p| p.row != spawn_row) {
            moved = true; // repositioned off its spawn row
        }
        if phull(&board) < 60 && first_hit_turn.is_none() {
            first_hit_turn = Some(turn);
        }
    }

    // #2: the enemy REPOSITIONED (closed toward range), not sat at spawn.
    assert!(
        moved,
        "#126: enemy must MOVE/close across turns (Bruce wants repositioning)"
    );
    // #1 + #67: it connects only AFTER closing + telegraphing — never on turn 1.
    let hit = first_hit_turn.expect("#126: after closing, the enemy fires + connects");
    assert!(
        hit > 1,
        "#67: enemy cannot fire on turn 1 (must close + telegraph first); fired turn {hit}"
    );
}

/// CORE COOLDOWN RULE (Bruce's loop): an enemy must NOT queue/telegraph a weapon
/// that is ON cooldown — the cooldown starts on FIRE, so it can re-fire only once
/// per (`cd_max` + 1) turns. (Bruce hit a re-queue-on-cooldown bug on the PLAYER
/// side; this confirms the ENEMY AI is correctly gated.) Traced + verified: a
/// `beam_cannon` (cd 3) enemy in range fires at turns 2, 7, 12 — gap 5 (>= cd+1),
/// and during recharge it maneuvers instead of re-queuing the on-cd weapon.
#[test]
fn enemy_does_not_queue_an_on_cooldown_weapon() {
    use broadside_engine::input::DemoContent;
    let bytes = std::fs::read("assets/broadside.catalog.json").expect("catalog asset");
    let catalog = broadside_engine::catalog::load_from_bytes(&bytes).expect("catalog parses");
    let mut content = DemoContent::default();
    content.register_synthetics();
    content.install_catalog_actions(&catalog);

    // Stationary player (never queues). Enemy mounts beam_cannon (catalog cd 3,
    // band "mid" -> 2-D Near/Far) in range + bearing: enemy (2,1) Bow(S), player
    // (2,3), dist 2 = Near. The ONLY pacing should be the cd-3 recharge. If the AI
    // re-queued an on-cd beam_cannon the player's hull would drop every turn.
    let cd_max = content
        .action("beam_cannon")
        .expect("beam_cannon catalog action")
        .cost
        .cooldown_max;
    assert!(
        cd_max > 0,
        "beam_cannon must have a real cooldown to exercise the gate (got {cd_max})"
    );
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        200,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pulse_laser",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        9,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "beam_cannon",
    );
    enemy.shield_profile = naked_shields();
    enemy.heat_max = 99; // isolate the cd gate (remove heat/lockout as a confound)
    let mut board = board_2d(vec![player, enemy]);

    let phull = |b: &broadside_engine::types::Board| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "p")
            .map_or(-1, |s| s.hull)
    };
    let ecd = |b: &broadside_engine::types::Board| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .and_then(|s| s.cooldowns.get("beam_cannon").copied())
            .unwrap_or(0)
    };
    let equeued_beam = |b: &broadside_engine::types::Board| {
        b.cells
            .iter()
            .flatten()
            .find(|s| s.id == "e")
            .is_some_and(|s| s.queue.iter().any(|a| a == "beam_cannon"))
    };

    let mut prev = 200;
    let mut hit_turns = Vec::new();
    for turn in 1..=12 {
        // INVARIANT: the AI must never have beam_cannon QUEUED while it is on cd
        // (cd > 0). (Checked at the start of each turn, before this turn resolves.)
        assert!(
            !(equeued_beam(&board) && ecd(&board) > 0),
            "turn {turn}: enemy has beam_cannon queued while it is on cooldown ({}) — re-queue-on-cd bug",
            ecd(&board),
        );
        resolve_round(&mut board, &content);
        let h = phull(&board);
        if h < prev {
            hit_turns.push(turn);
        }
        prev = h;
    }
    // Gaps between consecutive fires must be >= cd_max + 1 (the firing turn + the
    // cd_max recharge turns). A gap of 1 would mean the cd is ignored on queue.
    for w in hit_turns.windows(2) {
        let gap = w[1] - w[0];
        assert!(gap > cd_max as usize,
            "enemy re-fired beam_cannon after only {gap} turns (cd_max {cd_max}); cooldown not respected");
    }
    assert!(
        !hit_turns.is_empty(),
        "sanity: the enemy should fire beam_cannon at least once"
    );
}
