//! Enemy-and-player broadside integration coverage (#92, Model D).
//!
//! `tests/geometry2d.rs` + `src/geometry2d.rs` unit-test the Model D arc table
//! (`arc_bears`/`bearing_cardinals`: a `BroadsideArc` bears out the two flank
//! cardinals PERPENDICULAR to the hull's forward axis — turn the bow E/W and the
//! flanks face N/S). This file is the END-TO-END spec for the broadside hook
//! working through the live round: a broadside-armed ship that presents a flank
//! actually LANDS a shot via `resolve_round`, and a mis-facing broadside enemy
//! ROTATES (the #92/#86 arc-agnostic `rotate_to_make_weapon_bear`) until its
//! flank bears, then fires.
//!
//! Geometry (Model D): a `Bow(E)` hull's forward axis is `EastWest`, so its flanks
//! face N/S — it broadsides UP/DOWN a column. Two ships on the same column thus
//! present flanks at each other. We use that: player at the front of column 2
//! Bow(E), enemy up the column Bow(E), so each one's flank bears on the other.
//!
//! Fixtures are the shared invariant-A `board_2d`/`ship_2d` (tests/common).

mod common;

use broadside_engine::grid::{Dir4, Facing, Pos, Range};
use broadside_engine::resolve::{resolve_round, run_world_phase, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Effect, Faction, Projectile, RangeBand, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};
use common::{board_2d, naked_shields, ship_2d};
use std::collections::HashMap;

/// A BROADSIDE-pattern weapon on the `BroadsideArc`: fires out BOTH flank
/// cardinals, taking the first in-band occupant on each ray. `raw` damage,
/// no band falloff (so the landed number is legible), all three 2-D bands.
fn broadside_gun(id: &str, raw: i32) -> Action {
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
            range_band: vec![Range::Adjacent, Range::Near, Range::Far],
            optimal_range: Range::Adjacent,
            pattern: TargetingPattern::BROADSIDE,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(Arc::BroadsideArc),
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

/// Serves the broadside guns by id. No ordnance.
struct BroadsideContent {
    actions: HashMap<String, Action>,
}
impl BroadsideContent {
    fn new(actions: Vec<Action>) -> Self {
        Self {
            actions: actions.into_iter().map(|a| (a.id.clone(), a)).collect(),
        }
    }
}
impl Content for BroadsideContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("broadside scenarios fire beams, not ordnance");
    }
}

fn hull_at(b: &broadside_engine::types::Board, pos: Pos) -> i32 {
    b.cells[pos.to_index()].as_ref().expect("ship present").hull
}

/* =========================================================================
 * 1. The player's broadside bears up its column and lands a shot.
 * ====================================================================== */

#[test]
fn player_broadside_bears_up_the_column_and_hits() {
    // Player at (2,3) Bow(E): forward axis EastWest -> flanks face N/S, so the
    // BroadsideArc bears up (and down) column 2. A naked enemy at (2,1) is on the
    // N flank ray (distance 2 = Near, in band) -> the player's broadside hits it.
    let player_pos = Pos::new(2, 3);
    let enemy_pos = Pos::new(2, 1);
    let mut player = ship_2d(
        "p",
        Faction::Player,
        player_pos,
        30,
        Facing::Bow(Dir4::E),
        Arc::BroadsideArc,
        "broadside",
    );
    player.queue = vec!["broadside".into()];
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        enemy_pos,
        6,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "noop",
    );
    enemy.shield_profile = naked_shields();
    let mut board = board_2d(vec![player, enemy]);
    let content = BroadsideContent::new(vec![broadside_gun("broadside", 4)]);

    resolve_round(&mut board, &content);

    assert_eq!(
        hull_at(&board, enemy_pos),
        2,
        "player's BroadsideArc gun bears N up the column and lands 4 (6 -> 2)",
    );
}

/* =========================================================================
 * 2. Two broadside ships on a column trade flank shots (mutual broadside).
 * ====================================================================== */

