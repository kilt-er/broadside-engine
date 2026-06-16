# V-review — #49a live-combat keystone (`2364e7b` catalog merge + `0fea775` C1 camp / C2 spread)

**Status:** ✅ **APPROVE both.** The enemy-fire keystone is correct: catalog weapons now serve the
firing Content (no double-serve / no player shadow), C2 threat-spread is a tie-breaker that never
suppresses firing (single-source, #41/#71 lesson honored), and the C1 camp fix is over-extension-
safe. Static read (zero-builds hold); committers report tests green, trusted.

---

## `2364e7b` — serve catalog weapons to the bin's firing Content — APPROVE

The "enemies don't fire" root cause: catalog-synthesized enemies mount catalog weapon ids
(beam_cannon, railgun_broadside, …) but `DemoContent` only hardcoded pulse_laser/torpedo/
broadside_battery → `content.action(enemy_weapon)` returned `None` → the AI fire-gate skipped →
enemies inert. Fix: `DemoContent::install_catalog_actions` merges the loaded catalog's actions in.

- **No double-serve / no player-weapon shadow:** `self.actions.entry(id).or_insert_with(|| …)` —
  **insert-if-absent**. A HashMap keyed by id (one entry each); existing (hand-tuned player)
  entries WIN, catalog-only ids get added. Player loadout behavior unchanged. ✓
- **Fire-gate now passes for enemy weapons:** the catalog actions "already carry 2-D `range_band`
  (derived at load by `catalog::load_from_bytes`)" — so `resolve_targeting_2d`'s `in_band` can
  succeed for them, where pre-#28/#49a they had empty bands → always inert. This is the join of
  #28 (author 2-D bands) + #49a (serve them). ✓
- **Graceful None-catalog:** `build_content(None)` falls through to bare `fresh_content` (no panic;
  enemies stay inert as before). Re-applied on `self.content = build_content(self.catalog.as_ref())`.
- **Scope:** bin/`DemoContent` wiring only — the engine firing path (`resolve_targeting_2d`) is
  untouched, so no single-source surface changes. Lower-risk than an engine change.

## `0fea775` C2 — cross-enemy threat-spread (#35/#74) — APPROVE (tie-breaker, never a gate)

The load-bearing concern (the #41/#71 lesson: spread must never suppress a shot). **Verified it's a
tie-breaker, not a gate:**

- `spread_set = allies_threatened_cells(enemy_pos, board, content)` is used ONLY in the FIRE-rung
  score: `score -= SPREAD_OVERLAP_PENALTY(1) * overlap` (`ai.rs:155-156`), applied **inside the
  per-weapon scoring loop AFTER the fire-gate already passed** (the `resolve_targeting_2d` empty-
  check + friendly-fire filter `continue` earlier). It adjusts the score of an ALREADY-VIABLE shot.
- Penalty `1`/cell is dominated by `+10` player-hit and raw-damage — it breaks ties between
  comparable shots; it cannot make `best` empty or skip the FIRE rung. The `if let Some((_, id)) =
  best { push; return }` (`:162`) still fires the top-scored viable shot. **An overlapping shot is
  still fired**, just lower-ranked vs a non-overlapping alternative. ✓
- **Single-source:** `allies_threatened_cells` (`:268`) resolves each earlier-committed ally's
  queued action via **`resolve_targeting_2d`** (`:293`) — the same spine, NOT a parallel targeting
  computation. It reads `order[..self_idx]` (initiative order BEFORE this enemy) so spread is
  against already-decided allies (correct semantics, no circularity), damage-only. ✓

## `0fea775` C1 — camp fix (Rung 3.5 fallback close) — APPROVE (over-extension-safe)

Bruce's "enemies just sit there": the ladder used to fall through to an empty queue when an enemy
couldn't fire, couldn't band-maneuver, and had no reorient → CAMPED. Fix adds **Rung 3.5
FALLBACK CLOSE** (`ai.rs:205-226`): if not locked-out/anchored, close one step toward the player.

- **Correctly ordered:** FIRE (1) → CLOSE/HOLD-RANGE (2, band-aware) → REORIENT (3) → **FALLBACK
  CLOSE (3.5)** → VENT (4) → empty (5). It runs ONLY after fire + Rung 2 + reorient all declined,
  so it can't pre-empt firing.
- **Over-extension UNHARMED** (the #7 property I guard): it fires only when Rung 2's band-aware
  open/close already returned None — which means the weapon is IN BAND (Rung 2 owns the back-off
  decision for a closed-on Far gun and would return a back-off *direction*, not None, in a deadzone
  case). So closing here keeps the gun in/below band — it "never strands a Far gun's deadzone
  open." The deadzone-re-open logic is Rung 2's and runs first. ✓
- Preserves liveness (queues a visible move, not nothing). A locked-out enemy still prefers VENT.

## Out of scope (correctly)

The 1 remaining red `ai_fires_through_ally_to_reach_player` is NOT this code — it's the `hits_all`
pierce question (#3: resolver's `resolve_targeting_2d` SPINAL behavior + the tester's fixture
expectation), adjacent to the telegraph-vs-friendly-fire edge I flagged earlier (lead-ruled accept,
playtest-gated). The camp fix even improved its symptom (`[]` → `[__move_down]` — the enemy
correctly closes when its sweep can't reach). Not a blocker for this verdict.

---

## Verdict

**APPROVE both.** 2364e7b serves catalog weapons insert-if-absent (no double-serve / no player
shadow; fire-gate passes via the now-populated 2-D bands). 0fea775 C2 spread is a single-source
tie-breaker that never suppresses firing (the #41/#71 lesson holds); C1 camp fix is correctly-
ordered Rung 3.5, over-extension-safe. **Live 2-D combat now fires on both sides** through the one
`resolve_targeting_2d` spine — the keystone lands clean. **Caveat: static-only per the zero-builds
hold; spot-confirm the ai_ + combat tests when a build window opens.**

---

*Cross-ref: V4 (`072f1b7`) + V4-at-C1 (`5bbff3e`) — the single-source AI gate this builds on; the
#41/#71 over-extension/canary lesson (RED-triage `a5daf64`, the FF ruling `635155d`). #49a @
`2364e7b` + `0fea775`.*
