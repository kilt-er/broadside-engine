# Review: #41 O1 diversify-or-fire AI (d9b7c53, resolve.rs)

Reviewer drift/correctness pass. Supersedes #40 (the inert-term comment) and
closes the AI-diversity-gap I flagged in the #9 decide_enemy_action audit.
Status: **APPROVE.** No drift, no hidden bug. 14 AI tests green incl. both
changed/added. resolve.rs only.

## 1. +6 diversity term is now load-bearing — CONFIRMED (this was the gap)

Before: the +6 was added INSIDE the scoring loop to every candidate of the same
enemy. Since `my_end_from_player` doesn't vary per action, it was constant across
that enemy's own candidates → never broke a tie → never changed the pick. That is
exactly the inertness I called out in #9 and content ruled in #40.

Now: it's used OUTSIDE the loop at step 4 as `my_end_uncovered` to GATE
fire-vs-maneuver (resolve.rs:1920-1951):
- can-fire AND end uncovered → FIRE (pressure a distinct end; optimal-position-on-
  a-distinct-end takes precedence over repositioning).
- can-fire but end already covered by an earlier-queued ally → try
  queue_purposeful_maneuver FIRST (avoid stacking redundant pressure); fall
  through to firing only if no closing move exists.
- can't fire → maneuver → reorient → vent.
The term genuinely drives behavior now. Correct fix.

## 2. No canonical-mechanics drift — CONFIRMED

The scoring loop's gates are UNCHANGED: cooldown (1835), heat/lockout (1841),
heat-budget >heat_max+1 (1844), arc/band via resolve_targeting (1852),
friendly-fire filter (1866-1873). Only the POST-loop branch (what to do with the
best candidate) changed. The AI still pushes action ids onto the queue that
fire_player_queue runs through the unchanged pipeline — it never bypasses it.
covered_ends still built only from earlier-queued enemies in initiative order
(my prior audit's correctness point holds).

## 3. The flagged orientation-derived-move constraint — SOUND first-pass, not a hole

queue_purposeful_maneuver (resolve.rs:2024) queues a DISPLACE_SELF move ONLY when
the enemy's orientation-derived step closes toward the player:
- `move_end` = BowOn{Aft}→Aft else Fore — EXACTLY mirrors resolve_self_move's
  `direction: None` branch (verified against my resolve.md audit). Consistent.
- if `move_end != toward_player` → return false → caller's reorient fallback flips
  the enemy to face the player first; next turn the move closes.
This is the faithful workaround for the real limitation (the id-based queue can't
parameterize per-decision move direction without an r#mod→Vec-style types change,
correctly scoped out). Declining-then-reorient produces correct emergent pursuit
over 2 turns instead of 1 — slower, not wrong. Documented honestly. Acceptable.

## 4. Test changes lock CORRECT behavior — CONFIRMED (not baked-wrong)

- `ai_falls_back_to_movement_when_nothing_bears` updated: enemy now bow=Aft so the
  fallback is a REAL close (move_end Aft == toward_player Aft). The old version had
  the enemy facing away, where the new purposeful-maneuver correctly DECLINES — so
  the old assertion (generic drift) would now be wrong. Updating it to the bow=Aft
  closing case is the O1-correct fix, not a baked-around-the-new-code hack: it
  tests that a properly-oriented enemy closes.
- `ai_o1_repositions_instead_of_redundant_fire_on_covered_end` (new): a covered-end
  enemy that CAN fire chooses the close instead. This is precisely the behavior the
  formerly-inert +6 now drives — the test locks the actual gap-fix, correct.
Both green; 14 AI tests total pass.

Net: the diversity term that was inert (my #9 flag, content's #40) is now correctly
load-bearing, the gates are intact, the orientation-move constraint is a sound
documented first-pass, and the tests lock the real behavior. APPROVE — supersedes #40.
