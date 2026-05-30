# Broadside — Line-by-Line Walkthrough

*A complete prose walkthrough of every non-trivial line of the Broadside Rust engine.
Each function gets an opening paragraph of intent, then a line-by-line explanation, then
a cross-reference block. Each section opens with how the module mirrors (or deviates
from) its TypeScript original at `_drive_pull/broadside-engine/engine/*.ts`.*

*This file is the structural skeleton. **Line-entry bodies are empty until each Rust
source file lands** — they will be filled in as `broadside-architect`, `broadside-resolver`,
`broadside-content`, and `broadside-renderer` ship their modules. The skeleton exists so
the table of contents stays stable as content fills in, and so a reader can see at a
glance what is documented vs. what is pending.*

*Conventions used throughout:*

- ***Mirrors:*** *line points at the TS source the Rust function ports from.*
- ***Intent:*** *one paragraph on what the function exists to do.*
- ***Drift:*** *note appears when the Rust deviates from TS verbatim, explaining why.*
- ***Worked example:*** *concrete trace, usually pulled from the test suite or `demo.ts`
  scenarios.*

---

## Table of Contents

- [`src/lib.rs`](#srclibrs) — crate root, re-exports, module declarations
- [`src/types.rs`](#srctypesrs) — every type, no logic
- [`src/geometry.rs`](#srcgeometryrs) — pure geometry: bands, arcs, facings, shields
- [`src/resolve.rs`](#srcresolvers) — the four-phase round, queue gate, damage pipeline, effect dispatch
- [`src/effects.rs`](#srceffectsrs) — movement, modifiers, displacement (TS TODO bodies)
- [`src/content.rs`](#srccontentrs) — catalog loading, projectile spawn dispatch
- [`src/ai.rs`](#srcairs) — enemy decision layer
- [`src/bus.rs`](#srcbusrs) — event bus + hooks
- [`src/catalog.rs`](#srccatalogrs) — JSON catalog → typed records
- [`src/gfx/`](#srcgfx) — wgpu renderer, atlas, HUD
- [`tests/`](#tests) — integration tests, worked examples

---

## `src/lib.rs`

*Crate root: module declarations and re-exports.*

**Mirrors:** No direct TS analog. TS uses ESM imports; Rust gathers public surface here.

*Pending architect's first commit.*

---

## `src/types.rs`

*Every TS interface and type alias as a Rust struct or enum. No logic. Mirrors Section
XIII of the analysis HTML, which says: "Build the geometry + resolver first; catalogs
are pure content." Types are the precondition for both.*

**Mirrors:** `engine/types.ts` (the entire file).

### Section headings to fill

The TS file groups types under banner comments. The Rust port will follow the same
grouping. Each subsection below will get an intent paragraph and per-line walkthrough
when the Rust lands.

- **Geometry primitives** — `LaneEnd`, `Orientation` (sum type over bow-on / broadside),
  `HullZone`, `RangeBand`, `Arc`, `Faction`.
- **Board** — `Board`, `Hazard`.
- **Ship** — `Ship`, `ShieldFace`, `Mount`, `Status`, `StatusKind`, `Trait`.
- **Action** — `Action`, `ActionCost`, `Targeting`, `WeaponArchetype`, `TargetingPattern`.
- **Effects** — the `Effect` sum type with all nine variants, plus `MovementMode`.
- **Ordnance** — `Projectile`.
- **Subsystem + bus** — `Subsystem`, `Hook`, `HookContext`, `EventBus`.
- **Catalog** — `Catalog`, `EnemyDef`.

### Drift watch list (decisions for the architect)

These are the design choices where Rust will deviate from TS by *necessity*, not by
preference. When the architect picks one, this list moves into the actual line entries
as **Drift** notes.

- **`klass` → `class` / `ship_class` / `class_id`** — TS used `klass` to avoid the JS
  reserved word. Rust has no such conflict. Architect's call. (`types.ts:66`.)
- **`apply: (ctx) => void`** — TS subsystem stores a closure as a struct field. Rust
  will likely move this to a trait (`trait SubsystemApply { fn apply(&self, ctx:
  &mut HookContext); }`) or a function pointer; impacts how `Subsystem` is constructed.
  (`types.ts:173`.)
- **`Record<HullZone, ShieldFace>`** — TS object keyed by string-union. Rust options:
  `[ShieldFace; 4]` with `HullZone` as an index, or `HashMap<HullZone, ShieldFace>`.
  Array is faster and the cardinality is fixed; pick that unless there's reason not to.
  (`types.ts:60`.)
- **`bus: EventBus` field on Board** — TS stores the bus inline. Rust will likely keep
  the bus separate from `Board` to avoid borrow conflicts when emitters need
  `&mut Board` in the context. Watch for it. (`types.ts:37`.)
- **`Effect` discriminated union** — TS uses `kind: string` literal types. Rust uses a
  proper `enum` with payload per variant. Direct port; no design choice here.
- **`Catalog`'s `unknown[]` fields** — TS leaves capitals/classes/fieldkit/sectors/
  commendations as `unknown[]` placeholders. Rust will need concrete types or
  `serde_json::Value` for those. Decide at port time.

*Per-line walkthroughs pending architect's `src/types.rs` first commit. The file is
known to be on disk but not yet authorized for documentation.*

---

## `src/geometry.rs`

*Pure functions over the lane. No randomness, no content lookups, no I/O. Everything
that makes orientation, arcs, and range bands a real decision lives here. The Rust port
is a near-verbatim translation of the TS source — when in doubt, the TS is the canonical
reference (the module rustdoc says so explicitly at `geometry.rs:5`).*

**Mirrors:** `engine/geometry.ts` (the entire file).
**Design anchor:** HTML Part III (Targeting, Arcs & Range Bands) and Part IV
(Orientation & Movement).
**Source commit:** `d383c6a` — *Port engine/geometry.ts to src/geometry.rs*. All 19
tests in `#[cfg(test)] mod tests` pass; reviewer audited cleanly.

### Module header (lines 1–10)

The first six lines are a `//!` module rustdoc block that sets the contract: pure
geometry, no randomness, no content. The line that matters for every future reader:
*"when this port and the TS disagree, the TS is right."* That sentence is the
tie-breaker; cite it whenever a drift question turns into a judgment call.

Line 8 pulls in `HashMap` from `std::collections` — used only by
`default_shield_profile`, which returns a `HashMap<HullZone, ShieldFace>` so the on-disk
catalog shape (a JSON object keyed by hull-zone names) round-trips cleanly through serde.
The fact that this is a `HashMap` rather than `[ShieldFace; 4]` is itself a drift
decision; see **Drift note: ShieldProfile representation** below.

Line 10 imports the type vocabulary: `Arc`, `HullZone`, `LaneEnd`, `Orientation`,
`RangeBand`, `ShieldFace`, `Ship`. All seven come from `crate::types` and are documented
in this file's [`src/types.rs`](#srctypesrs) section.

---

### `fn opposite(end: LaneEnd) -> LaneEnd` (geometry.rs:13)

**Mirrors:** `engine/geometry.ts:11 — function opposite(end)`.
**Intent:** Flip a lane direction. `Fore` becomes `Aft` and vice versa. Used by
`arc_bears` to compute "the direction the *stern* points" from "the direction the
bow points," and indirectly by `facing_zone` callers in the resolver.

Line 13: signature — `LaneEnd` is `Copy`, so we take by value and return by value with
no lifetime concerns. The TS uses a string-union and a ternary; Rust uses a `match`,
which the compiler turns into a single conditional move at the machine-code level.

Lines 14–17: `match end { Fore => Aft, Aft => Fore }`. The match is exhaustive on a
two-variant enum, so there is no `_` fallthrough and the compiler will catch any future
`LaneEnd` variant addition by failing this match.

**Cross-references:** Called by `arc_bears` (`geometry.rs:121`) to compute the rear
arc's bearing direction. Will be called by `resolve.rs` once the resolver lands.

---

### `fn direction_to(a: usize, b: usize) -> LaneEnd` (geometry.rs:22)

**Mirrors:** `engine/geometry.ts:16 — function directionTo(a, b)`.
**Intent:** The direction you must travel along the lane to get *from* `a` *to* `b`.
The resolver uses this from the target's perspective when applying damage — given the
attacker's cell and the target's cell, it asks "which lane end does the shot arrive
from?" so it can look up the right hull zone.

Line 22: signature uses `usize` for cell indices (consistent with `Vec` indexing) and
returns `LaneEnd` by value.

Lines 23–27: `if b >= a` returns `Fore`, else `Aft`. The `>=` is load-bearing — when
`a == b` the function returns `Fore`, not panic, not error. The doc comment on line 21
flags this explicitly (*"`a == b` is `Fore`"*) because the TS does the same and the
behaviour is easy to miss when reading the function in isolation.

**Drift note: signed vs unsigned.** TS uses `number` and computes via `>=`, which
works on both. Rust uses `usize` for cells — `b - a` would underflow on `b < a`, so the
implementation uses `>=` rather than subtraction. Equivalent semantics, safer
representation. The architect carried the TS convention through.

**Worked example:** Player at cell 0 fires at a scout at cell 1. The resolver needs
to know which face of the scout takes the hit, so it calls
`direction_to(target.cell, attacker.cell)` = `direction_to(1, 0)` = `Aft`. From the
scout's frame, the shot arrives *from aft*. `facing_zone` then maps `Aft` plus the
scout's orientation to a hull zone.

**Cross-references:** Called by `bears` (`geometry.rs:134`) to derive the bearing
direction toward the target cell. Will be called heavily by the resolver in
`apply_damage` and `advance_projectile`.

---

### `fn distance(a: usize, b: usize) -> usize` (geometry.rs:31)

**Mirrors:** `engine/geometry.ts:21 — function distance(a, b)`.
**Intent:** Absolute cell distance between two lane positions. Used by `range_band`
to bucket distance into a band, and (eventually) by AI scoring.

Line 32: one line — `a.abs_diff(b)`. `usize::abs_diff` is the panic-free way to compute
`|a − b|` on unsigned integers, available since Rust 1.60. Equivalent to TS
`Math.abs(a - b)` but never underflows.

**Cross-references:** Called by `range_band` (`geometry.rs:53`). The TS version is the
only place `Math.abs` appears in `geometry.ts`; Rust's `abs_diff` is the idiomatic
replacement.

---

### `const BAND_ORDER: [RangeBand; 5]` and `fn band_index` (geometry.rs:39–49)

**Mirrors:** `engine/geometry.ts:27 — const BAND_ORDER`.
**Intent:** The canonical band ordering used by `band_falloff` to compute the delta
between an actual and optimal band. The doc-comment on line 37 warns that the array
order **must match `RangeBand`'s declaration order in `types.rs`**. Both files do — see
`types.rs:65–73`. If the enum is reordered, `band_index` will return the wrong index
and `band_falloff` will silently mis-scale damage.

Lines 39–45: the array literal. Five entries, one per band, in monotonically increasing
distance order.

Lines 47–49: `band_index(b: RangeBand) -> usize` — linear search returning the position.
`.expect(...)` panics with a clear message if a future `RangeBand` variant is added but
not appended to `BAND_ORDER`; the test suite catches it before merge.

**Drift note: lookup strategy.** TS uses `BAND_ORDER.indexOf(b)`. Rust uses
`.iter().position(|x| *x == b)`. Same O(n) scan over n=5 — negligible.

---

### `fn range_band(attacker_cell: usize, target_cell: usize) -> RangeBand` (geometry.rs:52)

**Mirrors:** `engine/geometry.ts:30 — function rangeBand(attackerCell, targetCell)`.
**Intent:** Bucket a cell distance into one of the five range bands. This is the
function that turns "the attacker and target are 3 cells apart" into "this is a Mid
range shot." Every targeting check passes through here.

Line 53: compute the absolute distance.

Lines 54–64: the if/else-if ladder mirrors the TS exactly:

| Distance | Band         |
|----------|--------------|
| 0–1      | `PointBlank` |
| 2        | `Close`      |
| 3–4      | `Mid`        |
| 5–6      | `Long`       |
| 7+       | `Extreme`    |

Note that `PointBlank` includes distance 0, even though no two ships occupy the same
cell — distance 0 only arises for `SELF` targeting and self-damage from splash. Tested
at `geometry.rs:201`.

**Cross-references:** Called by `in_band` and (eventually) the resolver every time it
checks whether a weapon may fire. Used inside `apply_damage` to pick the actual band for
falloff math.

---

### `fn band_falloff(raw: i32, actual: RangeBand, optimal: RangeBand) -> i32` (geometry.rs:69)

**Mirrors:** `engine/geometry.ts:41 — function bandFalloff(raw, actual, optimal)`.
**Design anchor:** HTML Part III (range bands as the new ranged economy).
**Intent:** Reduce raw damage by how far the actual band sits from the weapon's
optimal band. This is the central lever that makes range a live decision: a railgun
firing point-blank does the same arithmetic as a scatter laser firing long, and both
come out heavily reduced.

Line 70: compute the band delta. `band_index(actual) - band_index(optimal)` is signed
because `band_index` returns `usize` but Rust requires the cast to `i32` for the
subtraction; the result is fed to `unsigned_abs()` which returns a `u32`, then `as
usize` for the array lookup. Equivalent to `Math.abs(...)` in TS.

Line 71: the falloff table — `[1.0, 0.66, 0.5, 0.33, 0.2]` — keyed by delta. Same
values as the TS source.

Line 72: index into the table with `delta.min(4)` — saturates at the largest delta even
if a future enum addition pushes it past 4.

Line 73: the actual scaling — `(raw as f64 * factor).floor() as i32`. Float math floored
to integer, matching TS `Math.floor(raw * factor)`.

Line 74: `.max(0)` — floor at zero. Tested at `geometry.rs:227` against a negative
input (`band_falloff(-5, Mid, Mid) -> 0`).

**Drift note: float math is preserved.** Architect kept the TS f64 scaling rather than
switching to a fixed-point integer table. Determinism across platforms is not currently
a stated requirement; if it becomes one later (e.g. for networked play or deterministic
replays), the table can move to `[100, 66, 50, 33, 20]` percentages with integer
divide-by-100, and the function signature stays the same. Watch list item resolved.

**Worked example (`band_falloff_drops_off_outside_optimal_band`, geometry.rs:218):**
4 raw damage, actual = Mid, optimal = Close. `band_index(Mid) = 2`, `band_index(Close)
= 1`, delta = 1, factor = 0.66, `floor(4 * 0.66) = floor(2.64) = 2`. The function
returns 2. A second case in the same test: 10 raw, Extreme vs PointBlank → delta 4,
factor 0.2, `floor(10 * 0.2) = 2`.

**Cross-references:** Called by `apply_damage` (resolve.rs, future) at step 1 of the
five-step damage pipeline. The other four steps live in the resolver.

---

### `fn in_band(allowed: &[RangeBand], attacker_cell: usize, target_cell: usize) -> bool` (geometry.rs:78)

**Mirrors:** `engine/geometry.ts:48 — function inBand(allowed, attackerCell, targetCell)`.
**Intent:** Predicate: is the target's range band one of the action's allowed bands?
Used by `resolve_targeting` to filter out shots that can't physically reach. Note: this
is *allowed* vs. *optimal* — a weapon with `band = [PointBlank, Close, Mid]` and
`optimalBand = Close` returns `true` at point-blank (allowed, falloff applies),
but `false` at long (not allowed, won't fire at all).

Line 79: one-liner — `allowed.contains(&range_band(...))`. The slice borrow is taken
by reference; `RangeBand` is `Copy`, so the `.contains(&x)` form is idiomatic.

---

### `fn facing_zone(o: Orientation, incoming_from: LaneEnd) -> HullZone` (geometry.rs:91)

**Mirrors:** `engine/geometry.ts:61 — function facingZone(o, incomingFrom)`.
**Design anchor:** HTML Part IV — directional shields and the rotation principle.
**Intent:** A shot is travelling along the lane and has just reached a target ship.
This function answers the single question that makes orientation a meaningful
decision: *which of the ship's four fixed hull zones is the shot arriving at?* The
answer feeds straight into the directional shield step of the damage pipeline
(`absorb_shield`), and is the mechanism by which "the strong bow soaks, the weak stern
bleeds" actually happens.

`incoming_from` is the lane end that points *back toward the attacker* from the target's
position. If the attacker sits forward of the target, `incoming_from == LaneEnd::Fore`;
the shot is travelling aft, but the *face it lands on* is the one looking fore.

Line 92: `match o` — branch on the orientation enum. The two stances answer the
"which face?" question with different rules; there is no shared math, so we dispatch.

Lines 93–99: `Orientation::BowOn { bow }` arm. If the shot is arriving from the
direction the bow points (`incoming_from == bow`), the bow eats it; otherwise the
stern does. **Flanks never take a lane hit in bow-on stance** — they point off-lane.
This is the asymmetric-armour payoff: turning your bow toward a threat lets the strong
shield absorb the hit; turning it away exposes the weak stern.

Lines 100–106: `Orientation::Broadside` arm. Both flanks face the lane, so every lane
hit lands on *a* flank. To keep the model deterministic, fore-incoming maps to
starboard and aft-incoming to port. Both flanks carry the same medium armour in the
default profile, so the choice of which side is bookkeeping rather than mechanic, but
the convention is stable — important because subsystems (e.g. Reactive Shield) may
attach `charge` to a specific face and need the assignment to be predictable.

**Returns:** the `HullZone` whose `ShieldFace` the caller will pass to `absorb_shield`.

**Drift note: none.** Pure logic, ported verbatim. If the engine ever introduces a
richer `Orientation` (e.g. yaw angle for non-cardinal facing), this function will need
to enumerate the new buckets.

**Worked example (demo.ts Round 1, the canonical scenario):** Player at cell 0 fires
the Pulse Laser fore. Target scout sits at cell 1, bow-on stance with `bow = Fore`.
The resolver computes `incoming_from = direction_to(target.cell, atk_cell) =
direction_to(1, 0) = Aft`. The bow-on arm returns `incoming_from != bow` → `Stern`.
`absorb_shield` then runs against the scout's stern face (armour 0), and the full
(post-falloff) damage gets through. Round 2 swaps the scout to `bow = Aft`; now
`incoming_from == bow`, the bow face takes it (armour 2), and damage is reduced by 2.
Same weapon, same range — orientation alone changed the outcome. The
`facing_zone_bow_on_routes_lane_hits_to_bow_or_stern` test at `geometry.rs:239`
encodes both cases as a unit test.

**Cross-references:**
- Tested at `geometry.rs:239` (bow-on cases) and `:248` (broadside cases).
- Called by `apply_damage` (resolve.rs, future) immediately before `absorb_shield`.
- Caller computes `incoming_from` via `direction_to(target.cell, atk_cell)`.
- `arc_bears` (`geometry.rs:117`) is the offensive twin: it asks which **mounts**
  face the lane, while `facing_zone` asks which **shields** face the lane.

---

### `fn arc_bears(o: Orientation, arc: Arc, toward_end: LaneEnd) -> bool` (geometry.rs:117)

**Mirrors:** `engine/geometry.ts:74 — function arcBears(o, arc, towardEnd)`.
**Intent:** Does a mount with this arc currently aim at something lying toward
`toward_end`, given the ship's orientation? This is the *offensive* counterpart to
`facing_zone`: where `facing_zone` decides which shield face a hit lands on,
`arc_bears` decides whether a weapon can fire at all. The gate that makes facing a
turn-by-turn decision.

Line 118: `match arc` — dispatch on the four arc variants. Each variant has different
rules; no shared logic.

Line 119: `Arc::Turret => true`. Turrets always bear. The turret is the "no facing
constraint" mount.

Line 120: `Arc::Forward` — `matches!(o, Orientation::BowOn { bow } if toward_end == bow)`.
The match-guard pattern lets us destructure `BowOn` and check the bow direction in one
expression. A forward gun only fires when the ship is bow-on **and** the bow points at
the target's direction. Returns `false` for any broadside-stance ship.

Line 121: `Arc::Rear` — same pattern, but checks `toward_end == opposite(bow)`. A rear
gun only fires astern when bow-on. (Equivalent to: the ship's stern points at the
target's direction.)

Lines 122–124: `Arc::BroadsideArc => matches!(o, Orientation::Broadside)`. Broadside
batteries fire only when the hull is turned across the lane, and they fire both ways
at once. The `toward_end` parameter is *ignored* in this arm — the caller checks both
ends separately to enumerate the two cells a broadside battery hits. The comment on
lines 122–123 makes this explicit.

**Drift note: `matches!` macro.** TS uses an if/else inside a switch; Rust uses the
`matches!(value, pattern)` macro which generates the same machine code but reads as a
boolean expression. The match-guard syntax (`if toward_end == bow`) is unique to Rust
and is the cleanest way to combine destructuring with a predicate.

**Cross-references:**
- Tested exhaustively at `geometry.rs:255` (turret always), `:263` (forward only out
  the bow), `:272` (rear only astern), `:279` (broadside only when broadside).
- Called by `bears` (`geometry.rs:131`), which is the higher-level "given a target
  cell" wrapper.
- The `BroadsideArc` arm's "both ways at once" semantics are realized in
  `resolve_targeting`'s `BROADSIDE` pattern (future, `resolve.rs`).

---

### `fn bears(ship: &Ship, arc: Option<Arc>, target_cell: usize) -> bool` (geometry.rs:131)

**Mirrors:** `engine/geometry.ts:90 — function bears(ship, arc, targetCell)`.
**Intent:** Convenience wrapper over `arc_bears` that takes a ship and a target cell
(rather than a raw direction) and accepts `None` for arc-less actions like `SELF` or
`DEPLOYED_CELL`. The resolver almost never calls `arc_bears` directly — `bears` is the
public surface.

Line 132: `match arc`.

Line 133: `None => true`. Arc-less actions always resolve. Per the TS comment,
"`SELF` / arc-less actions always resolve" — the contract is documented at the
function level.

Line 134: `Some(a) => arc_bears(ship.orientation, a, direction_to(ship.cell, target_cell))`.
Compute the direction from the ship to the target, then delegate. `ship.orientation` is
`Copy`, so no borrow gymnastics.

**Drift note: `Option<Arc>` vs TS `Arc | null`.** The TS uses a nullable union; Rust
uses `Option<Arc>`. Semantically identical; idiomatic in both languages.

**Cross-references:** Will be called by `resolve_targeting` for every action that
declares an arc requirement. Currently unused outside tests because the resolver hasn't
landed.

---

### `fn absorb_shield(face: &mut ShieldFace, dmg: i32) -> i32` (geometry.rs:144)

**Mirrors:** `engine/geometry.ts:101 — function absorbShield(face, dmg)`.
**Design anchor:** HTML Part IV (directional shields).
**Intent:** Run incoming damage through one hull zone's defence. Step 4 of the
five-step damage pipeline. A held shield `charge` negates the hit entirely and is
consumed (one-for-one); otherwise the zone's permanent `armour` is subtracted. Returns
the damage that reaches hull. **Mutates `face` in place** — charge consumption is a
side effect.

Lines 145–147: `if dmg <= 0 { return 0; }`. Early return for zero or negative damage.
The test at `:309` confirms a `dmg == 0` call does **not** consume the charge — this is
the contract that lets future no-damage status effects pass through `absorb_shield`
without accidentally burning shields.

Lines 148–151: charge path. If `face.charge > 0`, decrement it and return zero.
**Consumes exactly one charge per hit**, regardless of damage magnitude — a 1-charge
shield absorbs a 10-damage railgun shot completely. This is the strong-but-finite
shield model from the design doc.

Line 152: armour path. `(dmg - face.armour).max(0)` — subtract armour, floor at zero.
Armour is *not* consumed; it's permanent zone armour. The test at `:294` confirms
`face.armour` is unchanged after the call.

**Drift note: `&mut ShieldFace` signature.** TS mutates via JavaScript reference
semantics (objects are heap-allocated and passed by reference implicitly). Rust requires
the explicit `&mut`. Callers in the resolver will need disjoint borrows to
`board.cells[i].shield_profile[&zone]` — the `HashMap` access will need to go through
`get_mut(&zone)` and the resolver must hold the `&mut Ship` only as long as needed.
Architect to handle this when `resolve.rs` lands; flagging here as a future-care item.

**Cross-references:**
- Tested at `:287` (charge negates and decrements), `:295` (armour subtracts and
  doesn't consume itself), `:304` (clamps when armour > damage), `:309` (zero damage
  doesn't burn charge).
- Called by `apply_damage` (resolve.rs, future) as step 4 of the damage pipeline. The
  caller looks up `face` via `target.shield_profile.get_mut(&zone)` where `zone` came
  from `facing_zone`.

---

### `fn default_shield_profile() -> HashMap<HullZone, ShieldFace>` (geometry.rs:160)

**Mirrors:** `engine/geometry.ts:112 — function defaultShieldProfile()`.
**Intent:** The starting Frigate's hull layout. Strong bow (armour 2), weak stern
(armour 0), medium flanks (armour 1, 1). Zero charge on every face — charges are
granted at runtime by Brace and similar defensive actions, never baked into the
profile. Used by `demo.rs` (future) and by tests as a known-good baseline.

Line 161: pre-allocate a `HashMap` with capacity 4 — exactly four hull zones, no need
to grow.

Lines 162–165: insert one `ShieldFace` per zone. The numeric values match the TS
`defaultShieldProfile` byte-for-byte.

Line 166: return the map.

**Drift note: ShieldProfile representation.** The TS uses
`Record<HullZone, ShieldFace>` — a TS object keyed by a string union. The Rust port
uses `HashMap<HullZone, ShieldFace>`. The earlier watch-list item asked whether the
Rust port would switch to `[ShieldFace; 4]` indexed by `HullZone` (which would be
faster and avoid the small hash overhead on every shield lookup). Architect chose
`HashMap` to keep the JSON wire shape symmetric with the TS — the catalog's
`shieldProfile` field deserialises straight into a HashMap with no custom adapter.
The HullZone enum derives `Eq + Hash`, so this works at the type level. **Drift watch
list item resolved.**

**Cross-references:**
- Tested at `:317`. The test asserts all four keys present and exactly the right
  armour/charge values, plus `len() == 4` to catch accidental extra keys.
- Will be used by integration tests when `tests/resolve.rs` ports demo.ts scenarios.

---

### `#[cfg(test)] mod tests` (geometry.rs:174–326)

19 unit tests, one per public function (plus extra cases for the multi-arm functions).
Each test name reads as a sentence asserting the property; this is gold for the doc —
the test names are documentation. Notable cases:

- **`direction_to_treats_equal_cells_as_fore`** (`:185`) — pins the `>=` behaviour.
  The TS comment in the original calls this out as "easy to miss."
- **`band_falloff_floors_negative_inputs_at_zero`** (`:225`) — the `.max(0)` clamp.
- **`absorb_shield_ignores_non_positive_damage`** (`:309`) — the no-consume-on-zero
  contract.
- **`absorb_shield_clamps_when_armour_exceeds_damage`** (`:303`) — overarmour case.

These tests will get pulled into worked examples for the resolver's `apply_damage`
walkthrough when `resolve.rs` lands. For now they stand as the executable contract for
each function above.

---

### Drift watch list (resolved by `d383c6a`)

- ~~**`Math.floor(raw * factor)` — float vs fixed-point.**~~ Architect kept f64. Cross-
  platform determinism is not a current requirement; can be changed later without
  signature impact.
- ~~**Mutation of `ShieldFace.charge`.**~~ Architect used `&mut ShieldFace`; resolver
  borrowing pattern is a future-care item to flag when `resolve.rs` lands.
- ~~**`Record<HullZone, ShieldFace>` representation.**~~ Architect kept `HashMap` for
  JSON wire-shape symmetry. Performance is acceptable; can be switched to
  `[ShieldFace; 4]` later if profiling justifies it.

No new drift introduced by this commit.

---

## `src/resolve.rs`

*The combat resolver. One execution path serves player, enemy, and ordnance. The
four-phase round, the arc/heat/cooldown gate in queue execution, the full damage
pipeline, ordnance advancement, and end-of-turn ticking all live here. Per the analysis
HTML this is the engine's most load-bearing file.*

**Mirrors:** `engine/resolve.ts`.
**Design anchor:** HTML Part I (Core Loop), Part XIII (Engine Integration).

### Functions to document

Grouped by the banner comments in the TS source.

#### The round
- **`fn resolve_round(board: &mut Board, content: &Content)`** — the four-phase entry
  point. *Mirrors `resolve.ts:31`.*

#### Queue execution
- **`fn execute_queue(ship: &mut Ship, board: &mut Board, content: &Content)`** — the
  arc + heat + cooldown gate. *Mirrors `resolve.ts:53`.*
  *This is the single most read function in the engine; the walkthrough needs to be
  thorough. Cover: lookup, lockout check, cooldown check, targeting, arc-bore check,
  effect application, heat tick, lockout transition, cooldown reset, event emit,
  chain detection, queue clear.*

#### Targeting
- **`fn resolve_targeting(a: &Action, board: &Board, ship: &Ship) -> Vec<usize>`** —
  the eight-pattern dispatch. *Mirrors `resolve.ts:81`.*
  *Document each arm separately as a sub-entry: `SELF`, `BROADSIDE`, `BEAM`/
  `POINT_BLANK`, `SPINAL_LINE`, `BLAST`, `DEPLOYED_CELL`/`ORDNANCE`. Worked example
  per pattern from the test suite.*

#### Damage pipeline
- **`fn apply_damage(target: &mut Ship, raw: i32, atk_cell: usize, weapon: &Action,
  board: &mut Board)`** — the five-step pipeline. *Mirrors `resolve.ts:139`.*
  *Walk the five steps in order: falloff, modifiers, target-lock, directional shield,
  hull. This is THE reference for "where do new balance levers go."*

#### Effect dispatch
- **`fn apply_effect(fx: &Effect, a: &Action, source: &mut Ship, cells: &[usize],
  board: &mut Board, content: &Content)`** — the closed match on effect kinds.
  *Mirrors `resolve.ts:167`.*
  *Each arm gets its own sub-entry: `DAMAGE`, `APPLY_STATUS`, `VENT_HEAT`, `REORIENT`,
  `SPAWN_ORDNANCE`, `DISPLACE_SELF`, `DISPLACE_TARGET`, `DEPLOY`, `BOARD`.*

#### Ordnance
- **`fn advance_projectile(p: &mut Projectile, board: &mut Board, content: &Content)`** —
  step a projectile by its speed, resolve impact. *Mirrors `resolve.ts:233`.*

#### End of turn
- **`fn end_of_turn(board: &mut Board, content: &Content)`** — cooldown tick, heat
  dissipation, lockout clear, status tick, `onTurnEnd` emit. *Mirrors `resolve.ts:254`.*

#### Helpers
- `ships_of`, `enemy_initiative`, `bearing_direction`, `cells_toward`,
  `first_target_toward`, `in_allowed_band`, `add_status`, `tick_statuses`, `skips_turn`,
  `destroy`, `detect_chain`, `flip_orientation`, `remove_projectile`, `dummy_weapon`.
  *Each gets a short entry — they are small, but the cross-references between them
  matter (e.g. `destroy` is the one place `ReactorBreach` splash damage lives).*

### Drift watch list

- **`Content` struct shape** — TS uses `Record<string, Action>` for the action lookup.
  Rust will likely use `HashMap<String, Action>` or `HashMap<ActionId, Action>` with a
  newtype. Watch for the borrowing implications: `apply_effect` reaches back into
  `content` for `spawn_projectile`.
- **Mutable board passing** — TS mutates `board.cells`, `board.ordnance`,
  `board.hazards` freely. Rust will encounter borrow conflicts; expect either interior
  mutability (`RefCell`) or restructuring the function signatures to pass disjoint
  borrows. Document whatever the architect picks.
- **`detect_chain` is a TODO** — TS leaves the chain-kill counter as a stub returning
  `false`. The Rust port should still wire the call site so subsystems hooking
  `onChainKill` can be tested.

*Per-line walkthroughs pending `src/resolve.rs`.*

---

## `src/effects.rs`

*The bodies the TS source leaves as TODO comments inside `resolve.ts`: full movement
mode implementations (`THRUST`, `BURN`, `SLIP`, `JUMP`, `TRACTOR_SWAP` with occupancy
and collision rules), push/pull/swap with collision damage, and subsystem damage
modifier math.*

**Mirrors:** TS keeps these inside `resolve.ts` as stubs (lines 371-393). The Rust port
may split them into their own module for readability.

### Functions to document

- **`fn apply_modifiers(dmg: i32, target: &Ship, band: RangeBand, board: &Board) -> i32`** —
  sum of subsystem damage bonuses. *Mirrors `resolve.ts:371` (stub).*
- **`fn resolve_self_move(ship: &mut Ship, mode: MovementMode, distance: i32,
  board: &mut Board)`** — full path rules per mode. *Mirrors `resolve.ts:376` (partial
  THRUST/BURN only).*
- **`fn resolve_target_move(target: &mut Ship, mode: DisplaceMode, distance: i32,
  board: &mut Board)`** — push/pull/swap with collision. *Mirrors `resolve.ts:390`
  (stub).*

### Drift watch list

- **Module split** — if architect keeps these in `resolve.rs`, this section gets folded
  into that file's entry. If they split into `effects.rs`, this stays.

*Per-line walkthroughs pending Rust implementation.*

---

## `src/content.rs`

*The runtime content layer: loads the catalog JSON, builds the `Action` lookup table,
implements `spawn_projectile` table-driven dispatch from the `kind` string.*

**Mirrors:** TS does this inline in `demo.ts`. The Rust port will have a dedicated
module.

### Functions to document

- **`fn load_catalog(path: &Path) -> Result<Catalog, ContentError>`** — read the JSON,
  validate, return the typed record.
- **`fn build_content(catalog: &Catalog) -> Content`** — index actions by ID, build the
  projectile spawn closure / table.
- **`fn spawn_projectile(kind: &str, owner: &Ship, board: &Board) -> Projectile`** —
  look up the projectile template by kind, instantiate at the owner's cell, set
  heading from the owner's orientation. *Mirrors `demo.ts:37`.*

*Per-line walkthroughs pending Rust implementation.*

---

## `src/ai.rs`

*The enemy decision layer. Picks actions to queue, then the same `execute_queue` runs
them. The design objective: maximize the number of distinct lane-ends threatened, which
is what manufactures the rotation pressure the orientation system depends on.*

**Mirrors:** `resolve.ts:395` (`decideEnemyAction`, stubbed).
**Design anchor:** HTML Part IV (closing paragraph on AI's flanking objective).

### Functions to document

- **`fn decide_enemy_action(enemy: &mut Ship, board: &Board, content: &Content)`** —
  the entry point. Fills `enemy.queue` based on threat analysis.
- *(Helpers TBD as the AI evolves: threat scoring, lane-end coverage analysis, mount
  utility per orientation, etc.)*

*Per-line walkthroughs pending Rust implementation. AI is task #6 in the team queue.*

---

## `src/bus.rs`

*The event bus and the `Hook` enum. Synchronous fan-out: `emit(hook, ctx)` walks the
subscriber list for that hook and calls each in registration order.*

**Mirrors:** `engine/types.ts:191` (`EventBus`) and `demo.ts:16` (`makeBus`).

### Items to document

- **`enum Hook`** — the closed set of 11 hook tags.
- **`struct HookContext<'a>`** — board reference, optional source/target, optional
  amount, and the extension fields the TS `[k: string]: unknown` allows.
- **`trait EventBus`** *or* **`struct EventBus`** — the `on` / `emit` interface.
  *Drift watch: TS uses a plain object with closures. Rust must decide between
  `Box<dyn Fn(&mut HookContext)>` subscribers (boxed closures) vs. a trait-based
  subscriber type. Architect's call; document whichever.*

*Per-line walkthroughs pending Rust implementation.*

---

## `src/catalog.rs`

*The on-disk catalog format: deserializes `broadside.catalog.json` (the export from the
analysis HTML) into the typed `Catalog` record. Currently a stub at 214 bytes — a
placeholder module.*

**Mirrors:** No direct TS analog; this is Rust-specific glue.

*Pending the architect expanding the stub.*

---

## `src/gfx/`

*The wgpu renderer subtree. Owns its own state; reads `Board` and `Ship` for layout;
subscribes to the event bus for damage / kill / vent / reorient animations.*

**Mirrors:** No TS analog — the TS reference is headless.
**Design anchor:** No HTML section; this is a Rust-port concern.

### Submodules to document (target)

- `src/gfx/mod.rs` — module root, renderer entry point.
- `src/gfx/pipeline.rs` — wgpu pipeline, shaders, vertex/index buffers.
- `src/gfx/atlas.rs` — sprite atlas packing and UV lookup by string ID.
- `src/gfx/hud.rs` — queue display, heat bar, cooldown pips, initiative badges,
  range-band ruler.
- `src/gfx/anim.rs` — event-driven animation queue (damage numbers, kill flashes,
  ordnance trails).

*Per-line walkthroughs pending implementation. Renderer is task #7 in the team queue.*

---

## `tests/`

*Integration tests that double as worked examples. The doc embeds the readable ones —
they are the gold-standard demonstrations of "what the engine does in scenario X."*

**Mirrors:** `demo.ts` is the only TS test scaffolding; the Rust port will have a real
test suite. Scenario A and B from `demo.ts` (orientation alone changing damage outcome)
are the first two tests to land. *Tracking under task #5.*

### Test files to document

- `tests/geometry.rs` — band falloff table, facing zone exhaustive cases, arc bearing
  exhaustive cases, shield absorption (charge consumption, armour subtraction, zero
  damage).
- `tests/resolve.rs` — the four-phase round end-to-end on small boards. Demo scenario A
  (weak stern, full damage gets through) and scenario B (strong bow, 2 reduced) ported
  as deterministic assertions.
- `tests/effects.rs` — each movement mode against boundary, occupancy, multi-step paths.
- `tests/projectiles.rs` — torpedo across a 7-cell lane, dodge, point-defense kill.

*Test entries get a "Worked example" block embedded into the relevant function
walkthrough — when `apply_damage` is documented, the demo.ts scenario A trace lives
right there.*

---

## Maintenance protocol

This document is updated by the Analysis Writer (`broadside-doc-writer`) within one
session of any source change. When a teammate's PR lands:

1. Re-read the changed Rust file end to end.
2. Update the affected function's intent paragraph if the function's *purpose* changed.
3. Replace the per-line walkthrough with the new content.
4. Update line-number citations throughout the section.
5. Add a **Drift** note if the change deviated from the TS reference in a way not
   previously documented.
6. If a function was added/removed, update the file's section heading list.
7. If a worked example became wrong, fix it; if a test changed, update the embedded
   trace.

Stale line citations are the most common rot pattern. Function names are the stable
anchor; line numbers are a courtesy to the reader.
