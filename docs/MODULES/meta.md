# `src/meta.rs` — cross-run meta-progression

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/meta.rs`](../LINE_BY_LINE.md#srcmetars) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

Broadside is a roguelike: you die, you start over, but **something** persists to
make the next run start a little stronger. That persistent layer is this module.
It owns three responsibilities:

1. **The `MetaProgression` struct** — the cross-run state that survives death:
   the set of unlocked subsystems/cards and a cumulative `total_salvage_earned`
   counter that never resets.
2. **Salvage math** — how many salvage credits a destroyed enemy is worth, and
   how to total a won encounter (with a boss multiplier).
3. **Unlock threshold logic** — a salvage-gated ladder: cross a total-salvage
   threshold and a new subsystem unlocks for future runs.

The contrast with [per-run state](save.md): the in-flight [`Run`](types.md)
holds the salvage you collect *this run*, which resets to 0 each fresh run.
`MetaProgression` accumulates across **every** run, won or lost — the design
rewards engagement, not just win-rate. On run end, the bin calls
`accumulate_into_meta` to roll the run's salvage into the persistent total and
apply any unlocks the new total crosses.

This module defines *structure + persistence + threshold logic only*. The
between-encounter "spend salvage to install a subsystem" UI is renderer's #77
screen layered on top; this is the data underneath it. No TS analog —
Phase-3-only.

### Unlock thresholds (the ladder)

The starter set (Marksman / Point-Blank Doctrine / Heat Sink) is always
available — baked into [`src/subsystems.rs`](../LINE_BY_LINE.md#srcsubsystemsrs).
The four meta-unlockable subsystems ladder by total salvage:

| Subsystem | Threshold | Effect (canonical catalog) |
|---|---|---|
| `rear_gunner` | 10 | +1 dmg through stern arc |
| `chain_bounty` | 25 | +1 credit on chain kill |
| `overcharge` | 50 | +1 dmg when queue has only one action |
| `crossfire` | 100 | +1 enemy-vs-enemy damage |

Thresholds are tuned so a typical run (~10-20 salvage on win, ~5 on defeat)
unlocks tier 1 in one run, tier 2 in two-or-three, and the top tier over roughly
a ten-run arc. Explicitly tunable.

---

## `struct MetaProgression` (src/meta.rs:72)

**Intent:** The whole persistent state in three fields, all
`#[serde(default)]` so an old save missing a field still loads.

Line 75: `unlocked_subsystems: Vec<String>` — subsystem ids unlocked beyond the
starter set. Line 80: `unlocked_cards: Vec<String>` — reserved for future card
unlocks; empty in the Phase 3 data layer. Line 84: `total_salvage_earned: u32` —
cumulative across every run; drives thresholds; never decremented. The
`#[derive(Default)]` gives a fresh first-run player the all-zero state.

**Cross-references:** Persisted via the impl methods below; consumed by the
between-encounter UI's "available subsystems" query.

---

## `enum MetaError` (src/meta.rs:91)

**Intent:** `Io` / `Parse`, mirroring [`catalog::LoadError`](catalog.md)'s shape
so callers can `?` either uniformly. Line 90: `#[non_exhaustive]`. Line 96-119:
`Display`, `Error::source`, and `From` impls for both `io::Error` and
`serde_json::Error` (no Encode/Decode split here — meta saves are simpler than
run saves, and the symmetric `From<serde_json::Error>` is unambiguous).

---

## `impl MetaProgression` — persistence + queries (src/meta.rs:121)

### `fn load_from_disk(path) -> Result<Self, MetaError>` (src/meta.rs:125)

**Intent:** Read meta state. A **missing file returns `Ok(default)`**, not an
error — first-run players have no save, and that's the normal case, not a
failure. Line 127-129: missing → default. Line 130-131: read + parse.

### `fn save_to_disk(&self, path) -> Result<(), MetaError>` (src/meta.rs:136)

**Intent:** Write meta state, creating the parent directory if missing. Line
138-142: parent bootstrap. Line 143-144: `to_vec_pretty` + `fs::write`. Note this
is a **plain write**, not the atomic tmp+rename that `Run::save_to_disk` uses —
meta saves are smaller and less frequent, and the docstring deems the extra
safety unnecessary here (a torn meta write at worst loses one run's unlock
rollover, recoverable next run).

### `fn has_subsystem(&self, id) -> bool` (src/meta.rs:150)

**Intent:** One check that covers both starter and unlocked subsystems. Line 151:
`STARTER_SUBSYSTEMS.contains(&id) || self.unlocked_subsystems.iter().any(...)` —
the starter set is always "owned," so callers don't special-case it.

### `fn available_subsystems(&self) -> HashSet<String>` (src/meta.rs:159)

**Intent:** The full available pool (starters + unlocks) as a `HashSet` for cheap
membership tests — the future purchase UI's "show available" query.

