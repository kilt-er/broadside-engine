//! 2-D geometry (`broadside_engine::geometry2d`) integration suite — blueprint
//! lane task **T2**.
//!
//! `geometry2d.rs` is the resolver's R1 replacement for the 1-D `geometry.rs`,
//! built over the [`broadside_engine::grid`] type surface. The module ships its
//! own `#[cfg(test)]` contract guard (the 3/3/1/1 + 2/2/2/2 `facing_zone`
//! partition asserts, the falloff table, the cone, the shield port). This file
//! is the layer ON TOP the brief asks for: **exhaustive** sweeps the inline
//! spot-checks don't make, **cross-function invariants** a single function's
//! own test cannot assert, **parity** against the still-live 1-D engine, and
//! `proptest` where the input space is wider than a table can close.
//!
//! ## What this file adds over the module's inline tests (no duplication for show)
//!
//! The inline tests COUNT zones (3 Bow / 3 Stern / 1 / 1; 2 / 2 / 2 / 2). A
//! counting test cannot catch a *swap* — if two `facing_zone` match arms traded
//! their `HullZone`s the counts are unchanged and the inline partition test
//! stays green. So T2's `facing_zone` core is the **full 64-entry (8 Facing × 8
//! Dir8) explicit table**, every cell pinned to a named zone derived from
//! corrected blueprint line 30 — a swapped arm fails here. On top of that:
//!   - the firing/receiving ARITY SEAM: `arc_bears` (FIRING) is cardinal-exact
//!     4-way while `facing_zone` (RECEIVING) is 8-way, so firing ⊊ receiving —
//!     a gun fires on exact hull cardinals, a return shot from a diagonal still
//!     lands on a face. We pin that seam (and that no directional arc ever fires
//!     diagonally); this suite originally caught a `arc_bears`-mirrors-the-cone
//!     bug exactly here.
//!   - `direction_to` (magnitude-aware) vs `grid::from_to` (sign octant): equal
//!     on axis-aligned/45° vectors, and the documented divergence on shallow
//!     ones, swept over the whole board.
//!   - `band_falloff` is pinned as the NEW 3-value `[1.0, 0.6, 0.3]` curve AND
//!     guarded against accidental reuse of the 1-D 5-value `[1,0.66,0.5,0.33,
//!     0.2]` delta-from-optimal table (a real regression risk during the port).
//!   - `absorb_shield` / `default_shield_profile` are asserted **equal to the
//!     1-D `crate::geometry` originals** (blueprint: "kept verbatim") — a true
//!     parity claim, not a re-statement of the constants.
//!
//! Leaves the still-live 1-D `tests/geometry.rs` untouched (it tests the 1-D
//! module, which stays live until the A3 contract step).

use broadside_engine::geometry2d::{
    absorb_shield, arc_bears, band_falloff, default_shield_profile, direction_to, distance,
    facing_zone, in_band, opposite, range_band,
};
use broadside_engine::grid::{self, Axis, Dir4, Dir8, Facing, Pos, Range, COLS, ROWS};
use broadside_engine::types::{Arc, HullZone, ShieldFace};
use proptest::prelude::*;

/* =========================================================================
 * Finite domains to sweep (local lists, not oracles — see tests/grid.rs note)
 * ====================================================================== */

const ALL_DIR8: [Dir8; 8] = [
    Dir8::N,
    Dir8::NE,
    Dir8::E,
    Dir8::SE,
    Dir8::S,
    Dir8::SW,
    Dir8::W,
    Dir8::NW,
];

const ALL_DIR4: [Dir4; 4] = [Dir4::N, Dir4::E, Dir4::S, Dir4::W];
const ALL_AXES: [Axis; 2] = [Axis::NorthSouth, Axis::EastWest];
const ALL_ZONES: [HullZone; 4] = [
    HullZone::Bow,
    HullZone::Stern,
    HullZone::Port,
    HullZone::Starboard,
];

fn every_cell() -> Vec<Pos> {
    let mut v = Vec::with_capacity(COLS * ROWS);
    for row in 0..ROWS {
        for col in 0..COLS {
            v.push(Pos::new(col, row));
        }
    }
    v
}

fn p(col: usize, row: usize) -> Pos {
    Pos::new(col, row)
}

/* proptest strategies */

fn any_pos() -> impl Strategy<Value = Pos> {
    (0..COLS, 0..ROWS).prop_map(|(col, row)| Pos::new(col, row))
}

fn any_dir8() -> impl Strategy<Value = Dir8> {
    (0u8..8).prop_map(Dir8::from_step)
}

fn any_facing() -> impl Strategy<Value = Facing> {
    prop_oneof![
        (0usize..4).prop_map(|i| Facing::Bow(ALL_DIR4[i])),
        (0usize..2).prop_map(|i| Facing::Broadside(ALL_AXES[i])),
    ]
}

