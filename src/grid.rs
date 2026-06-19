//! 2D spatial type surface for the Broadside v2 grid (blueprint lane task A2).
//!
//! This module is the **WHERE** vocabulary the v2 combat redesign is built on:
//! a `5×4` grid (5 columns lateral × 4 rows depth, blueprint decision #2),
//! eight-way directions, the two 4-cardinal facing stances (decision #9), and a
//! 3-band Chebyshev range (decision #6). It is pure, dimension-aware data +
//! helpers — no board, no resolver, no rendering.
//!
//! ## Relationship to the (soon-replaced) 1-D `geometry` module
//!
//! Today's [`crate::geometry`] is the 1-D `LaneEnd`/`usize`-cell world. The v2
//! rebuild **replaces** the spatial layer: `cell: usize → Pos`, `LaneEnd →
//! Dir8`, the 5-band `RangeBand` → the 3-band [`Range`]. This file lands FIRST,
//! standalone and additive (blueprint A2), so nothing here migrates the live
//! `Board`/`Ship` yet — that is the single atomic type-surface commit (A3). The
//! resolver then rewrites `geometry.rs` over these types (R1).
//!
//! Names here (`distance`, `range_band`, `offset`, …) deliberately mirror the
//! 1-D vocabulary but live behind the `grid::` path and take [`Pos`]/[`Dir8`],
//! so there is no collision with the `usize`-based `geometry::distance` /
//! `geometry::range_band` during the half-migrated window.
//!
//! ## Coordinate frame
//!
//! - `col` increases left → right, `0..COLS` (lateral, the dodge axis).
//! - `row` increases **toward the player**: `row 0` is the far/back row (where
//!   enemies spawn), `row ROWS-1` is the front row (nearest the camera /
//!   player). The renderer's per-row depth scale grows with `row`. The combat
//!   model treats all rows as pure dodge space (decision #8); this module fixes
//!   only the numbering so the projector (D2) and AI agree on which way is
//!   "toward the player".
//! - `Dir8::N` points toward `row 0` (away from the player, decreasing `row`);
//!   `Dir8::S` points toward the player (increasing `row`). `E` increases
//!   `col`, `W` decreases `col`. This is screen-down-is-+row, matching the flat
//!   `Vec<Option<Ship>>` index order (`row * COLS + col`).

use serde::{Deserialize, Serialize};

/* =========================================================================
 * Grid dimensions
 * ====================================================================== */

/// Lateral columns (the dodge axis). Blueprint decision #2: 5 wide.
pub const COLS: usize = 5;
/// Depth rows. Blueprint decision #2: 4 deep.
pub const ROWS: usize = 4;
/// Total cells in the flat board vector (`COLS * ROWS`). The board is a
/// `Vec<Option<Ship>>` of exactly this length, indexed by [`Pos::to_index`].
pub const CELLS: usize = COLS * ROWS;

/* =========================================================================
 * Pos — a grid coordinate
 * ====================================================================== */

/// A cell coordinate on the 5×4 grid. `col` is `0..COLS`, `row` is `0..ROWS`.
///
/// `Pos` is the v2 replacement for the 1-D `cell: usize`. It maps to/from the
/// flat `Vec<Option<Ship>>` index via [`Pos::to_index`] / [`Pos::from_index`]
/// in **row-major** order (`row * COLS + col`), so the existing flat-vector
/// access patterns — the faction scan
/// (`cells.iter().find_map(|c| c.as_ref().and_then(|s| (s.faction == …)…))`)
/// and find-by-id (`cells.iter().position(|c| … s.id == …)`) — carry over
/// unchanged; only the eventual `s.cell` field type changes (in A3, not here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pos {
    pub col: usize,
    pub row: usize,
}

impl Pos {
    /// Construct a position. No bounds check — callers that take untrusted
    /// indices should use [`Pos::from_index`] (bounds-checked) or
    /// [`Pos::in_bounds`].
    pub const fn new(col: usize, row: usize) -> Self {
        Self { col, row }
    }

