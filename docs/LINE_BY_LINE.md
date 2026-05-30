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
- [`src/perspective.rs`](#srcperspectivers) — screen-space projection: lane trapezoid, cell positions, ship sprite vertices
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

*Every TS interface and type alias as a Rust struct or enum, plus a working `EventBus`.
The TS calls this surface "pure types — no logic," and the Rust file's module rustdoc
echoes that (`types.rs:1`), but the Rust port actually carries real implementation in
two places: `ShieldProfile`'s `face`/`face_mut` (and the `Index`/`IndexMut` impls), and
the whole `EventBus` — `Default`, `on`, `emit`, plus the take-and-replace re-entrancy
machinery. Treat the file as "the data the resolver operates on, plus the smallest
amount of glue that data needs to be usable."*

**Mirrors:** `engine/types.ts` (the entire file).
**Design anchor:** HTML Part XIII (Engine Integration & Schema) — the "Type definitions"
codeblock is the canonical schema this file ports.
**Source commit:** `5625f30` (initial port) + `291206d` (reviewer audit response —
`ShieldProfile` struct, `HookContext` H2 cell-indices, `EventBus` take/replace,
`unlock_salvage` null-roundtrip, `band_falloff` predicate semantics). All 12 tests in
`#[cfg(test)] mod tests` pass.

### Module rustdoc (lines 1–53)

The 53-line module header is the single most important block in the file. Six sections:

- **Lines 1–6:** intent + the tie-breaker. "The TypeScript engine is the canonical
  reference; when this port and the TS disagree, the TS is right." Cite this whenever a
  drift question turns into a judgment call.
- **Lines 7–18:** the eight-section layout, matching the TS banner comments. Geometry
  → Board → Ship → Action → Effects → Ordnance → Subsystems/bus → Catalog. Same order,
  same sub-types per section.
- **Lines 20–33:** serde conventions. Three rules: `Orientation` is tagged on `stance`
  with camelCase; `Effect` is tagged on `kind` with variants preserved in
  `SCREAMING_SNAKE_CASE`; other enums get `rename_all = "camelCase"`; `Hook` variants
  are camelCase `"onFoo"` event names. The on-the-wire JSON shape must round-trip
  byte-for-byte with the design-doc's "Copy JSON" output.
- **Lines 35–41:** runtime vs catalog split. `Board` is *not* serde (holds the
  `EventBus` and `destroys_this_window`). `SubsystemDef` is the catalog half; the
  runtime `Subsystem` (with its callback) lives next to the content slice.
- **Lines 43–53:** numeric mappings. TS `number` → `i32` for game quantities that go
  negative mid-calc (hull, damage, cooldown, armour); `usize` for cell indices;
  `u32` for non-negative counts; `u8` for patrol tier 1..=7. Read this section before
  writing any numeric assertion against the engine.

Line 55: `use std::collections::HashMap;` — used by `Ship::cooldowns` (action-id to
turns remaining) and `HookContext::extras` (the `[k: string]: unknown` overflow).
Notably **not** used by `ShieldProfile` any more — that's now a named-field struct
(see Drift section).

Line 57: `use serde::{Deserialize, Serialize};` — every catalog-bound type derives both.

---

### Section 1: Geometry primitives (lines 59–117)

Six enums, all `Copy + Eq + Hash`, all deriving serde. The vocabulary every other type
in the file consumes.

#### `enum LaneEnd { Fore, Aft }` (types.rs:66)

**Mirrors:** `types.ts:10`.
**Intent:** The two lane directions. `Fore` = toward higher cell index. The single
named enum that turns every "which way?" question into a typed value.

Line 64: derive block. `Copy + Clone + Debug + PartialEq + Eq + Hash + Serialize +
Deserialize`. `Hash` is used by `HashMap` keys; `Copy` lets geometry functions take it
by value with no borrow concerns.
Line 65: `#[serde(rename_all = "camelCase")]` — variants serialize as `"fore"` /
`"aft"`, matching the TS string union.

#### `enum Orientation { BowOn { bow: LaneEnd }, Broadside }` (types.rs:76)

**Mirrors:** `types.ts:14`.
**Design anchor:** HTML Part IV — orientation as the primary tactical axis.
**Intent:** Hull stance. `BowOn` carries the bow direction as data so flipping is a
field change; `Broadside` has no payload. The serde tag `stance` (line 75) means JSON
`{ "stance": "bowOn", "bow": "fore" }` and `{ "stance": "broadside" }` both parse —
the canonical example pinned by `orientation_roundtrips_through_ts_shape` at
`types.rs:813`.

**Drift note: the tag name.** TS uses a discriminated union with `stance` as the tag.
Rust uses `#[serde(tag = "stance", rename_all = "camelCase")]` to produce the identical
wire shape. No structural drift.

#### `enum HullZone { Bow, Stern, Port, Starboard }` (types.rs:84)

**Mirrors:** `types.ts:19`.
**Intent:** The four fixed armour faces welded to the hull. Strong bow, weak stern,
medium flanks. Serializes as `"bow"`, `"stern"`, `"port"`, `"starboard"` — and is the
*key type* for `ShieldProfile::Index` (line 258), so `sp[HullZone::Bow]` is
ergonomic. Derives `Hash` so it can also be a `HashMap` key (the `default_shield_profile`
return type was a `HashMap<HullZone, ShieldFace>` pre-#10; the field on `Ship` is now
`ShieldProfile`).

#### `enum RangeBand { PointBlank, Close, Mid, Long, Extreme }` (types.rs:94)

**Mirrors:** `types.ts:22`.
**Intent:** The five distance buckets. **Declaration order is load-bearing** —
`geometry::band_index` (`geometry.rs:38`) is an exhaustive `match` returning the
position index for each variant; adding a `RangeBand` variant without updating
`band_index` fails to compile. The exhaustive match is the drift guard (post-audit
`21561f1`). Tested at `range_band_serializes_camel_case` (`types.rs:850`).

#### `enum Arc { Forward, BroadsideArc, Turret, Rear }` (types.rs:105)

**Mirrors:** `types.ts:25`.
**Intent:** A mount's firing window relative to the *hull*. The arc gate inside
`geometry::arc_bears` switches on this enum.

**Drift note: `BroadsideArc` name.** The TS uses `"broadsideArc"` (camelCase) for the
literal; the Rust enum variant is `BroadsideArc` (TitleCase) which serializes to
`"broadsideArc"` via `rename_all = "camelCase"`. Round-trip is exact.

#### `enum Faction { Player, Enemy }` (types.rs:114)

**Mirrors:** `types.ts:27`.
**Intent:** Player vs. enemy. Used by `Projectile::owner_faction` (to decide who a
torpedo can hit) and by enemy initiative filtering in the future resolver.

---

### Section 2: Board (lines 119–168)

#### `struct Board` (types.rs:132)

**Mirrors:** `types.ts:31`.
**Design anchor:** HTML Part I — the lane is the board.
**Intent:** The 1-D battlefield. Live runtime state — holds the event bus and a
chain-kill window counter. **Intentionally not serde-derived** (line 123–125): `Board`
is the runtime collaborator, not catalog data. A scenario file describes ships and
hazards; the engine builds a `Board` from them and runs it.

Fields:
- `size: usize` — lane length, TS uses 5/7/9.
- `cells: Vec<Option<Ship>>` — sparse occupancy; `None` for empty cells. Index = cell
  position, matching `Ship::cell` (a redundancy by design: every Ship carries its own
  cell so functions that own a `&Ship` don't need to scan the board to find it).
- `ordnance: Vec<Projectile>` — live torpedoes/missiles. Flat list; not per-cell.
- `hazards: Vec<Vec<Hazard>>` — per-cell hazard lists. Outer index matches `cells`.
- `patrol: u8` — global difficulty tier 1..=7.
- `bus: EventBus` — embedded. The resolver `mem::take`s this off `Board` before
  invoking subscribers so a callback's `&mut Board` doesn't alias the bus. See the
  `EventBus` walkthrough below and the `emit_fires_subscribers_in_registration_order`
  test at `types.rs:1002`.
- `destroys_this_window: usize` — **new vs TS**. The chain-kill counter. See **Drift
  note: addition** below.

**Drift note: addition of `destroys_this_window`.** The TS `Board` has no such field
(see `types.ts:31`); chain-kill detection in `resolve.ts:346` is a stubbed
`detectChain()` returning `false`. The Rust port adds an explicit counter on the
`Board`, per team coordination. Semantics (per the doc comment, lines 127–131): the
resolver increments on each destroy and resets at well-defined window boundaries
(start of `executeQueue` and start of the ordnance phase). Two or more destroys in one
window triggers `onChainKill`. **Reset semantics live in the resolver, not in this
file** — the field is a plain `usize`, not encapsulated, by intent.

**Drift note: `bus: EventBus` embedded.** Earlier drift watch list flagged the
borrow-conflict risk of putting `bus` directly on `Board`. The architect chose
embedded-plus-`mem::take`, not externalized — the resolver's emit pattern lifts the
bus off the board, invokes subscribers (which now have free `&mut Board`), and puts the
bus back. See `EventBus::Default` impl (line 651) which makes `mem::take` legal —
that's the impl `mem::take` rests on. The trade-off: brief moments where
`board.bus.subscribers.iter().any(...)` from outside the emit path would see an empty
list. Not a problem in practice because nothing reads the bus outside emit.

#### `struct Hazard` (types.rs:152)

**Mirrors:** `types.ts:40`.
**Intent:** A cell-resident feature — mine, drone, or debris field — that applies its
`payload` to anything entering the cell.

Fields: `id`, `kind` (the three-variant `HazardKind`), `cell`, `payload: Vec<Effect>`,
`ttl: Option<u32>`. The `#[serde(default, skip_serializing_if = "Option::is_none")]`
on `ttl` (line 158) matches the TS `ttl?: number` optional field — omitted in JSON
when `None`, parsed as `None` when absent. **Not** the same as
`SubsystemDef::unlock_salvage`, which is `number | null` and must serialize *as null*
(see **Drift note: `unlock_salvage` null handling** below).

#### `enum HazardKind { Mine, Drone, Debris }` (types.rs:164)

**Mirrors:** `types.ts:42`.
**Intent:** Hazard variants. Distinct from `DeployHazardKind` at line 485, which is the
*action-effect* subset (mines and drones only — `DEPLOY` cannot produce debris).

---

### Section 3: Ship (lines 170–316)

#### `struct Ship` (types.rs:177)

**Mirrors:** `types.ts:50`.
**Intent:** Player and enemy ships share this shape; `faction` distinguishes. Everything
the resolver needs to apply an action against a unit lives on this struct.

The walkthrough goes field by field:

- **`id: String`** — stable identifier; used by the `onLethal`/`onDamageTaken` payload
  consumers to look up which ship took the hit when the cell may have been vacated.
- **`faction: Faction`** — used by `Projectile`'s ownership check.
- **`cell: usize`** — lane position. The redundancy with `Board::cells[cell] ==
  Some(self)` is intentional; the resolver passes `&Ship` to many helpers that need to
  know cell position without scanning.
- **`orientation: Orientation`** — stance + bow direction. The primary input to every
  `geometry::*` query.
- **`hull: i32` / `max_hull: i32`** — current/max HP. `i32` because mid-calc the
  resolver computes `target.hull -= dmg`, then compares `<= 0`; momentary negative is
  fine.
- **`heat: i32` / `heat_max: i32`** — heat pool / lockout threshold. Crossing
  `heat_max` is what sets `locked_out`.
- **`locked_out: bool`** — overheat state. Cleared by `VENT_HEAT` or by end-of-turn
  passive dissipation dropping `heat < heat_max`.
- **`shield_profile: ShieldProfile`** — the four-zone defensive layout. Named-field
  struct, not a HashMap (see Drift section below).
- **`mounts: Vec<Mount>`** — weapon hardpoints, fixed at ship-design time.
- **`queue: Vec<String>`** — action ids the player loaded; fires bottom-up on
  execute.
- **`cooldowns: HashMap<String, i32>`** — action-id to turns remaining. `HashMap` is the
  right fit here because the key space is the catalog's action ids (variable, string-
  keyed) and cardinality is small but unbounded.
- **`statuses: Vec<Status>`** — active transient modifiers.
- **`traits: Vec<Trait>`** — intrinsic enemy modifiers.
- **`klass: Option<String>`** — optional class id; dispatches the Signature action.

Each `#[serde(rename = "camelCaseName")]` annotation maps a snake_case Rust field to
its TS-compatible JSON key. Examples on this struct: `maxHull`, `heatMax`, `lockedOut`,
`shieldProfile`.

**Drift note: `klass` kept as `klass`.** TS uses `klass` to dodge the JS reserved word
`class`. Rust has no such conflict, but the architect kept `klass` for *cross-port
identifier parity* — `grep -r 'klass' src/` and `grep -r 'klass' _drive_pull/` return
matched sets. Drift-watch item #1 from my pre-port list is **resolved by keeping the
TS name**. If anyone changes this later, the renames must happen in both `types.rs`
and any consuming files in lockstep; the `#[serde(default, skip_serializing_if =
"Option::is_none")]` on line 209 mirrors the TS `klass?:` optional shape.

**Worked example (`ship_roundtrips_with_pulse_laser_demo_shape`, types.rs:863):** The
demo.ts player frigate ported verbatim. Hull 10/10, heat 0/6, bow-on facing fore,
default shield profile (2/0/1/1), one forward Pulse Laser mount, the laser queued.
Serializes to JSON, deserializes back, asserts equality. The single most important
parity test in the file — if it ever breaks, the demo can't run.

#### `struct ShieldFace { armour: i32, charge: i32 }` (types.rs:217)

**Mirrors:** `types.ts:71`.
**Intent:** One hull zone's defence. `armour` is permanent directional reduction;
`charge` is consumable shield "pings" from Brace etc. Documented behaviour at
`geometry::absorb_shield`.

#### `struct ShieldProfile { bow, stern, port, starboard }` + impls (types.rs:228–265)

**Mirrors:** `types.ts:60` (`Record<HullZone, ShieldFace>`).
**Intent:** The four-zone defensive layout. *Required* completeness — every zone must
be present. The `Index`/`IndexMut` impls (lines 258 and 263) make `sp[HullZone::Bow]`
the idiomatic access pattern; `face(zone)` / `face_mut(zone)` exist too for callers
that prefer explicit method calls.

**Drift note: ShieldProfile is a named-field struct, not `HashMap`.** This is the
**single most important port decision** in the audit-response delta from commit
`291206d` (reviewer issue M1). Pre-audit, `Ship::shield_profile` was
`HashMap<HullZone, ShieldFace>`, which round-tripped the same JSON object shape but
allowed a catalog with three of the four keys to parse silently — the resolver's later
`HashMap::get(&zone).unwrap()` would then panic deep inside a damage application.
Reviewer flagged this as Major issue 1 (M1). Architect's fix: replace the HashMap with
a `struct ShieldProfile { bow, stern, port, starboard }` whose `Deserialize`
implementation is *total* — any missing key fails at parse, before the engine ever
touches the ship.

This **supersedes** the drift-watch resolution recorded in `b1bf47c`'s
`geometry.rs:160` walkthrough, which said "kept as HashMap." That resolution was
correct at the time of writing (the `default_shield_profile` function still returned
a `HashMap`); the resolver-adjacent rework will land a follow-up commit changing
`geometry::default_shield_profile` to return `ShieldProfile`, at which point the
geometry walkthrough's drift note becomes accurate again. **Bundled fix pending the
resolver geometry commit** (per team-lead direction).

Field order — `bow, stern, port, starboard` — matches the JSON object key order
emitted by the analysis HTML's "Copy JSON" button. Tested at
`shield_profile_rejects_missing_zone` (line 892) and `shield_profile_index_mut_decrements_charge`
(line 908).

#### `struct Mount { id, arc, weapon }` (types.rs:268)

**Mirrors:** `types.ts:76`.
**Intent:** A weapon hardpoint. `weapon` is an action id (the catalog key the resolver
uses to look up the Action). Mounts are fixed at ship-design time — the resolver never
mutates them.

#### `struct Status { kind, duration, face? }` (types.rs:276)

**Mirrors:** `types.ts:82`.
**Intent:** A transient unit modifier. The `face: Option<HullZone>` (line 286) is
documented as **dead weight pending confirmation** — present in the TS interface but
the TS resolver does not read it; `ShieldsUp` is tracked via `ShieldFace::charge`
instead. Mirrored for catalog-shape parity; flag for removal if content / resolver
confirms no plans to use it. (Watch list item for me on future passes.)

#### `enum StatusKind { HullBreach, SystemsOffline, TargetLock, ShieldsUp }` (types.rs:291)

**Mirrors:** `types.ts:88`.
**Intent:** The four transient ship modifiers. Each variant has a doc comment
identifying its origin analog: HullBreach = poison/DoT, SystemsOffline = frozen/skip,
TargetLock = curse/double-next-hit, ShieldsUp = held-charge. Serialized as camelCase
(`"hullBreach"`, `"systemsOffline"`, `"targetLock"`, `"shieldsUp"`).

#### `enum Trait` (types.rs:305)

**Mirrors:** `types.ts:94`.
**Intent:** Enemy traits — base layer (`Pursuit`, `Agile`, `ReactorBreach`, `BurnHard`,
`Anchored`) plus the Patrol-2+ Elite layer (`EliteAgile`, `EliteAnchored`,
`TwinLinked`, `ReactiveShield`, `Voidtouched`).

**Drift note: no `rename_all` here.** The TS uses TitleCase string literals
(`"Pursuit"`, `"BurnHard"`, etc.), so the Rust variant names already match the wire
shape — no rename attribute needed. The single enum in the file where the default
serialization happens to be correct without intervention.

---

### Section 4: Action (lines 318–396)

#### `struct Action` (types.rs:325)

**Mirrors:** `types.ts:100`.
**Intent:** The universal verb. Every weapon, system, maneuver, ordnance launch, and
vent is one of these. Lookups happen by `id` through the catalog.

Fields: `id`, `name`, `archetype`, `cost`, `targeting`, `effects: Vec<Effect>`,
`r#mod: Option<String>` (raw identifier for the reserved keyword `mod` — the TS field
name is just `mod`, no escape needed; Rust requires `r#mod` to use the keyword as an
identifier), `icon: Option<String>`.

**Drift note: `r#mod` raw identifier.** Rust's `mod` is reserved; the field name on
the wire is `"mod"` (the TS field) but accessed in Rust as `action.r#mod`. The
`Serialize`/`Deserialize` derive emits/parses the JSON key as `"mod"` by default (no
rename attribute needed — serde understands `r#` prefixes). Watch list item: any
future field accessor for this needs the `r#` prefix; clippy will not catch it.

#### `struct ActionCost { heat, cooldown_max, advances_turn }` (types.rs:341)

**Mirrors:** `types.ts:111`.
**Intent:** The cost gates. `heat` adds to the ship's heat pool on fire; `cooldown_max`
becomes `cooldowns[id]` on fire and ticks down; `advances_turn = false` is the free-fire
flag (Vent, maneuvers, Autoloader).

JSON wire shape uses camelCase (`cooldownMax`, `advancesTurn`); the `rename` attributes
on lines 343 and 346 translate.

#### `struct Targeting { pattern, band, optimal_band, requires_arc, facing_relative, hits_all }` (types.rs:351)

**Mirrors:** `types.ts:117`.
**Intent:** The cell-selection rules. `pattern` is the dispatch over eight branches;
`band: Vec<RangeBand>` is the allowed band list; `optimal_band` is the peak-damage
band; `requires_arc: Option<Arc>` is the mount-bear constraint; `facing_relative` is
TS legacy not currently used by the resolver; `hits_all` distinguishes
SPINAL_LINE-pierce from SPINAL_LINE-first.

**Drift note: `Eq + Hash` derives.** Unlike most structs in this file, `Targeting`
derives `Eq + Hash` (line 350) — useful for memoizing targeting results or using
targeting shape as a HashMap key. No callers use it that way today, but the derives
are cheap.

#### `enum WeaponArchetype` (types.rs:371)

**Mirrors:** `types.ts:126`.
**Intent:** A weapon's high-level family — `Beam`, `Ordnance`, `Broadside`,
`Displacement`, `Control`, `Movement`, `Defensive`. The resolver never branches on
archetype; it's UI + filtering metadata. Serializes camelCase (`"beam"`, `"ordnance"`,
…).