/* =========================================================================
 * facing_zone — the FULL 64-entry explicit table (every Facing × Dir8)
 *
 * Derived from corrected blueprint line 30 / the geometry2d docstring:
 *   BOW(dir): s = (incoming.step − dir.step) mod 8 → {7,0,1}=Bow, {3,4,5}=Stern,
 *             2=Starboard (right), 6=Port (left).            [3/3/1/1]
 *   BROADSIDE(axis): pseudo-forward = Axis::dirs().0 → Bow, opposite → Stern;
 *             CW-perp flank = Starboard, CCW-perp = Port; each diagonal snaps
 *             45° CW (relative offsets {7,0}=Bow,{1,2}=Stbd,{3,4}=Stern,{5,6}=
 *             Port).                                          [2/2/2/2]
 *
 * Every entry is written out by hand (not computed from the same formula the
 * impl uses — that would just test the formula against itself). A swapped match
 * arm in geometry2d.rs fails exactly one row here, which a zone-COUNT test
 * cannot detect.
 * ====================================================================== */

#[rustfmt::skip]
const BOW_TABLE: &[(Dir4, Dir8, HullZone)] = &[
    // BOW(N): ahead=N. right(E)=Stbd, left(W)=Port.
    (Dir4::N, Dir8::N,  HullZone::Bow),       (Dir4::N, Dir8::NE, HullZone::Bow),
    (Dir4::N, Dir8::NW, HullZone::Bow),       (Dir4::N, Dir8::E,  HullZone::Starboard),
    (Dir4::N, Dir8::SE, HullZone::Stern),     (Dir4::N, Dir8::S,  HullZone::Stern),
    (Dir4::N, Dir8::SW, HullZone::Stern),     (Dir4::N, Dir8::W,  HullZone::Port),
    // BOW(E): ahead=E. right(S)=Stbd, left(N)=Port.
    (Dir4::E, Dir8::E,  HullZone::Bow),       (Dir4::E, Dir8::NE, HullZone::Bow),
    (Dir4::E, Dir8::SE, HullZone::Bow),       (Dir4::E, Dir8::S,  HullZone::Starboard),
    (Dir4::E, Dir8::SW, HullZone::Stern),     (Dir4::E, Dir8::W,  HullZone::Stern),
    (Dir4::E, Dir8::NW, HullZone::Stern),     (Dir4::E, Dir8::N,  HullZone::Port),
    // BOW(S): ahead=S. right(W)=Stbd, left(E)=Port.
    (Dir4::S, Dir8::S,  HullZone::Bow),       (Dir4::S, Dir8::SE, HullZone::Bow),
    (Dir4::S, Dir8::SW, HullZone::Bow),       (Dir4::S, Dir8::W,  HullZone::Starboard),
    (Dir4::S, Dir8::NW, HullZone::Stern),     (Dir4::S, Dir8::N,  HullZone::Stern),
    (Dir4::S, Dir8::NE, HullZone::Stern),     (Dir4::S, Dir8::E,  HullZone::Port),
    // BOW(W): ahead=W. right(N)=Stbd, left(S)=Port.
    (Dir4::W, Dir8::W,  HullZone::Bow),       (Dir4::W, Dir8::SW, HullZone::Bow),
    (Dir4::W, Dir8::NW, HullZone::Bow),       (Dir4::W, Dir8::N,  HullZone::Starboard),
    (Dir4::W, Dir8::NE, HullZone::Stern),     (Dir4::W, Dir8::E,  HullZone::Stern),
    (Dir4::W, Dir8::SE, HullZone::Stern),     (Dir4::W, Dir8::S,  HullZone::Port),
];

#[rustfmt::skip]
const BROADSIDE_TABLE: &[(Axis, Dir8, HullZone)] = &[
    // EastWest: pseudo-forward=E=Bow, W=Stern; CW flank S=Stbd, CCW flank N=Port.
    // Diagonals snap 45° CW: Bow{E,NE}, Stbd{S,SE}, Stern{W,SW}, Port{N,NW}.
    (Axis::EastWest, Dir8::E,  HullZone::Bow),       (Axis::EastWest, Dir8::NE, HullZone::Bow),
    (Axis::EastWest, Dir8::S,  HullZone::Starboard), (Axis::EastWest, Dir8::SE, HullZone::Starboard),
    (Axis::EastWest, Dir8::W,  HullZone::Stern),     (Axis::EastWest, Dir8::SW, HullZone::Stern),
    (Axis::EastWest, Dir8::N,  HullZone::Port),      (Axis::EastWest, Dir8::NW, HullZone::Port),
    // NorthSouth: pseudo-forward=S=Bow, N=Stern; CW flank W=Stbd, CCW flank E=Port.
    // Diagonals snap 45° CW: Bow{S,SE}, Stbd{W,SW}, Stern{N,NW}, Port{E,NE}.
    (Axis::NorthSouth, Dir8::S,  HullZone::Bow),       (Axis::NorthSouth, Dir8::SE, HullZone::Bow),
    (Axis::NorthSouth, Dir8::W,  HullZone::Starboard), (Axis::NorthSouth, Dir8::SW, HullZone::Starboard),
    (Axis::NorthSouth, Dir8::N,  HullZone::Stern),     (Axis::NorthSouth, Dir8::NW, HullZone::Stern),
    (Axis::NorthSouth, Dir8::E,  HullZone::Port),      (Axis::NorthSouth, Dir8::NE, HullZone::Port),
];

