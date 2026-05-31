# `src/cards.rs` — field-kit Cards runtime layer

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/cards.rs`](../LINE_BY_LINE.md#srccardsrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

Cards are **tempo-free consumables** a ship carries in its field kit (Phase 2,
task #63). Playing one consumes a charge and resolves a board-wide effect — lock
every enemy, breach every enemy, clear every enemy's queue. This module owns the
card catalog, the per-ship inventory, the play-validation gate, and the board
dispatch.

The key design decision is the **`Effect::BOARD { note }` indirection** (module
docstring, src/cards.rs:9-21). A card's behavior ("every ship matching predicate
P") doesn't fit any existing per-cell `Effect` variant. Rather than grow `Effect`
with a variant per card, the team kept the single `Effect::BOARD { note: String }`
shape and dispatches by note string in the content layer: a card play queues a
synthetic action whose only effect is `BOARD { note: <card_id> }`; when the
resolver hits that effect it calls `Content::apply_board_effect(note, …)`, which
this module's `apply_card_effect` interprets. New cards = a new match arm here, no
resolver or type-surface change. The TS engine's `Effect::BOARD` carries the same
`{ note }` shape, so the indirection matches the canonical reference.

Per-ship inventories and the catalog live on [`DemoContent`](input.md) until the
architect lands `Ship::field_kit` / `Catalog::fieldkit` — a lead-authorized
placeholder. No TS analog for the runtime layer.

---

## Catalog + inventory types (src/cards.rs:47–146)

- `Card` (src/cards.rs:47) — `{ id, name, cost }`. The BOARD `note` is conventionally
  the same id. Placeholder cards cost 1 charge.
- `CardCharge` (src/cards.rs:58) — one inventory entry `{ card_id, charges }`.
- `FieldKit` (src/cards.rs:66) — a ship's `Vec<CardCharge>`. `grant` (src/cards.rs:77)
  accumulates charges if the card is already held; `find`/`find_mut` look up by id.
- `FieldKitRegistry` (src/cards.rs:101) — `HashMap<ship_id → FieldKit>`, owned by
  DemoContent. `grant` creates the kit if absent; `for_ship`/`for_ship_mut` look up.
- `CardCatalog` (src/cards.rs:130) — `HashMap<id → Card>` for cost lookup at
  play-validation time.

**Worked examples:** `field_kit_grant_then_find` (src/cards.rs:358, charges
accumulate), `registry_grants_to_named_ship_only` (src/cards.rs:369).

---

## Placeholder cards (src/cards.rs:152–178)

Three `const` ids — `CARD_MASS_LOCK`, `CARD_MASS_BREACH`, `CARD_SENSOR_PULSE` —
listed in `PLACEHOLDER_CARD_IDS`. `placeholder_catalog` (src/cards.rs:164) builds
the three `Card`s (cost 1); `grant_placeholder_kit` (src/cards.rs:174) grants one
charge of each to a ship (used by the demo setup). Pinned by
`grant_placeholder_kit_yields_three_cards` (src/cards.rs:380).

---

## `fn apply_card_effect(note, source_cell, board)` (src/cards.rs:191)

**Intent:** The board-dispatch the `BOARD` effect arm calls through `Content`. Line
196-203: derive the **target faction** from the source ship's faction (Player plays
hit Enemy and vice-versa — faction-symmetric, not enemy-only; an empty source cell
defaults to Player operator). The `match note`:
- `CARD_MASS_LOCK` (210) — apply `TargetLock` (duration 1) to every opposite-faction
  ship. Duration 1 because the lock is "next incoming hit doubled," consumed on hit.
- `CARD_MASS_BREACH` (223) — apply `HullBreach` (duration 3) to every opposite-faction
  ship, taking the **max** of any existing breach duration.
- `CARD_SENSOR_PULSE` (237) — clear every opposite-faction ship's queue (one-turn
  relief; the AI re-fills next turn — not a permanent silence).
- `_` (248) — unknown note → silent no-op.

`add_or_extend` (src/cards.rs:252) is the status helper: extend an existing status's
duration (max) or push a new one.

**Cross-references:** Called by [`DemoContent::apply_board_effect`](input.md), which
the resolver invokes from the `Effect::BOARD` arm of [`apply_effect`](resolve.md).
**Worked examples:** `mass_lock_applies_target_lock_to_every_enemy` (src/cards.rs:439,
player NOT locked by their own card), `mass_breach_applies_hull_breach_to_every_enemy`
(src/cards.rs:462), `sensor_pulse_clears_every_enemy_queue` (src/cards.rs:474) +
`sensor_pulse_does_not_clear_player_queue` (src/cards.rs:487),
`enemy_played_card_targets_player` (src/cards.rs:501, faction symmetry),
`unknown_note_is_silent_no_op` (src/cards.rs:521).

---

## `enum PlayResult` + `fn try_play_card(reg, cat, ship_id, card_id)` (src/cards.rs:266, 284)

**Intent:** The play-validation gate. `PlayResult` is `Played` / `NotCarried` /
`InsufficientCharges` / `UnknownCard`. `try_play_card` looks up the card's cost in
the catalog, finds the ship's inventory entry, checks `charges >= cost`, decrements,
and returns `Played`. **Splitting validation from queueing** is deliberate: the
queue stays a pure `Vec<String>` of action ids — no card-specific resource
bookkeeping leaks into the resolver. The caller (the bin's `PlayCard` intent arm)
pushes the synthetic `__card_<id>` only on `Played`.

**Cross-references:** Called by [`DemoContent::try_play_card`](input.md) from
[`broadside.rs::apply_intent`](broadside.md)'s `PlayCard` arm. **Worked examples:**
`try_play_unknown_card_rejected` (src/cards.rs:394), `try_play_not_carried_rejected`
(src/cards.rs:405), `try_play_insufficient_charges_rejected` (src/cards.rs:415),
`try_play_success_decrements_charges` (src/cards.rs:426).
