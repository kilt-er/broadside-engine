//! Grid (v2 2D spatial surface) integration suite — blueprint lane task **T1**.
//!
//! Exercises the public surface of [`broadside_engine::grid`] (`Pos`, `Dir8`,
//! `Dir4`, `Axis`, `Facing`, `Range`, and the free helpers). `src/grid.rs`'s own
//! `#[cfg(test)]` is the architect's light sanity (single representative cells);
//! this file is the thorough coverage: **exhaustive** sweeps over the finite
//! domains (all `CELLS` positions, all 8 `Dir8`, all 4 `Dir4`) plus `proptest`
//! invariants where the input space is wide enough that a table cannot close it
//! (index/coord round-trips over arbitrary `usize`, the Chebyshev metric over
//! arbitrary cell pairs, the offset bounds gate over arbitrary deltas).
//!
//! ## Method (per the tester brief)
//!
//! We assert **properties and relationships, not a recomputation**. Where the
//! brief said "don't re-implement the logic as an oracle", that means: we do not
//! shadow `distance` with our own `max(|dc|,|dr|)` and compare equal — that just
//! tests that two copies of one formula agree. Instead we pin the *defining
//! properties* of each operation:
//!   - metric axioms (identity, symmetry, triangle inequality, the chessboard
//!     discriminator that a diagonal step costs 1),
//!   - involutions / inverses (`opposite∘opposite`, `rotate_cw∘rotate_ccw`,
//!     `from_step∘step`, `from_index∘to_index`, `from_dir8∘to_dir8`),
//!   - cross-function consistency (`range_band` agrees with the `distance`
//!     bucket; `from_to` then `offset` strictly reduces `distance`; `neighbors`
//!     == the in-bounds `offset(·,d,1)` set; `delta`+`opposite` cancels).
//!
//! ## Scope note — `from_to` is the grid-step octant, not magnitude snapping
//!
//! `grid::from_to(a,b)` is **signum-based**: it reports the octant of the step
//! `b-a` by the sign of each axis delta (so `(+,−)` → `NE`, etc.). The
//! magnitude-aware "snap an arbitrary vector to the nearest of 8" the brief
//! calls "nearest of 8" is, per `src/grid.rs`, the **resolver's** R1
//! `direction_to`, NOT A2's `from_to`. We therefore test the octant contract
//! and its load-bearing property (stepping along it reduces Chebyshev distance);
//! the magnitude-snap tests belong to the geometry suite (T2) once R1 lands.

use broadside_engine::grid::{
    all_positions, distance, from_to, neighbors, offset, range_band, Axis, Dir4, Dir8, Facing, Pos,
    Range, CELLS, COLS, ROWS,
};
use proptest::prelude::*;

/* =========================================================================
 * Local enumerations (NOT oracles — just the finite domains to sweep)
 *
 * These mirror the module's own `ALL` constants but are kept local so the
 * integration test pins the *set of variants* independently: if someone adds a
 * 9th `Dir8` and forgets a match arm somewhere, the module `ALL` would grow
 * with it and silently hide the gap, whereas a stale local list makes the new
 * variant visibly untested.
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

const DIAGONALS: [Dir8; 4] = [Dir8::NE, Dir8::SE, Dir8::SW, Dir8::NW];

const ALL_AXES: [Axis; 2] = [Axis::NorthSouth, Axis::EastWest];

/// Every in-bounds position, built without using the module's `all_positions`
/// (so tests of `all_positions` itself have an independent reference set).
fn every_cell() -> Vec<Pos> {
    let mut v = Vec::with_capacity(CELLS);
    for row in 0..ROWS {
        for col in 0..COLS {
            v.push(Pos::new(col, row));
        }
    }
    v
}

/* =========================================================================
 * proptest strategies
 * ====================================================================== */

/// An arbitrary in-bounds [`Pos`].
fn any_pos() -> impl Strategy<Value = Pos> {
    (0..COLS, 0..ROWS).prop_map(|(col, row)| Pos::new(col, row))
}

/// An arbitrary [`Dir8`] (uniform over the eight).
fn any_dir8() -> impl Strategy<Value = Dir8> {
    (0u8..8).prop_map(Dir8::from_step)
}

/* =========================================================================
 * Pos ↔ index round-trip and bounds (exhaustive over all CELLS)
 * ====================================================================== */

#[test]
fn to_index_then_from_index_is_identity_for_every_cell() {
    for p in every_cell() {
        let i = p.to_index();
        assert!(i < CELLS, "{p:?} indexes inside the board");
        assert_eq!(
            Pos::from_index(i),
            Some(p),
            "to_index∘from_index round-trips {p:?}"
        );
    }
}

#[test]
fn from_index_then_to_index_is_identity_for_every_valid_index() {
    for i in 0..CELLS {
        let p = Pos::from_index(i).expect("index < CELLS is Some");
        assert_eq!(p.to_index(), i, "from_index∘to_index round-trips index {i}");
        assert!(p.in_bounds(), "recovered {p:?} is in bounds");
    }
}

#[test]
fn to_index_is_a_bijection_onto_zero_until_cells() {
    // Every cell maps to a DISTINCT index, and together they cover 0..CELLS
    // exactly once. (Catches a transposed `col*ROWS+row` row-major bug that a
    // single-cell round-trip can miss when COLS==ROWS — here COLS≠ROWS, but we
    // assert coverage explicitly so the property holds regardless.)
    let mut seen = vec![false; CELLS];
    for p in every_cell() {
        let i = p.to_index();
        assert!(!seen[i], "index {i} produced twice (collision at {p:?})");
        seen[i] = true;
    }
    assert!(seen.into_iter().all(|b| b), "indices 0..CELLS all covered");
}

#[test]
fn from_index_rejects_the_first_out_of_range_index() {
    assert_eq!(
        Pos::from_index(CELLS),
        None,
        "index == CELLS is out of range"
    );
    assert_eq!(Pos::from_index(CELLS + 1), None);
    assert_eq!(Pos::from_index(usize::MAX), None);
}

