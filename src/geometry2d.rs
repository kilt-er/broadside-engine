//! 2-D spatial core: pure functions over the 5×4 grid (`Pos`/`Dir8`/`Facing`/
//! `Range`).
//!
//! This is the v2 replacement for the 1-D [`crate::geometry`] module. It is the
//! resolver lane task **R1**: the geometry that makes orientation, arcs, and
//! range bands a real decision, ported onto the 2-D [`crate::grid`] type surface
//! (blueprint decision #2, the 5×4 board). No randomness, no content lookups,
//! no rendering — just geometry. Every function here is pure and deterministic,
//! which is load-bearing for the telegraph: the renderer's ThreatMap is painted
//! by running the *same* targeting/geometry the real shot uses, so a
//! non-deterministic helper here would let the telegraph and the actual hit
//! disagree (blueprint "single best idea").
//!
//! ## Why a separate file (`geometry2d.rs`) rather than overwriting `geometry.rs`
//!
//! The live [`crate::geometry`] (1-D `LaneEnd`/`usize`-cell world) is still
//! depended on by ~12 modules (`resolve`, `perspective`, `hud`, `catalog`, …)
//! because the atomic `cell:usize → Pos` type migration (blueprint A3) has not
//! landed yet. Overwriting `geometry.rs` now would break the whole crate until
//! A3 completes — a half-compiled shared tree, which the team forbids. So this
//! lands as an additive module (expand-contract): the 1-D `geometry.rs` stays
//! live until every consumer migrates, then the architect deletes it and
//! `git mv`s this file onto `geometry.rs` in one atomic contract commit. Until
//! then both coexist without collision (the 1-D fns take `usize`, these take
//! [`crate::grid::Pos`]).
//!
//! ## What lives here vs. in `grid.rs`
//!
//! [`crate::grid`] is the frozen *type surface* + the dimension-only helpers it
//! could define without a damage model: [`distance`](crate::grid::distance)
//! (Chebyshev), [`range_band`](crate::grid::range_band) (the 3-band classifier),
//! [`offset`](crate::grid::offset), [`from_to`](crate::grid::from_to) (the
//! *exact-octant* direction), neighbours. This module adds the resolver-owned
//! pieces grid.rs deliberately left out (see grid.rs's note on `from_to`):
//!
//! - [`band_falloff`] — the per-band damage multiplier `[1.0, 0.6, 0.3]`
//!   (blueprint decision #6); grid.rs names the [`Range`] buckets, the falloff
//!   table is the resolver's.
//! - [`in_band`] — is a target inside a weapon's allowed band set.
//! - [`direction_to`] — the **magnitude-aware** nearest-of-8 snap of an
//!   arbitrary vector (grid.rs's `from_to` only handles the exact-octant case
//!   and explicitly defers the general snap to "R1 `direction_to`").
//! - [`arc_bears`] / [`bears`] — the 2-D firing-arc gate (cardinal-exact).
//! - [`facing_zone`] — the correctness-critical 2-D quadrant table mapping an
//!   incoming direction to the [`HullZone`] that eats the hit.
//! - [`absorb_shield`] / [`default_shield_profile`] — **kept verbatim** from the
//!   1-D engine; they are frame-agnostic (they never touched the lane), so the
//!   port is a literal copy.
//!
//! [`opposite`], [`distance`], [`range_band`] are re-exposed here as thin
//! wrappers that delegate to `grid.rs` so resolver code can import its whole
//! geometry vocabulary from one place without duplicating the (single-source)
//! logic.

use crate::grid::{self, Axis, Dir4, Dir8, Facing, Pos, Range};
use crate::types::{Arc, HullZone, ShieldFace, ShieldProfile};

/* =========================================================================
 * Direction primitives (thin re-exposures + the magnitude-aware snap)
 * ====================================================================== */

/// The 180°-opposite direction. The 2-D analog of the 1-D `opposite(LaneEnd)`;
/// delegates to [`Dir8::opposite`] (the single source of the `+4 mod 8`
/// arithmetic) so there is no second copy of the rule.
pub fn opposite(dir: Dir8) -> Dir8 {
    dir.opposite()
}

/// Chebyshev distance between two cells. Thin re-exposure of
/// [`grid::distance`] so resolver code can pull `distance` from the geometry
/// module the way the 1-D engine did; the metric itself lives in grid.rs.
pub fn distance(a: Pos, b: Pos) -> usize {
    grid::distance(a, b)
}

/// Bucket the [`distance`] between two cells into a [`Range`] band. Thin
/// re-exposure of [`grid::range_band`] (0–1 → `Adjacent`, 2 → `Near`, 3+ →
/// `Far`).
pub fn range_band(a: Pos, b: Pos) -> Range {
    grid::range_band(a, b)
}

