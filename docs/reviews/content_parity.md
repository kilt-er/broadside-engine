# Content parity self-audit vs TypeScript reference

**Task:** #124 / #28 (content parity self-audit, read-only)
**Auditor:** broadside-content
**Scope:** the content systems content owns — enemy AI, displacement bodies,
subsystem modifier math, chain-kill detection, ordnance spawn/advance — checked
for behavioral drift against the canonical TypeScript reference engine
(`_drive_pull/broadside-engine/engine/resolve.ts`).
**Constraint:** read-only. No code was changed. This is a findings list for the
lead to triage into fixes vs. intentional port-time divergences.

## Verdict

Content systems are **faithful** to the TS reference. **Zero behavioral bugs
found.** One low-severity *structural* difference (no observable behavioral
effect today) is noted for the lead's call.

Note on methodology: several of these systems are TS `TODO` stubs that the Rust
port filled natively (per the original task #6 brief). For those there is no TS
*line* to drift from — the reference is the stub's stated intent plus the
analysis-doc design. Those are marked "faithful to intent."

## Findings by system (all faithful unless noted)

### 1. Chain-kill detection — FAITHFUL
- Rust: `src/resolve.rs:1746` `detect_chain` → `board.destroys_this_window >= 2`.
- TS: `resolve.ts:346-349` `detectChain` is a TODO returning `false`, intent
  documented as "count destroys within this execution window; >=2 is a chain
  kill."
- Rust implements the stated intent exactly: `destroys_this_window` is reset on
  queue entry, incremented in `destroy()`, read after the queue runs.

### 2. Subsystem modifier math — FAITHFUL
- Rust: `src/resolve.rs:1125` `apply_modifiers` → `Content::damage_modifier`;
  per-subsystem math in `src/subsystems.rs:142` `damage_modifier_for`.
- TS: `resolve.ts:147` `applyModifiers` is a TODO ("subsystem bonuses").
- Pipeline ORDER matches TS `applyDamage` (`resolve.ts:143-158`): band-falloff
  → modifiers → targetLock ×2 → directional shield → hull. Modifiers are
  additive, clamped ≥ 0, and applied BEFORE the lock doubling (so a +1 then ×2
  is `2*(raw+1)`), matching the TS comment ordering.
- Direction is attacker-side (audit #67) — correct per analysis-doc §VI.

### 3. Destroy / ReactorBreach splash — FAITHFUL
- Rust: `src/resolve.rs:1018` `destroy`.
- TS: `resolve.ts:334-344` `destroy`.
- Both: clear the cell, and if the ship has `ReactorBreach`, splash 2 damage to
  each neighbour (`idx-1`, `idx+1`, bounds-checked) via `dummy_weapon()`
  (`bandFalloff: false`), then emit the lethal hook.
- Rust additionally does `destroys_this_window += 1` — this is the intended
  chain-count fill (the consumer of TS's `detectChain` TODO), not drift.

### 4. Ordnance advance / impact — FAITHFUL
- Rust: `src/resolve.rs:439` `advance_projectile`.
- TS: `resolve.ts:233-250` `advanceProjectile`.
- Both: step `speed` cells in heading; remove on off-lane; on a non-owner
  occupant, drop each payload effect (DAMAGE via `apply_damage`, APPLY_STATUS
  via `add_status`) and remove the projectile. Both bill impact DAMAGE as
  `apply_damage(impact_cell, amount, impact_cell, …)` — target cell == attacker
  cell, so `directionTo` yields `fore` consistently on both sides. Both ignore
  non-DAMAGE/non-APPLY_STATUS payload effects on impact.

### 5. Displacement bodies — FAITHFUL TO INTENT (no TS line)
- Rust: `src/resolve.rs` `resolve_self_move` (THRUST/BURN/SLIP/JUMP/
  TRACTOR_SWAP) and `resolve_target_move` (push/pull/swap).
- TS: `resolve.ts:209` and `resolve.ts:215` are both TODOs — the Rust IS the
  canonical implementation here.
- Collision rule (stop one cell short of a block, take `remaining_distance × 1`
  collision damage routed through the damage pipeline so the directional shield
  mediates) matches the analysis-doc spec and the content role brief. No TS
  line to drift from.

### 6. Enemy AI (`decide_enemy_action`) — FAITHFUL TO INTENT (no TS line)
- Rust: `src/resolve.rs:1512` `decide_enemy_action`.
- TS: `enemyInitiative` (`resolve.ts:274-277`) is a lane-order stub; there is no
  TS scoring body — the AI is a native fill of the analysis-doc directive
  ("maximise the number of distinct lane-ends it threatens", analysis HTML
  ~line 499-500). Scoring (+10 hits-player, +6 uncovered lane-end, +raw dmg,
  −heat, trait nudges) + reorient/move/vent fallback ladder is faithful to that
  intent. Friendly-fire filter (#49) is an intentional AI-side addition, not a
  pipeline change.

## The one note (LOW severity — structural, not a behavioral bug)

**`tick_statuses` hullBreach application is batched, TS is interleaved.**
- Rust: `src/resolve.rs:977-1000` — sums all `HullBreach` statuses into
  `breach_hits`, applies `hull -= breach_hits` once, then checks death once,
  then decrements every status' duration.
- TS: `resolve.ts:319-328` — loops statuses; for EACH `hullBreach`, does
  `ship.hull -= 1` and an interleaved `if (ship.hull <= 0) destroy(...)` inside
  the loop, then `s.duration -= 1` per status.
- **Impact:** net hull arithmetic and death outcome are identical for 0 or 1
  hullBreach stacks. They could only diverge with 2+ simultaneous hullBreach
  stacks on one ship — which is **not producible today**: `add_status`
  (`resolve.rs:963`) de-dups by `kind` (refreshes duration via `max`), so a ship
  can hold at most one `HullBreach`. So this is a structural difference with no
  observable behavioral effect.
- **Recommendation:** leave as-is (the batched form is cleaner and equivalent).
  If bit-exact TS structure is wanted, it's a small `resolve.rs` edit and is the
  resolver's call, not content's. Flagged, not fixed.

## What was NOT changed

Nothing. This audit touched no source files. All findings above are for the
lead to triage.