#[test]
fn two_broadside_ships_on_a_column_trade_flank_shots() {
    // Player (2,3) Bow(E) and an armed enemy (2,1) Bow(E): both forward-EastWest,
    // both flanks N/S. The player's N flank bears on the enemy; the enemy's S
    // flank bears on the player. One round: the player fires its queued broadside
    // (enemy loses hull), then the world phase fires the enemy's telegraph from a
    // PRIOR decide — so to see the enemy's return shot land we run a second round.
    let player_pos = Pos::new(2, 3);
    let enemy_pos = Pos::new(2, 1);
    let mut player = ship_2d(
        "p",
        Faction::Player,
        player_pos,
        30,
        Facing::Bow(Dir4::E),
        Arc::BroadsideArc,
        "pbroad",
    );
    player.shield_profile = naked_shields();
    let mut enemy = ship_2d(
        "e",
        Faction::Enemy,
        enemy_pos,
        30,
        Facing::Bow(Dir4::E),
        Arc::BroadsideArc,
        "ebroad",
    );
    enemy.shield_profile = naked_shields();
    let mut board = board_2d(vec![player, enemy]);
    let content =
        BroadsideContent::new(vec![broadside_gun("pbroad", 4), broadside_gun("ebroad", 3)]);

    // Round 1: player fires (enemy 30 -> 26); enemy only telegraphs this phase.
    if let Some(s) = board.cells[player_pos.to_index()].as_mut() {
        s.queue = vec!["pbroad".into()];
    }
    resolve_round(&mut board, &content);
    assert_eq!(
        hull_at(&board, enemy_pos),
        26,
        "round 1: player broadside lands on the enemy (30 -> 26)"
    );

    // Round 2: the enemy fires the broadside it telegraphed in round 1 -> the
    // player (on the enemy's S flank) takes its return shot.
    if let Some(s) = board.cells[player_pos.to_index()].as_mut() {
        s.queue = vec!["pbroad".into()];
    }
    resolve_round(&mut board, &content);
    assert!(
        hull_at(&board, player_pos) < 30,
        "round 2: the enemy's telegraphed broadside lands on the player (its S flank bears down the column)",
    );
}

/* =========================================================================
 * 3. A mis-facing broadside enemy ROTATES its flank to bear, then fires.
 *    The end-to-end integration of #92 (Model D arc) + #86 (rotate-to-bear).
 * ====================================================================== */

#[test]
fn misfacing_broadside_enemy_rotates_flank_to_bear_then_fires() {
    // Enemy at (2,1) starts Bow(S): forward axis NorthSouth -> flanks face E/W,
    // so its BroadsideArc does NOT bear the player straight down column 2
    // (on-axis S does not bear a broadside). The AI's rotate-to-bear must turn it
    // toward a Bow(E)/Bow(W) facing (flanks N/S) so the player on its S flank
    // bears, THEN it fires. Player at (2,3), naked so the hit is observable.
    let player_pos = Pos::new(2, 3);
    let enemy_pos = Pos::new(2, 1);
    let mut player = ship_2d(
        "p",
        Faction::Player,
        player_pos,
        99,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "noop",
    );
    player.shield_profile = naked_shields();
    let enemy = ship_2d(
        "e",
        Faction::Enemy,
        enemy_pos,
        30,
        Facing::Bow(Dir4::S),
        Arc::BroadsideArc,
        "ebroad",
    );
    let mut board = board_2d(vec![player, enemy]);
    let content = BroadsideContent::new(vec![broadside_gun("ebroad", 3)]);

    // Precondition: the enemy does NOT bear on the player from its starting Bow(S)
    // (broadside is on-axis to the player down the column — does not bear).
    assert_eq!(
        broadside_engine::resolve::resolve_targeting_2d(
            content.action("ebroad").unwrap(),
            &board,
            enemy_pos,
        ),
        Vec::<Pos>::new(),
        "precondition: a Bow(S) broadside does NOT bear the down-column player",
    );

    // Drive world phases: the AI rotates the enemy toward a bearing flank, then
    // fires. Within a few phases the player must take damage. (Fire-then-decide +
    // a quarter-turn may take up to ~3 phases: rotate -> [maybe rotate] -> bear
    // + telegraph -> fire.)
    let hull_before = hull_at(&board, player_pos);
    let mut hit = false;
    for _ in 0..6 {
        run_world_phase(&mut board, &content);
        if hull_at(&board, player_pos) < hull_before {
            hit = true;
            break;
        }
    }
    assert!(
        hit,
        "a mis-facing BroadsideArc enemy rotates its flank to bear then fires; player hull never dropped from {hull_before}",
    );

    // And the enemy ended on a facing whose flank actually bears (Bow(E) or
    // Bow(W)) — it rotated to broadside, it didn't just sit Bow(S).
    let ef = board.cells[enemy_pos.to_index()]
        .as_ref()
        .expect("enemy alive")
        .facing;
    assert!(
        matches!(ef, Facing::Bow(Dir4::E | Dir4::W) | Facing::Broadside(_)),
        "the enemy rotated to a flank-bearing stance; ended at {ef:?}",
    );
}
