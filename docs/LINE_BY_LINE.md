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
- [`src/perspective.rs`](#srcperspectivers) — flat screen-space lane: cell-to-pixel transform, ship dims, view-angle stance
- [`src/resolve.rs`](#srcresolvers) — the four-phase round, queue gate, damage pipeline, effect dispatch, all movement/AI/modifier bodies
- [`src/catalog.rs`](#srccatalogrs) — catalog loader + `LoadError` + strict/canonical format auto-detect
- [`src/catalog_canonical.rs`](#srccatalog_canonicalrs) — canonical (design-doc) → strict catalog transformer
- [`src/runs.rs`](#srcrunsrs) — Phase 3 run-loop: encounter outcome, run advancement, board materialization, placeholder sectors
- [`src/meta.rs`](#srcmetars) — cross-run meta-progression: salvage math, unlock-threshold ladder, persistence
- [`src/save.rs`](#srcsavers) — per-run save/load (atomic JSON write), `SaveError`
- [`src/gfx.rs`](#srcgfxrs) — wgpu state, four pipelines, virtual-res offscreen + integer-scale blit
- [`src/atlas.rs`](#srcatlasrs) — procedural 256×256 sprite atlas
- [`src/hud.rs`](#srchudrs) — scene compositor (DrawCommand list)
- [`src/sprites.rs`](#srcspritesrs) — PNG loader for ship sprites, `SpriteRegistry` trait
- [`src/bin/broadside.rs`](#srcbinbroadsidesrs) — winit event loop, input → Intent → resolver
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

*Screen-space lane geometry: a flat horizontal strip bisecting the canvas. Cells
evenly spaced left-to-right at a constant y. Pure functions; no wgpu, no winit, no
rendering state. **The only module in the crate that knows about screen coordinates;**
everything else lives in lane-cell space.*

*A full 3-D projection lived here once — military-axonometric ship sprites rotated by
a lane slope, with `CellScreen` / `lane_slope_rad` / `ShipSprite` / `FacePoly` /
`beam_endpoints` / `cell_footprint` all defined locally. The flat-scene refactor
(`1d4d540`, **task #55**) deleted all of that. Ship projection now lives in
[`src/hud.rs`](#srchudrs) and is driven by a runtime `view_angle` scrubber rather
than baked into screen-space math. What's left here is a near-trivial coordinate
transform: a `LaneGeometry` struct and two interpolation functions.*

**Mirrors:** No longer mirrors the TS `perspective.ts` byte-for-byte; the flat-scene
refactor diverges intentionally. The TS lives at
`_drive_pull/broadside-engine/engine/perspective.ts` for historical reference, but the
Rust port is the canonical reference now for the flat lane.
**Design anchor:** Tasks #55 (flatten the scene) and #57–#60 (revive the angle
scrubber on the flat base, parallax planes responding to `view_angle`, promote the
camera scrubber to permanent feature). The lane is flat; the ships morph along a
view-angle axis owned by [`src/hud.rs`](#srchudrs).
**Source commits:** `1d4d540` (flat-scene refactor — delete `CellScreen`,
`lane_slope_rad`, `ShipSprite`, `FacePoly`, `ship_sprite`, `beam_endpoints`,
`cell_footprint`). Subsequent ship-dim tuning lives in `8d4a569` / `929fdf1` /
`4367e8d`. Module is 203 lines, 7 inline tests, all green.

### Module header (lines 1–12)

The 12-line `//!` block. The flat-scene reality in one paragraph:

> *The lane is a horizontal line at `LaneGeometry::center_y`; cells are evenly spaced
> left to right between `x_left` and `x_right`. The ship sprite math in `hud.rs`
> rotates around the lane using a `view_angle` parameter, so ships morph from pure
> side-view (θ = 0) to pure top-down (θ = π/2) while the lane itself stays flat. Both
> parallax planes — sky above the lane, floor below — foreshorten with the same angle
> so the background reads as a revolving camera.*

Three things to know up front, from the rustdoc:

1. **The lane is geometrically flat.** A single horizontal strip; no tilt, no
   trapezoid, no vanishing point. `cell_to_screen` is a 1-D linear interpolation
   along `x`. `y` is the same constant for every cell.
2. **Ship projection is no longer this module's job.** The flat-scene refactor moved
   the silhouette math into [`src/hud.rs`](#srchudrs). `hud.rs` reads a `view_angle`
   scrubber (controlled by `bin/broadside.rs`) and morphs each ship's silhouette by
   stacking a front face of height `dims.height × cos(θ)` underneath a top face of
   height `dims.beam × sin(θ) / 2`. At θ = 0 ships read as pure side silhouettes; at
   θ = π/2 they read as pure top-down rectangles.
3. **This is the only module that knows about screen coordinates.** Everything else
   in the crate works in cell-space. The rustdoc states this explicitly on line 11.

Line 14: `use crate::geometry::range_band;` — the canonical band-bucket function.
The only engine dependency.

---

### `struct Point2 { x: f32, y: f32 }` (perspective.rs:19)

**Intent:** A 2-D screen-space point. Pixels, y-down origin top-left (the canonical
screen-space convention, matching wgpu's viewport transform). `Copy + Debug +
PartialEq`. The only return type of the projection functions below.

---

### `struct LaneGeometry` (perspective.rs:28)

**Intent:** The flat horizontal strip the cells sit on. Four fields — that's the
entire model now. No back edge, no scale gradient, no slope angle.

Fields:
- `center_y: f32` (line 30) — vertical position of the lane on screen. The lane
  bisects the canvas at this y.
- `x_left: f32` (line 32) — x-coordinate of the leftmost cell's center.
- `x_right: f32` (line 34) — x-coordinate of the rightmost cell's center.
- `cell_count: u32` (line 36) — 5, 7, or 9 per the design doc.

One method on the struct (lines 42–48): `fn cell_width(&self) -> f32` returns half
the distance between two adjacent cells. Used by `hud.rs` to size ship silhouettes
relative to the lane spacing. Handles the `cell_count ≤ 1` degenerate by returning
the full span.

**Drift note: flat-scene refactor (1d4d540, task #55).** This struct used to carry
eight fields — `front_start`, `front_end`, `back_start`, `back_end`, `cell_count`,
`scale_near`, `scale_far`, plus a derived slope from `atan2(dy, dx)` over the front
edge. The flat-scene refactor deleted everything except `cell_count` and replaced
the four corner points with `center_y` + `x_left` + `x_right`. There is no scale
gradient any more; ships at cell 0 are the same size as ships at cell 6 because the
camera is square-on to the lane.

---

### `const DEFAULT_LANE: LaneGeometry` (perspective.rs:54)

**Intent:** Tuned baseline for the engine's 1320×480 virtual canvas. A 7-cell lane
bisecting the window horizontally, with ~130 px margin on each side for ship
overhang.

Constants:
- `center_y: 240.0` — half of `VIRTUAL_H`; the lane is the horizontal midline.
- `x_left: 130.0`, `x_right: 1190.0` — leaves 130 px margin at each end.
- `cell_count: 7`.

The 1320×480 virtual canvas comes from [`src/gfx.rs`](#srcgfxrs) constants
`VIRTUAL_W` / `VIRTUAL_H`. If those ever change, this constant block needs to follow.

**Worked example (`cell_to_screen_endpoints_match_lane_extents`, line 140):** Cell 0
lands at `(130, 240)`; cell 6 at `(1190, 240)`. Both have `y = center_y`; only `x`
varies.

---

### `fn cell_to_screen(cell_index: u32, geom: &LaneGeometry) -> Point2` (perspective.rs:63)

**Intent:** Map an integer cell index to its screen position. Linear interpolation
from `x_left` to `x_right`; `y` is constant.

Body (lines 64–73):
- Line 64: `n = cell_count − 1` (the number of *spans* between cells; n=6 for
  the default 7-cell lane). `saturating_sub(1)` avoids underflow on a one-cell lane.
- Lines 65–69: `t = cell_index / n`, or `0` if `n == 0` (single-cell lane). The
  parametric position along the lane.
- Lines 70–73: return `Point2 { x: x_left + t * (x_right - x_left), y: center_y }`.

**Returns:** the screen-space center of the cell. The renderer (`hud.rs`) uses this
as the *base position* for a ship sprite or projectile.

**Drift note: return type collapsed from `CellScreen` to `Point2`.** The old
function returned a `CellScreen { x, y, scale, rotation_rad }` struct because the
sloped lane required per-cell scale and rotation. The flat lane needs neither, so
the return type is now just the 2-D position. Anything that wanted the old `scale`
field reads it from `LaneGeometry::cell_width()` if needed (constant across cells);
anything that wanted `rotation_rad` no longer needs it (lane is geometrically flat).

**Worked example (`cell_to_screen_midpoint_is_halfway`, line 149):** Cell 3 of 7
lands at `t = 3/6 = 0.5`. `x = 130 + 0.5 × (1190 − 130) = 660`. `y = 240`. Exactly
the center of the canvas.

---

### `fn fractional_cell_to_screen(fractional_cell: f32, geom) -> Point2` (perspective.rs:79)

**Intent:** Continuous version of `cell_to_screen` for fractional positions along
the lane. Used by ordnance entities mid-flight — a torpedo at fractional cell 4.3
is somewhere between cells 4 and 5.

Body identical to `cell_to_screen` *except* line 84 clamps `t` to `[0, 1]` via
`.clamp(0.0, 1.0)` so an out-of-range fractional position (negative, or beyond
`cell_count − 1`) renders at the nearest endpoint rather than off-screen.

**Worked example (`fractional_cell_intermediate_interpolates_linearly`, line 175):**
`fractional_cell = 4.0` on a 7-cell lane. `t = 4/6 ≈ 0.6667`. `x = 130 + 0.6667 ×
1060 ≈ 836.67`. `y = 240`. Straight linear interpolation; no rounding tricks.

---

### `struct ShipDims` and `const FRIGATE_DIMS` (perspective.rs:104, 113)

**Intent:** A ship's design-pixel dimensions in three world-axes. The fields are
the same as before the flat-scene refactor (`length` = bow-stern, `beam` =
port-starboard, `height` = vertical at pure side view), but they're now consumed
by `hud.rs`'s **view-angle-driven morph** rather than baked into a military-
axonometric projection here.

Quoting the doc comment on lines 97–102:

> *The view-angle scrubber stacks a FRONT face of vertical extent
> `height × cos(angle)` underneath a TOP face of vertical extent
> `beam × sin(angle) / 2`. At angle = 0 the top face collapses and the ship reads
> as a pure side silhouette; at angle = π/2 the front face collapses and the ship
> reads as a pure top-down rectangle.*

**`FRIGATE_DIMS = { length: 168.0, beam: 42.0, height: 50.0 }`** (line 113). The
silhouette dominates a single lane cell — `DEFAULT_LANE`'s cell width is ~177
design px (1060 / 6 spans), so a 168-px-long frigate just fits. `beam` is ~25% of
`length` for a recognisable side / top contrast as the view angle scrubs.

**Drift note: ship dims grew ~3×.** Pre-flat the dims were `{ length: 56, beam: 14,
height: 6 }`. Multiple tuning rounds (`8d4a569` → `929fdf1` → `4367e8d` plus
`d4cd468`'s canvas-resize fix) settled at the current `168 / 42 / 50` to give the
flat scene a recognizable silhouette. Bumps were stop-gaps; the values may move
again as content/UX tunes around them. The constant is the only knob.

---

### `enum Stance { BowOn, Broadside }` (perspective.rs:119)

**Intent:** Which way the hull is turned in the rendering frame. Same idea as
before the refactor but consumed by `hud.rs`'s view-angle morph rather than by a
projection function here. `BowOn` ships run along the lane axis (length along x);
`Broadside` ships run perpendicular (length along the depth axis, which the view
angle maps to top-face vertical extent).

**Drift note: separate from `types::Orientation`.** `Orientation` carries the bow
direction; `Stance` does not. The renderer projects the *sprite*, which only cares
about along-lane vs. across-lane. Whoever wires the renderer to ship state does
the mapping `Orientation::BowOn { .. } → Stance::BowOn`, `Orientation::Broadside →
Stance::Broadside`. That conversion sits in `hud.rs`.

---

### `fn band_between_cells(source: u32, target: u32) -> RangeBand` (perspective.rs:127)

**Intent:** Renderer-side convenience wrapper over `geometry::range_band`. Keeps the
renderer code self-contained without reaching across module lines. Both paths MUST
agree — the cross-module test at `perspective.rs:191` asserts agreement over every
`(source, target)` pair in `0..=9 × 0..=9`.

Body (line 128): one line — `range_band(source as usize, target as usize)`. The
`u32 → usize` cast is infallible on any 32-bit-or-wider platform. Same identity-
modulo-cast as the pre-flat version; this is the only public function that survived
the refactor unchanged.

---

### Deleted functions

The flat-scene refactor (`1d4d540`, task #55) deleted six items that previously lived
in this module. Listed here so readers searching the old API find the migration
target:

- **`fn lane_slope_rad`** — gone. The lane has no slope.
- **`struct CellScreen`** + **`fn cell_to_screen`'s extended return** — `cell_to_screen`
  now returns `Point2`. Anything that wanted `scale` reads `LaneGeometry::cell_width()`
  (constant); anything that wanted `rotation_rad` no longer needs it.
- **`type FacePoly = [Point2; 4]`** — gone. Face polygons are computed by `hud.rs`'s
  ship-sprite path against the runtime `view_angle`.
- **`struct ShipSprite`** — gone. Ship projection moved to `hud.rs`; the renderer no
  longer needs a separate projection-output type.
- **`fn ship_sprite(cell, dims, stance) -> ShipSprite`** — gone. The whole
  military-axonometric projection algorithm (front-face rect, top-face rect with
  depth-offset, lane-slope rotation, post-rotation `bow_dir`) is replaced by
  `hud.rs`'s view-angle morph.
- **`fn beam_endpoints(source, target, geom) -> (Point2, Point2)`** — gone. Beam
  endpoints are computed inline in `hud.rs`'s beam-rendering path.
- **`fn cell_footprint(cell_index, geom) -> [Point2; 4]`** — gone. There is no
  trapezoid to highlight; cell highlights are axis-aligned rectangles drawn by
  `hud.rs` directly.

The math these functions encoded — military-axonometric projection, lane-slope
trigonometry — is no longer load-bearing. The new model is conceptually simpler: a
flat 1-D strip, with the *appearance* of perspective driven entirely by `hud.rs`'s
view-angle morph and the parallax planes' `cos(θ) / sin(θ)` reactions.

---

### `#[cfg(test)] mod tests` (perspective.rs:131–203)

7 inline tests, one per public function plus the cross-module drift guard:

```
cell_to_screen_endpoints_match_lane_extents
cell_to_screen_midpoint_is_halfway
cell_to_screen_single_cell_lane_is_safe
fractional_cell_clamps_into_bounds
fractional_cell_intermediate_interpolates_linearly
cell_width_matches_lane_span_divided_by_n_minus_1
band_between_cells_matches_geometry_range_band
```

The last one — **`band_between_cells_matches_geometry_range_band`** — iterates `(s, t)`
in `0..=9 × 0..=9` and asserts the renderer-side wrapper agrees with
`geometry::range_band` on every pair. The canonical drift guard against geometry /
perspective getting out of sync on bucket boundaries. Pinning a different bucket
boundary in either module fires this test before merge.

The other six tests are linear-interpolation sanity checks plus the single-cell
degenerate (`cell_count = 1 → n = 0`, division-by-zero guard must hold).

---

### Drift from the pre-flat module (resolved by `1d4d540` + tuning rounds)

The pre-flat module is documented at commit `47e9670`. Differences as of HEAD:

1. **Lane geometry: flat horizontal strip, not tilted trapezoid.** `LaneGeometry`
   shrank from 8 fields to 4. `front_start` / `front_end` / `back_start` / `back_end`
   / `scale_near` / `scale_far` are gone; only `center_y` / `x_left` / `x_right` /
   `cell_count` remain. The camera is square-on; cells at the far end of the lane
   are the same size as cells at the near end. The visual depth cue comes from the
   view-angle scrubber (in `hud.rs`) and the parallax planes (sky + floor reacting
   to `cos(θ)` / `sin(θ)`), not from per-cell scale.
2. **`cell_to_screen` returns `Point2`, not `CellScreen`.** Sprite scale is constant
   (read via `LaneGeometry::cell_width()` if a caller needs the cell pitch); there
   is no per-cell rotation because the lane has no slope.
3. **Ship projection moved to `hud.rs`.** The military-axonometric algorithm
   (front-face + top-face polygons with depth-offset, lane-slope rotation,
   post-rotation `bow_dir`) is gone. `hud.rs` now stacks a front face of vertical
   extent `height × cos(θ)` underneath a top face of `beam × sin(θ) / 2`, morphing
   the silhouette as the view-angle scrubber moves. At θ = 0 ships read as pure
   side silhouettes; at θ = π/2 they read as pure top-down rectangles. Default is
   45° per task #57 (`2caa712`).
4. **Ship dimensions grew ~3× to fill the flat-scene cell.** `FRIGATE_DIMS` went from
   `{ length: 56, beam: 14, height: 6 }` (designed for the small old `DEFAULT_LANE`)
   to `{ length: 168, beam: 42, height: 50 }` after multiple tuning rounds. The
   `length` is now ~95% of the default cell width (1060/6 ≈ 177 px).
5. **TS is no longer canonical for this file.** The flat scene is a deliberate
   Rust-port-specific direction the analysis doc didn't specify. The TS
   `perspective.ts` still exists in the reference repo, but the Rust module's
   rustdoc no longer cites it as the tie-breaker.

**Three pre-flat drifts no longer apply** because the underlying functions are gone:

- ~~`(pivot, rotation_rad)` vs pre-baked SVG transform string~~ — `ShipSprite`
  deleted.
- ~~`[Point2; 4]` polygon arrays vs formatted strings~~ — `FacePoly` deleted.
- ~~rotation in radians, not degrees~~ — `cell_to_screen` no longer returns a
  rotation field.

**One pre-flat drift survives:** the `bandBetweenCells` → `band_between_cells`
snake_case rename. That function is unchanged.

**No open architectural items.** The flat-scene refactor took perspective.rs from a
mid-complexity projection module to a near-trivial coordinate transform. If ship
sprites or beams need new rendering primitives in the future, they're more likely
to land in `hud.rs` than here — this module's API is essentially complete.


---

## `src/gfx.rs`

*wgpu state, four render pipelines, and the virtual-resolution presentation model.
Owns the GPU device, the swapchain, the procedural sprite atlas, the offscreen
target, and per-frame draw dispatch. Reads a `Vec<DrawCommand>` from
[`src/hud.rs`](#srchudrs) once per frame and turns it into GPU work.*

**Mirrors:** Ported from `GameEngine/mvp/src/gfx.rs` and adapted for Broadside (four
called-out structural changes; see Drift below).
**Design anchor:** Tasks #7 (Slice A — wgpu pipeline scaffold) + #28–#30 (the atlas /
hud / demo-board slices that built on top) + #46 (animation tweens) + #58 (camera-
revolves model) + #64 (sprite spec + side/top interpolation scaffold).
**Source commit:** stabilized at `95b94a6`. 1635 lines, no inline tests — `gfx.rs`
has no `#[cfg(test)]` module (`atlas.rs` carries the renderer-adjacent test coverage
with its own 7 tests). Reviewer audited.

### Module rustdoc (lines 1–28)

A 28-line `//!` block. The four structural changes from the source `GameEngine/mvp`
engine, quoted from the rustdoc verbatim because they're the canonical statement:

1. **Virtual resolution is 1320×480** (2× of the design doc's historical 660×240).
   Integer-scales cleanly on a 2560×1440 monitor (1× and 2×); keeps the
   `perspective::DEFAULT_LANE` coordinates usable after a uniform 2× map.
2. **The view uniform projects ONTO the virtual-pixel grid:** world is
   `[0, VIRTUAL_W] × [0, VIRTUAL_H]` with y-down. The source engine used a NDC-half-
   size world; Broadside feeds raw pixel coordinates from `crate::perspective`
   straight through. Y is flipped in the vertex shader so screen-space conventions
   match `perspective::cell_to_screen`.
3. **The atlas comes from `crate::atlas`** (Broadside content), not the source's
   humanoid set.
4. **The clear color is deep-space ink (`#080c14`)**, matching the analysis HTML's
   `--ink` token. Pre-converted to linear at the top of the file because the
   offscreen target is sRGB — wgpu interprets `wgpu::Color` linearly when the target
   is sRGB.

Two passes per frame, unchanged in spirit from the source:

1. **Sprite pass** — instanced colored quads drawn into the 1320×480 offscreen
   target. Every game pixel is one texel here.
2. **Blit pass** — the offscreen texture is sampled with nearest-neighbor filtering
   and drawn to the swapchain at the largest integer scale that fits the window.
   The leftover area is letterboxed.

**The `BlitPipeline` is the only thing that touches the swapchain's sRGB format.**
Everything else renders to `OFFSCREEN_FORMAT = Rgba8UnormSrgb`. The pre-converted
linear `CLEAR` color above is the consequence — a Slice-A papercut bruce reported in
early playtests.

---

### Constants and capacity ceilings (lines 38–69)

| Constant                        | Value      | Role                                                                       |
|---------------------------------|-----------:|----------------------------------------------------------------------------|
| `VIRTUAL_W` (line 40)           | `1320`     | Virtual-pixel canvas width.                                                |
| `VIRTUAL_H` (line 41)           | `480`      | Virtual-pixel canvas height. Lane bisects at y = 240.                      |
| `MAX_SPRITES` (line 47)         | `4096`     | Hard ceiling on `SpriteInstance` count per frame.                          |
| `MAX_POLYGONS` (line 51)        | `256`      | Hard ceiling on `PolygonInstance` count per frame.                         |
| `MAX_TEXTURED_SHIPS` (line 56)  | `16`       | Hard ceiling on textured-ship draws per frame.                             |
| `OFFSCREEN_FORMAT` (line 58)    | `Rgba8UnormSrgb` | Format for the virtual-res offscreen target.                         |
| `CLEAR` (lines 63–68)           | `0.001214,0.002428,0.006995,1.0` | Deep-space ink, pre-converted to linear.            |
| `LETTERBOX` (line 69)           | `0,0,0,1`  | Black bars outside the integer-scaled blit.                                |

**Capacity-ceiling behaviour is load-bearing:** all three `MAX_*` are **hard
pre-allocated ceilings**, not high-water marks. The instance buffers are sized once
at startup in each pipeline's `::new` (e.g. `MAX_SPRITES * sizeof::<SpriteInstance>()`)
and never reallocate. On overflow `Gfx::render` **silently drops** extra commands
(the `if (sprite_buf.len() as u64) >= MAX_SPRITES { continue }` branch at line 921)
and emits a `log::warn!` once per frame if the total `commands.len()` exceeds
`MAX_SPRITES + MAX_POLYGONS + MAX_TEXTURED_SHIPS` (lines 958–962).

**Symptom if a future scene blows the cap:** "stuff stops appearing" — not "panic."
This is intentional graceful degradation so a runaway scene doesn't crash the
playtest, but it's a quiet failure mode worth knowing about. The original values
(4096 / 256 / 16) were set in Slice A and have generous headroom over the current
demo board (~100–150 sprite instances + ~30 polygons per frame). Bumping a constant
only costs one extra VRAM allocation at startup; the buffer is reused frame-to-frame.

---

### Instance shapes (lines 71–224)

Four `#[repr(C)] bytemuck::Pod` shapes plus a `Copy` slug helper. These are the
data the GPU vertex stage reads.

#### `struct QuadVertex` + `const QUAD_VERTS` (lines 71–84)

Six 2D vertex positions in CCW order describing the unit quad (two triangles, six
verts). The sprite pipeline binds this once and uses it for every instance — only
the `SpriteInstance` data changes per draw.

#### `struct SpriteInstance` (lines 98–108)

The bread-and-butter instance for rotated, atlas-sampled rectangles:

| Field          | Type      | Role                                                          |
|----------------|-----------|---------------------------------------------------------------|
| `pos`          | `[f32;2]` | Rectangle center in virtual-pixel space.                      |
| `half_size`    | `[f32;2]` | Half-width / half-height. Quad spans `pos ± half_size`.       |
| `color`        | `[f32;4]` | Multiplies the sampled atlas texel. `1,1,1,1` = no tint.      |
| `uv_min`       | `[f32;2]` | Atlas UV top-left.                                            |
| `uv_max`       | `[f32;2]` | Atlas UV bottom-right.                                        |
| `rotation_rad` | `f32`     | Rotation around `pos` in radians.                             |
| `_pad`         | `[f32;3]` | Padding to 64 bytes for std140 / GPU alignment.               |

**`rotation_rad` is per-instance only on `SpriteInstance`.** `PolygonInstance` and
`TexturedShipInstance` have no rotation field — their corners are explicit. If a
caller wants a rotated textured ship, they precompute rotated corners on the CPU.
The design decision: ship facing is encoded in the bow chevron / sprite asymmetry,
not in instance rotation.

`SpriteInstance::axis_aligned(pos, half_size, color, uv) -> Self` (line 112) is the
convenience for the common case where rotation is zero — most HUD elements use it.

#### `struct PolygonInstance` (lines 139–153)

Four explicit corners (CCW with screen y-down: top-left, top-right, bottom-right,
bottom-left) plus tint and UV rect. Used for shapes the rotation-around-center
`SpriteInstance` cannot represent without pixel staircase — primarily the lane plate
parallelogram (`#37` audit fix) and ship-face polygons under forced perspective.

`PolygonInstance::flat(corners, color, solid_white_uv) -> Self` (line 160) builds a
flat-tint polygon by pointing the UVs at the atlas's `SOLID_WHITE` cell. The caller
supplies that UV rect to keep this module decoupled from `crate::atlas`.

#### `struct SpriteSlug` (lines 177–194)

A 32-byte inline-storage string identifier for loaded ship sprites. `Copy + Eq +
Hash`. Used inside `TexturedShipInstance` and as part of the `ship_bg_cache` key,
which both require `Copy` — a `String` won't do. Truncates silently at 31 bytes; the
SPRITE_SPEC defines every legal slug well under that.

#### `struct TexturedShipInstance` (lines 204–214)

Per-ship textured-quad draw. Four explicit corners + a `blend_t: f32` + two
`SpriteSlug`s (side, top). The bbox quad matches what the procedural silhouette
would produce; the fragment shader samples both `side` and `top` textures (looked up
via the slugs at draw time) and blends them by `blend_t = sin(view_angle)`.

Emitted by `hud::push_ship` only when both side and top PNGs are registered for the
ship's `class_stance`. Otherwise the procedural polygon-set is emitted instead.

#### `enum DrawCommand` (lines 219–236)

Three variants: `Sprite(SpriteInstance)`, `Polygon(PolygonInstance)`,
`TexturedShip(TexturedShipInstance)`. Plus `From` impls for each variant so call
sites can write `sprite.into()`.

**This is the hud↔gfx contract.** `hud::compose_scene` produces a `Vec<DrawCommand>`
in back-to-front order; `Gfx::render` consumes it. `DrawCommand` is `Copy`, which is
load-bearing — `SpriteSlug`'s inline storage exists specifically to keep this enum
`Copy`.

#### Uniform shapes (lines 238–261)

Three small `#[repr(C)] bytemuck::Pod` uniforms that flow CPU → GPU:

- **`ViewUniform { px_to_ndc, _pad }`** — `[2.0/VIRTUAL_W, 2.0/VIRTUAL_H]`.
  Multiplying a virtual-pixel position by this gives NDC half-extent; subtracting
  1.0 maps to `[-1, 1]`. Y is flipped in the vertex shader so virtual-pixel `(0, 0)`
  is the top-left corner of the offscreen — same y-down convention as
  `perspective::cell_to_screen`.
- **`BlitUniform { ndc_min, ndc_max }`** — the integer-scaled, letterboxed NDC
  rectangle on the swapchain. Recomputed by `update_blit_uniform` on every resize.
- **`BlendUniform { blend_t, _pad }`** — per-textured-ship blend factor. Padded to
  16 bytes for wgpu's uniform alignment.

---

### Shaders (lines 263–478)

Four inline WGSL string literals.

#### `SPRITE_SHADER` (lines 263–321)

The vertex stage rotates the quad-local vertex by `i_rotation` around the instance
center, translates by `i_pos`, then maps virtual-pixel coordinates to NDC via
`view.px_to_ndc`. **Y-flip happens here:** `ndc_y = 1.0 - pixel.y * view.px_to_ndc.y`
(line 301) — virtual-pixel `(0, 0)` lands at clip-space top-left.

The fragment stage samples the atlas at the interpolated UV and multiplies by the
instance color tint. UV interpolation flips y too (line 308–312) so the *top* of the
atlas cell shows at the *top* of the quad in screen space.

#### `POLYGON_SHADER` (lines 329–383)

Same view uniform + atlas binding as `SPRITE_SHADER`. The vertex stage uses
`vertex_index` to pick one of four explicit corner positions per instance (two
triangles: `0-1-2` and `0-2-3`); UVs are barycentrically blended across the polygon
from `uv_min` to `uv_max` with the same y-flip convention.

#### `TEXTURED_SHIP_SHADER` (lines 389–443)

Same vertex layout as `POLYGON_SHADER` (four explicit corners expanded by
`vertex_index`), but the bind group is different. The view uniform sits at
`@group(0)`; the per-ship `BlendUniform` + side texture + top texture + sampler all
sit at `@group(1)`. The fragment stage samples both textures and blends with
`mix(side_px, top_px, ship.blend_t)`.

The blend formula `blend_t = sin(view_angle)` is set by the caller; at `view_angle =
π/4` (the default) the blend favours the top sprite about 70/30. The SPRITE_SPEC
documents this dominance ratio as intentional — at 45° the camera is already looking
more down than across, so the top silhouette carries the orientation cue.

#### `BLIT_SHADER` (lines 445–478)

The simplest of the four. Takes a `BlitUniform { ndc_min, ndc_max }`, builds a
quad-from-vertex-index in those NDC coordinates, samples the offscreen target with
nearest filtering. No tint, no rotation. The only shader that targets the swapchain
directly.

---

### `struct Gfx` and the four pipeline owners (lines 480–558)

`Gfx` owns the surface, device, queue, surface config, the offscreen view, all four
pipelines, plus two HashMaps for ship-sprite textures and per-slot bind groups:

```rust
pub struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    offscreen_view: wgpu::TextureView,
    sprites: SpritePipeline,
    polygons: PolygonPipeline,
    textured_ships: TexturedShipPipeline,
    blit: BlitPipeline,
    ship_sprites: HashMap<String, ShipSpriteEntry>,
    ship_bg_cache: HashMap<(u32, SpriteSlug, SpriteSlug), wgpu::BindGroup>,
}
```

#### `SpritePipeline` (line 514)

Owns the **view UBO** (the single one shared by every pipeline), the quad vertex
buffer, the per-instance buffer, and a bind group binding `[view_ubo, atlas_view,
atlas_sampler]` at `@group(0)`.

**Load-bearing ownership detail:** *the shared view UBO is owned by
SpritePipeline.* `PolygonPipeline` and `TexturedShipPipeline` borrow it via
`&sprites.view_ubo` at construction time and bind it into their own bind groups.
If `SpritePipeline` were ever dropped, the other two would dangle. The
construction order in `Gfx::new` (lines 684–687) enforces this: sprites first,
then polygons + textured ships + blit can borrow from it.

#### `PolygonPipeline` (line 522)

Owns its own instance buffer and a bind group identical in shape to
SpritePipeline's — same view UBO, same atlas texture, same sampler. The pipeline
itself differs (different shader, different vertex attribute layout — explicit
corners rather than quad-plus-instance).

#### `TexturedShipPipeline` (line 536)

The most complex of the four. Owns its instance vbuf, a per-textured-ship
`Vec<wgpu::Buffer> blend_ubos` of length `MAX_TEXTURED_SHIPS`, the view bind group
at `@group(0)`, the per-ship bind group **layout** (`ship_bgl`) used to build
cached `(side, top)` bind groups on demand, and a shared sampler.

**Per-ship bind groups are slot-keyed:** `Gfx::ship_bg_cache` is keyed by
`(slot_idx: u32, side: SpriteSlug, top: SpriteSlug)` (line 502). The `slot_idx` is
in the key because each slot has its own pre-allocated `blend_ubo[slot_idx]`, so
the bind group must reference the correct slot's UBO. Two ships drawn at slots 3
and 7 with the same `(side, top)` slug pair get *different* bind groups.

#### `BlitPipeline` (line 554)

Owns the blit uniform buffer and a bind group binding `[blit_ubo, offscreen_view,
offscreen_sampler]`. Targets the swapchain (not the offscreen) — the only pipeline
that does.

#### `impl crate::sprites::SpriteRegistry for Gfx` (lines 560–569)

Implements the trait by delegating to `Gfx::has_ship_sprite`. Lets `hud::push_ship`
decide whether to emit a `TexturedShip` draw or fall back to the procedural
polygon-set without holding a `&Gfx` directly.

---

### `Gfx::new(window) -> Self` (lines 576–711)

The async constructor. Builds everything in this order, which is load-bearing:

1. **Surface** from the window (line 580).
2. **Adapter** (line 584) with `HighPerformance` power preference. Logged at INFO.
3. **Device + queue** (line 595) with default limits, `Trace::Off`.
4. **Surface configuration** (line 613) — chooses the first sRGB format the
   surface supports, sets `present_mode = AutoVsync`, sizes to the window.
5. **Offscreen virtual-res target** (line 625) — 1320×480 RGBA8 sRGB,
   `RENDER_ATTACHMENT | TEXTURE_BINDING` (rendered to by passes 1, sampled by
   pass 2).
6. **Atlas texture** (line 641) — calls `atlas::generate_atlas()` to produce the
   256×256 RGBA8 bytes, creates the texture, uploads with `queue.write_texture`,
   builds the texture view and a nearest-filtering `ClampToEdge` sampler.
7. **Four pipelines** (lines 684–687), in order:
   - `SpritePipeline::new(device, atlas_view, atlas_sampler)` — *creates* the
     shared view UBO.
   - `PolygonPipeline::new(device, &sprites.view_ubo, atlas_view, atlas_sampler)` —
     *borrows* the view UBO.
   - `TexturedShipPipeline::new(device, &sprites.view_ubo)` — *borrows* the view
     UBO; does not bind the atlas (textured ships sample their own loaded PNGs).
   - `BlitPipeline::new(device, format, &offscreen_view)` — binds the offscreen
     view as its source texture, targets the swapchain `format`.
8. **Write the view uniform** (line 703) — `[2.0/VIRTUAL_W, 2.0/VIRTUAL_H]`. Done
   once at startup; never changes (virtual resolution is fixed).
9. **Compute the blit uniform** (line 709) — calls `update_blit_uniform()` to set
   the initial letterbox rectangle.

The function takes `Arc<Window>` and is `async` because wgpu adapter / device
acquisition is async. Bruce's `bin/broadside.rs` resolves the future with
`pollster::block_on` (visible in #69's binary).

---

### `Gfx::resize` and `Gfx::reconfigure` (lines 713–725)

Both reconfigure the surface to a new size (or the same size if `reconfigure`)
and recompute the blit uniform so the letterboxing tracks. `resize` skips the
work if either dimension is zero (window minimised). The offscreen target is
**not** resized — virtual resolution is fixed at 1320×480 regardless of window
size; the blit step does the integer-scaling.

---

### Ship-sprite loader (lines 735–815)

Three public + one private function for loading PNG ship sprites at runtime:

- **`try_load_ship_sprites(asset_dir) -> usize`** (line 735) — walks the
  `<class>_<stance>_<view>` slug space (3 classes × 3 stances × 2 views = 18
  combinations) and calls `crate::sprites::load_sprite` for each. Missing files
  are skipped; returns the count loaded. Clears `ship_bg_cache` first because
  the underlying texture views may have changed.
- **`upload_ship_sprite(slug, img)`** (line 766) — private. One sprite to one
  GPU texture; inserts the texture view + dimensions into `ship_sprites`.
- **`has_ship_sprite(class, stance, view) -> bool`** (line 807) — query whether
  a slug has been loaded. Used by `hud.rs` (via the `SpriteRegistry` trait impl)
  to decide whether to emit `TexturedShip` or the procedural fallback.
- **`ensure_ship_bind_group(slot_idx, side, top)`** (line 821) — private. Build
  the per-slot `(slot_idx, side, top)` bind group on first request and cache it
  in `ship_bg_cache`. If either texture slug is missing from `ship_sprites`, the
  cache entry is **not** populated — the render loop checks the cache and skips
  the draw if absent (the procedural polygon below stays visible).

The cache invalidation contract: any time `ship_sprites` is mutated,
`ship_bg_cache` must be cleared. The only mutator today is
`try_load_ship_sprites`, which does the clear at the top (line 738).

---

### `update_blit_uniform` (lines 867–889) — the letterbox math

The single non-obvious piece of arithmetic in the file. Given the current swapchain
size `(w, h)`, compute the largest integer scale `s` such that `s × VIRTUAL_W ≤ w`
and `s × VIRTUAL_H ≤ h`:

```rust
let scale = (w / VIRTUAL_W).min(h / VIRTUAL_H).max(1);
```

`.max(1)` floors at 1× (the offscreen always draws at native size or larger; never
downscaled). **On a window smaller than 1320×480 the scale is still clamped to 1**,
which means the offscreen extends past the visible swapchain — by design, we never
downscale game pixels. Bruce will never see this on a 2560×1440 monitor, but the
behavior matters for anyone running on a smaller display. Then the scaled offscreen
size is `(s × VIRTUAL_W, s × VIRTUAL_H)` and the centering offsets are
`((w - scaled_w) / 2, (h - scaled_h) / 2)`. The four NDC corners follow from
converting `(offset, offset + scaled)` to NDC via the standard `pixel/dim × 2 - 1`
formula (with y flipped because NDC y-up vs swapchain y-down).

The resulting `BlitUniform` is written to `self.blit.ubo` and consumed by the blit
vertex shader's `mix(blit.ndc_min, blit.ndc_max, v)` per corner.

**Recomputed on every `resize` / `reconfigure`.** Not per-frame — the swapchain
dimensions only change on resize.

---

### `Gfx::render(&[DrawCommand]) -> Result<(), SurfaceError>` (lines 896–1089)

The frame-dispatch hot path. Two phases:

#### Phase 1: collect-and-batch (lines 900–963)

Walks `commands` once. For each `DrawCommand`:

- **`Sprite(s)`** — push `s` into `sprite_buf`. If `sprite_buf.len() >= MAX_SPRITES`
  (line 921), `continue` — silently truncate. If the previous batch was also
  `Sprite`, extend its count; otherwise start a new batch.
- **`Polygon(p)`** — same shape, against `polygon_buf` / `MAX_POLYGONS`.
- **`TexturedShip(t)`** — same overflow check against `MAX_TEXTURED_SHIPS`. Pushes
  the four corner positions into `ship_corner_buf` and the metadata (side slug,
  top slug, blend factor) into `ship_meta`. **Always its own batch** (line 954) —
  each textured ship has its own bind group, so they can't be batched together.

After the walk, if `commands.len() > MAX_SPRITES + MAX_POLYGONS + MAX_TEXTURED_SHIPS`
emit the per-frame `log::warn!` (lines 958–962). Upload all three instance buffers
in one `queue.write_buffer` each (only if non-empty). For each textured-ship slot,
write the `BlendUniform` and call `ensure_ship_bind_group(i, side, top)`.

#### Phase 2: encode-and-submit (lines 997–1089)

Two render passes inside one encoder:

**Pass 1: scene → offscreen** (lines 1009–1062). Clears to the deep-space ink
`CLEAR` color, then walks `batches` in order. For each batch:

- `BatchKind::Sprite` — set sprite pipeline, bind group, quad + instance vbufs,
  `draw(0..6, b.start..b.start+b.count)`.
- `BatchKind::Polygon` — set polygon pipeline, bind group, instance vbuf,
  `draw(0..6, b.start..b.start+b.count)` (6 verts × N instances; the polygon
  shader expands two triangles per instance).
- `BatchKind::TexturedShip(slot_idx)` — look up the slot's bind group in
  `ship_bg_cache`. If missing (PNG not loaded), `continue` — skip silently; the
  procedural polygons emitted alongside stay visible as the fallback. Otherwise
  set the textured-ship pipeline, bind groups 0 and 1, slice the instance vbuf
  at this slot's 32-byte offset, `draw(0..6, 0..1)`.

**Pass 2: offscreen → swapchain** (lines 1065–1084). Clears the swapchain to
`LETTERBOX` black, sets the blit pipeline + its bind group, `draw(0..6, 0..1)`.
The blit vertex shader uses the precomputed `BlitUniform` to position its single
quad at the integer-scaled, letterboxed rectangle.

Submits the encoder, presents the frame, returns `Ok(())`.

**Two render passes per frame, one queue submit.** No depth buffer (every draw is
strictly back-to-front in the input command list; the renderer is painter's-
algorithm by construction).

---

### Pipeline `::new` constructors (lines 1092–1635)

Four constructor functions, one per pipeline. The patterns are very similar; the
deltas are what matter.

#### `SpritePipeline::new(device, atlas_view, atlas_sampler)` (line 1093)

Builds:
1. **WGSL shader module** from `SPRITE_SHADER`.
2. **View UBO** (`std::mem::size_of::<ViewUniform>()`, `UNIFORM | COPY_DST`).
3. **Bind group layout** with three entries: uniform buffer (vertex stage),
   filterable 2D texture (fragment stage), filtering sampler (fragment stage).
4. **Bind group** binding `[view_ubo, atlas_view, atlas_sampler]`.
5. **Pipeline layout** referencing the single bgl.
6. **Render pipeline** with vertex state describing two vertex buffers:
   - Buffer 0 (`Vertex` step mode): `QuadVertex { pos: [f32;2] }`.
   - Buffer 1 (`Instance` step mode): six attributes mapping `SpriteInstance`'s
     layout (`pos`, `half_size`, `color`, `uv_min`, `uv_max`, `rotation_rad`).
     Offsets are hand-tabulated (lines 1188–1193).
   - Fragment state targeting `OFFSCREEN_FORMAT` with `ALPHA_BLENDING`.
   - `PrimitiveTopology::TriangleList`, `FrontFace::Ccw`, no culling.
7. **Quad vbuf** (line 1223) — initialized from `QUAD_VERTS` at startup; static.
8. **Instance vbuf** (line 1229) — sized `MAX_SPRITES * sizeof(SpriteInstance)`,
   written per-frame.

#### `PolygonPipeline::new(device, view_ubo, atlas_view, atlas_sampler)` (line 1246)

Borrows the view UBO from caller. Builds essentially the same scaffolding as
SpritePipeline but with one vertex buffer instead of two (the polygon shader pulls
corners from the instance directly via `vertex_index`). The bind group layout is
**byte-identical** to SpritePipeline's at the wgpu level — same bindings, same
visibility flags, same types — which is what lets the two pipelines share the view
UBO and atlas binding without a bind-group rebuild between them in `Gfx::render`.
The pipeline itself differs in shader and vertex attribute count.

#### `TexturedShipPipeline::new(device, view_ubo)` (line 1363)

Two bind group layouts:
- **Group 0** (view UBO) — identical to sprites/polygons.
- **Group 1** (per-ship) — uniform buffer (fragment stage, for the
  `BlendUniform`), two filterable 2D textures (side, top), filtering sampler.
  Cached as `self.ship_bgl` for later bind-group construction.

Pre-allocates `MAX_TEXTURED_SHIPS` blend UBO buffers (line ~1505), one per slot
index. Each is 16 bytes (padded `BlendUniform`). Storing as one big buffer with
dynamic offsets would be neater but the count is tiny so individual buffers are
simpler.

The pipeline uses **the same vertex layout as PolygonPipeline** (four explicit
corners per instance, expanded by `vertex_index`) but a different shader and a
different second bind group.

#### `BlitPipeline::new(device, target_format, offscreen_view)` (line 1522)

The simplest. Builds:
1. WGSL shader from `BLIT_SHADER`.
2. Bind group layout: uniform (`BlitUniform`, fragment), 2D texture (the
   offscreen view), non-filtering sampler (nearest-neighbor only).
3. Sampler with `Nearest` filtering and `ClampToEdge` addressing in both axes —
   the crisp-pixel look depends on this.
4. Bind group binding the three resources.
5. The blit uniform buffer (16 bytes).
6. Pipeline targeting `target_format` (the swapchain's sRGB format) with no
   blending. Empty vertex state — the shader generates its quad from
   `vertex_index` alone.

---

### Drift watch list (resolved by `95b94a6`)

Four called-out structural deltas from `GameEngine/mvp/src/gfx.rs` — these are the
intentional ports, not bugs:

1. **Virtual resolution: 1320×480** (Broadside) vs the source engine's NDC half-
   extent world. Pixel coords flow straight from `perspective::cell_to_screen`
   into the shader; the source needed an NDC mapping step.
2. **Y-down convention** (Broadside) vs y-up (source). The vertex shader's
   `ndc_y = 1.0 - pixel.y * view.px_to_ndc.y` flip is the load-bearing line.
   Matches `perspective::cell_to_screen` (also y-down). UV interpolation flips y
   too (`mix(uv_max.y, uv_min.y, ...)`) so atlas-top shows at screen-top.
3. **Procedural atlas from `crate::atlas`** vs the source's humanoid atlas. The
   atlas is generated at startup, uploaded once, and never changes — see
   [`src/atlas.rs`](#srcatlasrs).
4. **Per-instance `rotation_rad` on `SpriteInstance`** added vs the source
   engine. Lets axis-aligned HUD elements and lane-slope-aligned sprites share
   one pipeline. Only `SpriteInstance` has it; the polygon and textured-ship
   instances pre-compute rotated corners on the CPU.

**New decisions documented in this pass:**

- **Hard capacity ceilings with silent truncation.** `MAX_SPRITES = 4096`,
  `MAX_POLYGONS = 256`, `MAX_TEXTURED_SHIPS = 16`. Set in Slice A, never bumped.
  Symptom on overflow: "stuff stops appearing" + a once-per-frame `log::warn!`.
  Bumping the constant only costs one VRAM alloc at startup.
- **View UBO ownership lives on `SpritePipeline`.** The other two pipelines
  borrow it at construction. Construction order in `Gfx::new` enforces this.
- **`ship_bg_cache` keys on `(slot_idx, side, top)`.** Each slot has its own
  pre-allocated blend UBO, so the bind group is slot-specific. The `slot_idx`
  in the key is non-obvious GPU state management.
- **`BlitPipeline` is the only pipeline that touches the swapchain's sRGB
  format.** Everything else renders to `OFFSCREEN_FORMAT = Rgba8UnormSrgb`. The
  pre-converted-to-linear `CLEAR` color is the consequence — Slice-A papercut
  bruce reported.
- **Two render passes per frame, one queue submit, no depth buffer.** Painter's-
  algorithm by construction; the command list must be in back-to-front order.
- **No `#[cfg(test)]` module in `gfx.rs` itself.** GPU code is tested in
  isolation via the `atlas.rs` helpers and via integration runs. Per `95b94a6`
  reviewer audit: gfx-side coverage relies on visual confirmation + the
  procedural-fallback safety net (textured-ship missing → procedural polygons
  still draw).

No open architectural items.

---

## `src/atlas.rs`

*The procedural sprite atlas. Generates a single 256×256 RGBA8 texture at startup,
packed as an 8×8 grid of 32×32 cells, holding every HUD glyph, projectile sprite,
status badge, telegraph icon, parallax pixel-art tile, and a single SOLID_WHITE cell
for flat-color tinted quads. Decorative only — ship hulls are drawn as procedural
polygon silhouettes by `hud.rs`, so the atlas does **not** carry ship art.*

**Mirrors:** No TS analog. The TS engine is headless; there is no `atlas.ts`.
**Atlas is a pure Broadside-port concern from day one — the absence of a Drift
section in this walkthrough is intentional, not an oversight.**
**Design anchor:** Task #28 (Slice C — flesh out atlas with ship faces, chevron,
ordnance, HUD glyphs, parallax art). Cell layout canonically documented in
[`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) § "Atlas slot allocation"; this
walkthrough explains *the generation* and *the constants*, not the slot map.
**Source commit:** stabilized through Slice C / D. 818 lines, 7 inline tests, all
green. Reviewer audited.

### Module rustdoc (lines 1–17)

A 17-line `//!` block. Three things every reader needs to know up front:

1. **Fixed 8×8 grid of 32×32 cells = 256×256 total.** A cell is referenced by
   `(col, row)`; `cell_uvs((c, r))` converts that to the normalized UV rectangle
   the sprite shader samples.
2. **The atlas is decorative.** Ship hulls are drawn by `hud.rs` as procedural
   tinted polygons (using the `SOLID_WHITE` cell as the texture source + the
   instance color tint). The atlas does not carry ship-class art. Direction-
   specific detail the polygon math can't supply — bow chevron, torpedo
   silhouette — lives here; ship sprites do not.
3. **Palette is sampled from the analysis HTML's CSS tokens.** `--ink`, `--gold`,
   `--vermillion`, `--c-beam`, `--c-ord`, … transcribed to RGBA at the top of the
   palette section. Each cell function picks a few to stay on-brand with the
   design document's visual language.

### Why an 8×8 grid of 32×32 cells

Three design decisions encoded in the constants:

- **8×8 grid (one texture binding for the whole HUD/parallax/glyph surface).**
  The sprite pipeline and polygon pipeline both sample from a single atlas
  texture binding (group 0, binding 1 in `gfx.rs`). Packing every glyph into one
  256×256 texture means *no per-cell texture switches* — every HUD draw is one
  pipeline rebind at most. If glyphs lived in separate textures, the renderer
  would have to either bind each at draw time (kills batching) or keep a
  per-glyph bind group.
- **32×32 cells (smallest unit that holds readable detail at native virtual res).**
  At 1320×480 a 32-px cell is ~2.5% of the canvas width — about the right size
  for a status badge or queue glyph at native scale. Smaller would lose
  recognisability; larger would waste the grid.
- **Procedural, not baked.** `generate_atlas()` runs once at startup and produces
  the texture in memory. No PNG asset file in the repo, no asset-versioning
  story for art tweaks: change a `draw_*` function, rebuild, the new atlas ships
  with the binary. Bruce's hand-painted ship sprites are the exception — they
  *are* PNG assets loaded by `sprites.rs` and live outside this module.

---

### Constants (lines 18–20)

| Constant         | Value | Role                                                    |
|------------------|------:|---------------------------------------------------------|
| `ATLAS_SIZE`     | `256` | Side length of the RGBA8 texture in pixels.             |
| `CELL_SIZE`      | `32`  | Side length of one cell in pixels.                      |
| `CELLS_PER_ROW`  | `8`   | `ATLAS_SIZE / CELL_SIZE`. Derived, not free-set.        |

The constants are public so `gfx.rs::Gfx::new` can use `ATLAS_SIZE` when sizing the
GPU texture and `CELL_SIZE * 4` for the bytes-per-row on upload.

---

### Cell map (lines 22–68)

26 named `(col, row)` cell coordinates, grouped by row. The full slot allocation
table is in [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) § "Atlas slot allocation";
this section summarises the row-by-row intent.

| Row | Content                                                              |
|-----|----------------------------------------------------------------------|
| 0   | Projectiles + chevron: `BOW_CHEVRON` (0,0), `TORPEDO` (1,0), `MISSILE` (2,0). |
| 1   | Action-queue glyphs — one per `WeaponArchetype`, 7 total.            |
| 2   | Telegraph intent icons — 6.                                          |
| 3   | Status badges — 4 (`HullBreach`, `SystemsOffline`, `TargetLock`, `ShieldsUp`). |
| 4   | Parallax layer art — 5 (far stars, nebula, distant planet, mid stars, foreground dust). |
| 5–6 | Reserved for future ship-class detail / decals.                       |
| 7   | `SOLID_WHITE` at (7, 7) — the flat-color tint source.                |

`SOLID_WHITE` is the workhorse of the entire renderer: every heat bar, range-band
tick, ship face, lane plate, end-state overlay samples this single cell and lets
the per-instance `color` tint do the actual coloring. Tested explicitly at
`solid_white_cell_is_white` (line 719).

---

### `fn cell_uvs(cell: (u32, u32)) -> ([f32; 2], [f32; 2])` (line 72)

**Intent:** Convert a `(col, row)` cell coordinate to a `(uv_min, uv_max)` tuple in
normalized `[0, 1]` texture space. The single function call sites use to derive UV
coordinates for sprite + polygon instances.

Body (lines 73–78):
- `s = CELL_SIZE / ATLAS_SIZE` (i.e. `32 / 256 = 0.125`).
- `uv_min = (c * s, r * s)`.
- `uv_max = ((c + 1) * s, (r + 1) * s)`.

Linear math, no edge cases. Tested at `cell_uvs_at_origin_is_unit_cell` (line 696)
and `cell_uvs_at_corner_is_inside_unit_square` (line 705).

---

### `fn generate_atlas() -> Vec<u8>` (line 83)

**Intent:** Generate the entire atlas as a tight RGBA8 byte buffer
(`ATLAS_SIZE * ATLAS_SIZE * 4 = 262144` bytes). Called once by `Gfx::new` and
uploaded to GPU; the buffer is dropped after upload.

Body structure (lines 84–121):

1. **Allocate** a zero-filled buffer of `262144` bytes (line 84). All cells start
   as transparent black `(0, 0, 0, 0)`.
2. **Fill `SOLID_WHITE` first** (line 88) — explicit comment at lines 86–87 notes
   *"so every tinted-quad path works even if the rest of the atlas hasn't run
   yet."* If any subsequent `draw_*` call panicked, the renderer would still have
   a working flat-color cell.
3. **Call each named `draw_*` function in row order** (lines 90–118). One call per
   named cell, 25 calls total covering rows 0–4.

The buffer is returned; the GPU upload happens in `Gfx::new` (lines 657–671).

**No mutable state outside the buffer.** Determinism is total — the same build
always produces byte-identical atlas bytes, which matters for visual regression
testing.

---

### Low-level primitives (lines 125–152)

Three module-internal helpers used by every `draw_*` function:

- **`put_pixel(buf, x, y, rgba)`** (line 125) — write 4 RGBA bytes at `(x, y)` in
  the atlas-wide pixel space. Silently bounds-checked: out-of-range coordinates
  no-op rather than panic, which lets `draw_*` functions write near cell edges
  without explicit clipping.
- **`fill_rect(buf, x, y, w, h, rgba)`** (line 136) — solid rectangle of `rgba`
  pixels via two nested loops over `put_pixel`. The workhorse primitive.
- **`fill_cell(buf, cell, rgba)`** (line 144) — fill an entire 32×32 cell with one
  color. Used by `generate_atlas` for `SOLID_WHITE`.
- **`cell_origin(cell) -> (u32, u32)`** (line 150) — the atlas-pixel-space
  top-left corner of a named cell. Every `draw_*` calls this first to convert its
  local cell coordinate to atlas coordinates.

Visibility is `pub(crate)` — atlas helpers are used by no other module today, but
the renderer team kept them crate-visible in case `gfx.rs` or `hud.rs` ever needs
to render directly into the atlas buffer at runtime.

---

### Palette (lines 154–168)

Ten compile-time RGBA constants transcribed from the analysis HTML's CSS tokens.
The doc-comment at lines 154–157 names the source:

| Constant       | Hex      | CSS token            | Use                                        |
|----------------|----------|----------------------|--------------------------------------------|
| `GOLD`         | #54CFC9  | `--gold` (teal)      | Bow chevron, reorient ring, shields badge. |
| `VERMILLION`   | #E07A3C  | `--vermillion`       | Fire telegraph, target-lock variants.      |
| `C_BEAM`       | #5AD1CB  | beam archetype       | Beam glyph.                                |
| `C_ORD`        | #E0A23C  | ordnance archetype   | Ordnance glyph, deploy telegraph.          |
| `C_BROAD`      | #E0664A  | broadside archetype  | Broadside glyph.                           |
| `C_DISP`       | #9B8CDB  | displacement         | Displacement glyph, push/pull telegraphs.  |
| `C_CTRL`       | #6FBF7A  | control archetype    | Control glyph.                             |
| `C_MOVE`       | #5A9FE0  | movement archetype   | Movement glyph.                            |
| `C_DEF`        | #8AA0B8  | defensive archetype  | Defensive glyph.                           |
| `PAPER_DIM`    | #93A6BD  | `--paper-dim`        | Systems-offline ring.                      |

These map 1:1 to the **same colors the design HTML uses** to color-code archetypes
on its weapon cards — so the renderer's queue-glyph row reads exactly like the
designer's archetype legend.

---

### The 25 `draw_*` functions (lines 170–671)

Grouped by row of the atlas. Each is a private helper writing a single
hand-tuned pixel-art glyph into one named cell. I won't walk every function
line-by-line — the math is small fixed-coordinate `put_pixel` / `fill_rect`
calls — but here's the per-group summary:

#### Projectiles (lines 170–244)

- **`draw_bow_chevron`** (line 175) — a right-pointing `>` chevron made of two
  diagonal pixel runs plus a tip highlight. Renderer rotates it around its center
  by the lane direction + bow-on/aft.
- **`draw_torpedo`** (line 199) — horizontal capsule body, nose taper, tapering
  tail flame. Points right in the unrotated cell.
- **`draw_missile`** (line 227) — smaller, faster-looking variant: 3-pixel-tall
  body, two-pixel flame, sharper nose. Same orientation convention as torpedo.

#### Action-queue glyphs (lines 246–373)

One per `WeaponArchetype`, each centered ~16×16 inside the cell:

- `draw_glyph_beam` — horizontal lightning bolt zig-zag (`C_BEAM`).
- `draw_glyph_ordnance` — small filled circle + two trail dots (`C_ORD`).
- `draw_glyph_broadside` — two opposing arrows in a vertical band (`C_BROAD`).
- `draw_glyph_displacement` — stacked `⇄` arrow pair (`C_DISP`).
- `draw_glyph_control` — cross-hairs + diagonals through center (`C_CTRL`).
- `draw_glyph_movement` — forward chevron (`C_MOVE`).
- `draw_glyph_defensive` — shield outline with pointed bottom (`C_DEF`).

Each picks its archetype's brand color from the palette block above. Stacked
above the player by `hud::compose_scene` to show the queue contents.

#### Telegraph icons (lines 375–494)

Six per-intent icons drawn over enemy ships to telegraph their next action:

- `draw_telegraph_fire` — explosive starburst (`VERMILLION`).
- `draw_telegraph_lock` — square reticle with corners only + center dot.
- `draw_telegraph_push` — right-pointing arrow with arrowhead.
- `draw_telegraph_pull` — left-pointing arrow with arrowhead.
- `draw_telegraph_reorient` — circular arrow (ring with gap + arrowhead).
- `draw_telegraph_deploy` — downward arrow + hazard square at the bottom.

#### Status badges (lines 496–561)

Small badges drawn next to a ship for active statuses:

- `draw_status_hull_breach` — teardrop flame with inner highlight.
- `draw_status_systems_offline` — power-off ring (broken ring + vertical line).
- `draw_status_target_lock` — slim cross-hairs + tinted dot.
- `draw_status_shields_up` — gold shield outline with pointed bottom.

#### Parallax layer art (lines 563–671)

Five depth layers tiled across the backdrop:

- **`draw_parallax_far_stars`** (line 567) — 12 hardcoded `(x, y)` star positions
  per cell. The first paragraph of renderer's hint suggested this used a "Wang-
  hash LCG" for placement — **that's not the case in the current code.** The
  starfield uses a fixed `[(3,5), (7,11), …]` coordinate array; the four-cycle
  bright/dim tint pattern from `i % 4 == 0` is the only "randomness" in play.
  Determinism is achieved by the hardcoded coordinates, not by a seeded RNG.
- **`draw_parallax_nebula`** (line 584) — two soft blobs (squared-distance
  threshold) in dusty purple + dusty blue tints. The first procedural-rather-
  than-fixed-pixel cell.
- **`draw_parallax_distant_planet`** (line 605) — a shaded sphere via
  squared-distance with light-from-upper-left shading. One cell = one whole
  planet on screen.
- **`draw_parallax_mid_stars`** (line 634) — 12 hardcoded positions, brighter
  than far stars, with `+`-shaped sparkle on every third star.
- **`draw_parallax_foreground_dust`** (line 658) — 4 hardcoded mote positions,
  each a 2×2 highlight + 1-pixel halo on each side. Bright, sparse, drifts near
  the camera.

---

### `fn filled_circle(buf, cx, cy, radius, rgba)` (line 676)

The one shared helper used by both `draw_glyph_ordnance` and
`draw_telegraph_fire`. Brute-force `(dx, dy)` scan within `radius², pixel if
`dx² + dy² ≤ radius²`. Bounds-checked; clips out-of-atlas coordinates silently.

---

### `#[cfg(test)] mod tests` (lines 692–818)

7 inline tests, all green:

```
cell_uvs_at_origin_is_unit_cell
cell_uvs_at_corner_is_inside_unit_square
generate_atlas_sized_correctly
solid_white_cell_is_white
every_cell_inside_atlas_bounds
named_cells_are_distinct
every_cell_has_some_content
```

The last three are the load-bearing **drift guards**:

- **`every_cell_inside_atlas_bounds`** (line 727) — iterates every named cell
  constant and asserts `col < CELLS_PER_ROW` and `row < CELLS_PER_ROW`. Catches
  off-by-one errors when adding new named cells.
- **`named_cells_are_distinct`** (line 748) — `O(n²)` pairwise check that no two
  named cells share a `(col, row)`. The "two glyphs accidentally point at the
  same slot" drift guard.
- **`every_cell_has_some_content`** (line 771) — runs the full `generate_atlas`,
  then for every named cell scans its 32×32 region looking for at least one
  pixel with alpha > 0. Catches the "forgot to wire a `draw_*` call in
  `generate_atlas`" failure mode.

These three tests together let a future maintainer add a new named cell + its
`draw_*` function with confidence — adding the cell to either the constants block
*or* the `generate_atlas` call list (but not both) trips one of the three.

---

### No Drift section

This module has no Drift section because **there is no TS analog**. The TS engine
is headless; sprite atlases are a Rust-port concern. Listed here as an explicit
"absent by design" callout so future readers don't think the section was
accidentally omitted.

The absence of drift is itself a small load-bearing fact: any contributor
extending this module is free to design ergonomically without checking against a
canonical TS reference. The only cross-module agreement to maintain is the cell
layout in [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md), which mirrors the constants
at the top of this file.

**Known stale source-side note:** the module rustdoc at `atlas.rs:9` references
`crate::perspective::ship_sprite`, which was deleted in the flat-scene refactor
(`1d4d540`, task #55). The dangling link doesn't affect behavior — ship hulls are
in fact drawn as procedural polygons by `hud.rs` per the post-refactor
architecture — but the rustdoc should eventually drop the
`crate::perspective::ship_sprite` reference. Renderer-owned cleanup; flagged here
so the doc and the source stay in sync. This walkthrough already paraphrases
around it.

No open architectural items.

---

## `src/hud.rs`

*Scene compositor. Turns a `Board` + `LaneGeometry` + `view_angle_rad` into a
back-to-front `Vec<DrawCommand>` that `gfx.rs::Gfx::render` consumes. Every
draw call the renderer makes originates here. This is the largest renderer-side
module (1455 lines) and the one that owns the most visual decisions: ship
silhouette morphing, the camera-revolves parallax, the LCG starfield painter,
the inline 5×7 bitmap font, and all five per-ship HUD overlays.*

**Mirrors:** No TS analog. The TS engine is headless; scene composition is a
Rust-port concern from day one. **No Drift section — absent by design.**
**Design anchor:** Tasks #29 (Slice D — compose scene in hud.rs in the
documented render order) + #45 (win/lose overlays) + #46 (animation tweens) +
#58 (single-silhouette + bow morph) + #59 (parallax planes respond to
view_angle) + #77–#78 (between-encounter screen + salvage HUD).
**Source commit:** stabilized through Phase 3 Slice E. 1455 lines, inline tests
near the bottom (1 explicit + the integration suites consume the public surface).
Reviewer audited.

### Module rustdoc (lines 1–25)

The 25-line `//!` block sets the layout philosophy and the **canonical render
order** every frame must follow. Read it before touching any `push_*` function.

**Layout:** flat side-view. A horizontal lane bisects the canvas at
`LaneGeometry::center_y`; the area above is the "sky" (back-wall parallax:
stars, nebula, distant planet); below is the "floor" (foreground dust). Ships
are asymmetric side-view silhouettes anchored on the lane line.

**Render order (back to front):**

1. Sky parallax (stars + nebula + planet, upper half).
2. Floor parallax (dust, lower half).
3. Lane stroke (the horizon line + per-cell ticks).
4. Range-band tick marks (relative to the player ship).
5. Hazards.
6. Ships (one silhouette per occupied cell).
7. Live ordnance.
8. Heat bars + shield pips (per ship).
9. Action queue glyphs (stacked above each ship).
10. Status badges.
11. End-state overlays (defeat / victory / between-encounter).

`gfx.rs::Gfx::render` does **not** depth-sort — the back-to-front list is
authoritative. Reordering anything in this list reorders what the player sees.

---

### Palette (lines 38–66)

15 RGBA-as-`[f32; 4]` constants, all derived from the analysis HTML's CSS
tokens scaled to `[0, 1]`. Notable groupings:

- **`PLAYER_HULL_*`** / **`ENEMY_HULL_*`** — fill + stroke pairs that make
  faction visually obvious without reading the queue.
- **`LANE_STROKE`** / **`LANE_TICK`** — the horizon line and the per-cell tick
  marks.
- **`BAND_*`** — the five range-band tint colors (point-blank vermillion →
  extreme purple), matching the same archetype-palette family as the design
  HTML's range-band ruler.
- **`HEAT_*`** — heat bar background, normal fill, and lockout-red.
- **`SHIELD_PIP_CHARGE`** — gold for active charges.
- **`DEFEAT_TINT`** / **`VICTORY_TINT`** — the end-state overlay tints.

The intent is for a screenshot of any frame to read color-correct against the
design document by inspection.

---

### Entry points: three compose_scene shims (lines 81–175)

Three public entry points form a chain. **The bin calls
`compose_scene_tweened` directly;** the other two exist for hud's own tests and
for callers that don't need a tween state. Renderer flagged the call hierarchy
explicitly:

```
compose_scene(board, lane, view_angle_rad)
   └─► compose_scene_with(board, lane, view_angle_rad, &EmptySpriteRegistry)
          └─► compose_scene_tweened(board, lane, view_angle_rad, sprites, &TweenState::default())
                  // ← this is the real implementation; the others forward
                  //   default values for one argument each.
```

#### `fn compose_scene(board, lane, view_angle_rad) -> Vec<DrawCommand>` (line 81)

The simplest entry point. No sprite registry, no tween state. Forwards to
`compose_scene_with` with `EmptySpriteRegistry`. Used by tests that don't load
art assets.

#### `fn compose_scene_with(board, lane, view_angle_rad, sprites) -> Vec<DrawCommand>` (line 90)

Adds sprite-registry awareness. If both `side` and `top` PNGs are registered
for a ship's class/stance, the ship draws as a `TexturedShipInstance` (the
side/top blend pipeline) instead of the procedural silhouette polygon. Forwards
to `compose_scene_tweened` with a default `TweenState`.

#### `struct TweenState` (line 109)

```rust
pub struct TweenState {
    pub visual_cells: std::collections::HashMap<String, f32>,
}
```

Per-ship visual cell-position overrides keyed by `Ship::id`. Each entry is a
*fractional* cell index used in place of the ship's logical `ship.cell`.

The doc comment on lines 105–108 explains the pattern: *"The bin captures
previous cell positions before each input mutation and lerps `prev -> current`
over ~200ms using ease-out, producing a `TweenState` per frame that smooths
out the per-input snap under Shogun-Showdown turn semantics."* This is what
makes movement feel animated under the otherwise-instant SS turn model.

`TweenState::cell_for(ship)` (line 118) returns the visual cell or falls back
to `ship.cell as f32` when the ship is absent from the map. Returned as `f32`
so callers can feed it straight into `fractional_cell_to_screen`.

#### `fn compose_scene_tweened(board, lane, view_angle_rad, sprites, tween) -> Vec<DrawCommand>` (line 131)

**The canonical entry point.** Everything else is a shim around this. Walks
the render-order list:

1. `push_parallax(out, lane, view_angle_rad)` — both planes.
2. `push_lane(out, lane)` — horizon line + ticks.
3. `push_range_band_ticks(out, board, lane)` — colored band marks above lane.
4. `push_hazards(out, board, lane)`.
5. For each ship on the board: `push_ship(out, ship, visual_cell, lane,
   view_angle_rad, sprites)` — uses tweened cell.
6. For each projectile: `push_projectile(out, proj, lane)`.
7. For each ship (second pass): `push_heat_bar`, `push_shield_pips`,
   `push_queue_glyphs`, `push_status_badges` — all using the **same**
   tweened cell so HUD overlays track the smoothed silhouette.
8. `push_view_angle_overlay(out, view_angle_rad)` — the angle-scrubber readout.
   Paints a single bar + 7 tick marks at the top-right of the canvas indicating
   the current `view_angle` position. Not deep-walked here; trivial geometry.

**End-state overlays are NOT pushed here.** The doc comment at lines 164–172
notes the explicit refactor reason: through #45 the module auto-pushed
`push_end_state_overlay(out, win_state(board))`, but Phase 3's
between-encounter screens introduced overlay states beyond what
`win_state(&Board)` can describe (e.g. "encounter complete, awaiting path
choice"). The bin now drives the overlay-vs-no-overlay decision and calls
`push_end_state_overlay` / `push_between_encounter_overlay` /
`push_run_defeated_overlay` directly on top of this list when needed.

---

### `fn ship_bbox(ship, view_angle_rad) -> (f32, f32)` (line 181)

**Intent:** On-screen silhouette bounding box for a ship at the current view
angle. Returns `(width, total_h)` so the five overlay helpers (heat bar,
shield pips, queue glyphs, status badges, plus chevron placement) position
consistently against the current silhouette regardless of stance or angle.

**The total-height formula** is the canonical camera-revolves math, identical
to what SPRITE_SPEC.md documents:

```
total_h = FRIGATE_DIMS.height × cos(view_angle) + depth_dim × sin(view_angle)
```

where `depth_dim` is the off-lane axis: `FRIGATE_DIMS.beam` for `BowOn`,
`FRIGATE_DIMS.length` for `Broadside`. At `view_angle = 0` the height term
dominates (camera at side: silhouette is `height` tall); at `view_angle = π/2`
the depth term dominates (camera overhead: silhouette is `depth_dim` tall).

Width is fixed (no horizontal foreshortening) — `length` for `BowOn`, `beam`
for `Broadside`.

This function is called by every per-ship overlay below; **drift here ripples
through every HUD element**. Tested at `tests/perspective.rs` via the
SPRITE_SPEC reference values.

---

### Parallax — `push_parallax` (line 225)

The single longest function in the file (~105 lines). Renders **two foreshortening
planes** anchored at the lane line, plus the camera-revolves model that makes
the background read as a revolving camera.

**The two planes** (lines 235–238):

```
back_wall_h = (canvas above lane) × cos(view_angle)        // collapses at π/2
floor_h     = (canvas below lane) × sin(view_angle)        // collapses at 0
```

At `view_angle = 0` the back wall fills the upper half and the floor collapses
to nothing — pure side view. At `view_angle = π/2` the wall collapses and the
floor fills — pure top-down. At intermediate angles both are visible,
foreshortened. The lane line itself never moves; it's the **horizon between
the two planes** at every angle.

**Back-wall content** (lines 241–298):

- **3 nebula patches** (lines 246–259) — `PARALLAX_NEBULA` atlas cell tiled at
  fixed widths across the upper third of the wall. The atlas tile has a baked
  vertical extent, so it doesn't compress with the wall — it just slides.
- **1 distant planet** (lines 262–268) — `PARALLAX_DISTANT_PLANET` atlas cell
  at upper-right, ~30% down from wall top. One cell = one whole planet on
  screen.
- **60 far-star sprites** (lines 271–280) — single-pixel SOLID_WHITE quads
  scattered across the sky band via the LCG (see below). Alpha varies per-star
  via `lcg_unit(...)` so they twinkle slightly without animation.
- **24 mid-star sprites** (lines 282–297) — same LCG approach, slightly bigger
  (1.0 × 1.0 px), brighter alpha, packed into the top half of the wall.

**Floor content** (lines 301–326):

- **~18 dust speckles** (line 305) — count scaled by `0.4 + 0.6 × sin(angle)`,
  so the floor "fills in" as the camera tilts down.
- **1 `PARALLAX_FOREGROUND_DUST` atlas-cell sample** (lines 319–326) — drawn
  only when `sin(angle) > 0.2`. Hidden at low angles where the floor is
  edge-on.

**Drift note: the two starfield atlas cells are vestigial.** Atlas cells
`PARALLAX_FAR_STARS` (atlas.rs:59) and `PARALLAX_MID_STARS` (atlas.rs:62)
exist but are **never referenced by `hud.rs`**. The actual on-screen
starfield is painted per-frame via the LCG into single-pixel SOLID_WHITE
quads. The atlas cells are leftover scaffold from before the LCG-driven
starfield landed. Renderer flagged this as a future cleanup; documented here
so the discrepancy doesn't surprise future readers.

---

### The LCG: deterministic pseudo-random for the starfield (lines 331–352)

Three private functions form the LCG-style hash that powers the live
starfield painter:

#### `fn lcg_canvas_pos(seed: u32, rect: [f32; 4]) -> (f32, f32)` (line 331)

Returns a deterministic two-axis position inside the rectangle `[x, y, w, h]`,
seeded by `seed`. Hashes `seed` for x and `seed + 0x9E3779B9` (the golden-ratio
constant) for y, so the two axes don't correlate.

#### `fn lcg_unit(seed: u32) -> f32` (line 340)

Returns a deterministic float in `[0, 1]` for the supplied seed. Used for
per-star alpha variation.

#### `fn wang_hash(mut x: u32) -> u32` (line 344)

The actual hash — a **Wang hash** (Thomas Wang's variant), not a classical
linear-congruential generator. Three multiply + XOR + shift rounds with
specific magic constants:

```rust
x = (x ^ 61).wrapping_mul(0x27D4_EB2D);
x ^= x >> 16;
x = x.wrapping_mul(0x85EB_CA6B);
x ^= x >> 13;
x = x.wrapping_mul(0xC2B2_AE35);
x ^= x >> 16;
```

**Why a Wang hash and not `rand`?** Two reasons, both load-bearing:

1. **Determinism.** The same seed always produces the same star positions.
   This means the starfield is byte-identical across runs, frames, machines.
   Visual-regression tests that compare rendered frames don't have to seed
   anything; the LCG is "the seed."
2. **Zero-dep.** No `rand` crate, no per-frame RNG state. The seed is the
   input parameter; the function is pure. Works under the default-features
   build with no extra dependencies pulled in.

**The "LCG" naming is slightly inaccurate** — this is a Wang hash, not a
linear-congruential generator. The Rust functions are named `lcg_canvas_pos` /
`lcg_unit` for brevity; the underlying primitive is `wang_hash`. Documentation
preserves the source-side naming for greppability but flags the technical
distinction here so future readers know what to look up.

**The two starfields:** atlas.rs's `PARALLAX_FAR_STARS` / `PARALLAX_MID_STARS`
cells have hardcoded `(x, y)` arrays for 12 stars each (intended as 32×32
texture tiles). The live HUD starfield in `hud.rs` uses the Wang-hash LCG to
paint ~60 far + ~24 mid stars per frame as individual SOLID_WHITE pixels
scattered across the sky band. The atlas cells are vestigial; the LCG is
canonical.

---

### `fn push_lane` (line 358), `fn push_range_band_ticks` (line 385), `fn push_hazards` (line 418)

Three small back-to-front layers, all rendering via SOLID_WHITE quads tinted
by per-instance color:

- **`push_lane`** — one thin horizontal stroke at `lane.center_y` spanning the
  full canvas width, plus one short vertical tick per cell just below the
  lane line.
- **`push_range_band_ticks`** — for each cell within ±7 of the player, a short
  vertical mark above the lane line, colored by the range band that cell sits
  in (`BAND_POINT_BLANK` through `BAND_EXTREME`). Skip if no player on the
  board.
- **`push_hazards`** — small tinted squares above the lane at each hazard's
  cell. `Mine` → red, `Drone` → green, `Debris` → grey.

All three are short, mechanical, and well-commented in the source. Their order
in the render list (3, 4, 5) places them above parallax but below ships, so
ships occlude lane ticks where they overlap.

---

### `fn push_ship` (line 477) and the silhouette stack

The single most-read function in the file. ~125 lines covering: stance
inference, view-angle silhouette dimensioning, PNG-vs-procedural dispatch, and
optional chevron overlay.

**Inputs:** `ship`, `visual_cell` (the tweened fractional cell), `lane`,
`view_angle_rad`, `&dyn SpriteRegistry`.

**Body walkthrough:**

1. **Locate the ship on screen** (line 485): `fractional_cell_to_screen` maps
   the tweened cell to a `Point2`.
2. **Pick faction colors** (lines 486–490): player vs enemy.
3. **Infer Stance from Orientation** (lines 492–495): `BowOn → Stance::BowOn`,
   `Broadside → Stance::Broadside`. The renderer's `Stance` carries no bow
   direction; `bow_fore` is captured separately at line 496.
4. **Compute the silhouette bbox** (lines 498–514) using the camera-revolves
   formula from `ship_bbox` (inlined here for performance). Width is along-lane
   (`length` for BowOn, `beam` for Broadside); `total_h` uses the
   `cos(θ) + sin(θ)` blend.
5. **Center the silhouette vertically on the lane line.** Per the doc comment
   at lines 510–514: *"Silhouette is CENTERED on the lane line: half above,
   half below. The lane bisects the ship vertically at every angle."* `top_y =
   p.y - half_h`, `base_y = p.y + half_h`.
6. **PNG dispatch** (lines 516–544): if `sprites.has_pair(class, stance)`
   returns true, emit a `TexturedShipInstance` with the bbox corners + slug
   pair + `blend_t = sin(view_angle)`. Return early — skip the procedural
   silhouette and the chevron (the painted PNGs own bow direction). Heat
   bars / shield pips / queue glyphs / status badges still draw on top
   regardless.
7. **Procedural silhouette dispatch** (lines 546–553): if no PNG pair is
   loaded, call `push_bow_on_silhouette` or `push_broadside_silhouette` to
   emit the polygon set.
8. **Bow chevron overlay** (lines 555–580): when `sin(angle) > 0.05` and the
   silhouette is tall enough (`total_h > 6`), overlay a chevron sprite with
   alpha = `sin(angle)`. The chevron position differs by stance — for BowOn
   it's offset toward the bow end; for Broadside it's centered at the top,
   pointing off-lane.

**Why a chevron at all?** From the renderer team: the bow chevron is the
visual cue for "which way is the bow." At pure side view (`angle = 0`) the
silhouette's pointy bow taper carries that information; at pure top-down
(`angle = π/2`) the taper collapses and the chevron has to provide it
instead. The alpha = `sin(angle)` blend makes the chevron invisible at side
view (where it would clutter the silhouette) and fully opaque at top-down
(where it's the only readable bow indicator).

---

### `fn push_bow_on_silhouette` (line 601), `fn push_broadside_silhouette` (line 668)

Two procedural-silhouette polygon emitters. Each emits one filled polygon +
4 edge sprites to trace an outline. The polygons use the SOLID_WHITE atlas
cell and a per-instance color tint.

**`push_bow_on_silhouette`** emits a **5-vertex polygon** with a triangular
bow taper:

```
  stern-top ----------- bow-top
  |                            \
  |                             bow-tip
  |                            /
  stern-bot ----------- bow-bot
```

The bow taper width is `(length × 0.25) × cos(view_angle)` — at side view
the bow has its full pointy taper; at top-down it collapses to a rectangle.
The mirror-for-aft case is handled by negating the bow-end offset.

**`push_broadside_silhouette`** emits a stubbier rectangle (the broadside
hull has no preferred bow direction at any angle; both ends face off-lane).
No bow taper, no horizontal mirror.

**Important asymmetry** flagged in the source at lines 454–457: the procedural
broadside silhouette uses `length = beam, height = length / 3` rather than
the more obvious `length / beam` swap. Per the doc comment: *"For the flat
side-view model we don't have a great way to show broadside, so we use a
stubbier polygon (length = beam, height = length / 3) without the bow taper."*
A future content pass with custom broadside art would render a more
distinctive silhouette via the textured-ship path.

---

### `fn push_projectile` (line 797)

Single-line summary: emit a small tinted sprite at the projectile's
fractional cell position via `fractional_cell_to_screen`. Color is
ord-archetype gold for friendly, vermillion for hostile. Size is fixed.

Projectiles do not currently morph with view angle — they're flat sprites at
all angles. A future pass could tilt projectile orientation toward its
heading.

---

### Per-ship HUD overlay helpers (lines 821–1005)

Four functions that draw on top of every ship. All four take `visual_cell:
f32` so they ride along with the tween-smoothed position, and all four call
`ship_bbox` internally to size against the current silhouette regardless of
stance / angle.

#### `fn push_heat_bar(ship, visual_cell, lane, view_angle_rad)` (line 821)

A horizontal bar above each ship's silhouette. Background tint is
`HEAT_BG`; fill is `HEAT_FILL` (orange) at normal heat, `HEAT_LOCKOUT` (red)
when `ship.locked_out`. Bar width is fixed; fill ratio is
`ship.heat / ship.heat_max`.

#### `fn push_shield_pips(ship, visual_cell, lane, view_angle_rad)` (line 861)

One small pip per active shield charge, placed below the heat bar. Pip color
is `SHIELD_PIP_CHARGE` (gold). Scans `ship.shield_profile` for each zone's
`charge` count and draws one pip per held charge.

#### `fn push_queue_glyphs(ship, visual_cell, lane, view_angle_rad)` (line 911)

For each action in `ship.queue`, look up the action's archetype and draw the
corresponding atlas glyph (`GLYPH_BEAM`, `GLYPH_ORDNANCE`, etc.) stacked
above the ship. The bottom glyph is the next-to-fire action; subsequent
glyphs stack upward. This is the "queue contents over each ship" feature
the design HTML calls for.

#### `fn push_status_badges(ship, visual_cell, lane, view_angle_rad)` (line 959)

For each active status on a ship, draw the corresponding atlas badge
(`STATUS_HULL_BREACH`, `STATUS_SYSTEMS_OFFLINE`, etc.) at a fixed offset.
Multiple active statuses stack horizontally.

---

### `fn win_state(board) -> WinState` (line 1011)

A small `pub fn` that derives the end-state from the board. Three variants:

- `Victory` — no Enemy ships remain (and at least one Player ship survives).
- `Defeat` — no Player ship remains (irrespective of enemy count).
- `Playing` — otherwise.

The doc-comment notes the both-empty case: *"If both factions are empty
(shouldn't happen in normal play) Defeat wins — there's nobody to be
victorious."*

Tested at `win_state_classifies_factions_correctly` (line 1393).

---

### End-state overlay surface (lines 1025–1166)

Three overlay-pushing functions that the bin calls on top of
`compose_scene_tweened`'s output when appropriate state is active:

#### `fn push_end_state_overlay(out, state)` (line 1025)

Phase 1's win/lose overlay. For `Defeat` / `Victory`, pushes a full-canvas
tinted quad + a centered banner via `push_centered_banner`. `Playing` is a
no-op. Banner text: `"DEFEATED - PRESS ENTER TO RESTART"` or `"VICTORY - PRESS
ENTER TO RESTART"`.

#### `fn push_run_defeated_overlay(out, salvage)` (line 1046)

Phase 3 evolution. Like `push_end_state_overlay`'s `Defeat` variant but also
surfaces the run's earned salvage total: `"DEFEATED"` + `"TOTAL SALVAGE: {n}"`
+ `"PRESS ENTER TO RESTART"` as a three-banner stack. **The bin calls this
function, not the older `push_end_state_overlay(WinState::Defeat)`**, for the
`DemoState::RunDefeated` path — since salvage surfacing landed (#88/#89), the
older overlay is no longer touched by the run-defeat flow. It remains public
surface for callers that don't carry salvage state, but the canonical Phase-3
run-defeat overlay is this function.

#### `fn push_salvage_hud(out, salvage)` (line 1065)

The in-game top-right counter. A single row of inline 5×7 glyphs showing
`"SALVAGE: {n}"` ~16px from the top-right edge. Always visible during
`Playing` state so the player sees the counter tick up on encounter wins.

#### `enum BetweenEncounterChoice` (line 1103) + `fn push_between_encounter_overlay` (line 1129)

Phase 3 between-encounter and run-complete overlays. Two variants:

- `EncounterComplete { sector_idx, salvage }` — *"ENCOUNTER COMPLETE - SECTOR
  N"* + *"SALVAGE: N"* + *"1 REPAIR  2 UPGRADE  3 CONTINUE"* (three-banner
  stack with the choice row at the bottom).
- `RunComplete { salvage }` — *"RUN COMPLETE"* + *"TOTAL SALVAGE: N"* +
  *"PRESS ENTER TO RESTART"*.

The doc-comment at lines 1110–1115 notes `RunComplete` is **distinct from
`WinState::Victory`** — Victory fires on any single encounter win; `RunComplete`
is the campaign-end overlay only.

---

### The inline 5×7 bitmap font (lines 1168–end of file)

**Renderer:** *"Where does the renderer get text?"* — Answer: from this file,
in the `push_glyph_5x7` function at line 1175. **Not the atlas, not a font
crate.** A hand-rolled match arm with 7 rows of 5 bits per glyph.

#### `fn push_glyph_5x7(out, ch, x, y, pixel, color)` (line 1175)

Renders one character at `(x, y)` in virtual-pixel space, scaled by `pixel`
(typically 4 for title-style banners, 2 for body text, 2.5 for sub-banners).
The function does a giant `match` over the character literal returning a
`[u8; 7]` of 5-bit rows; iterates the 7×5 grid and emits one SOLID_WHITE
quad per lit bit.

**Character coverage** (line 1183–end): A, C, D, E, F, G, I, L, M, N, O, P,
R, S, T, U, V, Y, 0-9, `-`, `:`, space. **Sparse — only the characters that
appear in the overlay banner strings are defined.** Unknown characters
render as blank glyphs (5×7 of zeros) without erroring.

If a future overlay needs additional characters, add the match arm here. The
test suite doesn't pin font coverage; the symptom of a missing character is
"banner has gaps" rather than a panic.

**Why inline instead of atlas-packed?** Three reasons that came up during
Phase 1's win/lose work (#45):

1. **Variable character sizes.** A title-style banner wants `pixel = 4`
   (20×28 effective glyph); the salvage counter wants `pixel = 2` (10×14).
   An atlas-packed font would need to be packed at the largest size or
   render with bilinear filtering — neither matches the crisp pixel-art
   look.
2. **Sparse character set.** The full overlay vocabulary is ~30 characters.
   Packing them into atlas cells would consume 30 slots out of 64; leaving
   the inline `match` keeps the atlas free for game art.
3. **Build-time changes.** Adjusting a glyph is a one-line code change. With
   atlas-packed text, the atlas regeneration cycle adds friction for a
   feature that already has a clean inline solution.

#### `fn push_centered_banner(out, banner, y_center, pixel)` (line 1086)

Wrapper that centers a string at `y_center`. Computes total width, derives
`start_x`, walks the character iterator calling `push_glyph_5x7` for each
with the advance distance baked in. Space between glyphs is `pixel` (one
font-pixel wide).

---

### `#[cfg(test)] mod tests` (line ~1380 onward)

The module's inline tests are minimal — most coverage lives in integration
tests (`tests/hud_layout.rs`, `tests/render_example.rs`, the bin's
event-loop tests). One inline test pinned in this file:

- **`win_state_classifies_factions_correctly`** (line 1393) — pins the
  three-variant `WinState` semantics including the both-empty edge case.

---

### Drift note: vestigial atlas cells

Two atlas cells — `PARALLAX_FAR_STARS` (atlas.rs:59) and `PARALLAX_MID_STARS`
(atlas.rs:62) — are defined in `atlas.rs` and drawn by `atlas::generate_atlas`,
but **never referenced by `hud.rs`**. The actual on-screen starfield is
painted per-frame via the Wang-hash LCG into single-pixel SOLID_WHITE quads
(see the parallax section above). The atlas cells are scaffold from before
the LCG-driven approach landed.

Possible cleanup options for a future pass:

1. **Remove the cells** from atlas.rs (frees rows-4 slots 0 and 3) and update
   SPRITE_SPEC.md.
2. **Keep the cells, document the divergence** — which is what the docs do
   now.
3. **Switch the starfield to atlas-cell-tiled** (reverting the LCG approach)
   — would lose the per-frame deterministic-randomness property and would
   need a different solution to the "60 stars without 60 fixed positions"
   problem.

The current state (option 2) keeps documentation honest without forcing a
cleanup that nobody has prioritised. Renderer-owned decision.

---

No open architectural items.

---

## `src/resolve.rs`

*The combat resolver. One execution path serves player, enemy, and ordnance. The
four-phase round, the arc/heat/cooldown gate in queue execution, the full damage
pipeline, ordnance advancement, and end-of-turn ticking all live here. Per the
analysis HTML this is the engine's most load-bearing file — the file every other
module's documentation eventually cross-references.*

**Mirrors:** `_drive_pull/broadside-engine/engine/resolve.ts` (the entire file, plus
new bodies for the TS-stubbed helpers).
**Design anchor:** HTML Part I (Core Loop), Part XIII (Engine Integration & Schema).
**Source commits:** `c5855ce` (initial port) + `da243be` (content TODO closures,
`apply_modifiers` wired through Content, signature ripple to add `&dyn Content` across
the cascade) + `6575472` (EventBus γ-invariant docstrings). All implemented.

### Module rustdoc (lines 1–32)

A 32-line `//!` block split into "what is implemented" (lines 6–16) and "what is
stubbed" (lines 18–32). Read this section first when you're modifying any function
below; it is the canonical list of what the file owns vs. what the content slice
provides.

**Implemented:** the four-phase round, the arc + heat + cooldown gate, all eight
targeting patterns, the full damage pipeline (band falloff → modifiers → target-lock
×2 → directional shield → hull), effect dispatch for DAMAGE / APPLY_STATUS /
VENT_HEAT / REORIENT / SPAWN_ORDNANCE / DEPLOY, ordnance advance, end-of-turn.

**Stubbed but callable** (TS-body verbatim, each marked
`// TODO(broadside-content):` for the next teammate): `apply_modifiers` (default-impl
returns 0 — subsystem bonuses), `resolve_self_move` (the per-mode movement
implementation — *actually filled in* in `da243be`, no longer a stub), `resolve_target_move`
(push/pull/swap — also filled in `da243be`), `decide_enemy_action` (the AI decision
layer — *also filled in* per #6), and the `BOARD` effect arm (see Drift below).

Lines 33–38: imports — `geometry::{absorb_shield, bears, direction_to, facing_zone,
opposite, range_band}` and the full type vocabulary from `types::*`. Everything else
the resolver needs is local helpers.

---

### The `Content` trait (lines 47–82)

**Mirrors:** TS `interface Content { actions, spawnProjectile }`. The Rust port is a
trait, not a struct.

**Intent:** The resolver's view of the content/catalog layer. The resolver knows
nothing about *where* actions live (in a HashMap, in a Vec, in a JSON file); it only
asks `content.action(id)` and gets back an `Option<&Action>`.

Three required + default methods:

- **`fn action(&self, id: &str) -> Option<&Action>`** (line 50) — lookup by id. `None`
  is silently skipped in `execute_queue` (matches the TS `if (!a) continue`); never
  panic on a missing id.
- **`fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile`** (line 58) —
  build a projectile of `kind` owned by `owner`. The TS signature is
  `(kind, owner, board) => Projectile`; Rust drops the `&Board` parameter because the
  resolver's call site (`SPAWN_ORDNANCE`) already holds `&Board` separately. Trait
  implementations close over whatever board state they need via closure capture.
- **`fn damage_modifier(&self, target: &Ship, band: RangeBand, board: &Board) -> i32`**
  (line 79) — additive subsystem bonus for the canonical pipeline step 2. **Default
  impl returns 0** (line 80), so existing test/demo `Content` impls don't need to
  change. The runtime subsystem registry lives on the concrete `Content` type, not
  on `Board`, for two reasons documented inline (lines 68–77):
  1. Architect deliberately kept `Board` free of content-shaped fields; `SubsystemDef`
     is catalog-only.
  2. Subscribing to `OnDamageDealt` doesn't work — that hook fires at the *end* of
     `execute_queue`, after `apply_damage` already ran. Too late to influence step 2.

**Drift note: `damage_modifier` trait extension (commit `da243be`).** TS doesn't have
this method — the TS resolver leaves `applyModifiers` as a stub returning `dmg`
unchanged. The Rust port routes the subsystem-bonus computation through Content. This
also ripples a `&dyn Content` parameter into every call chain that may reach
`apply_damage`: `destroy`, `tick_statuses`, `end_of_turn`, `advance_projectile`,
`resolve_self_move`, `resolve_target_move`. The signature change is broad but
mechanical; the canonical pipeline ordering is preserved.

---

### `fn emit(board, hook, build)` — the temporary-detach helper (line 97)

**Intent:** `Board` owns its `bus`; emitting a hook needs `&mut bus` AND `&mut Board`
(because `HookContext` carries the board). The borrow conflict is resolved by
`mem::take`ing the bus, emitting, then putting it back. Closures registered by
subsystems can reach into the board through `ctx.board` without tripping Rust's
aliasing rules.

Three lines (98–102):

1. `let mut bus = std::mem::take(&mut board.bus);` — lift the bus off the board,
   leaving a default-constructed empty bus behind. The `Default` impl on `EventBus`
   (`types.rs:651`) is what makes this legal.
2. Build the `HookContext` with the freshly-borrowed `&mut Board`, let the caller
   populate `source_cell` / `target_cell` / `amount` via the `build` closure.
3. `bus.emit(hook, &mut ctx)` invokes every subscriber. Then `board.bus = bus` puts
   the bus back.

This composes with `EventBus::emit`'s own take/replace dance over subscriber slots
(see [`types.rs` § EventBus](#srctypesrs)). The two swaps don't conflict — emit
operates on slot indices within the temporarily-detached bus.

**Drift note: γ-invariant (no chained emit).** This pattern is what enforces "callbacks
cannot re-emit on the same bus" — the bus is detached from the board for the duration
of the call, so a callback that tries `ctx.board.bus.emit(...)` finds an empty bus and
silently no-ops. Documented in detail in the `EventBus` walkthrough at
[`types.rs:594`](#srctypesrs) and on the `HookContext` rustdoc; canonical statement
landed in commit `6575472`. Subsystem authors needing chained behaviour must call
resolver functions directly (e.g. `apply_damage`, `destroy`) — those *will* recurse
correctly because they don't go through the bus.

---

### Phase 0 — `fn resolve_round(board, content)` (line 110)

**Mirrors:** `resolve.ts:31`.
**Design anchor:** HTML Part I — the four-phase round is the engine's heartbeat.
**Intent:** One full round. The four phases in order:

1. **Player phase** (lines 111–114). Find the player by faction scan, then
   `execute_queue` on the player's cell. `find_map` + `then_some` returns the cell as
   `Option<usize>`; absent player means skip the phase (matches TS).
2. **Ordnance phase** (lines 126–130). Snapshot projectile ids first, then advance
   each by id-lookup. The snapshot is needed because `advance_projectile` removes its
   projectile from `board.ordnance` on impact. The TS iterates a `[...board.ordnance]`
   shallow copy for the same reason. Critically, **`board.destroys_this_window = 0`
   on line 126** opens a new chain-kill window for the ordnance phase: torpedo
   impacts that kill multiple enemies count as a chain within the phase, separately
   from the player queue. The TS does *not* emit `onChainKill` from the ordnance
   phase itself — only `executeQueue` does — and this port matches that.
3. **Enemy phase** (lines 133–139). Iterate enemy cells in initiative order
   (currently lane order; explicit initiative TBD). For each: check `skips_turn`,
   call `decide_enemy_action` to fill the queue, then `execute_queue`. The
   AI-fills-then-resolver-runs pattern is the design principle behind "the AI never
   bypasses the pipeline" (the resolver runs the same code path for enemy and
   player).
4. **End of turn** (line 142). `end_of_turn(board, content)` ticks cooldowns/heat/
   statuses and emits `OnTurnEnd`.

**Drift note: snapshot iteration over ordnance.** The TS uses `[...board.ordnance]`
to clone the array; Rust collects ids into a `Vec<String>` and re-looks-up by id each
iteration. Same semantic — iteration is stable across mid-iteration removals.

---

### Phase 1 / 3 — `fn execute_queue(ship_cell, board, content)` (line 153)

**Mirrors:** `resolve.ts:53`.
**Intent:** Execute one ship's queued actions in order. The single most-read function
in the engine. Same code path for the player (phase 1) and each enemy (phase 3).

The ship is identified by lane `cell`, **not** by `&Ship` — because applying an effect
can mutate the cells vector underneath us (movement, destroys), and a stable
borrow would not survive. The function looks up the ship by cell each time it
needs to read or mutate it; if a prior effect destroyed the ship mid-queue, the loop
returns early at line 178.

Line 158: `board.destroys_this_window = 0` — opens this ship's chain-kill window.
`destroy()` (line 819) increments this counter; `detect_chain` (line 1499) reads it
after the queue runs. Each `execute_queue` call is one window. The ordnance phase
above also opens its own window — same counter, different reset points.

Lines 164–167: clone the queue out up front. Iteration is stable across mid-iteration
mutations to the ship's record. Matches the TS `for (const actionId of ship.queue)`
which is also stable.

Lines 169–220: the per-action loop. For each `action_id`:

- **Line 172**: `content.action(action_id)`. Returns `Option<&Action>`; missing →
  continue silently. We clone the Action (line 173) so we don't hold a borrow on
  `content` while mutating the board.
- **Line 178**: re-check the ship still exists. A prior effect in *this same queue*
  may have destroyed it; if so, abort the queue entirely.
- **Lines 185–187**: lockout gate. Overheated ship can only fire free / zero-heat
  actions.
- **Lines 189–191**: cooldown gate. Action not yet charged.
- **Line 194**: `resolve_targeting(&action, board, ship_cell)` — returns the cells
  this action will resolve against.
- **Lines 197–199**: the "nothing bore" gate. If the action requires an arc and
  targeting returned an empty cell list, **the action is skipped with no heat cost
  and no cooldown reset**. This is the critical contract that lets a player queue
  optimistic actions; if their forward gun has no target, they don't lose the turn's
  heat budget.

> **Gotcha — the queue path does NOT gate on the ship owning a matching Mount.**
> `execute_queue` fires whatever `action_id`s sit in `ship.queue`, looked up purely by
> `content.action(action_id)`. There is no check that the ship has a `Mount` whose
> `weapon` is that action. An **unarmed ship (zero mounts) will still fire a queued
> weapon** — the only gates are lockout, cooldown, and the arc/target "nothing bore"
> check above (and the arc check only bites when the action has `requires_arc`; a
> `requires_arc: None` action with no mount fires unconditionally). This is **not a
> bug**: in real play the player's queue is built only from actions they have mounts
> for, and the **AI never hits this edge** because `decide_enemy_action` enumerates
> *from the enemy's mounts* and gates on arc/band/range via `resolve_targeting` before
> queuing anything. The sharp edge is exclusively the **direct queue-injection path**
> (player input, or a test/fixture that pushes an id onto `ship.queue` directly).
> Tester surfaced this while writing `tests/run_loop.rs`. If a future change wants
> mounts to be a hard prerequisite, the gate belongs right here at the top of the
> per-action loop.
- **Lines 202–204**: apply each effect via `apply_effect`. Effects may mutate cells,
  ordnance, statuses.
- **Lines 209–215**: heat + cooldown bookkeeping. Heat *always* increments by
  `action.cost.heat`; lockout fires when heat ≥ heat_max; cooldown resets
  unconditionally to `cost.cooldown_max` (hit or miss). Matches TS exactly.
- **Lines 217–219**: emit `OnDamageDealt` per action. Subsystem authors hooking this
  fire on every queued action, not just successful damage.

Lines 222–226: after the queue runs, `detect_chain(board)` reads `destroys_this_window`.
If ≥ 2, emit `OnChainKill`. Lines 230–232 clear the queue (if the ship survived).

**Worked example (`execute_queue_overheats_and_records_cooldown`, line 1656):**
Attacker starts heat=5, heat_max=6, queue=[pulse_laser]. After execute_queue: heat=6
(crossed threshold), `locked_out=true`, cooldowns["pulse_laser"]=0 (reset), queue
cleared. The lockout means the *next* round's pulse_laser is gated by the lockout
check (line 185) until vented.

**Worked example (`execute_queue_no_target_no_cost`, line 1684):** Attacker queues
pulse_laser into an empty lane (forward arc, no target). `resolve_targeting` returns
`[]`, the arc gate skips the action, heat stays 0, cooldown stays absent (or
unchanged). The contract is "no bore, no cost."

---

### Phase 2 — `fn advance_projectile(projectile_id, board, content)` (line 242)

**Mirrors:** `resolve.ts:233`.
**Intent:** Step a single projectile by its speed, resolving impacts. Identified by id
rather than borrow because the projectile may remove itself from `board.ordnance` on
impact.

Outer loop (line 247): repeat `speed` times — projectiles with `speed > 1` cover
multiple cells per turn. Each iteration:

- Re-find the projectile by id (line 251). The position is re-read because earlier
  steps may have changed cell.
- Compute the new cell via `checked_add`/`checked_sub` (lines 255–258) — `usize`
  arithmetic with explicit overflow check.
- If off-lane in either direction (lines 259, 264), remove the projectile via
  `retain` and return.
- Otherwise, update `cell` (line 269) and check for an occupant whose faction differs
  from the projectile's owner (lines 272–275).
- On impact (lines 274–293): clone the payload, then dispatch through the **regular
  damage pipeline** via `apply_damage` (with `dummy_weapon()` so falloff is skipped)
  or `add_status`. **Only `DAMAGE` and `APPLY_STATUS` effects are honoured on
  impact** (line 289 ignores everything else); the TS does the same.
- Remove the projectile and return.

**Drift note: dummy_weapon() for projectile impacts.** Projectile payloads are not
fired by any catalog action — they need an `Action` to thread through `apply_damage`
because the pipeline's falloff and modifier steps expect one. `dummy_weapon()` at
line 850 supplies a synthetic Action with `band_falloff: Some(false)` so the
payload's `amount` lands raw, no scaling.

---

### Phase 4 — `fn end_of_turn(board, content)` (line 305)

**Mirrors:** `resolve.ts:254`.
**Intent:** End-of-turn bookkeeping. Four things happen per ship:

1. Every positive cooldown decrements by 1 (lines 313–316).
2. Heat dissipates by 1, floored at 0 (line 319).
3. If heat dropped below heat_max, clear lockout (line 321).
4. Tick all statuses via `tick_statuses` (line 324). HullBreach deals 1 damage per
   active instance and may destroy the ship.

Final line 326: emit `OnTurnEnd`. Subsystems hooking this run *after* all per-ship
bookkeeping completes, so a turn-end subsystem sees the post-tick state.

**Drift note: status tick happens per-ship inside the loop.** TS does the same. The
hull-breach damage routes through `destroy` (and therefore the bus → `OnLethal`) if
the ship dies, so subscribers fire mid-loop, not at the end. Order is lane-order.

---

### Targeting — the eight-pattern dispatch (line 337)

**Mirrors:** `resolve.ts:81`.
**Intent:** `resolve_targeting(a, board, ship_cell)` returns the cells `a` resolves
on, honouring arc + band. The dispatch over eight branches. Patterns that don't pick
board cells (SELF / DEPLOYED_CELL / ORDNANCE) return the acting ship's own cell or
the spawn cell.

Each arm:

- **`SELF`** (line 343): `vec![ship_cell]`. The acting ship's own cell. Used by Vent,
  Brace, maneuvers, reorient.
- **`BROADSIDE`** (lines 345–362): both lane directions if the broadside arc bears.
  For each end (fore, aft): check arc bearing (probing the *far edge* of the lane in
  that direction), then find the first target in that direction at allowed band.
  Returns 0, 1, or 2 cells.
- **`BEAM` and `POINT_BLANK`** (lines 364–376): identical implementation. Find the
  bearing direction, first target, check band. Returns 0 or 1 cell.
- **`SPINAL_LINE`** (lines 378–391): line of occupied cells in the bearing direction
  filtered by band. If `hits_all` (the pierce flag), return all; else first only.
- **`BLAST`** (lines 393–410): first target, then expand to `[c-1, c, c+1]` clamped
  to the board. Signed-int math to avoid `usize` underflow at the fore edge.
- **`DEPLOYED_CELL` and `ORDNANCE`** (lines 412–424): the cell adjacent to the ship
  in the bearing direction. Returns 0 or 1 cell.

The implementation calls into private helpers (`bearing_direction`,
`first_target_toward`, `cells_toward`, `in_allowed_band`) all documented further below.

---

### The damage pipeline — `fn apply_damage(target_cell, raw, atk_cell, weapon, board, content)` (line 447)

**Mirrors:** `resolve.ts:139`.
**Design anchor:** HTML Part XIII implementation order #3 — the canonical damage
sequence.
**Intent:** Apply `raw` damage from cell `atk_cell` to the ship at cell `target_cell`
through the **load-bearing pipeline order:**

```
1. band falloff (unless ANY DAMAGE effect on the weapon disables it)
2. subsystem modifiers
3. target-lock 2x (consumes the status)
4. directional shield (charge -> armour)
5. hull subtraction + emit + destroy check
```

The doc comment at line 428 has this in bold caps with a "Do not re-order" rider. The
TS shape of this function is the canonical reference.

Step-by-step:

- **Step 1 (lines 461–473): band falloff.** First read the target's cell value (line
  461) — needed for both range computation and the post-mutation shield lookup. Then
  compute the band via `range_band(atk_cell, target.cell)`. The falloff-disabled
  predicate (lines 466–468) is **action-level, not effect-level**: a single DAMAGE
  effect on the weapon with `band_falloff: Some(false)` disables falloff for the
  *whole* `apply_damage` call. `None` and `Some(true)` both keep falloff on. This
  matches the TS predicate `effects.some(...)` and is pinned by tests
  `apply_damage_action_level_band_falloff_*` (multiple).
- **Step 2 (line 478): subsystem modifiers.** Delegates to `apply_modifiers` (line
  910) which routes through `content.damage_modifier`. Default impl returns 0.
- **Step 3 (lines 481–486): target-lock doubling.** If the target has a `TargetLock`
  status, double the damage and remove the status via `swap_remove`. The lock is
  consumed exactly once per hit; multiple locks on the same ship would each fire on
  successive hits, but the catalog doesn't generate that today.
- **Step 4 (lines 491–498): directional shield.** Compute the incoming direction via
  `direction_to(target_cell, atk_cell)` — from the target's frame, the lane end
  pointing back at the gun. Then `facing_zone` picks the hull zone, and
  `absorb_shield(face_mut, dmg)` consumes a charge or subtracts armour. Returns the
  damage that survives.
- **Step 5 (lines 502–516): hull + emit + destroy.** Subtract from hull. If
  `final_dmg > 0`, emit `OnDamageTaken` with target_cell and the *post-shield* amount
  (so subscribers see what actually landed). If hull ≤ 0, call `destroy(target_cell,
  board, content)`.

**Worked example (`apply_damage_weak_stern_takes_post_falloff_hit`, line 1582):** The
canonical demo Scenario A. Player at cell 0, scout at cell 1 with `bow: Fore` so the
*stern* faces the player. Distance 1 = `PointBlank`; weapon optimal=`Close` → falloff
delta 1 → factor 0.66 → `floor(4 × 0.66) = 2`. Stern armour 0 → 2 lands. Scout
hull 5 → 3. This is the exact math demo.ts exercises; the contrast against Scenario
B is the point, not the absolute number.

**Worked example (`apply_damage_strong_bow_soaks_to_zero`, line 1598):** Demo
Scenario B. Scout at `bow: Aft` so the *bow* faces the player. Same falloff: 2
damage. Bow armour 2 → `max(0, 2 - 2) = 0` lands. Hull stays 5. Same weapon, same
range, opposite orientation: zero damage.

**Worked example (`apply_damage_target_lock_doubles_and_consumes`, line 1613):** Scout
carries a `TargetLock` status. Same weapon, same range. Step 1: 2. Step 3: 2 × 2 = 4
(lock consumed). Step 4: stern armour 0 → 4 lands. Hull 20 → 16. Test also asserts
no `TargetLock` remains.

**Cross-references:**
- `apply_modifiers` (line 910) — step 2 implementation.
- `facing_zone`, `absorb_shield`, `direction_to` — from `geometry.rs`.
- `destroy` (line 811) — step 5's death path.
- `OnDamageTaken` and `OnLethal` hooks — emitted here and in `destroy`.

---

### `fn apply_effect(fx, a, source_cell, cells, board, content)` (line 526)

**Mirrors:** `resolve.ts:167`.
**Intent:** The closed match over the nine `Effect` variants. Called once per effect
per action, against the cells previously chosen by `resolve_targeting`.

Per-arm walkthroughs:

- **`Effect::DAMAGE { amount, .. }`** (lines 535–541): for each target cell with a
  ship, call `apply_damage`. The `..` pattern ignores `band_falloff` here — that
  field is read at the action level inside `apply_damage` step 1, not per-effect.
- **`Effect::APPLY_STATUS { status, duration }`** (lines 543–549): for each target
  cell with a ship, call `add_status`. Existing entry's duration becomes
  `max(existing, new)`.
- **`Effect::VENT_HEAT { amount, recharge_cooldowns }`** (lines 551–564): drop heat
  by `amount` floored at 0, clear lockout, optionally reset all cooldowns to 0
  (the `recharge_cooldowns: Some(true)` branch). Emits `OnVent`.
- **`Effect::REORIENT { to }`** (lines 566–577): switch orientation. `Flip` toggles
  via `flip_orientation`; `Broadside` sets `Orientation::Broadside`; `BowOn` defaults
  to `bow: Fore`. Emits `OnReorient`.
- **`Effect::SPAWN_ORDNANCE { projectile }`** (lines 579–588): clone the source ship
  (avoids holding `board.cells` borrowed while calling `content.spawn_projectile`),
  then push the new projectile onto `board.ordnance`.
- **`Effect::DISPLACE_SELF { mode, distance }`** (lines 590–592): delegate to
  `resolve_self_move`.
- **`Effect::DISPLACE_TARGET { mode, distance }`** (lines 594–598): for each target
  cell, delegate to `resolve_target_move`.
- **`Effect::DEPLOY { hazard }`** (lines 600–617): for each target cell, push a new
  `Hazard` onto `board.hazards[c]`. Note `DeployHazardKind` (mine|drone) widens to
  `HazardKind` (mine|drone|debris) — DEPLOY cannot produce debris, but the storage
  format is the broader enum.
- **`Effect::BOARD { .. }`** (lines 619–636): **doc-stubbed.** The lengthy comment
  explains: mass-* board-wide effects (mass_lock, mass_breach, mass_emp,
  sensor_pulse) are **field-kit Cards** in the analysis doc, not Actions — they live
  under `Catalog::fieldkit`, not `Catalog::actions`, and are resolved by a future
  field-kit handler, not through `applyEffect`. The TS body at `resolve.ts:226-227`
  is also empty, so the stub matches the canonical reference exactly. When a real
  Action carrying a BOARD effect lands (a class signature or capital-ship ability),
  this arm gets wired then.

**Drift note: `Effect::BOARD` is a documented stub, not an oversight.** The doc
comment is the canonical statement for any future reader who tries to "fix" this
arm. Don't.

---

### `fn destroy(cell, board, content)` (line 811)

**Mirrors:** `resolve.ts:334`.
**Design anchor:** HTML Part VII traits — ReactorBreach splash damage.
**Intent:** Destroy the ship at `cell`. The single place ships leave the board.

The walkthrough:

1. **Line 814**: `board.cells[cell].take()` removes the ship from the cells vector
   atomically. The cell is now `None` for any subsequent observer (including any
   splash damage we deal in step 3).
2. **Line 817**: capture the traits before the ship is moved out — needed for the
   ReactorBreach check below.
3. **Line 819**: increment `destroys_this_window`. The chain-kill counter that
   `execute_queue` and the ordnance phase consult.
4. **Lines 821–833**: if the ship had `Trait::ReactorBreach`, deal 2 splash damage
   to both lane neighbours through the **regular damage pipeline** (with
   `dummy_weapon()`). The splash routes through `apply_damage`, which means
   directional shields, target-lock, and subsystem modifiers all apply to splash
   hits; ReactorBreach hitting a flank could legitimately trigger a Marksman bonus.
5. **Line 835**: emit `OnLethal`. **This is the LAST step of `destroy()`.**

**Invariant (per the tester's `event_chain.rs` work, commit `4070a3d`):**
`destroy()` completes all splash-cascading direct calls (apply_damage on neighbours)
**before** emitting its own `OnLethal`. The OnLethal emit is the last step, after
the ReactorBreach splash loop has fully unwound — including any recursive `destroy()`
calls triggered by splash kills.

**Concrete consequence for subsystem authors:** an `OnLethal` subscriber for ship X
is guaranteed that any splash damage X dealt has already been observed via
`OnDamageTaken`. The ordering is **splash-before-lethal at every level of the
chain**.

**Worked example (cascading reactor breaches, `tests/event_chain.rs:cascading_reactor_breaches_chain_correctly`):**

Three ships on the lane: a "breacher" (ReactorBreach trait) at cell 1, a "tiny"
(ReactorBreach trait, low hull) at cell 2, a normal "neighbour" (10 hull) at cell 3.
`destroy(1, ...)` is called. The observable event order is:

```
damage(2)   // breacher's splash hits tiny
damage(3)   // tiny's splash hits neighbour (inside breacher's destroy)
lethal(2)   // tiny's OnLethal (after tiny's splash chain unwinds)
lethal(1)   // breacher's OnLethal (after the whole subtree returns)
```

**Both damages fire before either lethal.** If a future port moves the `OnLethal`
emit *before* the splash loop, this becomes `[damage(2), lethal(2), damage(3),
lethal(1)]` — the regression form named in the test's failure message (line 291–
293). The test's `Vec<String>` log assertion at lines 287–293 pins the exact trace.

`board.destroys_this_window` ends at 2 (line 296), which is what `detect_chain`
later reads as a chain-kill.

**Cross-references:**
- `apply_damage` (line 447) — the splash hits go through the full pipeline.
- `OnLethal` hook — emitted here, only here, after splash completes.
- `destroys_this_window` counter on `Board` — incremented unconditionally, even when
  the ship has no ReactorBreach trait.

---

### `fn detect_chain(board: &Board) -> bool` (line 1499)

**Mirrors:** `resolve.ts:346` (which is `TODO: count destroys within this execution
window; >=2 is a chain kill.`).
**Intent:** Read `board.destroys_this_window` and return whether the just-finished
window was a chain kill. Counter ≥ 2 → chain.

One line of body (line 1500): `board.destroys_this_window >= 2`. The counter is reset
to 0 at the top of every `execute_queue` and the top of the ordnance phase; `destroy`
increments it. The test `apply_damage_lethal_clears_the_cell` (line 1633) asserts
the counter increments to 1 on a single kill.

**Drift note: `destroys_this_window` field on `Board` (new vs. TS).** The TS has no
such field; the Rust port adds it explicitly per team coordination. Reset semantics
live in the resolver (the two `= 0` assignments in `execute_queue:158` and
`resolve_round:126`), not in the `Board` struct itself. See the `Board` walkthrough in
[`types.rs`](#srctypesrs) for the data-side description.

---

### Movement — `fn resolve_self_move(ship_cell, mode, distance, board, content)` (line 964)

**Mirrors:** Originally `resolve.ts:376` (which was a partial THRUST/BURN stub).
Filled in per task #6 / commit `da243be`.
**Intent:** Move the ship at `ship_cell` per `MovementMode`. Five modes, each with a
distinct landing rule and collision-damage policy. The doc comment at lines 927–963
is the canonical reference for the semantics; summarized here.

**Direction (lines 974–977):** the ship moves in its *bow* direction.
`BowOn { bow: Aft } → step = -1`; everything else → step +1. `Broadside` defaults to
+1 (arbitrary per the doc, matching TS).

**Per-mode landing computation (lines 983–1116):**

- **`THRUST`** (lines 984–997): exactly one step. Distance is ignored beyond the
  first cell. Blocked by either wall or occupant → stop in place, take 1 collision.
- **`BURN`** (lines 999–1016): walk step-by-step until blocked by wall or occupant.
  Collision damage = `max(0, distance - steps_taken)`.
- **`SLIP`** (lines 1018–1065): the path-passes-through-ships mode. Two passes:
  first cover the `distance` cells we're slipping through (no occupant check), then
  keep walking until the first free cell. If the lane runs out before a free cell
  appears, clamp to the edge and bill collision damage.
- **`JUMP`** (lines 1067–1084): blink-drive; compute the target cell directly. If
  off-board, clamp to edge and bill overflow as collision. If target cell occupied,
  the jump *fails entirely* (no-op) — JUMP "ignores the path" so there's nothing
  physical to collide with.
- **`TRACTOR_SWAP`** (lines 1086–1115): swap with the first adjacent occupant in the
  bow direction. No collision damage. No-op if the adjacent cell is empty or
  off-board. **This is the only mode with a fully-defined semantic that the TS
  source did not specify.**

**Drift note: `TRACTOR_SWAP` semantic (new in da243be).** TS leaves this mode as a
TODO. The doc-comment at lines 1086–1093 spells out the chosen semantic: "swap with
the first adjacent bow-direction occupant; no-op if empty." Coordinated with
team-lead. The choice was driven by the only carriers in today's catalog (the
Frigate's Slip signature, the Carrier's Swap-Toss), both of which target the ship
directly fore-of-bow.

**Drift note: collision damage routes through `apply_damage`.** Movement that ends
short of the requested cell bills `remaining_distance × 1` collision damage,
attributed via `dummy_weapon()` so falloff is skipped. The damage routes through
the regular pipeline — directional shield still mediates, so a ship that crashes
into something bow-first eats less damage than one that crashes stern-first.

The move is committed (lines 1118–1129) *before* the collision damage applies (lines
1135–1138), so the directional shield reads against the post-move orientation. The
attacker cell is one further in `step` from the landing — a "phantom attacker" on
the other side of the obstacle.

---

### Target displacement — `fn resolve_target_move(target_cell, source_cell, mode, distance, board, content)` (line 1159)

**Mirrors:** Originally `resolve.ts:390` (stub). Filled in per `da243be`.
**Intent:** Move the ship at `target_cell` per `DisplaceMode`. Three modes:

- **`Swap`** (lines 1177–1192): trade cells between source and target. No collision
  damage. No-op if source == target.
- **`Push`** (lines 1194–1252): target moves *away* from source (`step =
  sign(target - source)`). Stops at first occupant (including the source ship
  itself) or wall. Collision damage on stop, routed through the regular pipeline.
- **`Pull`** (same arm, different step direction): target moves *toward* source
  (`step = sign(source - target)`). Pull stops one cell short of source because
  the source counts as an occupant — "pull crashes the target into the operator,
  which is the canonical collision behaviour" (line 1226).

**Drift note: Push/Pull collision into source.** The TS stub doesn't specify what
happens when a pull would end on the source's cell. The chosen semantic: source
counts as an occupant, so pull stops one cell short and applies the standard
collision-damage rule. Documented inline at lines 1213–1228.

---

### `fn apply_modifiers(dmg, target_cell, band, board, content)` (line 910)

**Mirrors:** `resolve.ts:371` (stub).
**Intent:** Step 2 of the damage pipeline. Add subsystem damage modifiers to `dmg`,
then clamp to 0.

Formula (lines 887–905): `final = max(0, raw_falloff + Σ content.damage_modifier(...))`.
Additive across subsystems (Marksman +1, Point-Blank Doctrine +2 at pointBlank, …);
negative modifiers allowed but clamped to 0. **Target-lock doubling (step 3) is
applied to the post-modifier value**, so a +1 Marksman bonus followed by 2× lock is
`2 × (raw + 1)`, not `2 × raw + 1`.

Default `Content::damage_modifier` returns 0, so this function is a pass-through for
all current test/demo content. Concrete Content types that install subsystems
override the trait method.

---

### `fn decide_enemy_action(enemy_cell, board, content)` (line 1303)

**Mirrors:** `resolve.ts:395` (stub). Filled in per task #6 / commit `da243be`.
**Design anchor:** HTML Part IV closing paragraph — the AI maximises lane-end
diversity to force player stance flips.
**Intent:** Pick one action for this enemy and push it onto `ship.queue`. The
resolver runs the queue through `execute_queue` unchanged — the AI never bypasses
the pipeline.

The doc comment at lines 1255–1302 is the canonical algorithm description. Summary:

1. **Find the player** (lines 1310–1314). No player → return.
2. **Snapshot gating state** (lines 1318–1326). The scoring loop borrows the board
   read-only for `resolve_targeting`, so we copy out heat / cooldowns / mounts /
   traits up front.
3. **Compute covered ends** (lines 1338–1352). For each *already-queued* enemy (the
   AI runs in initiative order, so enemies 0..N-1 are decided by the time we run
   for enemy N), record which lane-end they threaten the player from.
   `direction_to(player_cell, enemy_cell)` is the lane end the shot arrives from.
4. **Enumerate + score threatening actions** (lines 1359–1417). For each mount's
   weapon: gate by cooldown, lockout, heat-budget (skip if firing would push more
   than 1 above heat_max), and arc/band via `resolve_targeting`. Score:
   - `+10` per cell hit that contains the player.
   - `+6` if the enemy threatens the player from a lane-end *not yet covered* by
     an already-queued enemy.
   - `+raw_damage` (sum of `Effect::DAMAGE` amounts).
   - `-heat` cost (halved for `BurnHard` ships).
   - `+2` for `Pursuit` ships that hit the player.
5. **Queue the best** (lines 1420–1424). If a threatening action scored, push it
   and return.
6. **Fallback ladder** (lines 1430–1483): when no action threatens the player, try
   in order: any DISPLACE_SELF action (movement intent), any REORIENT action
   (might bring the player into arc next turn), any VENT_HEAT action (at least
   clears heat). If even those fail, leave the queue empty.

**Drift note: visible-threat invariant.** Every successful AI turn produces a queued
action — the resolver renders queue contents over each ship, so pushing any action
id is enough to make the AI's intent legible to the player. The fallback ladder
exists precisely to ensure visibility even in degenerate setups.

---

### Helpers

- **`fn ships_of(board) -> Vec<Ship>`** (line 645) — clone every live ship. Used for
  snapshot iteration when the loop body may mutate the cells vector.
- **`fn enemy_initiative(board) -> Vec<usize>`** (line 652) — every enemy cell in
  lane order. TS comment: "telegraphed order; here simply lane order. Replace with
  explicit initiative."
- **`fn bearing_direction(ship, ship_cell, board, a) -> Option<LaneEnd>`** (line 666)
  — which lane direction does the action's mount bear toward? Arc-less actions
  return the first direction with a target; arc-required actions return whichever
  direction the mount actually bears toward.
- **`fn cells_toward(board, ship_cell, end) -> Vec<usize>`** (line 714) — all lane
  cells strictly in `end` direction from `ship_cell`. Safe at the lane edge thanks
  to the explicit aft-loop guard at line 725 (cell 0 cannot decrement further).
- **`fn first_target_toward(board, ship_cell, end) -> Option<usize>`** (line 743) —
  first occupied cell in `end` direction. One-line `find` over `cells_toward`.
- **`fn in_allowed_band(band, a, b) -> bool`** (line 749) — does the range band
  between cells `a` and `b` appear in the allowed-bands list?
- **`fn add_status(cell, kind, duration, board)`** (line 756) — add or extend.
  Existing status of the same kind gets `duration.max(new)`.
- **`fn tick_statuses(cell, board, content)`** (line 770) — pre-tick HullBreach
  damage, then decrement all durations and `retain` only positive ones. HullBreach
  routes through `destroy` if the ship dies.
- **`fn skips_turn(board, cell) -> bool`** (line 797) — does the ship have a
  `SystemsOffline` status?
- **`fn flip_orientation(o)`** (line 840) — `BowOn { bow }` becomes
  `BowOn { bow: opposite(bow) }`; `Broadside` is unchanged.
- **`fn dummy_weapon() -> Action`** (line 850) — the synthetic Action used by
  projectile impacts, ReactorBreach splash, and movement collisions. `band_falloff:
  Some(false)` so amounts land raw.

---

### `#[cfg(test)] mod tests` (resolve.rs:1508–end)

40+ inline tests, organized by function. Notable cases:

- **`apply_damage_weak_stern_takes_post_falloff_hit`** (line 1582) — demo Scenario A.
- **`apply_damage_strong_bow_soaks_to_zero`** (line 1598) — demo Scenario B.
- **`apply_damage_target_lock_doubles_and_consumes`** (line 1613) — step 3 contract.
- **`apply_damage_lethal_clears_the_cell`** (line 1634) — cell vacates + counter
  increments on kill.
- **`execute_queue_overheats_and_records_cooldown`** (line 1656) — heat → lockout
  transition.
- **`execute_queue_no_target_no_cost`** (line 1684) — the no-bore-no-cost contract.

Beyond the inline tests, the integration suite at `tests/event_chain.rs` (per
tester's work in commit `4070a3d`) walks multi-ship cascades — see the `destroy()`
worked example above for the canonical `cascading_reactor_breaches_chain_correctly`
trace.

---

### Drift watch list (resolved by `c5855ce + da243be + 6575472`)

- ~~**`Content` struct shape.**~~ Trait, not struct: `trait Content` with
  `action(id) -> Option<&Action>` + `spawn_projectile(kind, &owner) -> Projectile` +
  `damage_modifier(&target, band, &board) -> i32` default 0. The `&dyn Content`
  parameter rippled across the cascade (`apply_damage`, `destroy`, `tick_statuses`,
  `end_of_turn`, `advance_projectile`, `resolve_self_move`, `resolve_target_move`,
  effect dispatch); the pipeline ordering was preserved.
- ~~**Mutable board passing.**~~ Resolved by indexing cells by `usize` rather than
  holding `&mut Ship` borrows across calls. Every helper takes `ship_cell: usize`
  and re-looks-up the ship by index. The `emit` helper (line 97) `mem::take`s the
  bus off the board to release the borrow conflict during hook dispatch.
- ~~**`detect_chain` is a TODO.**~~ Wired through `Board.destroys_this_window`
  counter. `execute_queue` and the ordnance phase each open a window; `destroy`
  increments; `detect_chain` reads `>= 2`. The reset locations are documented
  inline.

**New decisions documented in this pass** (not from the pre-port watch list):

- **`Content::damage_modifier` trait extension**. Default impl returns 0 so existing
  test/demo Content types don't break. Subsystem registry lives on concrete Content,
  not Board, because the bus path can't reach the modifier step in time.
- **`destroy()` invariant: splash-before-OnLethal**. Worked-example trace recorded
  inline; the regression form is the reordered `[damage(2), lethal(2), damage(3),
  lethal(1)]`, named in the test's failure message.
- **`TRACTOR_SWAP` semantic**. "Swap with the first adjacent bow-direction occupant;
  no-op if empty." Coordinated with team-lead because the TS source didn't specify.
- **`Effect::BOARD` doc-stub**. Mass-* board-wide effects are field-kit Cards, not
  Actions; they live under `Catalog::fieldkit` and will be resolved by a future
  field-kit handler. The arm here mirrors the TS empty body.
- **Push/Pull collision into source**. Source counts as an occupant; pull stops one
  cell short and applies standard collision damage.
- **AI fallback ladder**. Movement → reorient → vent → empty queue. The
  visible-threat invariant ensures the AI's intent is always legible.
- **EventBus γ-invariant: no chained emit through `ctx.board.bus`**. The `emit`
  helper detaches the bus during dispatch; chained semantics must go through direct
  resolver calls (`apply_damage`, `destroy`, etc.). Canonical statement in the
  `EventBus` / `HookContext` docstrings (commit `6575472`).

No open items.

---

## Effects layer — folded into `src/resolve.rs`

*Navigational breadcrumb for readers who came looking for an `effects.rs` module.*

There is **no separate `src/effects.rs` file** in the Rust port. The five function
bodies the TS source leaves as TODO comments inside `resolve.ts` — `apply_modifiers`,
`resolve_self_move`, `resolve_target_move`, `decide_enemy_action`, and the per-effect
arms of `apply_effect` — all live inside `src/resolve.rs` alongside the resolver they
collaborate with. Documented in this file's [`src/resolve.rs`](#srcresolvers) section:

- **`apply_modifiers`** — see the "fn apply_modifiers" sub-entry. Step 2 of the
  damage pipeline; routes subsystem bonuses through `Content::damage_modifier`.
- **`resolve_self_move`** — see the "Movement" sub-entry. All five `MovementMode`s
  (THRUST / BURN / SLIP / JUMP / TRACTOR_SWAP) with collision damage routed through
  the regular pipeline.
- **`resolve_target_move`** — see the "Target displacement" sub-entry. Push / Pull /
  Swap with the "source counts as occupant" rule.
- **`decide_enemy_action`** — see the "AI" sub-entry. Scoring formula + visible-threat
  fallback ladder.
- **Per-effect dispatch** — see the "`fn apply_effect`" sub-entry covering all nine
  `Effect` variants including the `DEPLOY` arm and the `BOARD` doc-stub.

**Why no split?** Content kept these bodies in `resolve.rs` because they share the
resolver's private helpers (`dummy_weapon`, `add_status`, `cells_toward`,
`bearing_direction`) and because routing them through `&dyn Content` already
externalises the only state the TS would have wanted a separate module for
(subsystem registry). Splitting would either duplicate the helpers or force them
public for no benefit. The original `LINE_BY_LINE.md` skeleton's "effects.rs"
heading predates this decision; folding it here removes the confusion.

---

## Content / AI / EventBus — folded into `src/resolve.rs` and `src/types.rs`

*Navigational breadcrumb for readers who came looking for `content.rs`, `ai.rs`, or
`bus.rs` modules. None of these files exist in the Rust port.*

- **Content layer** — there is no `src/content.rs`. The runtime content surface is the
  `Content` trait plus its demo implementation, which live alongside the resolver; the
  catalog *loading* half lives in [`src/catalog.rs`](#srccatalogrs) and
  [`src/catalog_canonical.rs`](#srccatalog_canonicalrs). The projectile-spawn dispatch
  (`Content::spawn_projectile`) and board-effect dispatch (`Content::apply_board_effect`)
  are documented under the resolver's effect-dispatch entries.
- **AI** — there is no `src/ai.rs`. `decide_enemy_action` and its scoring helpers are
  folded into [`src/resolve.rs`](#srcresolvers) (see the "AI" sub-entry: scoring formula
  + visible-threat fallback ladder). They share the resolver's private helpers, so
  splitting would force those public for no benefit.
- **EventBus** — there is no `src/bus.rs`. The `Hook` enum, `HookContext`, and the
  `EventBus` (`on` / `emit`) interface live in [`src/types.rs`](#srctypesrs), documented
  there along with the "no chained emit" invariant (task #26).

These three skeleton headings predated the architecture settling; they're folded here so
a reader hunting for the old filenames lands somewhere useful.

---

## `src/catalog.rs`

*The front door to the content catalog: a `LoadError` enum that splits I/O from parse
failures, and a strict-first / canonical-fallback `load_from_path` / `load_from_bytes`
pair that auto-detects which on-disk shape it was handed and returns the same typed
[`Catalog`](#srctypesrs) either way. Full companion at
[`docs/MODULES/catalog.md`](MODULES/catalog.md).*

**Mirrors:** No direct TS analog. TypeScript loads its catalog inline in `demo.ts`; this
is Rust-specific loading glue.

**Intent:** Callers (the demo bin's startup loader, tester's `tests/catalog_smoke.rs`)
hand this module a path or byte slice and receive a typed `Catalog`. It owns error typing
and format auto-detect so no caller has to choose between a strict and a canonical loader.

### `enum LoadError` (src/catalog.rs:29)

Line 30: `#[non_exhaustive]` — leaves room for a future `BadSchema(String)` validation
variant without breaking downstream `match`es. Line 31-34: the two variants
`Io(io::Error)` / `Parse(serde_json::Error)`. Line 36-52: `Display` one-liners and an
`Error::source` impl that returns the wrapped cause for error-chain walking. Line 54-59:
`From<io::Error>` and `From<serde_json::Error>` so `?` lifts both transparently inside the
loaders.

### `fn load_from_path(path: impl AsRef<Path>) -> Result<Catalog, LoadError>` (src/catalog.rs:72)

**Intent:** Read the file, then defer to `load_from_bytes`. Line 73: `fs::read(path)?` —
any I/O error becomes `LoadError::Io`. Line 74: hand the bytes to `load_from_bytes` so the
path-based and byte-based loaders share one dispatch.

### `fn load_from_bytes(bytes: &[u8]) -> Result<Catalog, LoadError>` (src/catalog.rs:79)

**Intent:** Decode bytes with strict-first / canonical-fallback dispatch.

Line 81-83: **strict fast path** — `serde_json::from_slice::<Catalog>(bytes)`; on success
return immediately. The strict-parse error is swallowed on purpose (it just means "try the
other shape"). Line 85: `serde_json::from_slice` into a loose `Value`; a failure here is
genuinely malformed JSON and propagates as `LoadError::Parse`. Line 86: run
[`from_canonical_value`](#srccatalog_canonicalrs) and lift its error to `Parse`.

**Drift — auto-detect by trial, not by sniffing.** The loader doesn't inspect a
schema-version field; it tries strict and falls back. The canonical export is the only
loose shape expected today (see catalog_canonical.rs:47-54), and trial-decode keeps every
caller on one function.

**Cross-references:** Called by the demo bin startup and integration tests. On the
fallback path, calls `crate::catalog_canonical::from_canonical_value`
([`src/catalog_canonical.rs`](#srccatalog_canonicalrs)). Produces a
[`Catalog`](#srctypesrs) the resolver's content layer consumes.

**Tests** (src/catalog.rs:89): `loads_minimal_catalog` round-trips a hand-written strict
fixture; `placeholder_sections_default_to_empty_when_absent` pins the `#[serde(default)]`
attributes on the placeholder `Catalog` sections (reviewer m3/m4). The canonical fallback
path is exercised by tester's `tests/catalog_smoke.rs` against the real
`assets/broadside.catalog.json`.

---

## `src/catalog_canonical.rs`

*The bridge between the catalog's two on-disk shapes. The design HTML's "Copy JSON" button
emits a **flat** shape (terse field names, bare-string effects); the engine's strict types
expect a **nested** shape. This module walks the loose `serde_json::Value` tree, renames
fields, nests them into `cost`/`targeting`, and inflates each bare-string effect into a
typed record using documented defaults — producing a [`Catalog`](#srctypesrs) the resolver
can't distinguish from a strict-parsed one. Full companion at
[`docs/MODULES/catalog_canonical.md`](MODULES/catalog_canonical.md).*

**Mirrors:** No direct TS analog. TypeScript loads its catalog inline in `demo.ts` and
never grew a loose/strict split. This is Rust-specific glue born of consuming the analysis
HTML's export directly.

**Intent:** The canonical shape is the source of truth bruce hand-edits in the analysis
doc. Rather than make the design doc emit verbose engine JSON, this module infers the
missing structure. The flat action `{ id, heat, cd, band, pattern, arc, freeplay,
effects: ["DAMAGE"] }` becomes the nested `{ id, cost{…}, targeting{…}, effects: [{ kind:
"DAMAGE", amount: 3 }] }`. Every difference between those is synthesized here. Inference is
deliberately conservative (catalog_canonical.rs:360-364) — under-tuned defaults
playtesting can flag, not magic numbers scraped from `desc` prose.

### Inference rules (summary)

`heat→cost.heat`, `cd→cost.cooldownMax`, `!freeplay→cost.advancesTurn` (negated),
`band→targeting.optimalBand` and `targeting.band:[band]` (single-element — the conservative
"fires only at optimal range" read until a real allowed-bands field lands),
`pattern→targeting.pattern`, `arc→targeting.requiresArc` (null if absent),
`facingRelative:true` always, `hits_all`/`hitsAll` default false. Subsystems:
`unlock→unlockSalvage`, missing `level` defaults to 1. Classes: `affinity "bow-on"→"bowOn"`,
`set1`/`set2` display-names→action-ids, `signature` prose→snake_case id from the leading
title.

### `fn from_canonical_value(root: Value) -> Result<Catalog, serde_json::Error>` (src/catalog_canonical.rs:70)

**Intent:** The single public entry point. Transforms the three sections with structural
drift (`actions`, `subsystems`, `classes`), leaves every other section untouched, then
hands the rebuilt tree to `serde_json::from_value`.

Line 71-74: peel the root to its object map; a non-object root is handed straight to serde
so the error message describes the real type mismatch. Line 84: declare the
`action_name_to_id` lookup the class normalizer needs. Line 85-102: the **actions** block —
`transform_action` each element, `filter_map(...ok())` silently drops failures (losing one
weapon beats failing the whole load), then populate the lookup with case-folded
`name→id` so `"Twin-Linked"` and `"twin-linked"` both resolve. **Actions must go first**
because `transform_class` borrows this map. Line 103-109: **subsystems** (infallible
`map`). Line 110-116: **classes**, borrowing the lookup. Line 118-121: a comment noting the
canonical-only `archetypes`/`bays` top-level keys pass through harmlessly (no
`deny_unknown_fields`). Line 123-124: reassemble and do the real typed decode.

**Cross-references:** Called by `load_from_bytes` (catalog.rs) on the canonical fallback.
Calls `transform_action`, `transform_subsystem`, `transform_class`.

### `fn transform_action(v: Value) -> Result<Value, &'static str>` (src/catalog_canonical.rs:134)

**Intent:** Flat action → strict nested shape. `Err` on a missing *required* field so the
caller's `filter_map` can skip it.

Line 135-137: let-else bail on a non-object. Line 141-147: pull/rename the required scalars
(`heat`/`cd` as `i64`, `band`/`pattern` as `String`) via `remove` so the flat keys don't
survive. Line 143: `freeplay` defaults false. Line 148: `arc` kept as raw `Option<Value>`
(may be null/absent). Line 149-152: `hits_all` accepts either casing, default false.
Line 154-165: map the loose `effects` strings through `inflate_effect` using `archetype`
(default `"beam"`) and `id` as hints. Line 167-172: build `cost { heat, cooldownMax,
advancesTurn: !freeplay }`. Line 174-193: build `targeting` — note line 182-183 seeds
`band` as a single-element array; line 184-190 normalizes `arc` to a string or null; line
191 hardcodes `facingRelative: true`. Line 197: strip UI-only `desc`.

**Cross-references:** Called by `from_canonical_value`. Calls `inflate_effect`. Produces an
[`Action`](#srctypesrs).

**Worked example** (`canonical_pulse_laser_parses`, src/catalog_canonical.rs:617): the flat
`pulse_laser` decodes to `cost.heat=1`, `cooldown_max=0`, `advances_turn=true`
(freeplay=false), one `Effect::DAMAGE { amount: 3 }` (beam + heat 1 → heat+2).

### `fn transform_subsystem(v: Value) -> Value` (src/catalog_canonical.rs:205)

**Intent:** Infallible flat→strict subsystem. Line 209-211: `unlock→unlockSalvage` (value
preserved; `null` stays `None`). Line 213: missing `level` defaults to 1. Line 215: strip
`desc`.

**Worked example** (`subsystem_unlock_renames_to_unlock_salvage_and_level_defaults`,
src/catalog_canonical.rs:700): `marksman` with `"unlock": null` and no `level` →
`unlock_salvage=None`, `level=1`, `max_level=3`.

### `fn transform_class(v, action_name_to_id) -> Value` (src/catalog_canonical.rs:237)

**Intent:** Three drifts. Line 242-248: affinity `"bow-on"→"bowOn"` (others pass through).
Line 251-259: `set1`/`set2` display-names→ids via `normalize_action_ref`. Line 262-273:
`signature` prose→id via `signature_id_from_prose`, falling back to raw prose (with an
`eprintln!`) if derivation yields empty.

**Worked example** (`canonical_class_normalizes_set_refs_and_signature`,
src/catalog_canonical.rs:995): `wanderer` → `set1=["broadside_battery","pulse_laser"]`,
`set2=["railgun_broadside","grav_snare"]`, `signature="slip"`.

### `fn normalize_action_ref(...) -> Value` (src/catalog_canonical.rs:281)

**Intent:** Resolve one set-ref. Line 291-293: skip the lookup if the string already looks
like a snake_case id (hybrid-catalog support). Line 294-304: case-folded lookup; a miss
logs and passes the original through (resolver silently skips unknown refs — better than
failing the load over a typo).

**Worked examples:** `unmapped_set_ref_passes_through` (`"Ghost Weapon"` stays verbatim);
`snake_case_set_ref_skips_lookup` (`"pulse_laser"` skips the lookup).

### `fn signature_id_from_prose(prose: &str) -> String` (src/catalog_canonical.rs:317)

**Intent:** Pull a snake_case id from a Signature prose string. Line 320-324: split on
em-dash (U+2014), then `" - "`, else treat the whole string as the title. Line 331-344: the
snake_case loop (lowercase alnum, collapse whitespace/`-`/`_` to a single `_`, drop other
punctuation). Line 346-348: strip trailing underscore.

**Worked examples** (src/catalog_canonical.rs:959): `"Slip — …"→"slip"`,
`"Swap Toss — …"→"swap_toss"`, `"Phase - …"→"phase"`, `"Ram The Target"→"ram_the_target"`,
empty/whitespace/`"—"`→`""`.

### `fn rewrite_self_relative_signature(action_id, effects) -> Value` (src/catalog_canonical.rs:397)

**Intent:** A pre-pass run by `transform_action` (line 169) **before** `inflate_effect`,
added by the #84 follow-up fix. The canonical export ships `slip`/`swap_toss`/`ram`/`throw`
as `SELF`-pattern `DISPLACE_TARGET` actions — mechanically **dead**, because
`resolve_targeting` returns `[ship_cell]` for `SELF`, so a `DISPLACE_TARGET` runs against
the source ship itself (a no-op for the swaps, a wrong-direction self-shove for the rams,
with the trailing `DAMAGE` then striking the empty origin). The canonical prose is
self-relative, so the faithful kind is `DISPLACE_SELF`, which `resolve_self_move` runs
correctly. Line 375/382: the two id sets `["slip","swap_toss"]` / `["ram","throw"]`. Line
398-403: non-signature ids pass through untouched. Line 404-417: rewrite `"DISPLACE_TARGET"`
→ `"DISPLACE_SELF"`, and for the ram-style ids **drop** the trailing `"DAMAGE"` (collision
billing inside `resolve_self_move` owns it). The mode mapping itself happens downstream in
`inflate_effect`'s `DISPLACE_SELF` arm. **Cross-references:** called at line 169; output
mode-mapped by `inflate_effect`. **Worked example:**
`signature_actions_change_board_state_through_resolver` (src/catalog_canonical.rs:1165) fires
each signature through the real resolver and asserts board state changed (the regression
proving these are no longer dead).

### `fn inflate_effect(v, archetype, heat, action_id) -> Value` (src/catalog_canonical.rs:437)

**Intent:** Turn one bare-string effect into a `kind`-tagged object, inferring fields from
archetype/heat/id. Already-object effects pass through (hybrid-catalog support).

The `match kind`: **DAMAGE** — `amount` by tier (`beam`/`broadside`→`heat+2`,
`ordnance`→0, `displacement`/`control`→2, else `max(heat,1)`); falloff omitted to keep the
strict `None` default. **APPLY_STATUS** — `ordnance` and `displacement`/`control`→
`systemsOffline`, else `hullBreach`; `duration:3`. **DISPLACE_TARGET** (486-519) — `mode` by
id keyword (`tractor+toss`→swap, `tractor`/`pull`→pull, repulsor/push/snare→push, default
push); `distance:2`. Post-#84-fix this arm sees only **genuine target-displacement** actions
(tractor_beam/repulsor/grav_snare/tractor_toss); the self-relative signatures were already
rewritten away by `rewrite_self_relative_signature`. **DISPLACE_SELF** (520-563) — `mode`/
`distance` by id keyword: `phase`→`(SLIP,2)`; `slip`/`swap_toss`→`(TRACTOR_SWAP,1)`;
`ram`/`throw`→`(BURN,2)` (collision damage billed by `resolve_self_move`); else `(THRUST,1)`.
`direction` is omitted (orientation-relative) for all except `throw`, which sets
`direction:"aft"`. **REORIENT**→`to:"flip"`. **SPAWN_ORDNANCE**→`projectile:<id>`.
**VENT_HEAT**→`amount:3, rechargeCooldowns:false`. **DEPLOY**→`drone` if id contains
"drone", else `mine`. **BOARD**→`note:<id>`. Unknown kind → just `{ kind }`, letting serde
fail loudly downstream.

**Cross-references:** Called by `transform_action` (after `rewrite_self_relative_signature`).
Produces [`Effect`](#srctypesrs) variants consumed by the resolver's `apply_damage` / effect
dispatch / `resolve_self_move`.

**Worked examples:** `ordnance_apply_status_infers_systems_offline` (Heavy Torpedo →
`APPLY_STATUS{SystemsOffline,3}`); `tractor_beam_displace_infers_pull` (→ Pull);
`repulsor_displace_infers_push` (→ Push); `tractor_toss_infers_swap` (→ `DISPLACE_TARGET{Swap}`);
`slip_inflates_to_self_tractor_swap` + `swap_toss_inflates_to_self_tractor_swap`
(→ `DISPLACE_SELF{TRACTOR_SWAP}`); `phase_infers_slip_movement_mode` (→ `DISPLACE_SELF{SLIP}`);
`self_relative_signatures_inflate_to_displace_self` (the consolidated pin: ram→BURN
bow-relative, throw→BURN aft, swaps→TRACTOR_SWAP, phase→SLIP);
`already_strict_effect_passes_through` (`{kind:DAMAGE,amount:99}` survives untouched).

**Drift — inferred numerics.** Effect amounts/durations/distances are inferred from
archetype + heat, not present in canonical data — conservative defaults meant to be tuned
by playtesting, not authoritative balance.

**Drift — self-relative signature repair (#84 follow-up).** The loader knowingly deviates
from the literal export: `slip`/`swap_toss`/`ram`/`throw` are listed as dead `SELF`-pattern
`DISPLACE_TARGET` but rewritten to `DISPLACE_SELF` to match the canonical prose and run
faithfully. `swap_toss` is narrowed to a single bow-side `TRACTOR_SWAP` (the engine has no
two-sided swap effect) — a faithful subset, flagged to the lead.

---

## `src/runs.rs`

*Phase 3 run-loop logic — the campaign state machine. Reads a live [`Board`](#srctypesrs)
to decide if an encounter is won/lost (`encounter_outcome`), mutates a [`Run`](#srctypesrs)
to advance through the sector map (`advance_after_win`), and materializes a fresh `Board`
for each encounter from a spawn list plus the player's carried-over ship
(`build_encounter_board`). Ships three placeholder sectors with a final boss. Full
companion at [`docs/MODULES/runs.md`](MODULES/runs.md).*

**Mirrors:** No TS analog. `demo.ts` is a single hand-built board with no campaign layer.
Phase-3-only. Architect's #75 supplies the types; this is the runtime layer on top.

**Intent:** Turn the inert `Sector`/`EncounterDef`/`Run`/`ShipSpawn` structs into a
working campaign. The bin's loop: resolve a round → `encounter_outcome` → on `Won`,
`advance_after_win` + build the next board → on `Lost`, `mark_defeated`. Two design calls
baked in: placeholder sectors live in a stand-alone function (not on `DemoContent`) because
they're read once per transition, not per frame; and spawns reference a `class_id` resolved
by a builder closure, so the same encounter code works with placeholder and real catalog
data.

### `enum EncounterOutcome` + `fn encounter_outcome(board) -> EncounterOutcome` (src/runs.rs:63, 80)

**Intent:** One cheap single-pass scan of `board.cells` classifies the board as
`InProgress` / `Won` / `Lost`. Line 91-97: no player → `Lost` (loss takes precedence; an
empty board returns `Lost` as the more honest signal); no enemy → `Won`; else `InProgress`.
**Cross-references:** called by the bin after each `resolve_round`; gates `advance_after_win`
vs `mark_defeated`. **Worked examples** (src/runs.rs:659-687): both present → `InProgress`;
no enemies → `Won`; no player or empty → `Lost`.

### `enum AdvanceResult` + `fn advance_after_win(run, sectors) -> AdvanceResult` (src/runs.rs:106, 130)

**Intent:** Advance a confirmed-won run; mutates `completed_encounters` and possibly
`current_sector_idx` / `victorious`. **Pre-condition:** caller already confirmed the win via
`encounter_outcome` — this function never touches the board. Line 131-133: an already-ended
run → `AlreadyEnded`. Line 135-141: out-of-bounds sector → defensively declare victory. Line
144-150: more encounters in this sector → increment, `NextEncounter`. Line 153-158: next
sector exists → advance idx, reset `completed_encounters` to 0, `NextSector`. Line 162-163:
final sector cleared → set `victorious`, `Victorious`. The bin branches on the four variants
to pick the between-encounter UI / end-of-sector / final-victory screen.

**Worked examples** (src/runs.rs:695-746): within-sector → `NextEncounter`; last-of-sector →
`NextSector` (idx 1, encounters reset 0); last-of-last-sector → `Victorious`; already
ended → `AlreadyEnded`.

### `fn mark_defeated(run)` + `fn current_encounter(run, sectors) -> Option<&EncounterDef>` (src/runs.rs:169, 176)

`mark_defeated` flips `run.defeated` (idempotent), called on `Lost`. `current_encounter`
indexes `sectors[current_sector_idx].encounters[completed_encounters]`, returning `None` on
an ended run or out-of-bounds indices (bin shows the end-of-run overlay).
**Worked examples** (src/runs.rs:757-781): ended run → `None`; fresh → `"drift_belt_a"`;
after one win → `"drift_belt_b"`.

### `fn build_encounter_board<F>(encounter, player, class_to_ship) -> Board` (src/runs.rs:206)

**Intent:** Instantiate a fresh board for an encounter. The player's *current* ship
(hull/heat/cooldown/status carried over) is placed at cell 0 with its `cell` normalized to
0 ("start at the lane mouth"); enemy spawns populate the rest via the `class_to_ship`
closure; hazards drop in. Line 216-223: lane size from the max occupied cell, rounded up to
canonical 5/7/9. Line 233-252: place each spawn, **skipping** off-board, cell-0 (player
collision), or occupied cells; apply `orientation` and `hp_override`. Line 261-269: assemble
with a fresh `EventBus::default()`. The closure parameter is the flexibility seam — bin
passes a catalog-aware builder, tests pass `fallback_ship_for_spawn`.

**Worked examples** (src/runs.rs:847-920): player at cell 0 with hull preserved; enemies at
their cells with override applied; a cell-0 spawn dropped to protect the player.

### `fn canonical_lane_size(max_cell) -> usize` (src/runs.rs:275)

`0..=4 → 5`, `5..=6 → 7`, `_ → 9` — the analysis doc's early/mid/late lane lengths. Pinned
by `build_board_uses_canonical_lane_size` (src/runs.rs:978).

### `fn boss_ship_for_spawn(spawn) -> Ship` + `fn fallback_ship_for_spawn(spawn) -> Ship` (src/runs.rs:315, 362)

**Intent:** Two spawn→`Ship` builders. `boss_ship_for_spawn` is the Citadel Warlord (#83):
hull 14 (double the regular cap), bow armour 3 (hard frontal approach, soft stern flank),
three mounts (forward `pulse_laser` + `beam_cannon`, broadside `missile_salvo` — so the AI
telegraph surfaces real threats), and `Trait::ReactorBreach` (death splashes neighbors via
the resolver's `destroy()`). `hp_override` still wins if set. `fallback_ship_for_spawn` is
the "any enemy" default — hull 3, one forward `pulse_laser`. The bin's spawn callback routes
`class_id == "warlord"` to the boss builder, everything else to the fallback.

**Worked examples:** `boss_ship_for_spawn_has_climactic_loadout` (src/runs.rs:922) pins hull
≥14, `ReactorBreach`, ≥3 mounts, bow armour ≥3; `boss_ship_for_spawn_honors_hp_override`
(src/runs.rs:962) — override 20 beats the 14 default.

### `fn placeholder_sectors() -> Vec<Sector>` (src/runs.rs:413)

**Intent:** Three demo sectors of ascending difficulty: Drift Belt (patrol 1, two weak
encounters), Ion Reefs (patrol 2, three encounters with trait variety), Citadel Approach
(patrol 3, two encounters + the `is_boss: true` Citadel Warlord encounter flanked by two
`voidrunner` escorts). Returned as a plain `Vec<Sector>` so swapping to a typed
`Catalog::sectors` is mechanical. `spawn` / `enc` (src/runs.rs:421, 430) are terse local
constructors.

**Worked examples** (src/runs.rs:785-843): three sectors, ascending patrol tiers; every
sector has ≥1 encounter; exactly one boss, at the very end; loose density progression.

**Drift — placeholder data.** The sectors are Rust literals standing in until
`Catalog::sectors` (currently `Vec<serde_json::Value>`) is typed. `ShipSpawn::class_id` has
a deferred `→ template_id` rename noted in types.rs.

---

## `src/meta.rs`

*Cross-run meta-progression — the persistent layer that survives death. Owns the
`MetaProgression` struct (unlocked subsystems/cards + cumulative `total_salvage_earned`),
the salvage-per-kill math, and the salvage-gated unlock ladder. On run end the bin calls
`accumulate_into_meta` to roll the run's salvage forward and apply any thresholds crossed.
Full companion at [`docs/MODULES/meta.md`](MODULES/meta.md).*

**Mirrors:** No TS analog. Phase-3-only. Defines structure + persistence + threshold logic;
the between-encounter purchase UI is renderer's #77 screen on top.

### `struct MetaProgression` + `enum MetaError` (src/meta.rs:72, 91)

`MetaProgression` is three `#[serde(default)]` fields: `unlocked_subsystems`,
`unlocked_cards` (reserved, empty in Phase 3), `total_salvage_earned` (never decremented).
`MetaError` is `Io`/`Parse`, mirroring [`catalog::LoadError`](#srccatalogrs) so callers `?`
uniformly.

### `impl MetaProgression` — persistence + queries (src/meta.rs:121)

`load_from_disk` (src/meta.rs:125) returns `Ok(default)` on a **missing file** (first-run is
not an error). `save_to_disk` (src/meta.rs:136) creates parent dirs and does a plain
`fs::write` — **not** the atomic tmp+rename of `Run::save_to_disk`, because a torn meta write
at worst loses one rollover. `has_subsystem` (src/meta.rs:150) covers starter + unlocked in
one check; `available_subsystems` (src/meta.rs:159) returns the full pool as a `HashSet`.
`STARTER_SUBSYSTEMS` (src/meta.rs:169) pulls the three always-available ids from
`subsystems.rs` constants to avoid string drift.

### Salvage math (src/meta.rs:195, 220, 251)

`salvage_for_destroyed` — hull-weighted: `≤3 → 1`, `≤6 → 2`, `7+ → 3`.
`salvage_for_encounter_win` — sums over the encounter's **spawn list** (the board is empty
by win time), honoring `hp_override`, then `×2` for `is_boss`. `award_run_salvage` —
`saturating_add` into the live `Run`, called once per encounter-complete.

**Worked examples:** `salvage_for_encounter_sums_per_enemy` (src/meta.rs:387) — 1+1+3 = 5;
`salvage_for_boss_encounter_doubles` (src/meta.rs:421) — 3 × 2 = 6;
`award_run_salvage_saturates_not_overflows` (src/meta.rs:461).

### Unlock ladder + `fn accumulate_into_meta(meta, run) -> Vec<String>` (src/meta.rs:272, 293)

`SUBSYSTEM_UNLOCK_THRESHOLDS` (src/meta.rs:272) is the single source of truth:
`rear_gunner` 10, `chain_bounty` 25, `overcharge` 50, `crossfire` 100. `accumulate_into_meta`
rolls the run's salvage into the persistent total (`saturating_add`) and **edge-triggers**
each unlock with `prev_total < threshold && new_total >= threshold` (a `contains` guard
prevents duplicates), returning the newly-unlocked ids for the run-end "UNLOCKED: …"
overlay. Called on **every** run end, win or loss — the design rewards engagement.
Idempotent only at the caller's level; the bin must fire it once.

**Worked examples:** `accumulate_crosses_threshold_unlocks_subsystem` (src/meta.rs:514) —
salvage 10 unlocks `rear_gunner`; `accumulate_multiple_thresholds_in_one_jump`
(src/meta.rs:525) — salvage 26 unlocks two; `accumulate_idempotent_for_already_unlocked`
(src/meta.rs:537) — no duplicate. The `unlock_thresholds_are_in_ascending_order` invariant
(src/meta.rs:604) guarantees the ladder is monotonic, which the edge-trigger relies on.

**Cross-references:** Reads `subsystems.rs` constants; consumes [`Run::salvage`](#srctypesrs);
called by the bin between `Run::delete_save` and `MetaProgression::save_to_disk` on run end.

---

## `src/save.rs`

*Per-run save/load: three methods on [`Run`](#srctypesrs) (`save_to_disk` /
`load_from_disk` / `delete_save`) plus a `SaveError` enum. Persists the in-progress run only;
cross-run progression lives separately in [`meta.rs`](#srcmetars) so deleting a save on
Game-Over never wipes permanent unlocks. Full companion at
[`docs/MODULES/save.md`](MODULES/save.md).*

**Mirrors:** No TS analog. Phase-3-only.

**Drift — JSON, not postcard.** Task #79's brief asked for `postcard`. The module ships
**JSON** because postcard can't encode internally-tagged enums and
[`Orientation`](#srctypesrs) is `#[serde(tag = "stance")]`; JSON costs no new deps, matches
the `MetaProgression` precedent, and the format asymmetry the brief anticipated turned out
not to be load-bearing (src/save.rs:20-31).

### `enum SaveError` (src/save.rs:69)

`Io` / `Encode` / `Decode`. The `Encode` vs `Decode` split is the meaningful one: "couldn't
write" (prompt / fall back to memory) vs "couldn't read the file we have" (prompt to delete
+ start fresh). There is **no** blanket `From<serde_json::Error>` — it would be ambiguous
between the two — so each call site maps explicitly.

### `fn Run::save_to_disk(&self, path)` (src/save.rs:113)

**Intent:** Pretty-JSON serialize, written **atomically** via tmp+rename so a crash mid-write
can't leave a partial save. Line 115-119: bootstrap parent dir. Line 120: serialize
(`map_err(SaveError::Encode)`). Line 121-123: write `<path>.tmp`, then `fs::rename` (the
cross-platform atomic-replace). **Worked example** (`save_writes_atomically_no_tmp_file_left_behind`,
src/save.rs:287): the tmp file is renamed away, leaving no cruft.

### `fn Run::load_from_disk(path) -> Result<Option<Run>, SaveError>` (src/save.rs:132)

**Intent:** The `Option` encodes first-run as a non-error: `Ok(None)` = no save exists,
distinct from `Err` = save exists but is broken. Line 138: parse failure →
`SaveError::Decode`. **Worked examples** (src/save.rs:242-285): round-trips a populated run;
missing file → `None`; garbage → `Decode`.

### `fn Run::delete_save(path)` (src/save.rs:147) + `fn tmp_path_for(path)` (src/save.rs:159)

`delete_save` removes the file **idempotently** (already-absent is `Ok`), intended for
Game-Over, and does **not** touch the meta save. `tmp_path_for` computes the same-directory
`.tmp` path used by the atomic write (same filesystem guarantees the rename is atomic).
**Worked example:** `delete_is_idempotent` (src/save.rs:263).

**Cross-references:** Called by the bin at startup (`load_from_disk`), on turn-commit /
transitions (`save_to_disk`), and on run end (`delete_save`). The end-of-run sequence pairs
with [`meta.rs`](#srcmetars)'s `accumulate_into_meta` + `MetaProgression::save_to_disk`.

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
- `tests/event_chain.rs` — multi-ship cascades; the canonical
  `cascading_reactor_breaches_chain_correctly` event-order test pins the
  splash-before-OnLethal invariant (see the `destroy()` walkthrough in
  [`src/resolve.rs`](#srcresolvers)).
- `tests/pipeline.rs` — action-level `band_falloff` aggregation; pins the
  predicate that one `Effect::DAMAGE { band_falloff: Some(false) }` on an action
  disables falloff for the *whole* `apply_damage` call, not just that effect.
- `tests/catalog_placeholders.rs` — catalog parses with the five `unknown[]`
  placeholder sections (capitals/classes/fieldkit/sectors/commendations) absent.
- `tests/catalog_smoke.rs` — smoke test against the canonical
  `assets/broadside.catalog.json` exported by the analysis HTML.
- `tests/proptest.rs` — randomised invariants for `band_falloff` (monotone
  non-increasing with band distance, floored at 0) and `absorb_shield` (charge
  consumption, armour subtraction, no-consume on zero damage).

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