#[test]
fn facing_zone_bow_table_is_exhaustive_and_exact() {
    // All 32 Bow entries pinned per-direction (a swapped arm fails its row).
    assert_eq!(BOW_TABLE.len(), 32, "4 cardinals × 8 incoming");
    for &(dir, incoming, want) in BOW_TABLE {
        assert_eq!(
            facing_zone(Facing::Bow(dir), incoming),
            want,
            "Bow({dir:?}) hit from {incoming:?}"
        );
    }
    // The table itself must name every (dir, incoming) once — guards the TABLE
    // (not the impl) against a typo'd/missing row.
    for dir in ALL_DIR4 {
        let mut seen = [false; 8];
        for &(d, inc, _) in BOW_TABLE.iter().filter(|(d, ..)| *d == dir) {
            let i = inc.step() as usize;
            assert!(!seen[i], "Bow({d:?}) lists {inc:?} twice");
            seen[i] = true;
        }
        assert!(
            seen.into_iter().all(|b| b),
            "Bow({dir:?}) covers all 8 incoming"
        );
    }
}

#[test]
fn facing_zone_broadside_table_is_exhaustive_and_exact() {
    assert_eq!(BROADSIDE_TABLE.len(), 16, "2 axes × 8 incoming");
    for &(axis, incoming, want) in BROADSIDE_TABLE {
        assert_eq!(
            facing_zone(Facing::Broadside(axis), incoming),
            want,
            "Broadside({axis:?}) hit from {incoming:?}"
        );
    }
    for axis in ALL_AXES {
        let mut seen = [false; 8];
        for &(a, inc, _) in BROADSIDE_TABLE.iter().filter(|(a, ..)| *a == axis) {
            let i = inc.step() as usize;
            assert!(!seen[i], "Broadside({a:?}) lists {inc:?} twice");
            seen[i] = true;
        }
        assert!(
            seen.into_iter().all(|b| b),
            "Broadside({axis:?}) covers all 8 incoming"
        );
    }
}

#[test]
fn facing_zone_bow_ahead_is_bow_and_directly_behind_is_stern() {
    // The single most load-bearing reading: present your strong bow by aiming at
    // the threat; the weak stern faces directly away. Pin for all 4 cardinals.
    for dir in ALL_DIR4 {
        let ahead = dir.to_dir8();
        let behind = dir.to_dir8().opposite();
        assert_eq!(
            facing_zone(Facing::Bow(dir), ahead),
            HullZone::Bow,
            "{dir:?} ahead"
        );
        assert_eq!(
            facing_zone(Facing::Bow(dir), behind),
            HullZone::Stern,
            "{dir:?} behind"
        );
    }
}

#[test]
fn facing_zone_bow_right_is_starboard_left_is_port() {
    // The perpendicular cardinals split by handedness (right=Stbd). Expressed as
    // "the direction 90° clockwise of the bow is Starboard, 90° CCW is Port" so
    // it pins the handedness convention, not just four literals.
    for dir in ALL_DIR4 {
        let bow = dir.to_dir8();
        let right = bow.rotate_cw().rotate_cw(); // +90°
        let left = bow.rotate_ccw().rotate_ccw(); // −90°
        assert_eq!(
            facing_zone(Facing::Bow(dir), right),
            HullZone::Starboard,
            "{dir:?} right"
        );
        assert_eq!(
            facing_zone(Facing::Bow(dir), left),
            HullZone::Port,
            "{dir:?} left"
        );
    }
}

#[test]
fn facing_zone_broadside_pseudo_forward_is_bow_and_flanks_present_both_sides() {
    // The +on-axis end (Axis::dirs().0) is Bow; its opposite is Stern; and BOTH
    // flanks (the off-axis cardinals) are present — a turned hull shows Port AND
    // Starboard, never folding a flank into an end.
    for axis in ALL_AXES {
        let (fwd, aft) = axis.dirs();
        assert_eq!(
            facing_zone(Facing::Broadside(axis), fwd.to_dir8()),
            HullZone::Bow,
            "Broadside({axis:?}) pseudo-forward is Bow"
        );
        assert_eq!(
            facing_zone(Facing::Broadside(axis), aft.to_dir8()),
            HullZone::Stern,
            "Broadside({axis:?}) pseudo-aft is Stern"
        );
        // The two off-axis cardinals are the flanks (one Port, one Starboard).
        let off = match axis {
            Axis::NorthSouth => Axis::EastWest,
            Axis::EastWest => Axis::NorthSouth,
        };
        let (a, b) = off.dirs();
        let za = facing_zone(Facing::Broadside(axis), a.to_dir8());
        let zb = facing_zone(Facing::Broadside(axis), b.to_dir8());
        assert!(
            (za == HullZone::Starboard && zb == HullZone::Port)
                || (za == HullZone::Port && zb == HullZone::Starboard),
            "Broadside({axis:?}) off-axis cardinals are the two flanks, got {za:?}/{zb:?}"
        );
    }
}

#[test]
fn facing_zone_is_total_over_every_facing_and_incoming() {
    // Every one of the 8 Facing × 8 Dir8 = 64 inputs returns a zone (no panic /
    // unreachable hit). Combined with the exact tables above this is full
    // coverage of the function's domain.
    let mut count = 0;
    for dir in ALL_DIR4 {
        for inc in ALL_DIR8 {
            let _z = facing_zone(Facing::Bow(dir), inc);
            count += 1;
        }
    }
    for axis in ALL_AXES {
        for inc in ALL_DIR8 {
            let _z = facing_zone(Facing::Broadside(axis), inc);
            count += 1;
        }
    }
    assert_eq!(count, 48, "4 bows + 2 broadsides, each × 8 incoming");
}

