//! Spatial core: pure functions over the 1-D lane.
//!
//! Port of `broadside-engine/engine/geometry.ts`. Orientation, arcs, range
//! bands, and directional shield absorption all live here. No randomness, no
//! content lookups — just geometry. The TypeScript engine is the canonical
//! reference; when this port and the TS disagree, the TS is right.

use crate::types::{Arc, HullZone, LaneEnd, Orientation, RangeBand, ShieldFace, ShieldProfile, Ship};

/// `fore` is toward higher cell index; `opposite(fore) = aft` and vice versa.
pub fn opposite(end: LaneEnd) -> LaneEnd {
    match end {
        LaneEnd::Fore => LaneEnd::Aft,
        LaneEnd::Aft => LaneEnd::Fore,
    }
}

/// Direction one must travel to get FROM `a` TO `b` along the lane.
/// Mirrors `directionTo` exactly: `b >= a` returns `Fore`, so `a == b` is `Fore`.
pub fn direction_to(a: usize, b: usize) -> LaneEnd {
    if b >= a {
        LaneEnd::Fore
    } else {
        LaneEnd::Aft
    }
}

/// Cell distance between two lane positions.
pub fn distance(a: usize, b: usize) -> usize {
    a.abs_diff(b)
}

/* ---- range bands ----------------------------------------------------------- */

/// The canonical band ordering. Indexes here drive `band_falloff`'s delta math
/// and MUST match `RangeBand`'s declaration order in `types.rs`.
const BAND_ORDER: [RangeBand; 5] = [
    RangeBand::PointBlank,
    RangeBand::Close,
    RangeBand::Mid,
    RangeBand::Long,
    RangeBand::Extreme,
];

fn band_index(b: RangeBand) -> usize {
    BAND_ORDER.iter().position(|x| *x == b).expect("BAND_ORDER covers every RangeBand")
}

/// Bucket a cell distance into a range band. Matches the doc's band ruler.
pub fn range_band(attacker_cell: usize, target_cell: usize) -> RangeBand {
    let d = distance(attacker_cell, target_cell);
    if d <= 1 {
        RangeBand::PointBlank
    } else if d == 2 {
        RangeBand::Close
    } else if d <= 4 {
        RangeBand::Mid
    } else if d <= 6 {
        RangeBand::Long
    } else {
        RangeBand::Extreme
    }
}

/// Falloff for firing outside the weapon's optimal band: delta 0 = full, then
/// ~⅔, ½, ⅓, ⅕, floored at 0. Tunable table preserved 1:1 from the TS.
pub fn band_falloff(raw: i32, actual: RangeBand, optimal: RangeBand) -> i32 {
    let delta = (band_index(actual) as i32 - band_index(optimal) as i32).unsigned_abs() as usize;
    let factors = [1.0_f64, 0.66, 0.5, 0.33, 0.2];
    let factor = factors[delta.min(4)];
    let scaled = (raw as f64 * factor).floor() as i32;
    scaled.max(0)
}

/// Is the target within this weapon's allowed bands at the current distance?
pub fn in_band(allowed: &[RangeBand], attacker_cell: usize, target_cell: usize) -> bool {
    allowed.contains(&range_band(attacker_cell, target_cell))
}

/* ---- orientation: which hull zone faces a given lane direction ------------- */

/// Given a hit arriving FROM `incoming_from` (the direction pointing from the
/// target back toward the attacker), return which fixed hull zone takes it.
///
/// Bow-on: the bow faces its `bow` end (strong), the stern the opposite (weak);
/// the flanks point off-lane and never eat a lane hit.
/// Broadside: lane hits always land on a flank. Split deterministically across
/// starboard/port (fore -> starboard, aft -> port) so the model is stable.
pub fn facing_zone(o: Orientation, incoming_from: LaneEnd) -> HullZone {
    match o {
        Orientation::BowOn { bow } => {
            if incoming_from == bow {
                HullZone::Bow
            } else {
                HullZone::Stern
            }
        }
        Orientation::Broadside => {
            if incoming_from == LaneEnd::Fore {
                HullZone::Starboard
            } else {
                HullZone::Port
            }
        }
    }
}

/* ---- arcs: can a mount of this arc bear on a lane direction ----------------- */

