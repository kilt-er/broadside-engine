# `src/ai.rs` — enemy-AI decision layer (the 2-D ladder)

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/ai.rs`](../LINE_BY_LINE.md#srcairs) section of `LINE_BY_LINE.md`.*

**Mirrors:** `resolve.ts:395` (the `decide_enemy_action` stub) in origin, but the 2-D
ladder is a Rust-side rewrite with no faithful TS counterpart. **Extracted** from
[`resolve.rs`](resolve.md) at commit `1654e67` (blueprint C1).

---

## Why this module exists

The enemy AI used to live inside `resolve.rs`. It was lifted out (`1654e67`) so the
**resolver lane** (R4/R5/R7/R8 geometry + damage work) and the **content/AI lane**
(this 2-D rewrite) stop editing the same file — a parallel-work seam, not a logical
one. The four-phase round in `resolve.rs` still calls `decide_enemy_action` once per
living enemy, in [`enemy_initiative`](resolve.md) order, during the world phase; the
`usize`-cell entry point keeps that call site byte-identical (no `resolve.rs` co-edit).

The AI is **only a decision layer**: it builds an enemy's `queue` and stops. It never
runs `execute_queue`, never touches the damage pipeline, never moves a ship itself
(hard boundary). Whatever it queues is left **un-fired** until the next world phase, so
on the player's turn the renderer always has a telegraph to draw — the read-and-react
loop the whole game is built around.

### The one invariant that makes it correct: the single-source fire-gate

Every "can I fire?" and "does my arc bear?" question is answered by the **same**
[`resolve::resolve_targeting_2d`](resolve.md) the shot actually fires through and the
`ThreatMap` (R8) caches. There is **no second targeting path** — the 1-D
`resolve_targeting` is never called here (reviewer V4 greps this file and must find
zero `resolve_targeting(`). Consequences:
- *What the AI elects to fire == what the telegraph paints == where the shot lands.*
- **Over-extension is free.** A Far weapon's `range_band` excludes `Adjacent`, so once
  the player closes to distance 1 the gate returns empty → the enemy is correctly
  inert (blueprint decision #7). Rung 2 then backs it off to re-open range rather than
  charging in. The player punishes a camping long-gun by closing on it.

---

## `fn decide_enemy_action(enemy_cell, board, content)` (src/ai.rs:56)

The five-rung ladder; **first match wins: FIRE → CLOSE/HOLD-RANGE → REORIENT → VENT →
empty.**

**Setup (src/ai.rs:56-90).** Recover the enemy `Pos` from the flat cell via
`Pos::from_index` (board invariant A: `slot == pos.to_index()`). Find the player `Pos`
by scanning `board.cells`. **Snapshot** the enemy's gating state — `heat`, `heat_max`,
`locked_out`, cloned `cooldowns`, `mount_weapons`, `traits`, and (#166) `facing` —
in a read-only borrow that is released *before* the scoring loop re-borrows the board
for `resolve_targeting_2d`. Three trait flags are lifted: `burn_hard` (heat counts
half), `pursuit` (+2 on a player hit), `anchored` (skips every self-movement rung).

### RUNG 1 — FIRE (src/ai.rs:106-179)

Score every mount weapon, fire the best. Per weapon, gate then score:

| Gate (skip if fails) | Where |
|---|---|
| cooldown > 0 | src/ai.rs:113 |
| locked-out and `heat > 0` | src/ai.rs:118 |
| would push heat > `heat_max + 1` | src/ai.rs:121 |
| `resolve_targeting_2d` empty (off-arc / out-of-band / #7 deadzone) | src/ai.rs:125 |
| target set has no non-enemy (#49 friendly-fire filter) | src/ai.rs:130 |

**Score** (src/ai.rs:148-168) — selects WHICH weapon, never WHETHER to fire:
`+10` player in a hit cell · `+raw_damage` (summed `Effect::DAMAGE`) · `−heat` (halved
for `BurnHard`) · `+2` `Pursuit` on a player hit · `−1` per target cell overlapping the
ally **spread set** (`SPREAD_OVERLAP_PENALTY`, src/ai.rs:47). The spread term is
deliberately tiny — it fans a squad's threat across distinct cells but is dominated by
`+10`/damage and **never gates a shot** (the #41/#71 lesson: diversity must not cause
"march, don't shoot"). If anything scored, push the best id and **return — fire is
unconditional** (#71: fire-when-in-position beats holding).

### RUNG 2 — CLOSE / HOLD-RANGE (src/ai.rs:199-230)

Only if we couldn't fire, aren't locked out (locked-out prefers to VENT), and aren't
`Anchored`. [`choose_maneuver_dir`](#helpers) owns the **band** decision (close / open /
hold). Under Bruce's **no-strafe** ruling (#166) the ship never slides sideways, so the
absolute slide that function would pick is converted:
1. derive the **dominant cardinal** of the enemy→player delta
   ([`dominant_cardinal`](#helpers)), flipped to its opposite when *opening* range;
2. if that cardinal is on the bow axis (== forward or its reverse) → queue the on-axis
   forward/reverse step ([`synthetic_move_for_dir`](#helpers));
3. else (perpendicular) → queue a **ROTATE** toward it
   ([`rotate_toward_cardinal`](#helpers)); next phase the bow points right and the ship
   advances forward.

Rotate-then-forward, never a free lateral step — mirroring the resolver's forward-only
self-move gate (#167).

### RUNG 3 — REORIENT (src/ai.rs:236-279)

- **3a (src/ai.rs:236-256):** a mounted weapon that *itself* reorients (an
  `Effect::REORIENT`, e.g. a sweep) — fire it.
- **3b — arc-agnostic rotate-to-bear (#92, src/ai.rs:270-279):** queue a synthetic
  ROTATE toward the facing from which the enemy's own weapon BEARS on the player
  ([`rotate_to_make_weapon_bear`](#helpers)). This kills the "camp + never fire" bug
  where a mis-oriented hull closes forever without turning its gun to bear. Skipped for
  locked-out and `Anchored`.

### RUNG 3.5 — FALLBACK CLOSE (src/ai.rs:311-326)

Reached when we couldn't fire, Rung 2 *held* (it returns `None` for "in band but
blocked by arc/heat/cooldown"), and no `REORIENT` exists. Pre-fix the ladder fell to
empty here and the enemy **camped** (Bruce's "enemies just sit there"). So close one
step toward the player with the same rotate-then-forward discipline as Rung 2.
**Gated on ≥1 mount** (src/ai.rs:311): a mountless hull holds still — both correct (no
gun = no maneuver intent) and the fix that keeps the no-strafe winnability canary
converging (a wandering mountless "target" hull chase-livelocked it). No live enemy is
mountless; this only affects test-harness hulls.

### RUNG 4 — VENT (src/ai.rs:329-343) · RUNG 5 — empty (src/ai.rs:345)

Rung 4: any mount with an `Effect::VENT_HEAT` clears heat so the ship can fire again.
Rung 5: only a misconfigured (no valid mount) enemy reaches here; the world phase
no-ops its turn.

**Visible-threat invariant.** Every successful turn produces a queued action — fire OR
a fallback (close / reorient / vent), each a legible telegraph the renderer draws over
the ship.

---

## Helpers

- **`allies_threatened_cells(self_pos, board, content)`** (src/ai.rs:372) — the C2
  (#35) threat-spread context: cells already threatened by allies who committed
  **earlier in this decision pass**. The subtlety: `run_world_phase` interleaves
  fire-then-decide per enemy in `enemy_initiative` order, so when enemy *E* decides,
  allies *before* it hold fresh this-pass intent (count them) and allies *after* hold
  stale last-phase intent (skip them). Each ally's cells come from `resolve_targeting_2d`
  on its queued **damaging** action (the same single source). Pure read.
- **`choose_maneuver_dir(...) -> Option<Dir8>`** (src/ai.rs:430) — the band decision,
  keyed on the **dominant** (highest summed DAMAGE) weapon. Returns *toward* (player
  too far), *away* (player nearer than the weapon's nearest band — the #7 deadzone
  back-off), or `None` (in band but couldn't fire → arc problem; hold for Rung 3). An
  empty `range_band` (not-yet-re-authored EXPAND-window catalog) means "no preference →
  close" (v1 behaviour). `band_ordinal` (src/ai.rs:493) ranks `Adjacent < Near < Far`.
- **`synthetic_move_for_dir(dir)`** (src/ai.rs:513) — map a `Dir8` to a resolver-served
  synthetic move id. Post-no-strafe the AI only passes a **cardinal**; the diagonal
  arms remain only to keep the mapping total over `Dir8`.
- **`dominant_cardinal(from, to)`** (src/ai.rs:541) — collapse the delta to a single
  cardinal the hull can **face** (larger axis wins; ties pick the E/W dodge axis).
  Intentionally distinct from `grid::from_to`'s 8-way octant.
- **`rotate_toward_cardinal(current, target)`** (src/ai.rs:563) — shortest quarter-turn
  `__rotate_left`/`__rotate_right` toward `target` (`None` if aligned; 180° picks right,
  finishes next phase).
- **`rotate_to_make_weapon_bear(...)`** (src/ai.rs:606) — the #92 arc-agnostic
  rotate-to-bear. Clones the dominant weapon (drops the `content` borrow), then
  **probes** each of the four `Bow` cardinals by temporarily setting `facing`, running
  `resolve_targeting_2d`, checking the player's cell, and **restoring** the facing (pure
  probe). Picks the bearing facing needing the fewest quarter-turns. This is why a
  `BroadsideArc` enemy orients perpendicular and a Forward enemy orients bow-on — all
  from the one targeting path, no hardcoded stance rule.

---

## Worked examples (tests/ai_2d.rs + tests/broadside.rs)

- `ai_skips_out_of_band_action_and_closes` (tests/ai_2d.rs:360) — out-of-band gun,
  enemy closes instead of firing.
- `ai_closes_via_synthetic_move_when_cannot_fire` (tests/ai_2d.rs:389) — Rung 2 queues
  a synthetic move toward the player.
- `ai_rotates_to_bear_when_misfacing_in_band` (tests/ai_2d.rs:417) — in band but
  off-arc → Rung 3b rotate-to-bear, not a close.
- `ai_rotates_then_advances_when_approach_is_perpendicular` (tests/ai_2d.rs:459) — the
  #166 no-strafe case: perpendicular approach → ROTATE first, advance next phase.
- `ai_skips_action_on_cooldown_and_closes` (tests/ai_2d.rs:551) /
  `ai_skips_action_that_overshoots_heat_budget_and_closes` (tests/ai_2d.rs:579) — the
  cooldown and heat-budget gates.
- `misfacing_broadside_enemy_rotates_flank_to_bear_then_fires` (tests/broadside.rs:195)
  — the arc-agnostic rotate-to-bear bringing a flank gun onto the player.

---

## Status / drift

The 2-D ladder is live (`1654e67` extraction; #166 no-strafe rotate-then-forward; #167
resolver forward-only gate; #92 arc-agnostic rotate-to-bear; #35/#74 threat spread).
**Superseded model:** the old 1-D fire-vs-maneuver write-up (the `+6` lane-end
diversity bonus, `queue_purposeful_maneuver`, lateral `__move_left`/`__move_right`
closes, covered-end fire-suppression) is design history only — see the banner at the
top of [`resolve.md`](resolve.md) § "The AI loop." True cross-enemy threat coordination
(an initiative pass assigning enemies to distinct ends) was **never built**; lane-end
diversity is emergent from geometry, not directed.
