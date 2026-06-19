//! Damage pipeline integration tests focused on the subtle action-level
//! `bandFalloff` aggregation rule.
//!
//! The inline resolver test
//! `apply_damage_band_falloff_disabled_lands_full_amount` at
//! `src/resolve.rs:1108` proves the predicate works when the action carries a
//! SINGLE `Effect::DAMAGE` entry with `bandFalloff: Some(false)`. That is not
//! the load-bearing case.
//!
//! The TS predicate at `resolve.ts:142-145` is
//! `weapon.effects.some(e => e.kind === "DAMAGE" && e.bandFalloff === false)`
//! — i.e. ANY single DAMAGE effect on the action with the field explicitly
//! `false` disables falloff for the WHOLE `applyDamage` call, including
//! other DAMAGE effects on the same action that left the field absent /
//! `Some(true)`. A naive Rust port would check the predicate per-effect
//! inside `apply_effect`'s DAMAGE arm and accidentally make it per-effect
//! local instead of action-level.
//!
//! This file pins the action-level aggregation explicitly.

use broadside_engine::resolve::{apply_damage, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern,
    WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// Bare ship with a zero-armour, zero-charge shield profile so the damage
/// arithmetic in these tests reflects only the band-falloff predicate, not
/// directional shielding.
fn naked_ship(id: &str, faction: Faction, cell: usize, hull: i32) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace {
                armour: 0,
                charge: 0,
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
        },
        mounts: vec![Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "_".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// 7-cell board with the given ship list. `None` cells are empty lane slots.
fn empty_board(size: usize, ships: Vec<Option<Ship>>) -> Board {
    assert_eq!(ships.len(), size);
    Board {
        size,
        cells: ships,
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

/// Weapon with TWO `Effect::DAMAGE` entries. `band_falloff_flags` controls
/// the `bandFalloff` field on each entry. `optimal` is the optimal range
/// band; both DAMAGE effects deal `amount` raw damage.
fn dual_damage_weapon(
    optimal: RangeBand,
    amount: i32,
    band_falloff_flags: [Option<bool>; 2],
) -> Action {
    Action {
        id: "dual".into(),
        name: "Dual Damage".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: optimal,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![
            Effect::DAMAGE {
                amount,
                band_falloff: band_falloff_flags[0],
            },
            Effect::DAMAGE {
                amount,
                band_falloff: band_falloff_flags[1],
            },
        ],
        r#mod: None,
        icon: None,
    }
}

/// Empty content — `apply_damage` doesn't invoke content callbacks, but
/// `apply_effect` would, so we keep a trivial impl available for any test
/// that later upgrades to `apply_effect`.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("spawn_projectile not used in these tests");
    }
}

/* =========================================================================
 * The action-level aggregation predicate
 * ====================================================================== */

/// Baseline: two DAMAGE effects, both with `bandFalloff: None` (absent in
/// the JSON). At long range (delta 2 from optimal=close), each DAMAGE call
/// applies falloff: floor(4 * 0.5) = 2. Two calls -> total 4 lands on a
/// hull starting at 10. Final hull == 6.
///
/// Each `apply_damage` call is independent; this test exists to pin the
/// "no Some(false) anywhere -> falloff applies" reading of the predicate.
#[test]
fn dual_damage_both_absent_apply_falloff_to_each() {
    let attacker = naked_ship("frigate", Faction::Player, 0, 10);
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );
    let weapon = dual_damage_weapon(RangeBand::Close, 4, [None, None]);

    // Both effects route through apply_damage at distance 5 (long), delta
    // 2 from close, factor 0.5. Each lands floor(4 * 0.5) = 2 -> 10 - 4 = 6.
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(hull, 6, "two falloff-applied 2-damage hits leave 6 hull");
}

/// Baseline opposite: both DAMAGE effects with `bandFalloff: Some(false)`.
/// Falloff is disabled; each `apply_damage` lands the full 4. Two calls -> 8
/// total. Final hull == 2.
#[test]
fn dual_damage_both_some_false_bypass_falloff() {
    let attacker = naked_ship("frigate", Faction::Player, 0, 10);
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );
    let weapon = dual_damage_weapon(RangeBand::Close, 4, [Some(false), Some(false)]);

    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(hull, 2, "two raw 4-damage hits leave 2 hull");
}

