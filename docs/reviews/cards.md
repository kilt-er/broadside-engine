# Review: src/cards.rs (Phase-2 field-kit Cards)

Reviewer audit (task #9, extended pass). Rust-native Phase-2 module. The
BOARD-note dispatch indirection matches the TS `Effect::BOARD { note }` shape.
Status: **APPROVE.** No findings.

## Verified

- **BOARD-note dispatch** — cards resolve through a synthetic action whose only effect is `Effect::BOARD { note }`; `apply_card_effect` dispatches by note. Matches the TS Effect::BOARD shape and the lead's #63 "don't grow Effect per card" decision. New card = new match arm, no resolver/type change.
- **Faction symmetry** — source faction determines "the enemy"; an enemy-played card targets the player (tested `enemy_played_card_targets_player`), and a card never hits its own side. Source-cell-empty defaults to Player operator (documented; demo never plays from a dead ship).
- **Card behaviors** — mass_lock (TargetLock dur 1, consumed-on-hit so dur only matters if no follow-up), mass_breach (HullBreach dur 3), sensor_pulse (clear enemy queues — one-turn relief, AI re-fills next turn). `add_or_extend` uses duration-MAX (not amount-stack), matching the resolver's add_status semantics.
- **try_play_card** — UnknownCard (not in catalog) / NotCarried (no kit or no entry) / InsufficientCharges (< cost) / Played (decrements). Validation split from queueing keeps the queue a pure Vec<String>. Tested all four outcomes.
- **FieldKit / FieldKitRegistry / CardCatalog** — grant accumulates charges (saturating), per-ship keyed, spent cards stay at 0 charges (serialize as "spent"). 15 tests cover the surface.

## Notes

- `add_or_extend` (cards.rs:252) duplicates the resolver's add_status duration-max logic. Necessary DRY exception: the BOARD path applies statuses directly without routing through the resolver's add_status (module boundary), so a small duplication is the right call over coupling cards.rs to resolve internals. Behavior is identical — verified.
- Storage lives on DemoContent (FieldKitRegistry) until `Ship::field_kit` lands — lead-pre-authorized placeholder, documented. Consistent with the subsystems.rs registry pattern.