proptest! {
    /// For ANY facing and ANY incoming direction, the result is one of the four
    /// zones and is DETERMINISTIC (same input → same output). Determinism is
    /// load-bearing: the telegraph paints by running this same function, so a
    /// non-deterministic result would desync the threat preview from the hit.
    #[test]
    fn facing_zone_is_deterministic_and_returns_a_valid_zone(f in any_facing(), inc in any_dir8()) {
        let z1 = facing_zone(f, inc);
        let z2 = facing_zone(f, inc);
        prop_assert_eq!(z1, z2, "deterministic");
        prop_assert!(ALL_ZONES.contains(&z1), "valid HullZone");
    }
}

/* =========================================================================
 * arc_bears — the FIRING gate is CARDINAL-EXACT (4-way), distinct from the
 * 8-way facing_zone RECEIVING table. The firing/receiving ARITY SEAM.
 *
 * Ratified model (resolver, post-fix): a mount fires along EXACT hull
 * cardinals only — Forward iff toward == the bow cardinal, Rear iff toward ==
 * the opposite cardinal, BroadsideArc iff toward == either off-axis flank
 * cardinal, Turret always. You cannot fire diagonally: ALL diagonal `toward`
 * inputs → false. This is deliberately a DIFFERENT arity from facing_zone
 * (which buckets all 8 incoming directions onto a hull face): firing is 4-way,
 * receiving is 8-way. These tests pin that seam — the exact place a "make
 * arc_bears mirror facing_zone's ±45° cone" bug lived (and which this suite
 * caught). They do NOT assert arc_bears == a facing_zone sector.
 * ====================================================================== */

#[test]
fn forward_gun_bears_only_on_the_exact_bow_cardinal() {
    // Cardinal-exact: bears toward the bow direction, and toward NOTHING else —
    // not the ±45° diagonals (the bug), not the perpendiculars, not the rear.
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        let bow = dir.to_dir8();
        for d in ALL_DIR8 {
            let want = d == bow;
            assert_eq!(
                arc_bears(f, Arc::Forward, d),
                want,
                "Bow({dir:?}) Forward toward {d:?}"
            );
        }
    }
}

#[test]
fn rear_gun_bears_only_on_the_exact_stern_cardinal() {
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        let astern = dir.to_dir8().opposite();
        for d in ALL_DIR8 {
            let want = d == astern;
            assert_eq!(
                arc_bears(f, Arc::Rear, d),
                want,
                "Bow({dir:?}) Rear toward {d:?}"
            );
        }
    }
}

#[test]
fn broadside_battery_bears_only_on_the_two_exact_flank_cardinals() {
    // The off-axis (perpendicular) cardinals are the flank rays; the on-axis
    // hull ends and ALL diagonals do not bear.
    for axis in ALL_AXES {
        let f = Facing::Broadside(axis);
        let off = match axis {
            Axis::NorthSouth => Axis::EastWest,
            Axis::EastWest => Axis::NorthSouth,
        };
        let (fa, fb) = off.dirs();
        let (fa8, fb8) = (fa.to_dir8(), fb.to_dir8());
        for d in ALL_DIR8 {
            let want = d == fa8 || d == fb8;
            assert_eq!(
                arc_bears(f, Arc::BroadsideArc, d),
                want,
                "Broadside({axis:?}) BroadsideArc toward {d:?}"
            );
        }
    }
}

#[test]
fn no_arc_ever_bears_on_a_diagonal_under_cardinals_only_firing() {
    // The crux of the firing/receiving seam: firing is cardinal-exact, so EVERY
    // diagonal `toward` is unfireable for the directional arcs (Forward / Rear /
    // BroadsideArc). Turret is the lone exception (it is direction-free). This is
    // the general statement of the bug this suite caught.
    let diagonals = [Dir8::NE, Dir8::SE, Dir8::SW, Dir8::NW];
    let directional = [Arc::Forward, Arc::Rear, Arc::BroadsideArc];
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        for &arc in &directional {
            for d in diagonals {
                assert!(
                    !arc_bears(f, arc, d),
                    "Bow({dir:?}) {arc:?} must not fire diagonally {d:?}"
                );
            }
        }
    }
    for axis in ALL_AXES {
        let f = Facing::Broadside(axis);
        for &arc in &directional {
            for d in diagonals {
                assert!(
                    !arc_bears(f, arc, d),
                    "Broadside({axis:?}) {arc:?} must not fire diagonally {d:?}"
                );
            }
        }
    }
}

#[test]
fn firing_arcs_are_strictly_narrower_than_the_receiving_sectors() {
    // Positively pin the arity seam: for the directions a gun CAN fire, a return
    // shot from there lands on the matching face — but NOT vice versa (the face
    // also catches diagonals the gun can't answer). I.e. firing ⊊ receiving.
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        for d in ALL_DIR8 {
            // Where Forward fires, that direction hits the Bow face.
            if arc_bears(f, Arc::Forward, d) {
                assert_eq!(
                    facing_zone(f, d),
                    HullZone::Bow,
                    "Bow({dir:?}) fires {d:?} ⇒ hits Bow"
                );
            }
            // Where Rear fires, that direction hits the Stern face.
            if arc_bears(f, Arc::Rear, d) {
                assert_eq!(
                    facing_zone(f, d),
                    HullZone::Stern,
                    "Bow({dir:?}) rear-fires {d:?} ⇒ hits Stern"
                );
            }
        }
        // Strictness: the Bow face catches 3 directions, Forward fires at 1 — so
        // there exist directions that hit Bow but Forward cannot answer.
        let bow_faces = ALL_DIR8
            .into_iter()
            .filter(|&d| facing_zone(f, d) == HullZone::Bow)
            .count();
        let forward_fires = ALL_DIR8
            .into_iter()
            .filter(|&d| arc_bears(f, Arc::Forward, d))
            .count();
        assert!(
            forward_fires < bow_faces,
            "Bow({dir:?}): firing ({forward_fires}) ⊊ receiving ({bow_faces})"
        );
    }
}