#[test]
fn index_is_row_major_col_is_the_fast_axis() {
    // Stepping +1 in col steps +1 in index; stepping +1 in row steps +COLS.
    // (Pins the row-major orientation: the flat Vec fills a whole row before
    // advancing depth, so the back row 0 occupies indices 0..COLS.)
    assert_eq!(Pos::new(0, 0).to_index(), 0);
    assert_eq!(Pos::new(1, 0).to_index(), 1);
    assert_eq!(Pos::new(COLS - 1, 0).to_index(), COLS - 1);
    assert_eq!(Pos::new(0, 1).to_index(), COLS);
    assert_eq!(Pos::new(COLS - 1, ROWS - 1).to_index(), CELLS - 1);
}

/* =========================================================================
 * Absolute frame convention — the load-bearing "which way is the camera" pin
 *
 * Everything above this point is SIGN-AGNOSTIC: the round-trips, the
 * delta+opposite cancellation, the from_to/delta sign-consistency — all survive
 * a global flip of the N/S (or E/W) delta signs, because they only assert
 * *relative* structure. But the module docs fix an ABSOLUTE frame the projector
 * (D2) and the AI depend on: `row 0` is the far/back row (enemy spawn), `row
 * ROWS-1` is the front row nearest the camera/player; `Dir8::S` (and `Dir4::S`)
 * points toward the camera by INCREASING row, `Dir8::N` away by decreasing it;
 * `E` increases col, `W` decreases it. These tests pin those exact signs, so
 * flipping a single delta constant in `src/grid.rs` — which would invert the
 * board under the renderer and make the AI "close" by retreating — fails LOUDLY
 * here instead of passing silently through the symmetric suite below.
 * ====================================================================== */

#[test]
fn row_zero_is_the_far_back_row_and_the_front_row_faces_the_camera() {
    // The blueprint frame (decision context, module docs): enemies spawn at the
    // back (row 0), the player/camera is at the front (row ROWS-1). Pin both the
    // endpoints and that increasing `row` walks back→front in index order.
    let back = Pos::new(0, 0);
    let front = Pos::new(0, ROWS - 1);
    assert_eq!(back.row, 0, "the back/far row is row 0");
    assert_eq!(front.row, ROWS - 1, "the front/camera row is row ROWS-1");
    // The front row sits at a STRICTLY GREATER flat index than the back row in
    // the same column — i.e. row increases toward the camera, monotonically.
    assert!(
        front.to_index() > back.to_index(),
        "front row (toward camera) has the higher flat index"
    );
    // Walking column 0 from back to front, the index strictly increases by COLS
    // each step (depth advances one full row toward the camera per row++).
    for r in 1..ROWS {
        let here = Pos::new(0, r);
        let behind = Pos::new(0, r - 1);
        assert_eq!(
            here.to_index(),
            behind.to_index() + COLS,
            "row {r} (one nearer the camera) is COLS past row {}",
            r - 1
        );
    }
}

#[test]
fn cardinal_deltas_pin_the_absolute_frame_not_just_a_consistent_one() {
    // THE pin the symmetric suite cannot make: the exact unit step of each
    // cardinal. `+row` is toward the camera, so S must be (0, +1) and N (0, -1);
    // `+col` is rightward, so E is (+1, 0) and W (-1, 0). A flip of either pair
    // in `src/grid.rs` lands here.
    assert_eq!(
        Dir8::N.delta(),
        (0, -1),
        "N steps AWAY from the camera (row--)"
    );
    assert_eq!(Dir8::S.delta(), (0, 1), "S steps TOWARD the camera (row++)");
    assert_eq!(Dir8::E.delta(), (1, 0), "E increases col (rightward)");
    assert_eq!(Dir8::W.delta(), (-1, 0), "W decreases col (leftward)");
    // And the four diagonals are the exact componentwise combination of their
    // cardinal parts under that same frame (so a diagonal can't be flipped on
    // one axis independently).
    assert_eq!(Dir8::NE.delta(), (1, -1), "NE = E + N");
    assert_eq!(Dir8::SE.delta(), (1, 1), "SE = E + S");
    assert_eq!(Dir8::SW.delta(), (-1, 1), "SW = W + S");
    assert_eq!(Dir8::NW.delta(), (-1, -1), "NW = W + N");
}

#[test]
fn stepping_south_increases_row_toward_the_camera_and_north_decreases_it() {
    // Expressed through `offset` (the function callers actually use to move), not
    // just `delta`, so the frame is pinned at the API the resolver/AI call.
    // From every cell with room ahead, S lands one row nearer the camera.
    for p in every_cell() {
        if p.row + 1 < ROWS {
            assert_eq!(
                offset(p, Dir8::S, 1),
                Some(Pos::new(p.col, p.row + 1)),
                "S from {p:?} moves toward the camera (row+1)"
            );
        }
        if p.row >= 1 {
            assert_eq!(
                offset(p, Dir8::N, 1),
                Some(Pos::new(p.col, p.row - 1)),
                "N from {p:?} moves away from the camera (row-1)"
            );
        }
        if p.col + 1 < COLS {
            assert_eq!(
                offset(p, Dir8::E, 1),
                Some(Pos::new(p.col + 1, p.row)),
                "E from {p:?} moves right (col+1)"
            );
        }
        if p.col >= 1 {
            assert_eq!(
                offset(p, Dir8::W, 1),
                Some(Pos::new(p.col - 1, p.row)),
                "W from {p:?} moves left (col-1)"
            );
        }
    }
}