/// The nearest-of-eight direction pointing from `a` toward `b`, or `None` when
/// `a == b`.
///
/// Unlike [`grid::from_to`] — which classifies by the *sign* of each axis delta
/// and so is only exact for axis-aligned or 45° vectors — this is the
/// **magnitude-aware** snap grid.rs defers to the resolver (see its `from_to`
/// note). A shallow vector like `(d_col, d_row) = (3, 1)` snaps to `E` here
/// (true nearest octant ≈ 18° off East), where `from_to` would return `SE`.
///
/// Method: pick the [`Dir8`] whose unit step has the greatest cosine similarity
/// to the vector `b - a` (dot product over the step's magnitude). Ties — a
/// vector exactly between two directions — resolve to the lower [`Dir8::step`]
/// index ([`Dir8::ALL`] is in clockwise `step` order), a deterministic
/// tie-break so the telegraph and the shot always agree. The board is 5×4 so
/// the components are tiny (`≤4`); `f64` is exact and deterministic for these.
pub fn direction_to(a: Pos, b: Pos) -> Option<Dir8> {
    let dc = (b.col as i32) - (a.col as i32);
    let dr = (b.row as i32) - (a.row as i32);
    if dc == 0 && dr == 0 {
        return None;
    }
    let (vc, vr) = (dc as f64, dr as f64);
    let mut best = Dir8::N;
    let mut best_score = f64::NEG_INFINITY;
    for d in Dir8::ALL {
        let (sc, sr) = d.delta();
        let mag = ((sc * sc + sr * sr) as f64).sqrt();
        // cosine similarity (up to the constant |v|, which is common to all
        // candidates and so does not affect the argmax): dot(v, step) / |step|.
        let score = (vc * sc as f64 + vr * sr as f64) / mag;
        if score > best_score {
            best_score = score;
            best = d;
        }
    }
    Some(best)
}

/* =========================================================================
 * Range bands & falloff (blueprint decision #6)
 * ====================================================================== */

/// Index of a [`Range`] band in near→far order, for [`band_falloff`]'s table
/// lookup. The exhaustive match is the drift guard: adding a [`Range`] variant
/// without extending this (and the falloff table) fails to compile.
fn band_index(b: Range) -> usize {
    match b {
        Range::Adjacent => 0,
        Range::Near => 1,
        Range::Far => 2,
    }
}

/// Per-band damage multiplier applied to a shot, `[1.0, 0.6, 0.3]` for
/// `Adjacent`/`Near`/`Far` (blueprint decision #6, "tune in playtest"). Floored
/// at 0 and floored to an integer, matching the 1-D `band_falloff` contract
/// (`Math.floor`, `Math.max(0, …)`).
///
/// This is keyed on the **actual** band the shot crosses — the 3-band v2 model
/// is an absolute falloff curve (closer = more damage), not the 1-D engine's
/// distance-from-optimal-band delta. A weapon's *allowed* bands (the
/// over-extension deadzone, decision #7 — e.g. a Far weapon that may not fire
/// Adjacent) are enforced separately by [`in_band`] at targeting time; this
/// function only scales damage once the shot is legal.
pub fn band_falloff(raw: i32, actual: Range) -> i32 {
    let factors = [1.0_f64, 0.6, 0.3];
    let factor = factors[band_index(actual)];
    let scaled = (raw as f64 * factor).floor() as i32;
    scaled.max(0)
}

/// Is `target` inside this weapon's `allowed` band set at the current
/// [`distance`]? The gate that realizes the over-extension deadzone (decision
/// #7): a weapon whose `allowed` omits `Adjacent` cannot hit a cell it has been
/// closed on, and one whose `allowed` omits `Far` cannot reach across the board.
pub fn in_band(allowed: &[Range], attacker: Pos, target: Pos) -> bool {
    allowed.contains(&range_band(attacker, target))
}

/* =========================================================================
 * facing_zone — the 2-D quadrant table (correctness-critical, blueprint §
 * "Defense + telegraph"; reviewer V3)
 * ====================================================================== */