/// Does a mount with `arc` currently bear on something lying toward `toward_end`,
/// given the ship's orientation? This is the gate that makes facing matter:
/// a forward gun only fires out the bow, a broadside battery only fires when
/// the hull is turned across the lane, a turret always bears, a rear gun fires
/// astern.
pub fn arc_bears(o: Orientation, arc: Arc, toward_end: LaneEnd) -> bool {
    match arc {
        Arc::Turret => true,
        Arc::Forward => matches!(o, Orientation::BowOn { bow } if toward_end == bow),
        Arc::Rear => matches!(o, Orientation::BowOn { bow } if toward_end == opposite(bow)),
        // Both flanks face the lane only when turned broadside; then it fires
        // both ways.
        Arc::BroadsideArc => matches!(o, Orientation::Broadside),
    }
}

/// Does the ship have ANY orientation-legal bearing for this arc toward the
/// target cell? `None` arc means arc-less (SELF / DEPLOYED_CELL) and always
/// bears.
pub fn bears(ship: &Ship, arc: Option<Arc>, target_cell: usize) -> bool {
    match arc {
        None => true,
        Some(a) => arc_bears(ship.orientation, a, direction_to(ship.cell, target_cell)),
    }
}

/* ---- directional shield absorption ----------------------------------------- */

/// Run incoming damage through one hull zone's defence. A held shield `charge`
/// negates the hit entirely and is consumed; otherwise the zone's permanent
/// `armour` is subtracted. Mutates `face` (charge consumption) and returns the
/// damage that reaches hull.
pub fn absorb_shield(face: &mut ShieldFace, dmg: i32) -> i32 {
    if dmg <= 0 {
        return 0;
    }
    if face.charge > 0 {
        face.charge -= 1;
        return 0;
    }
    (dmg - face.armour).max(0)
}

/* ---- a default hull, for tests and the starting Frigate -------------------- */

/// The starting Frigate's hull: strong bow (2), weak stern (0), medium flanks (1).
/// Mirrors `defaultShieldProfile` in `geometry.ts` and matches the shape used
/// throughout `demo.ts`.
pub fn default_shield_profile() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 2, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 1, charge: 0 },
        starboard: ShieldFace { armour: 1, charge: 0 },
    }
}