    /// `true` iff this position lies inside the `COLS × ROWS` grid.
    pub const fn in_bounds(self) -> bool {
        self.col < COLS && self.row < ROWS
    }

    /// Row-major flat index into a length-[`CELLS`] `Vec<Option<Ship>>`
    /// (`row * COLS + col`). Caller must ensure [`Pos::in_bounds`]; an
    /// out-of-range `Pos` yields an out-of-range index (a later `Vec` index
    /// would panic, matching the existing usize-cell behaviour).
    pub const fn to_index(self) -> usize {
        self.row * COLS + self.col
    }

    /// Inverse of [`Pos::to_index`]: recover the `Pos` for a flat index, or
    /// `None` if `index >= CELLS`. Use this when reading an index that came
    /// from outside (deserialized data, a loop bound) so an invalid index is a
    /// handled `None` rather than a wrong-cell silent bug.
    pub const fn from_index(index: usize) -> Option<Self> {
        if index >= CELLS {
            return None;
        }
        Some(Self {
            col: index % COLS,
            row: index / COLS,
        })
    }
}

/// Iterate every in-bounds [`Pos`] in flat row-major order (the same order as
/// `(0..CELLS).map(|i| Pos::from_index(i).unwrap())`). Handy for board scans
/// and tests; cheap (`Copy` items, no allocation beyond the returned `Vec`).
pub fn all_positions() -> Vec<Pos> {
    (0..CELLS)
        .map(|i| Pos {
            col: i % COLS,
            row: i / COLS,
        })
        .collect()
}

/* =========================================================================
 * Dir8 — eight-way direction
 * ====================================================================== */

/// One of the eight grid directions (4 cardinals + 4 diagonals).
///
/// Frame (see module docs): `N` decreases `row` (away from the player), `S`
/// increases `row` (toward the player), `E` increases `col`, `W` decreases
/// `col`. Diagonals combine the two. Ordered clockwise starting at `N` so
/// [`Dir8::rotate_cw`] / [`Dir8::rotate_ccw`] are `±1 (mod 8)` and
/// [`Dir8::opposite`] is `+4 (mod 8)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dir8 {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Dir8 {
    /// All eight directions in clockwise order from `N`.
    pub const ALL: [Self; 8] = [
        Self::N,
        Self::NE,
        Self::E,
        Self::SE,
        Self::S,
        Self::SW,
        Self::W,
        Self::NW,
    ];

    /// Clockwise index `0..8` (`N`=0, `NE`=1, … `NW`=7). The single source of
    /// truth for rotation/opposite arithmetic and the inverse of
    /// [`Dir8::from_step`].
    pub const fn step(self) -> u8 {
        match self {
            Self::N => 0,
            Self::NE => 1,
            Self::E => 2,
            Self::SE => 3,
            Self::S => 4,
            Self::SW => 5,
            Self::W => 6,
            Self::NW => 7,
        }
    }

    /// Inverse of [`Dir8::step`]: build a direction from a clockwise index,
    /// taken `mod 8` so rotation arithmetic never panics.
    pub const fn from_step(step: u8) -> Self {
        match step % 8 {
            0 => Self::N,
            1 => Self::NE,
            2 => Self::E,
            3 => Self::SE,
            4 => Self::S,
            5 => Self::SW,
            6 => Self::W,
            _ => Self::NW,
        }
    }

    /// The unit step this direction applies to a [`Pos`], as `(d_col, d_row)`.
    /// `+row` is toward the player (see module docs).
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Self::N => (0, -1),
            Self::NE => (1, -1),
            Self::E => (1, 0),
            Self::SE => (1, 1),
            Self::S => (0, 1),
            Self::SW => (-1, 1),
            Self::W => (-1, 0),
            Self::NW => (-1, -1),
        }
    }

    /// The 180°-opposite direction (`+4 mod 8`).
    pub const fn opposite(self) -> Self {
        Self::from_step(self.step() + 4)
    }

    /// Rotate one eighth-turn clockwise (`N → NE → E → …`).
    pub const fn rotate_cw(self) -> Self {
        Self::from_step(self.step() + 1)
    }

    /// Rotate one eighth-turn counter-clockwise (`N → NW → W → …`). `+7 mod 8`
    /// to keep the arithmetic in `u8` without an underflow.
    pub const fn rotate_ccw(self) -> Self {
        Self::from_step(self.step() + 7)
    }

    /// `true` for the four cardinal directions (`N`/`E`/`S`/`W`); `false` for
    /// the diagonals. (Cardinals are the even clockwise indices.)
    pub const fn is_cardinal(self) -> bool {
        self.step().is_multiple_of(2)
    }
}

