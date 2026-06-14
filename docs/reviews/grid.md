# Review: src/grid.rs — v2 2D spatial type surface

Reviewer audit (blueprint lane task **V1**, gating **A3** the atomic type-surface commit).
Canonical references: `docs/design/BROADSIDE_V2_BLUEPRINT.md` (locked decisions),
the 1-D `src/types.rs` / `src/geometry.rs` this layer replaces, and the TS
`_drive_pull/broadside-engine/engine/geometry.ts` whose vocabulary it ports to 2D.

Status: **APPROVE — complete and correct, no required changes.** Two optional, non-blocking
nits at the end. Crate compiles; all 16 inline grid tests pass.

This review happened *before* A3 locks the surface — the cheapest moment to change it.
Grep confirms no other module references `grid::` yet, so naming was unconstrained.

---

## What grid.rs is

The v2 redesign keeps the dimension-free engine spine (four-phase round, damage-pipeline
order, heat/cooldown economy, EventBus/Content seams) and **replaces only the spatial
layer**. `grid.rs` is the new **WHERE vocabulary** that swap is built on, landed first as
a standalone additive module (blueprint A2):

| 1-D today (`types.rs` / `geometry.rs`) | v2 (`grid.rs`)                          |
|----------------------------------------|-----------------------------------------|
| `cell: usize`                          | `Pos { col, row }`                      |
| `LaneEnd { Fore, Aft }`                | `Dir8` (8-way) + `Dir4` (4 cardinals)   |
| `Orientation { BowOn{LaneEnd}, Broadside }` | `Facing { Bow(Dir4), Broadside(Axis) }` |
| `RangeBand` (5-band)                   | `Range { Adjacent, Near, Far }` (3-band)|
| `distance(a,b) = abs_diff` (1-D)       | `distance(a,b) = Chebyshev` (2-D)       |

Nothing here migrates the live `Board`/`Ship` yet — that is the single atomic A3 commit.
`grid.rs` is pure data + helpers: no board, no resolver, no rendering.

---

## Verified correct

### 1. The coordinate frame (row-major, row-toward-camera)

- **`Pos::to_index = row * COLS + col`** (`grid.rs:87`) is **row-major with `col` as the
  fast axis**. This is exactly the order the live board is already scanned today: the flat
  `Vec<Option<Ship>>` (`types.rs:136`), the faction scan
  (`cells.iter().find_map(...)`), and find-by-id (`cells.iter().position(...)`). So A3's
  `cell: usize → Pos` change is a **field-type swap only** — the existing flat-vector access
  patterns carry over unchanged. Confirmed against the actual `types.rs` source, not just
  the module's own claim.
- **`from_index`** (`grid.rs:95`) is bounds-checked → `Option` (use for any index from
  outside: deserialized data, loop bounds). **`to_index` / `new`** are unchecked by design;
  an out-of-range `Pos` yields an out-of-range index that panics at the later `Vec` access —
  which **matches today's `usize`-cell behavior** exactly. This asymmetry (checked-in,
  unchecked-out) is the right call and is documented at the call sites.
- **`row` increases toward the player** (`row 0` = far/back where enemies spawn,
  `row ROWS-1` = front nearest the camera). `Dir8::N` decreases `row` (away from player),
  `Dir8::S` increases `row` (toward player), `E`/`W` move `col`. This screen-down-is-+row
  convention is **internally consistent** across `Dir8::delta` (`grid.rs:182`), `from_to`
  (`:227`), `offset` (`:252`), and the `Dir4` doc comments (`:282`). It matches blueprint
  **decision #8** (rows = pure dodge space) and the renderer's D2 contract ("per-row depth
  scale grows with `row`").

### 2. Dir8 — eight-way direction

Clockwise-ordered from `N` so the rotation arithmetic is trivial and total:

- `step()` / `from_step()` (`grid.rs:152`, `:167`) are inverse; `from_step` takes its
  argument `mod 8`, so rotation math **never panics**.
- `opposite()` = `+4 mod 8`; `rotate_cw` = `+1`; `rotate_ccw` = `+7` (`= -1 mod 8` without
  `u8` underflow). All cross-checked by tests (`dir8_step_roundtrips_and_opposite_is_plus_four`,
  `rotate_cw_and_ccw_are_inverses`, `delta_and_opposite_agree`).
- `delta()` unit steps are correct for the frame and the opposite-cancels test pins it.
- `is_cardinal()` = even `step()`.

This is everything the resolver's **R2 `facing_zone` 2D quadrant table** needs to compute
"incoming within ±45° of the bow" (step arithmetic) and the Port/Starboard left/right
tiebreak (sign of `(step(incoming) − step(bow)) mod 8`). Surface is sufficient for R2.

### 3. Facing — the two 4-cardinal stances (decision #9)

- `Dir4` (`:282`) is a separate type from `Dir8` precisely so a `Facing` **cannot be
  constructed pointing at a diagonal** — the type system enforces decision #9 ("keep two
  stances at 4 cardinals; 8-way deferred"). `to_dir8` / `from_dir8` widen/narrow with the
  diagonal→`None` case, roundtrip-tested.
