//! Geometry integration suite. Exercises every public function in
//! `broadside_engine::geometry` against the canonical TypeScript reference
//! (`broadside-engine/engine/geometry.ts`).
//!
//! Inline tests in `src/geometry.rs` are sanity smoke checks (one or two
//! asserts per function). These integration tests are exhaustive: full
//! distance sweeps, complete arc/orientation matrices, every state of the
//! shield absorption state machine. If the TS reference and this crate ever
//! disagree, one of these tests must fail.
//!
//! Test names describe the behaviour under test, not the function name. See
//! `broadside-tester.md` § "Test conventions".
//!
//! ## Canonical reference snapshots
//!
//! - `geometry.ts:30-37` for the range-band bucket boundaries
//! - `geometry.ts:41-45` for the falloff factor table `[1, 0.66, 0.5, 0.33, 0.2]`
//! - `geometry.ts:61-66` for the bow-on / broadside zone routing
//! - `geometry.ts:74-86` for the arc-vs-orientation gate
//! - `geometry.ts:101-108` for the shield-charge-then-armour pipeline

use broadside_engine::geometry::{
    absorb_shield, arc_bears, band_falloff, bears, default_shield_profile, direction_to, distance,
    facing_zone, in_band, opposite, range_band,
};
use broadside_engine::types::{
    Arc, Faction, HullZone, LaneEnd, Mount, Orientation, RangeBand, ShieldFace, ShieldProfile, Ship,
};
use std::collections::HashMap;

/* =========================================================================
 * Helpers
 * ====================================================================== */

/// Bare-bones ship for `bears` tests — only `cell` and `orientation` matter
/// to that function. Everything else is filler picked to satisfy the type:
/// hull/heat are 1/0, mounts/queue/statuses/traits are empty, and
/// `shield_profile` is `default_shield_profile()` (strong bow, weak stern,
/// medium flanks) so tests that read the profile incidentally see the
/// canonical Frigate layout.
fn ship_at(cell: usize, orientation: Orientation) -> Ship {
    Ship {
        id: "test".into(),
        faction: Faction::Player,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation,
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull: 1,
        max_hull: 1,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: Vec::<Mount>::new(),
        queue: vec![],
        cooldowns: HashMap::new(),
        statuses: vec![],
        traits: vec![],
        klass: None,
        tail: None,
    }
}

/* =========================================================================
 * opposite + direction_to + distance — the lane-axis primitives
 * ====================================================================== */

#[test]
fn opposite_is_an_involution() {
    // Calling opposite twice returns the original — a property weaker than
    // the inline "swaps ends" assertion but worth keeping pinned. If anyone
    // ever introduces a third LaneEnd this breaks immediately.
    assert_eq!(opposite(opposite(LaneEnd::Fore)), LaneEnd::Fore);
    assert_eq!(opposite(opposite(LaneEnd::Aft)), LaneEnd::Aft);
}

#[test]
fn direction_to_returns_fore_when_target_is_at_or_beyond_attacker() {
    // The TS predicate is `b >= a ? "fore" : "aft"`. Equal cells map to
    // Fore — a non-obvious edge case worth a dedicated assertion.
    assert_eq!(direction_to(0, 0), LaneEnd::Fore);
    assert_eq!(direction_to(3, 3), LaneEnd::Fore);
    assert_eq!(direction_to(0, 6), LaneEnd::Fore);
}

#[test]
fn direction_to_returns_aft_only_when_target_is_strictly_behind() {
    assert_eq!(direction_to(6, 0), LaneEnd::Aft);
    assert_eq!(direction_to(5, 4), LaneEnd::Aft);
}

#[test]
fn distance_is_symmetric() {
    assert_eq!(distance(2, 5), distance(5, 2));
    assert_eq!(distance(0, 6), 6);
}

/* =========================================================================
 * range_band — the bucket ruler
 * ====================================================================== */

#[test]
fn range_band_bucket_sweep_zero_through_eight() {
    // Full sweep so the boundary conditions are all visible side-by-side.
    // d <= 1 -> PointBlank, d == 2 -> Close, d in 3..=4 -> Mid,
    // d in 5..=6 -> Long, d >= 7 -> Extreme.
    let expected = [
        (0, RangeBand::PointBlank),
        (1, RangeBand::PointBlank),
        (2, RangeBand::Close),
        (3, RangeBand::Mid),
        (4, RangeBand::Mid),
        (5, RangeBand::Long),
        (6, RangeBand::Long),
        (7, RangeBand::Extreme),
        (8, RangeBand::Extreme),
    ];
    for (d, want) in expected {
        assert_eq!(
            range_band(0, d),
            want,
            "range_band(0,{d}) should be {want:?}"
        );
        // Symmetry: distance is absolute, so swapping cells must not change band.
        assert_eq!(
            range_band(d, 0),
            want,
            "range_band({d},0) should be {want:?}"
        );
    }
}