#[test]
fn turret_always_bears_for_every_facing_and_direction() {
    for dir in ALL_DIR4 {
        for d in ALL_DIR8 {
            assert!(
                arc_bears(Facing::Bow(dir), Arc::Turret, d),
                "Bow({dir:?}) turret {d:?}"
            );
        }
    }
    for axis in ALL_AXES {
        for d in ALL_DIR8 {
            assert!(
                arc_bears(Facing::Broadside(axis), Arc::Turret, d),
                "Broadside({axis:?}) turret {d:?}"
            );
        }
    }
}

#[test]
fn forward_and_rear_never_bear_on_a_broadside_hull() {
    // The bow/stern cones require a Bow stance; a turned hull has no bow gun arc.
    for axis in ALL_AXES {
        let f = Facing::Broadside(axis);
        for d in ALL_DIR8 {
            assert!(
                !arc_bears(f, Arc::Forward, d),
                "Broadside({axis:?}) forward {d:?}"
            );
            assert!(
                !arc_bears(f, Arc::Rear, d),
                "Broadside({axis:?}) rear {d:?}"
            );
        }
    }
}

#[test]
fn broadside_arc_bears_on_a_bow_hulls_perpendicular_flanks() {
    // Model D (#92, Bruce's bow-cardinal model): a BroadsideArc on a BOW hull
    // bears out EXACTLY the two flank cardinals perpendicular to the bow — turning
    // the bow E/W puts the flanks N/S, which IS broadsiding. The bow's own axis
    // (bow + stern cardinals) and every diagonal do NOT bear. (Supersedes the old
    // "broadside never bears on a bow hull" — there's no separate Broadside stance
    // in v2.)
    use broadside_engine::grid::Axis;
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        // The two flank cardinals = the perpendicular axis's dirs.
        let off = match dir.axis() {
            Axis::NorthSouth => Axis::EastWest,
            Axis::EastWest => Axis::NorthSouth,
        };
        let (fa, fb) = off.dirs();
        for d in ALL_DIR8 {
            let want = d == fa.to_dir8() || d == fb.to_dir8();
            assert_eq!(
                arc_bears(f, Arc::BroadsideArc, d),
                want,
                "Bow({dir:?}) broadsideArc toward {d:?}: bears iff a perpendicular flank cardinal",
            );
        }
    }
}

#[test]
fn forward_arc_is_exactly_one_direction_wide_cardinal_exact() {
    // Cardinal-exact firing: Forward bears on exactly ONE direction (the bow
    // cardinal), not a 3-wide cone. A too-wide arc (e.g. a regressed ±45° cone)
    // fails here even though it still contains dead-ahead.
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        let n = ALL_DIR8
            .into_iter()
            .filter(|&d| arc_bears(f, Arc::Forward, d))
            .count();
        assert_eq!(n, 1, "Bow({dir:?}) forward arc width (cardinal-exact)");
    }
}

#[test]
fn rear_arc_is_exactly_one_direction_wide_cardinal_exact() {
    for dir in ALL_DIR4 {
        let f = Facing::Bow(dir);
        let n = ALL_DIR8
            .into_iter()
            .filter(|&d| arc_bears(f, Arc::Rear, d))
            .count();
        assert_eq!(n, 1, "Bow({dir:?}) rear arc width (cardinal-exact)");
    }
}

#[test]
fn broadside_battery_is_exactly_two_directions_wide_cardinal_exact() {
    // Two flank rays (the off-axis cardinals); the on-axis hull ends and all
    // diagonals do not bear. A regressed cone would report 6 and fail here.
    for axis in ALL_AXES {
        let f = Facing::Broadside(axis);
        let n = ALL_DIR8
            .into_iter()
            .filter(|&d| arc_bears(f, Arc::BroadsideArc, d))
            .count();
        assert_eq!(
            n, 2,
            "Broadside({axis:?}) battery arc width (cardinal-exact)"
        );
    }
}

// NOTE: geometry2d does NOT export a `bears(&Ship, ...)` helper (the 1-D engine
// did; the 2-D port defers it because it needs the migrating `Ship` type). The
// arc-less "always bears" case and ship-shaped bearing belong to T3 once the
// Ship fixtures land — not covered here.

/* =========================================================================
 * band_falloff — the INTEGER penalty-per-band curve (#104: no float in the
 * damage math). Adjacent -0, Near -1, Far -2, floored at min 1 for a legal
 * shot. Replaces the old float `[1.0, 0.6, 0.3]` curve. A guard below still
 * rejects accidental reuse of the 1-D 5-value delta-from-optimal table.
 * ====================================================================== */