/// Which fixed [`HullZone`] eats a hit arriving **from** direction
/// `incoming_from` (the direction pointing from the target back toward the
/// attacker, i.e. where the shot comes from), given the target's [`Facing`].
///
/// This is the 2-D replacement for the 1-D `facing_zone(Orientation, LaneEnd)`.
/// It is pure logic over types that already exist ([`Facing`] + [`Dir8`] +
/// [`HullZone`]); A3 later *wires* it into the damage pipeline (the attacker→
/// target direction feeds `incoming_from`), but the table itself is stable, so
/// it lands and is unit-tested now (blueprint: "pin + unit-test BEFORE the
/// rewrite").
///
/// ## Bow(dir) — nose pointed at cardinal `dir`
///
/// Split the 8 incoming directions into ±45° sectors around the bow vector:
/// - within ±45° of `dir` (i.e. `dir` and its two diagonal neighbours) → `Bow`
///   (the strong face you present by aiming at the threat),
/// - within ±45° of `opposite(dir)` → `Stern` (the weak face),
/// - the two remaining pure-perpendicular cardinals → the flanks, assigned by
///   left/right of the bow vector: **right → `Starboard`, left → `Port`**
///   (standard nautical, and the renderer's bow-arrow encodes the same forward
///   axis so the gold shield pip lands on the matching side).
///
/// With 8 directions at 45° spacing this is a clean 3 / 3 / 1 / 1 partition.
///
/// ## Broadside(axis) — hull turned across the grid, both flanks out
///
/// A Broadside hull *runs along* its `axis` (grid.rs: an `EastWest` hull "runs
/// E↔W"), so its broad flanks face **perpendicular** to the axis and its narrow
/// ends point **along** it. This matches grid.rs's [`Axis`] doc ("the hull along
/// `EastWest` presents Port/Starboard to the N/S sectors") and the corrected
/// blueprint:
/// - the two **off-axis** cardinals (perpendicular to the hull) → `Port` /
///   `Starboard`,
/// - the two **on-axis** cardinals (the hull's ends) → `Bow` / `Stern`,
/// - diagonals snap to the nearest face by signed angle (each diagonal is
///   adjacent to one on-axis and one off-axis cardinal; see the per-arm notes).
///
/// Port/Starboard and Bow/Stern within a broadside are assigned deterministically
/// (a turned hull has no inherent "front", so the split is a stable convention,
/// not a physical fact): on the off-axis pair the increasing-coordinate
/// direction is `Starboard`; on the on-axis pair the increasing-coordinate
/// direction is `Bow`.
pub fn facing_zone(facing: Facing, incoming_from: Dir8) -> HullZone {
    match facing {
        Facing::Bow(dir) => bow_zone(dir, incoming_from),
        Facing::Broadside(axis) => broadside_zone(axis, incoming_from),
    }
}

/// `facing_zone` for the `Bow(dir)` stance. See [`facing_zone`] for the spec.
fn bow_zone(dir: Dir4, incoming_from: Dir8) -> HullZone {
    let bow = dir.to_dir8();
    // Clockwise offset of the incoming direction from the bow vector, 0..8.
    let rel = (incoming_from.step() + 8 - bow.step()) % 8;
    match rel {
        // dead ahead + the two ±45° diagonals → strong bow face.
        7 | 0 | 1 => HullZone::Bow,
        // +90° (clockwise = right of the bow vector) → Starboard.
        2 => HullZone::Starboard,
        // the rear ±45° arc → weak stern face.
        3..=5 => HullZone::Stern,
        // -90° (counter-clockwise = left of the bow vector) → Port.
        6 => HullZone::Port,
        // `rel` is `% 8`, so 0..=7 is exhaustive; this satisfies the checker.
        _ => unreachable!("rel is mod 8"),
    }
}

/// `facing_zone` for the `Broadside(axis)` stance. See [`facing_zone`] for the
/// spec.
///
/// A Broadside hull runs along `axis`, so its ends (Bow/Stern) point *along* the
/// axis and its flanks (Port/Starboard) face *perpendicular* to it. We anchor
/// the table on a deterministic "pseudo-forward" — the increasing-coordinate
/// direction of the hull axis (`Axis::dirs().0`: `S` for NorthSouth, `E` for
/// EastWest) — and assign by clockwise offset from it:
/// - pseudo-forward (the +on-axis end) → `Bow`, its opposite → `Stern`,
/// - the clockwise (right) perpendicular flank → `Starboard`, the
///   counter-clockwise (left) → `Port`.
///
/// **Diagonal tiebreak (the load-bearing part).** Every diagonal sits exactly
/// 45° from one end *and* one flank — a true tie. We break it consistently so
/// the table is a clean **2 / 2 / 2 / 2** partition, each face owning its
/// cardinal plus one diagonal: a diagonal snaps to the cardinal **45°
/// clockwise** of it (equivalently, each cardinal claims the diagonal one step
/// counter-clockwise). Worked for `EastWest` (pseudo-forward `E`): `Bow{E,NE}`,
/// `Starboard{S,SE}`, `Stern{W,SW}`, `Port{N,NW}`. For `NorthSouth`
/// (pseudo-forward `S`): `Bow{S,SE}`, `Starboard{W,SW}`, `Stern{N,NW}`,
/// `Port{E,NE}`. A turned hull has no inherent front, so this split is a stable
/// *convention* (locked by the tester's exhaustive Dir8×Facing table T2 +
/// reviewer V3), not a physical fact.
fn broadside_zone(axis: Axis, incoming_from: Dir8) -> HullZone {
    // Pseudo-forward = the +on-axis hull end (= Bow). `rel` is the clockwise
    // offset of the incoming direction from it, 0..8.
    let fwd = axis.dirs().0.to_dir8();
    let rel = (incoming_from.step() + 8 - fwd.step()) % 8;
    match rel {
        // pseudo-forward (rel 0) + the diagonal 45° CCW of it (rel 7) → Bow end.
        7 | 0 => HullZone::Bow,
        // CW-perpendicular flank (rel 2) + the diagonal 45° CCW of it (rel 1).
        1 | 2 => HullZone::Starboard,
        // pseudo-aft (rel 4) + the diagonal 45° CCW of it (rel 3) → Stern end.
        3 | 4 => HullZone::Stern,
        // CCW-perpendicular flank (rel 6) + the diagonal 45° CCW of it (rel 5).
        5 | 6 => HullZone::Port,
        _ => unreachable!("rel is mod 8"),
    }
}