/// The nearest-of-eight direction pointing from `a` toward `b`, or `None` when
/// `a == b` (no direction is meaningful). Uses the sign of each axis delta to
/// pick the octant: a non-zero `d_col` contributes `E`/`W`, a non-zero `d_row`
/// contributes `S`/`N`, and both non-zero yields the corresponding diagonal.
///
/// This is the exact-octant case. A magnitude-aware "snap an arbitrary vector
/// to the nearest of 8" lives with the resolver's 2D geometry (R1
/// `direction_to`); A2 only needs the grid-step octant, which the signs give
/// directly.
pub fn from_to(a: Pos, b: Pos) -> Option<Dir8> {
    let dc = (b.col as i32) - (a.col as i32);
    let dr = (b.row as i32) - (a.row as i32);
    let sc = dc.signum();
    let sr = dr.signum();
    Some(match (sc, sr) {
        (0, 0) => return None,
        (0, -1) => Dir8::N,
        (1, -1) => Dir8::NE,
        (1, 0) => Dir8::E,
        (1, 1) => Dir8::SE,
        (0, 1) => Dir8::S,
        (-1, 1) => Dir8::SW,
        (-1, 0) => Dir8::W,
        (-1, -1) => Dir8::NW,
        // signum only ever yields -1/0/1, so the match is exhaustive in
        // practice; this arm satisfies the type checker.
        _ => unreachable!("signum yields only -1, 0, 1"),
    })
}

/// Step `dist` cells from `pos` along `dir`, or `None` if the destination
/// leaves the grid (including underflow past `col`/`row` 0). `dist` is `i32`
/// so callers can pass a negative to step backward without flipping the
/// direction; the bounds check covers both ends.
pub const fn offset(pos: Pos, dir: Dir8, dist: i32) -> Option<Pos> {
    let (dc, dr) = dir.delta();
    let col = (pos.col as i32) + dc * dist;
    let row = (pos.row as i32) + dr * dist;
    if col < 0 || row < 0 || col >= COLS as i32 || row >= ROWS as i32 {
        return None;
    }
    Some(Pos {
        col: col as usize,
        row: row as usize,
    })
}

/// The in-bounds 8-neighbours of `pos`, in clockwise [`Dir8::ALL`] order.
/// Length 3 (corner), 5 (edge), or 8 (interior). Off-grid steps are dropped,
/// so the result is always a valid set of board cells.
pub fn neighbors(pos: Pos) -> Vec<Pos> {
    Dir8::ALL
        .iter()
        .filter_map(|&d| offset(pos, d, 1))
        .collect()
}

/* =========================================================================
 * Facing — the two 4-cardinal stances (blueprint decision #9)
 * ====================================================================== */

/// A cardinal direction (no diagonals). The 4-way restriction the v2 facing
/// model keeps (decision #9): the bow can only point at a cardinal, and a
/// broadside hull lies along a cardinal axis. Kept separate from [`Dir8`] so a
/// `Facing` cannot be constructed pointing at a diagonal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dir4 {
    /// Toward `row 0` (away from the player).
    N,
    /// Toward higher `col`.
    E,
    /// Toward the player (higher `row`).
    S,
    /// Toward lower `col`.
    W,
}