#[test]
fn an_enemy_at_the_back_closing_on_a_front_player_steps_south() {
    // The concrete gameplay reading of the frame: an enemy spawned at the back
    // (low row) closing on the player at the front (high row) must head SOUTH
    // (toward the camera). If the N/S frame were flipped, `from_to` would report
    // N here and the AI's "close the distance" would walk enemies off the back
    // wall — this asserts the sign that prevents that.
    let enemy = Pos::new(2, 0); // back row, centre column
    let player = Pos::new(2, ROWS - 1); // front row, same column
    assert_eq!(from_to(enemy, player), Some(Dir8::S), "back→front bears S");
    // And the reciprocal: the player bears N to the enemy.
    assert_eq!(from_to(player, enemy), Some(Dir8::N), "front→back bears N");
    // A diagonal close from a back corner toward a front-centre player bears a
    // southerly (camera-ward) diagonal, never a northerly one.
    let corner = Pos::new(0, 0);
    let bearing = from_to(corner, player).expect("distinct");
    assert!(
        matches!(bearing, Dir8::SE | Dir8::S),
        "back-left → front-centre bears camera-ward (got {bearing:?})"
    );
}

#[test]
fn dir4_cardinals_inherit_the_same_absolute_frame_via_dir8() {
    // The stance/facing cardinals must share the board frame, otherwise a
    // Bow(S) ship would face away from the camera while the world's S points
    // toward it. Dir4 has no `delta` of its own, so pin it through `to_dir8`.
    assert_eq!(
        Dir4::N.to_dir8().delta(),
        (0, -1),
        "Dir4::N is world N (row--)"
    );
    assert_eq!(
        Dir4::S.to_dir8().delta(),
        (0, 1),
        "Dir4::S is world S (row++)"
    );
    assert_eq!(
        Dir4::E.to_dir8().delta(),
        (1, 0),
        "Dir4::E is world E (col++)"
    );
    assert_eq!(
        Dir4::W.to_dir8().delta(),
        (-1, 0),
        "Dir4::W is world W (col--)"
    );
    // The "positive" direction of each axis (used by `Axis::dirs`) is the one
    // whose delta has a +1 component — S on NorthSouth, E on EastWest. This ties
    // the Axis::dirs ordering (tested elsewhere structurally) to the ABSOLUTE
    // sign, closing the loop between the axis API and the board frame.
    let (ns_pos, _) = Axis::NorthSouth.dirs();
    let (ew_pos, _) = Axis::EastWest.dirs();
    assert_eq!(
        ns_pos.to_dir8().delta(),
        (0, 1),
        "NorthSouth positive is +row (S)"
    );
    assert_eq!(
        ew_pos.to_dir8().delta(),
        (1, 0),
        "EastWest positive is +col (E)"
    );
}

#[test]
fn in_bounds_is_true_exactly_inside_the_grid() {
    // Inside.
    for p in every_cell() {
        assert!(p.in_bounds(), "{p:?} is inside");
    }
    // Just outside on each axis (and the diagonal corner just past the far
    // corner). `Pos::new` is unchecked, so these are constructible.
    assert!(!Pos::new(COLS, 0).in_bounds(), "col == COLS is out");
    assert!(!Pos::new(0, ROWS).in_bounds(), "row == ROWS is out");
    assert!(!Pos::new(COLS, ROWS).in_bounds(), "far corner +1,+1 is out");
}

#[test]
fn all_positions_matches_an_independent_row_major_enumeration() {
    // `all_positions` must equal the same set in the same order, and every
    // entry must round-trip to its slot index.
    let lib = all_positions();
    assert_eq!(lib.len(), CELLS, "all_positions has CELLS entries");
    assert_eq!(
        lib,
        every_cell(),
        "all_positions is row-major over the grid"
    );
    for (i, p) in lib.into_iter().enumerate() {
        assert_eq!(p.to_index(), i, "all_positions[{i}] sits at index {i}");
    }
}

proptest! {
    /// For ANY usize, `from_index` is `Some` iff the index is in range, and when
    /// `Some` it round-trips back to the same index. Closes the unbounded-input
    /// gap the per-cell table cannot (table only covers `0..CELLS`).
    #[test]
    fn from_index_round_trips_or_rejects_for_any_usize(i in any::<usize>()) {
        match Pos::from_index(i) {
            Some(p) => {
                prop_assert!(i < CELLS, "Some only for in-range index");
                prop_assert!(p.in_bounds());
                prop_assert_eq!(p.to_index(), i);
            }
            None => prop_assert!(i >= CELLS, "None only for out-of-range index"),
        }
    }

    /// For ANY in-bounds Pos, the index→Pos→index loop is identity. (The
    /// forward direction of the round-trip over the random-Pos generator.)
    #[test]
    fn pos_index_round_trip_for_any_in_bounds_pos(p in any_pos()) {
        prop_assert_eq!(Pos::from_index(p.to_index()), Some(p));
    }
}

/* =========================================================================
 * Dir8 — rotation / opposite involutions (exhaustive over all 8)
 * ====================================================================== */

#[test]
fn step_then_from_step_is_identity_for_every_dir8() {
    for d in ALL_DIR8 {
        assert_eq!(Dir8::from_step(d.step()), d, "step round-trips {d:?}");
        assert!(d.step() < 8, "{d:?} step is in 0..8");
    }
}

#[test]
fn dir8_steps_are_distinct_and_cover_zero_until_eight() {
    // The eight directions occupy the eight clockwise slots exactly once.
    let mut seen = [false; 8];
    for d in ALL_DIR8 {
        let s = d.step() as usize;
        assert!(!seen[s], "step {s} assigned twice");
        seen[s] = true;
    }
    assert!(seen.into_iter().all(|b| b), "all 8 clockwise slots used");
}

#[test]
fn opposite_is_an_involution_and_changes_every_direction() {
    for d in ALL_DIR8 {
        assert_eq!(
            d.opposite().opposite(),
            d,
            "opposite∘opposite == id for {d:?}"
        );
        assert_ne!(d.opposite(), d, "no direction is its own opposite");
    }
}