/* =========================================================================
 * Arcs — the 2-D firing-arc gate (cardinal-exact)
 * ====================================================================== */

/// Does a mount with firing `arc` bear on something lying toward `toward`,
/// given the ship's [`Facing`]? This is the gate that makes facing matter in
/// 2-D: a forward gun only fires out the bow cardinal, a rear gun only astern,
/// a broadside battery only when the hull is turned across the grid (and then
/// out both flank cardinals), a turret always bears.
///
/// `toward` is the direction from the firing ship to the target (use
/// [`direction_to`]). Under the v2 **cardinals-only firing** model (decision
/// #9: 4-cardinal facing, 8-way deferred) a weapon fires along an *exact*
/// cardinal ray, so an arc bears iff `toward` is exactly that arc's cardinal
/// direction — **not** a ±45° cone. A diagonal `toward` never bears (you cannot
/// fire diagonally), so e.g. a `Broadside` battery does NOT bear on a target
/// that is diagonal from the ship; it must be due-N/S or due-E/W of a flank.
///
/// This is deliberately a *different arity* from [`facing_zone`]: FIRING is
/// cardinal-exact (4-way) here, while RECEIVING ([`facing_zone`]) is 8-way (an
/// off-axis BLAST splash or ordnance hit can arrive on a diagonal and land on
/// whatever face it presents). Conflating the two — making `arc_bears` a ±45°
/// cone to mirror `facing_zone`'s receiving sectors — would (wrongly) let a
/// broadside "bear" on a diagonal target it cannot actually hit with a cardinal
/// shot. (For every *cardinal* `toward` the cone and the exact test agree; they
/// differ only on diagonals, which is exactly the case that must be rejected.)
///
/// 2-D port of the 1-D `arc_bears(Orientation, Arc, LaneEnd)`. The 1-D version
/// was a binary fore/aft test; in 2-D `Forward`/`Rear` fire out the bow/stern
/// cardinal and `BroadsideArc` fires out either flank cardinal of a `Broadside`
/// hull.
pub fn arc_bears(facing: Facing, arc: Arc, toward: Dir8) -> bool {
    match arc {
        // Turret bears in every direction regardless of facing.
        Arc::Turret => true,
        // Forward: only a Bow stance, firing out the exact bow cardinal.
        Arc::Forward => match facing {
            Facing::Bow(dir) => toward == dir.to_dir8(),
            Facing::Broadside(_) => false,
        },
        // Rear: only a Bow stance, firing out the exact stern cardinal.
        Arc::Rear => match facing {
            Facing::Bow(dir) => toward == dir.to_dir8().opposite(),
            Facing::Broadside(_) => false,
        },
        // Broadside battery (Model D, #92 — Bruce's bow-cardinal stance model):
        // fires out the two flank cardinals PERPENDICULAR to the bow. Turning the
        // bow E/W puts the flanks N/S — that IS broadsiding; there is no separate
        // `Facing::Broadside` stance in v2 (the canonical-TS turned-stance is
        // legacy 1-D, kept only as the vestigial Broadside arm). On-axis (the bow
        // cardinal + its opposite stern) and all diagonals do NOT bear.
        // DELIBERATELY deviates from the TS `arc_bears` (which required a Broadside
        // stance); firing (`bearing_cardinals`) mirrors this exactly so the gate
        // and the shot stay one model.
        Arc::BroadsideArc => {
            let axis = match facing {
                Facing::Bow(dir) => dir.axis(),
                Facing::Broadside(axis) => axis,
            };
            // Flanks are perpendicular to the hull's forward axis.
            let off = match axis {
                Axis::NorthSouth => Axis::EastWest,
                Axis::EastWest => Axis::NorthSouth,
            };
            let (a, b) = off.dirs();
            toward == a.to_dir8() || toward == b.to_dir8()
        }
    }
}