#[test]
fn in_band_filters_by_allowed_set() {
    let allowed = [RangeBand::Close, RangeBand::Mid];
    // pointBlank (d=1) is NOT in the allowed set
    assert!(!in_band(&allowed, 0, 1));
    // close (d=2) IS allowed
    assert!(in_band(&allowed, 0, 2));
    // mid (d=3) IS allowed
    assert!(in_band(&allowed, 0, 3));
    // long (d=5) is NOT allowed
    assert!(!in_band(&allowed, 0, 5));
}

#[test]
fn in_band_empty_allowed_set_rejects_everything() {
    let allowed: [RangeBand; 0] = [];
    assert!(!in_band(&allowed, 0, 0));
    assert!(!in_band(&allowed, 0, 3));
    assert!(!in_band(&allowed, 0, 9));
}

/* =========================================================================
 * band_falloff — the factor table [1, 0.66, 0.5, 0.33, 0.2]
 * ====================================================================== */

#[test]
fn band_falloff_delta_zero_returns_raw_unchanged() {
    // Every band, paired with itself, must return the raw value verbatim.
    for &b in &[
        RangeBand::PointBlank,
        RangeBand::Close,
        RangeBand::Mid,
        RangeBand::Long,
        RangeBand::Extreme,
    ] {
        assert_eq!(
            band_falloff(10, b, b),
            10,
            "{b:?} self-pair should be unchanged"
        );
    }
}

#[test]
fn band_falloff_delta_one_applies_two_thirds_factor() {
    // 0.66 * 10 = 6.6, floor -> 6
    assert_eq!(band_falloff(10, RangeBand::Close, RangeBand::PointBlank), 6);
    assert_eq!(band_falloff(10, RangeBand::Mid, RangeBand::Close), 6);
    assert_eq!(band_falloff(10, RangeBand::Long, RangeBand::Mid), 6);
    assert_eq!(band_falloff(10, RangeBand::Extreme, RangeBand::Long), 6);
}

#[test]
fn band_falloff_delta_two_applies_half_factor() {
    // 0.5 * 10 = 5
    assert_eq!(band_falloff(10, RangeBand::Mid, RangeBand::PointBlank), 5);
    assert_eq!(band_falloff(10, RangeBand::Long, RangeBand::Close), 5);
    assert_eq!(band_falloff(10, RangeBand::Extreme, RangeBand::Mid), 5);
}

#[test]
fn band_falloff_delta_three_applies_third_factor() {
    // 0.33 * 10 = 3.3, floor -> 3
    assert_eq!(band_falloff(10, RangeBand::Long, RangeBand::PointBlank), 3);
    assert_eq!(band_falloff(10, RangeBand::Extreme, RangeBand::Close), 3);
}

#[test]
fn band_falloff_delta_four_applies_one_fifth_factor() {
    // 0.2 * 10 = 2
    assert_eq!(
        band_falloff(10, RangeBand::Extreme, RangeBand::PointBlank),
        2
    );
}

#[test]
fn band_falloff_is_symmetric_in_actual_and_optimal() {
    // The TS uses abs(actual - optimal) so swapping endpoints must not
    // change the result. This protects against a refactor that drops the
    // abs and re-routes the lookup.
    for &(a, b) in &[
        (RangeBand::PointBlank, RangeBand::Extreme),
        (RangeBand::Close, RangeBand::Long),
        (RangeBand::Mid, RangeBand::Mid),
    ] {
        assert_eq!(band_falloff(10, a, b), band_falloff(10, b, a));
    }
}

#[test]
fn band_falloff_floors_fractional_results() {
    // The TS uses Math.floor on the product, not round. 5 * 0.66 = 3.3
    // floors to 3 (not 4, which a round-half-up would produce).
    assert_eq!(band_falloff(5, RangeBand::Close, RangeBand::PointBlank), 3);
    // 3 * 0.66 = 1.98 floors to 1.
    assert_eq!(band_falloff(3, RangeBand::Close, RangeBand::PointBlank), 1);
}

