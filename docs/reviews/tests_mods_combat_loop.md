# Review: #54 tests/mods.rs + #55 tests/combat_loop.rs — correctness/drift pass

Reviewer pass on the two tester-authored integration suites. Charge: verify they
lock CORRECT canonical behavior, not bake wrong expected values. Status: **both
APPROVE — every assertion validated against the canonical pipeline; no
baked-wrong values.** mods 11/11 + combat_loop 4/4 green.

## #54 tests/mods.rs — every expected value re-derived, all correct

`damage_action` helper uses `band_falloff: Some(false)` + optimal PointBlank, so
the primary lands raw intact regardless of band — the expected hull deltas below
are therefore pure (raw − armour), which I re-computed:
- **m1 flak**: raw4 armour-0 t → 10... (h5) → 1. Splash 1 to each neighbour via dummy_weapon (falloff off): self-Player a 5→4, foe 5→4. Faction-blind. Correct. The shield-mediated variant: splash arrives at foe FROM hit cell (Aft rel. to foe, bow=Fore → STERN); stern armour 1 absorbs the 1 → foe stays 5. Direction reasoning + facing_zone(BowOn{Fore}, Aft)=Stern verified. Correct.
- **m2 twin_linked**: 3×2 passes = 6, armour-0 h10 → 4; heat once (3 not 6); cd once (4). Re-target variant: pass1 kills t1 (3v3), pass2 RE-RESOLVES to t2 → 5→2. Validates the cost-once + between-pass re-resolve semantics I checked in 1619bac. Correct.
- **m3 incendiary/emp**: stern armour 5 absorbs raw2 → hull unchanged, HullBreach/SystemsOffline land ON CONTACT. Validates rider-on-contact ruling. Correct.
- **m4 targeting_laser**: shot1 raw2 → 8 + TargetLock; shot2 raw2 doubled by lock → 4 → 8→4, lock consumed. Validates target-lock 2× pipeline order + consume-once. Correct.
- **m5 precision_core**: kill (raw9 v h3, overkill) → cd recharged to 0; non-lethal (raw9 v h10 → survives 1) → cd = cd_max(5). BOTH branches tested. Validates the any-lethal recharge + the run_action cooldown-ordering I scrutinized in 1619bac. Correct.
- **m6**: enemy-fired twin_linked identical (faction-agnostic). Correct.
- **m7 (the sharpest)**: Marksman(+1@Long) on attacker a. Primary at Long: raw4 +1 = 5 → 10→5. Flak splash to n: 1 with NO +1, because the splash's apply_damage uses atk_cell = HIT CELL (t, no Marksman), not a. So the subsystem modifier hits PRIMARY only. This is exactly the attacker-side-modifier (#67) + flak-uses-hit-cell-as-attacker (1619bac) interaction — a wrong test would bake splash=2 by mis-applying Marksman; this gets it RIGHT. Correct and valuable.

Net mods.rs: the expected values match the canonical pipeline AND specifically lock the exact mod semantics I verified in the 1619bac source review — implementation and test agree on canonical behavior. No drift.

## #55 tests/combat_loop.rs — property assertions, correctly framed

This is a structural/termination suite, and (correctly) asserts PROPERTIES, not
brittle exact arithmetic:
- player_clears_two_enemies: terminates < 32 rounds, enemies==0, player survives.
- idle_player_dies: find_player_id → None, cell cleared.
- lone_player_survives_a_no_target_round: empty queue round doesn't kill/panic.
- board_consistent_across_rounds: no orphan state.
The `rounds < 32` bound + assert-it's-reached is the termination guarantee (the
same property the #65 stalemate stresses at a higher tier). The winnable-board
test uses hand-built armed beams at bearing positions — a CONTROLLED winnable
scenario by construction, not a baked-wrong balance assumption (the author owns
the loadout/positions). Legitimate integration-test design.

## Note

#55's winnability is scenario-controlled (hand-built bearing beams) and is NOT
in tension with the #65 generated-tier-2 stalemate: combat_loop proves the LOOP
terminates on a winnable board; #65 shows a GENERATED board may not be winnable
in the bound (balance). Different claims, both correct. No baked-wrong value in
either suite.
