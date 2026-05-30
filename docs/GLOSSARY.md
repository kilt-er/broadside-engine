# Broadside — Glossary

*Every domain term used in the engine, the design document, or the docs. Alphabetical.
Each entry cites the HTML Part where the term is introduced and, where relevant, the TS
source line in `_drive_pull/broadside-engine/`. Revised as code lands.*

---

## A

**Action**
The universal verb. Every weapon, system, maneuver, ordnance launch, vent, and signature
is one `Action` record: `{ id, name, archetype, cost, targeting, effects, mod?, icon? }`.
The resolver knows nothing about specific actions; it reads the shape and applies the
effects. New content is data, never new code. (HTML Part II; `types.ts:100`.)

**Action queue**
See **Queue**.

**Advances turn**
The `cost.advancesTurn` flag on an Action. If `true`, loading the action into the queue
costs the player's turn. If `false` it is **free-fire** — load and continue. Vent,
maneuvers, the Autoloader mod, and most signature systems are free-fire. (HTML Part I,
Part II; `types.ts:114`.)

**Arc**
A mount's firing window relative to the *hull*, not the lane. Four values: `forward`
(out the bow), `broadsideArc` (out both flanks), `turret` (any direction), `rear` (out
the stern). Whether a mount currently *bears* on a target depends on the ship's
orientation, checked by `arcBears`. (HTML Part III; `types.ts:25`, `geometry.ts:74`.)

**Archetype**
A weapon's high-level family, used for filtering and UI grouping, not resolution:
`beam`, `ordnance`, `broadside`, `displacement`, `control`, `movement`, `defensive`.
The resolver never branches on archetype. (HTML Part V; `types.ts:126`.)

**Armour**
A `ShieldFace`'s permanent directional reduction. Subtracted from damage that has
already cleared band falloff, modifiers, and target-lock. Bow has the most (default 2),
stern the least (default 0), flanks medium (default 1). Cannot be consumed; **charge**
is the consumable counterpart. (HTML Part IV; `types.ts:72`, `geometry.ts:101`.)

## B

**Band**
See **Range band**.

**Band falloff**
The damage penalty for firing outside a weapon's `optimalBand`. Indexed by the absolute
distance between actual and optimal band positions (mapped via `geometry::band_index`,
an exhaustive `match` over `RangeBand`). Factor table currently
`[1, 0.66, 0.5, 0.33, 0.2]`, floored at 0. Skipped when an `Effect::DAMAGE` on the
action carries `band_falloff: Some(false)` — `None` and `Some(true)` both apply
falloff. (HTML Part III; `src/geometry.rs:69`, `src/types.rs:409`.)

**Bay**
A subsystem's purchasing/grouping category. Six bays: `gunnery`, `helm`, `engineering`,
`tactical`, `general`, `astrogation`. Bays map to the original game's shop categories.
(HTML Part VI; `types.ts:167`.)

**Bears**
A mount **bears** on a target when its arc, given the ship's current orientation, points
at the target's lane direction. The arc gate inside `resolveTargeting`. (HTML Part III;
`geometry.ts:74`, `:90`.)

**Beam**
(a) An archetype — instant-energy weapons. (b) A targeting pattern (`BEAM`) — hitscan
on the first target in the bearing direction. (HTML Part III, Part V.)

**Blast**
A targeting pattern. Hits the first target and its two neighbours (3 contiguous cells).
The AOE pattern. (HTML Part III.)

**Board**
The game state record: lane size (5 / 7 / 9), per-cell ship slots, live ordnance list,
per-cell hazard lists, patrol tier, and event bus. The single mutable state the
resolver operates on. (HTML Part I; `types.ts:31`.)

**BOARD effect**
A board-wide effect kind, currently a stub. Reserved for mass-card items and
lightning-style chain effects from the field kit. (HTML Part X; `types.ts:145`.)

