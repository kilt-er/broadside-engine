# C1 — 2D Enemy AI Ladder Spec

**Status:** SIGNED OFF (team-lead, 2026-06-14). Implementation gated on A3.1
(the additive Pos/Facing type-surface step); the `decide_enemy_action` rewrite
+ `queue_close_or_open` helper resume on top of it.
**Author:** broadside-content (C1).
**Reviewers/consumers:** resolver (seam owner), renderer (telegraph), tester
(T5 AI tests), reviewer.

This is the design of record for the 2D enemy AI decision layer. It is a
*rewrite* of the v1 `decide_enemy_action` ladder (`src/resolve.rs`), lifted from
the 1-D lane to the 5×4 Chebyshev grid, honoring blueprint decisions #6/#7/#8/#9
(`docs/design/BROADSIDE_V2_BLUEPRINT.md`).

## 0. Coordinate frame (from `src/grid.rs`, fixed)

- 5 columns (lateral, the dodge axis) × 4 rows (depth).
- `row 0` = far/back row (where enemies spawn); `row ROWS-1` = front row
  (nearest the player/camera).
- `Dir8::S` (`+row`) points **toward** the player; `Dir8::N` (`-row`) points
  away. `E`/`W` are `±col`.
- Range is 3-band Chebyshev (`grid::range_band`): `Adjacent` = distance 0–1,
  `Near` = 2, `Far` = 3+. Per-band damage falloff `[1.0, 0.6, 0.3]` is the
  resolver's to apply; the AI only reads the band bucket and the `in_band` gate.

## 1. Design objective

The AI is a **decision layer that builds an enemy's action queue, then lets the
resolver's `execute_queue` / `run_world_phase` run it UNCHANGED** — it never
bypasses the pipeline, never re-orders phases, and queues only real catalog
actions (or the resolver-served synthetic move). Its two non-negotiable goals:

1. **Keep over-extension a real threat** (decision #7) — see §3.
2. **Produce a readable telegraph every live phase** (the liveness invariant) —
   see §4.

### Telegraph timing (precise mechanics)

Within the enemy phase the order is **decide-then-execute per enemy**, walked in
`enemy_initiative` order: `decide_enemy_action` fills the enemy's queue, then the
resolver executes it. The **"telegraph one turn ahead" property is CROSS-TURN**:
after an enemy executes, its queue persists holding its *next* intended action,
so on the **player's** turn the complete next-turn threat set is already known
and painted. The net player-facing behavior — you always see an enemy's next
action before it lands — is exactly as the ladder below intends.

## 2. The ladder (priority order; first match wins)

Mirrors v1 §4–5d. Per enemy, after locating the player cell:

### Rung 1 — FIRE (commit when able)

Enumerate the enemy's mount weapons; for each, apply the same gates v1 uses, now
in 2D:

- **cooldown** clear (`cooldowns[weapon] == 0`);
- **heat/lockout**: if locked out, only zero-heat actions are eligible;
  otherwise the action must not push heat past `heat_max + 1` (happy to overheat
  exactly once);
- **arc + band + deadzone gate** = `let cells = resolve_targeting(action, board,
  enemy_pos); !cells.is_empty()`. This is THE single source the ThreatMap caches
  (no second targeting path for telegraphs — reviewer V4 enforces), so what the
  AI elects to fire is exactly what the telegraph paints.
- **friendly-fire filter** (task #49 carried forward): the target set must
  contain ≥1 non-enemy-occupied cell. The pipeline still *permits* friendly fire
  (the "Unfriendly Fire" mechanic), but the AI won't elect it unprompted.

**Score** (argmax wins):

- `+10` if the player's cell is in the target set;
- `+ Σ raw DAMAGE amount` of the action's effects;
- `− heat` cost (`− heat/2` if the enemy has the `BurnHard` trait — less
  heat-averse);
- `+2` if `Pursuit` and the action hits the player (commit to firing over
  positioning when both are available).

**If any action scores, queue the best one and STOP.** This preserves the #71
ruling — "fire when in position" beats "hold fire to maybe pressure a better
angle" — the fix for the v1 "march in a line, never shoot, die" bug.

### Rung 2 — CLOSE / HOLD-RANGE (the 2D over-extension decision)

Reached only if the enemy **cannot fire** this turn **and is not locked out**
(a locked-out enemy prefers to VENT, Rung 4, so it can fire again rather than
mindlessly maneuvering). Replaces v1's 1-D `queue_purposeful_maneuver` with a
2-D `queue_close_or_open`. The decision keys off the enemy's **dominant weapon**
(its highest-damage mount) and *why* that weapon is currently inert:

- **Inert because the player is TOO CLOSE** (player at `Adjacent` but the
  weapon's band set excludes `Adjacent` — the decision-#7 deadzone): **do NOT
  advance.** Queue a maneuver that **OPENS** distance — one step along
  `from_to(player_pos, enemy_pos)` (away from the player, toward `row 0`) — or
  holds, so distance reopens and the long-range gun comes back online next
  phase.
- **Inert because the player is TOO FAR** (out of the weapon's max band) **or
  OFF-ARC**: queue a maneuver that **CLOSES** — one `Dir8` step that reduces
  Chebyshev distance toward the weapon's optimal-band window (toward the nearest
  in-band, on-arc firing cell — not necessarily onto the player).
- The maneuver is a **telegraphed queued action** (a `DISPLACE_SELF`-bearing
  synthetic move, resolver-served like v1's `__move_left` / `__move_right` so it
  resolves without any `Content` dependency), so the player sees the move-arrow
  one phase ahead and can react.
- **`Anchored` trait → skip this rung entirely** (immune to self-displacement;
  falls through to Rung 3/4).

### Rung 3 — REORIENT

If it can neither fire nor usefully maneuver, queue a `REORIENT`-bearing action —
turning the bow/broadside may bring the player into a forward/broadside arc next
phase. The AI only *picks* the reorient; the 2-D arc/zone math lives in the
resolver's `arc_bears` / `facing_zone` rewrite.

### Rung 4 — VENT

Locked-out, or nothing above is viable → queue a `VENT_HEAT` action so next
phase is live again.

### Rung 5 — empty queue

Only reachable by a misconfigured enemy with no valid mount. A vented /
reoriented / moving enemy still satisfies liveness (its queued move-arrow or
reorient/vent glyph is a visible non-damage telegraph).

## 3. How over-extension survives in 2D (decision #7 — load-bearing)

v1 got this for free on a line: a long-range gun at mid-lane is inert at
point-blank. In 2-D it must survive across Chebyshev bands **and** the AI must
not "fix" it by always charging in. Two mechanisms, working together:

1. **Gate reuse (passive).** The AI's fire gate IS `resolve_targeting`, which
   honors the deadzone at exactly one enforcement point —
   `in_band(&weapon.band, enemy_pos, player_pos)`, where a `Far` weapon's band
   set excludes `Range::Adjacent`. So a `Far` enemy the player has closed on
   returns an empty target set → cannot fire → correctly inert. **There is no
   separate min-range check, and the AI has no parallel logic** — it reads the
   same seam the resolver and ThreatMap do.
2. **Rung-2 active back-off.** An inert-because-too-close enemy elects to OPEN
   range, never to advance. So over-extension is a genuine positional play: a
   player who dives past a `Far` enemy to kill a `Near` one buys a phase of
   safety from the `Far` gun, and that enemy spends the phase re-opening to
   re-threaten — visible and readable.

**A long-range enemy backing off to re-open range READS AS INTENDED, not a bug**
(team-lead, signed off). It is the visible payoff of the over-extension
mechanic and the explicit fix for the "close-only AI weakens the check" nuance.
The renderer telegraphs it as a queued move-arrow (away from the player) and
draws it as intentional repositioning.

## 4. Telegraph / visible-threat (quality bar + liveness invariant)

- Every rung leaves a queued, **un-fired** action → the renderer's per-enemy
  telegraph draws it: red threat-fill under the target cells for FIRE (via the
  cached ThreatMap), a move-arrow for CLOSE / back-off, a reorient/vent glyph
  otherwise. **No live phase is silent.**
- **threat == hit under a no-op player**: FIRE queues the exact action that
  `resolve_targeting` re-resolves next phase (single source). If the player
  moves, the threat legitimately shifts — the invariant is asserted only under a
  no-op player (tester T4).
- **Dodge-whiff**: when the player vacates a telegraphed cell, the queued shot
  resolves onto an empty cell → the resolver's `hit:false` emission (R7) → the
  renderer draws a whiffed beam into the vacated cell. The AI does nothing
  special; it simply queued an honest shot.

## 5. Trait hooks (carried forward from v1)

- **Pursuit** — `+2` score to firing at the player (commit to the shot).
- **BurnHard** — heat counts half in scoring; can still pick the CLOSE rung
  while warm.
- **Anchored** — skips the CLOSE/back-off rung (immune to self-displacement).

The **Coward "flee-when-approached"** hook (`docs/design/capital_distinctiveness.md`,
PARKED) would be a Rung-2 variant — back off whenever the player is within N
regardless of band. **It is explicitly out of C1 scope** and stays parked
pending a separate greenlight; do not expand C1 to cover it.

## 6. Boundaries honored

- Builds queues only; never bypasses `execute_queue` / `run_world_phase` / the
  damage-pipeline order.
- Requests no type-surface change — consumes `Pos` / `Dir8` / `Facing` /
  `Range` / `Trait` as landed in `grid.rs` / `types.rs`.
- The synthetic AI move stays **resolver-served**, so the AI has no `Content`
  dependency to close or back off.

## 7. Confirmed seams (resolver, 2026-06-14)

The implementation builds on these (planned R3 shapes — the mechanical
`cell:usize → Pos` / `Vec<usize> → Vec<Pos>` swap of the existing 1-D fns):

- `resolve_targeting(a: &Action, board: &Board, ship_pos: Pos) -> Vec<Pos>` —
  board-state-dependent, arc+band gated, pure; THE single source the ThreatMap
  caches. (Arg order: action, board, pos; it looks the ship up on the board
  internally.)
- Deadzone enforced **only** via `in_band(&weapon.band, enemy_pos, player_pos)`.
- `enemy_initiative(board: &Board) -> Vec<Pos>` — stable; the seam where C2
  (cross-enemy threat-SPREAD, the #74 feature) assigns enemies to distinct
  bearings.

## 8. Implementation scope

C1 lands a full rewrite of `decide_enemy_action` plus its 2-D maneuver helper
(`queue_purposeful_maneuver` → `queue_close_or_open`), pure where possible, on
top of A3.1. **C2** (cross-enemy threat-spread over `enemy_initiative`) is the
explicit follow-up and is *not* folded into C1.
