# Review: src/subsystems.rs (Phase-2 runtime subsystem layer)

Reviewer audit (task #9, extended pass). Rust-native Phase-2 module — no 1:1
TS counterpart. Audited for internal correctness + consistency with the
canonical mechanics (analysis-HTML subsystem descs) + the post-#67 direction.
Status: **APPROVE.** No findings.

## Verified

- **Attacker-side direction (post-#67)** — `damage_modifier_for(installed, attacker, band, board)` sums bonuses over the caller-supplied installed list, which the resolver populates from the ATTACKER's subsystems (apply_modifiers looks up atk_cell). Correct per the audit: descs are attacker-frame ("+1 when firing"). The `_attacker`/`_board` params are unused — band-only dispatch — which correctly means board-state-predicate subsystems (a hypothetical Crossfire) can't be expressed yet; resolve.rs's trait doc already scopes that out. Consistent.
- **Marksman** +1 @ Long, **Point-Blank Doctrine** +2 @ PointBlank — match the analysis-doc flat bonuses. Additive stacking (no multiplicative tier) matches the resolve.rs apply_modifiers contract. Clamp-at-0 happens in apply_modifiers, not here — correct layering.
- **HeatSink on_turn_end_for** — subtracts 1 extra heat/install AFTER base dissipation, floors at 0, clears lockout when heat < heat_max (mirrors the end_of_turn invariant), stacks additively. Called between base dissipation and the OnTurnEnd emit — correct ordering.
- **Installations** registry — ship_id -> Vec<SubsystemId>, install/for_ship. Commutative effects, order-independent. 11 tests cover install/lookup, per-band gating, additive stacking, heat floor, lockout clear, stacking, and no-subsystem no-op. Strong coverage.

## Cross-module naming collision (FLAGGED — latent, low-sev)

`overcharge` is overloaded across modules:
- meta.rs:275 — a meta-unlockable SUBSYSTEM (threshold 50). Canonical catalog confirms `overcharge` exists ONLY as a gunnery subsystem.
- classes.rs:75 — `SIG_OVERCHARGE = "overcharge"`, the Vanguard placeholder Signature ACTION.

No live collision today (canonical has no `overcharge` action; the canonical Vanguard signature is `slip`, and classes.rs is explicitly the placeholder set destined for replacement). But if the placeholder class signatures get registered into the same Content::action registry alongside the canonical `overcharge` subsystem unlock, the string id is overloaded across two different game-object kinds. Flagged to architect/content: the placeholder signature should not reuse a canonical subsystem id — rename SIG_OVERCHARGE (e.g. `vanguard_overcharge`) before the canonical roster lands. Not blocking.
