# V-review R7 — dodge-whiff `hit:false` FireEvent (`b72035b`)

**Status:** ✅ **APPROVE.** R7 draws the dodge-whiff entirely off the single-source `board.threats`
(R8's paint) — **no second targeting path** — and is well-tested (4 r7_ tests incl. the subtle
multi-enemy source-filter case). Static review (read-only, fine under the build freeze).

**Feature:** when the player VACATES a cell an enemy telegraphed last phase, the enemy's queued
shot finds it empty; R7 emits a `hit:false` FireEvent (firer → the now-empty telegraphed cell) so
the renderer draws the beam firing into the space the target just left — the read-and-react payoff
of the telegraph (blueprint "dodge-whiff").

---

## The single-source check (the V4-relevant property) — HOLDS

The whiff block (`run_action`, `resolve.rs:563-581`) computes the whiffed cells by **reading
`board.threats`**, NOT by recomputing targeting:

```
let whiffed: Vec<Pos> = board.threats.iter()
    .filter(|th| th.source == ship_pos && board.ship_at(th.pos).is_none())
    .map(|th| th.pos).collect();
```

There is **no `resolve_targeting_2d` call** here — it consumes the R8-painted set (which itself is
the single-source spine, V4-at-R8). So R7 introduces no parallel telegraph/targeting path: paint
(R8) → dodge (player) → whiff (R7) all key off the one `board.threats`. A naive R7 would have
re-run targeting to ask "what would I have hit"; this correctly reuses the painted threats. ✓

## Semantics — correct

- **Placement:** before the nothing-bore gate (`requires_arc.is_some() && cells.is_empty()` early
  return) — so a vacated shot draws its miss before the gate would swallow it silently. The gate
  still governs heat/cooldown unchanged; the whiff is additive render state. ✓
- **Whiffs ONLY telegraphed-by-this-firer AND now-empty cells:** `th.source == ship_pos` (this
  ship's own telegraph) + `ship_at(th.pos).is_none()` (player vacated). A still-occupied
  telegraphed cell resolves normally via the `hit:true` path below — no double-draw. ✓
- **Fires-only:** gated on an `Effect::DAMAGE` present, so a queued move/vent/reorient (no Damage
  threat) emits no whiff. ✓
- **FireEvent shape:** `from_pos: ship_pos`, `to_pos: pos` (vacated cell), `hit: false`,
  `attacker_faction: ship_faction`, archetype from the action. ✓
- **Timing is right:** within this phase's `run_action`, `board.threats` still holds LAST phase's
  paint (R8 repaints at the END of the world phase, after firing). So the whiff reflects "what I
  telegraphed last turn vs where the player is NOW" — exactly the one-turn-ahead read. ✓

## Tests — real + well-chosen (4)

- `r7_whiff_emitted_when_player_vacated_a_telegraphed_cell` — core: telegraphed (2,3) now empty →
  exactly one whiff (2,1)→(2,3), hit:false. ✓
- `r7_no_whiff_when_telegraphed_cell_still_occupied` — player still on (2,3) → hit:true, NO whiff
  (guards over-whiffing). ✓
- `r7_no_whiff_for_a_non_damage_action` — queued vent → no FireEvent at all. ✓
- `r7_whiff_only_for_this_firers_telegraph` — two enemies telegraph different cells; firer A
  whiffs ONLY A's vacated cell, not B's (proves the `source == ship_pos` filter). ✓ — the subtle
  one, and the right thing to pin.

## Observations (not blockers)

- **Different-ship-now-occupies case:** the filter is `ship_at(pos).is_none()`, so if a *different*
  ship (e.g. an ally that drifted in) now sits on the telegraphed cell, R7 emits no whiff and the
  shot resolves `hit:true` against whoever is there. That's a reasonable reading ("the shot hits
  whatever's in the cell now"), consistent with "whiff only empty cells." Neither a clean whiff nor
  the originally-telegraphed target, but a coherent design choice, not a bug. R7 introduces ZERO
  new behavior in that branch — it adds ONLY the empty-cell whiff; the occupied-cell case resolves
  through the UNCHANGED pre-R7 firing path. Fine as-is.

- **Pre-existing telegraph-vs-AI-friendly-fire edge (NOT R7's, surfaced from this review — flagged
  to lead for a conscious call).** Verified in source: the friendly-fire guard is **election-only**
  (`decide_enemy_action`'s `any_hostile` check; test `ai_skips_friendly_fire_only_target`
  `:4186`). The **fire-time** path (`apply_effect` DAMAGE arm → `apply_damage_2d`) has **no**
  friendly-fire filter — it applies to whatever `resolve_targeting_2d` returns (first occupant,
  any faction; the `occ_faction != owner_faction` checks at `:789`/`:855` are projectile-detonation
  only, not beam targeting). So a shot **telegraphed last turn** (election guard ran against last
  turn's board) can hit an **ally that drifted onto the ray this turn**. Pre-existing (the 1-D
  engine had the same election-only guard), independent of R7/R-series, and partly intersects the
  intended "Unfriendly Fire" design (player-*forced* friendly fire is a feature, `:4177-4178`).
  Whether an **AI** enemy accidentally hitting its **own ally** via a stale telegraph is intended
  is the lead's call; if addressed it'd be a fire-time friendly-fire guard = a NEW task, not a
  resolver R-series fix. NOT blocking; tracked for decision.

---

## Verdict

**APPROVE.** R7 completes the telegraph read-and-react loop (paint → dodge → whiff) entirely on the
single-source `board.threats` with no recomputation — fully consistent with the V4 single-source
mandate. Placement, fires-only gating, source-filtering, and FireEvent shape all correct; 4 tests
cover the core + the three edge cases that matter. With R7, the R-series is fully landed
(R1/R3/R6/R6b/R4/R5/R8/R7).

**Remaining: the CONTRACT commit** — git mv geometry2d→geometry, delete the 1-D world
(`apply_damage`/`resolve_targeting`/`advance_projectile`/`LaneEnd`/`Orientation`/`RangeBand`/
`board.size`), un-suffix `_2d`/`heading8`/`range_band`/`direction_2d`, drop the `range_to_rangeband`
shim + `default_*` fns, and un-ignore the stale-fixture tests as real 2-D. Highest blast radius;
I'll give it the heavy review + re-verify the ignored tests return GREEN. Tracked against V2
checklist §8.

---

*Cross-ref: V4-at-R8 (`ff3728c`, `board.threats` = single-source paint that R7 reads); V4
(`072f1b7`, the no-second-path mandate R7 honors by reading not recomputing). R7 done @ `b72035b`.*
