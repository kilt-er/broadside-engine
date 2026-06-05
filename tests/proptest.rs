//! Property-based tests for the narrow places where enumeration is unsafe.
//!
//! Three cases earn proptest:
//!
//! - **`band_falloff`** — the input space (`raw: i32`) is unbounded, so an
//!   enumerated table cannot exhaustively catch overflow or sign-bug
//!   regressions. The property is "result is in `[0, max(raw, 0)]` for every
//!   `(actual, optimal)` pair".
//! - **`absorb_shield`** — the state machine has three integer dimensions
//!   (`dmg`, `armour`, `charge`) and one observable side-effect (charge
//!   decrement). Property: charge decreases by exactly 0 or 1, returned
//!   damage is non-negative and never exceeds `dmg`, and consumption only
//!   happens when `dmg > 0`.
//! - **heat curve** (`heat::*`) — the heat/lockout state machine spans
//!   `run_action` (accumulate + set-lockout-at-max) and `end_of_turn`
//!   (dissipate + clear-lockout-below-max). The fixed sequences in
//!   `tests/combat_loop.rs` (#73) pin ONE weapon (pulse_laser heat 2, max 6);
//!   the dimensions that actually vary in play — per-weapon heat cost and a
//!   ship's `heat_max` — plus arbitrary fire/idle turn orderings are an
//!   unbounded space. The property drives the REAL resolver (no oracle
//!   re-implementation) and asserts the engine's two load-bearing invariants
//!   at every observable step: heat never goes negative, and `locked_out`
//!   holds iff heat has reached `heat_max`.
//!
//! Everything else in the geometry surface is finite and enumerated in
//! `tests/geometry.rs`. We deliberately do NOT sprinkle proptest there.

use broadside_engine::geometry::{absorb_shield, band_falloff};
use broadside_engine::types::{RangeBand, ShieldFace};
use proptest::prelude::*;

/// The five RangeBand variants in canonical declaration order. Mirrors the
/// private `BAND_ORDER` in `src/geometry.rs:39-45`; kept local so the
/// integration test doesn't depend on an exported constant.
const ALL_BANDS: [RangeBand; 5] = [
    RangeBand::PointBlank,
    RangeBand::Close,
    RangeBand::Mid,
    RangeBand::Long,
    RangeBand::Extreme,
];

/// Strategy producing one of the five `RangeBand` variants uniformly.
fn any_band() -> impl Strategy<Value = RangeBand> {
    prop_oneof![
        Just(RangeBand::PointBlank),
        Just(RangeBand::Close),
        Just(RangeBand::Mid),
        Just(RangeBand::Long),
        Just(RangeBand::Extreme),
    ]
}

/// Index of `b` in [`ALL_BANDS`]. Used by monotonicity property below.
fn band_idx(b: RangeBand) -> usize {
    ALL_BANDS.iter().position(|&x| x == b).expect("ALL_BANDS covers every variant")
}