/* =========================================================================
 * Directional shield absorption — KEPT VERBATIM from the 1-D engine
 * ====================================================================== */

/// Run incoming damage through one hull zone's defence. A held shield `charge`
/// negates the hit entirely and is consumed; otherwise the zone's permanent
/// `armour` is subtracted. Mutates `face` (charge consumption) and returns the
/// damage that reaches hull.
///
/// **Kept verbatim** from the 1-D `geometry::absorb_shield` (and `absorbShield`
/// in `geometry.ts`): it is frame-agnostic — it never referenced the lane — so
/// the 2-D port is a literal copy. Blueprint: `absorb_shield` is reused as the
/// secondary "hits you can't dodge" buffer.
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

/// The starting Frigate's hull: strong bow (2), weak stern (0), medium flanks
/// (1). **Kept verbatim** from the 1-D `geometry::default_shield_profile` /
/// `defaultShieldProfile`; the [`HullZone`] set is unchanged by the 2-D move.
pub fn default_shield_profile() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 2, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 1, charge: 0 },
        starboard: ShieldFace { armour: 1, charge: 0 },
    }
}

/* =========================================================================
 * Tests — one+ sanity assert per pure function. Deep coverage (every
 * Dir8×Facing for facing_zone, the full falloff/Chebyshev sweep) is the
 * tester's lane (blueprint T2); these guard the contract at the source.
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Pos;

    fn p(col: usize, row: usize) -> Pos {
        Pos::new(col, row)
    }

    /* ---- direction primitives ---- */

    #[test]
    fn opposite_delegates_to_dir8() {
        for d in Dir8::ALL {
            assert_eq!(opposite(d), d.opposite());
            assert_eq!(opposite(opposite(d)), d);
        }
    }

    #[test]
    fn direction_to_is_none_only_for_same_cell() {
        let c = p(2, 2);
        assert_eq!(direction_to(c, c), None);
        assert!(direction_to(c, p(0, 0)).is_some());
    }

    #[test]
    fn direction_to_matches_exact_octants() {
        // For axis-aligned / 45° vectors the magnitude-aware snap agrees with
        // grid::from_to (the exact-octant classifier).
        let c = p(2, 2);
        for target in [
            p(2, 1), // N
            p(3, 1), // NE
            p(3, 2), // E
            p(3, 3), // SE
            p(2, 3), // S
            p(1, 3), // SW
            p(1, 2), // W
            p(1, 1), // NW
        ] {
            assert_eq!(direction_to(c, target), grid::from_to(c, target));
        }
    }

    #[test]
    fn direction_to_snaps_shallow_vectors_to_the_nearer_cardinal() {
        // (dc, dr) = (3, 1): shallow, ~18° off East. The sign-based from_to
        // would say SE; the magnitude-aware snap says E.
        let a = p(0, 0);
        let b = p(3, 1);
        assert_eq!(direction_to(a, b), Some(Dir8::E));
        assert_eq!(grid::from_to(a, b), Some(Dir8::SE)); // contrast
        // Steep mirror: (1, 3) ~18° off South -> S.
        assert_eq!(direction_to(p(0, 0), p(1, 3)), Some(Dir8::S));
    }

    #[test]
    fn distance_and_range_band_delegate_to_grid() {
        let a = p(0, 0);
        let b = p(3, 1);
        assert_eq!(distance(a, b), grid::distance(a, b));
        assert_eq!(range_band(a, b), grid::range_band(a, b));
        // The mission's canonical sanity check: distance-3 cells read Far in
        // the 3-band model (the old 1-D "distance 3 = mid" has no analog here).
        assert_eq!(distance(p(0, 0), p(3, 0)), 3);
        assert_eq!(range_band(p(0, 0), p(3, 0)), Range::Far);
    }

    /* ---- range bands & falloff (decision #6) ---- */

    #[test]
    fn band_falloff_table_is_one_point_six_point_three() {
        assert_eq!(band_falloff(10, Range::Adjacent), 10); // ×1.0
        assert_eq!(band_falloff(10, Range::Near), 6); //      ×0.6
        assert_eq!(band_falloff(10, Range::Far), 3); //       ×0.3
    }

    #[test]
    fn band_falloff_floors_to_int_and_clamps_negatives() {
        // floor(7 * 0.6) = floor(4.2) = 4
        assert_eq!(band_falloff(7, Range::Near), 4);
        // floor(7 * 0.3) = floor(2.1) = 2
        assert_eq!(band_falloff(7, Range::Far), 2);
        // negative raw clamps to 0
        assert_eq!(band_falloff(-5, Range::Adjacent), 0);
        // zero stays zero
        assert_eq!(band_falloff(0, Range::Far), 0);
    }

    #[test]
    fn in_band_respects_allowed_set_and_deadzone() {
        // A Far-only weapon (over-extension deadzone, decision #7): cannot hit
        // Adjacent/Near, can reach Far.
        let far_only = [Range::Far];
        assert!(!in_band(&far_only, p(0, 0), p(1, 0))); // Adjacent
        assert!(!in_band(&far_only, p(0, 0), p(2, 0))); // Near
        assert!(in_band(&far_only, p(0, 0), p(3, 0))); // Far
        // A short weapon allowed Adjacent+Near.
        let close = [Range::Adjacent, Range::Near];
        assert!(in_band(&close, p(0, 0), p(1, 1))); // Adjacent (diag)
        assert!(in_band(&close, p(0, 0), p(2, 0))); // Near
        assert!(!in_band(&close, p(0, 0), p(4, 0))); // Far
    }

    /* ---- facing_zone: Bow stance (uncontested) ---- */

    #[test]
    fn facing_zone_bow_dead_ahead_is_bow_behind_is_stern() {
        // Bow pointed N: a shot from N hits the bow, from S hits the stern.
        let f = Facing::Bow(Dir4::N);
        assert_eq!(facing_zone(f, Dir8::N), HullZone::Bow);
        assert_eq!(facing_zone(f, Dir8::S), HullZone::Stern);
    }

    #[test]
    fn facing_zone_bow_diagonals_within_45_fold_into_bow_or_stern() {
        let f = Facing::Bow(Dir4::N);
        // ±45° of N -> Bow
        assert_eq!(facing_zone(f, Dir8::NE), HullZone::Bow);
        assert_eq!(facing_zone(f, Dir8::NW), HullZone::Bow);
        // ±45° of S (opposite) -> Stern
        assert_eq!(facing_zone(f, Dir8::SE), HullZone::Stern);
        assert_eq!(facing_zone(f, Dir8::SW), HullZone::Stern);
    }

    #[test]
    fn facing_zone_bow_perpendiculars_are_starboard_right_port_left() {
        // Bow N: right hand (E) is Starboard, left hand (W) is Port.
        let f = Facing::Bow(Dir4::N);
        assert_eq!(facing_zone(f, Dir8::E), HullZone::Starboard);
        assert_eq!(facing_zone(f, Dir8::W), HullZone::Port);
        // Bow E (facing right): right hand is S, left hand is N.
        let fe = Facing::Bow(Dir4::E);
        assert_eq!(facing_zone(fe, Dir8::S), HullZone::Starboard);
        assert_eq!(facing_zone(fe, Dir8::N), HullZone::Port);
    }

    #[test]
    fn facing_zone_bow_partition_is_total_and_three_three_one_one() {
        // Every Dir8 maps to exactly one zone; counts are 3 Bow / 3 Stern /
        // 1 Port / 1 Starboard for any cardinal bow.
        for dir in Dir4::ALL {
            let f = Facing::Bow(dir);
            let mut bow = 0;
            let mut stern = 0;
            let mut port = 0;
            let mut star = 0;
            for inc in Dir8::ALL {
                match facing_zone(f, inc) {
                    HullZone::Bow => bow += 1,
                    HullZone::Stern => stern += 1,
                    HullZone::Port => port += 1,
                    HullZone::Starboard => star += 1,
                }
            }
            assert_eq!((bow, stern, port, star), (3, 3, 1, 1), "bow {dir:?}");
        }
    }

    /* ---- facing_zone: Broadside stance (grid.rs / corrected-blueprint
     * semantics; pseudo-forward = Axis::dirs().0, see broadside_zone docs) ---- */

    #[test]
    fn facing_zone_broadside_eastwest_cardinals() {
        // EastWest hull: ends point along E/W, flanks face the perpendicular
        // N/S. Pseudo-forward = E (the +on-axis end) = Bow; facing E, the right
        // hand (CW) is S => Starboard, left (CCW) is N => Port.
        let f = Facing::Broadside(Axis::EastWest);
        assert_eq!(facing_zone(f, Dir8::E), HullZone::Bow); // +on-axis end
        assert_eq!(facing_zone(f, Dir8::W), HullZone::Stern); // -on-axis end
        assert_eq!(facing_zone(f, Dir8::S), HullZone::Starboard); // CW flank
        assert_eq!(facing_zone(f, Dir8::N), HullZone::Port); // CCW flank
    }

    #[test]
    fn facing_zone_broadside_eastwest_diagonals() {
        // The 4 diagonal tiebreaks (each snaps 45° CW): see broadside_zone docs.
        // Bow{E,NE}, Starboard{S,SE}, Stern{W,SW}, Port{N,NW}.
        let f = Facing::Broadside(Axis::EastWest);
        assert_eq!(facing_zone(f, Dir8::NE), HullZone::Bow);
        assert_eq!(facing_zone(f, Dir8::SE), HullZone::Starboard);
        assert_eq!(facing_zone(f, Dir8::SW), HullZone::Stern);
        assert_eq!(facing_zone(f, Dir8::NW), HullZone::Port);
    }

    #[test]
    fn facing_zone_broadside_northsouth_cardinals() {
        // NorthSouth hull: ends point along N/S, flanks face the perpendicular
        // E/W. Pseudo-forward = S (the +on-axis end, toward the player) = Bow;
        // facing S, the right hand (CW) is W => Starboard, left (CCW) is E =>
        // Port. (This is the handedness flip from the EastWest case — facing
        // "down the board" puts West on your right.)
        let f = Facing::Broadside(Axis::NorthSouth);
        assert_eq!(facing_zone(f, Dir8::S), HullZone::Bow); // +on-axis end
        assert_eq!(facing_zone(f, Dir8::N), HullZone::Stern); // -on-axis end
        assert_eq!(facing_zone(f, Dir8::W), HullZone::Starboard); // CW flank
        assert_eq!(facing_zone(f, Dir8::E), HullZone::Port); // CCW flank
    }

    #[test]
    fn facing_zone_broadside_northsouth_diagonals() {
        // Bow{S,SE}, Starboard{W,SW}, Stern{N,NW}, Port{E,NE}.
        let f = Facing::Broadside(Axis::NorthSouth);
        assert_eq!(facing_zone(f, Dir8::SE), HullZone::Bow);
        assert_eq!(facing_zone(f, Dir8::SW), HullZone::Starboard);
        assert_eq!(facing_zone(f, Dir8::NW), HullZone::Stern);
        assert_eq!(facing_zone(f, Dir8::NE), HullZone::Port);
    }

    #[test]
    fn facing_zone_broadside_partition_is_total_two_two_two_two() {
        // Both flanks present, both ends present: the diagonal tiebreak makes a
        // clean 2 Bow / 2 Stern / 2 Port / 2 Starboard across the 8 incoming
        // directions (each face = its cardinal + one snapped diagonal).
        for axis in [Axis::NorthSouth, Axis::EastWest] {
            let f = Facing::Broadside(axis);
            let mut bow = 0;
            let mut stern = 0;
            let mut port = 0;
            let mut star = 0;
            for inc in Dir8::ALL {
                match facing_zone(f, inc) {
                    HullZone::Bow => bow += 1,
                    HullZone::Stern => stern += 1,
                    HullZone::Port => port += 1,
                    HullZone::Starboard => star += 1,
                }
            }
            assert_eq!((bow, stern, port, star), (2, 2, 2, 2), "broadside {axis:?}");
        }
    }

    /* ---- arc_bears (2-D firing gate: cardinal-EXACT, diagonals never bear) ---- */

    #[test]
    fn arc_bears_turret_always_bears() {
        let f = Facing::Bow(Dir4::N);
        for d in Dir8::ALL {
            assert!(arc_bears(f, Arc::Turret, d));
        }
        assert!(arc_bears(Facing::Broadside(Axis::EastWest), Arc::Turret, Dir8::S));
    }

    #[test]
    fn arc_bears_forward_is_the_exact_bow_cardinal_only() {
        let f = Facing::Bow(Dir4::N);
        // exactly the bow cardinal bears
        assert!(arc_bears(f, Arc::Forward, Dir8::N));
        // diagonals flanking the bow do NOT bear (can't fire diagonally —
        // cardinals-only firing, decision #9)
        assert!(!arc_bears(f, Arc::Forward, Dir8::NE));
        assert!(!arc_bears(f, Arc::Forward, Dir8::NW));
        // perpendicular / astern cardinals don't bear
        assert!(!arc_bears(f, Arc::Forward, Dir8::E));
        assert!(!arc_bears(f, Arc::Forward, Dir8::S));
        // never when broadside
        assert!(!arc_bears(Facing::Broadside(Axis::EastWest), Arc::Forward, Dir8::N));
    }

    #[test]
    fn arc_bears_rear_is_the_exact_stern_cardinal_only() {
        let f = Facing::Bow(Dir4::N);
        // exactly the stern cardinal (opposite the bow) bears
        assert!(arc_bears(f, Arc::Rear, Dir8::S));
        // flanking diagonals do NOT bear
        assert!(!arc_bears(f, Arc::Rear, Dir8::SE));
        assert!(!arc_bears(f, Arc::Rear, Dir8::SW));
        assert!(!arc_bears(f, Arc::Rear, Dir8::N));
        assert!(!arc_bears(f, Arc::Rear, Dir8::W));
        assert!(!arc_bears(Facing::Broadside(Axis::EastWest), Arc::Rear, Dir8::S));
    }

    #[test]
    fn arc_bears_broadside_fires_exact_flank_cardinals_only() {
        // Model D (#92): a BroadsideArc bears out the two flank cardinals
        // PERPENDICULAR to the hull's forward axis — for BOTH a Bow stance (the
        // bow's perpendicular flanks; Bruce's bow-cardinal model) AND the
        // vestigial Broadside stance. On-axis (the forward cardinal + its
        // opposite) and all diagonals do NOT bear.

        // EastWest forward axis: flanks face the exact cardinals N and S.
        let f = Facing::Broadside(Axis::EastWest);
        assert!(arc_bears(f, Arc::BroadsideArc, Dir8::N));
        assert!(arc_bears(f, Arc::BroadsideArc, Dir8::S));
        // diagonals do NOT bear — a broadside cannot fire at a diagonal target.
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::NE));
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::SE));
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::NW));
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::SW));
        // the on-axis cardinals (E/W) do NOT bear a broadside battery.
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::E));
        assert!(!arc_bears(f, Arc::BroadsideArc, Dir8::W));

        // Model D: a BOW stance bears the broadside off its PERPENDICULAR flanks.
        // Bow N/S (NorthSouth axis) -> flanks E/W bear; the bow axis N/S does NOT.
        assert!(arc_bears(Facing::Bow(Dir4::N), Arc::BroadsideArc, Dir8::E));
        assert!(arc_bears(Facing::Bow(Dir4::N), Arc::BroadsideArc, Dir8::W));
        assert!(!arc_bears(Facing::Bow(Dir4::N), Arc::BroadsideArc, Dir8::N), "on-axis (bow) does not bear");
        assert!(!arc_bears(Facing::Bow(Dir4::N), Arc::BroadsideArc, Dir8::S), "on-axis (stern) does not bear");
        assert!(!arc_bears(Facing::Bow(Dir4::N), Arc::BroadsideArc, Dir8::NE), "diagonal does not bear");
        // Bow E/W (EastWest axis) -> flanks N/S bear.
        assert!(arc_bears(Facing::Bow(Dir4::E), Arc::BroadsideArc, Dir8::N));
        assert!(arc_bears(Facing::Bow(Dir4::E), Arc::BroadsideArc, Dir8::S));
        assert!(!arc_bears(Facing::Bow(Dir4::E), Arc::BroadsideArc, Dir8::E), "on-axis (bow) does not bear");

        // NorthSouth forward axis (vestigial Broadside stance): flanks E/W; SE
        // (the tester's old case) is off-axis and must NOT bear.
        let ns = Facing::Broadside(Axis::NorthSouth);
        assert!(arc_bears(ns, Arc::BroadsideArc, Dir8::E));
        assert!(arc_bears(ns, Arc::BroadsideArc, Dir8::W));
        assert!(!arc_bears(ns, Arc::BroadsideArc, Dir8::SE));
        assert!(!arc_bears(ns, Arc::BroadsideArc, Dir8::N));
    }

    /* ---- shield absorption (verbatim port) ---- */

    #[test]
    fn absorb_shield_charge_negates_and_decrements() {
        let mut face = ShieldFace { armour: 5, charge: 1 };
        assert_eq!(absorb_shield(&mut face, 10), 0);
        assert_eq!(face.charge, 0);
    }

    #[test]
    fn absorb_shield_falls_back_to_armour() {
        let mut face = ShieldFace { armour: 2, charge: 0 };
        assert_eq!(absorb_shield(&mut face, 5), 3);
        assert_eq!(face.armour, 2); // permanent, unchanged
    }

    #[test]
    fn absorb_shield_clamps_and_ignores_nonpositive() {
        let mut face = ShieldFace { armour: 5, charge: 0 };
        assert_eq!(absorb_shield(&mut face, 2), 0); // armour > dmg
        let mut charged = ShieldFace { armour: 5, charge: 3 };
        assert_eq!(absorb_shield(&mut charged, 0), 0);
        assert_eq!(charged.charge, 3); // not consumed on a no-op hit
    }

    #[test]
    fn default_shield_profile_matches_the_frigate() {
        let p = default_shield_profile();
        assert_eq!(*p.face(HullZone::Bow), ShieldFace { armour: 2, charge: 0 });
        assert_eq!(*p.face(HullZone::Stern), ShieldFace { armour: 0, charge: 0 });
        assert_eq!(*p.face(HullZone::Port), ShieldFace { armour: 1, charge: 0 });
        assert_eq!(*p.face(HullZone::Starboard), ShieldFace { armour: 1, charge: 0 });
    }
}