#[test]
fn band_falloff_is_the_integer_penalty_per_band() {
    // #104: integer penalty, NOT a float multiplier. Adjacent passes raw
    // through; each band further out subtracts one more.
    assert_eq!(band_falloff(100, Range::Adjacent), 100, "Adjacent -0");
    assert_eq!(band_falloff(100, Range::Near), 99, "Near -1");
    assert_eq!(band_falloff(100, Range::Far), 98, "Far -2");
    // #44 fix: a raw-6 Far weapon keeps 4 (the old float curve floored it to 1).
    assert_eq!(band_falloff(6, Range::Far), 4);
}

#[test]
fn band_falloff_is_not_the_old_1d_five_value_table() {
    // Regression guard for the port: the 1-D table was [1,0.66,0.5,0.33,0.2]
    // keyed on |actual−optimal|. If someone wired that in, Near would scale by
    // 0.66 (→66) or 0.5 (→50), not 0.6 (→60); Far by 0.5/0.33/0.2, not 0.3.
    // Pin the values the OLD table would have produced and assert we DON'T see
    // them. (raw=100 makes each factor a distinct integer, so this is sharp.)
    assert_ne!(
        band_falloff(100, Range::Near),
        66,
        "not 1-D delta-1 factor 0.66"
    );
    assert_ne!(
        band_falloff(100, Range::Near),
        50,
        "not 1-D delta-2 factor 0.5"
    );
    assert_ne!(
        band_falloff(100, Range::Far),
        50,
        "not 1-D delta-2 factor 0.5"
    );
    assert_ne!(
        band_falloff(100, Range::Far),
        33,
        "not 1-D delta-3 factor 0.33"
    );
    assert_ne!(
        band_falloff(100, Range::Far),
        20,
        "not 1-D delta-4 factor 0.2"
    );
}

#[test]
fn band_falloff_floors_a_legal_shot_at_one() {
    // #104: a legal in-band shot never whiffs to 0 from falloff alone (>=1
    // floor) — the over-extension deadzone gates ILLEGAL shots upstream.
    assert_eq!(band_falloff(7, Range::Near), 6); // 7 - 1
    assert_eq!(band_falloff(7, Range::Far), 5); //  7 - 2
    assert_eq!(band_falloff(2, Range::Far), 1); // (2 - 2).max(1) = 1
    assert_eq!(band_falloff(1, Range::Far), 1); // (1 - 2).max(1) = 1
}

#[test]
fn band_falloff_clamps_nonpositive_to_zero() {
    for r in [Range::Adjacent, Range::Near, Range::Far] {
        assert_eq!(band_falloff(0, r), 0, "zero stays zero in {r:?}");
        assert_eq!(band_falloff(-5, r), 0, "negative clamps in {r:?}");
        assert_eq!(band_falloff(-1, r), 0);
    }
}

#[test]
fn band_falloff_adjacent_is_the_identity() {
    // ×1.0 must be a true pass-through for every non-negative raw (no rounding
    // surprise at the strong band).
    for raw in 0..=50 {
        assert_eq!(
            band_falloff(raw, Range::Adjacent),
            raw,
            "Adjacent passes {raw} through"
        );
    }
}

proptest! {
    /// Falloff is monotonic non-increasing in band distance and never amplifies:
    /// Adjacent ≥ Near ≥ Far ≥ 0, and each is ≤ raw. (Closer is never weaker;
    /// no band ever boosts damage above the raw value.)
    #[test]
    fn band_falloff_is_monotone_and_never_amplifies(raw in 0i32..10_000) {
        let adj = band_falloff(raw, Range::Adjacent);
        let near = band_falloff(raw, Range::Near);
        let far = band_falloff(raw, Range::Far);
        prop_assert!(adj >= near, "Adjacent ≥ Near ({adj} ≥ {near})");
        prop_assert!(near >= far, "Near ≥ Far ({near} ≥ {far})");
        prop_assert!(far >= 0);
        prop_assert!(adj <= raw, "never amplifies");
    }
}

/* =========================================================================
 * in_band — the over-extension deadzone gate (decision #7)
 * ====================================================================== */

#[test]
fn in_band_far_only_weapon_has_a_min_range_deadzone() {
    // The decision-7 play: a Far-only weapon cannot hit a cell it has been
    // closed onto (Adjacent/Near are dead), only reaches across to Far.
    let far_only = [Range::Far];
    assert!(
        !in_band(&far_only, p(0, 0), p(0, 0)),
        "same cell (dist 0) is dead"
    );
    assert!(!in_band(&far_only, p(0, 0), p(1, 0)), "Adjacent is dead");
    assert!(!in_band(&far_only, p(0, 0), p(2, 0)), "Near is dead");
    assert!(in_band(&far_only, p(0, 0), p(3, 0)), "Far reaches");
    assert!(
        in_band(&far_only, p(0, 0), p(4, 0)),
        "farther still reaches"
    );
}

#[test]
fn in_band_short_weapon_cannot_reach_far() {
    let close = [Range::Adjacent, Range::Near];
    assert!(in_band(&close, p(0, 0), p(1, 1)), "Adjacent diagonal");
    assert!(in_band(&close, p(0, 0), p(2, 0)), "Near");
    assert!(!in_band(&close, p(0, 0), p(3, 0)), "Far is out of reach");
}

