# V4 review — `resolve_targeting_2d` single-source targeting path (`59c0baa`)

**Status:** ✅ **APPROVE** of the single-source FIRING/telegraph property, **with one load-bearing
caveat** (the live 1-D AI gate — see §3). The caveat is C1's lane, not an R3 defect, but it is
flagged as **live-and-wrong, NOT dead-for-live** — the resolver's handoff understated it.

**Mandate (blueprint):** "enforce: NO second targeting path for telegraphs." The ThreatMap (R8)
must paint cells by running the *same* `resolve_targeting` the real shot uses, so painted threat
≡ where the shot lands. V4 verifies R3 establishes exactly one cell-selection function.

---

## 1. The single-source property — HOLDS for firing + telegraph

`resolve_targeting_2d(a, board, ship_pos) -> Vec<Pos>` (`resolve.rs:901`) is the sole 2-D
cell-selection function. Verified by **enumerating every caller** of both it and the legacy 1-D
`resolve_targeting` across `src/`:

| Call site | Path | Live? | Verdict |
|-----------|------|-------|---------|
| `run_action` `resolve.rs:434` (firing) | `_2d` | **LIVE** | ✓ the firing path |
| `run_action` `resolve.rs:526` (2nd pass, twin-linked re-resolve) | `_2d` | **LIVE** | ✓ same fn, current Pos |
| `decide_enemy_action` `resolve.rs:2216` (AI action-gate) | **1-D** | **LIVE** (see §3) | ⚠ caveat |
| `resolve.rs:3079, 3096` (spinal tests) | 1-D | test-only | fine (1-D fixtures) |
| `resolve.rs:4648–4746` (rt2d_* tests) | `_2d` | test-only | fine |

So for the **firing** path: 100% `resolve_targeting_2d`, no parallel selection. When R8 lands, it
runs the *same* fn against each enemy's queued action — single source by construction. ✓

## 2. Body is faithful to the firing-direction contract

`resolve_targeting_2d` (`resolve.rs:901–990`), per-pattern:
- **SELF** → `[ship_pos]`. ✓
- **BEAM/POINT_BLANK** → first in-band occupant along the first bearing cardinal that has one
  (faithful 2-D analog of the 1-D fore-first scan). ✓
- **BROADSIDE** → first in-band occupant on *each* bearing cardinal (both flanks). ✓
- **SPINAL_LINE** → pierce one bearing cardinal; `hits_all` → all in-band occupants, else first. ✓
- **BLAST** → first occupant on a bearing cardinal + its `grid::neighbors` (8-neighbour splash).
  This is the agreed **gate #2** widening, documented in-code as the intended off-ray
  diagonal-`incoming_from` case (handled at R4/V5; `facing_zone` is total over 8). ✓
- **DEPLOYED_CELL/ORDNANCE** → one `grid::offset` step along the forward cardinal. ✓

All firing rays use `bearing_cardinals(ship.facing, requires_arc)` (cardinal-only, gated by the
**cardinal-exact `arc_bears`** I re-blessed at `f1db141`) + `first_target_along`/`cells_along`
(via `grid::offset` — **gate #1**, no underflow hack, the 1-D `first_target_toward` probe was
genuinely re-derived, not ported). `in_band` realises the **decision-#7 deadzone** (gate #3 same-
cell `direction_to → None` is moot here — targeting walks outward from `ship_pos`, never tests a
cell against itself). The three semantic-drift gates from my V2 §7 are all addressed. ✓

## 3. CAVEAT — the live 1-D AI gate (`decide_enemy_action`), C1's lane

**The resolver's handoff said `decide_enemy_action`'s 1-D `resolve_targeting` call is "dead-for-
live AI pending C1." That is imprecise: it is LIVE, and on the 2-D board it is WRONG.** Trace:
`resolve_round` (`resolve.rs:184`) → `run_world_phase` (`:293`) → the `enemy_initiative` loop
calls `decide_enemy_action(enemy_cell, …)` at **`:355` every world phase for every living
enemy**. Inside, `:2216` calls the **1-D** `resolve_targeting(action, board, enemy_cell)` as the
arc/band gate that scores + picks which action the enemy queues.

So there are currently **two live targeting calls with different geometry**:
1. AI **decision**: 1-D `resolve_targeting` (reads `enemy_cell` as a *lane* index on a *grid* board
   — wrong geometry; the AI will pick actions whose 1-D gate disagrees with where the 2-D shot
   actually lands).
2. **Firing**: 2-D `resolve_targeting_2d` (correct).

**Why this is still an APPROVE, not a rejection:**
- It is **not a second FIRING/telegraph path** — firing is single-source `_2d`, and R8's
  ThreatMap will use `_2d`. V4's literal mandate (no second telegraph selection) is met.
- R3 **correctly did not touch the AI** — `decide_enemy_action` is **C1's lane** (`ai.rs` rewrite,
  task #6, in progress). Touching it in R3 would cross lanes.
- It is currently masked: the demo/tests exercise it but the geometry mismatch only surfaces as
  *suboptimal/incorrect enemy action choice*, not a crash or a firing desync.

**Why it must NOT be left as "dead":**
- It is **executed live**, so until C1 lands, 2-D enemy AI picks actions on 1-D geometry — real
  wrong behavior, not dormant code.
- **Convergence requirement (the V4-relevant part):** C1 MUST route `decide_enemy_action` through
  `resolve_targeting_2d`, and R8's ThreatMap MUST use the same call against the queued action. If
  C1 picks via `_2d` but R8 paints via anything else (or the AI keeps the 1-D gate), the telegraph
  desyncs from the shot — the exact failure V4 exists to prevent. **This is the single-source
  seam to hold at C1 + R8 review.**

## 4. Out of scope (correctly deferred, noted for V5)

`apply_damage` (`resolve.rs:1011`) is still the 1-D version (`target.cell`, `direction_to →
LaneEnd`, `facing_zone(orientation, …)`). That is **R4/V5**, correctly untouched by R3. The
`Pos → to_index()` shim at the two live `_2d` call sites (`:435`, `:528`) is a **representation
conversion of the already-selected cells** under the documented slot==`pos.to_index()` invariant
(A) — NOT a re-selection, so it is not a second path. It is removed when R4 takes
`apply_effect`/`apply_damage` to `Pos`.

---

## Verdict

**APPROVE** — R3 establishes `resolve_targeting_2d` as the single firing+telegraph cell-selection
path; body faithful to the firing-direction contract; all three V2 §7 drift gates addressed; 8
rt2d_* tests + 443 lib green; clippy clean. **Caveat carried forward (not blocking R3):**
`decide_enemy_action`'s 1-D `resolve_targeting` is LIVE-and-wrong on the 2-D board (not dead) —
C1 must converge it onto `resolve_targeting_2d`, and R8's ThreatMap must reuse the same call, or
the telegraph will desync. **Holding this as the single-source seam for V4-at-C1 and V4-at-R8.**

---

*Cross-ref: V2 checklist §3 (resolve.rs spatial bodies) + §7 (drift gates, firing-direction
contract); V3 (`arc_bears` cardinal-exact, the firing gate); V5 (R4 pipeline ORDER +
`direction_to → incoming_from`). V4 done @ `59c0baa`; AI-convergence re-checked at C1 + R8.*
