# `src/input.rs` — key → Intent mapping + the demo Content layer

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/input.rs`](../LINE_BY_LINE.md#srcinputrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **input plumbing** that turns a keypress into something the resolver
understands, plus the demo's `Content` implementation. It sits between the winit
binary ([`broadside.rs`](broadside.md)) and the engine core
([`resolve.rs`](resolve.md)): the bin translates winit keycodes to this module's
framework-agnostic `Key` enum, this module maps `Key` → `Intent` → action id, and
the resolver fires the resulting queue. Keeping it `pub` but winit-free means the
library never depends on winit — the bin owns that translation.

It also defines **`DemoContent`**, the concrete `Content` impl the demo runs on:
a registry of synthetic actions (move/flip/vent/card) plus the demo's two mount
weapons, wired to the subsystem and field-kit registries. Until the JSON catalog
flow fully replaces it, `DemoContent` *is* the engine's content for the running
demo.

No direct TS analog — TS handled input inline in browser event handlers.
Phase-1+ Rust-side plumbing.

### The three-layer flow

```
winit KeyCode → (bin) keycode_to_key → input::Key
                                          │
                              key_to_intent(key, ship, content)
                                          │
                                        Intent
                                          │
                  intent_to_action_id  ──┴──  apply_intent (bin) handles
                  (synthetic/real id)         CommitTurn / Restart / PlayCard
                                          │
                                  pushed to ship.queue → resolver fires