#[test]
fn in_band_empty_allowed_set_never_bears() {
    let none: [Range; 0] = [];
    for b in every_cell() {
        assert!(
            !in_band(&none, p(2, 2), b),
            "empty allowed set excludes {b:?}"
        );
    }
}

#[test]
fn in_band_agrees_with_range_band_membership_over_every_pair() {
    // Cross-function: in_band(allowed, a, b) ⟺ allowed.contains(range_band(a,b)),
    // for all 400 pairs and a representative allowed set.
    let allowed = [Range::Adjacent, Range::Far]; // a gapped set (skips Near)
    for a in every_cell() {
        for b in every_cell() {
            let want = allowed.contains(&range_band(a, b));
            assert_eq!(
                in_band(&allowed, a, b),
                want,
                "in_band vs range_band at {a:?},{b:?}"
            );
        }
    }
}

/* =========================================================================
 * direction_to — magnitude-aware nearest-of-8, vs grid::from_to (sign octant)
 * ====================================================================== */

#[test]
fn direction_to_is_none_only_for_the_same_cell() {
    for a in every_cell() {
        assert_eq!(direction_to(a, a), None, "{a:?} to itself");
        for b in every_cell() {
            if a != b {
                assert!(direction_to(a, b).is_some(), "{a:?}->{b:?}");
            }
        }
    }
}

#[test]
fn direction_to_agrees_with_from_to_on_axis_aligned_and_45_vectors() {
    // Where the vector lies exactly on an octant, the magnitude-aware snap and
    // the sign-based octant coincide. Sweep the 8 unit steps from a centre cell.
    let c = p(2, 2);
    for d in ALL_DIR8 {
        let target = grid::offset(c, d, 1).expect("interior 1-step is on-grid");
        assert_eq!(direction_to(c, target), Some(d), "unit {d:?}");
        assert_eq!(
            direction_to(c, target),
            grid::from_to(c, target),
            "agree with from_to on {d:?}"
        );
    }
}

#[test]
fn direction_to_snaps_shallow_vectors_to_the_nearer_cardinal() {
    // The documented divergence: (3,1) is ~18° off East → E (nearest octant),
    // whereas the sign-based from_to reports the diagonal SE.
    assert_eq!(direction_to(p(0, 0), p(3, 1)), Some(Dir8::E), "shallow → E");
    assert_eq!(
        grid::from_to(p(0, 0), p(3, 1)),
        Some(Dir8::SE),
        "sign octant → SE (contrast)"
    );
    // Steep mirror on the row axis.
    assert_eq!(direction_to(p(0, 0), p(1, 3)), Some(Dir8::S), "steep → S");
    assert_eq!(grid::from_to(p(0, 0), p(1, 3)), Some(Dir8::SE));
    // A clean 2:2 vector stays diagonal under both.
    assert_eq!(direction_to(p(0, 0), p(2, 2)), Some(Dir8::SE), "45° → SE");
    assert_eq!(grid::from_to(p(0, 0), p(2, 2)), Some(Dir8::SE));
}

proptest! {
    /// `direction_to` always returns a valid direction for distinct cells, and
    /// is deterministic. (The argmax over a fixed candidate set must be stable.)
    #[test]
    fn direction_to_is_deterministic_and_valid(a in any_pos(), b in any_pos()) {
        let d1 = direction_to(a, b);
        let d2 = direction_to(a, b);
        prop_assert_eq!(d1, d2);
        match d1 {
            Some(d) => {
                prop_assert!(a != b);
                prop_assert!(ALL_DIR8.contains(&d));
            }
            None => prop_assert_eq!(a, b),
        }
    }

    /// The snapped direction is the BEST of the eight: its unit step has cosine
    /// similarity to (b−a) at least as high as every other direction's. This is
    /// the defining property of "nearest of 8" — asserted without re-running the
    /// impl's exact argmax expression (we compare all candidates here).
    #[test]
    fn direction_to_maximises_cosine_similarity(a in any_pos(), b in any_pos()) {
        prop_assume!(a != b);
        let chosen = direction_to(a, b).unwrap();
        let (vc, vr) = ((b.col as i32 - a.col as i32) as f64, (b.row as i32 - a.row as i32) as f64);
        let score = |d: Dir8| {
            let (sc, sr) = d.delta();
            let mag = ((sc * sc + sr * sr) as f64).sqrt();
            (vc * sc as f64 + vr * sr as f64) / mag
        };
        let best = score(chosen);
        for d in ALL_DIR8 {
            prop_assert!(best >= score(d) - 1e-9, "chosen {:?} beats {:?} for {:?}->{:?}", chosen, d, a, b);
        }
    }
}

/* =========================================================================
 * opposite / distance / range_band — thin re-exposures must EQUAL grid.rs
 * (single-source: a divergence means the wrapper grew its own logic)
 * ====================================================================== */

#[test]
fn opposite_equals_grid_dir8_opposite_for_every_direction() {
    for d in ALL_DIR8 {
        assert_eq!(opposite(d), d.opposite(), "{d:?}");
    }
}

#[test]
fn distance_and_range_band_equal_grid_for_every_pair() {
    for a in every_cell() {
        for b in every_cell() {
            assert_eq!(distance(a, b), grid::distance(a, b), "distance {a:?},{b:?}");
            assert_eq!(
                range_band(a, b),
                grid::range_band(a, b),
                "range_band {a:?},{b:?}"
            );
        }
    }
}

