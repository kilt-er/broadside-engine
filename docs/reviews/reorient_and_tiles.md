# Review: #52 reorient (83503f6) + #53 ability tiles (bd6e47e)

Reviewer soundness pass, overnight drive. Both bruce-eyeball-pending; fast
correctness pass. Status: **APPROVE both.** No findings. (333 lib tests pass per
the commits.)

## 83503f6 — #52 reorient 90°-to-broadside + faster tween — APPROVE

THE KEY CLAIM (bin-local, no AI/resolver drift): CONFIRMED.
- `git show --name-only` = broadside.rs + loft_gpu.rs only. grep "resolve" = NOT
  touched. So flip_orientation, decide_enemy_action, and the enemy reorient path
  are untouched — zero AI drift. Enemy reorient still uses its own action def.
- Control semantics: the `Intent::ReorientFlip` arm reads `player.orientation`,
  picks `ReorientTo::Broadside` if currently BowOn{..} else `ReorientTo::BowOn`,
  and overrides ONLY the synthetic action's REORIENT effect
  (`action.effects = vec![Effect::REORIENT { to }]`); name/cost/targeting still
  from the synthetic. This is exactly the design I pre-flagged in the #52 decline:
  the control computes the explicit target stance rather than reusing Flip (which
  toggles only bow direction and can't express the bow-on↔broadside toggle). The
  engine REORIENT arm handles both targets (verified previously: Broadside →
  Orientation::Broadside, BowOn → BowOn{Fore}).
- Reaches Broadside and back (bow-on→Broadside→bow-on). ReorientTo::BowOn always
  lands BowOn{Fore}, so reaching bow-Aft via control is DEFERRED — documented in
  the commit, acceptable.
- Tween REORIENT_SECS 0.45→0.28 (loft_gpu); shortest-path interp already present,
  now a clean 90° (no 180° over-spin). Matches bruce's "snappier, stop perpendicular."

## bd6e47e — #53 Shogun ability tiles — APPROVE

- Scope: hud.rs + bin assembly only. No resolve/catalog/types.
- READ-ONLY render input (the cross-teammate-consistency check): `push_ability_tiles`
  / `push_cooldown_row` take `&[AbilityTile]` (a data snapshot) and emit DrawCommands
  — NO `&mut board` / `&mut ship` / `.queue` mutation anywhere. The HUD is a pure
  state consumer; the resolver-owns-state invariant holds.
- Data sourcing correct: AbilityTile = name + cooldown_max (catalog Action defs) +
  live `cooldown` (Ship::cooldowns — the resolver's own cooldown map). Bin assembles
  (only the bin has Content + the player Ship). Mounts→slots 1/2/3, cards→5/6/7,
  matching the input.rs key→action map I reviewed (D1-3 / D5-7).
- Blurb synthesized from archetype + first damage/heat effect (first-pass; one-line
  swap to action.description when content adds it). Placeholder, no correctness issue.
- Bin edit limited to #53 helpers + Playing-branch calls + imports; pre-existing fmt
  drift untouched (bin no-touch protocol respected).

## Incidental clear (drift scan the lead asked for)

6ec5cdb "AI telegraph behavior-locks B2/B4/B7" is `#[cfg(test)]`-ONLY (202 lines,
all #[test] fns calling super::decide_enemy_action) — ZERO change to the AI
scoring/behavior. Tester-lane coverage of the AI I already audited (#9), not a
behavior change. No drift. The other recent resolver-touching commits since my
last pass (b81b073 onDamageDealt, 1619bac mods, f6d9458 tick parity test) are all
already reviewed/approved in docs/reviews/. Nothing unreviewed-and-drifting on the
mechanics surface.
