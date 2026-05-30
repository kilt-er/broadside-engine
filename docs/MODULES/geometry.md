# `src/geometry.rs` — Module Companion

*A self-contained walkthrough of the spatial core. The same content as the
[`geometry.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcgeometryrs), but
scoped: this file assumes you only care about geometry and don't need the rest of the
engine in context. Read this if you are about to touch range bands, arcs, facings, or
shield absorption.*

**Source commit:** `d383c6a` — *Port engine/geometry.ts to src/geometry.rs*.
**Mirrors:** `_drive_pull/broadside-engine/engine/geometry.ts`.
**Design anchor:** [Broadside Mechanics & Engine Analysis](../../_drive_pull/broadside-analysis.html),
Part III (Targeting, Arcs & Range Bands) and Part IV (Orientation & Movement).

---

## Why this file exists

Broadside has four mechanical axes the Shogun Showdown engine did not need: **range
bands**, **hull orientation**, **firing arcs**, and **directional shields**. All four
are pure spatial logic — they ask questions like "is this target in mid range?" and
"does this mount currently aim at the bow side?" without touching content, randomness,
or game state outside the small set of types the question requires.

Everything that answers those questions lives in this one file. The resolver
([`resolve.rs`](resolve.md) — pending) imports from here; this file imports only from
[`types.rs`](types.md) (pending) and `std`. The dependency arrow points one direction.

If you change a function here, expect to update the resolver's damage pipeline and
targeting dispatch. If you find yourself reaching for randomness or content lookups,
you are in the wrong file.

---

## The public surface

Eleven items, all `pub`:

| Item                                  | Kind     | One-line role                                       |
|---------------------------------------|----------|-----------------------------------------------------|
| `opposite(end)`                       | fn       | Flip `Fore` ↔ `Aft`.                                |
| `direction_to(a, b)`                  | fn       | Lane direction from cell `a` to cell `b`.           |
| `distance(a, b)`                      | fn       | Absolute cell distance.                             |
| `range_band(atk, tgt)`                | fn       | Bucket distance into one of five bands.             |
| `band_falloff(raw, actual, optimal)`  | fn       | Reduce damage by band-distance from optimal.        |
| `in_band(allowed, atk, tgt)`          | fn       | Predicate: is the target in an allowed band?        |
| `facing_zone(o, incoming_from)`       | fn       | Which hull zone takes a lane hit?                   |
| `arc_bears(o, arc, toward_end)`       | fn       | Does this mount aim at this direction?              |
| `bears(ship, arc, target_cell)`       | fn       | Higher-level wrapper; `None` arc always bears.      |
| `absorb_shield(face, dmg)`            | fn       | Run damage through one zone's shield + armour.      |
| `default_shield_profile()`            | fn       | The starting Frigate's four-zone shield layout.     |

Plus one private item — `band_index(b)` — and the const `BAND_ORDER`, used together to
implement `band_falloff`.

---

## How it all fits

The cleanest mental model: every weapon resolution touches *at most three* of these
functions, in a fixed order.

**On the offensive side** (does the shot even fire? where does it land?):

```
   resolver
     │
     ▼
   bears(ship, action.requiresArc, target_cell)        ← can the mount fire?
     │
     ├──► arc_bears(ship.orientation, arc, dir)
     │      │
     │      └──► opposite(bow)     [rear arc]
     │
     └──► direction_to(ship.cell, target_cell)

   resolver
     │
     ▼
   in_band(action.targeting.band, ship.cell, tgt.cell) ← does range allow it?
     │
     └──► range_band(...)
            │
            └──► distance(...)
```

**On the defensive side** (now the damage pipeline reaches this file twice):

```
   apply_damage (resolve.rs, future)
     │
     ├──► band_falloff(raw, range_band(atk, tgt), weapon.optimalBand)
     │      │
     │      └──► band_index(...) twice via BAND_ORDER
     │
     │      ... subsystem modifiers, target-lock x2 (resolver-internal) ...
     │
     ├──► facing_zone(target.orientation, direction_to(target.cell, atk_cell))
     │
     └──► absorb_shield(&mut target.shield_profile[zone], dmg)
```

`facing_zone` and `arc_bears` are the **shield/mount twins**: `facing_zone` answers
"which shield faces the lane?", `arc_bears` answers "which mount faces the lane?". The
asymmetry between them — bow-shielded vs. forward-mounted may align differently for the
same orientation — is exactly the rotation pressure HTML Part IV describes.

---

## Function reference

Detailed line-by-line walkthroughs for every function in this module are in
[`LINE_BY_LINE.md` § src/geometry.rs](../LINE_BY_LINE.md#srcgeometryrs). The reference
below is a quick lookup table; cross-link out for the full prose.

### `opposite(end: LaneEnd) -> LaneEnd`
**Line:** `geometry.rs:13`. **Mirrors:** `geometry.ts:11`. Flip `Fore` ↔ `Aft`. Used by
`arc_bears` to compute the rear-arc direction.

### `direction_to(a: usize, b: usize) -> LaneEnd`
**Line:** `geometry.rs:22`. **Mirrors:** `geometry.ts:16`. `b >= a → Fore`, else `Aft`.
**Watch:** equal cells return `Fore` — pinned by `direction_to_treats_equal_cells_as_fore`
at `:185`.

### `distance(a: usize, b: usize) -> usize`
**Line:** `geometry.rs:31`. **Mirrors:** `geometry.ts:21`. `a.abs_diff(b)`. Underflow-safe
substitute for TS `Math.abs(a - b)`.

### `range_band(attacker_cell, target_cell) -> RangeBand`
**Line:** `geometry.rs:52`. **Mirrors:** `geometry.ts:30`. Buckets distance:
0–1 → `PointBlank`, 2 → `Close`, 3–4 → `Mid`, 5–6 → `Long`, 7+ → `Extreme`.

### `band_falloff(raw, actual, optimal) -> i32`
**Line:** `geometry.rs:69`. **Mirrors:** `geometry.ts:41`. `floor(raw * factor)` where
`factor = [1.0, 0.66, 0.5, 0.33, 0.2][|actual - optimal|]`, floored at 0. **Worked
example:** raw 4, actual Mid, optimal Close → delta 1 → factor 0.66 → returns 2.

### `in_band(allowed, atk_cell, tgt_cell) -> bool`
**Line:** `geometry.rs:78`. **Mirrors:** `geometry.ts:48`. `allowed.contains(&range_band(...))`.

### `facing_zone(o, incoming_from) -> HullZone`
**Line:** `geometry.rs:91`. **Mirrors:** `geometry.ts:61`. The function that makes
orientation meaningful.

| Stance       | Rule                                                                          |
|--------------|-------------------------------------------------------------------------------|
| `BowOn{bow}` | `incoming_from == bow` → `Bow`, else `Stern`. Flanks never hit.               |
| `Broadside`  | `Fore` → `Starboard`, `Aft` → `Port`. Bow is wasted off-lane.                 |

### `arc_bears(o, arc, toward_end) -> bool`
**Line:** `geometry.rs:117`. **Mirrors:** `geometry.ts:74`. The offensive twin of
`facing_zone`.

| Arc            | Bears when…                                                                  |
|----------------|------------------------------------------------------------------------------|
| `Turret`       | Always.                                                                      |
| `Forward`      | `BowOn{bow}` with `toward_end == bow`.                                       |
| `Rear`         | `BowOn{bow}` with `toward_end == opposite(bow)`.                             |
| `BroadsideArc` | `Broadside` (toward_end ignored — caller enumerates both ends).              |

### `bears(ship: &Ship, arc: Option<Arc>, target_cell: usize) -> bool`
**Line:** `geometry.rs:131`. **Mirrors:** `geometry.ts:90`. `None → true`; else compute
the direction and delegate to `arc_bears`. The public surface — resolver should call
this, not `arc_bears` directly.

### `absorb_shield(face: &mut ShieldFace, dmg: i32) -> i32`
**Line:** `geometry.rs:144`. **Mirrors:** `geometry.ts:101`. Step 4 of the damage
pipeline. Charge negates the hit one-for-one regardless of magnitude; armour subtracts
permanently. Zero damage does **not** burn charge.

### `default_shield_profile() -> HashMap<HullZone, ShieldFace>`
**Line:** `geometry.rs:160`. **Mirrors:** `geometry.ts:112`. Bow armour 2, stern 0,
flanks 1/1. All zero charge.

---

## Drift from TypeScript

Three watch-list items from before the port landed have been **resolved** by `d383c6a`:

1. **Float math in `band_falloff` (drift watch #6).** Architect kept the TS `f64`
   scaling rather than switching to fixed-point integer percentages. Cross-platform
   determinism is not currently a stated requirement; can be revisited later without
   changing the signature.

2. **Shield-profile representation (drift watch #3).** TS uses
   `Record<HullZone, ShieldFace>` — a JS object keyed by a string union. Rust kept
   `HashMap<HullZone, ShieldFace>` rather than `[ShieldFace; 4]` indexed by `HullZone`.
   Trade-off: keeps the JSON wire shape symmetric with TS at the cost of one hash
   lookup per shield check. Can switch to an array later if profiling demands it.

3. **`ShieldFace` mutation in `absorb_shield`.** TS mutates via reference; Rust uses
   `&mut ShieldFace`. The resolver will need disjoint borrows to call this — flagged
   as a future-care item for `resolve.rs`.

**No new drift was introduced.** The Rust port is line-for-line equivalent to the TS
except for:

- `direction_to` uses `>=` rather than subtraction to avoid `usize` underflow.
- `distance` uses `usize::abs_diff` rather than `Math.abs`.
- `arc_bears` uses `matches!(o, ... if ...)` rather than the TS switch/case.
- `band_falloff` uses `unsigned_abs() as usize` for the delta computation.

All four are idiomatic substitutions, not behavioural changes.

---

## Tests

19 unit tests live in `#[cfg(test)] mod tests` at `geometry.rs:174–326`. Each
function has at least one test; multi-arm functions (`facing_zone`, `arc_bears`,
`absorb_shield`) have one test per arm. The test names read as sentences:

```
opposite_swaps_ends
direction_to_treats_equal_cells_as_fore
distance_is_absolute
range_band_buckets_match_the_ruler
band_falloff_full_damage_when_actual_equals_optimal
band_falloff_drops_off_outside_optimal_band
band_falloff_floors_negative_inputs_at_zero
in_band_respects_allowed_set
facing_zone_bow_on_routes_lane_hits_to_bow_or_stern
facing_zone_broadside_routes_to_flanks_deterministically
arc_bears_turret_always_bears
arc_bears_forward_only_fires_out_the_bow
arc_bears_rear_only_fires_astern
arc_bears_broadside_only_when_turned_broadside
absorb_shield_charge_negates_hit_and_decrements
absorb_shield_falls_back_to_armour_when_no_charge
absorb_shield_clamps_when_armour_exceeds_damage
absorb_shield_ignores_non_positive_damage
default_shield_profile_matches_the_doc
```

A broader integration test suite at `tests/geometry.rs` is currently in progress
(task #18, owned by tester); when it lands, this file should reference it as the
"see also" for cross-function scenarios.

---

## Cross-references

- **Type vocabulary:** `Arc`, `HullZone`, `LaneEnd`, `Orientation`, `RangeBand`,
  `ShieldFace`, `Ship` — all from [`src/types.rs`](types.md) (companion pending).
- **Consumer:** the future resolver, which calls into this file at every targeting
  check and every damage application. See [`resolve.md`](resolve.md) (pending).
- **Domain terms:** every concept here is in the [glossary](../GLOSSARY.md) — start with
  *Range band*, *Bow-on / Broadside*, *Hull zone*, *Armour*, *Charge*, *Arc*.
- **Design intent:** Parts III and IV of the
  [analysis document](../../_drive_pull/broadside-analysis.html).