#[test]
fn opposite_is_four_clockwise_steps() {
    // Relationship to the step arithmetic, asserted as a property of step (not a
    // re-derivation of opposite's match body).
    for d in ALL_DIR8 {
        assert_eq!(d.opposite(), Dir8::from_step(d.step() + 4));
        // Four CW rotations also reach the opposite.
        assert_eq!(
            d.rotate_cw().rotate_cw().rotate_cw().rotate_cw(),
            d.opposite()
        );
    }
}

#[test]
fn rotate_cw_and_ccw_are_mutual_inverses() {
    for d in ALL_DIR8 {
        assert_eq!(d.rotate_cw().rotate_ccw(), d, "cw then ccw == id for {d:?}");
        assert_eq!(d.rotate_ccw().rotate_cw(), d, "ccw then cw == id for {d:?}");
    }
}

#[test]
fn eight_rotations_in_either_direction_return_to_start() {
    for d in ALL_DIR8 {
        let mut cw = d;
        let mut ccw = d;
        for _ in 0..8 {
            cw = cw.rotate_cw();
            ccw = ccw.rotate_ccw();
        }
        assert_eq!(cw, d, "8 CW rotations cycle {d:?}");
        assert_eq!(ccw, d, "8 CCW rotations cycle {d:?}");
    }
}

#[test]
fn rotate_cw_advances_exactly_one_clockwise_slot() {
    // The single-step rotation is +1 (mod 8) in step space; ccw is -1.
    for d in ALL_DIR8 {
        assert_eq!(d.rotate_cw(), Dir8::from_step(d.step() + 1));
        assert_eq!(d.rotate_ccw(), Dir8::from_step(d.step() + 7));
    }
}

#[test]
fn is_cardinal_partitions_the_eight_into_four_and_four() {
    let cardinals: Vec<Dir8> = ALL_DIR8.into_iter().filter(|d| d.is_cardinal()).collect();
    let diagonals: Vec<Dir8> = ALL_DIR8.into_iter().filter(|d| !d.is_cardinal()).collect();
    assert_eq!(
        cardinals,
        [Dir8::N, Dir8::E, Dir8::S, Dir8::W],
        "the four cardinals"
    );
    assert_eq!(diagonals, DIAGONALS.to_vec(), "the four diagonals");
    // A cardinal's opposite is a cardinal; a diagonal's opposite is a diagonal.
    for d in ALL_DIR8 {
        assert_eq!(d.opposite().is_cardinal(), d.is_cardinal(), "{d:?}");
    }
}

#[test]
fn delta_plus_opposite_delta_cancels_for_every_direction() {
    // The unit step and its opposite are negatives — pins delta against opposite
    // without re-listing the eight (dc,dr) pairs.
    for d in ALL_DIR8 {
        let (dc, dr) = d.delta();
        let (oc, or) = d.opposite().delta();
        assert_eq!(
            (dc + oc, dr + or),
            (0, 0),
            "{d:?} delta + opposite delta == 0"
        );
    }
}

#[test]
fn delta_magnitudes_match_cardinal_vs_diagonal() {
    // Cardinal steps move on exactly one axis; diagonal steps move on both.
    // Every component is in {-1,0,1} and the step is never the zero vector.
    for d in ALL_DIR8 {
        let (dc, dr) = d.delta();
        assert!(
            (-1..=1).contains(&dc) && (-1..=1).contains(&dr),
            "{d:?} unit components"
        );
        assert_ne!((dc, dr), (0, 0), "{d:?} is a real step");
        let nonzero = u8::from(dc != 0) + u8::from(dr != 0);
        if d.is_cardinal() {
            assert_eq!(nonzero, 1, "cardinal {d:?} moves on one axis");
        } else {
            assert_eq!(nonzero, 2, "diagonal {d:?} moves on both axes");
        }
    }
}

proptest! {
    /// `from_step` is well-defined for ANY u8 (taken mod 8) and agrees with the
    /// canonical reduction. Pins the "never panics" promise on the rotation
    /// arithmetic's wraparound input.
    #[test]
    fn from_step_is_mod_eight_for_any_u8(s in any::<u8>()) {
        prop_assert_eq!(Dir8::from_step(s), Dir8::from_step(s % 8));
        // And it agrees with widening the in-range representative.
        prop_assert_eq!(Dir8::from_step(s).step(), s % 8);
    }
}

/* =========================================================================
 * from_to — the grid-step octant (exhaustive over the 8 octants + properties)
 * ====================================================================== */

#[test]
fn from_to_is_none_exactly_when_source_equals_target() {
    // None for the same cell, for every cell; Some otherwise.
    for a in every_cell() {
        assert_eq!(from_to(a, a), None, "same cell {a:?} has no direction");
        for b in every_cell() {
            if a != b {
                assert!(from_to(a, b).is_some(), "{a:?}->{b:?} has a direction");
            }
        }
    }
}

#[test]
fn from_to_reports_the_octant_of_the_step_by_sign() {
    // From a central cell, each of the eight surrounding cells resolves to the
    // matching Dir8. This is the defining contract of the signum octant.
    let c = Pos::new(2, 2);
    let cases = [
        (Pos::new(2, 1), Dir8::N),
        (Pos::new(3, 1), Dir8::NE),
        (Pos::new(3, 2), Dir8::E),
        (Pos::new(3, 3), Dir8::SE),
        (Pos::new(2, 3), Dir8::S),
        (Pos::new(1, 3), Dir8::SW),
        (Pos::new(1, 2), Dir8::W),
        (Pos::new(1, 1), Dir8::NW),
    ];
    for (b, want) in cases {
        assert_eq!(from_to(c, b), Some(want), "{c:?}->{b:?}");
    }
}