**Cross-references:** `has_subsystem` / `available_subsystems` read
`STARTER_SUBSYSTEMS`. The bin calls `load_from_disk` at startup, `save_to_disk`
after `accumulate_into_meta` on run end.

---

## `const STARTER_SUBSYSTEMS` (src/meta.rs:169)

The three always-available ids, pulled from `crate::subsystems::{MARKSMAN,
POINT_BLANK_DOCTRINE, HEAT_SINK}` constants so there's no string drift between
the two modules. Adding a starter goes here **and** in `subsystems.rs`.

---

## Salvage math

### `fn salvage_for_destroyed(ship: &Ship) -> u32` (src/meta.rs:195)

**Intent:** Salvage for one kill, weighted by the ship's `max_hull` (the analysis
HTML's proxy for "how big a deal was this kill"). Line 196-200: `≤3 → 1`,
`≤6 → 2`, `7+ → 3`. The lead's brief ("1-3 per enemy, weighted by hull") maps
exactly here; this function is the single place callers consult.

**Worked examples** (src/meta.rs:367-385): hull 1/2/3 → 1; hull 4/5/6 → 2;
hull 7/12 → 3.

### `fn salvage_for_encounter_win<F>(encounter, class_to_ship) -> u32` (src/meta.rs:220)

**Intent:** Total salvage for a won encounter — sum `salvage_for_destroyed` over
every spawn in the encounter, then `×2` if `is_boss`.

It reads the encounter's **`enemy_ships` spawn list, not the live board**: by the
time an encounter is `Won` the board cells are empty (everyone's dead), so the
spawn list is what captures who died. Line 227-240: `filter_map` each spawn
through the `class_to_ship` builder closure (the same one the bin passes to
[`build_encounter_board`](runs.md)) to recover a `Ship` whose `max_hull` we can
read; line 235-237 honours `hp_override` because that's the hull the encounter
actually fielded. Line 241-245: boss `saturating_mul(2)`.

**Worked examples:** `salvage_for_encounter_sums_per_enemy` (src/meta.rs:387) —
two hull-3 + one hull-7(override) = 1+1+3 = 5; `salvage_for_boss_encounter_doubles`
(src/meta.rs:421) — hull-10 boss → 3 × 2 = 6.

### `fn award_run_salvage<F>(run, encounter, class_to_ship)` (src/meta.rs:251)

**Intent:** Add a won encounter's salvage to the live `Run`, in place, with
`saturating_add`. The bin calls this once per encounter-complete event (not per
frame). `award_run_salvage_saturates_not_overflows` (src/meta.rs:461) pins the
saturation.

---

## Run end → meta rollover

### `const SUBSYSTEM_UNLOCK_THRESHOLDS` / `CARD_UNLOCK_THRESHOLDS` (src/meta.rs:272, 282)

The single source of truth for `(id, total_salvage_required)` pairs. Adding an
unlock means appending one row here plus a catalog `subsystems[]` entry — no
other code path changes. Cards table is currently empty.

### `fn accumulate_into_meta(meta, run) -> Vec<String>` (src/meta.rs:293)

**Intent:** Roll the run's salvage into the persistent total and apply any
threshold the new total just crossed. Returns the list of newly-unlocked
subsystem ids so the bin can flash "UNLOCKED: Rear Gunner" on the run-end screen.

Line 295-297: capture `prev_total`, roll forward with `saturating_add`, capture
`new_total`. Line 301-309: for each threshold, the **edge-trigger** condition
`prev_total < threshold && new_total >= threshold` fires exactly once as the
total crosses, and the `contains` guard prevents duplicate entries. Line 311-318:
the identical card loop (no-op while `CARD_UNLOCK_THRESHOLDS` is empty).

Called on **every** run end, defeated or victorious. It is idempotent only at the
caller's level — double-firing the run-end event doubles salvage, so the bin must
fire it once.

**Worked examples:** `accumulate_crosses_threshold_unlocks_subsystem`
(src/meta.rs:514) — salvage 10 unlocks `rear_gunner` only;
`accumulate_multiple_thresholds_in_one_jump` (src/meta.rs:525) — salvage 26
unlocks both `rear_gunner` and `chain_bounty`;
`accumulate_idempotent_for_already_unlocked` (src/meta.rs:537) — re-crossing an
owned threshold adds no duplicate. The
`unlock_thresholds_are_in_ascending_order` invariant (src/meta.rs:604) guarantees
the ladder is monotonic, which the edge-trigger logic relies on.

**Cross-references:** Reads `SUBSYSTEM_UNLOCK_THRESHOLDS`. Mutates
`MetaProgression`. Consumes the [`Run`](types.md)'s `salvage`. Called by the bin
between `Run::delete_save` and `MetaProgression::save_to_disk` on run end.
