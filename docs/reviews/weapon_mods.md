# Review: 7 weapon-mods via Action.r#mod dispatch (1619bac, #50)

Reviewer soundness pass. src/resolve.rs only (+443/-3). Status: **APPROVE.** No
blocking findings. 13 mod/modifier tests green (7 new, one per mod); full suite
281 lib + integration green per resolver, re-confirmed locally.

## EventBus no-re-entry invariant (the cross-teammate check) — PASS

`apply_on_hit_mod` is called DIRECTLY from the DAMAGE arm of `apply_effect`
(resolve.rs:807), right after `apply_damage` — a plain function call in the
effect path, NOT a bus subscriber. So it cannot trip the "no resolver re-entry
inside a bus callback / bus-is-stale-during-emit" invariant I pinned in the
#20-26 EventBus audit: that invariant is about calling resolver fns from inside a
registered hook callback, and this dispatch never touches the bus. The on-hit
mods call add_status / apply_damage (which emit hooks normally) — they EMIT, they
don't subscribe-and-re-enter. Correct, and the design comment explicitly cites
the invariant.

## precision_core cooldown-ordering (the flagged non-obvious bit) — PASS

The crux: a clean-kill recharges the action's cooldown to 0, but the canonical
post-effect `cooldowns.insert(lookup_id, cooldown_max)` would CLOBBER a recharge
applied during effects. The fix is correct:
1. `precision_targets` snapshots targeted cells holding a ship BEFORE effects (run_action ~397).
2. `precision_kill` = any of those is now empty, computed BEFORE the mutable `ship` borrow (avoids aliasing board.cells) (~437).
3. The SINGLE cooldown insert chooses 0-if-kill-else-cooldown_max (~451-452) — folded into the one insert, no clobber.
The on-hit arm for PrecisionCore is correctly a NO-OP (1066-1075) with a comment explaining the recharge lives in run_action post-bookkeeping. "Detect kill via pre/post cell occupancy" is the right approach (apply_damage returns no killed-flag to run_action). Borrow ordering sound.

Single-mod constraint makes this safe: r#mod is ONE id (commit: "single-mod-only per lead ruling"), so twin_linked + precision_core can't co-occur — meaning `precision_targets` computed from the FIRST-pass `cells` can't miss a kill on a twin_linked re-resolved second-pass cell (that combo is impossible). If r#mod ever becomes a Vec (the deferred autoloader-combo follow-up), precision_targets would need to also cover re-resolved pass_cells — flag for that future change.

## flak_burst faction-blind splash — PASS (matches ReactorBreach precedent)

±1 lane-neighbours of the hit cell, bounds-checked, `apply_damage(nc, 1, hit_cell, &dummy_weapon(), ...)` — full pipeline, shield-mediated, falloff off (dummy has band_falloff:Some(false)), faction-blind (no faction check — content's Unfriendly-Fire ruling). Hit cell not re-damaged; splash origin = hit_cell so the directional shield reads the burst direction. Structurally identical to destroy()'s ReactorBreach splash. Tested faction-blind.

## Riders land on contact — PASS

incendiary (HullBreach 3) / emp_charge (SystemsOffline 3) / targeting_laser (TargetLock 5) call add_status on hit_cell after the `is_some` pre-hit gate guarantees the shot CONNECTED — so they land even if the shield fully absorbed hull damage (content ruling). Tested explicitly (targeting_laser-through-full-shield). add_status on an empty cell early-returns, so a rider on a just-killed target is harmless.

## twin_linked — PASS

Effects applied twice (passes loop), cost/heat/cooldown paid ONCE (the single post-loop bookkeeping). Targeting RE-RESOLVED on pass 2 against the current ship cell + mutated board (so a first-pass kill shortens a spinal line, and a DISPLACE that moved the ship re-aims). pass_source = ship's current cell per pass. Matches content's cost-once + re-resolve ruling. Tested.

## Notes (non-blocking)

- Minor redundancy: `killed` is computed at the DAMAGE arm (805) and passed to apply_on_hit_mod, but only PrecisionCore's arm receives it and that arm is a no-op (the real kill detection is precision_kill in run_action via the same occupancy mechanism). So `killed` is currently dead for every arm. Harmless; kept for a uniform on-hit signature. Could drop the param if no future on-hit mod needs it.
- SCOPE flag (per the lead, NOT a gap): the autoloader turn-seam is unwired — resolver exposes the public `action_advances_turn()` but input.rs doesn't call it yet (deferred to the input/turn owner; no playable loop exercises it). Correct that the RESOLVER has no turn-advance gate to flip — advances_turn lives in the SS turn layer. So autoloader is mechanically inert until input.rs consumes the seam. Tracked, not a defect in 1619bac.

Net: the mod layer is sound — the precision_core ordering is correct, the on-hit dispatch respects the bus invariant, flak matches precedent, riders are contact-gated. APPROVE.