proptest! {
    /// `band_falloff` must return a value in `[0, max(raw, 0)]` for any
    /// `(raw, actual, optimal)`. The factor table never goes above 1.0 and
    /// the function clamps at zero, so this is the strongest invariant
    /// that holds for the full TS-port equivalence.
    ///
    /// `raw` is constrained to a range that, when multiplied by 1.0 in
    /// f64, still fits cleanly in i32 — this avoids testing the f64-to-i32
    /// saturation behaviour of `as i32`, which is allowed to be lossy and
    /// is not the property under test.
    #[test]
    fn band_falloff_result_is_bounded_by_max_zero_raw(
        raw in -1_000_000_i32..1_000_000_i32,
        actual in any_band(),
        optimal in any_band(),
    ) {
        let out = band_falloff(raw, actual, optimal);
        prop_assert!(out >= 0, "band_falloff({raw}, {actual:?}, {optimal:?}) returned negative {out}");
        let ceiling = raw.max(0);
        prop_assert!(
            out <= ceiling,
            "band_falloff({raw}, {actual:?}, {optimal:?}) = {out} exceeds ceiling {ceiling}",
        );
    }

    /// When `actual == optimal`, the falloff factor is exactly 1.0 and the
    /// function must return non-negative raw unchanged. (Negative raw is
    /// clamped to 0.)
    #[test]
    fn band_falloff_self_pair_returns_clamped_raw(
        raw in -1_000_000_i32..1_000_000_i32,
        band in any_band(),
    ) {
        let out = band_falloff(raw, band, band);
        prop_assert_eq!(out, raw.max(0));
    }

    /// Monotonicity in delta: for fixed `raw >= 0` and fixed `optimal`, the
    /// returned damage is non-increasing as `actual` moves further from
    /// `optimal`. This pins that the factor table `[1, 0.66, 0.5, 0.33, 0.2]`
    /// is monotonically non-increasing — a typo that swapped two entries
    /// (e.g. `[1, 0.5, 0.66, ...]`) would make this property fail.
    ///
    /// The integration suite at `tests/geometry.rs` covers monotonicity for
    /// `raw == 10`; this generalises to every non-negative raw.
    #[test]
    fn band_falloff_is_monotonically_non_increasing_in_delta(
        raw in 0_i32..1_000_000_i32,
        optimal in any_band(),
    ) {
        let opt_idx = band_idx(optimal) as i32;
        // For every pair of bands (a, b), if a is closer to optimal than b,
        // then band_falloff at a must be >= band_falloff at b.
        for &a in &ALL_BANDS {
            let d_a = (band_idx(a) as i32 - opt_idx).unsigned_abs();
            let r_a = band_falloff(raw, a, optimal);
            for &b in &ALL_BANDS {
                let d_b = (band_idx(b) as i32 - opt_idx).unsigned_abs();
                if d_a <= d_b {
                    let r_b = band_falloff(raw, b, optimal);
                    prop_assert!(
                        r_a >= r_b,
                        "monotonicity broken at raw={raw}, optimal={optimal:?}: \
                         delta {d_a} ({a:?}) -> {r_a}, delta {d_b} ({b:?}) -> {r_b}",
                    );
                }
            }
        }
    }

    /// `absorb_shield` invariants:
    ///
    /// 1. The returned damage is non-negative.
    /// 2. The returned damage is at most `dmg` (no amplification — armour
    ///    only subtracts).
    /// 3. `charge` decreases by exactly 0 or 1.
    /// 4. A charge is consumed iff `dmg > 0` AND `charge > 0` (i.e. when a
    ///    positive hit lands on a face that has charge to spend).
    ///
    /// Pairing all four properties in one test cuts proptest runtime over
    /// splitting them.
    #[test]
    fn absorb_shield_invariants(
        dmg in -10_000_i32..10_000_i32,
        armour in 0_i32..1_000_i32,
        charge in 0_i32..1_000_i32,
    ) {
        let mut face = ShieldFace { armour, charge };
        let initial_charge = face.charge;
        let initial_armour = face.armour;
        let out = absorb_shield(&mut face, dmg);

        prop_assert!(out >= 0, "absorb_shield returned negative {out}");
        prop_assert!(out <= dmg.max(0), "absorb_shield returned {out} > dmg {dmg}");
        prop_assert_eq!(face.armour, initial_armour, "armour must be permanent");

        let consumed = initial_charge - face.charge;
        prop_assert!(
            consumed == 0 || consumed == 1,
            "charge changed by {consumed}, expected 0 or 1 (initial={initial_charge}, final={})",
            face.charge,
        );

        let expect_consumed = dmg > 0 && initial_charge > 0;
        prop_assert_eq!(
            consumed == 1,
            expect_consumed,
            "charge consumption mismatch: dmg={}, charge={}, consumed={}",
            dmg, initial_charge, consumed,
        );
    }

    /// When `dmg > 0` and `charge == 0`, returned damage must be exactly
    /// `max(0, dmg - armour)`. This is the armour-arithmetic branch in
    /// isolation, scanned across the full parameter range.
    #[test]
    fn absorb_shield_armour_arithmetic_when_no_charge(
        dmg in 1_i32..10_000_i32,
        armour in 0_i32..10_000_i32,
    ) {
        let mut face = ShieldFace { armour, charge: 0 };
        let out = absorb_shield(&mut face, dmg);
        prop_assert_eq!(out, (dmg - armour).max(0));
    }
}

/// Heat / lockout state-machine invariants, driven through the REAL resolver.
///
/// The heat curve is split across two engine functions:
///   - [`fire_player_queue`] -> `run_action` ADDS `cost.heat` and sets
///     `locked_out` when `heat >= heat_max` (resolve.rs:488-490);
///   - [`end_of_turn`] SUBTRACTS 1 (floored at 0) and CLEARS `locked_out`
///     when `heat < heat_max` (resolve.rs:611-613).
///
/// `tests/combat_loop.rs` (#73) pins ONE concrete weapon/max. Here we vary the
/// two dimensions that actually move in play — per-weapon `cost.heat` and the
/// ship's `heat_max` — over arbitrary fire/idle turn sequences, and assert the
/// engine's exact invariant at every observable step. We deliberately DON'T
/// re-implement the curve as an oracle (that would re-encode the engine and
/// pass for the wrong reason, the trap #76 cleared out); we drive the live
/// functions and check the relationship that must always hold.
mod heat {
    use broadside_engine::resolve::{end_of_turn, fire_player_queue, Content};
    use broadside_engine::types::{
        Action, ActionCost, Arc, Board, EventBus, Effect, Faction, LaneEnd, Mount, Orientation,
        Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern,
        WeaponArchetype,
    };
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn naked_shields() -> ShieldProfile {
        ShieldProfile {
            bow: ShieldFace { armour: 0, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 0, charge: 0 },
            starboard: ShieldFace { armour: 0, charge: 0 },
        }
    }