#[test]
fn band_falloff_clamps_negative_raw_at_zero() {
    // The TS does `Math.max(0, ...)`. Negative raw must never produce a
    // negative output.
    assert_eq!(band_falloff(-1, RangeBand::Mid, RangeBand::Mid), 0);
    assert_eq!(
        band_falloff(-100, RangeBand::Extreme, RangeBand::PointBlank),
        0
    );
}

#[test]
fn band_falloff_zero_raw_returns_zero() {
    for &b in &[RangeBand::PointBlank, RangeBand::Extreme] {
        assert_eq!(band_falloff(0, b, RangeBand::Mid), 0);
    }
}

/* =========================================================================
 * facing_zone — the 12-case enumeration
 *
 * 2 stances ({BowOn, Broadside}) × the inputs that matter:
 *   - BowOn:     {bow=Fore, bow=Aft} × {incoming=Fore, incoming=Aft} = 4
 *   - Broadside: {incoming=Fore, incoming=Aft} = 2
 * = 6 distinct input tuples. Add the reverse-symmetry checks below for the
 * full safety net.
 * ====================================================================== */

#[test]
fn facing_zone_bow_on_fore_incoming_from_fore_hits_bow() {
    let o = Orientation::BowOn { bow: LaneEnd::Fore };
    assert_eq!(facing_zone(o, LaneEnd::Fore), HullZone::Bow);
}

#[test]
fn facing_zone_bow_on_fore_incoming_from_aft_hits_stern() {
    let o = Orientation::BowOn { bow: LaneEnd::Fore };
    assert_eq!(facing_zone(o, LaneEnd::Aft), HullZone::Stern);
}

#[test]
fn facing_zone_bow_on_aft_incoming_from_aft_hits_bow() {
    // Bow points aft — a hit arriving from the aft direction is taken on
    // the bow. This is the scenario B configuration from demo.ts.
    let o = Orientation::BowOn { bow: LaneEnd::Aft };
    assert_eq!(facing_zone(o, LaneEnd::Aft), HullZone::Bow);
}

#[test]
fn facing_zone_bow_on_aft_incoming_from_fore_hits_stern() {
    let o = Orientation::BowOn { bow: LaneEnd::Aft };
    assert_eq!(facing_zone(o, LaneEnd::Fore), HullZone::Stern);
}

#[test]
fn facing_zone_broadside_fore_routes_to_starboard() {
    // The deterministic split: incoming-from-fore -> starboard,
    // incoming-from-aft -> port. The model is stable; tests pin it.
    assert_eq!(
        facing_zone(Orientation::Broadside, LaneEnd::Fore),
        HullZone::Starboard
    );
}

#[test]
fn facing_zone_broadside_aft_routes_to_port() {
    assert_eq!(
        facing_zone(Orientation::Broadside, LaneEnd::Aft),
        HullZone::Port
    );
}

#[test]
fn facing_zone_never_returns_a_zone_off_the_lane_axis_for_bow_on() {
    // Property check: bow-on can never return port or starboard from a
    // lane-axis hit (those zones face off-lane). Walks all four BowOn
    // input combinations.
    for &bow in &[LaneEnd::Fore, LaneEnd::Aft] {
        for &incoming in &[LaneEnd::Fore, LaneEnd::Aft] {
            let z = facing_zone(Orientation::BowOn { bow }, incoming);
            assert!(
                z == HullZone::Bow || z == HullZone::Stern,
                "BowOn{{bow:{bow:?}}} incoming={incoming:?} returned off-axis zone {z:?}",
            );
        }
    }
}

#[test]
fn facing_zone_never_returns_bow_or_stern_when_broadside() {
    // Inverse property: broadside-stance lane hits never land on the bow
    // or stern.
    for &incoming in &[LaneEnd::Fore, LaneEnd::Aft] {
        let z = facing_zone(Orientation::Broadside, incoming);
        assert!(
            z == HullZone::Port || z == HullZone::Starboard,
            "Broadside incoming={incoming:?} returned on-axis zone {z:?}",
        );
    }
}

/* =========================================================================
 * arc_bears — the 16-case matrix (4 arcs × 2 stances × 2 directions)
 *
 * Encoded as a table so a change to any single cell shows up as one
 * obvious test failure.
 * ====================================================================== */