#[test]
fn from_to_octant_is_translation_invariant() {
    // The same relative step yields the same octant regardless of where it
    // starts — pins that from_to keys on the delta, not absolute position.
    // Step (+2,+1): col rises, row rises ⇒ SE octant, anywhere it fits.
    let starts = [Pos::new(0, 0), Pos::new(1, 1), Pos::new(2, 2)];
    for s in starts {
        let b = Pos::new(s.col + 2, s.row + 1);
        assert_eq!(from_to(s, b), Some(Dir8::SE), "{s:?} -> {b:?}");
    }
    // Step (+3,0) is due E from several columns.
    assert_eq!(from_to(Pos::new(0, 1), Pos::new(3, 1)), Some(Dir8::E));
    assert_eq!(from_to(Pos::new(1, 0), Pos::new(4, 0)), Some(Dir8::E));
}

proptest! {
    /// For ANY two distinct in-bounds cells, stepping ONE cell along `from_to`
    /// strictly reduces Chebyshev distance and stays in bounds. This is the
    /// load-bearing property the AI's "close the distance" relies on — and the
    /// reason the signum octant is the right tool (it always points downhill).
    #[test]
    fn stepping_along_from_to_reduces_distance(a in any_pos(), b in any_pos()) {
        prop_assume!(a != b);
        let d = from_to(a, b).expect("distinct cells have a direction");
        let next = offset(a, d, 1).expect("a single step from an in-bounds cell stays in bounds");
        prop_assert!(next.in_bounds());
        prop_assert_eq!(
            distance(next, b) + 1,
            distance(a, b),
            "one step toward {:?} cuts distance by exactly 1 (from {:?} via {:?})",
            b, a, d
        );
    }

    /// `from_to` agrees with the sign of each axis delta — stated as a property
    /// over arbitrary cell pairs rather than the eight-arm table. (col rises ⇒
    /// an easterly octant; row rises ⇒ a southerly octant; etc.)
    #[test]
    fn from_to_sign_consistency(a in any_pos(), b in any_pos()) {
        if let Some(d) = from_to(a, b) {
            let (dc, dr) = d.delta();
            prop_assert_eq!(dc.signum(), (b.col as i32 - a.col as i32).signum(), "col sign");
            prop_assert_eq!(dr.signum(), (b.row as i32 - a.row as i32).signum(), "row sign");
        } else {
            prop_assert_eq!(a, b, "None only when equal");
        }
    }
}

/* =========================================================================
 * offset / neighbors — bounds and clamping
 * ====================================================================== */

#[test]
fn offset_returns_none_off_the_near_edge_underflow() {
    // Stepping past col 0 / row 0 underflows and must be rejected (not wrap).
    assert_eq!(offset(Pos::new(0, 0), Dir8::W, 1), None, "W off col 0");
    assert_eq!(offset(Pos::new(0, 0), Dir8::N, 1), None, "N off row 0");
    assert_eq!(
        offset(Pos::new(0, 0), Dir8::NW, 1),
        None,
        "NW off the corner"
    );
    // Edge cells: stepping along the edge stays on, stepping off it is None.
    assert_eq!(
        offset(Pos::new(0, 2), Dir8::W, 1),
        None,
        "W off the left wall"
    );
    assert_eq!(
        offset(Pos::new(2, 0), Dir8::N, 1),
        None,
        "N off the back wall"
    );
}

#[test]
fn offset_returns_none_off_the_far_edge() {
    let far = Pos::new(COLS - 1, ROWS - 1);
    assert_eq!(offset(far, Dir8::E, 1), None, "E off the right wall");
    assert_eq!(offset(far, Dir8::S, 1), None, "S off the front wall");
    assert_eq!(offset(far, Dir8::SE, 1), None, "SE off the far corner");
}

#[test]
fn offset_steps_interior_cells_on_grid() {
    assert_eq!(offset(Pos::new(1, 1), Dir8::SE, 1), Some(Pos::new(2, 2)));
    assert_eq!(offset(Pos::new(2, 2), Dir8::N, 1), Some(Pos::new(2, 1)));
    assert_eq!(offset(Pos::new(2, 2), Dir8::W, 2), Some(Pos::new(0, 2)));
}

#[test]
fn offset_negative_distance_steps_backward() {
    // A negative distance walks the opposite way without flipping the direction;
    // the bounds gate still applies at the other end.
    assert_eq!(offset(Pos::new(1, 1), Dir8::SE, -1), Some(Pos::new(0, 0)));
    assert_eq!(offset(Pos::new(2, 2), Dir8::E, -2), Some(Pos::new(0, 2)));
    assert_eq!(
        offset(Pos::new(0, 0), Dir8::SE, -1),
        None,
        "backward off the near corner"
    );
}

#[test]
fn offset_distance_zero_is_the_same_cell() {
    for p in every_cell() {
        for d in ALL_DIR8 {
            assert_eq!(
                offset(p, d, 0),
                Some(p),
                "zero step from {p:?} via {d:?} stays put"
            );
        }
    }
}

#[test]
fn offset_one_then_opposite_one_returns_home() {
    // Where a forward step lands on-grid, the opposite step returns to start.
    for p in every_cell() {
        for d in ALL_DIR8 {
            if let Some(next) = offset(p, d, 1) {
                assert_eq!(
                    offset(next, d.opposite(), 1),
                    Some(p),
                    "{p:?} via {d:?} then back via {:?}",
                    d.opposite()
                );
            }
        }
    }
}

#[test]
fn neighbors_count_is_three_five_or_eight_by_position_class() {
    // Corners have 3, edges 5, interior 8.
    assert_eq!(neighbors(Pos::new(0, 0)).len(), 3, "back-left corner");
    assert_eq!(
        neighbors(Pos::new(COLS - 1, 0)).len(),
        3,
        "back-right corner"
    );
    assert_eq!(
        neighbors(Pos::new(0, ROWS - 1)).len(),
        3,
        "front-left corner"
    );
    assert_eq!(
        neighbors(Pos::new(COLS - 1, ROWS - 1)).len(),
        3,
        "front-right corner"
    );
    assert_eq!(neighbors(Pos::new(1, 0)).len(), 5, "back edge");
    assert_eq!(neighbors(Pos::new(0, 1)).len(), 5, "left edge");
    assert_eq!(neighbors(Pos::new(2, ROWS - 1)).len(), 5, "front edge");
    assert_eq!(neighbors(Pos::new(2, 1)).len(), 8, "interior");
}

