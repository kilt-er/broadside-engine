# Broadside — Architecture Overview

*First cut, drafted from the TypeScript reference engine at `_drive_pull/broadside-engine/`
and the design document at `_drive_pull/broadside-analysis.html`. This page describes the
shape the Rust port is targeting; it will be revised as `src/` lands.*

Broadside is a turn-based tactical space-combat roguelike. It ports the Shogun Showdown
action model to ranged ship warfare, adding four new axes — range bands, hull orientation
with directional shields, ordnance as live board entities, and a heat economy. Everything
below is the engine that drives those axes; the design intent for each is in the analysis
HTML (cited by Part number).

---

## The data model in one paragraph

Every weapon, system, maneuver, ordnance launch, and vent is one shape — an `Action`:
`{ id, archetype, cost: { heat, cooldownMax, advancesTurn }, targeting: { pattern, band[],
optimalBand, requiresArc, hitsAll }, effects: Effect[] }`. The resolver knows nothing
about specific actions; it reads this shape and applies the effects. Adding content is
adding rows to a table, not branches to code. (HTML Part II.)

Ships, projectiles, and hazards are the entities on the board. A `Board` is a fixed-size
lane (5/7/9 cells), a per-cell ship slot, a flat list of live `Projectile`s, a per-cell
hazard list, the global `patrol` tier, and an `EventBus`. (HTML Part I.)

---

## The four-phase round

`resolveRound(board, content)` runs every turn. The same execution path serves player,
enemy, and ordnance — that is the engine's first principle.

1. **Player phase.** The player's queued actions fire bottom → top through
   `executeQueue`.
2. **Ordnance step.** Every live `Projectile` advances by its `speed` and resolves any
   impact through `advanceProjectile`. Telegraphed order — the player sees the path
   before committing.
3. **Enemy phase.** Enemies act in a *visible* initiative order (lane order today,
   explicit initiative later). For each enemy: AI fills the queue (`decideEnemyAction`,
   stubbed), then the same `executeQueue` runs.
4. **End of turn.** Cooldowns tick down, heat dissipates by 1, lockout clears if heat
   has fallen below `heatMax`, statuses tick and decay, `onTurnEnd` fires on the bus.

The interleave is what makes ordnance positional: the player commits to a torpedo on
turn N, the projectile then moves on turn N+1, N+2, … and either is shot down, dodged,
or eats hull. This is the slot Shogun Showdown's instant-hit model could never fill.
(HTML Part I.)

```
resolveRound
  ├─ executeQueue(player, ...)
  ├─ for p in board.ordnance: advanceProjectile(p, ...)
  ├─ for e in enemyInitiative(board):
  │     if not skipsTurn(e):
  │         decideEnemyAction(e, ...)   // AI, currently stubbed
  │         executeQueue(e, ...)
  └─ endOfTurn(board, ...)
```

---

## Queue execution: the arc + heat + cooldown gate

`executeQueue(ship, board, content)` walks the ship's `queue` of action IDs. For each:

1. Look the action up in the content catalog. If missing, skip.
2. If the ship is `lockedOut` (heat ≥ heatMax) and the action costs heat, skip it.
   Free-fire / zero-heat actions still resolve while locked out — that is the only way
   a venting overheated ship can save itself.
3. If the action's cooldown is non-zero, skip — not charged.
4. Resolve targeting (`resolveTargeting`, see below) to a list of cells.
5. If the action requires an arc and *nothing bore* (no cells came back), skip. This is
   the orientation gate: a forward gun trying to fire when broadside-on simply doesn't
   shoot.
6. Apply each effect in `a.effects` to the cells.
7. Add `a.cost.heat` to ship heat. If that crosses `heatMax`, set `lockedOut = true`.
8. Reset `cooldowns[actionId] = a.cost.cooldownMax`. The cooldown always resets — hit or
   miss, on bore or off — preventing rapid retry-spam.
9. Emit `onDamageDealt` (the umbrella "the ship fired" event subsystems hook).

After the queue empties, detect chain kills and clear the queue. (TS source:
`resolve.ts:53`.)

---

## Targeting: choosing the cells an action resolves on

`resolveTargeting(action, board, ship)` is a closed dispatch over eight patterns:

| Pattern         | Returns                                                           |
|-----------------|-------------------------------------------------------------------|
| `SELF`          | `[ship.cell]`                                                     |
| `POINT_BLANK`   | First target in bearing direction at allowed band, or `[]`        |
| `BEAM`          | Same as `POINT_BLANK` — hitscan in the firing arc                 |
| `SPINAL_LINE`   | Line of targets in bearing direction; pierces if `hitsAll`, else 1 |
| `BLAST`         | First target ± neighbours (3 cells, AOE)                          |
| `BROADSIDE`     | Both lane ends if broadside arc bears, one target each direction  |
| `ORDNANCE`      | Adjacent cell in bearing direction (spawn point)                  |
| `DEPLOYED_CELL` | Adjacent cell (mine/drone placement)                              |

The two gates inside every non-`SELF` branch:

- **Arc** via `bears(ship, arc, towardEnd)` — does the mount currently aim that way given
  the ship's orientation? `turret` always bears; `forward` only when bow-on and the bow
  points that end; `rear` only when bow-on and the bow points the *other* end;
  `broadsideArc` only when stance is broadside.
- **Band** via `inBand(allowed, ship.cell, target.cell)` — is the target inside one of
  the action's allowed range bands?

The arc gate is what makes orientation a turn-by-turn decision. (HTML Part III, Part IV.)

---

## The damage pipeline

`applyDamage(target, raw, atkCell, weapon, board)` runs every hit through the same five
ordered steps. Every numeric modifier in the game plugs into this list at a fixed slot;
new content cannot reorder it.

1. **Band falloff.** `bandFalloff(raw, actualBand, optimalBand)` reduces damage based on
   how far the actual band sits from the weapon's optimal. Tunable factor table —
   currently `[1, 0.66, 0.5, 0.33, 0.2]` indexed by band distance. Effects with
   `bandFalloff: false` skip this step (impact damage from collisions, fixed-payload
   ordnance, etc.).
2. **Subsystem modifiers.** `applyModifiers(dmg, target, band, board)` — currently a
   stub. Subsystems like Marksman or Point-Blank Doctrine add bonuses here. *Additive*,
   so target-lock doubling lands on top.
3. **Target-lock doubling.** If the target carries `targetLock` status, `dmg *= 2` and
   the status is consumed. The curse analog. (HTML Part VII.)
4. **Directional shield.** Compute the incoming direction
   (`directionTo(target.cell, atkCell)`), look up the hull zone that faces it
   (`facingZone`), and run damage through that zone's `ShieldFace` via `absorbShield`.
   A held `charge` negates the hit entirely and is consumed; otherwise the zone's fixed
   `armour` is subtracted. Strong bow soaks; weak stern bleeds.
5. **Hull.** What survives reaches `target.hull`. If hull drops to zero, `destroy`
   removes the ship from the board and fires `onLethal`; ships with `ReactorBreach`
   splash 2 damage to adjacent cells before dying.