/* =============================================================================
 * Tests — one sanity assert per pure function. Deeper coverage comes from
 * `broadside-tester`.
 * ========================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_swaps_ends() {
        assert_eq!(opposite(LaneEnd::Fore), LaneEnd::Aft);
        assert_eq!(opposite(LaneEnd::Aft), LaneEnd::Fore);
    }

    #[test]
    fn direction_to_treats_equal_cells_as_fore() {
        // TS: `b >= a ? "fore" : "aft"` — equal maps to fore. Easy to miss.
        assert_eq!(direction_to(3, 3), LaneEnd::Fore);
        assert_eq!(direction_to(2, 5), LaneEnd::Fore);
        assert_eq!(direction_to(5, 2), LaneEnd::Aft);
    }

    #[test]
    fn distance_is_absolute() {
        assert_eq!(distance(2, 5), 3);
        assert_eq!(distance(5, 2), 3);
        assert_eq!(distance(4, 4), 0);
    }

    #[test]
    fn range_band_buckets_match_the_ruler() {
        assert_eq!(range_band(0, 0), RangeBand::PointBlank);
        assert_eq!(range_band(0, 1), RangeBand::PointBlank);
        assert_eq!(range_band(0, 2), RangeBand::Close);
        // distance 3 -> mid (the canonical sanity check)
        assert_eq!(range_band(0, 3), RangeBand::Mid);
        assert_eq!(range_band(0, 4), RangeBand::Mid);
        assert_eq!(range_band(0, 5), RangeBand::Long);
        assert_eq!(range_band(0, 6), RangeBand::Long);
        assert_eq!(range_band(0, 7), RangeBand::Extreme);
    }

    #[test]
    fn band_falloff_full_damage_when_actual_equals_optimal() {
        assert_eq!(band_falloff(10, RangeBand::Close, RangeBand::Close), 10);
    }

    #[test]
    fn band_falloff_drops_off_outside_optimal_band() {
        // delta 1 => factor 0.66 => floor(4 * 0.66) = 2
        assert_eq!(band_falloff(4, RangeBand::Mid, RangeBand::Close), 2);
        // delta 4 => factor 0.2 => floor(10 * 0.2) = 2
        assert_eq!(band_falloff(10, RangeBand::Extreme, RangeBand::PointBlank), 2);
    }

    #[test]
    fn band_falloff_floors_negative_inputs_at_zero() {
        assert_eq!(band_falloff(-5, RangeBand::Mid, RangeBand::Mid), 0);
    }

    #[test]
    fn in_band_respects_allowed_set() {
        let allowed = [RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid];
        assert!(in_band(&allowed, 0, 1)); // pointBlank
        assert!(in_band(&allowed, 0, 3)); // mid
        assert!(!in_band(&allowed, 0, 5)); // long is not in the set
    }

    #[test]
    fn facing_zone_bow_on_routes_lane_hits_to_bow_or_stern() {
        let o = Orientation::BowOn { bow: LaneEnd::Fore };
        // Incoming from fore lines up with the bow.
        assert_eq!(facing_zone(o, LaneEnd::Fore), HullZone::Bow);
        // Incoming from aft hits the stern.
        assert_eq!(facing_zone(o, LaneEnd::Aft), HullZone::Stern);
    }

    #[test]
    fn facing_zone_broadside_routes_to_flanks_deterministically() {
        let o = Orientation::Broadside;
        assert_eq!(facing_zone(o, LaneEnd::Fore), HullZone::Starboard);
        assert_eq!(facing_zone(o, LaneEnd::Aft), HullZone::Port);
    }

    #[test]
    fn arc_bears_turret_always_bears() {
        let o = Orientation::BowOn { bow: LaneEnd::Fore };
        assert!(arc_bears(o, Arc::Turret, LaneEnd::Fore));
        assert!(arc_bears(o, Arc::Turret, LaneEnd::Aft));
        assert!(arc_bears(Orientation::Broadside, Arc::Turret, LaneEnd::Fore));
    }

    #[test]
    fn arc_bears_forward_only_fires_out_the_bow() {
        let o = Orientation::BowOn { bow: LaneEnd::Fore };
        assert!(arc_bears(o, Arc::Forward, LaneEnd::Fore));
        assert!(!arc_bears(o, Arc::Forward, LaneEnd::Aft));
        // Forward never bears when the hull is broadside.
        assert!(!arc_bears(Orientation::Broadside, Arc::Forward, LaneEnd::Fore));
    }

    #[test]
    fn arc_bears_rear_only_fires_astern() {
        let o = Orientation::BowOn { bow: LaneEnd::Fore };
        assert!(!arc_bears(o, Arc::Rear, LaneEnd::Fore));
        assert!(arc_bears(o, Arc::Rear, LaneEnd::Aft));
    }

    #[test]
    fn arc_bears_broadside_only_when_turned_broadside() {
        assert!(arc_bears(Orientation::Broadside, Arc::BroadsideArc, LaneEnd::Fore));
        assert!(arc_bears(Orientation::Broadside, Arc::BroadsideArc, LaneEnd::Aft));
        let o = Orientation::BowOn { bow: LaneEnd::Fore };
        assert!(!arc_bears(o, Arc::BroadsideArc, LaneEnd::Fore));
    }

    #[test]
    fn absorb_shield_charge_negates_hit_and_decrements() {
        let mut face = ShieldFace { armour: 5, charge: 1 };
        let through = absorb_shield(&mut face, 10);
        assert_eq!(through, 0);
        assert_eq!(face.charge, 0);
    }

    #[test]
    fn absorb_shield_falls_back_to_armour_when_no_charge() {
        let mut face = ShieldFace { armour: 2, charge: 0 };
        let through = absorb_shield(&mut face, 5);
        assert_eq!(through, 3);
        // Armour is permanent — unchanged.
        assert_eq!(face.armour, 2);
    }

    #[test]
    fn absorb_shield_clamps_when_armour_exceeds_damage() {
        let mut face = ShieldFace { armour: 5, charge: 0 };
        assert_eq!(absorb_shield(&mut face, 2), 0);
    }

    #[test]
    fn absorb_shield_ignores_non_positive_damage() {
        let mut face = ShieldFace { armour: 5, charge: 3 };
        assert_eq!(absorb_shield(&mut face, 0), 0);
        // Charge must not be consumed if there was nothing to absorb.
        assert_eq!(face.charge, 3);
    }

    #[test]
    fn default_shield_profile_matches_the_doc() {
        let p = default_shield_profile();
        assert_eq!(*p.face(HullZone::Bow), ShieldFace { armour: 2, charge: 0 });
        assert_eq!(*p.face(HullZone::Stern), ShieldFace { armour: 0, charge: 0 });
        assert_eq!(*p.face(HullZone::Port), ShieldFace { armour: 1, charge: 0 });
        assert_eq!(*p.face(HullZone::Starboard), ShieldFace { armour: 1, charge: 0 });
    }
}