impl Dir4 {
    /// All four cardinals, clockwise from `N`.
    pub const ALL: [Self; 4] = [Self::N, Self::E, Self::S, Self::W];

    /// Widen to the matching [`Dir8`] cardinal.
    pub const fn to_dir8(self) -> Dir8 {
        match self {
            Self::N => Dir8::N,
            Self::E => Dir8::E,
            Self::S => Dir8::S,
            Self::W => Dir8::W,
        }
    }

    /// Narrow a [`Dir8`] to a [`Dir4`], or `None` if it is a diagonal.
    pub const fn from_dir8(dir: Dir8) -> Option<Self> {
        match dir {
            Dir8::N => Some(Self::N),
            Dir8::E => Some(Self::E),
            Dir8::S => Some(Self::S),
            Dir8::W => Some(Self::W),
            _ => None,
        }
    }

    /// The 180°-opposite cardinal.
    pub const fn opposite(self) -> Self {
        match self {
            Self::N => Self::S,
            Self::E => Self::W,
            Self::S => Self::N,
            Self::W => Self::E,
        }
    }

    /// Rotate one quarter-turn **clockwise** (`N → E → S → W → N`). [`Dir4::ALL`]
    /// is ordered clockwise from `N`, so this is `+1 (mod 4)`. The renderer's
    /// rotate-RIGHT control turns the player's bow this way (toward higher `col`
    /// when starting from `N`).
    pub const fn rotate_cw(self) -> Self {
        match self {
            Self::N => Self::E,
            Self::E => Self::S,
            Self::S => Self::W,
            Self::W => Self::N,
        }
    }

    /// Rotate one quarter-turn **counter-clockwise** (`N → W → S → E → N`), i.e.
    /// `−1 (mod 4)`. The renderer's rotate-LEFT control turns the player's bow
    /// this way.
    pub const fn rotate_ccw(self) -> Self {
        match self {
            Self::N => Self::W,
            Self::W => Self::S,
            Self::S => Self::E,
            Self::E => Self::N,
        }
    }

    /// The axis this cardinal lies on.
    pub const fn axis(self) -> Axis {
        match self {
            Self::N | Self::S => Axis::NorthSouth,
            Self::E | Self::W => Axis::EastWest,
        }
    }
}

/// One of the two grid axes a [`Facing::Broadside`] hull can lie along. A
/// broadside stance is axis-only (not a single direction) because both flanks
/// face outward symmetrically — the hull along `EastWest` presents Port/
/// Starboard to the N/S sectors, and vice versa. The resolver's `facing_zone`
/// table (R2) consumes this; it is recorded here so the stance type is total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    /// The vertical axis (`col` fixed, hull runs `N`↔`S`).
    NorthSouth,
    /// The horizontal axis (`row` fixed, hull runs `E`↔`W`).
    EastWest,
}

impl Axis {
    /// The two cardinals lying on this axis, as `(positive, negative)` where
    /// "positive" is the increasing-coordinate direction (`S` for `NorthSouth`
    /// since `+row` is toward the player; `E` for `EastWest`).
    pub const fn dirs(self) -> (Dir4, Dir4) {
        match self {
            Self::NorthSouth => (Dir4::S, Dir4::N),
            Self::EastWest => (Dir4::E, Dir4::W),
        }
    }
}

/// A ship's hull orientation in v2. Two stances at 4 cardinals (decision #9,
/// preserving `ClassAffinity` + the `REORIENT` action):
///
/// - `Bow(dir)` — nose pointed at cardinal `dir`; the strong bow face takes
///   hits from that side, the weak stern from the opposite.
/// - `Broadside(axis)` — hull turned across the grid along `axis`; both flanks
///   present outward.
///
/// This is the v2 replacement for [`crate::types::Orientation`]
/// (`BowOn { bow: LaneEnd } | Broadside`); the field swap on `Ship` happens in
/// the atomic A3 commit, not here. 8-way facing is deferred (decision #9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "stance", rename_all = "camelCase")]
pub enum Facing {
    Bow(Dir4),
    Broadside(Axis),
}