    /// A ship with one forward-arc mount loaded with `weapon`.
    fn ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd, weapon: &str) -> Ship {
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
            shield_profile: naked_shields(),
            mounts: vec![Mount { id: format!("{id}-m1"), arc: Arc::Forward, weapon: weapon.into() }],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    /// A forward beam costing `heat` per shot, dealing 1 damage (low enough
    /// that the high-hull target never dies, so the shooter keeps firing).
    fn beam(heat: i32) -> Action {
        Action {
            id: "beam".into(),
            name: "beam".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost { heat, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![
                    RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid,
                    RangeBand::Long, RangeBand::Extreme,
                ],
                optimal_band: RangeBand::PointBlank,
                requires_arc: Some(Arc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE { amount: 1, band_falloff: Some(false) }],
            r#mod: None,
            icon: None,
        }
    }

    struct OneBeam(Action);
    impl Content for OneBeam {
        fn action(&self, id: &str) -> Option<&Action> {
            (id == "beam").then_some(&self.0)
        }
        fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
            unreachable!("the heat property fires a beam, not ordnance");
        }
    }

    fn board(size: usize, cells: Vec<Option<Ship>>) -> Board {
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

    /// Read the shooter's (heat, heat_max, locked_out).
    fn read(b: &Board, id: &str) -> (i32, i32, bool) {
        let s = b.cells.iter().flatten().find(|s| s.id == id).expect("shooter alive");
        (s.heat, s.heat_max, s.locked_out)
    }

    /// The exact engine invariant: heat is never negative, and `locked_out`
    /// holds IFF heat has reached `heat_max`. Set on fire (heat >= max), cleared
    /// on EOT (heat < max); since a fire only adds heat and an EOT only removes
    /// it, the biconditional holds at every observable point — see the module
    /// doc for the derivation.
    fn assert_invariant(b: &Board, id: &str, when: &str) -> Result<(), TestCaseError> {
        let (heat, heat_max, locked) = read(b, id);
        prop_assert!(heat >= 0, "{when}: heat went negative ({heat})");
        prop_assert_eq!(
            locked,
            heat >= heat_max,
            "{} locked_out={} but heat={} heat_max={} (lockout must hold iff heat >= heat_max)",
            when, locked, heat, heat_max,
        );
        Ok(())
    }

    proptest! {
        /// Drive an arbitrary fire/idle turn sequence with an arbitrary
        /// per-shot heat cost and ship heat_max; the heat/lockout invariant
        /// must hold after every fire and every end-of-turn.
        ///
        /// `cost` starts at 1 (a 0-heat weapon never accrues heat or locks out,
        /// so it can't exercise the lockout edge); `heat_max` ranges low so a
        /// few shots actually reach it. `fire` flags choose, per turn, whether
        /// the shooter commits its beam (true) or idles (false) — idling still
        /// runs end_of_turn, so cooling is exercised too.
        #[test]
        fn heat_stays_nonnegative_and_lockout_tracks_heat_max(
            cost in 1_i32..=5_i32,
            heat_max in 1_i32..=10_i32,
            fire in prop::collection::vec(any::<bool>(), 1..40),
        ) {
            // Shooter (player) at cell 0 bow=Fore; an inert high-hull target at
            // cell 1 keeps the beam in-arc/in-band every turn so a queued shot
            // actually fires and spends heat. The target's hull (10_000) far
            // exceeds the 1-damage beam over <40 turns, so it never dies.
            let mut shooter = ship("pc", Faction::Player, 0, 50, LaneEnd::Fore, "beam");
            shooter.heat_max = heat_max;
            let target = ship("tgt", Faction::Enemy, 1, 10_000, LaneEnd::Aft, "beam");
            let mut b = board(7, vec![
                Some(shooter), Some(target), None, None, None, None, None,
            ]);
            let content = OneBeam(beam(cost));

            assert_invariant(&b, "pc", "initial")?;

            for (turn, &do_fire) in fire.iter().enumerate() {
                if do_fire {
                    if let Some(s) = b.cells.iter_mut().flatten().find(|s| s.id == "pc") {
                        s.queue = vec!["beam".into()];
                    }
                    fire_player_queue("pc", &mut b, &content);
                    assert_invariant(&b, "pc", &format!("after fire on turn {turn}"))?;
                }
                end_of_turn(&mut b, &content);
                assert_invariant(&b, "pc", &format!("after end_of_turn on turn {turn}"))?;
            }
        }
    }
}