#[test]
fn every_neighbor_is_in_bounds_distinct_and_one_step_away() {
    for p in every_cell() {
        let ns = neighbors(p);
        // In bounds + exactly Chebyshev 1 from p + never p itself.
        for &n in &ns {
            assert!(n.in_bounds(), "{n:?} (neighbour of {p:?}) is in bounds");
            assert_ne!(n, p, "a cell is not its own neighbour");
            assert_eq!(distance(p, n), 1, "{n:?} is one Chebyshev step from {p:?}");
        }
        // No duplicates.
        let mut sorted: Vec<(usize, usize)> = ns.iter().map(|q| (q.col, q.row)).collect();
        sorted.sort_unstable();
        let len = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), len, "neighbours of {p:?} are distinct");
    }
}

#[test]
fn neighbors_equals_the_in_bounds_unit_offsets() {
    // `neighbors` is exactly the set of in-bounds 1-steps in Dir8::ALL order.
    // Pins it against `offset` so the two can't drift apart.
    for p in every_cell() {
        let from_offset: Vec<Pos> = ALL_DIR8.iter().filter_map(|&d| offset(p, d, 1)).collect();
        assert_eq!(
            neighbors(p),
            from_offset,
            "neighbours of {p:?} == in-bounds 1-offsets"
        );
    }
}

proptest! {
    /// For ANY in-bounds cell, ANY direction, ANY distance, `offset` returns a
    /// cell that is `Some` iff it is in bounds — it NEVER returns an
    /// out-of-bounds `Pos` and never silently wraps. (Distance bounded to a
    /// window comfortably past both walls so both the in- and out-of-bounds
    /// branches are exercised.)
    #[test]
    fn offset_is_some_iff_in_bounds(p in any_pos(), d in any_dir8(), dist in -6i32..=6) {
        let (dc, dr) = d.delta();
        let col = p.col as i32 + dc * dist;
        let row = p.row as i32 + dr * dist;
        let in_grid = (0..COLS as i32).contains(&col) && (0..ROWS as i32).contains(&row);
        match offset(p, d, dist) {
            Some(q) => {
                prop_assert!(in_grid, "Some only when the target is on-grid");
                prop_assert!(q.in_bounds(), "returned Pos is in bounds");
                prop_assert_eq!((q.col as i32, q.row as i32), (col, row), "lands on the stepped cell");
            }
            None => prop_assert!(!in_grid, "None only when the target is off-grid"),
        }
    }
}

/* =========================================================================
 * Chebyshev distance — metric properties (exhaustive pairs + proptest axioms)
 * ====================================================================== */

#[test]
fn distance_is_zero_iff_same_cell() {
    for a in every_cell() {
        assert_eq!(distance(a, a), 0, "identity at {a:?}");
        for b in every_cell() {
            if a != b {
                assert!(
                    distance(a, b) > 0,
                    "distinct cells {a:?},{b:?} are > 0 apart"
                );
            }
        }
    }
}

#[test]
fn distance_is_symmetric_over_every_pair() {
    for a in every_cell() {
        for b in every_cell() {
            assert_eq!(distance(a, b), distance(b, a), "symmetry {a:?},{b:?}");
        }
    }
}

#[test]
fn distance_is_chebyshev_diagonal_step_costs_one() {
    // The discriminator that makes this Chebyshev and not Manhattan/Euclid: a
    // pure diagonal of length k costs k (max axis), not 2k and not k·√2.
    assert_eq!(
        distance(Pos::new(0, 0), Pos::new(3, 3)),
        3,
        "diag 3 costs 3"
    );
    assert_eq!(
        distance(Pos::new(0, 0), Pos::new(1, 1)),
        1,
        "diag 1 costs 1"
    );
    // A mixed step takes the LARGER axis delta, not the sum.
    assert_eq!(distance(Pos::new(0, 0), Pos::new(4, 1)), 4, "max(4,1) == 4");
    assert_eq!(distance(Pos::new(0, 0), Pos::new(1, 3)), 3, "max(1,3) == 3");
    // Pure-lateral / pure-depth gaps are the single-axis delta.
    assert_eq!(distance(Pos::new(0, 2), Pos::new(4, 2)), 4, "lateral");
    assert_eq!(distance(Pos::new(2, 0), Pos::new(2, 3)), 3, "depth");
}

#[test]
fn the_diameter_of_the_grid_is_the_long_diagonal() {
    // The two farthest-apart cells are the opposite corners; on 5×4 that is
    // max(COLS-1, ROWS-1) == 4. No pair exceeds it.
    let mut max_seen = 0;
    for a in every_cell() {
        for b in every_cell() {
            max_seen = max_seen.max(distance(a, b));
        }
    }
    assert_eq!(max_seen, (COLS - 1).max(ROWS - 1), "grid diameter");
    assert_eq!(
        distance(Pos::new(0, 0), Pos::new(COLS - 1, 0)),
        COLS - 1,
        "widest row"
    );
}