#### `enum TargetingPattern` (types.rs:387)

**Mirrors:** `types.ts:130`.
**Intent:** The eight `resolve_targeting` branches.

**Drift note: SCREAMING_SNAKE_CASE preservation.** Variant names are kept verbatim
(`POINT_BLANK`, `SPINAL_LINE`, `BEAM`, `BROADSIDE`, `BLAST`, `ORDNANCE`, `SELF`,
`DEPLOYED_CELL`) rather than rewritten to Rust-idiomatic PascalCase (`PointBlank`
etc). The `#[allow(non_camel_case_types)]` on line 385 silences the clippy lint. The
**reason** is grep parity: `grep -r 'SPINAL_LINE' src/` and `grep -r 'SPINAL_LINE'
_drive_pull/` return matched sets across both ports. Reviewers and future readers can
search either ecosystem and land on the same tokens. Tested at
`targeting_pattern_preserves_screaming_snake` (line 856).

This is the convention architect called load-bearing in their kickoff message; the
same pattern is repeated for `Effect` variants and `MovementMode` variants below.

---

### Section 5: Effects (lines 398–500)

#### `enum Effect` (types.rs:409)

**Mirrors:** `types.ts:136`.
**Intent:** The closed verb set an action emits. Internally tagged on `kind` (line 408)
so JSON `{ "kind": "DAMAGE", "amount": 4 }` deserializes directly into
`Effect::DAMAGE { amount: 4, band_falloff: None }`. Nine variants:

| Variant            | Payload                                              | Walkthrough                                   |
|--------------------|------------------------------------------------------|-----------------------------------------------|
| `DAMAGE`           | `amount: i32`, `band_falloff: Option<bool>`          | See **predicate semantics** sub-section below |
| `APPLY_STATUS`     | `status: StatusKind`, `duration: i32`                | Resolver dispatches via `add_status`           |
| `DISPLACE_TARGET`  | `mode: DisplaceMode`, `distance: i32`                | push/pull/swap, collision damage TBD          |
| `DISPLACE_SELF`    | `mode: MovementMode`, `distance: i32`                | THRUST/BURN/SLIP/JUMP/TRACTOR_SWAP            |
| `REORIENT`         | `to: ReorientTo`                                     | flip / bowOn / broadside                       |
| `SPAWN_ORDNANCE`   | `projectile: String`                                 | resolver calls `content.spawn_projectile(...)` |
| `VENT_HEAT`        | `amount: i32`, `recharge_cooldowns: Option<bool>`    | clears heat, optionally recharges cooldowns    |
| `DEPLOY`           | `hazard: DeployHazardKind`                           | mines + drones only (no debris)                |
| `BOARD`            | `note: String`                                       | TODO in TS resolve.ts:226; content owns it     |

**Drift note: `band_falloff` predicate semantics — Effect::DAMAGE.** TS has
`bandFalloff?: boolean` at `types.ts:137`. The resolver predicate at `resolve.ts:143`
is:

```ts
weapon.effects.some((e) => e.kind === "DAMAGE" && e.bandFalloff === false)
```

Strict-equal-to-false. This means:

- **`None`** (field absent) → apply falloff.
- **`Some(true)`** → apply falloff.
- **`Some(false)`** → bypass falloff.

The naive Rust port `!band_falloff.unwrap_or(true)` happens to be correct (`None`
unwraps to `true` → bypass = `!true` = false → apply falloff; `Some(false)` → bypass
= `!false` = true → bypass), but it reads backwards. The architect documented the
correct idiom on lines 412–422 of the doc comment, and pinned it with a test at
`types.rs:958` (`damage_band_falloff_predicate_semantics`):

```rust
let bypass = |e: &Effect| matches!(e, Effect::DAMAGE { band_falloff: Some(false), .. });
```

