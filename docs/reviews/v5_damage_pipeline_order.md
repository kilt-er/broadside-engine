# V5 review — `apply_damage_2d` pipeline ORDER + EventBus contract (`a61db55`)

**Status:** ✅ **APPROVE.** The 2-D damage wiring (R4) preserves the canonical pipeline ORDER
byte-for-byte and leaves the EventBus contract untouched. Only steps 1 + 4 swap to 2-D geometry,
exactly as scoped. Lib green (368 default-feature / 451 all-feature, 0 fail, 8 ignored);
`clippy --lib` clean.

**Mandate (blueprint V5):** "guard the damage-pipeline ORDER + EventBus contract through the
rewrite." The order is the documented load-bearing invariant
(`resolve.rs` LOAD-BEARING ORDER comment + `resolve.ts` canonical): re-ordering it changes
balance silently.

---

## 1. Pipeline ORDER — byte-for-byte identical to the 1-D `apply_damage`

Compared `apply_damage_2d` (`resolve.rs:1104`) step-for-step against the canonical 1-D
`apply_damage` (`resolve.rs:1011`):

| Step | 1-D `apply_damage` | 2-D `apply_damage_2d` | |
|------|---------------------|------------------------|---|
| guard | `cells[target].is_none() → return` | `ship_at(target_pos).is_none() → return` | ✓ |
| **1. band falloff** | `range_band(atk,tgt)` + `band_falloff(raw, band, optimal_band)` | `geometry2d::range_band` + `geometry2d::band_falloff(raw, band)` — absolute [1.0,0.6,0.3] curve, **drops `optimal_band`** (decision #6); **same disable predicate** `any(DAMAGE{band_falloff:Some(false)})` | ✓ swapped to 2-D |
| **2. subsystem modifiers** | `apply_modifiers(dmg, atk_cell, band, …)` | `apply_modifiers(dmg, atk_pos.to_index(), range_to_rangeband(band), …)` — **attacker-side preserved**; Range→RangeBand shim | ✓ |
| **3. target-lock ×2** | `position(TargetLock)` → `*=2` → `swap_remove` | identical | ✓ |
| **4. directional shield** | `direction_to(tgt,atk)` → `facing_zone(orientation, incoming)` → `absorb_shield` | `geometry2d::direction_to(tgt,atk)` → `facing_zone(facing, incoming)` (None→Bow) → `absorb_shield` | ✓ swapped to 2-D |
| **5. hull + emit + destroy** | `hull -= final`; `emit(OnDamageTaken){target_cell,amount}`; `if killed destroy` | identical (via `target_idx`) | ✓ |

**No step re-ordered, added, or removed.** Steps 2, 3, 5 are logically identical; only 1 and 4
change geometry — the exact scope V5 allows.

## 2. The two swapped steps are correct

- **Step 1 (band falloff):** the 2-D `band_falloff(raw, band)` is the ABSOLUTE [1.0,0.6,0.3]
  curve keyed on the *actual* band — deliberately NOT the 1-D distance-from-`optimal_band` model
  (decision #6; the function itself was V3-reviewed in geometry2d). Dropping the `optimal_band`
  arg is correct, not a lost input. The disable predicate is unchanged (one DAMAGE effect with
  `band_falloff: Some(false)` disables falloff for the whole call). Because `apply_damage_2d` is a
  NEW function, no existing damage test asserted the old curve — consistent with the resolver's
  "flipped ZERO damage tests" (the `mod_*`/`apply_damage` tests exercise the untouched 1-D fn).
- **Step 4 (directional shield) — the `direction_to → incoming_from` wiring I hold:**
  `incoming_from = direction_to(target_pos, atk_pos)` points back at the gun (correct: the shot
  arrives FROM the attacker). Fed to `facing_zone(target.facing, incoming_from)` (the V3-approved
  + arc_bears-consistent table). **Same-cell `None` → `HullZone::Bow`** — reachable only by a
  degenerate self-collision; benign (a zone-less hit lands on a defined face, no panic). Per the
  firing-direction contract: DIRECT fired hits give an exact opposite cardinal; BLAST splash /
  ordnance give a diagonal — and `facing_zone` is total over all 8 (the arity seam confirmed at
  V3/V4). So every `incoming_from` resolves to a defined zone. ✓

## 3. EventBus contract — UNTOUCHED

R4's diff (`git show a61db55`) does **not** touch the `emit` wrapper or `mem::take` — they are
not in the diff at all. `apply_damage_2d` step 5 calls `emit(board, Hook::OnDamageTaken, |ctx| {
ctx.target_cell = Some(target_idx); ctx.amount = Some(final_dmg); })` — identical hook, identical
payload shape, identical fire-condition (`final_dmg > 0`) to the 1-D version — then `destroy` on
kill. No chained emit introduced (the callback only sets ctx fields; the no-chained-emit
invariant is preserved). The damage-pipeline → EventBus seam is intact. ✓

## 4. Live wiring + additivity

- The 1-D `apply_damage` is **genuinely untouched** — R4's diff adds `apply_damage_2d` purely
  additively (`@@ -1082,6 +1082,116 @@`, a `+`-only block after the 1-D fn). The 1-D fn is kept
  for ordnance / ReactorBreach-splash / fixture tests until those R-tasks migrate.
- **Three live call sites switched to the 2-D path** (verified in the diff), each `apply_damage(…
  .to_index()…)` → `apply_damage_2d(… Pos …)`:
  1. `apply_effect` DAMAGE arm (the main firing path) — real Pos via invariant A.
  2. `self_move_2d_commit` collision (R6).
  3. `resolve_target_move_2d` collision (R6b, `78ebb59`).
  The two collision sites now compute the **true 2-D shield face** (was a provisional 1-D zone) —
  a correctness improvement, and they reuse `apply_damage_2d` (NOT a parallel damage path).
- `range_to_rangeband` shim (`resolve.rs:1187`): documented R4-transition-only; collapses
  `Adjacent→PointBlank / Near→Close / Far→Mid` to feed the still-1-D `Content::damage_modifier`
  (an inert stub today). Removed when the trait migrates to `Range` (#34). Benign — modifiers are
  a no-op now, so the lossy 3→5 collapse changes nothing live.

## 5. R6b (`78ebb59`, `resolve_target_move_2d`) — spot-check

Folded into this pass since it shares the collision-damage seam. It routes DISPLACE_TARGET
push/pull/swap collisions through `apply_damage_2d` (the same single damage path) over grid +
invariant A — not a new damage or targeting path. Consistent with V4 (single-source) + this V5.
No concern.

---

## Verdict

**APPROVE.** Pipeline ORDER preserved byte-for-byte (only steps 1+4 → 2-D geometry); EventBus
`emit`/`mem::take` contract untouched; `direction_to → incoming_from → facing_zone` wiring
correct with sound `None`→Bow degenerate handling; 1-D `apply_damage` additive-untouched; live
wiring (DAMAGE arm + both movement collisions) routes through the one 2-D damage path with real
Pos. The `range_to_rangeband` shim is a benign documented transition (removed at #34). Lib green,
clippy clean.

**Forward-note for CONTRACT:** when the 1-D world is deleted, the 1-D `apply_damage` + the
`range_to_rangeband` shim + the `optimal_band`-based 1-D `band_falloff` all go, and `damage_modifier`
migrates to `Range` (#34). Tracked against V2 checklist §8 / R-series.

---

*Cross-ref: V2 checklist §3 (resolve.rs spine — pipeline ORDER + EventBus), §7 (firing-direction
contract — the `incoming_from` arity); V3 (`facing_zone` table fed at step 4); V4 (single-source —
the same `apply_damage_2d` is the one damage sink). V5 done @ `a61db55` (+ R6b `78ebb59`).*