**Bow**
The front of a ship, and the strongest hull zone (default armour 2). In bow-on stance
the bow points at one lane end; in broadside the bow points off-lane and its armour is
wasted. (HTML Part IV; `types.ts:19`.)

**Bow-on**
The orientation `{ stance: "bowOn", bow: LaneEnd }`. The bow faces one lane end; the
weak stern faces the other; flanks point off-lane. Forward mounts fire toward `bow`,
rear toward the opposite, broadside mounts cannot fire at all. Best when threats are
stacked on one side — you tank with the bow. (HTML Part IV; `types.ts:14`.)

**Broadside (stance)**
The orientation `{ stance: "broadside" }`. The hull is turned across the lane; both
flank shields face it (both medium); the bow points off-lane. Broadside mounts fire to
both lane ends at once. Best when flanked. (HTML Part IV.)

**Broadside (archetype / pattern)**
(a) The `broadside` archetype groups two-way batteries. (b) The `BROADSIDE` targeting
pattern fires at the first target in *both* lane directions in one action — but only
when the ship's stance is `broadside`. (HTML Part III, Part V.)

**Bus**
See **Event bus**.

## C

**Capital ship**
A boss ship. Each sector ends in one. From Patrol 4 a capital can appear **Corrupted**,
a buff/aura overlay. Capitals are the only enemies that drop **salvage**. (HTML Part
VIII.)

**Catalog**
The full content dataset — actions, mods, subsystems, statuses, enemies, capitals,
classes, field kit, sectors, patrol tiers, commendations — exported as one JSON object.
The content seed; loaded once at start. (HTML Part XIII; `types.ts:198`.)

**Cell**
One lane position. Indexed `0..size`. Higher index is `fore`, lower is `aft`. At most
one ship per cell; ordnance and hazards overlay. (HTML Part I.)

**Chain kill**
Two or more ships destroyed in a single execution window. Emits `onChainKill` so
Tactical subsystems can hook in. The Shogun Showdown combo analog. (HTML Part I;
`resolve.ts:72`, `:346`.)

**Charge**
A held shield "ping" on a `ShieldFace`. Negates the next hit entirely and is consumed
on use. Granted by Brace and similar defensive actions. Distinct from **armour**,
which is permanent and subtractive. (HTML Part IV; `types.ts:72`, `geometry.ts:103`.)

**Class**
A starting configuration: two base weapon loadouts plus a `signature` system delivered
through the free-fire Signature action, which the resolver dispatches by class
identity. Each class has a **stance affinity** (bow-on vs broadside). The hero analog.
(HTML Part IX; `types.ts:66 — klass`.)

**Close**
The second range band, distance 2. (HTML Part III; `geometry.ts:33`.)

**Commendation**
A meta-progression achievement. Tracks run telemetry (turns, chain count, hits taken,
credits held, damage per shot, kill distance, …). Some unlock the second class loadout
or new sectors. (HTML Part XII.)

**Content (resolver dependency)**
The `Content` struct passed alongside `Board` to the resolver: action lookup table plus
`spawnProjectile(kind, owner, board)`. Decouples the resolver from where the data
lives. (TS: `resolve.ts:24`.)

**Cooldown**
Per-action throttle, 0–8 pips. Resets to `cost.cooldownMax` on fire (hit or miss),
ticks down 1 at end of turn, blocks re-queue until 0. (HTML Part I; `types.ts:111`.)

**Corrupted**
A Patrol-4+ overlay on capital ships. Aura swap, bonus hull, and an additional
mechanic. The boss equivalent of the Elite layer. (HTML Part VIII, Part XI.)

## D