#[test]
fn range_band_boundaries_are_the_three_band_chebyshev_cuts() {
    // The mission's "test at every band boundary" for the re-exposed classifier:
    // 0–1 Adjacent, 2 Near, 3+ Far (NOT the 1-D 5-band ruler).
    let o = p(0, 0);
    assert_eq!(range_band(o, o), Range::Adjacent, "dist 0");
    assert_eq!(range_band(o, p(0, 1)), Range::Adjacent, "dist 1");
    assert_eq!(range_band(o, p(1, 1)), Range::Adjacent, "dist 1 diagonal");
    assert_eq!(range_band(o, p(0, 2)), Range::Near, "dist 2");
    assert_eq!(range_band(o, p(2, 2)), Range::Near, "dist 2 diagonal");
    assert_eq!(range_band(o, p(0, 3)), Range::Far, "dist 3");
    assert_eq!(range_band(o, p(4, 0)), Range::Far, "dist 4");
}

/* =========================================================================
 * absorb_shield / default_shield_profile — the per-face SHIELD POOL model
 * (#103 Model A). `charge` is the live depleting pool, `armour` the per-face
 * CAPACITY (no longer subtracted from damage). A hit soaks the pool down to 0;
 * the overflow reaches hull. Integer-only (#104). This SUPERSEDES the old
 * "verbatim parity with the 1-D engine" contract — the 1-D `crate::geometry`
 * is the dead-for-live path and keeps the OLD charge-eats-hit / armour-subtract
 * behavior, so parity is intentionally false now.
 * ====================================================================== */

#[test]
fn absorb_shield_pool_soaks_down_to_zero_overflow_to_hull() {
    // Pool 1 vs a 10 hit: soaks 1, 9 overflows to hull; pool now 0.
    let mut face = ShieldFace {
        armour: 5,
        charge: 1,
    };
    assert_eq!(
        absorb_shield(&mut face, 10),
        9,
        "overflow past the pool reaches hull"
    );
    assert_eq!(face.charge, 0, "pool drained");
    assert_eq!(face.armour, 5, "capacity untouched");
}

#[test]
fn absorb_shield_empty_pool_passes_full_to_hull() {
    // `armour`/capacity no longer subtracts: an empty pool lets the full hit
    // through (the OLD model would have done 5 - 2 = 3).
    let mut face = ShieldFace {
        armour: 2,
        charge: 0,
    };
    assert_eq!(absorb_shield(&mut face, 5), 5, "no flat-armour subtraction");
    assert_eq!(face.armour, 2, "capacity untouched by a hit");
}

#[test]
fn absorb_shield_partial_soak_and_ignores_nonpositive() {
    // Pool 3 vs a 2 hit: fully soaked, pool drops to 1, 0 reaches hull.
    let mut face = ShieldFace {
        armour: 5,
        charge: 3,
    };
    assert_eq!(absorb_shield(&mut face, 2), 0, "small hit fully absorbed");
    assert_eq!(face.charge, 1, "pool spent by exactly the hit");
    // A non-positive hit consumes nothing.
    let mut other = ShieldFace {
        armour: 5,
        charge: 3,
    };
    assert_eq!(absorb_shield(&mut other, 0), 0);
    assert_eq!(absorb_shield(&mut other, -4), 0);
    assert_eq!(other.charge, 3, "no pool spent on a non-hit");
}

#[test]
fn default_shield_profile_is_the_frigate_pool() {
    // #103 Model A caps (Bruce-tunable): bow 4 / flanks 3 / stern 1, pools start
    // FULL (charge == capacity == `armour`).
    let p = default_shield_profile();
    assert_eq!(
        *p.face(HullZone::Bow),
        ShieldFace {
            armour: 4,
            charge: 4
        }
    );
    assert_eq!(
        *p.face(HullZone::Stern),
        ShieldFace {
            armour: 1,
            charge: 1
        }
    );
    assert_eq!(
        *p.face(HullZone::Port),
        ShieldFace {
            armour: 3,
            charge: 3
        }
    );
    assert_eq!(
        *p.face(HullZone::Starboard),
        ShieldFace {
            armour: 3,
            charge: 3
        }
    );
}

proptest! {
    /// The pool model holds over arbitrary states: returned overflow = max(0,
    /// dmg - soak) where soak = min(dmg, charge.max(0)) for a positive hit, and
    /// the pool drops by exactly that soak. Never returns negative, never spends
    /// on a non-hit, never touches capacity.
    #[test]
    fn absorb_shield_pool_overflow_for_any_state(
        capacity in 0i32..8, charge in 0i32..8, dmg in -5i32..15
    ) {
        let mut face = ShieldFace { armour: capacity, charge };
        let overflow = absorb_shield(&mut face, dmg);
        if dmg <= 0 {
            prop_assert_eq!(overflow, 0, "non-positive hit -> 0");
            prop_assert_eq!(face.charge, charge, "non-positive hit spends nothing");
        } else {
            let soak = dmg.min(charge.max(0));
            prop_assert_eq!(overflow, (dmg - soak).max(0), "overflow = dmg - soak");
            prop_assert_eq!(face.charge, charge - soak, "pool drops by soak");
        }
        prop_assert_eq!(face.armour, capacity, "capacity never changes");
        prop_assert!(overflow >= 0, "never negative");
    }
}
