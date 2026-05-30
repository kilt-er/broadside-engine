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

*Pure functions over the lane. No randomness, no content, no I/O. Everything that makes
orientation, arcs, and range bands a real decision lives here. The Rust port of this
file is small and translates almost verbatim from TS.*

**Mirrors:** `engine/geometry.ts`.
**Design anchor:** HTML Part III (Targeting, Arcs & Range Bands) and Part IV
(Orientation & Movement).

### Functions to document

- **`fn opposite(end: LaneEnd) -> LaneEnd`** — flip a lane direction.
  *Mirrors `geometry.ts:11`.*

- **`fn direction_to(a: usize, b: usize) -> LaneEnd`** — which way to walk from `a` to
  reach `b`. *Mirrors `geometry.ts:16`.*
  *(Possible drift: TS uses `number`; Rust may use `i32` for safe subtraction or
  `usize` with explicit comparison. Decide at port time.)*

- **`fn distance(a: usize, b: usize) -> usize`** — cell distance, absolute.
  *Mirrors `geometry.ts:21`.*

- **`fn range_band(atk: usize, tgt: usize) -> RangeBand`** — bucket distance into one of
  the five bands. *Mirrors `geometry.ts:30`.*

- **`fn band_falloff(raw: i32, actual: RangeBand, optimal: RangeBand) -> i32`** —
  reduce damage by band distance using the falloff table. *Mirrors `geometry.ts:41`.*
  *Embed the worked example from `demo.ts` Round 1 here: Pulse Laser optimal=close, fire
  at point-blank, delta=1, factor=0.66, 4 → 2.*

- **`fn in_band(allowed: &[RangeBand], atk: usize, tgt: usize) -> bool`** —
  convenience predicate. *Mirrors `geometry.ts:48`.*

- **`fn facing_zone(o: Orientation, incoming_from: LaneEnd) -> HullZone`** — the rotation
  principle made concrete. *Mirrors `geometry.ts:61`.*
  *The sample line-by-line entry already drafted (see writer's earlier proposal) drops
  in here once the Rust is on disk.*

- **`fn arc_bears(o: Orientation, arc: Arc, toward: LaneEnd) -> bool`** — does a mount
  with `arc` aim that way given the ship's stance? *Mirrors `geometry.ts:74`.*

- **`fn bears(ship: &Ship, arc: Option<Arc>, target_cell: usize) -> bool`** —
  convenience over `arc_bears`. *Mirrors `geometry.ts:90`.*

- **`fn absorb_shield(face: &mut ShieldFace, dmg: i32) -> i32`** — run damage through
  one zone's defense; charge negates and is consumed, else armour subtracts.
  *Mirrors `geometry.ts:101`.*
  *Watch for drift: TS mutates the face; Rust signature must take `&mut`.*

- **`fn default_shield_profile() -> [ShieldFace; 4]`** *(or `HashMap`)* — the starting
  Frigate's shield table. *Mirrors `geometry.ts:112`.*

### Drift watch list

- **`Math.floor(raw * factor)`** — TS does float math then floors. Rust port should
  decide: fixed-point integer table (e.g. percentages), or `(raw as f32 * factor) as i32`.
  The factor table `[1, 0.66, 0.5, 0.33, 0.2]` should probably become
  `[100, 66, 50, 33, 20]` and divide-by-100 at the end to keep determinism across
  platforms.
- **Mutation of `ShieldFace.charge`** — TS mutates via reference. Rust needs `&mut
  ShieldFace`; ensure call sites at `apply_damage` can supply it without borrow conflict.

*Per-line walkthroughs pending `src/geometry.rs`.*

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
