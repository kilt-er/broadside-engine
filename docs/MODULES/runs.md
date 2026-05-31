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

## Drift / notes

- **JSON sectors are placeholder.** `placeholder_sectors()` is Rust-literal data
  standing in until `Catalog::sectors` (currently `Vec<serde_json::Value>`) is
  typed. The module is structured so swapping the data source is mechanical.
- **`ShipSpawn::class_id` rename pending.** types.rs notes a deferred
  `class_id → template_id` rename (it refers to either a `ClassDef::id` or an
  `EnemyDef::id`); when it lands, this module's spawn helpers update with it.