impl Facing {
    /// The forward axis the renderer's bow-arrow must encode (blueprint:
    /// "the renderer's bow-arrow MUST encode the SAME forward axis"). For a
    /// `Bow` stance it is the bow direction's axis; for a `Broadside` stance it
    /// is the hull's axis.
    pub const fn forward_axis(self) -> Axis {
        match self {
            Self::Bow(dir) => dir.axis(),
            Self::Broadside(axis) => axis,
        }
    }
}

/* =========================================================================
 * Range — 3-band Chebyshev distance (blueprint decision #6)
 * ====================================================================== */

/// The 3-band range bucket (blueprint decision #6), replacing the 1-D 5-band
/// [`crate::types::RangeBand`]. Bands are cut by Chebyshev (chessboard)
/// distance: `Adjacent` = 1, `Near` = 2, `Far` = 3+. (Distance 0 — same cell —
/// also reads `Adjacent`; a ship is never at range 0 from a distinct target.)
///
/// The per-band damage falloff `[1.0, 0.6, 0.3]` (decision #6) is the
/// resolver's to apply (R1 `band_falloff`); this enum only names the buckets so
/// the falloff table and the over-extension deadzone (decision #7) have a
/// shared vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Range {
    Adjacent,
    Near,
    Far,
}

/// Chebyshev (chessboard) distance between two cells: `max(|d_col|, |d_row|)`.
/// This is the metric a diagonal step costs 1, matching [`Dir8`] movement
/// where a diagonal advances one cell on both axes at once.
pub fn distance(a: Pos, b: Pos) -> usize {
    let dc = (a.col as i32 - b.col as i32).unsigned_abs() as usize;
    let dr = (a.row as i32 - b.row as i32).unsigned_abs() as usize;
    dc.max(dr)
}

/// Bucket the [`distance`] between two cells into a [`Range`] band: 0–1 →
/// `Adjacent`, 2 → `Near`, 3+ → `Far`.
pub fn range_band(a: Pos, b: Pos) -> Range {
    match distance(a, b) {
        0 | 1 => Range::Adjacent,
        2 => Range::Near,
        _ => Range::Far,
    }
}