proptest! {
    /// Metric axioms over arbitrary triples: non-negativity, identity-of-
    /// indiscernibles, symmetry, and the triangle inequality. The triangle
    /// inequality in particular is a relationship no single formula re-derives —
    /// it ties three independent distance calls together.
    #[test]
    fn distance_obeys_the_metric_axioms(a in any_pos(), b in any_pos(), c in any_pos()) {
        let dab = distance(a, b);
        // Non-negativity is structural (usize) but identity is a real claim.
        prop_assert_eq!(dab == 0, a == b, "d==0 iff a==b");
        // Symmetry.
        prop_assert_eq!(dab, distance(b, a));
        // Triangle inequality: a→b is no longer than a→c→b.
        prop_assert!(dab <= distance(a, c) + distance(c, b), "triangle via {:?}", c);
    }

    /// A single Dir8 step changes distance to any fixed point by at most 1 (it
    /// is a unit step under the Chebyshev metric). Combined with the
    /// from_to-reduces-distance property, this pins step size in both
    /// directions.
    #[test]
    fn one_step_changes_distance_by_at_most_one(p in any_pos(), d in any_dir8(), target in any_pos()) {
        if let Some(q) = offset(p, d, 1) {
            let before = distance(p, target) as i32;
            let after = distance(q, target) as i32;
            prop_assert!((after - before).abs() <= 1, "unit step moves distance by ≤1");
        }
    }
}

/* =========================================================================
 * range_band — the 3-band Chebyshev buckets (decision #6 boundaries)
 * ====================================================================== */

#[test]
fn range_band_boundaries_match_decision_six() {
    // Cuts: 0–1 → Adjacent, 2 → Near, 3+ → Far. Walk a straight column line so
    // the distance equals the row delta, hitting each boundary exactly.
    let o = Pos::new(0, 0);
    assert_eq!(range_band(o, o), Range::Adjacent, "dist 0");
    assert_eq!(range_band(o, Pos::new(0, 1)), Range::Adjacent, "dist 1");
    assert_eq!(range_band(o, Pos::new(0, 2)), Range::Near, "dist 2");
    assert_eq!(range_band(o, Pos::new(0, 3)), Range::Far, "dist 3");
    // Lateral confirms the same cuts on the wider axis (up to dist 4).
    assert_eq!(
        range_band(o, Pos::new(1, 0)),
        Range::Adjacent,
        "dist 1 lateral"
    );
    assert_eq!(range_band(o, Pos::new(2, 0)), Range::Near, "dist 2 lateral");
    assert_eq!(range_band(o, Pos::new(3, 0)), Range::Far, "dist 3 lateral");
    assert_eq!(range_band(o, Pos::new(4, 0)), Range::Far, "dist 4 lateral");
}

#[test]
fn range_band_counts_a_diagonal_step_as_adjacent() {
    // Because the metric is Chebyshev, a diagonal neighbour is Adjacent (dist 1),
    // not Near — the property that lets corner pressure read as point-blank.
    let o = Pos::new(0, 0);
    assert_eq!(
        range_band(o, Pos::new(1, 1)),
        Range::Adjacent,
        "diagonal neighbour"
    );
    // A dist-2 diagonal is Near.
    assert_eq!(
        range_band(o, Pos::new(2, 2)),
        Range::Near,
        "two-step diagonal"
    );
}

#[test]
fn range_band_agrees_with_the_distance_bucket_over_every_pair() {
    // Cross-function consistency: the band is exactly the bucket of `distance`,
    // for all 400 ordered pairs. Asserts the RELATIONSHIP between range_band and
    // distance (not a re-derivation of either body).
    for a in every_cell() {
        for b in every_cell() {
            let want = match distance(a, b) {
                0 | 1 => Range::Adjacent,
                2 => Range::Near,
                _ => Range::Far,
            };
            assert_eq!(range_band(a, b), want, "band of {a:?},{b:?}");
        }
    }
}

#[test]
fn range_band_is_symmetric() {
    for a in every_cell() {
        for b in every_cell() {
            assert_eq!(range_band(a, b), range_band(b, a), "{a:?},{b:?}");
        }
    }
}

proptest! {
    /// The band is monotonic in distance: farther never reads as a nearer band.
    /// (Adjacent < Near < Far as buckets of a non-decreasing metric.) Pins the
    /// ordering without re-listing the cut points.
    #[test]
    fn range_band_is_monotonic_in_distance(a in any_pos(), b in any_pos(), c in any_pos(), e in any_pos()) {
        fn rank(r: Range) -> u8 {
            match r {
                Range::Adjacent => 0,
                Range::Near => 1,
                Range::Far => 2,
            }
        }
        if distance(a, b) <= distance(c, e) {
            prop_assert!(rank(range_band(a, b)) <= rank(range_band(c, e)),
                "closer pair is not in a farther band");
        }
    }
}

/* =========================================================================
 * Dir4 / Axis / Facing — the cardinal stance surface
 * ====================================================================== */

#[test]
fn dir4_to_dir8_maps_to_the_matching_cardinal() {
    // The widening is the obvious identity-of-name and always lands cardinal.
    let pairs = [
        (Dir4::N, Dir8::N),
        (Dir4::E, Dir8::E),
        (Dir4::S, Dir8::S),
        (Dir4::W, Dir8::W),
    ];
    for (d4, d8) in pairs {
        assert_eq!(d4.to_dir8(), d8, "{d4:?} widens to {d8:?}");
        assert!(d8.is_cardinal(), "{d8:?} is cardinal");
    }
}

#[test]
fn dir4_dir8_narrow_widen_round_trips_and_diagonals_do_not_narrow() {
    // Widen then narrow is identity on all four cardinals.
    for d4 in ALL_DIR4 {
        assert_eq!(
            Dir4::from_dir8(d4.to_dir8()),
            Some(d4),
            "{d4:?} round-trips"
        );
    }
    // Narrow then widen is identity on the four cardinal Dir8.
    for d8 in [Dir8::N, Dir8::E, Dir8::S, Dir8::W] {
        let d4 = Dir4::from_dir8(d8).expect("cardinal narrows");
        assert_eq!(d4.to_dir8(), d8, "{d8:?} narrows then widens back");
    }
    // Diagonals refuse to narrow.
    for diag in DIAGONALS {
        assert_eq!(Dir4::from_dir8(diag), None, "{diag:?} has no Dir4");
    }
}

