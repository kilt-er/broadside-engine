# `src/types.rs` — Module Companion

*A self-contained walkthrough of the type surface. The same content as the
[`types.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srctypesrs), but scoped:
this file assumes you only care about the data model and don't need the rest of the
engine in context. Read this if you are about to add a content field, change a serde
shape, or modify the event bus.*

**Source commits:** `5625f30` (initial port) + `291206d` (reviewer-audit response).
**Mirrors:** `_drive_pull/broadside-engine/engine/types.ts`.
**Design anchor:** Part XIII (Engine Integration & Schema) of the
[Broadside Mechanics & Engine Analysis](../../_drive_pull/broadside-analysis.html).

---

## Why this file exists

Broadside is built on the principle that *the data describes the game, the resolver
applies the data*. Every weapon, system, maneuver, ordnance launch, and vent is the
same `Action` record. Every status, mod, and trait is a tagged value. Every event the
resolver fires is one of a closed set of `Hook` names. New content is data added to
the catalog, not code added to the resolver.

`types.rs` is the single source of truth for that data shape. If you want to know what
the engine *can* model, this is the file to read. If you want to know what JSON the
analysis HTML's "Copy JSON" button produces, this is the file that parses it.

Three things to know up front:

1. **The TS is canonical.** When this Rust port and the TS reference disagree, the TS
   is right. (`types.rs:5`.)
2. **The catalog wire shape must round-trip byte-for-byte with the TS.** The serde
   conventions documented at `types.rs:20–33` exist to enforce that.
3. **The runtime vs. catalog split is load-bearing.** `Board` and `EventBus` are not
   serde — they're runtime collaborators. `SubsystemDef` is the catalog half;
   `Subsystem` (with its callback) lives next to the content slice.

---

## The 53-line module header

Read `types.rs:1–53` before doing anything else. Six sub-sections:

- **Intent + tie-breaker** (1–6) — the canonical-TS rule.
- **Layout** (7–18) — the eight-section structure that mirrors the TS.
- **Serde conventions** (20–33) — three rules:
  `Orientation` tagged on `stance` camelCase;
  `Effect` tagged on `kind` with `SCREAMING_SNAKE_CASE` variants;
  most other enums `rename_all = "camelCase"`;
  `Hook` variants are camelCase `"onFoo"`.
- **Runtime vs. catalog split** (35–41) — see above.
- **Numeric types** (43–53) — TS `number` → `i32` for game quantities (can go negative
  mid-calc), `usize` for cell indices, `u32` for non-negative counts, `u8` for patrol
  tier.

The numeric mapping is the single rule most likely to bite you on a future port.
**Hull, damage, cooldown, armour, charge are all `i32`** because the resolver computes
`target.hull -= dmg` and checks `<= 0`; the field is allowed to hold a momentary
negative.

---

## The eight sections

The file follows the TS banner comments exactly. Each section gets a one-line
summary plus a deeper note on the items that matter.

### 1. Geometry primitives (lines 59–117)

Six enums, all `Copy + Eq + Hash + Serialize + Deserialize`:

| Type          | Variants                                                 | Wire shape                          |
|---------------|----------------------------------------------------------|-------------------------------------|
| `LaneEnd`     | `Fore`, `Aft`                                            | `"fore"`, `"aft"`                   |
| `Orientation` | `BowOn { bow: LaneEnd }`, `Broadside`                    | tagged on `stance`                  |
| `HullZone`    | `Bow`, `Stern`, `Port`, `Starboard`                      | camelCase                           |
| `RangeBand`   | `PointBlank`, `Close`, `Mid`, `Long`, `Extreme`          | camelCase, **declaration order is load-bearing** for `geometry::band_index` (exhaustive match — drift caught at compile time) |
| `Arc`         | `Forward`, `BroadsideArc`, `Turret`, `Rear`              | camelCase                           |
| `Faction`     | `Player`, `Enemy`                                        | camelCase                           |

### 2. Board (lines 119–168)

- **`Board`** — *not* serde. Runtime only. Holds `cells`, `ordnance`, `hazards`, the
  `EventBus`, the chain-kill counter `destroys_this_window`, and `fire_events:
  Vec<FireEvent>` (both new vs. TS; both transient render state excluded from
  `BoardSnapshot`).
- **`FireEvent`** (#59) — one exact attacker→target shot `{from_cell, to_cell,
  archetype, attacker_faction, hit}`, recorded by the resolver in `run_action` so the
  renderer draws a precise beam instead of guessing who-shot-whom. `hit` is currently
  always `true` (reserved for the #81 dodge-whiff miss path). See the LINE_BY_LINE
  `FireEvent` entry and [`vfx.md`](vfx.md).
- **`Hazard`** — cell-resident feature: mine / drone / debris. `ttl: Option<u32>` is
  `?:` (omittable), not `null` (always-present-or-null).
- **`HazardKind`** — three variants. Distinct from `DeployHazardKind` (action-effect
  subset, mines + drones only).

### 3. Ship (lines 170–316)

The biggest data record. Field order matches the TS Ship interface.

- **`Ship`** — id, faction, cell, orientation, hull, heat, locked_out,
  `shield_profile: ShieldProfile`, mounts, queue, cooldowns, statuses, traits,
  `klass: Option<String>`. `klass` is **kept as `klass`** (not renamed to `class`)
  for cross-port grep parity.
- **`ShieldFace`** — `{ armour, charge }`. Direct port.
- **`ShieldProfile`** — `{ bow, stern, port, starboard }`. **Named-field struct**, not
  HashMap (audit M1 fix). Implements `Index`/`IndexMut` over `HullZone` so
  `sp[HullZone::Bow]` works.
- **`Mount`** — `{ id, arc, weapon: actionId }`.
- **`Status`** — `{ kind, duration, face? }`. The `face` field is dead weight pending
  content/resolver confirmation that nobody plans to read it.
- **`StatusKind`** — `HullBreach`, `SystemsOffline`, `TargetLock`, `ShieldsUp`.
- **`Trait`** — 10 variants: 5 base (Pursuit, Agile, ReactorBreach, BurnHard,
  Anchored) + 5 elite. **No `rename_all`** — TitleCase variants already match the TS
  string union.

### 4. Action (lines 318–396)

- **`Action`** — id, name, archetype, cost, targeting, effects, `r#mod: Option<String>`,
  icon. The `r#mod` raw identifier is needed because `mod` is a Rust keyword; JSON
  wire shape is `"mod"`.
- **`ActionCost`** — `{ heat, cooldown_max, advances_turn }`. Field renames:
  `cooldownMax`, `advancesTurn`.
- **`Targeting`** — `{ pattern, band, optimal_band, requires_arc, facing_relative,
  hits_all }`. Renames: `optimalBand`, `requiresArc`, `facingRelative`, `hitsAll`.
- **`WeaponArchetype`** — 7 variants, camelCase.
- **`TargetingPattern`** — 8 variants, **SCREAMING_SNAKE_CASE preserved** for grep
  parity. `#[allow(non_camel_case_types)]` silences clippy.

### 5. Effects (lines 398–500)

- **`Effect`** — 9 variants, internally tagged on `kind`. SCREAMING_SNAKE_CASE.
- **`DAMAGE { amount, band_falloff: Option<bool> }`** — the predicate semantics are
  load-bearing. See the dedicated section below.
- **`DisplaceMode`** — `Push`, `Pull`, `Swap`; lowercase wire.
- **`ReorientTo`** — `BowOn`, `Broadside`, `Flip`, **`RotateLeft`, `RotateRight`**; camelCase.
  `BowOn`/`Broadside`/`Flip` are the TS-parity orientation reorients. `RotateLeft`/`RotateRight`
  are a **Rust-port extension** (#75, additive — never authored in catalog JSON, so the TS
  contract is unchanged, like the `direction` field on `DISPLACE_SELF`): they turn the ship's
  `facing` (`Dir4` bow direction) a quarter-turn ccw/cw and re-derive `orientation` from it. They
  are produced only by the synthetic player rotate actions (`input::synthetic_rotate_*`). Because
  the loft render and the 2-D fire-gate both key off `facing`, the hull visibly rotates and the
  firing arcs follow — see [`resolve.md`](resolve.md)'s REORIENT-rotate arm and the cross-module
  hook in `LINE_BY_LINE.md`.
- **`DeployHazardKind`** — `Mine`, `Drone`. (No debris.)
- **`MovementMode`** — `THRUST`, `BURN`, `SLIP`, `JUMP`, `TRACTOR_SWAP`; SCREAMING_SNAKE_CASE.

### 6. Ordnance entity (lines 502–522)

- **`Projectile`** — `{ id, kind, cell, heading, speed: u32, hull, payload, owner_faction }`.
  The resolver only impacts on cells whose ship has `faction != owner_faction`.

### 7. Subsystems / event bus (lines 524–724)

The richest section.

- **`SubsystemDef`** — catalog half; no callback. `unlock_salvage: Option<i32>` with
  `#[serde(rename = "unlockSalvage", default)]` **but no `skip_serializing_if`** so
  `None` round-trips as JSON `null`. See **Drift note: `unlock_salvage`** below.
- **`SubsystemBay`** — 6 categories, camelCase.
- **`Hook`** — 11 variants. **Declaration order is load-bearing** for `EventBus::slot`.
- **`HookContext<'b>`** — `{ board: &'b mut Board, source_cell: Option<usize>,
  target_cell: Option<usize>, amount: Option<i32>, extras: HashMap<String,
  serde_json::Value> }`. **Cell indices, not raw pointers** (H2 audit fix).
- **`EventBus`** — `[Vec<Option<Box<dyn FnMut(&mut HookContext)>>>; 11]`. Take/replace
  re-entrancy via `Option`. `Default` impl exists (load-bearing for `mem::take`).

### 8. Catalog (lines 727–803)

- **`Catalog`** — top-level JSON payload. The remaining `unknown[]` placeholders map to
  `Vec<serde_json::Value>` with `#[serde(default)]`; **`sectors` is now typed
  `Vec<SectorDef>`** (#149 — see §9 below), no longer a loose placeholder.
- **`CatalogMeta`** — schema, lane sizes, new-axes tracker, declared bands.
- **`ModDef`, `StatusDef`, `PatrolDef`, `EnemyDef`** — straight ports of the TS
  catalog sub-records. (`ModDef` is the *catalog record* for a weapon mod; the
  *runtime behavior* of the seven recognised mods — `twin_linked` / `autoloader` /
  `flak_burst` / `incendiary` / `emp_charge` / `targeting_laser` / `precision_core` —
  is dispatched off `Action::r#mod` in the resolver: see the weapon-mod section of
  [`resolve.md`](resolve.md).)

### 9. Run-loop / campaign types

- **`Run`, `Sector`, `EncounterDef`, `ShipSpawn`, `BoardSnapshot`, `SaveState`** — the
  Phase-3 run-loop shapes (the *runtime* campaign vocabulary). Documented in depth in
  [`runs.md`](runs.md) / [`save.md`](save.md); they're the materialized-board side.
- **`SectorDef`** (#56) — the **catalog** shape of one campaign sector, deliberately
  distinct from the runtime `Sector`. Per the canonical dynamic-spawn-pool model
  (§XI / §VIII), a sector does **not** carry a static encounter list: `name`, `node`
  (a *string* graph id like `"4.2"` encoding the branching map — branch siblings share
  a major number), `lane` (board size 5/7/9), `intro` (display names of enemy types
  **first introduced** here — they seed the global spawn pool on arrival, NOT a
  per-encounter list), and `capital` (the end-of-sector boss display name; the catalog's
  `"—"`/`""` "no capital" sentinels deserialize to `None` via `deserialize_capital`).
  The pool→encounter generator that turns `SectorDef` into runtime `Sector`s is
  [`runs::generate_campaign`](runs.md) (#60). `Catalog::sectors` is now typed
  `Vec<SectorDef>` (#149 — it was a `Vec<serde_json::Value>` placeholder).
- **`CapitalDef`** (#63) — the **catalog** shape of one boss capital ship (the
  end-of-sector engagement [`SectorDef::capital`](runs.md) names; one per sector). Six
  fields, the whole canonical spec: `id`, `name` (e.g. `"The Dasher"`), `sector` (the
  `SectorDef::name` it ends), `corrupt` (a Patrol-4+ corrupted-variant **eligibility
  flag** — the variant's stats are content's), and the **salvage reward** pair
  `salvage_p1: Option<i32>` / `salvage_p7: i32`. **Salvage_p1/p7 are the salvage PAYOUT
  for destroying the capital at Patrol tier 1 vs 7 (rewards that scale with tier — NOT
  combat stats/hull).** `salvage_p1` is `Option` because the catalog stores `null` for
  the one capital not awarded at tier 1 (the Void Sovereign). **Serde keys stay `sP1` /
  `sP7`** (`#[serde(rename)]`), both `#[serde(default)]` so minimal entries parse. The
  doc authors **no per-capital combat loadout** here — per-capital combat distinctiveness
  (the Twins spawning two ships, the Coward fleeing) is content's future runtime-synthesis
  follow-up, decoupled from this type. `Catalog::capitals` is now typed `Vec<CapitalDef>`.
  The tier→salvage interpolation that consumes it is
  [`meta::capital_salvage_for_tier`](meta.md).

---

## The `DAMAGE.band_falloff` predicate gotcha

This is the single trickiest semantic in the entire file. Spelled out so the resolver
port can't drift.

The TS shape is `bandFalloff?: boolean` (`types.ts:137`). The resolver predicate
(`resolve.ts:143`) is:

```ts
weapon.effects.some((e) => e.kind === "DAMAGE" && e.bandFalloff === false)
```

**Strict-equal-to-false.** Three cases:

| `band_falloff` value     | Behaviour              |
|--------------------------|------------------------|
| `None` (field absent)    | Apply falloff          |
| `Some(true)`             | Apply falloff          |
| `Some(false)`            | **Bypass** falloff     |

The idiomatic Rust check is:

```rust
let bypass = action.effects.iter().any(|e|
    matches!(e, Effect::DAMAGE { band_falloff: Some(false), .. })
);
```

Pinned by the `damage_band_falloff_predicate_semantics` test at `types.rs:958`.

**Two gotchas:**

1. **The predicate is at action level, not effect level.** ONE damage effect with
   `bandFalloff: false` on an action disables falloff for the *whole* `applyDamage`
   call. A per-effect implementation would be a subtle drift.
2. **`!band_falloff.unwrap_or(true)` happens to be correct but reads backwards.**
   Prefer `matches!(..., Some(false), ..)`.

---

## The `EventBus` re-entrancy story

The bus has to survive callbacks that fire more events. Three cases:

1. **Same-hook re-emit.** A subscriber for `OnLethal` calls `bus.emit(OnLethal, ...)`.
   The implementation takes the currently-executing callback's slot out via `.take()`,
   invokes other subscribers (whose slots are still `Some`), and puts it back.
   Currently-executing slot reads `None` and is skipped — the callback does not
   re-invoke itself. This fixes the ReactorBreach/Voidtouched chain where a nested
   `destroy` would otherwise silently lose the second `onLethal`.

2. **Same-hook re-register.** A subscriber calls `bus.on(OnTurnEnd, ...)`. The new
   subscriber lands at the end of the slot's vec. The outer `emit` loop re-reads
   `.len()` each iteration, so newly-pushed subscribers fire in the same pass —
   matching the TS `forEach` semantics for in-place push.

3. **Cross-hook emit.** Unaffected — only the live hook's slot participates in the
   take/replace dance.

**Caveat at the resolver layer:** the resolver pattern (future `resolve::emit`) uses
`std::mem::take(&mut board.bus)` to lift the entire bus off the board for the duration
of the emit, so a callback that tries `ctx.board.bus.emit(...)` finds an empty bus
and silently no-ops. The ReactorBreach/Voidtouched chain is blocked by *that* layer,
not by `EventBus`. Pending a team design call (likely `RefCell<EventBus>` on
`Board`); see the `NOTE on N1` comment block at `types.rs:1053`.

**Worked example:** `emit_fires_subscribers_in_registration_order` at `types.rs:1002`
walks the canonical pattern: build a minimal `Board`, `mem::take` the bus,
register two subscribers, emit once, assert ordering. Read this test to understand
the `mem::take` pattern the resolver will use.

---

## Drift from TypeScript

Six watch-list items from before the port have been **resolved**:

1. **`klass`** kept as `klass` — cross-port grep parity over Rust-idiomatic
   `class`/`ship_class`.
2. **Subsystem callback split** into `SubsystemDef` (catalog) + future `Subsystem`
   (runtime, content slice owns it).
3. **`Record<HullZone, ShieldFace>`** became `ShieldProfile` struct (M1 audit fix) —
   forces total deserialization, rejecting partial JSON at parse time.
4. **`bus: EventBus` on `Board`** kept embedded; resolver uses `mem::take` to satisfy
   borrow checking during emit. `EventBus: Default` is the load-bearing impl.
5. **`Effect` discriminated union** ported as `#[serde(tag = "kind")]` tagged enum
   with `SCREAMING_SNAKE_CASE` variants.
6. **`Catalog`'s `unknown[]`** placeholders mapped to `Vec<serde_json::Value>` with
   `#[serde(default)]`. Tightenable later.

**New decisions documented during this pass** (not from the pre-port watch list):

- `Board.destroys_this_window: usize` — new field for chain-kill counting; reset
  semantics owned by the resolver.
- SCREAMING_SNAKE_CASE universalised across `TargetingPattern`, `Effect`,
  `MovementMode`. Grep-parity argument.
- `r#mod` raw identifier on `Action.mod` and `PatrolDef.mod`; JSON wire shape is
  `"mod"`.
- `HookContext` carries `source_cell` / `target_cell` as `Option<usize>` (H2 audit
  fix), not raw pointers.
- `EventBus` take/replace re-entrancy semantics formalised in the impl + tests.
- `EventBus` is `!Send + !Sync` because of boxed `FnMut`. Future watch item for the
  renderer.
- `Status.face` flagged as dead weight pending content/resolver confirmation.

**Open architectural item:** bus borrowing during emit (see re-entrancy story above).
Pending team design call.

---

## Tests

12 tests in `#[cfg(test)] mod tests` at `types.rs:809–1063`. Each pins down a
serde-parity contract or a port-decision invariant. Test names are documentation:

```
orientation_roundtrips_through_ts_shape
effect_damage_roundtrips_with_optional_band_falloff
effect_displace_self_parses_movement_mode
range_band_serializes_camel_case
targeting_pattern_preserves_screaming_snake
ship_roundtrips_with_pulse_laser_demo_shape
shield_profile_rejects_missing_zone
shield_profile_index_mut_decrements_charge
subsystem_def_unlock_salvage_null_roundtrips
damage_band_falloff_predicate_semantics
hook_count_matches_enum_cardinality
emit_fires_subscribers_in_registration_order
```

The `NOTE on N1` block at line 1053 — not a test, but documentation — formalises the
open bus-borrowing question for future readers.

---

## Cross-references

- **Geometry consumers:** every type in section 1 plus `ShieldFace` is consumed by
  [`geometry.rs`](geometry.md).
- **Resolver consumers:** every type in this file is consumed by the future
  [`resolve.rs`](resolve.md). See the `Action`, `Effect`, `Board`, and `Ship`
  walkthroughs.
- **Domain terms:** every concept here is in the [glossary](../GLOSSARY.md).
- **Design intent:** Part XIII of the
  [analysis document](../../_drive_pull/broadside-analysis.html) (the codeblock that
  produced these types).