This is the most important table in the engine: every balance lever — armour values,
falloff factor, target-lock ratio, subsystem stacking — is one slot in this pipeline.
(TS source: `resolve.ts:139`; HTML Part XIII implementation order #3.)

---

## Effects: the verb payload

An `Effect` is a tagged union with a closed set of kinds. `applyEffect` dispatches over
them:

| Effect             | What it does                                                  |
|--------------------|---------------------------------------------------------------|
| `DAMAGE`           | Each cell with a ship runs through `applyDamage`.             |
| `APPLY_STATUS`     | Each ship gets the status; existing duration is `max(old, new)`. |
| `VENT_HEAT`        | Source heat drops, lockout clears, cooldowns optionally reset; emits `onVent`. |
| `REORIENT`         | Source flips stance, or sets bow-on / broadside; emits `onReorient`. |
| `SPAWN_ORDNANCE`   | Pushes a new `Projectile` into `board.ordnance` via `content.spawnProjectile`. |
| `DISPLACE_SELF`    | Moves the source per `MovementMode` (THRUST / BURN / SLIP / JUMP / TRACTOR_SWAP). |
| `DISPLACE_TARGET`  | Push / pull / swap each cell's ship; collision damage on impact. |
| `DEPLOY`           | Drops a `Hazard` (mine / drone / debris) into the cell's hazard list. |
| `BOARD`            | Board-wide effect (mass cards, lightning analogs). Stubbed.   |

New verbs require a new arm here *and* a new variant in the `Effect` enum. New
*content* using existing verbs is data only. (TS source: `resolve.ts:167`; HTML Part II.)

---

## Two gates: heat and cooldown

Shogun Showdown had one resource — cooldown. Ranged ships need a reason *not* to fire
every turn, so Broadside splits it.

- **Cooldown** is per-action, 0–8 pips. Resets on fire, ticks down 1 at end of turn.
  Blocks re-queue of *that* action. Per-action throttle — variety pressure.
- **Heat** is per-ship. Every action adds to it. Cross `heatMax` and the ship sets
  `lockedOut = true`; no heat-positive actions until `VENT_HEAT` clears it. Per-ship
  throttle — tempo pressure. You cannot alpha-strike every turn.

`Vent` is the WAIT analog: a free, zero-cost action that resets heat and (sometimes)
recharges cooldowns. Overheating *forces* it. The cycle "burn → vent → burn" is the
combat heartbeat. (HTML Part I.)

---

## Ordnance as entities

Torpedoes and missiles are not hits; they are `Projectile` records living on the lane,
each with `cell`, `heading`, `speed`, `hull`, and a `payload: Effect[]` applied on
impact. They:

- Advance during phase 2 of the round, one cell at a time, up to `speed` cells.
- Resolve on the first occupied cell whose ship is *not* the owner faction.
- Walk off the board if they reach the lane edge with no target.
- Can be shot down by point-defense (any action whose effect would reduce their `hull`
  to zero — payload TBD).

This is the only entity type that mutates the lane between ship phases. It is also the
sole reason `directionTo` and `firstTargetToward` exist as helpers — the same primitives
that resolve a beam also resolve torpedo collision. (TS source: `resolve.ts:233`;
HTML Part I, Part X.)

---

## Orientation: the rotation principle

A ship has two stances:

- **Bow-on** `{ stance: "bowOn", bow: LaneEnd }` — the bow points at one lane end. The
  strong bow shield faces it; the weak stern faces the other; the flanks point off-lane
  and never eat a lane hit. Forward mounts fire toward `bow`; rear mounts fire
  toward the opposite; broadside mounts cannot fire at all.
- **Broadside** `{ stance: "broadside" }` — the hull turned across the lane. Both flank
  shields face the lane (both medium); the bow points off-lane (its armour wasted).
  Broadside mounts fire both ways at once; forward and rear mounts cannot fire.

The design rule that keeps rotation a live decision every turn: *no single orientation
is best against the current threat layout.* Stacked threats on one end want bow-on;
flanked threats want broadside. The enemy AI's job is to manufacture the wrong stance.
(HTML Part IV.)

`REORIENT` (`fx.to`) is `"flip"` (toggle bow direction or stay broadside), `"bowOn"`
(default fore), or `"broadside"`.

---

## The event bus

`EventBus` is `on(hook, fn)` / `emit(hook, ctx)`. The resolver emits at fixed points:

| Hook              | Emitted from                                                  |
|-------------------|---------------------------------------------------------------|
| `onDamageDealt`   | After each ship's queue executes                              |
| `onDamageTaken`   | After `applyDamage` reaches hull with non-zero damage         |
| `onLethal`        | Inside `destroy`, after the ship is removed                   |
| `onChainKill`     | After queue execution if `detectChain(board)` returns true    |
| `onVent`          | After `VENT_HEAT` resolves                                    |
| `onReorient`      | After `REORIENT` resolves                                     |
| `onTurnEnd`       | Last line of `endOfTurn`                                      |
| `onWaveStart`     | Wave spawn (renderer/runner emits, not the resolver itself)   |
| `onHeatThreshold` | Heat crossing a threshold (subsystem-driven, not yet wired)   |
| `onHeal`          | Reserved for repair effects                                   |

Subsystems are passive event subscribers: `{ id, bay, hook, level, apply(ctx) }`. They
never queue, take a turn, or carry a cooldown — they sit on the bus and modify behavior
from the side. Adding a subsystem is content, not code. (HTML Part VI.)

---

## ECS layout (target Rust shape)

The TS reference is plain interfaces with mutable methods on the board. The Rust port
will favor a struct-of-data layout with the resolver as free functions over `&mut Board`,
mirroring the TS module split:

```
src/
├── types.rs       // every TS type as Rust enums/structs; no logic
├── geometry.rs    // pure functions: rangeBand, bandFalloff, facingZone, arcBears, ...
├── resolve.rs     // resolveRound, executeQueue, applyDamage, applyEffect, advanceProjectile
├── content.rs     // catalog loading; Action lookup; spawnProjectile dispatch
├── ai.rs          // decideEnemyAction (stubbed today, fills in lane-end threat objective)
├── effects.rs    // resolveSelfMove, resolveTargetMove, applyModifiers (TS TODO bodies)
├── bus.rs         // EventBus + Hook enum + HookContext
└── gfx/           // wgpu renderer, atlas, HUD — separate from the simulation
```

The simulation never imports from `gfx/`. The renderer reads `Board` and `Ship` state and
listens to the event bus for kill / damage / vent animations. (HTML Part XIII
implementation order #1–#8.)

---

## AI loop (current intent)

`decideEnemyAction(enemy, board, content)` is the one decision point. Its objective —
per the design — is to *maximize the number of distinct lane-ends it threatens.* That
is what manufactures the rotation pressure the orientation system depends on: if both
ends are threatened, neither bow-on nor broadside is correct, and the player must use
displacement / movement / reorient to break the dilemma.

The decision layer picks actions; the *same* `executeQueue` then runs them. No special
enemy code path. (TS source: `resolve.ts:395`; HTML Part IV closing paragraph.)

---

## Renderer scaffold

Not yet implemented. The plan:

- **wgpu** pipeline drawing the lane as a horizontal strip of cells, ships as sprite
  quads, projectiles as smaller sprites, hazards as cell overlays.
- **Atlas** module packs all ship / weapon / status icons into one texture; UV lookup
  by string ID matches the TS `icon` field convention.
- **HUD** overlay shows queue, heat bar, cooldown pips, initiative badges, range-band
  ruler. Driven by event-bus subscriptions for damage numbers and kill flashes.
- **Frame separation:** the simulation runs one `resolveRound` per player commit; the
  renderer interpolates ordnance motion and animation timing between steps. The
  simulation is *not* per-frame; the renderer is.

---

## What ships in the first playable

Following the HTML's suggested order:

1. `Board`, `Ship`, the five movement modes, orientation.
2. `resolveTargeting` for all eight patterns with band + arc checks.
3. The damage pipeline.
4. Ordnance + the round interleave.
5. Heat / vent / overheat.

That gets a deterministic duel running through the resolver. 6–8 (event bus +
subsystems, AI, mods/traits/classes/field kit) are pure content layered on top.