- `Axis { NorthSouth, EastWest }` (`:344`) models a `Broadside` hull as **axis-only** (both
  flanks face outward symmetrically), which is the correct shape — a broadside stance is not
  a single direction. `Axis::dirs()` returns `(positive, negative)` and the
  `axis_dirs_lie_on_the_axis` test pins the convention.
- `Facing { Bow(Dir4), Broadside(Axis) }` (`:376`) cleanly replaces the 1-D
  `Orientation { BowOn{LaneEnd}, Broadside }` (`types.rs:76`). serde is
  `#[serde(tag = "stance", rename_all = "camelCase")]` — the **same tag discriminator** the
  1-D `Orientation` uses, so the wire shape stays familiar (`{"stance":"bow","dir":"n"}`).
- **`forward_axis()`** (`:386`) satisfies the blueprint's hard requirement that "the
  renderer's bow-arrow MUST encode the SAME forward axis." `Bow(dir) → dir.axis()`,
  `Broadside(axis) → axis`. This is the single contract the renderer (D4) and resolver (R2)
  share for orientation; good that it lives on the type.

### 4. Range — 3-band Chebyshev (decision #6)

- **`distance`** (`:418`) is Chebyshev `max(|d_col|, |d_row|)` — the metric where a diagonal
  step costs 1, matching `Dir8` movement (a diagonal advances both axes at once). Correct
  choice; `distance_is_chebyshev` pins it against Manhattan/Euclidean.
- **`range_band`** (`:426`) cuts `0|1 → Adjacent`, `2 → Near`, `3+ → Far`, matching
  decision #6. `range_band_cuts_match_decision_6` pins every boundary.
- `Range { Adjacent, Near, Far }` replaces the 5-band `RangeBand` (`types.rs:94`).

### 5. The seam split — ENDORSED

The architect kept `range_band` in `grid.rs` as a **pure `Pos → Range` classifier** and left
`in_band` + the `[1.0, 0.6, 0.3]` `band_falloff` to the resolver's `geometry.rs` (R1). **This
is the right split**, and it mirrors the existing 1-D layering exactly: today
`geometry.rs:66` / `:75` own `band_falloff` / `in_band` and import only the band *type* from
`types.rs`. Keeping the falloff **table** (a tunable damage policy — decision #6 says "tune in
playtest") out of the dimension-vocabulary module is correct layering: `grid.rs` stays the
pure WHERE-vocabulary; the resolver owns damage policy.

Likewise, **`from_to`** (`:227`) correctly ships only the **exact grid-step octant** (eight
sign-pair cases) and explicitly defers the magnitude-aware "snap an arbitrary vector to the
nearest of 8" to the resolver's R1 `direction_to`. That is the clean seam for R2's
"diagonals snap to the nearest face by signed angle" rule — A2 only needs the grid-step
octant, which the signs of `(d_col, d_row)` give directly.

---

## Optional nits (non-blocking — architect's call, fine to ship as-is)

1. **`range_band(p, p) = Adjacent`** (the distance-0 → `Adjacent` collapse, doc'd at
   `:401`). The doc notes "a ship is never at range 0 from a distinct target," which is true,
   but `Adjacent` *is* a real return value for a co-located / self-targeted lookup. Harmless;
   flagging only because the 0→`Adjacent` fold is load-bearing if any future code distance-
   buckets a cell against itself and expects a distinct "same cell" answer.
2. **`all_positions()` and `neighbors()` allocate a `Vec`** (`:106`, `:265`). Totally fine at
   `CELLS = 20` and matches the "cheap, no allocation beyond the returned Vec" doc note. If a
   hot AI/threat loop ever calls `neighbors()` per-cell × per-enemy × per-phase, an
   iterator / `SmallVec` variant could be added later — a future perf nicety, **not** an A2
   concern and not something that should shape the A3 type commit.

---

## Tests

16 inline tests in `#[cfg(test)] mod tests` (`grid.rs:438–629`), all passing. Coverage is
appropriately light here — the module docstring correctly defers heavy coverage to the
tester's **T1** (grid property/table tests). Test names read as sentences and cover: index
roundtrip + row-major order, `Dir8` step/opposite/rotate algebra, `delta`/`opposite`
cancellation, `from_to` octants + `None`-on-same-cell, `offset` bounds at both ends,
`neighbors` counts (corner 3 / edge 5 / interior 8), Chebyshev distance, `range_band` cuts,
`Dir4↔Dir8` roundtrip, `Axis::dirs`, `all_positions` order, and serde roundtrips for `Pos` /
`Dir8` / `Facing`.

---

## Verdict

**APPROVE.** The type surface is complete and correct for 2D combat; the conventions are
internally consistent and match the live board's access order; the seam split is the right
layering. **A3 is clear to lock this surface.** No required changes.