**Additional gotcha** (also from architect's doc comment, lines 420–422): the
predicate is `effects.some(...)`, not per-effect. ONE damage effect on the action with
`bandFalloff: false` disables falloff for the **whole** `applyDamage` call, not just
that effect. The resolver port must preserve this; a per-effect implementation would be
a subtle drift. Watch list item for when `resolve.rs` lands.

**Drift note: SCREAMING_SNAKE_CASE variants.** Same convention as `TargetingPattern`.
`#[allow(non_camel_case_types)]` on line 406; grep parity preserved across ports.

Worked examples in tests at `types.rs:828` (DAMAGE roundtrip with optional
band_falloff) and `:843` (DISPLACE_SELF parses with MovementMode).

#### `enum DisplaceMode { Push, Pull, Swap }` (types.rs:464)

**Mirrors:** `types.ts:139`.
**Intent:** Variants of `DISPLACE_TARGET.mode`. TS uses lowercase literals; serde
`rename_all = "lowercase"` (line 463) matches.

#### `enum ReorientTo { BowOn, Broadside, Flip }` (types.rs:475)

**Mirrors:** `types.ts:141`.
**Intent:** Variants of `REORIENT.to`. `BowOn` and `Broadside` align with the
`Orientation` tag values; `Flip` is the stance-preserving inversion (bow-on
fore→aft; broadside stays broadside).

#### `enum DeployHazardKind { Mine, Drone }` (types.rs:485)

**Mirrors:** `types.ts:144`.
**Intent:** The action-effect subset of `HazardKind`. `DEPLOY` cannot produce debris —
debris is environmental (capital-ship wreckage), not deployable. Type-level separation
prevents a content typo from emitting an unspawnable hazard.

#### `enum MovementMode { THRUST, BURN, SLIP, JUMP, TRACTOR_SWAP }` (types.rs:494)

**Mirrors:** `types.ts:147`.
**Intent:** Self-movement path rules. SCREAMING_SNAKE_CASE preserved; same convention
as `Effect` and `TargetingPattern`. Tested at `effect_displace_self_parses_movement_mode`
(`types.rs:843`).

---

### Section 6: Ordnance entity (lines 502–522)

#### `struct Projectile` (types.rs:509)

**Mirrors:** `types.ts:151`.
**Intent:** A torpedo or missile travelling the lane. Spawned by `SPAWN_ORDNANCE`
effects, advanced during the ordnance phase, can be shot down by point-defense. Lives
in `Board::ordnance` until impact or off-board.

Fields: `id`, `kind` (lookup key for spawn-time stats), `cell`, `heading: LaneEnd`,
`speed: u32` (cells per turn), `hull: i32` (point-defense damages this), `payload:
Vec<Effect>` (applied on impact), `owner_faction: Faction`. The `owner_faction` is
critical: the resolver only impacts on a cell whose ship has `faction !=
owner_faction`, so a player torpedo won't hit the player.

---

### Section 7: Subsystems / event bus (lines 524–724)

The longest section — and the one with the most port-specific design work. Four items:
`SubsystemDef`, `SubsystemBay`, `Hook`, `HookContext`, plus the actual `EventBus`
struct with its `Default`/`on`/`emit` impl.

#### `struct SubsystemDef` (types.rs:533)

**Mirrors:** `types.ts:164` (`Omit<Subsystem, "apply">`).
**Intent:** The serde-shaped catalog half of a subsystem. The runtime half — the one
that carries an `apply` callback — lives next to the content slice.

Fields: `id`, `name`, `bay: SubsystemBay`, `hook: Hook`, `cost: i32`, `unlock_salvage:
Option<i32>`, `level: i32`, `max_level: i32`.

**Drift note: `unlock_salvage` null handling (the H4 fix).** The TS shape is
`unlockSalvage: number | null`, **not** `unlockSalvage?: number`. Difference: in TS,
the former *requires* the field to be present (with value `null` or a number); the
latter allows omission. The catalog uses `null` explicitly to signal "available from
the start," and the byte-stable JSON serialization the analysis-doc round-trip relies
on requires `None` to serialize as `null`, not be omitted.

In Rust, that means:

```rust
#[serde(rename = "unlockSalvage", default)]
pub unlock_salvage: Option<i32>,
```

with `default` but **without** `skip_serializing_if = "Option::is_none"`. The
`#[serde(default)]` is *defensive* — it lets a future catalog version that drops the
key entirely still parse — but the absence of `skip_serializing_if` is what guarantees
`None` round-trips as `null`. Pinned by `subsystem_def_unlock_salvage_null_roundtrips`
at `types.rs:921`:

```rust
assert!(json.contains(r#""unlockSalvage":null"#));
```

If anyone adds `skip_serializing_if` here in a "cleanup" PR, that test fails.
Important to surface in the LINE_BY_LINE doc because the difference between optional-
omittable and optional-nullable is invisible at a glance.

#### `enum SubsystemBay` (types.rs:553)

**Mirrors:** `types.ts:167`.
**Intent:** Six bay categories: `Gunnery`, `Helm`, `Engineering`, `Tactical`,
`General`, `Astrogation`. Used by UI grouping and salvage-gating; resolver doesn't
branch on it.

#### `enum Hook` (types.rs:566)

**Mirrors:** `types.ts:176`.
**Intent:** Event-bus hook names. Eleven variants in declaration order:
`Passive`, `OnChainKill`, `OnTurnEnd`, `OnVent`, `OnWaveStart`, `OnHeatThreshold`,
`OnDamageDealt`, `OnDamageTaken`, `OnHeal`, `OnReorient`, `OnLethal`. Variant names
serialize as `"passive"`, `"onChainKill"`, `"onTurnEnd"`, … via `rename_all =
"camelCase"`.

**Drift note: declaration order is load-bearing for `EventBus`.** `EventBus::slot`
(line 666) is an exhaustive match returning a hard-coded `usize` per variant. Adding a
variant without extending `slot` is a compile error (the exhaustive match catches it).
Adding a variant *and* extending `slot` without bumping `HOOK_COUNT` is a test failure
(`hook_count_matches_enum_cardinality` at `types.rs:975`).

#### `struct HookContext<'b>` (types.rs:594)

**Mirrors:** `types.ts:183`.
**Design anchor:** H2 from the reviewer audit — cell-indices, not raw pointers.
**Intent:** The payload a hook subscriber receives. Strongly-typed core for the common
fields (`source_cell`, `target_cell`, `amount`) plus an `extras: HashMap<String,
serde_json::Value>` bag for the TS `[k: string]: unknown` overflow.

**Drift note: cell-indices vs. raw pointers (H2 audit fix).** TS uses
`{ source?: Ship; target?: Ship; ... }` — references into the board's ship objects.
Reviewer flagged the naive Rust port (`*mut Ship` or `&'b mut Ship`) as unsound: a
callback that also has `&'b mut Board` would alias the same memory through two
mutable references. The audit-response fix (commit `291206d`) replaced the references
with `Option<usize>` cell indices into `Board::cells` (lines 596–597). Subscribers
look up the ship via `ctx.board.cells[*ctx.source_cell.unwrap()].as_ref()` — which
also gracefully handles the "ship destroyed mid-callback" case as `None`.

Documented in the doc comment lines 583–593 — read that paragraph in full when
modifying the bus or writing a subscriber. The `'b` lifetime on `&'b mut Board` is
named so callers can name it; the most common emit shape (just board, nothing else) is
constructed via `HookContext::new(&mut board)` at line 604.

#### `struct EventBus` + `Default` + `on` + `emit` (types.rs:641–725)

**Mirrors:** `types.ts:191` (the interface) and `demo.ts:16` (the trivial JS impl).
**Intent:** Synchronous pub/sub. Eleven slots, one per `Hook`. Each slot holds a
`Vec<Option<Box<dyn FnMut(&mut HookContext)>>>` — boxed closures, mutable callbacks
(so subsystem state can accumulate), wrapped in `Option` for the take/replace dance.

The doc comment on lines 615–640 is **the most important architectural note in this
file**. Read it before writing any subscriber or modifying the bus. Three re-entrancy
cases:

1. **Same-hook re-emit during a callback.** The slot at the currently-executing index
   reads as `None` (taken out at the top of the loop iteration); every other
   subscriber fires. Fix for the ReactorBreach/Voidtouched chain where a nested
   `destroy` would otherwise silently lose the second `onLethal`.
2. **Same-hook re-register during a callback.** A callback that calls
   `bus.on(...)` pushes a new subscriber to the end of the vec. The outer `emit` loop
   re-reads `len()` each iteration, so the new subscriber fires in the same pass —
   same semantics as TS `forEach` for in-place push.
3. **Cross-hook emit.** Unaffected — only the live hook's slot is in the take/replace
   dance.

The implementation:

- **`HOOK_COUNT: usize = 11`** (line 649) — hand-counted. The compile-time guard is
  `EventBus::slot`'s exhaustive match; the cardinality cross-check is the
  `hook_count_matches_enum_cardinality` test at line 975.
- **`Default::default()`** (line 652) — builds via `std::array::from_fn(|_|
  Vec::new())`. `Vec` is not `Copy`, so the `[Vec::new(); 11]` shorthand doesn't work;
  the explicit `from_fn` builds the array element by element. Existence of this impl
  is what lets `mem::take(&mut board.bus)` work — the take leaves a default-
  constructed empty bus behind on the board.
- **`slot(hook: Hook) -> usize`** (line 666) — the dense index mapping. Exhaustive
  match is the drift guard.
- **`on<F>(&mut self, hook, f)`** (line 686) — push `Some(Box::new(f))` to the slot's
  vec. The `F: FnMut(&mut HookContext) + 'static` bound is required because the
  closure is boxed and outlives the call.
- **`emit(&mut self, hook, ctx)`** (line 701) — the take/replace loop. Each iteration:
  `.take()` the slot at index `i`, invoke if `Some`, put the box back. The loop
  condition re-reads `self.subscribers[slot].len()` each iteration to pick up
  same-hook re-registers.

The doc comment on lines 715–719 flags a future-work item: a `bus.off(...)` API would
mark slots `None` to drain; the emit loop would need a compaction pass at the end.
Not implemented today because nothing needs to unsubscribe.

**Drift note: not `Send + Sync`.** Boxed `FnMut` is `?Send + ?Sync` by default; the
renderer slice cannot move a `Board` across threads without revisiting this bound.
Flagged in the doc comment lines 638–640. Watch list item for the renderer team.

**Worked example (`emit_fires_subscribers_in_registration_order`, types.rs:1002):**
Builds a minimal `Board`, takes the bus off it via `mem::take`, registers two
`OnDamageDealt` subscribers that write to a shared log, emits once, asserts both
fired in registration order. This test also doubles as the canonical example of the
**`mem::take` pattern** the resolver uses to satisfy borrow checking — see the
`NOTE on N1` comment block at line 1053 explaining why the bus comes off the board
during emit and what re-entrancy limitations that creates.

**Open architectural item (line 1053–1062):** the storage-level take/replace fix in
`EventBus::emit` makes the bus itself re-entrant-safe, but the resolver pattern
(`resolve::emit` future) moves the entire bus off the board for the duration of the
emit, so a callback that tries `ctx.board.bus.emit(...)` finds an empty bus and
silently no-ops. The ReactorBreach/Voidtouched chain is blocked by *that* layer, not
by `EventBus`. Pending a team design call (likely a `RefCell<EventBus>` on `Board`);
see the architect-to-reviewer thread.

---

### Section 8: Catalog (lines 727–803)

#### `struct Catalog` (types.rs:736)

**Mirrors:** `types.ts:198`.
**Intent:** The JSON payload exported by the analysis doc's "Copy JSON" button.
Field-for-field port of the TS `Catalog` interface.

**Drift note: `unknown[]` placeholders → `Vec<serde_json::Value>`.** Five fields —
`capitals`, `classes`, `fieldkit`, `sectors`, `commendations` — are typed as
`unknown[]` in the TS source (`types.ts:206–210`). The Rust port carries them as
`Vec<serde_json::Value>` (lines 744–753) so they parse today and can be tightened to
real types later without breaking any consumer. Each has `#[serde(default)]` so a
catalog missing one of these arrays still parses — defensive against partial test
fixtures. **Drift watch list item resolved.**

#### `struct CatalogMeta` (types.rs:757)

**Mirrors:** `types.ts:199 (meta inline)`.
**Intent:** Metadata header: `schema` version, `lane: Vec<u32>` (allowed lane sizes,
typically `[5, 7, 9]`), `new_axes` (the design's tracked novelty list — band,
orientation, ordnance, heat), `bands` (the declared `RangeBand` order).

#### `struct ModDef` (types.rs:767)

**Mirrors:** `types.ts:202`.
**Intent:** A weapon mod catalog entry: `id`, `name`, `cd` (cooldown in turns), `desc`.
`Action.r#mod` carries the id of one of these.

#### `struct StatusDef` (types.rs:778)

**Mirrors:** `types.ts:203`.
**Intent:** A *catalog* entry describing a status — distinct from the runtime
`Status` instance. Fields: `id`, `name`, `effect` (free-form description), `origin`
(provenance — which subsystem/mod grants it).

#### `struct PatrolDef` (types.rs:787)

**Mirrors:** `types.ts:209`.
**Intent:** Per-patrol-tier metadata: `n: u8` (tier 1..=7) plus `r#mod: String` (the
cumulative modifier description). Same `r#mod` reserved-keyword escape as `Action`.

#### `struct EnemyDef` (types.rs:794)

**Mirrors:** `types.ts:213`.
**Intent:** An enemy ship type's catalog entry. `hull` is base; `hull5` is the
effective hull at Patrol 5+. Traits and weapons are lists of catalog ids (the resolver
looks them up at spawn time).

---

### `#[cfg(test)] mod tests` (types.rs:809–1063)

Twelve tests, each pinning down a serde-parity contract or a port-decision invariant.
Notable cases:

- **`orientation_roundtrips_through_ts_shape`** (`:813`) — the tagged-enum shape.
- **`effect_damage_roundtrips_with_optional_band_falloff`** (`:828`) — both the
  field-absent case and the field-present case.
- **`effect_displace_self_parses_movement_mode`** (`:843`) — nested-enum parsing.
- **`range_band_serializes_camel_case`** (`:850`) — naming convention enforcement.
- **`targeting_pattern_preserves_screaming_snake`** (`:856`) — the grep-parity test.
- **`ship_roundtrips_with_pulse_laser_demo_shape`** (`:863`) — the demo.ts player
  parity test; the most important Ship test.
- **`shield_profile_rejects_missing_zone`** (`:892`) — the M1 fix.
- **`shield_profile_index_mut_decrements_charge`** (`:908`) — the IndexMut impl.
- **`subsystem_def_unlock_salvage_null_roundtrips`** (`:921`) — the H4 null
  serialization.
- **`damage_band_falloff_predicate_semantics`** (`:958`) — pins the
  Some(false)-only-bypasses contract.
- **`hook_count_matches_enum_cardinality`** (`:975`) — the cross-check the bus relies
  on.
- **`emit_fires_subscribers_in_registration_order`** (`:1002`) — the bus baseline.

Plus the `NOTE on N1` comment block at line 1053 documenting the open
bus-borrowing architectural question.

---

### Drift watch list (resolved by `5625f30 + 291206d`)

- ~~**`klass` → `class` / `ship_class` / `class_id`.**~~ Kept as `klass` for
  cross-port grep parity.
- ~~**`apply: (ctx) => void` subsystem callback.**~~ Split: `SubsystemDef` (catalog,
  no callback) here; runtime `Subsystem` (with callback) deferred to the content
  slice.
- ~~**`Record<HullZone, ShieldFace>` representation.**~~ `ShieldProfile` named-field
  struct (M1 audit fix). The `geometry.rs` walkthrough's drift note has been updated
  in this same commit — `default_shield_profile` now returns `ShieldProfile`, not the
  HashMap shape `b1bf47c` documented.
- ~~**`bus: EventBus` field on Board.**~~ Embedded; resolver uses `mem::take` pattern
  to satisfy borrow checking during emit. `EventBus: Default` is the load-bearing impl.
- ~~**`Effect` discriminated union.**~~ Tagged enum with `#[serde(tag = "kind")]`;
  SCREAMING_SNAKE_CASE variants preserved.
- ~~**`Catalog`'s `unknown[]` placeholders.**~~ Mapped to `Vec<serde_json::Value>`
  with `#[serde(default)]`; tightenable later.

**New decisions documented in this pass (not from the pre-port watch list):**

- `Board.destroys_this_window: usize` — new field, chain-kill counter. Reset
  semantics owned by the resolver.
- SCREAMING_SNAKE_CASE convention extends from `Effect` and `TargetingPattern` to
  `MovementMode`. Pattern is universal across actions/effects/movement; preserved for
  grep parity.
- `r#mod` raw identifier used on `Action.mod` and `PatrolDef.mod`. JSON wire shape
  unchanged (`"mod"`).
- `HookContext` carries `source_cell` / `target_cell` as `Option<usize>` (H2 audit
  fix); raw `*mut Ship` ruled out as unsound.
- `EventBus` take/replace re-entrancy: same-hook re-emit safe; same-hook re-register
  appended in same pass; cross-hook unaffected.
- `EventBus: !Send + !Sync` — boxed closures default to single-thread. Future watch
  item for the renderer.
- `Status.face: Option<HullZone>` is dead weight pending content/resolver confirmation.
  Mirrored for parity; flag for removal if nobody plans to use it.

**Open architectural item:**

- Bus borrowing during emit: the resolver's `mem::take` pattern means callbacks cannot
  re-emit via `ctx.board.bus.emit(...)` (finds empty bus). `EventBus::emit`'s
  storage-level take/replace handles the same-bus re-entrancy case; the layer above it
  needs `RefCell<EventBus>` or similar. Pending team design call — see
  architect-to-reviewer thread and the `NOTE on N1` block at `types.rs:1053`.

---

---

## `src/geometry.rs`

*Pure functions over the lane. No randomness, no content lookups, no I/O. Everything
that makes orientation, arcs, and range bands a real decision lives here. The Rust port
is a near-verbatim translation of the TS source — when in doubt, the TS is the canonical
reference (the module rustdoc says so explicitly at `geometry.rs:5`).*

**Mirrors:** `engine/geometry.ts` (the entire file).
**Design anchor:** HTML Part III (Targeting, Arcs & Range Bands) and Part IV
(Orientation & Movement).
**Source commit:** `d383c6a` (initial port) + `21561f1` (audit response: `band_index`
exhaustive match for G4, explicit Extreme-band test for G3). Plus the
`default_shield_profile` return-type shift that came alongside the `ShieldProfile`
struct landing in `types.rs:291206d`. All tests pass.

### Module header (lines 1–8)

The first six lines are a `//!` module rustdoc block that sets the contract: pure
geometry, no randomness, no content. The line that matters for every future reader:
*"when this port and the TS disagree, the TS is right."* That sentence is the
tie-breaker; cite it whenever a drift question turns into a judgment call.

Line 8 is the single `use` statement: `crate::types::{Arc, HullZone, LaneEnd,
Orientation, RangeBand, ShieldFace, ShieldProfile, Ship}`. All eight imports come
from [`src/types.rs`](#srctypesrs). Notably **`std::collections::HashMap` is no longer
imported** — `default_shield_profile` now returns the `ShieldProfile` struct
(`types.rs:228`) rather than a HashMap, eliminating the only hash dependency this
module used to have.

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

### `fn band_index(b: RangeBand) -> usize` (geometry.rs:38)

**Mirrors:** `engine/geometry.ts:27 — const BAND_ORDER` + `BAND_ORDER.indexOf(...)`.
**Intent:** Map a `RangeBand` to its position in the canonical band ordering used by
`band_falloff` to compute the delta between actual and optimal band. The doc comment
on lines 35–37 names what changed: **the exhaustive match here is the drift guard,
caught at compile time.** Reordering or extending `RangeBand` without updating this
function fails to compile.

Lines 39–45: `match b { PointBlank => 0, Close => 1, Mid => 2, Long => 3, Extreme => 4 }`.
A constant-time lookup at the machine-code level (the compiler turns this into a jump
table or a small computed offset). No allocation, no scan.

**Drift note: G4 audit response (commit `21561f1`).** The original port mirrored the TS
literally: a `const BAND_ORDER: [RangeBand; 5]` array + a linear `.iter().position(...)`
scan + a `.expect("BAND_ORDER covers every RangeBand")` panic. The reviewer's G4
finding flagged that as a *parallel invariant* — the array order and the enum
declaration order had to be kept in sync by hand, with no compile-time check; a
runtime `.expect` would catch the divergence only when the bad code path actually ran.
The exhaustive match removes the invariant: the match itself is the order, and the
compiler enforces totality. Faster, simpler, drift-proof.

This **supersedes** the pre-fix walkthrough that documented the array form and the
`.expect` panic. If anyone re-introduces a parallel array later (for performance or
table-driven lookup), the comment block needs to come back too.

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

### `fn default_shield_profile() -> ShieldProfile` (geometry.rs:157)

**Mirrors:** `engine/geometry.ts:112 — function defaultShieldProfile()`.
**Intent:** The starting Frigate's hull layout. Strong bow (armour 2), weak stern
(armour 0), medium flanks (armour 1, 1). Zero charge on every face — charges are
granted at runtime by Brace and similar defensive actions, never baked into the
profile. Used by `demo.rs` (future) and by tests as a known-good baseline.

Lines 158–163: construct the `ShieldProfile` struct literal with all four fields
named. No HashMap, no allocation — `ShieldProfile` is `Copy` (it derives `Copy` at
`types.rs:227`) and 32 bytes (four `ShieldFace`s, each two `i32`s), so returning by
value is a register-level copy.

Line 164: return the struct.

**Drift note: ShieldProfile representation (M1 audit fix, supersedes the pre-audit
walkthrough).** The original port used `HashMap<HullZone, ShieldFace>` to mirror the TS
`Record<HullZone, ShieldFace>` JSON wire shape. Reviewer's M1 finding flagged that a
HashMap deserializes successfully even with a missing key — the resolver would then
panic mid-damage-application on `.get(&zone).unwrap()`. The fix (commit `291206d` in
`types.rs`, plus a follow-up in `geometry.rs`) replaced the HashMap with the
named-field `ShieldProfile` struct (`types.rs:228`), whose `Deserialize` impl is total:
any catalog missing a zone fails at parse, before the engine touches the ship.

This **supersedes** the `b1bf47c` walkthrough that recorded "kept as HashMap." The wire
shape is unchanged — JSON still emits `{ "bow": ..., "stern": ..., "port": ...,
"starboard": ... }` — but the parsing contract is stricter and the access is
zero-overhead (no hash, no allocation).

The `ShieldProfile` struct also implements `Index<HullZone>` and `IndexMut<HullZone>`
(`types.rs:258`, `:263`), so the call-site syntax `profile[HullZone::Bow]` still works
unchanged from the HashMap days. The full `ShieldProfile` walkthrough lives in
[`src/types.rs`](#srctypesrs) § Section 3.

**Cross-references:**
- Tested at the end of `geometry.rs`'s test module. Each field is asserted to hold the
  expected `ShieldFace` values.
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

### Drift watch list (resolved by `d383c6a` + `21561f1`)

- ~~**`Math.floor(raw * factor)` — float vs fixed-point.**~~ Architect kept f64. Cross-
  platform determinism is not a current requirement; can be changed later without
  signature impact.
- ~~**Mutation of `ShieldFace.charge`.**~~ Architect used `&mut ShieldFace`; the resolver
  reaches it via the `IndexMut<HullZone>` impl on `ShieldProfile`
  (`types.rs:263`).
- ~~**`Record<HullZone, ShieldFace>` representation.**~~ Started as
  `HashMap<HullZone, ShieldFace>` in `d383c6a`; **revised to the named-field
  `ShieldProfile` struct (`types.rs:228`) as part of the M1 audit fix** (`291206d`).
  Total deserialization rejects partial catalogs at parse rather than panicking deep
  in the resolver. JSON wire shape unchanged. Access via `Index<HullZone>`/
  `IndexMut<HullZone>` keeps call-site syntax identical (`profile[HullZone::Bow]`).
- ~~**`BAND_ORDER` const + linear-scan `band_index`.**~~ Started as a const array + a
  `.position(...)` scan with a runtime `.expect`. **Revised to an exhaustive `match` in
  `band_index` as part of the G4 audit fix** (`21561f1`). The match itself is now the
  drift guard: reordering or extending `RangeBand` without updating `band_index`
  fails to compile. Faster (constant-time, no allocation), simpler, and the parallel-
  invariant comment block could be deleted.

No new drift introduced by this update.

---

## `src/perspective.rs`

*Screen-space perspective: lane trapezoid layout, cell-to-pixel projection, ship
sprite polygon vertices. Pure functions over a `LaneGeometry` plus inputs — no wgpu,
no winit, no rendering state. **The only module in the crate that knows about screen
coordinates;** everything else lives in lane-cell space. The module's rustdoc on lines
4–7 says "when this port and the TS disagree, the TS is right (modulo intentional
Rust-shape changes called out below)," which makes this the only file in the engine
that opens with an explicit "drift expected, here's why" rider.*

**Mirrors:** `_drive_pull/broadside-engine/engine/perspective.ts`.
**Design anchor:** Slice A of the renderer plan (HTML Part XIII implementation order
item — the renderer scaffold sits on top of these primitives). Also see the TS
`PERSPECTIVE.md` rationale doc.
**Source commit:** `70155ed` — *Port engine/perspective.ts to src/perspective.rs*.
462 lines, 15 inline tests, all green. Reviewer audited cleanly.

### Module header (lines 1–35)

A 35-line `//!` block. Three subsections:

**Lines 1–7: intent + tie-breaker.** "Pure functions; no wgpu, no winit, no rendering
state." The TS-is-canonical rule applies *modulo intentional Rust-shape changes*. That
caveat is unique in this codebase — every other module's rustdoc says the TS is right
unconditionally.

**Lines 9–24: six numbered decisions encoded in the module.** This is the design
intent every reader needs in their head before reading any function below:

1. The lane is a tilted trapezoid running left-to-right, one-point perspective receding
   to the right. Vanishing point off-screen.
2. Cells get smaller along the lane: linear scaling from `scale_near` to `scale_far`.
3. Ship sprites use **military axonometric** projection: the port-starboard depth axis
   projects straight up in the ship's local unrotated frame. (No foreshortening on
   depth — that's the "military" part of military-axonometric, distinguishing it from
   true axonometric or cabinet projections.)
4. Every ship sprite is then rotated around its base by the lane's slope angle, so its
   long axis aligns with the lane (bow-on) or runs perpendicular to it (broadside).
5. **Only the FRONT face and TOP face are rendered.** Side faces collapse to zero
   width under military projection — that's intentional, not a bug.
6. The lane is a straight line, so the rotation angle is a single constant for every
   cell. A curved lane would compute a per-cell tangent — flagged as a future-care
   item, not implemented today.

**Lines 26–34: Rust-shape differences from `perspective.ts`.** Two called out (the
third — radians vs degrees — is documented at the `CellScreen` definition instead).
See **Drift notes** below for the full list.

Lines 36–37: imports — `geometry::range_band` and `types::RangeBand`. The only
dependency on the rest of the engine is the canonical band-bucket function; everything
else is local.

---

### `struct Point2 { x: f32, y: f32 }` (perspective.rs:43)

**Mirrors:** `perspective.ts` Point2.
**Intent:** A 2-D screen-space point. Pixels, y-down origin top-left (the canonical
screen-space convention, matching wgpu's viewport transform). `Copy` so callers pass
by value with no borrow concerns.

---

### `struct LaneGeometry` (perspective.rs:51)

**Mirrors:** `perspective.ts` LaneGeometry.
**Intent:** The lane's screen-space footprint plus the cell count and scale gradient.
One source of truth for the entire renderer — every projection function below takes
`&LaneGeometry` rather than recomputing constants.

Fields:
- `front_start`, `front_end` (lines 53, 55) — the two endpoints of the lane's front
  edge on screen. The foreground end (cell 0) and background end (cell N−1). These
  define both the *position* and the *slope* of the lane.
- `back_start`, `back_end` (lines 57–58) — the back edge of the lane (the parallel
  edge farther from camera). Used by `cell_footprint` to compute the trapezoid for
  selection highlights.
- `cell_count: u32` (line 60) — 5, 7, or 9 per the design.
- `scale_near: f32`, `scale_far: f32` (lines 62, 64) — sprite scale at each end. The
  recession factor; defaults give 1.0 → 0.55 (cells at the back are 55% the size of
  those at the front).

### `const DEFAULT_LANE: LaneGeometry` (perspective.rs:71)

**Mirrors:** `perspective.ts:DEFAULT_LANE`.
**Intent:** The viewport-agnostic baseline. Tuned for a 660×240 design viewport (the
TS reference resolution). The Rust gfx layer (`gfx.rs:0c9d358`) either bumps the
engine's virtual resolution to a superset of 660×240 or supplies its own retuned
`LaneGeometry` — the math here is the same either way.

Constants:
- `front_start: (35, 217)` — left-front corner.
- `front_end: (615, 162)` — right-front corner. Visible "uphill" slope toward the
  background (y decreases as x grows, because screen y-down).
- `back_start: (28, 198)`, `back_end: (615, 153)` — back edge. The front and back are
  not parallel in screen space; the trapezoid widens slightly toward the foreground.
- `cell_count: 7`, `scale_near: 1.0`, `scale_far: 0.55`.

**Worked example:** every cell test in the module uses these constants. Cell 0 lands
at `(35, 217)` with scale 1.0; cell 6 at `(615, 162)` with scale 0.55. Test pinning at
lines 281 and 289.

---

### `fn lane_slope_rad(geom: &LaneGeometry) -> f32` (perspective.rs:98)

**Mirrors:** `perspective.ts` lane-slope computation (inline in TS, factored out here).
**Intent:** The slope of the lane's front edge in radians. Used to align ship sprites
with the lane. Module-private — callers go through `cell_to_screen`'s
`rotation_rad` field.

Lines 99–101: `atan2(dy, dx)` over the front-edge vector. Returns negative radians
for `DEFAULT_LANE` because dy is negative (screen y-down + lane rises to the right).
Tested at `lane_slope_is_modest_uphill_to_the_right` (line 307): expected value
`-5.418°` (in degrees, for human readability — function returns radians).

---

### `struct CellScreen` + `fn cell_to_screen` (perspective.rs:86, 106)

**Mirrors:** `perspective.ts` cellToScreen.
**Intent:** Map a cell index `0..cell_count` to its screen position, sprite scale, and
the lane-slope rotation to apply. The single function the renderer calls for each
ship's "where on screen?".

**`struct CellScreen`** fields (line 86):
- `x: f32, y: f32` — center of the cell on the lane's *front* edge (where the ship's
  base sits).
- `scale: f32` — uniform sprite scale at this cell.
- `rotation_rad: f32` — rotation (in radians) around `(x, y)` to align sprites with
  the lane. **Radians, not degrees** — see Drift below.

**`fn cell_to_screen`** body (lines 106–113):
- Line 107: `n = cell_count − 1` (the number of *spans* between cells; n=6 for the
  default 7-cell lane). `saturating_sub(1)` avoids underflow on a one-cell lane.
- Line 108: `t = cell_index / n`, or `0` if n is zero (single-cell lane). The
  parametric position along the lane.
- Lines 109–111: linear interpolate x, y, scale between near and far endpoints.
- Line 112: return `CellScreen` with `rotation_rad = lane_slope_rad(geom)` —
  constant across cells (the lane is straight; per the module rustdoc decision #6).

**Drift note: rotation in radians, not degrees.** The TS `cellToScreen` returns
degrees because SVG `transform="rotate(deg ...)"` takes degrees natively. Every
downstream Rust consumer (rotation matrices, `f32::sin`/`cos`, wgpu transforms) wants
radians. Architect's three-decision approval list flagged this — math is line-for-line
TS, output unit is the only change. The doc-comment on line 94 calls this out
explicitly.

**Worked example (test `cell_to_screen_midpoint_interpolates_evenly`, line 297):** Cell
3 of 7 lands at `t = 3/6 = 0.5`. x = 35 + 0.5×(615−35) = 325. y = 217 + 0.5×(162−217)
= 189.5. Scale = 1.0 + 0.5×(0.55−1.0) = 0.775. Linear interpolation, no surprises.

---

### `fn fractional_cell_to_screen(fractional_cell: f32, geom)` (perspective.rs:118)

**Mirrors:** `perspective.ts` fractionalCellToScreen.
**Intent:** Continuous version of `cell_to_screen` for fractional positions along the
lane. Used by ordnance entities mid-flight (a torpedo at fractional cell 4.3 is
between cells 4 and 5, sized accordingly).

Body identical to `cell_to_screen` *except* line 120 clamps `t` to `[0, 1]` with
`.clamp(0.0, 1.0)` so an out-of-range fractional position (negative, or beyond
`cell_count − 1`) renders at the nearest endpoint rather than off-screen. The TS
version does the same clamp.

**Worked example (test `fractional_cell_at_4_matches_ts_reference`, line 337):**
`fractional_cell = 4.0`, `t = 4/6 ≈ 0.6667`. x ≈ 421.67, y ≈ 180.33, scale = 0.7. The
test asserts to ±0.01 px — these are the canonical values from the TS
`render-example.ts` reference, used as the cross-port parity check.

---

### `struct ShipDims` and `const FRIGATE_DIMS` (perspective.rs:132, 139)

**Mirrors:** `perspective.ts` ShipDims + FRIGATE_DIMS.
**Intent:** A ship's world-unit dimensions. `length` is bow-stern, `beam` is
port-starboard, `height` is vertical. Other classes (Capital, Destroyer, etc.) will
provide their own constants when content adds them.

`FRIGATE_DIMS = { length: 56, beam: 14, height: 6 }` — units are world-pixels at
`scale_near`; the projection multiplies by the cell's scale.

---

### `enum Stance { BowOn, Broadside }` (perspective.rs:144)

**Mirrors:** `perspective.ts` Stance.
**Intent:** Which way the hull is turned in the rendering frame. *Not the same as*
`types::Orientation` — that carries a bow direction too (`Orientation::BowOn { bow:
LaneEnd }`), needed by the resolver for damage routing. The renderer projects the
*sprite*, which only cares about along-lane vs. across-lane; the bow direction
is conveyed by the chevron overlay computed from `bow_dir` below.

**Drift note: separate from `types::Orientation`.** Could in principle have been a
`From<Orientation>` adapter; architect kept it as a distinct enum because the
renderer never needs the bow direction at the geometry-projection layer. The mapping
is the obvious one: `Orientation::BowOn { .. } → Stance::BowOn`,
`Orientation::Broadside → Stance::Broadside`. Whoever wires the renderer to ship
state will do that conversion.

---

### `type FacePoly = [Point2; 4]` (perspective.rs:153)

**Intent:** The four vertices of a rectangle in the unrotated screen frame. Vertex
order is **bottom-left, bottom-right, top-right, top-left** (CCW with screen y-down).
The renderer's vertex shader rotates them as a group around `pivot` by
`rotation_rad`.

**Drift note: `[Point2; 4]` arrays vs TS formatted strings.** The TS computes face
polygons as `"x,y x,y x,y x,y"` strings ready to drop into SVG `<polygon points="">`.
The Rust port returns raw vertex arrays. Architect's approved drift: wgpu wants
vertex buffers, not formatted strings; formatting at the geometry layer would force
the renderer to parse strings back into floats. Math is identical; output shape is
the only change.

---

### `struct ShipSprite` (perspective.rs:159)

**Mirrors:** `perspective.ts` ShipSprite (but with very different field shape — see
Drift below).
**Intent:** The output of `ship_sprite`. Everything the renderer needs to draw one
ship at one cell: two face polygons, the pivot+angle to rotate them, anchors for
chevron and bridge overlays, and a unit bow-direction vector for beam-origin and
chevron-direction math.

Fields:
- `pivot: Point2` — the rotation pivot, equal to `(cell.x, cell.y)` (the cell's
  screen position). The whole sprite rotates about this point.
- `rotation_rad: f32` — lane slope in radians; the rotation to apply about the pivot.
- `front_face: FacePoly` — four vertices of the front face (small, at the lane
  surface).
- `top_face: FacePoly` — four vertices of the top face (larger, above the front
  face).
- `top_center: Point2` — center of the top face in the unrotated frame. Chevron
  anchor.
- `front_center: Point2` — center of the front face. Bridge / status anchor.
- `bow_dir: Point2` — **POST-rotation** unit vector along the ship's bow direction.
  This is the only field that already has the rotation baked in; everything else is
  in the unrotated frame and gets rotated by the vertex shader. Used for chevron
  orientation and beam-origin offsets.

**Drift note: pivot + angle vs pre-baked SVG transform string.** The TS `ShipSprite`
carries a `transform: string` field ready to drop into SVG `<g transform="...">`. The
Rust port carries `(pivot, rotation_rad)` so the renderer composes the rotation into
its vertex shader. Architect's approved drift; the renderer never wants strings, the
SVG-string layer would have to be re-parsed for wgpu's purposes. Third of the three
approved output-shape drifts (the others: `[Point2; 4]` for polygons, radians for
rotation).

---

### `fn ship_sprite(cell: CellScreen, dims: ShipDims, stance: Stance) -> ShipSprite` (perspective.rs:185)

**Mirrors:** `perspective.ts` shipSprite.
**Intent:** The core projection function. Compute the polygon vertices and rotation
transform for one ship at one cell in one stance. Military-axonometric projection in
the unrotated frame, then a single rotation around the base aligns it with the lane.

The walkthrough is long because every line matters; this is the dense geometry of the
module.

**Line 186:** destructure `CellScreen` — pull x, y, scale, rotation_rad as locals so
the body reads cleanly without `cell.` prefixes.

**Lines 189–192: stance swap.** The world-axis dimensions stay constant (a Frigate
is always 56 long × 14 wide × 6 tall) but the *screen-axis assignment* swaps with
stance. Bow-on: along-lane width is `length × scale`, depth is `beam × scale`.
Broadside: along-lane width is `beam × scale`, depth is `length × scale`. The visual
effect: rotating the hull 90° in world swaps the two on-screen axes.

**Line 193:** `screen_h = height × scale` — vertical projection. Independent of
stance; the hull is the same height bow-on or broadside.

**Lines 194–195:** half-extents — `hw = screen_w / 2` (half on-lane width) and
`depth_offset = screen_d / 2` (half depth). All polygons sit ±hw and ±depth_offset
from the center.

**Lines 197–202: front face polygon.** Four vertices in CCW order:
- `(x - hw, y)` — bottom-left (lane-surface, left side)
- `(x + hw, y)` — bottom-right
- `(x + hw, y - screen_h)` — top-right (above the lane surface by the hull height)
- `(x - hw, y - screen_h)` — top-left

(Screen y-down: `y - screen_h` is *above* the lane on screen.)

**Lines 203–208: top face polygon.** The top of the hull, projected up by
`depth_offset` (military axonometric: depth projects straight up, no foreshortening):
- `(x - hw, y - screen_h)` — front-left edge (shared with the front face's top-left)
- `(x + hw, y - screen_h)` — front-right
- `(x + hw, y - screen_h - depth_offset)` — back-right (further up by depth)
- `(x - hw, y - screen_h - depth_offset)` — back-left

Note vertices [0] and [1] of `top_face` are the same as [3] and [2] of `front_face`
— the two faces share the hull's top-edge ridge. Renderer can dedupe if it cares
about vertex count; the projection itself emits both for clarity.

**Lines 214–218: bow direction (POST-rotation).** This is the only field that bakes
in the lane rotation, because the renderer needs an angle-aware direction for
chevron placement.

- Compute `cos_r, sin_r` from `rotation_rad` once.
- `Stance::BowOn`: bow points along +x in the local unrotated frame. After rotation
  by `rotation_rad`, that becomes `(cos_r, sin_r)`. For `DEFAULT_LANE`'s slope
  ≈ -5.4°, this gives ≈ (0.996, -0.094) — pointing slightly up and strongly right.
- `Stance::Broadside`: bow points along +depth in the local unrotated frame, which
  projects to -y in screen coordinates (depth projects straight up = -y). After
  rotation, that becomes `(-sin_r, -cos_r)`. For the default slope, ≈ (0.094,
  -0.996) — pointing strongly up with a small rightward skew.

The bow-direction tests at lines 391 and 404 pin these unit vectors with the actual
default slope and assert unit length.

**Lines 220–228: assemble the struct.** Pivot at `(x, y)`, `top_center` and
`front_center` at the geometric centers of their respective faces (in the unrotated
frame), and the computed `bow_dir`.

**Worked examples in tests:**
- `ship_sprite_bow_on_long_axis_runs_along_lane` (line 348): Frigate at cell 0,
  scale 1.0. Front face: 56 wide, 6 tall. Top face depth: 7. Asserts exact dimensions
  in the unrotated frame.
- `ship_sprite_broadside_rotates_dimensions_90_degrees` (line 366): same Frigate,
  broadside. Front face: 14 wide (beam), 6 tall. Top face depth: 28 (length / 2).
- `ship_sprite_scales_with_cell_distance` (line 381): far/near ratio = 0.55.
- `ship_sprite_bow_dir_bow_on_points_along_lane` (line 391): unit length, +x heavy.
- `ship_sprite_bow_dir_broadside_points_off_lane` (line 404): unit length, -y heavy.

---

### `fn beam_endpoints(source_cell, target_cell, geom) -> (Point2, Point2)` (perspective.rs:235)

**Mirrors:** `perspective.ts` beamEndpoints.
**Intent:** Endpoints for a weapon beam from one cell to another. Both endpoints sit
on the lane's front edge, so the beam visually follows the lane plane automatically
— no explicit z-axis needed.

Lines 236–238: `cell_to_screen` for source and target, then strip out just the
`(x, y)` positions. The full `CellScreen` is overkill for this caller; only the
position is used.

**Worked example (test `beam_endpoints_run_along_the_lane_front_edge`, line 416):**
Beam from cell 0 to cell 6. Endpoints at `(35, 217)` and `(615, 162)`. The slope
between them must equal the lane's front-edge slope (-55 / 580 ≈ -0.0948); test
asserts to ±1e-4.

---

### `fn cell_footprint(cell_index, geom) -> [Point2; 4]` (perspective.rs:244)

**Mirrors:** `perspective.ts` cellFootprint.
**Intent:** The four corners of a cell's footprint on the lane top surface — a
parallelogram in the lane plane. Used for selection highlights and cell-hover
overlays.

Body (lines 245–257):
- `n = cell_count` (note: not `cell_count − 1` like `cell_to_screen` — this function
  divides the lane into n equal strips, not n−1 spans between n centers).
- `t0 = cell_index / n`, `t1 = (cell_index + 1) / n` — the parametric bounds of this
  cell's strip.
- A `lerp_pt` closure does linear interpolation between two points.
- Return four vertices in order: **front-near, front-far, back-far, back-near**
  (line 252–256). This ordering is important for the renderer's index buffer; the
  comment on line 244 specifies it.

**Worked example (test `cell_footprint_returns_four_distinct_points`, line 429):**
Cell 3's front edge must lie on the lane's front line (same slope), back edge on the
back line. Tested to ±1e-4.

---

### `fn band_between_cells(source: u32, target: u32) -> RangeBand` (perspective.rs:264)

**Mirrors:** `perspective.ts` bandBetweenCells.
**Intent:** Thin convenience wrapper over `geometry::range_band` so renderer code can
stay in this module without reaching into the resolver-side `geometry`. **Both paths
MUST agree** — the test at line 444 cross-checks every cell-distance combination
0..=9.

Line 265: `range_band(source as usize, target as usize)`. The `u32 → usize` cast is
infallible on 32-bit-or-wider platforms (all targets we care about). Could in
principle change the type of `geometry::range_band` to accept `u32` instead, but
that would ripple into every resolver call site for no benefit — the cast is local
and free.

**Drift note: rename.** TS was `bandBetweenCells`; Rust is `band_between_cells`
(snake_case per the language convention). Function-body math is identical.

**Cross-reference test (`band_between_cells_matches_geometry_range_band`, line 443):**
Iterates every `(s, t)` in `0..=9 × 0..=9` and asserts
`band_between_cells(s, t) == range_band(s, t)`. Drift guard — if `geometry::range_band`
ever changes its bucket boundaries, this test fires before merge.

---

### `#[cfg(test)] mod tests` (perspective.rs:273–458)

15 tests covering every public function plus the cross-port reference points. Test
names read as sentences:

```
cell_to_screen_near_matches_front_start
cell_to_screen_far_matches_front_end
cell_to_screen_midpoint_interpolates_evenly
lane_slope_is_modest_uphill_to_the_right
cell_to_screen_single_cell_lane_is_safe
fractional_cell_clamps_into_bounds
fractional_cell_at_4_matches_ts_reference
ship_sprite_bow_on_long_axis_runs_along_lane
ship_sprite_broadside_rotates_dimensions_90_degrees
ship_sprite_scales_with_cell_distance
ship_sprite_bow_dir_bow_on_points_along_lane
ship_sprite_bow_dir_broadside_points_off_lane
beam_endpoints_run_along_the_lane_front_edge
cell_footprint_returns_four_distinct_points
band_between_cells_matches_geometry_range_band
```

The TS-reference parity test (`fractional_cell_at_4_matches_ts_reference`, line 337)
is the canonical cross-port check; it asserts exact numeric output against the TS
`render-example.ts` reference values to ±0.01 px. If a future port change drifts the
projection math, this test fires first.

The cross-module agreement test (`band_between_cells_matches_geometry_range_band`,
line 443) is the canonical drift guard against `geometry::range_band` and
`perspective::band_between_cells` getting out of sync. Run on every `cargo test`.

---

### Drift watch list (approved by team-lead, recorded in `70155ed`)

Three intentional drifts from TS, all output-shape only — math is line-for-line TS:

1. **`(pivot: Point2, rotation_rad: f32)` instead of pre-baked SVG `transform` string.**
   The TS produces a `"rotate(deg cx cy)"` string ready for SVG; Rust returns the
   pivot point and angle separately. Reason: the wgpu vertex shader composes the
   rotation into its instance transform; it never wants strings, and parsing a TS-
   formatted transform back into floats would be wasted work. Math identical.

2. **`[Point2; 4]` polygon arrays instead of formatted `"x,y x,y"` strings.** The TS
   builds polygon point strings for SVG `<polygon points="...">`. Rust returns the
   raw vertex array. Reason: wgpu wants vertex buffers, not strings. Same vertex
   positions, same order, just unformatted.

3. **Rotation in radians, not degrees.** The TS returns degrees because SVG
   `transform="rotate(deg ...)"` takes degrees natively. Rust returns radians because
   every downstream consumer (`f32::sin`/`cos`, rotation matrices, wgpu's WGSL math)
   wants radians. Conversion is `to_degrees()` / `to_radians()` if any caller needs
   the other unit; in practice none does.

All three are documented inline in the module: rustdoc lines 26–34 cover (1) and (2);
the `CellScreen.rotation_rad` doc comment on line 92 covers (3).

**No other drift introduced by this commit.** The math (linear interpolation along the
lane, military-axonometric projection, atan2 slope) is line-for-line ported from the
TS. The renderer plan rests on this; if any of the three approved drifts grow to a
fourth, it should also land in this section as a new Drift note.

---

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