#[test]
fn arc_bears_full_matrix() {
    // (arc, orientation, toward_end, expected)
    //
    // Bow points fore in the BowOn rows; bow=Aft is covered in a separate
    // case below so the directional symmetry is also exercised.
    let bow_on_fore = Orientation::BowOn { bow: LaneEnd::Fore };
    let broadside = Orientation::Broadside;
    let cases: &[(Arc, Orientation, LaneEnd, bool)] = &[
        // Turret: always bears.
        (Arc::Turret, bow_on_fore, LaneEnd::Fore, true),
        (Arc::Turret, bow_on_fore, LaneEnd::Aft, true),
        (Arc::Turret, broadside, LaneEnd::Fore, true),
        (Arc::Turret, broadside, LaneEnd::Aft, true),
        // Forward: bears only when bow-on AND the firing direction matches
        // the bow.
        (Arc::Forward, bow_on_fore, LaneEnd::Fore, true),
        (Arc::Forward, bow_on_fore, LaneEnd::Aft, false),
        (Arc::Forward, broadside, LaneEnd::Fore, false),
        (Arc::Forward, broadside, LaneEnd::Aft, false),
        // Rear: bears only when bow-on AND firing in the direction opposite
        // the bow.
        (Arc::Rear, bow_on_fore, LaneEnd::Fore, false),
        (Arc::Rear, bow_on_fore, LaneEnd::Aft, true),
        (Arc::Rear, broadside, LaneEnd::Fore, false),
        (Arc::Rear, broadside, LaneEnd::Aft, false),
        // BroadsideArc: bears only when broadside, and then both directions.
        (Arc::BroadsideArc, bow_on_fore, LaneEnd::Fore, false),
        (Arc::BroadsideArc, bow_on_fore, LaneEnd::Aft, false),
        (Arc::BroadsideArc, broadside, LaneEnd::Fore, true),
        (Arc::BroadsideArc, broadside, LaneEnd::Aft, true),
    ];
    for &(arc, o, end, want) in cases {
        let got = arc_bears(o, arc, end);
        assert_eq!(
            got, want,
            "arc_bears({arc:?}, {o:?}, {end:?}) = {got}, expected {want}",
        );
    }
}

#[test]
fn arc_bears_forward_and_rear_track_the_bow_direction() {
    // When bow points aft, the forward gun fires aft and the rear gun
    // fires fore. This is the bow-flip symmetry that the inline test
    // doesn't cover.
    let bow_aft = Orientation::BowOn { bow: LaneEnd::Aft };
    assert!(arc_bears(bow_aft, Arc::Forward, LaneEnd::Aft));
    assert!(!arc_bears(bow_aft, Arc::Forward, LaneEnd::Fore));
    assert!(arc_bears(bow_aft, Arc::Rear, LaneEnd::Fore));
    assert!(!arc_bears(bow_aft, Arc::Rear, LaneEnd::Aft));
}

/* =========================================================================
 * bears — the ship-level convenience wrapper
 * ====================================================================== */

#[test]
fn bears_with_none_arc_always_returns_true() {
    // Arc-less actions (SELF, DEPLOYED_CELL) skip the gate entirely.
    let s = ship_at(3, Orientation::BowOn { bow: LaneEnd::Fore });
    assert!(bears(&s, None, 0));
    assert!(bears(&s, None, 3));
    assert!(bears(&s, None, 6));
}

#[test]
fn bears_resolves_direction_from_ship_cell_to_target_cell() {
    // Ship at cell 3, bow-on facing fore. Forward arc should bear on
    // targets at cells > 3 (Fore direction) and not on targets at < 3.
    // Equal-cell case maps to Fore per direction_to.
    let s = ship_at(3, Orientation::BowOn { bow: LaneEnd::Fore });
    assert!(bears(&s, Some(Arc::Forward), 5));
    assert!(bears(&s, Some(Arc::Forward), 3)); // equal -> Fore
    assert!(!bears(&s, Some(Arc::Forward), 1));
    assert!(bears(&s, Some(Arc::Rear), 1));
    assert!(!bears(&s, Some(Arc::Rear), 5));
}

#[test]
fn bears_broadside_arc_fires_in_either_lane_direction_when_turned_broadside() {
    let s = ship_at(3, Orientation::Broadside);
    assert!(bears(&s, Some(Arc::BroadsideArc), 0));
    assert!(bears(&s, Some(Arc::BroadsideArc), 6));
}

/* =========================================================================
 * absorb_shield — the charge-then-armour state machine
 * ====================================================================== */

