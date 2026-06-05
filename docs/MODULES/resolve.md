# `src/resolve.rs` — Module Companion

*A self-contained walkthrough of the combat resolver. The same content as the
[`resolve.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcresolvers), but
scoped: this file assumes you only care about how a round resolves and don't need the
rest of the engine in context. Read this if you are about to add a new effect verb,
modify the damage pipeline, write a subsystem hook, or debug a cascading event chain.*

**Source commits:** `c5855ce` (initial port) + `da243be` (content TODO closures +
`Content::damage_modifier` extension + `&dyn Content` cascade) + `6575472`
(EventBus γ-invariant docstrings).
**Mirrors:** `_drive_pull/broadside-engine/engine/resolve.ts`.
**Design anchor:** HTML Part I (Core Loop) and Part XIII (Engine Integration & Schema).

---

## Why this file exists

The resolver is what makes Broadside a game rather than a data model. Every action
queued by player or enemy, every torpedo crossing the lane, every status tick at
end-of-turn — all of it runs through this one file. **One execution path serves
player, enemy, and ordnance**; the resolver doesn't branch on faction, and the AI
never bypasses the pipeline.

Three things to know up front:

1. **The TS is canonical** when this port disagrees, but the Rust port also fills in
   several TS TODO bodies (`apply_modifiers`, `resolve_self_move`,
   `resolve_target_move`, `decide_enemy_action`) with implementations that match
   the analysis-doc intent. Those are documented as Drift notes.
2. **The damage pipeline is load-bearing**. Five steps in a fixed order:
   band falloff → modifiers → target-lock ×2 → directional shield → hull. Every
   balance lever plugs into one of these slots; **do not reorder**.
3. **The bus is detached during emit** (`mem::take` pattern). Callbacks cannot
   re-emit on the same bus — they must call resolver functions directly to cascade.
   See "Re-entrancy and the γ-invariant" below.

---

## The four-phase round

`resolve_round(board, content)` runs one full round:

| Phase | What                                                                          |
|-------|-------------------------------------------------------------------------------|
| 1     | Player phase — find the player by faction scan, call `execute_queue` on their cell. |
| 2     | Ordnance phase — snapshot projectile ids; advance each by id-lookup. **Opens its own chain-kill window** (`destroys_this_window = 0`). |
| 3     | Enemy phase — for each enemy in initiative order: `skips_turn` check, `decide_enemy_action` to fill the queue, `execute_queue`. |
| 4     | End of turn — tick cooldowns, dissipate heat, clear lockout, tick statuses, emit `OnTurnEnd`. |

Phase 2 reuses the same `destroys_this_window` field as phase 1 — both reset to 0 on
entry, both let `destroy()` accumulate kills, but only `execute_queue` emits
`OnChainKill` from `detect_chain`. The TS ordnance phase does the same (no
`onChainKill` emit during ordnance). If you want torpedo-driven chain kills to fire
the hook, that's a future-work item; today it's intentionally omitted.

---

## The damage pipeline (the load-bearing sequence)

`apply_damage(target_cell, raw, atk_cell, weapon, board, content)`:

```
   raw damage
     │
     ▼
1. band falloff
     │   if ANY DAMAGE effect on the weapon has band_falloff: Some(false),
     │   step is skipped (action-level predicate, not per-effect)
     ▼
2. subsystem modifiers
     │   apply_modifiers(dmg, target, band, board, content)
     │   delegates to content.damage_modifier; default impl returns 0
     │   result is clamped to max(0, dmg + bonus)
     ▼
3. target-lock 2x
     │   if target carries TargetLock status: dmg *= 2 and the status is consumed
     │   (swap_remove the matching entry)
     ▼
4. directional shield
     │   incoming_from = direction_to(target.cell, atk_cell)
     │   zone = facing_zone(target.orientation, incoming_from)
     │   absorb_shield(face_mut(zone), dmg)
     │     - charge > 0: decrement, return 0
     │     - else:       (dmg - armour).max(0)
     ▼
5. hull subtraction + emit + destroy check
     │   target.hull -= final_dmg
     │   if final_dmg > 0: emit OnDamageTaken
     │   if target.hull <= 0: destroy(target_cell, board, content)
     ▼
   resolved
```

**This order is invariant.** Every numeric modifier in the game plugs into one of
these slots; reordering would change observable behaviour. If you add a new balance
lever, decide which slot it belongs to and document it in the slot, not as a new
slot.

---

## Queue execution: the arc + heat + cooldown gate

> **Naming note (post-refactor).** What this section calls `execute_queue` is now
> split in the source: the queue-firing seam is `fire_player_queue(ship_id, board,
> content)` (src/resolve.rs:212, the former `executeQueue` body — used for player
> *and* enemy), and the per-action gate + effect application is factored into
> `run_action(...)` (src/resolve.rs:346). `resolve_round` (src/resolve.rs:183) now
> just composes `fire_player_queue` (phase 1) + `run_world_phase` (phases 2-4). The
> pseudocode below still describes the combined behavior faithfully; see the
> [`fire_player_queue` / `run_action` walkthrough](../LINE_BY_LINE.md#srcresolvers)
> in LINE_BY_LINE for the current per-line cites. Also note the **no-mount-gate
> gotcha** documented there: the queue path fires by id lookup and does not require
> the ship to own a matching `Mount`.

`fire_player_queue` / `run_action` (formerly one `execute_queue`):

```
   reset destroys_this_window = 0   // open the chain-kill window
   clone the queue out (stable iteration across mutations)

   for each action_id in queue:
     action = content.action(action_id)   // skip if None
     re-check ship still exists           // earlier effect may have destroyed it
     check lockout: skip if locked_out && action.cost.heat > 0
     check cooldown: skip if cooldowns[action_id] > 0
     cells = resolve_targeting(...)
     check arc-bore: skip if requires_arc && cells empty  // no cost, no cooldown
     for each effect:
       apply_effect(...)                  // may destroy ships, mutate cells
     ship.heat += action.cost.heat
     if ship.heat >= heat_max: ship.locked_out = true
     ship.cooldowns[action_id] = action.cost.cooldown_max   // unconditional reset
     emit OnDamageDealt with source_cell

   if destroys_this_window >= 2: emit OnChainKill
   clear queue (if ship survived)
```

Three contracts worth memorising:

1. **No bore, no cost.** If the action requires an arc and `resolve_targeting`
   returns an empty cell list, the action is skipped entirely. Heat is NOT spent;
   cooldown is NOT reset. This is what makes optimistic queueing safe.
2. **Cooldown resets unconditionally on a successful fire.** Hit or miss on individual
   effects (e.g. damage absorbed entirely by shields), the cooldown still goes back
   to `cost.cooldown_max`. Matches TS exactly.
3. **`OnDamageDealt` fires per action, not per effect.** Subscribers see one event per
   queued action that passed the arc gate, regardless of whether any damage landed.

---

## Targeting — eight patterns

`resolve_targeting(action, board, ship_cell)` returns the cells the action will
resolve against:

| Pattern         | Returns                                                              |
|-----------------|----------------------------------------------------------------------|
| `SELF`          | `[ship_cell]`                                                        |
| `BROADSIDE`     | Both lane directions if the broadside arc bears (0–2 cells)          |
| `BEAM`          | First target in the bearing direction at allowed band (0–1 cells)    |
| `POINT_BLANK`   | Same implementation as BEAM (the TS draws the same code path)        |
| `SPINAL_LINE`   | Line of occupied cells in bearing direction, filtered by band; all if `hits_all`, else first |
| `BLAST`         | First target ± neighbours (`[c-1, c, c+1]` clamped to lane)          |
| `DEPLOYED_CELL` | Adjacent cell in bearing direction (0–1 cells)                       |
| `ORDNANCE`      | Same as DEPLOYED_CELL (spawn point is the adjacent cell)             |

The arc gate is enforced by `bears(ship, arc, target_cell)` from `geometry.rs`. The
band gate is enforced by `in_allowed_band(action.targeting.band, ship_cell,
target_cell)`.

---

## Effects — the closed verb set

`apply_effect` dispatches over nine `Effect` variants:

| Variant            | What happens                                                                |
|--------------------|-----------------------------------------------------------------------------|
| `DAMAGE`           | For each cell with a ship: `apply_damage` through the pipeline.             |
| `APPLY_STATUS`     | For each cell with a ship: `add_status` (existing entry takes `max(old, new)` duration). |
| `VENT_HEAT`        | Drop source heat, clear lockout, optionally reset all cooldowns. Emit `OnVent`. |
| `REORIENT`         | Flip/set source orientation. Emit `OnReorient`.                             |
| `SPAWN_ORDNANCE`   | Call `content.spawn_projectile(kind, &owner)`, push onto `board.ordnance`.  |
| `DISPLACE_SELF`    | Delegate to `resolve_self_move` (THRUST / BURN / SLIP / JUMP / TRACTOR_SWAP). |
| `DISPLACE_TARGET`  | For each target cell: `resolve_target_move` (Push / Pull / Swap).           |
| `DEPLOY`           | For each cell: push a `Hazard` with `kind` widened from `DeployHazardKind`. |
| `BOARD`            | **Doc-stub**. See Drift below.                                              |

---

## Movement modes

`resolve_self_move(ship_cell, mode, distance, board, content)`:

| Mode           | Path rule                                                                                       | Collision rule                                                       |
|----------------|-------------------------------------------------------------------------------------------------|----------------------------------------------------------------------|
| `THRUST`       | Exactly 1 cell. Distance ignored beyond the first cell.                                         | 1 damage if blocked by wall or occupant.                             |
| `BURN`         | Multi-cell; stops at first ship or wall.                                                        | `max(0, distance - steps_taken)` damage.                             |
| `SLIP`         | Passes *through* ships for `distance` cells, then continues until the first free cell.          | If lane runs out, clamp to edge + bill overflow.                     |
| `JUMP`         | Blink-drive: `start + step * distance` directly, no path scan.                                  | Off-board: clamp + bill overflow. Occupied target: **fail entirely** (no-op). |
| `TRACTOR_SWAP` | Swap with the first adjacent occupant in the bow direction. No-op if adjacent cell empty/off-lane. | None.                                                                |

**Direction:** the ship moves in its *bow* direction. `BowOn { bow: Aft } → step -1`;
everything else → step +1.

**Collision routes through `apply_damage`** with `dummy_weapon()` so `band_falloff`
is skipped. The directional shield still mediates — bow-first collisions eat less
than stern-first.

---

## Target displacement

`resolve_target_move(target_cell, source_cell, mode, distance, board, content)`:

| Mode   | What                                                                                  |
|--------|---------------------------------------------------------------------------------------|
| `Swap` | Trade cells between source and target. No collision damage. No-op if `source == target`. |
| `Push` | Target moves *away* from source. Stops at first occupant (incl. wall). Collision damage on stop. |
| `Pull` | Target moves *toward* source. Source counts as an occupant — pull stops one cell short and applies collision. |

---

## Re-entrancy and the γ-invariant

The bus is detached from the board for the duration of any emit:

```
fn emit(board, hook, build) {
    let mut bus = std::mem::take(&mut board.bus);  // detach
    let mut ctx = HookContext::new(board);
    build(&mut ctx);
    bus.emit(hook, &mut ctx);                      // dispatch
    board.bus = bus;                               // reattach
}
```

Three consequences for subsystem authors:

1. **A callback CANNOT call `ctx.board.bus.emit(...)`.** It finds an empty bus on
   the board and silently no-ops. This is the **γ-invariant**, canonically stated
   in the `EventBus` and `HookContext` docstrings (commit `6575472`).
2. **A callback CAN call resolver functions directly.** `apply_damage`, `destroy`,
   `add_status`, etc. recurse correctly because they don't go through the bus.
3. **The `EventBus::emit` storage-level take/replace** (see
   [`types.rs`](types.md#section-7-subsystems--event-bus-lines-524-724)) handles
   same-bus same-hook re-entrancy if you ever do reach the bus directly — but that
   path is blocked by the detach above, so it's a defence-in-depth detail rather
   than a load-bearing case.

---

## The `destroy()` invariant: splash-before-OnLethal

When a ship dies, the resolver follows a specific ordering. **`destroy()`'s
`OnLethal` emit is the last step**, after the ReactorBreach splash loop has fully
unwound — including any recursive `destroy()` calls triggered by splash kills.

**Concrete consequence for subsystem authors:** an `OnLethal` subscriber for ship X
is guaranteed that any splash damage X dealt has already been observed via
`OnDamageTaken`. The ordering is splash-before-lethal at every level of the chain.

### Worked example: a 3-ship reactor-breach cascade

From `tests/event_chain.rs::cascading_reactor_breaches_chain_correctly`:

- A "breacher" (ReactorBreach trait) sits at cell 1.
- A "tiny" (ReactorBreach trait, low hull) sits at cell 2.
- A normal "neighbour" (10 hull) sits at cell 3.
- `destroy(1, board, content)` is called.

The observable event order:

```
damage(2)   // breacher's splash hits tiny
damage(3)   // tiny's splash hits neighbour (inside breacher's destroy)
lethal(2)   // tiny's OnLethal (after tiny's splash chain unwinds)
lethal(1)   // breacher's OnLethal (after the whole subtree returns)
```

**Both damage emits fire before either lethal emit.** If a future port moves the
`OnLethal` emit *before* the splash loop, the observable order becomes
`[damage(2), lethal(2), damage(3), lethal(1)]` — the test's failure message
explicitly names this regression form (lines 287–293).

`board.destroys_this_window` ends at 2, which `detect_chain` reads as a chain-kill
when the surrounding `execute_queue` completes.

---

## The AI loop (decide_enemy_action)

`decide_enemy_action(enemy_cell, board, content)` picks one action and pushes it onto
the enemy's queue. The resolver then runs the queue through `execute_queue`
unchanged — **the AI never bypasses the pipeline**.

### Objective: fire when in position, else close (#71/#74)

The design intent (analysis doc) is still "the enemy controls which situation you are
in — it maximises the distinct lane-ends it threatens, so the player keeps flipping
stance." **But that intent is now served by the maneuver step, not by a scoring
term.** The current rule is blunt and correct: **if this enemy has any in-band,
bearing, affordable, hostile-targeting action, it FIRES — full stop; otherwise it
CLOSES toward the player** (then reorients, then vents). Firing from a good position
is the whole point of the AI, so it must actually happen.

> **Drift / history (#41 → #71 → #74).** An earlier design (#41 "diversify-or-fire")
> scored a `+6` bonus for threatening the player from a lane-end no already-queued
> ally covered, and would **suppress firing** (reposition instead) when this enemy's
> end was already covered. Two problems, both now fixed:
> - **#71 dropped the covered-end fire-suppression.** With the live spawn shape (all
>   enemies on one side of the player) every enemy after the first saw its end
>   "covered", so every one maneuvered instead of firing; since they were all on the
>   same side none ever reached an "uncovered" end, so they marched into the player
>   and died without firing — bruce's "they line up and never shoot" bug. On a 1-D
>   lane, repositioning to a fresh end is rarely achievable, so "fire when in
>   position" must win over "hold fire to maybe pressure a different end".
> - **#74 removed the `+6` term entirely** as vestigial. `my_end_from_player` is
>   constant across one enemy's own candidates, so the bonus was added to all of an
>   enemy's options or none — it never changed that enemy's argmax (the queued pick).
>   With the suppression gone (#71) it had no behavioral effect at all. **True
>   cross-enemy threat coordination — an initiative pass assigning enemies to
>   distinct lane-ends — was never built; lane-end diversity today is emergent from
>   geometry, not directed.** The term was deleted rather than left to mislead; if
>   explicit coordination is wanted later it's a real resolver feature, not a dead
>   scoring constant.

### Algorithm

1. **Locate the player**. If absent, return.
2. **Enumerate available actions**: every mount's weapon, gated by cooldown, heat,
   lockout, arc, band. Heat-budget gate skips actions that would push more than 1
   above `heat_max` (happy to overheat exactly once; a 2+ overshoot wastes a whole
   vent turn). A locked-out enemy may only fire zero-heat actions. Arc/band is
   checked by `resolve_targeting` against the real board, so "available" means "would
   actually resolve to a non-empty cell set." A **friendly-fire filter** (#49) drops
   any action whose target cells are all empty or all same-faction.
3. **Score** each available action (selects WHICH weapon, no longer WHETHER to fire):
   - `+10` if a hit cell contains the player (the visible threat)
   - `+raw_damage` (sum of `DAMAGE` effect amounts)
   - `-heat` cost (halved for `BurnHard` ships — they're less heat-averse)
   - `+2` for `Pursuit` ships when the action hits the player
4. **FIRE the best-scoring action if there is one** — unconditionally queue it and
   return. (This is the #71 change: no covered-end detour.)
5. **Else maneuver, then reorient, then vent** (fallback ladder, all visible
   telegraphs):
   - **Close** — `queue_purposeful_maneuver` queues a SYNTHETIC lane-relative move
     (`__move_left`/`__move_right`) toward the player. Live enemies carry no movement
     action in their mounts (mounts are built from `def.weapons`), so the AI issues
     the same synthetic move ids the player uses; `resolver_ai_move` serves them even
     when the running `Content` doesn't register them (no DemoContent dependency).
     Skipped when **locked out** (an overheated enemy vents first rather than crawl
     forward unable to shoot) — #68 anti-camp / #41 "optimal position".
   - **Reorient** — any REORIENT action, in case a flip brings the player into arc
     next turn.
   - **Vent** — any VENT_HEAT action, to clear heat for next round.
   - If even that fails, leave the queue empty (a correctly-configured enemy with one
     valid mount should never reach here).

> **`Pursuit` +2 — live but currently unreachable.** The `Pursuit` nudge (resolve.rs
> ~:1972) IS live in the score math and CAN flip the pick: it's conditional on
> `hits_player`, so it races a candidate that does NOT hit the player. On real
> single-player boards, though, every candidate that an in-position enemy can fire
> hits the same lone player, so the +2 is added uniformly and never breaks a tie —
> it only races on a hypothetical board with a second player-faction ship. Documented
> as "live but currently unreachable," not inert.

### Visible-threat invariant

Every successful AI turn produces an action whose `resolve_targeting` returns a
non-empty cell set, OR a fallback action (reorient / move / vent) that is itself a
visible queued telegraph. The TS resolver renders queue contents over each ship, so
pushing any action id is enough to make the AI's intent legible.

---

## Drift from TypeScript

Resolved by `c5855ce + da243be + 6575472`:

1. **`Content` trait, not struct.** TS uses
   `interface Content { actions, spawnProjectile }`; Rust uses a trait with three
   methods. Lookup is `Option<&Action>`; spawn drops the `&Board` parameter (caller
   already has it); a new `damage_modifier` method routes subsystem bonuses through
   the trait.
2. **`damage_modifier` trait extension** (new in `da243be`). TS leaves
   `applyModifiers` as a stub returning `dmg`. The Rust port routes subsystem
   bonuses through `content.damage_modifier(target, band, board)` with a default
   impl returning 0. The runtime subsystem registry lives on concrete Content, not
   on Board, because the bus path can't reach the modifier step in time
   (`OnDamageDealt` fires after `apply_damage` already ran).
3. **`&dyn Content` cascade**. The `Content` parameter rippled into `destroy`,
   `tick_statuses`, `end_of_turn`, `advance_projectile`, `resolve_self_move`,
   `resolve_target_move`, and the effect dispatch. Broad but mechanical; pipeline
   ordering preserved.
4. **`destroys_this_window` counter on Board** (new vs TS). The TS `detectChain` is
   a stub returning `false`. The Rust port adds an explicit counter on `Board`;
   `execute_queue` and the ordnance phase each reset it; `destroy` increments;
   `detect_chain` reads `>= 2`. Reset semantics live in the resolver, not on
   `Board`.
5. **`TRACTOR_SWAP` semantic**. TS doesn't specify. Chosen: "swap with the first
   adjacent bow-direction occupant; no-op if empty." Coordinated with team-lead.
6. **`Effect::BOARD` doc-stub**. Mass-* board-wide effects (mass_lock, mass_breach,
   mass_emp, sensor_pulse) are **field-kit Cards** in the analysis doc, not
   Actions — they live under `Catalog::fieldkit` and will be resolved by a future
   field-kit handler, not through `applyEffect`. The TS body is also empty, so
   leaving this stubbed matches the canonical reference exactly.
7. **Push/Pull collision into source**. Source counts as an occupant; pull stops
   one cell short and applies the standard collision-damage rule. TS didn't
   specify.
8. **AI fallback ladder**. Movement → reorient → vent → empty queue. Ensures the
   AI's intent is always visible in the queue.
9. **EventBus γ-invariant**. The `emit` helper detaches the bus during dispatch;
   chained semantics must go through direct resolver calls. Canonical statement in
   `EventBus` / `HookContext` docstrings (commit `6575472`).
10. **`mem::take` pattern for bus borrowing**. TS doesn't have borrow checking; the
    Rust port detaches the bus to release the `&mut Board` conflict during hook
    dispatch. `EventBus: Default` is the load-bearing impl that makes this legal.

---

## Tests

40+ inline tests in `#[cfg(test)] mod tests` plus integration suites at
`tests/event_chain.rs`, `tests/pipeline.rs`, `tests/geometry.rs`, and others.
Notable inline tests:

```
apply_damage_weak_stern_takes_post_falloff_hit       (demo Scenario A)
apply_damage_strong_bow_soaks_to_zero                 (demo Scenario B)
apply_damage_target_lock_doubles_and_consumes
apply_damage_lethal_clears_the_cell
execute_queue_overheats_and_records_cooldown
execute_queue_no_target_no_cost                      (no-bore-no-cost)
```

The integration suite at `tests/event_chain.rs` covers cascading reactor breaches
and the splash-before-lethal invariant.

---

## Weapon mods (the 7-mod dispatch, commit 1619bac / #50)

A weapon mod attaches to ONE action via [`Action::r#mod`](types.md) (a single mod-id
string) and changes how that action fires. `WeaponMod::from_id` (resolve.rs:961) is an
**exhaustive match that doubles as the drift guard** — an unrecognised id → `None` → the
action fires un-modded (forward-compatible with mods not yet implemented). No TS analog;
the TS engine never wired mods. The seven dispatch at **two points by kind**:

**Action-level (in `run_action`)** — change how the whole action runs:
- **`twin_linked`** — effect list runs twice, targeting **re-resolved before pass 2** (the
  second volley re-aims at the board the first left); cost/heat/cooldown paid once.
- **`precision_core`** — pre-snapshot targeted-occupied cells; on a clean kill, override
  the post-effect cooldown reset to **0** (recharge). The subtlety: applied *after* the base
  cooldown insert in `run_action`'s bookkeeping so it wins — a recharge written during
  effects would be clobbered by that insert.
- **`autoloader`** — free-fire (no turn advance). The resolver never branches on
  turn-advance, so this is a **public seam** `action_advances_turn` (resolve.rs:1004) for the
  SS turn dispatcher in [`input.rs`](input.md). **Resolver-side ready, turn-layer wiring
  pending** — `input.rs::apply_intent` does not yet call it, so autoloader is parsed +
  override-ready but not yet visibly free-fire.

**On-hit (in `apply_on_hit_mod`, resolve.rs:1020)** — riders that land per connected hit,
called from the DAMAGE arm of `apply_effect` once per cell that held a ship pre-hit (so they
land on contact **even if the shield ate the hull damage**):
- **`flak_burst`** — 1 dmg to each lane-neighbour (±1) via the shield-mediated dummy-impact
  pipeline (falloff off, same precedent as ReactorBreach splash), **faction-blind**; the hit
  cell itself isn't re-damaged.
- **`incendiary`** → `hullBreach 3`; **`emp_charge`** → `systemsOffline 3`;
  **`targeting_laser`** → `targetLock` (dur 5, consumed by the next hit). **`precision_core`**
  is a no-op here (its recharge is handled in `run_action`, see above).

**Design choice — on-hit helper, NOT a bus subscriber** (resolve.rs:1017-1019): dispatched by
a direct call from the DAMAGE arm, never via the `EventBus`, so it can't re-enter the resolver
through the bus — same rationale as content-side subsystems vs bus closures. **Scoping:**
single-mod-only first pass (`r#mod` is one id); the "autoloader + another mod" combo is a
deferred `r#mod → Vec` change. Mods referenced from class loadouts in
[`classes.md`](classes.md) and the catalog `ModDef` in [`catalog.md`](catalog.md) /
[`types.md`](types.md).

---

## Cross-references

- **Type vocabulary:** every type from [`src/types.rs`](types.md).
- **Geometry primitives:** `range_band`, `band_falloff`, `facing_zone`,
  `absorb_shield`, `direction_to`, `bears`, `opposite` from
  [`src/geometry.rs`](geometry.md).
- **Event bus:** `EventBus`, `HookContext`, `Hook` from
  [`src/types.rs`](types.md#section-7-subsystems--event-bus-lines-524-724).
- **Domain terms:** every concept in the [glossary](../GLOSSARY.md). Start with
  *Damage pipeline*, *Heat lockout*, *Chain kill*, *Target lock*, *Movement mode*.
- **Design intent:** Parts I, IV, VI, VII, and XIII of the
  [analysis document](../../_drive_pull/broadside-analysis.html).