/* =========================================================================
 * Tests — light sanity (heavy coverage is the tester's lane, T1)
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_index_roundtrips_for_every_cell() {
        for i in 0..CELLS {
            let p = Pos::from_index(i).expect("index < CELLS is Some");
            assert_eq!(p.to_index(), i);
            assert!(p.in_bounds());
        }
        assert_eq!(Pos::from_index(CELLS), None, "out-of-range index is None");
    }

    #[test]
    fn index_is_row_major() {
        // row 0 fills first, then row 1, …; col is the fast axis.
        assert_eq!(Pos::new(0, 0).to_index(), 0);
        assert_eq!(Pos::new(COLS - 1, 0).to_index(), COLS - 1);
        assert_eq!(Pos::new(0, 1).to_index(), COLS);
    }

    #[test]
    fn dir8_step_roundtrips_and_opposite_is_plus_four() {
        for d in Dir8::ALL {
            assert_eq!(Dir8::from_step(d.step()), d);
            assert_eq!(d.opposite(), Dir8::from_step(d.step() + 4));
            assert_eq!(d.opposite().opposite(), d);
            // A full 8-step clockwise rotation returns to start.
            let mut r = d;
            for _ in 0..8 {
                r = r.rotate_cw();
            }
            assert_eq!(r, d);
        }
    }

    #[test]
    fn rotate_cw_and_ccw_are_inverses() {
        for d in Dir8::ALL {
            assert_eq!(d.rotate_cw().rotate_ccw(), d);
            assert_eq!(d.rotate_ccw().rotate_cw(), d);
        }
    }

    #[test]
    fn dir4_rotate_cycles_cardinals_and_inverts() {
        // CW order N→E→S→W→N (the player rotate-RIGHT control).
        assert_eq!(Dir4::N.rotate_cw(), Dir4::E);
        assert_eq!(Dir4::E.rotate_cw(), Dir4::S);
        assert_eq!(Dir4::S.rotate_cw(), Dir4::W);
        assert_eq!(Dir4::W.rotate_cw(), Dir4::N);
        for d in Dir4::ALL {
            // CW and CCW are inverses.
            assert_eq!(d.rotate_cw().rotate_ccw(), d);
            assert_eq!(d.rotate_ccw().rotate_cw(), d);
            // Two quarter-turns == opposite; four == identity.
            assert_eq!(d.rotate_cw().rotate_cw(), d.opposite());
            let mut r = d;
            for _ in 0..4 {
                r = r.rotate_cw();
            }
            assert_eq!(r, d);
        }
    }

    #[test]
    fn delta_and_opposite_agree() {
        for d in Dir8::ALL {
            let (dc, dr) = d.delta();
            let (oc, or) = d.opposite().delta();
            assert_eq!((dc + oc, dr + or), (0, 0), "{d:?} + opposite cancels");
        }
    }

    #[test]
    fn from_to_is_none_only_for_same_cell() {
        let c = Pos::new(2, 2);
        assert_eq!(from_to(c, c), None);
        // The eight grid-step octants resolve to the matching Dir8.
        assert_eq!(from_to(c, Pos::new(2, 1)), Some(Dir8::N));
        assert_eq!(from_to(c, Pos::new(3, 1)), Some(Dir8::NE));
        assert_eq!(from_to(c, Pos::new(3, 2)), Some(Dir8::E));
        assert_eq!(from_to(c, Pos::new(3, 3)), Some(Dir8::SE));
        assert_eq!(from_to(c, Pos::new(2, 3)), Some(Dir8::S));
        assert_eq!(from_to(c, Pos::new(1, 3)), Some(Dir8::SW));
        assert_eq!(from_to(c, Pos::new(1, 2)), Some(Dir8::W));
        assert_eq!(from_to(c, Pos::new(1, 1)), Some(Dir8::NW));
    }

    #[test]
    fn from_to_then_offset_steps_one_cell_toward_target() {
        // For an exact octant, stepping 1 along from_to moves one cell closer.
        let a = Pos::new(0, 0);
        let b = Pos::new(4, 3); // pure SE octant
        let d = from_to(a, b).unwrap();
        assert_eq!(d, Dir8::SE);
        assert_eq!(offset(a, d, 1), Some(Pos::new(1, 1)));
    }

    #[test]
    fn offset_bounds_check_covers_both_ends() {
        // Off the near edge (underflow past 0).
        assert_eq!(offset(Pos::new(0, 0), Dir8::W, 1), None);
        assert_eq!(offset(Pos::new(0, 0), Dir8::N, 1), None);
        // Off the far edge.
        assert_eq!(offset(Pos::new(COLS - 1, ROWS - 1), Dir8::E, 1), None);
        assert_eq!(offset(Pos::new(COLS - 1, ROWS - 1), Dir8::S, 1), None);
        // Interior step stays on-grid.
        assert_eq!(offset(Pos::new(1, 1), Dir8::SE, 1), Some(Pos::new(2, 2)));
        // Negative distance steps backward (and is bounds-checked).
        assert_eq!(offset(Pos::new(1, 1), Dir8::SE, -1), Some(Pos::new(0, 0)));
    }

    #[test]
    fn neighbors_count_by_position() {
        assert_eq!(neighbors(Pos::new(0, 0)).len(), 3, "corner");
        assert_eq!(neighbors(Pos::new(1, 0)).len(), 5, "edge");
        assert_eq!(neighbors(Pos::new(1, 1)).len(), 8, "interior");
        // Every neighbour is in bounds and exactly one Chebyshev step away.
        for n in neighbors(Pos::new(1, 1)) {
            assert!(n.in_bounds());
            assert_eq!(distance(Pos::new(1, 1), n), 1);
        }
    }

    #[test]
    fn distance_is_chebyshev() {
        // Diagonal of 3 costs 3 (max axis), not 6 (Manhattan) or ~4.2 (Euclid).
        assert_eq!(distance(Pos::new(0, 0), Pos::new(3, 3)), 3);
        // A pure-lateral gap is the column delta.
        assert_eq!(distance(Pos::new(0, 2), Pos::new(4, 2)), 4);
        // Symmetric.
        let a = Pos::new(1, 0);
        let b = Pos::new(4, 3);
        assert_eq!(distance(a, b), distance(b, a));
        // Identity.
        assert_eq!(distance(a, a), 0);
    }

    #[test]
    fn range_band_cuts_match_decision_6() {
        let o = Pos::new(0, 0);
        assert_eq!(range_band(o, o), Range::Adjacent); // dist 0
        assert_eq!(range_band(o, Pos::new(1, 1)), Range::Adjacent); // dist 1 (diag)
        assert_eq!(range_band(o, Pos::new(0, 1)), Range::Adjacent); // dist 1
        assert_eq!(range_band(o, Pos::new(2, 0)), Range::Near); // dist 2
        assert_eq!(range_band(o, Pos::new(2, 2)), Range::Near); // dist 2 (diag)
        assert_eq!(range_band(o, Pos::new(3, 0)), Range::Far); // dist 3
        assert_eq!(range_band(o, Pos::new(4, 3)), Range::Far); // dist 4
    }

    #[test]
    fn dir4_dir8_narrow_widen_roundtrip() {
        for d in Dir4::ALL {
            assert_eq!(Dir4::from_dir8(d.to_dir8()), Some(d));
            assert!(d.to_dir8().is_cardinal());
        }
        // Diagonals do not narrow.
        for diag in [Dir8::NE, Dir8::SE, Dir8::SW, Dir8::NW] {
            assert_eq!(Dir4::from_dir8(diag), None);
            assert!(!diag.is_cardinal());
        }
    }

    #[test]
    fn facing_serde_roundtrips_via_stance_tag() {
        // Bow stance carries a cardinal direction.
        let bow = Facing::Bow(Dir4::N);
        let s = serde_json::to_string(&bow).unwrap();
        let back: Facing = serde_json::from_str(&s).unwrap();
        assert_eq!(back, bow);
        assert_eq!(bow.forward_axis(), Axis::NorthSouth);

        // Broadside stance carries an axis.
        let bs = Facing::Broadside(Axis::EastWest);
        let s2 = serde_json::to_string(&bs).unwrap();
        let back2: Facing = serde_json::from_str(&s2).unwrap();
        assert_eq!(back2, bs);
        assert_eq!(bs.forward_axis(), Axis::EastWest);
    }

    #[test]
    fn pos_and_dir8_serde_roundtrip() {
        let p = Pos::new(3, 2);
        let pj = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Pos>(&pj).unwrap(), p);

        for d in Dir8::ALL {
            let dj = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<Dir8>(&dj).unwrap(), d);
        }
    }

    #[test]
    fn axis_dirs_lie_on_the_axis() {
        for a in [Axis::NorthSouth, Axis::EastWest] {
            let (pos, neg) = a.dirs();
            assert_eq!(pos.axis(), a);
            assert_eq!(neg.axis(), a);
            assert_eq!(pos.opposite(), neg);
        }
    }

    #[test]
    fn all_positions_is_every_cell_in_index_order() {
        let all = all_positions();
        assert_eq!(all.len(), CELLS);
        for (i, p) in all.into_iter().enumerate() {
            assert_eq!(p.to_index(), i);
        }
    }
}