#[test]
fn absorb_shield_zero_damage_short_circuits_without_consuming_charge() {
    let mut f = ShieldFace {
        armour: 5,
        charge: 2,
    };
    assert_eq!(absorb_shield(&mut f, 0), 0);
    assert_eq!(
        f.charge, 2,
        "zero-damage hit must not consume a shield charge"
    );
}

#[test]
fn absorb_shield_negative_damage_short_circuits_without_consuming_charge() {
    // The TS guard is `if (dmg <= 0)`. Negative damage shouldn't happen in
    // practice (band_falloff floors at 0) but the contract is "no charge
    // consumed unless damage was actually inflicted".
    let mut f = ShieldFace {
        armour: 5,
        charge: 2,
    };
    assert_eq!(absorb_shield(&mut f, -3), 0);
    assert_eq!(f.charge, 2);
}

#[test]
fn absorb_shield_charge_negates_arbitrarily_large_hit() {
    // One shield charge eats one hit entirely, regardless of magnitude.
    let mut f = ShieldFace {
        armour: 0,
        charge: 1,
    };
    assert_eq!(absorb_shield(&mut f, 999), 0);
    assert_eq!(f.charge, 0);
}

#[test]
fn absorb_shield_charge_consumes_exactly_one_per_hit() {
    let mut f = ShieldFace {
        armour: 0,
        charge: 3,
    };
    absorb_shield(&mut f, 4);
    assert_eq!(f.charge, 2);
    absorb_shield(&mut f, 4);
    assert_eq!(f.charge, 1);
    absorb_shield(&mut f, 4);
    assert_eq!(f.charge, 0);
    // Next hit falls through to armour (which is 0) and bleeds full damage.
    assert_eq!(absorb_shield(&mut f, 4), 4);
}

#[test]
fn absorb_shield_armour_subtracts_when_no_charge() {
    let mut f = ShieldFace {
        armour: 2,
        charge: 0,
    };
    assert_eq!(absorb_shield(&mut f, 5), 3);
    // Armour is permanent — it does NOT decrement on a hit.
    assert_eq!(f.armour, 2);
}

#[test]
fn absorb_shield_armour_clamps_at_zero_when_overkill() {
    // damage 2, armour 5 -> 0 (not -3). TS uses Math.max(0, dmg - armour).
    let mut f = ShieldFace {
        armour: 5,
        charge: 0,
    };
    assert_eq!(absorb_shield(&mut f, 2), 0);
}

#[test]
fn absorb_shield_armour_equal_to_damage_returns_zero() {
    // Boundary case: armour exactly meets the damage.
    let mut f = ShieldFace {
        armour: 3,
        charge: 0,
    };
    assert_eq!(absorb_shield(&mut f, 3), 0);
}

#[test]
fn absorb_shield_charge_is_checked_before_armour() {
    // If both charge and armour are present, charge eats the hit FIRST.
    // If the order were reversed, this hit would be reduced to 8 (10-2)
    // by armour and the charge would not be consumed.
    let mut f = ShieldFace {
        armour: 2,
        charge: 1,
    };
    assert_eq!(
        absorb_shield(&mut f, 10),
        0,
        "charge should negate before armour applies"
    );
    assert_eq!(f.charge, 0);
    assert_eq!(f.armour, 2);
}

/* =========================================================================
 * default_shield_profile — the starting Frigate hull
 * ====================================================================== */

#[test]
fn default_shield_profile_is_the_canonical_frigate_layout() {
    let p: ShieldProfile = default_shield_profile();
    // Strong bow, weak stern, medium flanks — the Frigate doc shape.
    assert_eq!(
        p.bow,
        ShieldFace {
            armour: 2,
            charge: 0
        }
    );
    assert_eq!(
        p.stern,
        ShieldFace {
            armour: 0,
            charge: 0
        }
    );
    assert_eq!(
        p.port,
        ShieldFace {
            armour: 1,
            charge: 0
        }
    );
    assert_eq!(
        p.starboard,
        ShieldFace {
            armour: 1,
            charge: 0
        }
    );
}

#[test]
fn default_shield_profile_starts_with_no_held_charges() {
    // Held charges come from Brace etc. at runtime; the catalog shape has
    // none.
    let p = default_shield_profile();
    assert_eq!(p.bow.charge, 0);
    assert_eq!(p.stern.charge, 0);
    assert_eq!(p.port.charge, 0);
    assert_eq!(p.starboard.charge, 0);
}
