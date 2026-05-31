# Review: src/resolve.rs vs engine/resolve.ts

Reviewer audit (task #9). Canonical reference: `_drive_pull/broadside-engine/engine/resolve.ts`.
Status: **APPROVE with 2 noted divergences** (1 edge-case behavioral, 1 intentional enhancement). No hard fails outstanding (bearing_direction #96 + dead-signature #97 already fixed and verified).

## Verified faithful (line-by-line vs TS)

- **Damage pipeline order** (`apply_damage`, resolve.rs:638-710) — band falloff -> subsystem modifiers -> target-lock 2x (consumes status) -> directional shield (charge then armour) -> hull subtraction -> emit -> destroy. Exact match to TS applyDamage (resolve.ts:139-163). The falloff-disable predicate `effects.any(DAMAGE { band_falloff: Some(false) })` matches TS `effects.some(e => e.kind==="DAMAGE" && e.bandFalloff===false)`; None and Some(true) both keep falloff on.
- **run_action gate cascade** (resolve.rs:346-407) — lockout+heat gate, cooldown gate, resolve_targeting, "nothing bore" arc gate, effect loop, post-effect heat/cooldown bookkeeping, onDamageDealt emit. Matches TS executeQueue body (resolve.ts:54-70) gate-for-gate.
- **Four-phase round** — Rust splits TS resolveRound into resolve_round (phase 1) + run_world_phase (phases 2-4) for the SS turn model, composed in the same order: player queue -> ordnance advance -> enemy phase -> end_of_turn. Order preserved.
- **tick_statuses** (resolve.rs:957-980) — hullBreach deals 1/status pre-decrement, then all durations -1, then retain duration>0. Behaviorally equivalent to TS (resolve.ts:319-328): Rust sums breach hits and subtracts once vs TS per-status subtract-in-loop; same total, same end state on lethal.
- **advance_projectile** (resolve.rs:428-483) — steps speed cells, off-lane removal, non-owner impact applies DAMAGE/APPLY_STATUS payload via dummy_weapon (falloff off), removes on impact. Matches TS advanceProjectile (resolve.ts:233-250).
- **destroy** (resolve.rs:998-1025) — ReactorBreach 2-splash to both neighbours via dummy_weapon. Matches TS destroy (resolve.ts:334-344).
- **end_of_turn** (resolve.rs:491-518) — cooldown decrement, heat -1 floored, lockout clear, tick_statuses, then OnTurnEnd emit. Matches TS endOfTurn (resolve.ts:254-264). on_turn_end content hook is an additive Phase-2 extension (pre-approved).

## Divergence 1 (edge-case behavioral — FLAGGED to resolver)

`run_action` guards the post-effect heat/cooldown bookkeeping AND the onDamageDealt emit behind `if let Some(post_cell) = find_cell_by_id(ship_id)` (resolve.rs:394-405). The TS runs both UNCONDITIONALLY after the effect loop (resolve.ts:65-69), with no liveness check on the firing ship.

Consequence: if a ship destroys ITSELF during its own action (self-destruct, or a ReactorBreach splash from a kill it caused rebounding lethally — contrived but possible), TS still emits onDamageDealt with the (dangling) source ship; Rust skips the emit. A subsystem subscribed to onDamageDealt would fire in TS, not in Rust.

Severity: low (requires a ship to die from its own queued action). Not a correctness hazard for current content. Documented so it's a known, deliberate-able choice rather than silent drift. Resolver's call whether to match TS (emit with last-known cell) or keep the guard.

**Disposition (team-lead):** bundled with the pending ram-collision design decision — both live in the same self-damage/self-destruct path (ram currently self-rams the operator, one way a firing ship can self-destruct). Resolver addresses the onDamageDealt-emit fidelity together with ram semantics once bruce rules, avoiding a second touch of that area. If it proves independent of ram, it's a standalone low-pri resolver fix. Latent/non-blocking either way.

## Divergence 2 (intentional enhancement — no action needed)

`detect_chain` (resolve.rs:1727) is LIVE: returns `destroys_this_window >= 2`, backed by Board::destroys_this_window reset at each window boundary (fire_player_queue, run_world_phase ordnance pass) and incremented in destroy(). The TS detectChain (resolve.ts:346-349) is a stub returning `false`. So onChainKill fires in Rust where it never did in TS. This is the intended completion of a TS TODO, not drift.

## Stubs filled (TS TODO -> Rust real impl)

resolve_self_move, resolve_target_move, decide_enemy_action all replace TS stubs. These are content's movement/AI MODEL, NOT TS ports (TS bodies were stubs) — verifiable only against the analysis-doc prose, not resolve.ts. The ram/throw self-collision semantic is queued for bruce (see #97 follow-up).
