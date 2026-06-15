# V5-tail (R5) + V4-at-R8 review — `advance_projectile_2d` + ThreatMap single-source (`1969040`)

**Status:** ✅ **APPROVE (both gates), pending a targeted test-run confirmation** (6 ap2d_* + the
pt2d_* set — held for a disk slot while the renderer build runs; static review complete). This is
the **last forward V-gate** — it closes the single-source spine end-to-end. Bundled commit
(R5+R8 in one resolve.rs commit; tooling couldn't cleanly split — lead-approved).

---

## V4-at-R8 — ThreatMap single-source (the load-bearing proof) — APPROVE

The blueprint "single best idea": painted threat cells must equal where the shot lands, because
they're computed by the SAME targeting spine. Verified `paint_threats` (`resolve.rs:397`):

- **SAME spine:** line 423 calls `resolve_targeting_2d(&action, board, enemy_pos)` — the literal
  same function the AI elects with (`ai.rs:101`) and `fire_player_queue` fires with. Not a
  reimplementation.
- **SAME action resolution:** lines 415-421 resolve the queued id via `content.action(id)` then
  the `resolver_ai_move` fallback — explicitly "exactly as fire_player_queue does." A queued
  synthetic-move is handled identically at paint-time and fire-time.
- **Resolves the QUEUED action**, called after the whole `decide_enemy_action` loop
  (`run_world_phase:368`), so the painted set reflects the fully-telegraphed board and matches
  what fires next phase (telegraph-one-turn-ahead).
- **Clear + rebuild each phase** (line 398) — stale telegraph cleared; consistent with the
  `Board.threats`-is-transient contract (NOT snapshotted, A3.1).
- **`Threat.source = enemy_pos`** (invariant A) for the renderer beam + R7 whiff attribution.
- **Self-paint skip** (line 429): a queued maneuver resolves to the firer's own cell, correctly
  not flagged as a threat against another cell.

**The equivalence test is REAL, not a tautology** (`pt2d_threat_set_equals_resolve_targeting_2d_single_source`,
`resolve.rs:5599`): it computes `fired = resolve_targeting_2d(&weapon, &board, pos)` **directly**,
runs `paint_threats` (which calls `resolve_targeting_2d` via the paint path), and asserts
`painted == fired`. Both funnel through the one function but are invoked **independently** — so the
test would catch a divergent reimplementation, a wrong action-resolution, or a paint-loop off-by-
one. Supporting tests are well-chosen: `..._off_the_ray` (painted set tracks the actual bearing →
empty when the player isn't on the ray, the deterministic basis for R7's whiff) and
`..._clears_stale_threats_each_pass` (the rebuild contract).

**SINGLE-SOURCE SPINE CLOSED END-TO-END:** AI elects via `resolve_targeting_2d` → R8 paints via
`resolve_targeting_2d` → firing resolves via `resolve_targeting_2d`. One cell-selection function,
three call sites, provably equal. The V4 mandate ("NO second targeting path for telegraphs") is
fully satisfied — V4, V4-at-C1, and now V4-at-R8 all green. This is the correctness foundation the
whole telegraph/dodge-whiff design rests on.

---

## V5-tail — `advance_projectile_2d` (R5) — APPROVE

Side-by-side vs the 1-D `advance_projectile` (`resolve.rs:756`) — semantics preserved 1:1:

| Aspect | 1-D | 2-D (`advance_projectile_2d`, `:831`) | |
|--------|-----|----------------------------------------|---|
| find + speed loop | `position(id)` + `for 0..speed` + re-find each step | identical | ✓ |
| step | `cell ±1` per `heading` (checked) | `grid::offset(cur, heading8, 1)` | ✓ |
| off-grid removal | checked-None OR `>= size` → `retain(!=id); return` | `offset`-None → `retain(!=id); return` | ✓ |
| position update | `cell = new_cell` | `pos = new_pos; cell = new_pos.to_index()` (invariant-A mirror) | ✓ |
| impact check | non-owner occupant at `new_cell` | non-owner occupant at `new_pos` (`ship_at`) | ✓ |
| payload | DAMAGE→`apply_damage`, APPLY_STATUS→`add_status`, else `{}` | DAMAGE→`apply_damage_2d`, APPLY_STATUS→`add_status`, else `{}` | ✓ |
| remove after impact | `retain(!=id); return` | identical | ✓ |

Lifetime / trail (step-by-step, stop at first non-owner) / impact / bounds / removal all match.
Payload impact routes through `apply_damage_2d` — the single 2-D damage sink (consistent with V5,
not a parallel path).

**One deliberate correctness IMPROVEMENT (documented `:825-830`):** the 1-D version passed
`impact_cell` as BOTH target and attacker, so `direction_to(c,c)` always read the Bow face — a
latent 1-D bug. The 2-D version computes `from = grid::offset(new_pos, heading.opposite(), 1)`
(clamped to `new_pos` if off-grid), so the projectile's `incoming_from` is the hull face it
actually flew at. This is a fix, not a path-behavior change; correctly feeds step 4 of
`apply_damage_2d`.

**Live wiring:** the ordnance phase (`resolve_round` phase 2, `resolve.rs:309`) now calls
`advance_projectile_2d`; the 1-D `advance_projectile` is additive-retained for its fixture tests
(documented `:753`), dead-for-live until CONTRACT. 6 `ap2d_*` tests (`:5431-5546`).

---

## Verdict

**APPROVE both gates.** V4-at-R8: `paint_threats` is the same `resolve_targeting_2d` spine as
election + firing, with a real (non-tautological) equivalence test — the single-source spine is
closed end-to-end. V5-tail: `advance_projectile_2d` is a faithful 1:1 port with a documented
impact-direction fix, routing through the single 2-D damage sink, live caller switched. **Pending:
a targeted `ap2d_*` + `pt2d_*` test-run confirmation once I get a disk slot (renderer build has
priority); static review is complete and clean.**

**Canary note (#41):** independently satisfied — the tester's two-direction proof (zero-damage
1-D-driver diagnostic + the positive `player_fires_and_kills_one_enemy_in_2d` probe `4c46fc4`)
shows the 2-D campaign IS winnable and the `generated_spawn_pool` stalemate was the 1-D test
driver, NOT the engine. Consistent with my #24 classification (harness-not-engine). No masking.

**Remaining to CONTRACT:** R7 (hit:false dodge-whiff) is the last R-body; then the CONTRACT commit
(git mv geometry2d→geometry, delete the 1-D world, drop `board.size`, un-suffix the `_2d`/`heading8`/
`range_band`/`direction_2d` names, remove the `range_to_rangeband` shim + `default_*` fns) — the
highest-blast-radius commit, a major V2-continuation review. Tracked against V2 checklist §8.

---

*Cross-ref: V4 (`072f1b7`, single-source mandate) + V4-at-C1 (`5bbff3e`, AI gate) — this closes the
trio; V5 (`970361b`, apply_damage_2d — the sink R5 impact routes through). V5-tail + V4-at-R8 done
@ `1969040`.*
