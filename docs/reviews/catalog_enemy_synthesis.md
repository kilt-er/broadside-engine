# Review: src/catalog.rs enemy synthesis + patrol seam (c4c85d2, bc9d25e)

Reviewer audit. Catalog-driven `EnemyDef -> Ship` materialization + the
patrol-tier seam. Status: **APPROVE.** No findings. Patrol seam directly
addresses the runs.md Phase-3 dormancy flag.

## Enemy synthesis (c4c85d2) — verified

- **Mounts** from `def.weapons`: each weapon resolved via display-name->id lookup (or passed through if already snake_case), arc pulled from the resolved action's `requires_arc` (default Forward), unmapped weapons skipped with an eprintln warning rather than crashing. Faithful.
- **Traits** via `trait_from_str` filter_map — unmapped strings skipped (forward-compatible: a future catalog trait won't crash the load). Fixes the real latent issue the commit calls out: pre-synthesis, every non-boss enemy went through fallback_ship_for_spawn (hull 3, no traits), so decide_enemy_action's Pursuit/BurnHard/Agile nudges were implemented-but-dead. Now enemies carry their canonical traits and those code paths actually fire.
- **hp_override still wins** (`spawn.hp_override.unwrap_or_else(|| select_hull(...))`) — consistent with boss_ship_for_spawn / fallback_ship_for_spawn.
- **enemy_shield_default** (bow 1 / stern 0 / flanks 1) — soft-stern, the flank-from-behind invariant the analysis doc rewards; deliberately separate from the player default so enemy/player armour can diverge in tuning.

## trait_from_str mapping — verified COMPLETE vs the Trait enum

Normalizes by stripping non-alphanumerics + lowercasing, so "Burn-Hard" / "burn_hard" / "Burn Hard" / "Reactor Breach" all resolve — faithful to the canonical Title-Case-with-hyphens enemy-def strings. All 10 Trait variants mapped (Pursuit, Agile, ReactorBreach, BurnHard, Anchored, EliteAgile, EliteAnchored, TwinLinked, ReactiveShield, Voidtouched), cross-checked against types.rs::Trait — no variant missed, no extras. Unknown -> None (skipped). Tested.

## Patrol seam (bc9d25e) — verified behavior-NEUTRAL

`select_hull(def, _patrol_tier) -> def.hull` ignores the tier today, returning base hull at every tier — confirmed identical to pre-seam behavior. The value of the commit is the SIGNATURE: `patrol_tier: u8` is now threaded through enemy_ship_from_catalog_at_tier -> ship_from_enemy_def_at_tier -> select_hull, so the scheduled tier-scaling (`if patrol_tier >= 5 { def.hull5 }` + wire patrol_tier -> Board.patrol at the encounter builder) is a one-line edit with no signature churn. This is the right response to the runs.md dormancy flag: leave the seam now, avoid a later signature-breaking retrofit. The non-tier path (enemy_ship_from_catalog == tier 1) preserves every existing caller.

## Tests (9 green)

trait_from_str display-string mapping, synthesized-enemy-carries-traits-and-mounts, real-catalog-synthesizes-canonical-enemies-with-traits. The last one exercises the actual assets/broadside.catalog.json enemy defs end-to-end — strong.