/// THE LOAD-BEARING CASE: two DAMAGE effects, ONE with `bandFalloff:
/// Some(false)` and ONE with `bandFalloff: None`. The predicate is
/// action-level (`effects.iter().any(...)` at resolve.rs:427), so BOTH
/// calls into `apply_damage` see the predicate as `true` and BOTH bypass
/// falloff. Each lands the full 4. Two calls -> 8 total. Final hull == 2.
///
/// A naive port that checked the bandFalloff field per-effect inside
/// `apply_effect`'s DAMAGE arm would apply falloff to the `None` effect
/// (delivering 2) and bypass it for the `Some(false)` effect (delivering
/// 4), landing 6 total and leaving hull == 4. That value is the canary —
/// if this test ever produces `hull == 4`, the port has regressed to a
/// per-effect predicate.
#[test]
fn dual_damage_mixed_predicate_aggregates_at_action_level() {
    let attacker = naked_ship("frigate", Faction::Player, 0, 10);
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );
    let weapon = dual_damage_weapon(RangeBand::Close, 4, [Some(false), None]);

    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(
        hull, 2,
        "any Some(false) on the action disables falloff for the WHOLE call; \
         hull == 4 would mean the port regressed to a per-effect predicate",
    );
}

/// Symmetry: order of the DAMAGE effects must not matter. Same setup as
/// the load-bearing test with the flags swapped.
#[test]
fn dual_damage_mixed_predicate_order_independent() {
    let attacker = naked_ship("frigate", Faction::Player, 0, 10);
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );
    let weapon = dual_damage_weapon(RangeBand::Close, 4, [None, Some(false)]);

    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(
        hull, 2,
        "order of bandFalloff:Some(false) in effects must not matter"
    );
}

/// `Some(true)` is NOT the same as `Some(false)`: only `Some(false)`
/// disables falloff. A mix of `None` and `Some(true)` keeps falloff on for
/// every effect.
#[test]
fn dual_damage_mixed_none_and_some_true_keeps_falloff_on() {
    let attacker = naked_ship("frigate", Faction::Player, 0, 10);
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );
    let weapon = dual_damage_weapon(RangeBand::Close, 4, [None, Some(true)]);

    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);
    apply_damage(5, 4, 0, &weapon, &mut board, &NoContent);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(
        hull, 6,
        "Some(true) is the default-on form, not the bypass form"
    );
}

/// Boundary check: drive the load-bearing mixed-predicate scenario through
/// the full `execute_queue` -> `apply_effect` -> `apply_damage` pipeline,
/// not by calling `apply_damage` twice by hand.
///
/// The other tests in this file call `apply_damage` directly, which is the
/// inner correctness boundary. This test exists so that a future refactor
/// to `apply_effect`'s DAMAGE arm (e.g. batching, short-circuiting after
/// the first effect, moving the action-level predicate into the per-effect
/// match) shows up here. Per reviewer's follow-up note on commit 96ecd6c.
///
/// Setup: attacker at cell 0 with the dual-DAMAGE `pulse_laser` queued
/// (Some(false) + None mix). Target at cell 5 (Long range, delta 2 from
/// Close optimal). With the action-level predicate aggregating correctly,
/// BOTH `apply_damage` calls bypass falloff, landing raw 4 each. Two effects
/// -> two `apply_damage` invocations -> total 8 damage. Naked target with
/// hull 10 ends at 2. (Same expected outcome as
/// `dual_damage_mixed_predicate_aggregates_at_action_level`, but routed
/// through the resolver's effect-dispatch boundary instead of bypassing it.)
#[test]
fn dual_damage_mixed_predicate_through_execute_queue() {
    let mut attacker = naked_ship("frigate", Faction::Player, 0, 10);
    attacker.queue = vec!["dual".into()];
    let target = naked_ship("scout", Faction::Enemy, 5, 10);
    let mut board = empty_board(
        7,
        vec![Some(attacker), None, None, None, None, Some(target), None],
    );

    // Content impl that returns the dual-DAMAGE weapon under id "dual".
    struct DualContent(Action);
    impl Content for DualContent {
        fn action(&self, id: &str) -> Option<&Action> {
            (id == "dual").then_some(&self.0)
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            panic!("spawn_projectile not used in this test");
        }
    }
    let content = DualContent(dual_damage_weapon(RangeBand::Close, 4, [Some(false), None]));

    broadside_engine::resolve::fire_player_queue("frigate", &mut board, &content);

    let hull = board.cells[5].as_ref().unwrap().hull;
    assert_eq!(
        hull, 2,
        "execute_queue should drive both DAMAGE effects through apply_effect, \
         each calling apply_damage which independently sees the action-level \
         Some(false) and bypasses falloff. hull == 6 would mean apply_effect \
         short-circuited after one effect; hull == 4 would mean the predicate \
         dropped its action-level aggregation",
    );
}

/// Avoid the unused-import warning on `Content` / `NoContent` (kept for
/// other apply_effect-driven tests in this file).
#[test]
fn no_content_constructible() {
    let _: &dyn Content = &NoContent;
}
