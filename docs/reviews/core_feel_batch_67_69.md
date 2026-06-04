# Review: core-feel batch — #68 close-move / #69 capital-boss / #67 telegraph

Reviewer correctness/drift pass. Status: **all APPROVE.** No drift, no baked-wrong
tests, dispatch routes correctly, render read-only. 15/15 AI lib tests green.
(run_loop integration test not run locally — live-bin file lock from bruce's
re-run; lead build-verified full HEAD green incl. run_loop.)

## #68 AI close-move (12e8cf8 + 078bb8a, resolve.rs) — APPROVE

THE FIX: catalog enemies carry NO mounted DISPLACE_SELF (mounts = def.weapons,
combat only), so the old maneuver helper scanned mounts for a move that isn't
there → empty queue → "enemies never move" (bruce's bug). Now the AI queues a
SYNTHETIC lane-relative move (__move_left/__move_right toward the player), served
by the resolver itself:
- `resolver_ai_move` (resolve.rs:1351) materializes a ZERO-HEAT THRUST-1 move with
  `direction: Some(LaneEnd::…)` (absolute lane dir → closes regardless of bow
  orientation, fixing the old orientation-decline trap). SELF pattern, requires_arc
  None → always resolves.
- It's served via run_action's UNKNOWN-ACTION fallback (resolve.rs:241:
  `None => match resolver_ai_move(action_id)`), so catalog enemies (which don't
  register the synthetic in Content) still get the move materialized and run
  through the SAME pipeline. Does NOT bypass run_action — clean extension of the
  TS `if(!a) continue` to `if(!a){ try resolver move; else continue }`. No drift.

NO canonical drift: decide_enemy_action's scoring-loop gates (cooldown/heat/lockout/
heat-budget/arc+band/friendly-fire) unchanged. The locked_out gate moved to the
CALL SITE: step-5 fallback is `if !locked_out && queue_purposeful_maneuver(...)`
(overheated → skip move → vent), step-4 reposition is unguarded — sound asymmetry
because the synthetic move is ZERO-HEAT (run_action's `locked_out && heat>0` never
filters it), so a locked-out enemy can still make the free reposition at step 4
while step 5 deliberately prefers venting over drifting when it can't fire.

TEST CHURN locks the FIX, not baked-wrong: the 4 old AI tests asserted EMPTY-QUEUE
camp — that was pinning the BUG (mount-scan finds no move). The new assertions
(e.g. ai_closes_via_synthetic_move_when_cannot_fire → queue == [__move_left] toward
an aft player) lock the close. Correct re-baseline, not a hack around the new code.
15/15 AI tests green.

## #69 capital→boss (3610538 runs + 57f8b9d bin) — APPROVE

Fixes the popgun bug: `capital_spawn` writes the capital's display name as class_id,
which previously hit the hull-3 `fallback_ship_for_spawn` for every capital except
"warlord". Now `synth_enemy_for_spawn` (bin:510) dispatches:
warlord → boss_ship_for_spawn; is_capital_spawn → capital_boss_ship_for_spawn
(armed boss baseline); catalog enemy → enemy_ship_from_catalog_at_tier; else
fallback. Precedence correct.

BOARD + REWARD AGREE (the key check): BOTH call sites — broadside.rs:746
(board-build closure) and :781 (salvage reward calc) — route through the SAME
synth_enemy_for_spawn, so the ship a capital fights as is identical to the one its
reward is computed from. The popgun bug was exactly this kind of path divergence;
single-function routing guarantees agreement. is_capital_spawn matches catalog
capitals by name (tested).

## #67 telegraph render half (13f20bc, hud.rs) — APPROVE

(b9268c4 fire-then-decide turn-model concept reviewed earlier.) The render half is
READ-ONLY render input: push_enemy_telegraph_stack + compose_scene_tweened return
`Vec<DrawCommand>` from read-only `&Board`/`&LaneGeometry`/`&dyn SpriteRegistry`;
no &mut board/ship, no .queue mutation in the diff. TweenState is a read-only
visual-cell override map keyed by id (cell_for reads, never writes). The renderer
reads enemy.queue to draw the telegraph but never mutates it — resolver-owns-queue
invariant holds.

## Note

Could not run tests/run_loop locally: `cargo test --test run_loop` wants to relink
broadside.exe, which is locked by bruce's live re-run (os error 5, access denied) —
purely environmental, not a code issue. Relied on the lead's full-HEAD build-verify
(302 lib + clippy --all-targets green incl. run_loop) for that suite; ran the AI lib
tests directly (15/15 green) since they're lib-target, not bin-linked.
