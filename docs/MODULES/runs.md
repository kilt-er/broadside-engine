# `src/runs.rs` — Phase 3 run-loop logic

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/runs.rs`](../LINE_BY_LINE.md#srcrunsrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

The architect's #75 foundation supplies the *types* for a campaign —
[`Sector`](types.md), [`EncounterDef`](types.md), [`Run`](types.md),
[`ShipSpawn`](types.md). This module is the **runtime layer** that turns those
inert structs into a working campaign: it reads a live [`Board`](types.md) to
decide if an encounter is won/lost, mutates a `Run` to advance through the
sector map, and materializes a fresh `Board` for each new encounter from a
spawn list plus the player's carried-over ship.

Think of it as the campaign state machine. The demo bin's loop is roughly:
resolve a round → ask `encounter_outcome` → on `Won` call `advance_after_win`
and build the next board → on `Lost` call `mark_defeated` and show the defeat
screen. There is no TS analog — `demo.ts` is a single hand-built board with no
campaign layer. Phase-3-only.

> **Gotcha (run-loop ↔ resolver seam).** The round the bin resolves between
> `encounter_outcome` checks runs `execute_queue`, which fires queued action ids
> via `Content::action` lookup and **does not gate on the firing ship owning a
> matching `Mount`** — an unarmed ship still fires a queued weapon. This only
> bites the direct queue-injection path (player input or a test fixture pushing
> onto `ship.queue`); the AI gates on mounts/arc/band. Full detail in the
> [`execute_queue` walkthrough](../LINE_BY_LINE.md#srcresolvers). Relevant here
> because a `run_loop` fixture that hand-builds a ship + queue can see a "weapon
> fired with no mount" result that looks wrong but isn't.

### Design decisions baked into the module

- **Placeholder sectors live here, not on `DemoContent`** (src/runs.rs:30-39).
  Subsystems/cards live on `DemoContent` because the resolver queries them every
  frame; sectors are consulted *once per encounter transition*, so there's no
  perf reason to bake them in. A stand-alone `placeholder_sectors()` keeps the
  eventual switch to `Catalog::sectors` mechanical — the rest of the code only
  ever sees `&[Sector]`.
- **Spawns reference a `class_id`, not an embedded `Ship`** (src/runs.rs:41-49).
  One ClassDef/EnemyDef defines the loadout; the encounter just says "spawn this
  class at this cell." A builder closure materializes the `Ship`, so the same
  encounter code works with placeholder data and real catalog data.

---

## Encounter outcome

### `enum EncounterOutcome` (src/runs.rs:63)

`InProgress` / `Lost` / `Won` — the result of inspecting a board after a round.

### `fn encounter_outcome(board: &Board) -> EncounterOutcome` (src/runs.rs:80)

**Intent:** Scan the board once and classify it. Line 81-90: walk `board.cells`,
flag whether any player and any enemy ship survives. Line 91-97: no player →
`Lost` (player loss takes precedence — a board with neither faction returns
`Lost` as the more honest signal); no enemy → `Won`; otherwise `InProgress`.
Cheap — one pass, no allocation.

**Cross-references:** Called by the bin after each `resolve_round`. Its return
gates `advance_after_win` (on `Won`) vs `mark_defeated` (on `Lost`).

**Worked examples** (src/runs.rs:659-687): both factions present → `InProgress`;
no enemies → `Won`; no player → `Lost`; empty board → `Lost` (precedence).

---

## Run advancement

### `enum AdvanceResult` (src/runs.rs:106)

The four mutually-exclusive outcomes of a won encounter: `NextEncounter` (more
in this sector), `NextSector` (sector cleared, another exists),
`Victorious` (final encounter of final sector cleared), `AlreadyEnded`
(inconsistent state — e.g. advancing an already-won run; a no-op signal). The bin
branches on this to choose between the between-encounter card-pick UI, the
end-of-sector screen, and the final-victory overlay.

### `fn advance_after_win(run: &mut Run, sectors: &[Sector]) -> AdvanceResult` (src/runs.rs:130)

**Intent:** Advance the run after a confirmed win. Mutates
`completed_encounters` and possibly `current_sector_idx` / `victorious`.
**Pre-condition:** the caller has confirmed the win via `encounter_outcome`; this
function does not look at the board.

Line 131-133: guard — an already-ended run returns `AlreadyEnded`. Line 135-141:
out-of-bounds sector index → declare victory defensively. Line 144-150: if a next
encounter exists in this sector, increment and return `NextEncounter`. Line
153-158: else if a next sector exists, advance the sector index, reset
`completed_encounters` to 0, return `NextSector`. Line 162-163: else the final
sector is cleared — set `victorious` and return `Victorious`.

**Cross-references:** Called by the bin on `EncounterOutcome::Won`. Reads
`Sector::encounters` lengths; mutates [`Run`](types.md).

**Worked examples** (src/runs.rs:695-746): within-sector → `NextEncounter`
(`completed_encounters == 1`); last encounter of sector → `NextSector` (sector idx
1, encounters reset 0); last encounter of last sector → `Victorious`
(`run.victorious`, not `defeated`); already victorious / already defeated →
`AlreadyEnded`.

### `fn mark_defeated(run: &mut Run)` (src/runs.rs:169)

Flips `run.defeated = true`. Idempotent. Called on `EncounterOutcome::Lost`.

### `fn current_encounter<'s>(run, sectors) -> Option<&'s EncounterDef>` (src/runs.rs:176)

**Intent:** Look up the encounter the run currently points at. `None` if the run
has ended (defeated/victorious) or the indices are out of bounds — the bin shows
the end-of-run overlay in that case. Otherwise indexes
`sectors[current_sector_idx].encounters[completed_encounters]`.

**Worked examples** (src/runs.rs:757-781): ended run → `None`; fresh run →
`"drift_belt_a"`; after one win → `"drift_belt_b"`.

---

## Encounter → Board materialization

### `fn build_encounter_board<F>(encounter, player, class_to_ship) -> Board` (src/runs.rs:206)

**Intent:** Instantiate a fresh `Board` for an encounter. The player's
**current** ship (carrying hull/heat/cooldown/status from the prior encounter) is
placed at cell 0 with its `cell` field normalized to 0 — "you start a new
encounter at the lane mouth" regardless of where you ended the last one. Enemy
spawns populate the rest via the `class_to_ship` builder closure; hazards drop
into their cells.

Line 216-223: lane size — take the max of all spawn cells and the player cell,
then round up to a canonical 5/7/9 via `canonical_lane_size`. Line 225-226:
allocate empty cell and hazard vectors. Line 229-230: normalize the player to
cell 0 and place. Line 233-252: place each enemy spawn, **skipping** any spawn
off-board or at cell 0 (player collision) or onto an occupied cell; the builder
closure produces the `Ship`, then `orientation` and `hp_override` are applied.
Line 255-259: drop hazards. Line 261-269: assemble the `Board` with a fresh
`EventBus::default()`, patrol 1, and `destroys_this_window: 0`.

The closure parameter is the key flexibility: the bin passes a catalog-aware
builder; tests pass `|spawn| Some(fallback_ship_for_spawn(spawn))`. The same
board builder works with placeholder and real data.

> **Gotcha — the spawn's orientation wins, not the closure's.** Line 245
> (`ship.orientation = spawn.orientation;`) **unconditionally overwrites** the
> orientation the builder closure set on the ship. A ship's facing on the board
> is therefore determined by the [`ShipSpawn`](types.md), *not* by
> `boss_ship_for_spawn` / `fallback_ship_for_spawn` / any custom builder — even
> though those builders set an `orientation` field, it's discarded here. This is
> counterintuitive when constructing boards: if a fixture or caller wants a
> ship bow-on aft, it must set that on the **spawn**, not in the closure. (The
> same line also applies `hp_override` from the spawn, with the same
> "spawn-data-wins" intent: the encounter author controls facing and hull, the
> builder controls the rest of the loadout.)

**Cross-references:** Called by the bin on each encounter transition (and on
resume, rebuilding around `run.player.clone()`). Uses `canonical_lane_size`;
takes a closure typically wrapping `boss_ship_for_spawn` / `fallback_ship_for_spawn`.

**Worked examples** (src/runs.rs:847-920): player lands at cell 0 with hull
preserved; enemies appear at their cells with `orientation`/`hp_override`
applied; a spawn at cell 0 is dropped to protect the player.

### `fn canonical_lane_size(max_cell: usize) -> usize` (src/runs.rs:275)

Maps the highest occupied cell to the smallest canonical lane that fits:
`0..=4 → 5`, `5..=6 → 7`, `_ → 9` (the analysis doc's early/mid/late lane
lengths). Pinned by `build_board_uses_canonical_lane_size` (src/runs.rs:978).

---

## Spawn → Ship builders

### `fn boss_ship_for_spawn(spawn: &ShipSpawn) -> Ship` (src/runs.rs:315)

**Intent:** Build the Citadel Warlord final boss (task #83) — a ship that *feels*
like a boss. The bin's spawn callback dispatches here when it sees
`class_id == "warlord"`; everything else falls through to `fallback_ship_for_spawn`.

Tuning, all documented in the source: hull 14 (double the regular-enemy cap, so
the fight reads as different); `heat_max: 8` (sustain fire); a tougher bow shield
(armour 3, so the frontal approach is a real fight while the stern stays the soft
flank the player is rewarded for reaching); three mounts (forward `pulse_laser` +
forward `beam_cannon` + broadside `missile_salvo`, so the AI's telegraph queue
surfaces serious threats a turn ahead and punishes the player for sitting in the
flank arc); and `Trait::ReactorBreach` (the resolver's `destroy()` splashes
neighbors on its death — killing it matters at point-blank). Line 350-353:
`hp_override` still wins if set, so the encounter can tier-scale.

**Worked examples:** `boss_ship_for_spawn_has_climactic_loadout` (src/runs.rs:922)
pins hull ≥14, `ReactorBreach`, ≥3 mounts, bow armour ≥3;
`boss_ship_for_spawn_honors_hp_override` (src/runs.rs:962) confirms override 20
wins over the 14 default.

### `fn fallback_ship_for_spawn(spawn: &ShipSpawn) -> Ship` (src/runs.rs:362)

**Intent:** The "any enemy" default — bow-on facing the player, hull 3, one
forward `pulse_laser` mount so the AI has something to fire. Used for any
`class_id` the caller's class registry doesn't recognize; real class-specific
stats come from the bin's table. `hp_override` applies if set. (Line 396's
`let _ = HullZone::Bow;` is a deliberate import-keepalive for a future
per-class-shield variant.)

---

## Placeholder sectors

### `fn placeholder_sectors() -> Vec<Sector>` (src/runs.rs:413)

**Intent:** The three demo sectors, returned as a plain `Vec` (not on
`DemoContent`, per the module rationale). Progressive difficulty:

- **Drift Belt** (patrol 1, `sector_drift_belt`, src/runs.rs:441) — two weak
  encounters; "feel the controls."
- **Ion Reefs** (patrol 2, `sector_ion_reefs`, src/runs.rs:469) — three
  encounters, trait variety.
- **Citadel Approach** (patrol 3, `sector_citadel_approach`, src/runs.rs:507) —
  two encounters plus the boss. The final encounter (`citadel_boss`,
  src/runs.rs:541) is `is_boss: true`: the Warlord at mid-board flanked by two
  `voidrunner` escorts. The intended play is clear the escorts (they're `Agile`)
  then maneuver to the warlord's stern, since its bow is hard to break. `is_boss`
  is the flag `AdvanceResult::Victorious` reads.

`spawn` (src/runs.rs:421) and `enc` (src/runs.rs:430) are terse local
constructors for `ShipSpawn` / `EncounterDef`.

**Worked examples** (src/runs.rs:785-843): three sectors with ascending patrol
tiers; every sector has ≥1 encounter; exactly one boss, at the very end; enemy
density loosely increases across sectors.

---

## Spawn-pool encounter generator (#60 — the data-driven campaign)

*This is the runtime generator that **replaces** `placeholder_sectors` with sectors
generated from the canonical [`SectorDef`](types.md) catalog data (the bin uses it when
a catalog is loaded, falling back to the placeholders otherwise). It implements the
design doc's **dynamic-spawn-pool** campaign model (analysis HTML §XI 788-796, §VIII
699).*

The model (module banner, src/runs.rs:588-612):
- Each sector's `intro[]` lists the enemy ship **types first introduced** there. They
  **enter a global run pool on arrival** and persist for the rest of the run ("seen
  once → can appear in any later sector").
- Encounters are **not authored per-sector** — they're **sampled** from the
  accumulated pool, scaled by the sector `lane` (board size) + the run's patrol tier.
- Each sector **ends in its `capital` boss** engagement (no waves, just the boss).

**Determinism (#111):** generation is a pure function of `(node, patrol_tier)` via a
local `wang_hash` PRNG (src/runs.rs:622) — no global RNG, so a run-state always
regenerates the identical sector. The generator owns no I/O; the bin feeds it the
loaded `Catalog`.

### `struct SpawnPool` + `fn accumulate` (src/runs.rs:637, 648)

The run's accumulated pool: enemy `class_ids` in first-seen order (stable → deterministic).
**Derived from the route, not a mutable run field** — the pool at sector N is the union
of `intro[]` over sectors `0..=N`, mapped from catalog display names to enemy ids
(`resolve_enemy_id`, src/runs.rs:682 — snake_case ids pass through, display names look
up via the catalog `enemies[]`). Unknown intro names are skipped + logged (no dangling
ids). `spawn_pool_accumulates_intro_along_the_route` (src/runs.rs:1301) pins the union.

### `fn generate_sector(sector_def, pool, patrol_tier, catalog) -> Sector` (src/runs.rs:714)

**Intent:** Materialize one runtime `Sector` from a `SectorDef`. Seeds from the node
string + patrol tier. Emits `ENCOUNTERS_PER_SECTOR` (src/runs.rs:617, **=2**, a doc-silent
balance knob flagged for bruce) pool-sampled non-boss encounters, then the capital boss
encounter (if the sector has a `capital`). An **empty pool** (e.g. the run-start Staging
sector introduces nothing and nothing prior seeded it) → only the boss (if any); a sector
with neither is a passthrough (empty `encounters`, which `encounter_outcome` treats as
already-won). Helpers: `encounter_enemy_count` (src/runs.rs:695, lane→count 5→2/7→3/9→4,
another balance knob), `sample_encounter_spawns` (src/runs.rs:778, deterministic
pool-pick at distinct cells packed from the far edge, bow facing Aft toward the player),
`capital_spawn` (src/runs.rs:819, confirms the capital exists in the loose `capitals[]`,
spawns a boss-class ship at mid-lane carrying the capital's name as `class_id` — the
bin's spawn callback routes capitals to `boss_ship_for_spawn` until a typed `CapitalDef`
lands; an unknown capital yields a boss-less sector, not a crash).

**Worked examples:** `generate_sector_produces_encounters_then_boss` (src/runs.rs:1319),
`generate_sector_is_deterministic` (src/runs.rs:1352),
`staging_sector_has_no_encounters_and_no_boss` (src/runs.rs:1365),
`unknown_capital_yields_bossless_sector_not_a_crash` (src/runs.rs:1388).

### `fn generate_campaign(catalog, patrol_tier) -> Vec<Sector>` (src/runs.rs:851)

The full campaign: one runtime `Sector` per catalog `SectorDef`, accumulating the pool
along the route (sector N sees intro from `0..=N`, so earlier sectors field smaller pools
— the "ships unlock as you progress" model). The data-driven replacement for
`placeholder_sectors`. `generate_campaign_covers_every_catalog_sector` (src/runs.rs:1377).

**Cross-references:** consumes [`SectorDef`](types.md) from `Catalog::sectors` (now typed
— #149); produces the same runtime `Sector`/`EncounterDef`/`ShipSpawn` shapes the rest of
this module already uses, so `build_encounter_board` / `advance_after_win` /
`encounter_outcome` are unchanged. Capitals route through
[`boss_ship_for_spawn`](#fn-boss_ship_for_spawnspawn-shipspawn---ship-srcrunsrs315) via
the bin's spawn callback.

---

## Drift / notes

- **`placeholder_sectors` is now the FALLBACK, not the default.** Since #60,
  `generate_campaign` is the real path (catalog-driven); `placeholder_sectors` is the
  no-catalog fallback and is on track to retire once the generator is the only campaign
  source. `Catalog::sectors` is now typed `Vec<SectorDef>` (#149, was
  `Vec<serde_json::Value>`).
- **Balance knobs flagged for bruce:** `ENCOUNTERS_PER_SECTOR` (2), `encounter_enemy_count`
  (lane→2/3/4), and uniform pool sampling are doc-silent playtest dials, not pinned values.
- **Capitals share one synthesizer for now:** all capitals materialize via
  `boss_ship_for_spawn` (hull 14, ReactorBreach) until a typed `CapitalDef` gives per-boss
  stats — a future content+architect follow-up.
- **`ShipSpawn::class_id` rename pending.** types.rs notes a deferred
  `class_id → template_id` rename (it refers to either a `ClassDef::id` or an
  `EnemyDef::id`); when it lands, this module's spawn helpers update with it.
