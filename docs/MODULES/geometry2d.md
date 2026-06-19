# `src/geometry2d.rs` — Module Companion

*A self-contained walkthrough of the **2-D** spatial core — the live geometry that drives
combat in v2. The same content as the relevant parts of
[`LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcresolvers) (it cross-references this file), but
scoped: read this if you are about to touch range bands, firing arcs, hull-facing damage
zones, or the per-face shield model. Assumes only that you have read
[`grid.md`](grid.md) (the frozen `Pos`/`Dir8`/`Facing`/`Range` type surface).*

**Source:** `src/geometry2d.rs`.
**Mirrors:** the 2-D port of `_drive_pull/broadside-engine/engine/geometry.ts` (and of the
1-D `src/geometry.rs`). Several functions deliberately **deviate** — see Drift.
**Design anchor:** [Broadside Mechanics & Engine Analysis](../../_drive_pull/broadside-analysis.html),
Part III (Targeting, Arcs & Range Bands) and Part IV (Orientation & Movement); blueprint
decisions #2 (5×4 board), #6/#104 (band falloff), #7 (over-extension deadzone), #9
(cardinals-only firing); combat model #103/#104 (integer damage + shield pools).

---

## Why this file exists (and why it is separate from `geometry.rs`)

This is the v2 replacement for the 1-D `geometry.rs`. It is the resolver lane task **R1**:
the geometry that makes orientation, arcs, and range bands a real decision, ported onto the
2-D [`grid`](grid.md) type surface. Every function is **pure and deterministic** — that is
load-bearing for the telegraph: the renderer's ThreatMap is painted by running the *same*
geometry the real shot uses ([`paint_threats`](resolve.md)), so a non-deterministic helper
here would let the telegraph and the actual hit disagree.

It lands as an **additive** module rather than overwriting `geometry.rs` because the atomic
`cell:usize → Pos` type migration (blueprint A3) has not fully landed: ~12 modules still
depend on the 1-D `geometry`. The two coexist without collision (1-D fns take `usize`,
these take [`Pos`](grid.md)); when A3 contracts, the architect deletes `geometry.rs` and
`git mv`s this file onto it. **Until then, the live combat path uses `geometry2d`** and the
1-D `geometry` survives only for the legacy `apply_damage` / unmigrated callers.

---

## The public surface

| Symbol | Kind | One-line |
|--------|------|----------|
| `opposite(dir)`                       | fn | 180°-opposite `Dir8` (delegates to `Dir8::opposite`). |
| `distance(a, b)`                      | fn | Chebyshev distance (thin re-export of `grid::distance`). |
| `range_band(a, b)`                    | fn | Bucket distance into `Adjacent`/`Near`/`Far` (re-export of `grid::range_band`). |
| `direction_to(a, b) -> Option<Dir8>`  | fn | **Magnitude-aware** nearest-of-8 snap of the vector `b - a`; `None` if `a == b`. |
| `band_falloff(raw, actual) -> i32`    | fn | **Integer absolute** per-band damage penalty (#104). |
| `in_band(allowed, atk, tgt) -> bool`  | fn | Is the target inside a weapon's allowed band set (the over-extension deadzone)? |
| `facing_zone(facing, incoming_from) -> HullZone` | fn | Which hull face eats a hit arriving from `incoming_from` (8-way). |
| `arc_bears(facing, arc, toward) -> bool` | fn | Does a mount's firing arc bear toward `toward` (cardinal-exact)? |
| `absorb_shield(face, dmg) -> i32`     | fn | Soak a hit through one face's depleting shield POOL; returns overflow to hull. |
| `default_shield_profile() -> ShieldProfile` | fn | The starting Frigate per-face pool caps (bow 4 / stern 1 / flanks 3). |

---

## Function reference

### `direction_to(a, b) -> Option<Dir8>` (geometry2d.rs:101)

The nearest-of-eight direction pointing from `a` toward `b`. **Magnitude-aware**, unlike
`grid::from_to` (which classifies by the *sign* of each axis delta and so is only exact for
axis-aligned / 45° vectors): a shallow vector like `(3, 1)` snaps to `E` here (true nearest
octant), where `from_to` returns `SE`. Method: pick the `Dir8` whose unit step has the
greatest cosine similarity to `b - a` (dot product over the step's magnitude). Ties resolve
to the lower `Dir8::step` index — a deterministic tie-break so the telegraph and the shot
always agree. The board is 5×4 so components are tiny (`≤4`); `f64` is exact and
deterministic here. Used by the damage step (`incoming_from = direction_to(target, attacker)`)
and by `paint_threats` / the AI to point a shot.

### `band_falloff(raw, actual) -> i32` (geometry2d.rs:150)

The per-band damage curve. **`[0, 1, 2]` integer PENALTY** indexed by the *actual* band
(`Adjacent -0, Near -1, Far -2`), then `.max(1)` (floored at ≥1) and `.min(raw.max(0))`
(capped at raw). Keyed on the **actual** band the shot crosses — an *absolute* falloff
(closer = more damage), **not** the 1-D engine's distance-from-optimal delta. `optimal_band`
is no longer consulted (#44/#95). A weapon's *allowed* bands (the over-extension deadzone,
#7) are enforced separately by `in_band` at targeting; this only scales damage once the shot
is already legal — which is *why* it floors at 1 rather than 0 (a legal in-band hit never
whiffs to 0 from falloff alone). This is the #104 INTEGER ruling: no float in the damage
path. Penalties are Bruce-tunable constants. Pipeline step 1 of
[`apply_damage_2d`](resolve.md).

### `in_band(allowed, attacker, target) -> bool` (geometry2d.rs:167)

`allowed.contains(&range_band(attacker, target))`. The gate that realizes the
over-extension deadzone (#7): a weapon whose `allowed` omits `Adjacent` cannot hit a cell it
has been closed on; one whose `allowed` omits `Far` cannot reach across the board. (This is
where a *long-range positional check on player over-extension* is enforced — the design
intent that inert long-range enemies at mid-lane are intended, not a bug.)

### `facing_zone(facing, incoming_from) -> HullZone` (geometry2d.rs:218)

The **correctness-critical** 2-D quadrant table: which fixed `HullZone` eats a hit arriving
**from** direction `incoming_from`, given the target's `Facing`. The 2-D replacement for the
1-D `facing_zone(Orientation, LaneEnd)`. Pure logic over `Facing` + `Dir8` + `HullZone`. Two
stances:

- **`Bow(dir)`** (`bow_zone`, geometry2d.rs:226) — a clean **3 / 3 / 1 / 1** partition by
  clockwise offset `rel` of `incoming_from` from the bow vector: dead-ahead ± the two 45°
  diagonals (`rel 7|0|1`) → **Bow** (strong face); the rear ±45° arc (`rel 3..=5`) →
  **Stern** (weak face); the +90° right cardinal (`rel 2`) → **Starboard**; the -90° left
  (`rel 6`) → **Port** (standard nautical: right = starboard).
- **`Broadside(axis)`** (`broadside_zone`, geometry2d.rs:267) — a hull turned across the
  grid runs *along* its axis, so its ends (Bow/Stern) point along the axis and flanks
  (Port/Starboard) perpendicular. Anchored on a deterministic pseudo-forward (`axis.dirs().0`
  — `S` for NorthSouth, `E` for EastWest). A clean **2 / 2 / 2 / 2** partition: each face
  owns its cardinal plus the diagonal 45° counter-clockwise of it. A turned hull has no
  inherent front, so the Bow/Stern + Port/Starboard split is a stable *convention*, not a
  physical fact (locked by tester T2's exhaustive Dir8×Facing table + reviewer V3).

Pipeline step 4 of `apply_damage_2d` (`facing_zone(target.facing, incoming_from)` picks the
face whose pool soaks the hit). Also read by `end_of_turn`'s under-fire-pause to mark which
faces took fire.

### `arc_bears(facing, arc, toward) -> bool` (geometry2d.rs:316)

The 2-D firing-arc gate — the gate that makes facing matter when *shooting*. `toward` is
`direction_to(firer, target)`. Under the v2 **cardinals-only firing** model (#9) a weapon
fires along an *exact* cardinal ray, so an arc bears **iff `toward` is exactly that arc's
cardinal — not a ±45° cone**. A diagonal `toward` never bears.

- `Turret` — bears in every direction.
- `Forward` — only a `Bow` stance, firing out the exact bow cardinal.
- `Rear` — only a `Bow` stance, firing out the exact stern (bow-opposite) cardinal.
- `BroadsideArc` — fires out the two flank cardinals **perpendicular to the bow** (Model D,
  #92): turning the bow E/W puts the flanks N/S — that *is* broadsiding. On-axis (bow + its
  opposite) and all diagonals do NOT bear.

> **Different arity from `facing_zone` — do not conflate.** FIRING (`arc_bears`) is
> cardinal-exact (4-way); RECEIVING (`facing_zone`) is 8-way (an off-axis splash or ordnance
> hit can arrive diagonally and land on whatever face it presents). Making `arc_bears` a
> ±45° cone would wrongly let a broadside "bear" on a diagonal target it cannot hit.
> `arc_bears` ⊊ the corresponding `facing_zone` sector — never assert they are equal.

### `absorb_shield(face, dmg) -> i32` (geometry2d.rs:368)

The per-face shield, the #103 Model A overhaul. `face.charge` is the live **depleting shield
POOL**; `face.armour` is repurposed as the pool **CAPACITY**. A hit soaks the pool down to 0
and the **overflow** reaches hull:

```
if dmg <= 0 { return 0 }                  // zero/negative never burns the pool
soak = dmg.min(face.charge.max(0))
face.charge -= soak                        // pool depletes
return (dmg - soak).max(0)                 // overflow -> hull
```

Flat `armour` subtraction is **gone** (that was the 1-D model). All integer (#104). The pool
regenerates over turns in [`resolve::end_of_turn`](resolve.md) — refilling by
`SHIELD_REGEN_PER_TURN` (= 1) toward `armour`, but **only on faces that did not take fire
that turn** (the under-fire pause). Pipeline step 4 of `apply_damage_2d`.

> **Why this fixed "combat feels dead."** Under the old flat-armour model, a bow armour of 2
> exactly cancelled the `floor(4 × 0.6) = 2` chip from a Near shot — every chip hit
> netted 0. The depleting pool + integer falloff means chip fire actually erodes the bow
> pool (and a sustained focus cracks it), so combat resolves.

### `default_shield_profile() -> ShieldProfile` (geometry2d.rs:390)

The starting Frigate per-face pool. `armour` = capacity, `charge` = live pool (starts FULL).
Caps (Bruce-tunable): **bow 4, stern 1, port/starboard 3** — the directional gradient the
design rewards flanking against (a bow-on player tanks chip fire on the strong bow pool; the
soft stern cracks fast). Per-face regen amount/cooldown are global consts in `resolve`.

---

## Drift from the 1-D `geometry.rs` / TypeScript

1. **`band_falloff` is integer + absolute, not a float distance-from-optimal curve.** The
   1-D `geometry::band_falloff(raw, actual, optimal)` used a float table indexed by
   band-distance from the weapon's optimal band. This one drops `optimal` entirely and
   applies a flat integer penalty by absolute band (#44/#95/#104).
2. **`absorb_shield` is a depleting pool, not charge-then-flat-armour.** The 1-D version:
   a held `charge` negates one hit; otherwise flat `armour` is subtracted permanently. This
   version: `charge` is a pool that soaks and overflows, `armour` is its capacity, and it
   regenerates over turns (#103).
3. **`facing_zone` is 8-way over `Facing`, not 2-way over `Orientation`/`LaneEnd`.** The 1-D
   version answered fore/aft on a lane; this one is the full quadrant table over 8 incoming
   directions and the `Bow`/`Broadside` stances.
4. **`arc_bears` for `BroadsideArc` deviates from canonical TS** (which required a separate
   `Broadside` *stance*). In v2 the bow-cardinal stance model (#92) means turning the bow
   E/W *is* broadsiding; firing (`bearing_cardinals`) mirrors this exactly so the gate and
   the shot stay one model.
5. **`direction_to` is the magnitude-aware snap** grid.rs's `from_to` explicitly defers to
   "R1 `direction_to`."

---

## Tests

In-file `#[cfg(test)]` mod has one+ sanity assert per pure function (the contract guard at
the source). Deep coverage — the full `Dir8 × Facing` `facing_zone` sweep, the whole
falloff/Chebyshev range — is the tester's lane (blueprint T2, `tests/geometry2d.rs`), and
reviewer V3 guards the `facing_zone` table. Representative names:
`band_falloff_is_integer_penalty_per_band`,
`band_falloff_floors_legal_shot_at_one_and_keeps_zero_zero`,
`absorb_shield_pool_soaks_down_to_zero_overflow_to_hull`,
`absorb_shield_empty_pool_passes_full_to_hull`,
`absorb_shield_partial_soak_and_ignores_nonpositive`.

---

## Cross-references

- Consumed by [`resolve.rs`](resolve.md): `apply_damage_2d` (steps 1 + 4), `end_of_turn`
  (shield regen + under-fire pause), `resolve_targeting_2d` (arc/band gating), `paint_threats`.
- Type surface from [`grid.rs`](grid.md): `Pos`, `Dir8`, `Dir4`, `Facing`, `Axis`, `Range`,
  `HullZone`, `Arc`, `ShieldFace`, `ShieldProfile`.
- Pipeline order + the `direction_to → incoming_from` wiring are guarded by reviewer V5
  (`a61db55`). Combat model law: [`docs/design/CORE_GAMEPLAY_LOOP.md`](../design/CORE_GAMEPLAY_LOOP.md)
  §Implementation (integer band falloff + per-face shield pools).
