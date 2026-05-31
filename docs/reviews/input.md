# Review: src/input.rs (key->Intent mapping + SS turn semantics)

Reviewer audit (task #9, extended pass). Rust-native (Phase-1 controls). Audited
for mapping correctness + the Shogun-Showdown turn model the resolver expects.
Status: **APPROVE.** No findings.

## Verified

- **key_to_intent** — Left/Right -> Move (instant), Tab -> ReorientFlip, V -> Vent, D1-3 -> QueueAction of mounts[N].weapon (gated: None if N>=mounts.len()), D5-7 -> PlayCard of card slot N (gated: None if slot absent), R/Space -> CommitTurn, Enter -> Restart. Gating via `mount_action`/`card_at` returning Option is the correct defensive shape — pressing D3 with two mounts is a no-op, not a panic.
- **intent_to_action_id** — QueueAction -> the id; Move/Reorient/Vent -> their `__`-prefixed synthetic ids; PlayCard/CommitTurn/Restart -> None (handled directly, not queued). The `__` prefix on synthetics correctly avoids collision with catalog action ids — and ties to the classes.rs invariant that real Signature actions do NOT use `__`.
- **SS turn semantics** (cross-checked against resolve.rs) — instant intents (move/flip/vent) go through `apply_instant_action` + `run_world_phase`; queueing intents push to queue + `run_world_phase` (queue not fired); CommitTurn -> `fire_player_queue` + `run_world_phase`. Every input advances time exactly once. Matches the resolve_round/run_world_phase split documented in resolve.rs and verified in docs/reviews/resolve.md.
- **Card play** — PlayCard validates+consumes via try_play_card THEN pushes synthetic_card_action_id manually (the resolver's BOARD arm applies the effect). Card bookkeeping stays out of the resolver. Consistent with cards.rs.

## Notes

- DemoContent (the concrete Content impl) owns the subsystem Installations, FieldKitRegistry, CardCatalog, and class-signature registry. Single content-side state home, consistent with the subsystems/cards module rationale. The `register_class_signatures` step inserts the placeholder signatures into the action registry — see the docs/reviews/subsystems.md `overcharge` id-collision flag for the one naming hazard there.
