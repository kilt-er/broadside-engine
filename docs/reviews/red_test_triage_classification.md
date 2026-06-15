# Reviewer safeguard — committed-RED integration test classification

**Task:** independently sanity-check the tester's `#[ignore]`-vs-fix calls on the committed-RED
integration tests (lead's audit found the suite RED, latent since ~R3 because commits verified
`--lib` only). The risk: `#[ignore]`-ing a test that is catching a **real 2-D bug** and thereby
burying a regression. **Verdict per failing test below.**

**Method (not trusting descriptions):** classified against **committed HEAD** (`55954f8`), run in a
**throwaway detached `git worktree`** (`git worktree add --detach … HEAD`) so the shared tree's
uncommitted R6 WIP — which currently does not even compile (`resolve.rs` references
`input::SYNTHETIC_MOVE_UP`/`DOWN` not yet added) — did NOT contaminate the result. Worktree
removed after. For each test: ran it, read its asserts, traced the failure to root cause, and
checked git history for when it went red.

---

## Root cause (shared by ALL failures) — STALE 1-D FIXTURES, not a 2-D engine bug

Every failing test builds ships with a **distinct 1-D `cell`** (its intent: 0, 1, 2, …) but a
**constant transitional `pos: Pos::new(0,0)`**. That `pos:(0,0)` was filled by the A3.1 EXPAND
literal-sweep (`e2d0403`) — the additive commit I reviewed at V2, where every construction site
got the transitional default because **nothing read `pos` yet**. The tests stayed green then.

They went RED at **R3 (`59c0baa`)**, which switched the firing path to read `ship.pos` via
`resolve_targeting_2d`. Now every ship sits at grid `(0,0)` → targeting is degenerate (all
attackers and targets co-located) → **no shot connects**. Every failure below is a downstream
symptom of "no shot fired," not a defect in the 2-D engine:

- `resolve_targeting_2d` itself is correct — its 8 `rt2d_*` unit tests pass against a *properly
  built* 2-D board (verified at V4).
- The heat economy, the telegraph mechanism, and the damage pipeline are **untouched** by R3
  (confirmed below).

This is the **expected, designed** consequence of the expand→migrate→contract sequence: test
fixtures are the last consumer to migrate. Fixing them is **tester #20** ("proper 2D-fixture
rewrite of the run_action tests") + the analogous combat_loop/run_loop fixture rewrite. The 1-D
`cell` field will be deleted at CONTRACT; the fixtures must move to real `pos` before/with that.

---

## Per-test verdict

| Test | Panic | Root cause | Verdict |
|------|-------|-----------|---------|
| `combat_loop_player_clears_two_armed_enemies` | :188 "must terminate" | player beam can't hit (all at (0,0)) → enemies never die → 32-round timeout | **SAFE-TO-IGNORE** (stale fixture) |
| `combat_loop_player_death_clears_cell_and_is_detectable` | :219 "should kill idle player" | enemy beams can't hit player (stacked) → player never dies | **SAFE-TO-IGNORE** (stale fixture) |
| `telegraph_persists_in_enemy_queue_between_world_phases` | :367 "fires on NEXT phase" | enemy *decides*/telegraphs fine, but the telegraphed shot resolves on the (0,0)-degenerate board → no hull drop | **SAFE-TO-IGNORE** (stale fixture; telegraph mechanism intact — see note) |
| `enemy_fires_and_holds_when_in_band_does_not_march` | :411 | TWO causes (see below) | **SAFE-TO-IGNORE** (stale fixture + the V4 1-D-AI-gate caveat) |
| `pulse_laser_sustained_fire_overheats_into_lockout` | :546 `None != Some(5)` | no shot fires → no heat accrues → no lockout | **SAFE-TO-IGNORE** (stale fixture; heat economy intact — see note) |
| `pulse_laser_three_shot_alpha_locks_out_instantly` | :585 | same — no shot fires, no heat | **SAFE-TO-IGNORE** (stale fixture) |
| `full_campaign_played_to_victory_sets_victorious` (run_loop) | "clears the haul" | campaign ships spawn at `(0,0)` via the not-yet-pos-wired spawn builders → player can't clear → victory never sets | **SAFE-TO-IGNORE** (transitional spawn-pos) |

