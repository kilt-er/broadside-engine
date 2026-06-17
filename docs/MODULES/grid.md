# `src/grid.rs` — Module Companion

*A self-contained walkthrough of the v2 2D spatial type surface. Read this if you are about
to touch positions, directions, facings, or range bands on the new 5×4 grid — or if you need
to understand how the v2 spatial layer replaces the 1-D lane.*

**Blueprint task:** A2 (lands first, standalone, ahead of the atomic type-surface commit A3).
**Design anchor:** [`docs/design/BROADSIDE_V2_BLUEPRINT.md`](../design/BROADSIDE_V2_BLUEPRINT.md)
— decisions **#2** (5×4 grid), **#6** (3-band Chebyshev range), **#8** (rows = dodge space),
**#9** (two stances at 4 cardinals).
**Replaces:** the 1-D spatial vocabulary in [`types.rs`](types.md) (`LaneEnd`, `Orientation`,
`RangeBand`) and [`geometry.rs`](geometry.md) (`distance`, `range_band`).

---

## Why this file exists

The Broadside v2 redesign is a **surgical amputation**, not a rewrite. The engine is already
two engines welded together: a **WHAT/WHEN** engine that is geometry-free (the four-phase
round, the damage-pipeline order, the heat/cooldown economy, the EventBus and Content seams)
and a **WHERE** engine that is 100% one-dimensional. The v2 plan keeps the first verbatim and
**replaces only the second**.

`grid.rs` is the new WHERE vocabulary. It is the single place that knows the battlefield is a
**5-column × 4-row grid** instead of a 1-D lane. It is pure, dimension-aware data plus small
helpers — no `Board`, no resolver, no rendering. It lands first and standalone (additive, so
the crate keeps compiling) ahead of the one atomic commit (A3) that flips `Ship.cell:
usize → Pos` and every other spatial field at once.

Names here (`distance`, `range_band`, `offset`, …) deliberately mirror the 1-D vocabulary but
live behind the `grid::` path and take `Pos`/`Dir8`, so they don't collide with the still-live
`usize`-based `geometry::distance` / `geometry::range_band` during the half-migrated window.

If you change a type here, expect to touch the A3 type commit and everything downstream of it
(the resolver's geometry, the targeting templates, the AI, the renderer's projector). If you
find yourself reaching for board state, damage numbers, or screen coordinates, you are in the
wrong file.

---

## The coordinate frame (read this first)

Everything in the module hangs off one convention:

- **`col`** increases **left → right**, `0..COLS`. This is the lateral **dodge axis**.
- **`row`** increases **toward the player**. `row 0` is the far/back row (where enemies
  spawn); `row ROWS-1` is the front row (nearest the camera). The renderer's per-row depth
  scale grows with `row`.
- **`Dir8::N`** points toward `row 0` (**away** from the player, decreasing `row`).
  **`Dir8::S`** points toward the player (increasing `row`). **`E`** increases `col`,
  **`W`** decreases `col`.

