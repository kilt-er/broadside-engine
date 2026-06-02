# Capital-ship combat distinctiveness — design spec

**Status:** PARKED for bruce (balance/taste review). Not self-implemented.
**Author:** broadside-content (#63 follow-up).
**Scope:** the runtime capital → `Ship` synthesizer that replaces "every
capital spawns as the warlord template (hull 14, ReactorBreach)" with a
mechanically distinct boss per capital.

## Grounding

The design doc (`broadside-analysis.html` §VIII / `CAPITALS`) authors only
`{id, name, sector, corrupt, sP1, sP7}` per capital — **no combat loadout**
(hull / shields / mounts / behavior). So the mechanics below are *derived*
from each capital's name + sector + the engine's already-shipped vocabulary
(traits, weapons, effects). **Nothing here is an invented mechanic** — every
signature maps to a shipped `Trait` / catalog action / `Effect`. Only the
*assignment* (which trait to which capital) and the balance numbers are new,
and those are the taste calls for bruce.

Baseline for all capitals: larger hull than regular enemies, stronger bow
armour, `BowOn` facing the player, hull scaling roughly with sector order
(early → late). The Warlord (the existing tuned `boss_ship_for_spawn`) is
the reference high end at hull 14.

## Per-capital signatures

| # | Capital | Sector | Hull* | Signature mechanic (shipped vocabulary) |
|---|---------|--------|-------|------------------------------------------|
| 1 | The Dasher | Drift Belt | 8 | `BurnHard` + `Pursuit` (dashes in, then chases); pulse_laser + afterburner. Intro "kite the aggressor" boss. |
| 2 | The Impaler | Ion Reefs | 9 | Forward `SPINAL_LINE` alpha (particle_lance) — impales down the lane; punishes stacking in its forward line. |
| 3 | The Barricader | Ashen Expanse | 10 | High shields (bow armour 3, port/starboard 2) + `Anchored` (immune to push/pull/reorient). The wall; brute attrition. broadside_battery. |
| 4 | The Twins | Spindle Port | 5 ×2 | **Two ships** (structural special-case): two hull-5 `BowOn` hulls at different cells, covering both lane-ends. Kill order matters. pulse_laser each. |
| 5 | The Sentinel | Spirit Gate | 10 | `ReactiveShield` (shield after taking damage) + point_defense (downs your ordnance). Patient turret; punishes chip + neutralizes torpedoes. |
| 6 | The Coward | Hot Verge | 8 | `BurnHard` + reverse_thrust signature — flees when approached, forcing a chase; long-range railgun_broadside punishes from distance. **Needs a small resolver AI "flee" hook** if greenlit. |
| 7 | The Fallen | Forsaken Drift | 9 | `ReactorBreach` (big death-blast) + mine_layer (hazard-strewn approach). Kill-it-carefully. |
| 8 | The Stagemaster | Mirror Theatre | 9 | sensor_scramble signature (flips the PLAYER's orientation — "the stage turns") + grav_snare. Control/disorientation boss. |
| 9 | The Warlord | Inner Keeps | 14 | The existing `boss_ship_for_spawn` (ReactorBreach, 3 mounts forward+broadside). Keep as-is — already tuned. |
| 10 | The Flagship | Citadel | 16 | Run's true final boss: beam_cannon + missile_salvo + broadside_battery, `TwinLinked` (fires twice), bow armour 3. The hardest fair fight. `corrupt:false`. |
| 11 | Void Sovereign | Crimson Anomaly | 16 | Hidden P7-only superboss: `Voidtouched` (on death spawns a Void Progeny — the doc's P7 trait) + heaviest loadout. `corrupt:false`. |

\* Hull = starting suggestion at Patrol 1; scales up by patrol tier like
`EnemyDef::hull5`, plus the corrupted variant (+hull / +1 trait) when the
Patrol-4 `corrupt` roll fires.

## Implementation note (content runtime lane, when scheduled)

A `capital_ship_for_spawn(capital_id, patrol_tier)` in `runs.rs`: match on
the capital id → the loadout above, hull scaled by patrol tier, corrupted
variant when the `corrupt` Patrol-4 roll fires. Wire it into
`generate_sector`'s boss encounter (replacing the current warlord-only
dispatch). **The Twins is the one structural special-case** — its boss
encounter spawns TWO `enemy_ships`; every other capital is one ship with a
distinct trait/weapon set, all from shipped vocabulary, **no new `Effect`
needed** — EXCEPT The Coward's flee, which would need a small resolver-lane
AI "reverse-thrust when the player closes" hook (resolver's, only if bruce
greenlights the flee behavior).

## For bruce (balance / taste calls)

- The hull numbers (8–16) and the patrol-tier scaling curve.
- Which trait each capital carries (the assignment is the design taste call;
  the mechanics themselves are all shipped vocabulary).
- Whether The Coward's flee and The Stagemaster's flip-you read as fun vs
  gimmicky — these two are the most "novel feel" and the likeliest to cut.
- Greenlight (or not) the resolver AI flee-hook for The Coward.

Ready to implement on bruce's thumbs-up + architect's `CapitalDef` (#63)
landing.