**NONE is a real 2-D engine bug.** All are the one stale-fixture/transitional-spawn class.

---

## The two the lead flagged — answered precisely

### `pulse_laser` overheat — "heat economy wrong in 2D, or firing geometry?"
**Firing geometry, NOT heat economy.** `git log -L /fn run_action/` confirms R3's only change to
`run_action` was the targeting-call swap (the `resolve_targeting_2d` shim reviewed at V4); the
heat bookkeeping (`ship.heat += action.cost.heat; if ship.heat >= ship.heat_max { locked_out =
true }`) is byte-identical. The catalog `pulse_laser.cost.heat == 2` is also unchanged. The test
fails because the dummy target sits at `pos:(0,0)` same as the firer, so `resolve_targeting_2d`
returns empty, the `requires_arc.is_some() && cells.is_empty()` gate returns **before heat is
spent**, and heat never climbs. Give the dummy a real distinct `pos` (Close band) and the curve
returns. **The #73 heat-gate is intact; only the fixture is stale.**

### `enemy_fires_and_holds_when_in_band_does_not_march` — "1-D gate transitional, or deeper bug?"
**Transitional — and it's BOTH the stale fixture AND my V4 1-D-AI-gate caveat, compounding:**
1. Stale fixture: e1/e2/player all at `pos:(0,0)`, so even a correctly-decided shot resolves on a
   degenerate board.
2. The V4 caveat: `decide_enemy_action` *decides* via the **1-D** `resolve_targeting` on `cell`
   (which IS distinct: 5, 6) — so the AI's decision reads 1-D geometry — then *fires* via 2-D
   `resolve_targeting_2d` on `pos` (stacked). Two different geometries.

So this test is the live manifestation of the exact desync I flagged at V4. It is **not a deeper
2-D engine bug** — it's the AI-gate convergence (V4-at-C1) plus the fixture, both pending. **It
will only pass once (a) the fixture moves to real `pos` AND (b) C1 routes `decide_enemy_action`
through `resolve_targeting_2d`.** Worth a tracking note linking it to the C1 convergence gate.

---

## CRITICAL proviso (the lead's "don't lose #71–#76")

These ignores MUST be **tracked-to-restoration**, not permanent. The behaviors they pin are real
combat-feel guarantees that must come back as **2-D versions**:
- `#67` telegraph-one-turn-ahead (`telegraph_persists`)
- `#71` in-band enemy fires-and-holds (`enemy_fires_and_holds`)
- `#73` pulse_laser heat-gate (both `pulse_laser_*`)
- campaign winnability (`full_campaign_…`)

**Recommendation:** `#[ignore = "stale 1-D fixture — restore at 2-D fixture rewrite (#20) / C1 AI-gate convergence; tracks #22"]` on each, with the tracking task (#22) owning the restoration. Do NOT let any of these be deleted or left permanently ignored — each must be re-asserted on a real 2-D board. I will re-verify at the fixture-rewrite commit that they come back GREEN (not silently dropped), and that `enemy_fires_and_holds` specifically is restored as part of the V4-at-C1 convergence check.

---

*Method note: classified against committed HEAD via a detached worktree because the shared tree
carried non-compiling R6 WIP — see [[feedback_shared_tree_clippy_attribution]] (a build state may
be a teammate's uncommitted WIP; verify against HEAD). Cross-ref: V4 (`072f1b7`, the 1-D-AI-gate
caveat that `enemy_fires_and_holds` manifests); tester #20 / #22 (fixture rewrite + triage).*