**Damage pipeline**
The five fixed steps every hit runs through: band falloff → subsystem modifiers →
target-lock ×2 → directional shield → hull. New balance levers slot into this list;
the order is invariant. (HTML Part XIII implementation order #3; `resolve.ts:139`.)

**DAMAGE effect**
The `kind: "DAMAGE"` effect. Each targeted cell with a ship runs through `applyDamage`.
Carries `amount` and optional `bandFalloff: false` to bypass step 1 of the pipeline.
(`types.ts:137`.)

**Decide enemy action**
The AI entry point — currently stubbed. Its design objective is to maximize the number
of distinct lane-ends the enemy threatens, manufacturing the rotation pressure on the
player. Picks actions; the same `executeQueue` then runs them. (HTML Part IV;
`resolve.ts:395`.)

**Deploy / DEPLOY effect**
The effect that drops a `Hazard` (mine / drone / debris) into a cell's hazard list.
Persists; triggers when a ship enters the cell. (`types.ts:144`, `resolve.ts:218`.)

**Deployed cell**
A targeting pattern (`DEPLOYED_CELL`). Places a hazard on the cell directly in the
bearing direction. (HTML Part III.)

**Direction to**
`directionTo(a, b)` — the lane end you must travel to get from `a` to `b`. Returns
`fore` if `b >= a`, else `aft`. The primitive that lets `applyDamage` compute incoming
direction and `firstTargetToward` walk the lane. (`geometry.ts:16`.)

**Displace self / DISPLACE_SELF**
The effect that moves the *acting* ship by some `MovementMode` (THRUST / BURN / SLIP /
JUMP / TRACTOR_SWAP) over `distance` cells. Collision rules differ per mode.
(`types.ts:140`.)

**Displace target / DISPLACE_TARGET**
The effect that moves the *targeted* ship — push, pull, or swap — over `distance`.
Collisions during the path deal collision damage. (`types.ts:139`.)

## E

**Effect**
A verb inside an Action's `effects[]` list. Closed set: `DAMAGE`, `APPLY_STATUS`,
`DISPLACE_TARGET`, `DISPLACE_SELF`, `REORIENT`, `SPAWN_ORDNANCE`, `VENT_HEAT`, `DEPLOY`,
`BOARD`. Adding a verb requires extending the enum and the dispatch in `applyEffect`;
new content using existing verbs is data only. (HTML Part II; `types.ts:136`,
`resolve.ts:167`.)

**Elite**
A Patrol-2+ layer on enemies: palette swap, bonus hull, and exactly one of
`EliteAgile`, `EliteAnchored`, `TwinLinked`, `ReactiveShield`, `Voidtouched`. Tender
and Bulwark hulls can never roll Elite — the engine checks at spawn. (HTML Part VII,
Part VIII.)

**End of turn**
The fourth phase of the round. Cooldowns tick down by 1, heat dissipates by 1, lockout
clears if heat has fallen below `heatMax`, statuses tick and decay, `onTurnEnd` fires.
(HTML Part I; `resolve.ts:254`.)

**Enemy initiative**
The order in which enemies act during phase 3 of the round. Telegraphed — the player
sees the badges before committing. Currently lane order, will become explicit.
(`resolve.ts:274`.)

**Event bus**
A simple `on(hook, fn)` / `emit(hook, ctx)` dispatcher carried by `Board`. The resolver
emits at fixed points; subsystems subscribe. (HTML Part VI; `types.ts:191`.)

**Execute queue**
The per-ship action loop. Walks the queue bottom → top, applying the arc + heat +
cooldown gate to each action, dispatching effects, ticking heat / cooldowns / lockout,
firing `onDamageDealt` per action and `onChainKill` at the end. The same code path
serves player and enemy. (`resolve.ts:53`.)

**Extreme**
The fifth (longest) range band, distance 7+. (`geometry.ts:36`.)

## F

**Facing**
See **Orientation**.

**Facing zone**
`facingZone(orientation, incomingFrom)` — which hull zone takes a hit arriving along the
lane. Bow-on: bow if the shot arrives from the direction the bow points, else stern.
Broadside: starboard if from fore, port if from aft (deterministic split). Flanks never
take a lane hit in bow-on stance. (HTML Part IV; `geometry.ts:61`.)

**Faction**
`player` or `enemy`. Used by `advanceProjectile` to decide what to hit, by `enemyInitiative`
to filter, and by future AI logic. (`types.ts:27`.)

**Field kit**
Items the player carries between fights. Resolve without spending a turn. Three groups:
Stims (repair/buff), Cards (board-wide status), Recon Die (reroll). Holds three by
default; expanded by Cargo Expansion. (HTML Part X.)

**Fore**
The lane end pointing toward higher cell index. Opposite of `aft`. (`types.ts:10`.)

**Free-fire**
Loading an action that has `cost.advancesTurn = false`. Does not end the turn. Vent,
maneuvers, the Autoloader mod, and most signatures are free-fire. The tempo lever.

## G

**General (bay)**
A subsystem bay that can appear in any shop. The catch-all category. (HTML Part VI.)

## H

**Hazard**
A per-cell feature with an ID, kind (mine / drone / debris), payload `Effect[]`, and
optional `ttl`. Triggers when a ship enters the cell. The trap analog from the original
game. (HTML Part I; `types.ts:40`.)

**Heat**
Per-ship resource pool. Every action adds `cost.heat`. Crossing `heatMax` sets
`lockedOut`, blocking further heat-positive actions until vent. The tempo throttle —
prevents alpha-strike every turn. (HTML Part I; `types.ts:57`.)

**Heat lockout**
The state of a ship whose heat has met or exceeded `heatMax`. While locked out, only
zero-heat actions can resolve. Cleared by `VENT_HEAT` or by passive dissipation
dropping heat below the threshold. (`types.ts:59`; `resolve.ts:57`, `:66`.)

**Heat threshold**
A subsystem hook (`onHeatThreshold`). Fires when heat crosses a configured level. Used
by Engineering subsystems that buff defense at high heat or auto-vent at the cap. (Not
yet wired in the TS.)

**Helm**
A subsystem bay — movement / displacement specialists. The Dancer analog. (HTML Part
VI.)

**Hook**
An event-bus tag: `passive`, `onChainKill`, `onTurnEnd`, `onVent`, `onWaveStart`,
`onHeatThreshold`, `onDamageDealt`, `onDamageTaken`, `onHeal`, `onReorient`, `onLethal`.
Subsystems register on exactly one. (`types.ts:176`.)

**Hook context**
The payload an emitter passes to subscribers: `{ board, source?, target?, amount?, ... }`.
(`types.ts:183`.)

**Hull**
(a) A ship's HP integer (`Ship.hull` / `maxHull`). When it hits 0 the ship is destroyed.
(b) A ship-type's effective starting hull at a given Patrol tier and Elite status:
`hull(type, patrol, elite)`. (HTML Part VIII; `types.ts:55`.)

**Hull breach**
A status (`hullBreach`) — 1 damage at end of turn for N turns. The poison/DoT analog.
Granted by Incendiary mod. (HTML Part VII; `resolve.ts:321`.)

**Hull zone**
One of four fixed armour faces welded to the hull: `bow` (strong), `stern` (weak),
`port` and `starboard` (medium). Which zone a hit lands on is decided by `facingZone`.
(HTML Part IV; `types.ts:19`.)

## I

**Icon**
A `string` field on `Action` and `Subsystem`. Either an SVG glyph (string of markup) or
a sprite URL — the renderer chooses how to render. (`types.ts:108`.)

**Initiative**
See **Enemy initiative**.

**In band**
`inBand(allowed, atkCell, targetCell)` — convenience wrapper that returns `true` iff
the target's range band is in the action's `band` list. (`geometry.ts:48`.)

## J

**Jump**
A `MovementMode`. Blink-drive teleport that ignores the path entirely — no collision
along the way, lands on the target cell if free. (HTML Part IV; `types.ts:147`.)

## K

**Klass**
The TS field name (avoiding the JS reserved word `class`) that dispatches the
Signature action by class identity. The Rust port can use `class` freely.
(`types.ts:66`.)

## L

**Lane**
The 1-D board. 5, 7, or 9 cells. Higher index is `fore`, lower is `aft`. One ship per
cell; ordnance and hazards overlay. (HTML Part I.)

**Lane end**
One of the two lane edges, named `fore` or `aft`. Used everywhere a direction matters:
bow points to a `LaneEnd`, projectiles travel toward a `LaneEnd`, arcs bear toward a
`LaneEnd`. (`types.ts:10`.)

**Long**
The fourth range band, distance 5–6. (`geometry.ts:35`.)

## M

**Maneuver**
An informal name for movement-archetype actions: Thrusters, Afterburner, Blink Drive,
Evasive Roll, Reverse Thrust. All use `SELF` targeting with a `DISPLACE_SELF` and/or
`REORIENT` effect. Most are free-fire.

**Mid**
The third range band, distance 3–4. (`geometry.ts:34`.)

**Mod**
A weapon modification. Attaches to one action (at most one per action, except the
free-fire-granting Autoloader, which can stack with one other), adds an effect, and
raises the action's cooldown by some delta. The enchantment analog. (HTML Part VII.)

**Mount**
A weapon hardpoint on a ship: `{ id, arc, weapon }`. Fixed at ship-design time. Arc
controls when the mount can fire given the ship's orientation. (`types.ts:76`.)

**Movement mode**
Five path rules for self-displacement: `THRUST` (1 cell), `BURN` (multi-cell, stops at
first obstacle), `SLIP` (passes through ships, lands beyond), `JUMP` (teleport ignoring
path), `TRACTOR_SWAP` (swap cells with a target). (HTML Part IV; `types.ts:147`.)

## O

**Onboard**
A general modifier category on a ship: traits + elite trait + statuses + active
subsystems. Used informally; not a code field.

**Optimal band**
The `Targeting.optimalBand` — the range band where the weapon deals full damage. Firing
outside applies band falloff. (HTML Part III; `types.ts:121`.)

**Ordnance**
(a) Live projectile entities on the lane — torpedoes and missiles. (b) The `ordnance`
archetype on `Action`. (c) The `ORDNANCE` targeting pattern that spawns one. Ordnance
advances during phase 2 of the round, can be shot down before impact, and detonates
applying its `payload`. (HTML Part I, Part X; `types.ts:151`, `resolve.ts:233`.)

**Orientation**
A ship's stance: `{ stance: "bowOn", bow: LaneEnd }` or `{ stance: "broadside" }`. The
primary tactical axis — controls which shields and mounts face the lane. (HTML Part
IV; `types.ts:14`.)

**Overheat**
See **Heat lockout**.

## P

**Passive (hook)**
A hook value meaning "subscribed permanently, not to any specific event." Subsystems
that compute on every read (HUD multipliers, etc.) use this.

**Patrol**
Global difficulty tier 1–7. Applied cumulatively per run. Drives enemy hull rolls,
elite roll rate, corruption roll rate, drop rates, and starting player hull. Stored on
`Board.patrol`. (HTML Part XI; `types.ts:36`.)

**Pattern**
See **Targeting pattern**.

**Point blank**
The first (closest) range band, distance 0–1. (`geometry.ts:32`.)

**Point-blank pattern**
`POINT_BLANK` — targets the cell directly ahead. (HTML Part III.)

**Port**
The left flank, a medium-armour hull zone (default armour 1). (`types.ts:19`.)

**Projectile**
An entity record for live ordnance: `{ id, kind, cell, heading, speed, hull, payload,
ownerFaction }`. Advances during phase 2, resolves on first non-owner ship.
(`types.ts:151`.)

**Pursuit**
A base enemy trait: after firing, moves toward the player. The Aggro analog. (HTML
Part VII.)

## Q

**Queue**
Per-ship action buffer, max 3 slots. Fires bottom → top during the ship's phase.
Loading costs the turn unless the action is free-fire; reordering and clearing are
free. (HTML Part I; `types.ts:62`.)

## R

**Range band**
Distance bucket: `pointBlank` (1), `close` (2), `mid` (3–4), `long` (5–6), `extreme`
(7+). Every weapon declares allowed bands and an optimal band; firing outside optimal
triggers band falloff. The new ranged economy. (HTML Part III; `types.ts:22`,
`geometry.ts:30`.)

**Reactor breach**
A base enemy trait: on death, deal 2 damage to adjacent cells. The Explosive analog.
(HTML Part VII; `resolve.ts:337`.)

**Rear**
A mount arc. Fires astern when bow-on, never when broadside. (`types.ts:25`.)

**Recon Die**
A field-kit item that rerolls intent — the Lucky Die analog. (HTML Part X.)

**Reorient**
A first-class movement verb (`REORIENT` effect). Flips a ship between stances or
rotates a bow-on ship 180°. Equally tactical with cell movement. Emits `onReorient`.
(HTML Part IV; `types.ts:141`, `resolve.ts:194`.)

**Requires arc**
The `Targeting.requiresArc` — the mount arc that must bear for the action to resolve.
`null` for arc-less actions (`SELF`, signature, vent). (`types.ts:121`.)

**Resolve round**
The top-level entry point: runs the four phases in order on a `Board`. The same code
path serves every turn. (`resolve.ts:31`.)

**Resolve targeting**
The closed dispatch over the eight targeting patterns, returning the cells an action
affects. (`resolve.ts:81`.)

## S

**Salvage**
Meta-progression currency. Dropped only by capital ships, scaling with Patrol tier.
Used to unlock subsystems from astrogation. (HTML Part VIII, Part XI.)

**Sector**
A node in the campaign branching graph. A run is a path through sectors. Each sector
introduces specific enemies to the global spawn pool and ends in a capital fight.
(HTML Part XI.)

**Self pattern**
`SELF` — targets the acting ship's own cell. Used for vent, maneuvers, reorient.
(HTML Part III.)

**Shield face**
A hull zone's defense record: `{ armour, charge }`. Armour is permanent and
subtractive; charge is a consumable held shield ping. (HTML Part IV; `types.ts:71`.)

**Shield profile**
A ship's `Record<HullZone, ShieldFace>` — the full per-zone defense table. Fixed to
the hull; orientation chooses which zone faces a hit, not the values. (`types.ts:60`.)

**Shields up**
A status meaning "a held shield charge is active on `face`." Tick down with duration.
(HTML Part VII; `types.ts:92`.)

**Ship**
A unit on the board: cell, faction, orientation, hull, heat, shield profile, mounts,
queue, cooldowns, statuses, traits, optional class. Same shape for player and enemy.
(HTML Part VIII; `types.ts:50`.)

**Signature**
A class-specific free-fire action dispatched by the ship's `klass` field. The hero
"special move" analog (Slip, Ram, Phase, Throw, Swap Toss, …). (HTML Part IX.)

**Skips turn**
A predicate: does this ship have `systemsOffline` status? If so, its phase is skipped.
(`resolve.ts:330`.)

**Slip**
A `MovementMode`. Passes through ships, lands in the first free cell beyond.
(`types.ts:147`.)

**Spawn ordnance**
The effect (`SPAWN_ORDNANCE`) that pushes a new `Projectile` into `board.ordnance` via
`content.spawnProjectile(kind, owner, board)`. (`types.ts:142`.)

**Spinal line**
A targeting pattern (`SPINAL_LINE`). Hits a line of targets in the bearing direction;
pierces all if `hitsAll`, otherwise first only. (HTML Part III.)

**Stance**
A ship's orientation type discriminator — `"bowOn"` or `"broadside"`. (`types.ts:14`.)

**Stance affinity**
Per-class metadata indicating which orientation the class's kit is built around.
(HTML Part IX.)

**Starboard**
The right flank, a medium-armour hull zone (default armour 1). (`types.ts:19`.)

**Status**
A transient unit modifier with a duration: `hullBreach`, `systemsOffline`, `targetLock`,
`shieldsUp`. Tick down at end of turn; duplicates take the max duration via
`addStatus`. (HTML Part VII; `types.ts:82`, `resolve.ts:313`.)

**Stern**
The back of a ship, the weakest hull zone (default armour 0). In bow-on stance, the
stern faces whichever lane end the bow is not pointing at. (`types.ts:19`.)

**Stims**
A field-kit group: repair / shield / coolant. The potion analog. (HTML Part X.)

**Subsystem**
A passive event subscriber bought between sectors. Never queues, takes a turn, or
carries a cooldown. Registers an `apply(ctx)` against one `Hook` and modifies behavior
from the side. New content, not new code. (HTML Part VI; `types.ts:164`.)

**Systems offline**
A status that skips the affected ship's next turn(s). The freeze/ice analog. Granted
by EMP Burst, EMP Charge mod, and Grav Snare. (HTML Part VII; `types.ts:90`.)

## T

**Target lock**
A status that doubles the next incoming hit, then consumes itself. Sits between
subsystem modifiers and directional shields in the damage pipeline. The curse analog.
Granted by Targeting Marker and Targeting Laser mod. (HTML Part VII; `resolve.ts:149`.)

**Targeting**
The Action sub-record `{ pattern, band, optimalBand, requiresArc, facingRelative,
hitsAll }`. Drives `resolveTargeting`. (`types.ts:117`.)

**Targeting pattern**
The closed set of eight cell-selection rules: `POINT_BLANK`, `SPINAL_LINE`, `BEAM`,
`BROADSIDE`, `BLAST`, `ORDNANCE`, `SELF`, `DEPLOYED_CELL`. (HTML Part III;
`types.ts:130`.)

**Telegraphed**
Said of any ordering the player sees before committing — enemy initiative, ordnance
path, AI intent. Never hidden. (HTML Part I.)

**Tender**
A ship hull type. Cannot roll Elite — the engine excludes the combination because the
kit conflicts with the elite set. (HTML Part VII.)

**Thrust**
A `MovementMode`. Exactly one cell; blocked by occupancy. (`types.ts:147`.)

**Tick**
The end-of-turn step that decrements cooldowns, heat, and status durations. (Not a
named function — see `endOfTurn` and `tickStatuses`.) (`resolve.ts:254`, `:319`.)

**Trait**
A base enemy modifier: `Pursuit`, `Agile`, `ReactorBreach`, `BurnHard`, `Anchored`.
Intrinsic to the enemy type, distinct from the Elite layer. (HTML Part VII;
`types.ts:94`.)

**Tractor swap**
A `MovementMode`. Trades cells with a target. (`types.ts:147`.)

**TTL**
Time-to-live on a hazard. Optional turns-until-removal counter. (`types.ts:45`.)

**Turret**
A mount arc. Always bears — fires regardless of orientation. (`types.ts:25`,
`geometry.ts:76`.)

## V

**Vent**
The `VENT_HEAT` action and effect. Clears heat (by `amount`), unlocks an overheated
ship, optionally recharges *all* cooldowns. The Shogun Showdown WAIT analog. Emits
`onVent`. (HTML Part I; `types.ts:143`, `resolve.ts:185`.)

**Voidtouched**
A Patrol-7-only Elite trait: on death, spawns a Void Progeny. (HTML Part VII.)

## W

**Wave start**
A hook (`onWaveStart`) fired when a wave spawns. Not emitted by the resolver itself —
the runner / scenario layer fires it.

**Weapon**
Informal — an `Action` of archetype `beam`, `ordnance`, `broadside`, or `displacement`.
Distinct from movement, defensive, and control actions.

**Weapon archetype**
See **Archetype**.

## Z

**Zero-heat**
An action with `cost.heat == 0`. Resolvable while locked out — the only way an
overheated ship can act. Vent and Reverse Thrust are the canonical examples. (TS:
`resolve.ts:57`.)