#[test]
fn dir4_opposite_is_an_involution_and_flips_the_axis_sense() {
    for d4 in ALL_DIR4 {
        assert_eq!(
            d4.opposite().opposite(),
            d4,
            "opposite∘opposite == id for {d4:?}"
        );
        assert_ne!(d4.opposite(), d4, "no cardinal is its own opposite");
        // Opposite stays on the same axis, and matches the widened Dir8 opposite.
        assert_eq!(d4.opposite().axis(), d4.axis(), "opposite shares the axis");
        assert_eq!(
            d4.opposite().to_dir8(),
            d4.to_dir8().opposite(),
            "agrees with Dir8 opposite"
        );
    }
}

#[test]
fn dir4_axis_groups_n_s_and_e_w() {
    assert_eq!(Dir4::N.axis(), Axis::NorthSouth);
    assert_eq!(Dir4::S.axis(), Axis::NorthSouth);
    assert_eq!(Dir4::E.axis(), Axis::EastWest);
    assert_eq!(Dir4::W.axis(), Axis::EastWest);
    // Exactly two cardinals on each axis.
    for axis in ALL_AXES {
        let on_axis = ALL_DIR4.into_iter().filter(|d| d.axis() == axis).count();
        assert_eq!(on_axis, 2, "two cardinals on {axis:?}");
    }
}

#[test]
fn axis_dirs_are_the_two_opposite_cardinals_on_that_axis() {
    for axis in ALL_AXES {
        let (pos, neg) = axis.dirs();
        assert_eq!(pos.axis(), axis, "positive dir is on {axis:?}");
        assert_eq!(neg.axis(), axis, "negative dir is on {axis:?}");
        assert_eq!(pos.opposite(), neg, "the two dirs are opposites");
        assert_ne!(pos, neg);
    }
    // The "positive" dir is the increasing-coordinate one: +row (S) toward the
    // player for NorthSouth, +col (E) for EastWest — pins the frame convention.
    assert_eq!(
        Axis::NorthSouth.dirs(),
        (Dir4::S, Dir4::N),
        "NS positive is S (+row)"
    );
    assert_eq!(
        Axis::EastWest.dirs(),
        (Dir4::E, Dir4::W),
        "EW positive is E (+col)"
    );
}

#[test]
fn facing_forward_axis_is_the_bow_axis_or_the_hull_axis() {
    // Bow(dir) ⇒ the bow direction's axis.
    for d4 in ALL_DIR4 {
        assert_eq!(
            Facing::Bow(d4).forward_axis(),
            d4.axis(),
            "Bow({d4:?}) forward axis is the bow's axis"
        );
    }
    // Broadside(axis) ⇒ that axis verbatim.
    for axis in ALL_AXES {
        assert_eq!(
            Facing::Broadside(axis).forward_axis(),
            axis,
            "Broadside({axis:?}) forward axis is the hull axis"
        );
    }
    // Concretely: a N-facing bow and a Broadside-NS hull share the N/S forward
    // axis (the renderer's bow-arrow must encode the same axis for both).
    assert_eq!(
        Facing::Bow(Dir4::N).forward_axis(),
        Facing::Broadside(Axis::NorthSouth).forward_axis()
    );
    assert_eq!(
        Facing::Bow(Dir4::E).forward_axis(),
        Facing::Broadside(Axis::EastWest).forward_axis()
    );
    // Opposite bows share a forward axis (N and S both read NorthSouth).
    assert_eq!(
        Facing::Bow(Dir4::N).forward_axis(),
        Facing::Bow(Dir4::S).forward_axis()
    );
    assert_eq!(
        Facing::Bow(Dir4::E).forward_axis(),
        Facing::Bow(Dir4::W).forward_axis()
    );
}

/* =========================================================================
 * serde round-trips — the types cross the JSON catalog / save boundary
 * (decision #5: fixtures live in JSON), so byte-stable round-trip matters.
 * ====================================================================== */

#[test]
fn pos_round_trips_through_json_for_every_cell() {
    for p in every_cell() {
        let json = serde_json::to_string(&p).expect("Pos serializes");
        let back: Pos = serde_json::from_str(&json).expect("Pos deserializes");
        assert_eq!(back, p, "{p:?} survives JSON");
    }
}

#[test]
fn dir8_round_trips_through_json_for_every_direction() {
    for d in ALL_DIR8 {
        let json = serde_json::to_string(&d).expect("Dir8 serializes");
        let back: Dir8 = serde_json::from_str(&json).expect("Dir8 deserializes");
        assert_eq!(back, d, "{d:?} survives JSON");
    }
}

#[test]
fn facing_round_trips_through_json_for_both_stances() {
    // Internally-tagged (`stance`) enum — the same shape the live `Orientation`
    // uses, so this guards the catalog/save contract once A3 swaps the field.
    let facings = [
        Facing::Bow(Dir4::N),
        Facing::Bow(Dir4::E),
        Facing::Bow(Dir4::S),
        Facing::Bow(Dir4::W),
        Facing::Broadside(Axis::NorthSouth),
        Facing::Broadside(Axis::EastWest),
    ];
    for f in facings {
        let json = serde_json::to_string(&f).expect("Facing serializes");
        let back: Facing = serde_json::from_str(&json).expect("Facing deserializes");
        assert_eq!(back, f, "{f:?} survives JSON");
    }
    // The discriminant really is the `stance` tag (pins the wire shape so a
    // later rename of the tag is a visible test break, not a silent save-format
    // change).
    let s = serde_json::to_string(&Facing::Bow(Dir4::N)).unwrap();
    assert!(
        s.contains("\"stance\""),
        "Facing is tagged by `stance`: {s}"
    );
}

#[test]
fn range_round_trips_through_json() {
    for r in [Range::Adjacent, Range::Near, Range::Far] {
        let json = serde_json::to_string(&r).expect("Range serializes");
        let back: Range = serde_json::from_str(&json).expect("Range deserializes");
        assert_eq!(back, r, "{r:?} survives JSON");
    }
}