This is "screen-down is +row," and it is chosen to match the flat board vector's index order
(`row * COLS + col`). The combat model treats **all four rows as pure dodge space**
(decision #8) — moving back a row is a dodge, never required for progress; a level clears by
eliminating its enemies, not by reaching a row. The module fixes only the *numbering* so the
renderer's projector and the AI agree on which way is "toward the player."

---

## The public surface

| Item                              | Kind        | One-line role                                            |
|-----------------------------------|-------------|----------------------------------------------------------|
| `COLS` / `ROWS` / `CELLS`         | const       | 5 / 4 / 20 — grid dimensions and flat-vector length.     |
| `Pos { col, row }`                | struct      | A cell coordinate. The v2 replacement for `cell: usize`. |
| `Pos::new` / `in_bounds`          | fn          | Construct (unchecked) / bounds predicate.                |
| `Pos::to_index` / `from_index`    | fn          | Row-major flat-index ↔ `Pos` (`from_index` is checked).  |
| `all_positions()`                 | fn          | Every in-bounds `Pos` in flat row-major order.           |
| `Dir8`                            | enum        | Eight-way direction (4 cardinals + 4 diagonals).         |
| `Dir8::ALL` / `step` / `from_step`| const/fn    | Clockwise order and the index ↔ direction bijection.     |
| `Dir8::delta`                     | fn          | The `(d_col, d_row)` unit step.                          |
| `Dir8::opposite` / `rotate_cw` / `rotate_ccw` | fn | 180° flip and ±⅛-turn rotation.                    |
| `Dir8::is_cardinal`               | fn          | `true` for `N`/`E`/`S`/`W`.                              |
| `from_to(a, b)`                   | fn          | Nearest-of-eight grid-step direction `a → b`, or `None`. |
| `offset(pos, dir, dist)`          | fn          | Step `dist` cells along `dir`, or `None` if off-grid.    |
| `neighbors(pos)`                  | fn          | In-bounds 8-neighbours (length 3 / 5 / 8).               |
| `Dir4`                            | enum        | A cardinal-only direction (no diagonals).                |
| `Dir4::to_dir8` / `from_dir8`     | fn          | Widen to `Dir8` / narrow (diagonal → `None`).            |
| `Dir4::opposite` / `axis`         | fn          | 180° cardinal flip / which `Axis` it lies on.            |
| `Axis { NorthSouth, EastWest }`   | enum        | The two grid axes a broadside hull can lie along.        |
| `Axis::dirs`                      | fn          | The two cardinals on the axis, `(positive, negative)`.   |
| `Facing { Bow(Dir4), Broadside(Axis) }` | enum  | A ship's hull orientation. Replaces `Orientation`.       |
| `Facing::forward_axis`            | fn          | The forward axis the renderer's bow-arrow must encode.   |
| `Range { Adjacent, Near, Far }`   | enum        | The 3-band range bucket. Replaces `RangeBand`.           |
| `distance(a, b)`                  | fn          | Chebyshev (chessboard) distance.                         |
| `range_band(a, b)`                | fn          | Bucket `distance` into a `Range`.                        |

---

## How it all fits

`grid.rs` sits at the **bottom** of the v2 dependency graph — everything spatial depends on
it, it depends on nothing but `serde`. The arrows point *into* this file:

```
   A3 atomic type commit          R1 geometry.rs (2D)        D2 renderer projector
   (Ship.cell: Pos,               in_band, band_falloff,     grid_cell_quad(Pos),
    orientation: Facing,          direction_to (snap),       per-row depth_scale
    Board reshape)                facing_zone (R2)
        │                              │                          │
        └──────────────┬───────────────┴──────────────┬──────────┘
                       ▼                                ▼
                  ┌─────────────────────────────────────────┐
                  │              src/grid.rs                 │
                  │   Pos · Dir8 · Dir4 · Axis · Facing       │
                  │   Range · distance · range_band · helpers │
                  └─────────────────────────────────────────┘
                       │
                       ▼
                    serde   (only dependency)
```

Two seam decisions are worth internalizing, because they define where `grid.rs` stops:

**1. `range_band` (here) vs `band_falloff` + `in_band` (resolver).** `grid.rs` answers *"what
band is this distance?"* — a pure classifier. It does **not** own the damage **policy**: the
`[1.0, 0.6, 0.3]` per-band falloff table and the allowed-bands predicate live in the
resolver's `geometry.rs` (R1), exactly as the 1-D code splits them today. The falloff numbers
are a playtest-tuned balance lever (decision #6); keeping them out of the dimension vocabulary
is deliberate layering.

**2. `from_to` (here, exact octant) vs `direction_to` (resolver, magnitude-aware snap).**
`from_to` only resolves the **grid-step** octant — the eight `(sign(d_col), sign(d_row))`
cases. Snapping an *arbitrary* vector to the nearest of eight (needed when the resolver's
`facing_zone` decides which hull face a diagonal hit lands on) is the resolver's R1 job. A2
only needs the grid-step octant, and the signs give it directly.

The single contract `grid.rs` shares *upward* with both the resolver and the renderer is
**`Facing::forward_axis`**: the blueprint requires the renderer's bow-arrow to encode the same
forward axis the resolver's `facing_zone` reasons about. Putting it on the type keeps the two
slices from drifting.

---

## Function reference

### `Pos::to_index(self) -> usize`
**Line:** `grid.rs:87`. Row-major flat index `row * COLS + col` into the length-`CELLS`
`Vec<Option<Ship>>`. The board is scanned in exactly this order today (`types.rs:136`), so the
`cell: usize → Pos` swap in A3 leaves the scan call sites untouched. Unchecked — an
out-of-range `Pos` yields an out-of-range index that panics at the later `Vec` access, which
matches the existing `usize`-cell behavior. Use `from_index` for any index that came from
outside.

### `Pos::from_index(index) -> Option<Pos>`
**Line:** `grid.rs:95`. The bounds-checked inverse: `None` if `index >= CELLS`, else
`Pos { col: index % COLS, row: index / COLS }`. The checked-in / unchecked-out asymmetry is
intentional: invalid *external* indices become a handled `None` rather than a wrong-cell
silent bug.

### `all_positions() -> Vec<Pos>`
**Line:** `grid.rs:106`. Every in-bounds `Pos` in flat row-major order (the same order as
`(0..CELLS).map(|i| Pos::from_index(i).unwrap())`). For board scans and tests. Allocates a
20-element `Vec`.

### `Dir8::step(self) -> u8` / `Dir8::from_step(step: u8) -> Dir8`
**Lines:** `grid.rs:152`, `:167`. The clockwise index bijection (`N`=0 … `NW`=7), the single
source of truth for all rotation/opposite arithmetic. `from_step` takes its argument `mod 8`,
so the arithmetic **never panics**.

### `Dir8::delta(self) -> (i32, i32)`
**Line:** `grid.rs:182`. The unit `(d_col, d_row)` step. `+row` is toward the player. Used by
`offset` and pinned against `opposite` by the cancellation test.

### `Dir8::opposite` / `rotate_cw` / `rotate_ccw`
**Lines:** `grid.rs:196`, `:201`, `:207`. `opposite` = `+4 mod 8`; `rotate_cw` = `+1`;
`rotate_ccw` = `+7` (= `−1 mod 8` without `u8` underflow).

### `Dir8::is_cardinal(self) -> bool`
**Line:** `grid.rs:213`. `true` for `N`/`E`/`S`/`W` (the even clockwise indices). Lets the
resolver's `facing_zone` separate cardinal incoming directions from diagonals.

### `from_to(a: Pos, b: Pos) -> Option<Dir8>`
**Line:** `grid.rs:227`. The nearest-of-eight **grid-step** direction from `a` toward `b`, by
the sign of each axis delta. `None` iff `a == b` (no direction is meaningful). For an exact
octant, `offset(a, from_to(a,b)?, 1)` steps one cell closer to `b`. The magnitude-aware
"snap an arbitrary vector to the nearest of 8" is the resolver's job, not this.

### `offset(pos: Pos, dir: Dir8, dist: i32) -> Option<Pos>`
**Line:** `grid.rs:252`. Step `dist` cells along `dir`, or `None` if the destination leaves the
grid (including underflow past `col`/`row` 0). `dist` is `i32` so a caller can pass a negative
to step backward; the bounds check covers both ends.

### `neighbors(pos: Pos) -> Vec<Pos>`
**Line:** `grid.rs:265`. The in-bounds 8-neighbours in clockwise `Dir8::ALL` order. Length 3
(corner), 5 (edge), or 8 (interior) — off-grid steps are dropped, so the result is always a
valid set of board cells.

### `Dir4` and `Dir4::to_dir8` / `from_dir8` / `opposite` / `axis`
**Lines:** `grid.rs:282`–`335`. A cardinal-only direction, kept **separate from `Dir8` so a
`Facing` cannot be constructed pointing at a diagonal** — the type system enforces the
4-cardinal rule (decision #9). `from_dir8` returns `None` for the four diagonals. `axis` maps
`N`/`S → NorthSouth`, `E`/`W → EastWest`. `ALL` is `[N, E, S, W]` — the four cardinals in
**clockwise** order from `N`, which is what makes the two rotation helpers below pure index
arithmetic.

### `Dir4::rotate_cw` / `rotate_ccw` (#75 — the player rotation primitive)
**Lines:** `grid.rs:332`, `:344`. A quarter-turn of the bow direction: `rotate_cw` steps
`N → E → S → W → N` (`+1 mod 4`); `rotate_ccw` steps `N → W → S → E → N` (`−1 mod 4`). They
mirror `Dir8::rotate_cw`/`rotate_ccw` but on the 4-cardinal alphabet. These are the geometric
core of the **player rotation control** (`Q` = rotate-left/ccw, `E` = rotate-right/cw, `Tab` =
180° = two cw turns): the resolver's `REORIENT::RotateLeft`/`RotateRight` arm turns the
player's `facing` by calling these, and because *both* the loft render and the 2-D fire-gate
read `facing`, the hull visibly rotates and the firing arcs follow by construction. Pinned by
`rotate_cw_and_ccw_are_inverses`, the explicit `N→E→S→W` sequence test, and a
four-turns-round-trip in `grid.rs`'s test module. See the cross-module hook
["The rotation mechanic"](resolve.md) and [`resolve.md`](resolve.md)'s REORIENT-rotate arm.

### `Axis` and `Axis::dirs`
**Lines:** `grid.rs:344`, `:355`. The two grid axes a `Broadside` hull can lie along. A
broadside stance is **axis-only** (not a single direction) because both flanks face outward
symmetrically. `dirs()` returns `(positive, negative)` where "positive" is the
increasing-coordinate direction (`S` for `NorthSouth`, since `+row` is toward the player; `E`
for `EastWest`).

### `Facing { Bow(Dir4), Broadside(Axis) }` and `Facing::forward_axis`
**Lines:** `grid.rs:376`, `:386`. A ship's hull orientation, the v2 replacement for
`Orientation { BowOn{LaneEnd}, Broadside }`. `Bow(dir)` points the strong bow face at a
cardinal (the weak stern takes hits from the opposite); `Broadside(axis)` turns the hull
across the grid so both flanks present outward. `forward_axis()` — `Bow(dir) → dir.axis()`,
`Broadside(axis) → axis` — is the contract the renderer's bow-arrow must match (the blueprint
requires the arrow to encode the *same* forward axis the resolver reasons about).

### `distance(a: Pos, b: Pos) -> usize`
**Line:** `grid.rs:418`. Chebyshev (chessboard) distance `max(|d_col|, |d_row|)`. The metric
where a **diagonal step costs 1**, matching `Dir8` movement (a diagonal advances both axes at
once). Not Manhattan (would cost 2) or Euclidean (~1.41).

### `range_band(a: Pos, b: Pos) -> Range`
**Line:** `grid.rs:426`. Buckets `distance`: `0 | 1 → Adjacent`, `2 → Near`, `3+ → Far`
(decision #6). A pure classifier — the per-band damage falloff is the resolver's.

---

## Drift from the 1-D engine

`grid.rs` is **net-new** (no TS counterpart — the TS engine is 1-D), so this is *intended
divergence from the engine it replaces*, not a port mismatch. The shape changes the v2
redesign makes on purpose:

1. **`cell: usize` → `Pos { col, row }`.** A 1-D lane index becomes a 2-D coordinate. The
   flat board vector and its row-major index order are preserved, so the swap is a field-type
   change, not a board-access rewrite.
2. **`LaneEnd { Fore, Aft }` → `Dir8` + `Dir4`.** A two-valued lane direction becomes
   eight-way movement (`Dir8`) plus a four-cardinal facing alphabet (`Dir4`). The 1-D
   `opposite(LaneEnd)` generalizes to `Dir8::opposite` (`+4 mod 8`) and `Dir4::opposite`.
3. **`Orientation { BowOn{LaneEnd}, Broadside }` → `Facing { Bow(Dir4), Broadside(Axis) }`.**
   Same two stances (decision #9 keeps the `ClassAffinity` + `REORIENT` design), but the bow
   now points at one of four cardinals and a broadside hull lies along one of two axes. 8-way
   facing is explicitly **deferred**, and the `Dir4`/`Axis` types make the deferral
   unrepresentable-by-construction rather than a runtime check. serde keeps the same
   `tag = "stance"` discriminator the 1-D `Orientation` used.
4. **`RangeBand` (5 bands) → `Range` (3 bands).** The 1-D `pointBlank/close/mid/long/extreme`
   ruler collapses to `Adjacent/Near/Far` (decision #6), and the metric changes from 1-D
   `abs_diff` to 2-D **Chebyshev**.
5. **Seam: `range_band` here, `band_falloff` + `in_band` in the resolver.** This **matches**
   the 1-D split (`geometry.rs:66`/`:75` already own the falloff table and the allowed-bands
   predicate), so it is *continuity*, not drift — `grid.rs` is the dimension vocabulary, the
   resolver owns damage policy.

---

## Tests

16 inline tests in `#[cfg(test)] mod tests` (`grid.rs:438–629`), all passing. Coverage is
deliberately light here — heavy property/table coverage is the tester's **T1** lane. The
names read as sentences:

```
pos_index_roundtrips_for_every_cell
index_is_row_major
dir8_step_roundtrips_and_opposite_is_plus_four
rotate_cw_and_ccw_are_inverses
delta_and_opposite_agree
from_to_is_none_only_for_same_cell
from_to_then_offset_steps_one_cell_toward_target
offset_bounds_check_covers_both_ends
neighbors_count_by_position
distance_is_chebyshev
range_band_cuts_match_decision_6
dir4_dir8_narrow_widen_roundtrip
facing_serde_roundtrips_via_stance_tag
pos_and_dir8_serde_roundtrip
axis_dirs_lie_on_the_axis
all_positions_is_every_cell_in_index_order
```

When the tester's `tests/grid.rs` (T1) lands, this file should reference it as the "see also"
for property-level coverage.

---

## Cross-references

- **Replaces:** the 1-D spatial vocabulary in [`types.rs`](types.md) (`LaneEnd`,
  `Orientation`, `RangeBand`) and [`geometry.rs`](geometry.md) (`distance`, `range_band`).
  Those still describe the **live** code until the A3 atomic commit lands.
- **Consumers (after A3):** the resolver's rewritten `geometry.rs` (R1 — `in_band`,
  `band_falloff`, `direction_to`, `facing_zone`), the 8 targeting templates (R3), the 2D AI
  (C1), and the renderer's 5×4 projector (D2 `grid_cell_quad`).
- **Review record:** [`docs/reviews/grid.md`](../reviews/grid.md) (the V1 audit that gated A3).
- **Design intent:** [`BROADSIDE_V2_BLUEPRINT.md`](../design/BROADSIDE_V2_BLUEPRINT.md),
  decisions #2 / #6 / #8 / #9 and the "Decisive fact" + "Reuse vs replace" sections.