```

---

## `enum Key` (src/input.rs:47)

**Intent:** A framework-agnostic key identity — one variant per binding the
tutorial advertises (`Left`/`Right`/`Tab`/`V`, **`Q`/`E`** for rotation (#75), digits
`D1`-`D3` for mounts and `D5`-`D7` for cards, `R`/`Space`/`Enter`). The bin maps
`winit::KeyCode` onto this (e.g. `KeyQ → Key::Q`) so the lib never imports winit. Adding a
binding touches three places in lockstep: `Key`, `key_to_intent`, `tutorial_lines`.

## `enum Intent` (src/input.rs:80)

**Intent:** What the player *meant*. `QueueAction(String)` carries a real action id
(a mount's weapon); `MoveLeft`/`MoveRight`/`MoveUp`/`MoveDown`/`ReorientFlip`/`Vent`/
**`RotateLeft`/`RotateRight`** are the synthetic actions; `PlayCard(String)` plays a
field-kit card; `CommitTurn` fires the queue; `Restart` rebuilds the board. The
doc-comments spell out each synthetic's effect (e.g. `Vent` → `VENT_HEAT 3, recharge
cooldowns`; `RotateLeft`/`RotateRight` → `REORIENT::RotateLeft`/`RotateRight`, a ±90°
turn of `facing`). `Key::Q → RotateLeft`, `Key::E → RotateRight` (key_to_intent,
src/input.rs:158).

---

## `fn key_to_intent(key, ship, content) -> Option<Intent>` (src/input.rs:127)

**Intent:** The canonical binding table. Most keys map to a fixed intent; the
digit keys are **gated by inventory** — `D1`/`D2`/`D3` resolve to
`ship.mounts[0/1/2].weapon` only if that mount exists (`mount_action`, src/input.rs:144),
and `D5`/`D6`/`D7` resolve to `content.card_at(ship.id, 0/1/2)` only if that card
slot exists with charges. An unbound key, or a digit past the mount/card count,
returns `None`. Note cards are queried from `content` (the runtime `FieldKit` lives
on `Content` until `Ship::field_kit` lands), while mounts come from the `ship`.

**Cross-references:** Called by [`broadside.rs`](broadside.md)'s `window_event`.
Reads `Ship::mounts` and `Content::card_at`. **Worked examples:**
`key_to_intent_digits_resolve_to_mount_weapons` (src/input.rs:651),
`key_to_intent_out_of_range_digits_return_none` (src/input.rs:669, 1 mount → D2/D3
are `None`), `key_to_intent_commit_aliases` (src/input.rs:682, R and Space both
commit).

## `fn intent_to_action_id(intent) -> Option<&str>` (src/input.rs:158)

**Intent:** Convert an `Intent` to the queue's action id. `QueueAction` passes its
id through; the four synthetics map to their `__`-prefixed constants. Returns
`None` for `CommitTurn`/`Restart` (control flow, not queued) **and** for `PlayCard`
(cards need a separate validate-and-decrement step the caller does via
`try_play_card`, then pushes `synthetic_card_action_id` manually). The `__` prefix
guarantees synthetic ids can't collide with real catalog actions. **Worked
examples:** `intent_to_action_id_synthetics_use_double_underscore` (src/input.rs:697),
`intent_to_action_id_returns_none_for_play_card` (src/input.rs:1025).

## `fn synthetic_card_action_id(card_id) -> String` (src/input.rs:180)

Returns `"__card_<card_id>"`. After `try_play_card` returns `Played`, the caller
pushes this onto the queue; the resolver looks up the registered `Action { effects:
[BOARD { note: <id> }] }` and the BOARD arm routes through
`Content::apply_board_effect`. Pinned by `synthetic_card_action_id_format`
(src/input.rs:1032).

---

## Synthetic action ids + builders (src/input.rs:193–304)

The four `SYNTHETIC_*` const ids (`__move_left`/`__move_right`/`__reorient_flip`/
`__vent`) and their `Action` builders. These exist so the synthetic intents flow
through the **normal `fire_player_queue`/`run_action` pipeline** without the
resolver special-casing any of them — the demo content just knows these ids and
returns the canonical `Action` records. Helpers `all_bands` (src/input.rs:200,
five-band coverage so the band gate never rejects a synthetic), `self_targeting`
(src/input.rs:210, SELF pattern, no arc), `zero_cost` (src/input.rs:221, free +
advances turn).

- `synthetic_move_left`/`_right` (src/input.rs:236, 257) — `DISPLACE_SELF { mode:
  THRUST, distance: 1, direction: Some(Aft/Fore) }`. **Lane-relative** (#50): the
  `direction: Some(...)` override makes `resolve_self_move` ignore
  `ship.orientation`, so Left always moves leftward on screen regardless of bow —
  a predictable 2D control scheme. AI/scripted moves pass `direction: None` to keep
  orientation-relative behavior.
- `synthetic_reorient_flip` (src/input.rs:274) — `REORIENT { to: Flip }`.
- `synthetic_rotate_left`/`_right` (src/input.rs:395, 411) — `REORIENT { to: RotateLeft
  }` / `RotateRight` (#75), ids `__rotate_left` / `__rotate_right` (`SYNTHETIC_ROTATE_*`).
  A quarter-turn of the player's `facing`; the resolver re-derives `orientation`. Both
  registered on `DemoContent` like the other synthetics.
- `synthetic_vent` (src/input.rs:290) — `VENT_HEAT { amount: 3, recharge_cooldowns:
  Some(true) }`, matching the catalog's Defensive vent.

**Worked example:** `synthetic_actions_are_free_and_uncooldowned` (src/input.rs:744)
pins zero cost; `synthetic_vent_flows_through_execute_queue` (src/input.rs:794)
proves a synthetic runs end-to-end through the resolver.

---

## `struct DemoContent` (src/input.rs:326)

**Intent:** The demo's concrete `Content` impl. Four fields: an
`actions: HashMap<String, Action>` registry, a `subsystems::Installations`
registry (per-ship installed subsystems), a `cards::CardCatalog`, and a
`cards::FieldKitRegistry` (per-ship card inventories). The subsystem/card
registries live here rather than on `Board` because the resolver queries them
every shot/turn (see the subsystems module rationale).

### Constructors + registration (src/input.rs:333–423)

`empty` (all-empty), `insert` (add/replace an action), and the four registration
helpers: `register_synthetics` (the move/flip/vent set), `register_card_synthetics`
(one `__card_<id>` shell per placeholder card, each a `BOARD { note }` effect),
`register_class_signatures` (the three placeholder class Signatures — defs exist in
the registry, input wiring deferred), plus `install_subsystem` / `grant_card` /
`grant_placeholder_kit` convenience setters. `card_synthetic_action`
(src/input.rs:412) builds the zero-cost SELF `BOARD` shell.

### `impl Default` (src/input.rs:425)

The demo loadout: all four input synthetics + three card synthetics + three class
Signatures + the placeholder card catalog + the two mount weapons (`pulse_laser` —
close-range forward BEAM, 4 damage; `torpedo` — forward ORDNANCE that spawns a
projectile). Matches `broadside.rs::render_example_board`'s player. **Worked
examples:** `demo_content_serves_every_synthetic` (src/input.rs:728),
`demo_content_serves_demo_mount_weapons` (src/input.rs:737),
`demo_content_registers_every_placeholder_signature` (src/input.rs:778).

### `impl Content for DemoContent` (src/input.rs:480)

The trait that makes `DemoContent` usable by the resolver (the trait is defined at
[`resolve.rs:47`](resolve.md)):
- `action(id)` (481) — registry lookup.
- `damage_modifier(attacker, band, board)` (485) — routes through the
  **attacker's** installed subsystems (audit #67: bonuses fire from the attacker's
  fittings, not the target's). `marksman_subsystem_adds_one_through_apply_damage`
  (src/input.rs:829) pins this.
- `on_turn_end(board)` (497) — drives subsystem end-of-turn hooks
  (`heatsink_subsystem_doubles_dissipation_per_turn`, src/input.rs:898).
- `apply_board_effect(note, source_cell, board)` (501) — dispatches a card's BOARD
  effect via `cards::apply_card_effect` (`mass_lock_card_play_through_execute_queue`,
  src/input.rs:929).
- `card_at` (505) / `try_play_card` (514) — the field-kit read + play path
  (`second_play_of_one_charge_card_rejected`, src/input.rs:989).
- `spawn_projectile(kind, owner)` (518) — a small hardcoded table: `torpedo` (slow,
  payload 4), `missile` (fast, payload 2), unknown → 0-damage dummy (so a typo
  doesn't crash the demo). Heading derives from the owner's orientation.

**Cross-references:** Implements [`resolve::Content`](resolve.md); drives
[`subsystems`](../LINE_BY_LINE.md#srcsubsystemsrs) and
[`cards`](../LINE_BY_LINE.md#srccardsrs). Consumed by
[`broadside.rs`](broadside.md) and `resolve_round`.

---

## `fn tutorial_lines() -> &'static [&'static str]` (src/input.rs:574)

The terse one-line-per-binding strings the renderer stacks as a top-of-screen
overlay. Kept in lockstep with `Key` + `key_to_intent` (adding a binding touches
all three in one commit). `tutorial_lines_cover_every_binding` (src/input.rs:761)
pins the sync.
