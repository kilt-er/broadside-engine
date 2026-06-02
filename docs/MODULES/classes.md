# `src/classes.rs` — canonical class roster + Signature actions

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/classes.rs`](../LINE_BY_LINE.md#srcclassesrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

[`ClassDef`](types.md) (in types.rs) is the wire shape — a player class's id, name,
affinity, two loadout sets, signature action id, optional passive, flavour desc.
This module is the **content half**: the actual roster the demo seeds, plus the
Signature [`Action`](types.md)s each class's `signature` field references. A
signature only fires if it's a real `Action` present in `Content::action(id)` — so
this module builds them, and `DemoContent::register_class_signatures`
([`input.rs`](input.md)) inserts every signature builder into the action registry.

It is a **content/runtime registration step**, parallel to
[`catalog_canonical.rs`](catalog_canonical.md) (which produces the same classes from
the canonical JSON). The two are deliberately kept in lockstep so the demo's
hand-built `DemoContent` (which doesn't load the full catalog) serves *identical*
behaviour to the catalog path.

### The roster (#50 — canonical 5 + Aegis)

Five canonical classes transcribed verbatim from the analysis doc
(`broadside-analysis.html` CLASSES, lines 1143-1165), plus the broadside-native
Aegis:

| id | name | affinity | signature | unlock |
|---|---|---|---|---|
| `wanderer` | Frigate "Drifter" | Flexible | Slip | default |
| `ronin` | Destroyer "Ronin" | BowOn | Ram | Defeat The Twins |
| `shadow` | Phantom "Shade" | Broadside | Phase (+passive) | Defeat The Warlord |
| `jujitsuka` | Monitor "Anvil" | BowOn | Throw | Defeat the Flagship P2 |
| `chainmaster` | Carrier "Tessen" | Broadside | Swap Toss | Defeat the Flagship P3 |
| `aegis` | Aegis | Broadside | Broadside Sweep | default — **provisional** |

This retires the Phase-2 placeholders (Vanguard/Wraith/Bulwark, task #62).

> **PROVISIONAL — Aegis (the "Sweep" 6th class).** Aegis is **not** in the canonical
> doc roster — it's an additive 6th class built around bruce's hand-painted ship art
> (the bin sets `player.klass = "aegis"`). It is documented here **as currently
> built**, but **bruce has not yet ruled whether it's a true 6th class (current
> state) or a reskin of a canonical broadside class** (chainmaster / shadow). If he
> rules reskin, the `aegis()` ClassDef + `synthetic_broadside_sweep()` retire cheaply
> and this section needs a touch-up. Treat the Aegis subsection as pending his call.

---

## Ids + roster builders (src/classes.rs:71–122)

The `CLASS_*` and `SIG_*` consts (src/classes.rs:71-94) name the six classes and six
signatures; `SIGNATURE_IDS` lists every synthesized signature in roster order.
`canonical_classes()` (src/classes.rs:113) returns the six `ClassDef`s;
`placeholder_classes()` (src/classes.rs:107) is a **stability alias** that returns the
same — the name is kept because the bin and the input.rs signature-coverage test
consume it by that name (it is no longer placeholders).

Each class builder (`wanderer` src/classes.rs:127, `ronin` :146, `shadow` :166,
`jujitsuka` :190, `chainmaster` :210) transcribes its doc CLASSES row: affinity,
set1/set2 action-id lists, signature id, optional passive (only `shadow` has one —
"advance as far as possible"), and flavour desc. **Worked examples:**
`canonical_roster_has_six_distinct_classes` (src/classes.rs:417),
`affinities_cover_all_three_variants` (src/classes.rs:469),
`every_signature_id_is_synthesized` (src/classes.rs:440, pins that no class points at a
signature this module doesn't build — else the resolver silently no-ops the press).

### `fn aegis() -> ClassDef` (src/classes.rs:243) — provisional

**Intent:** The first broadside-native *player* class — content's doc-grounded
"Option A: Sweep" identity (the lead approved proceeding since the doc is silent on
Aegis). The design hook: where a plain broadside battery just fires both lane-ends,
Aegis's Sweep fires both flanks **and** sweeps the hull around (REORIENT flip) —
turning the defensive stance-flip the player is otherwise forced into by enemy
lane-end pressure into an **offensive** identity. It's the mechanical inverse of the
enemy AI's "maximise distinct threatened lane-ends" directive. **Provisional** per the
note above. **Worked example:** `aegis_is_a_broadside_class_with_the_sweep_signature`
(src/classes.rs:513).

---

## Signature actions

### The five canonical self-moves (src/classes.rs:275–368)

The doc's five signatures are **self-relative maneuvers** (the #84/#97 fix — the
canonical export tags them `pattern: SELF` and they resolve as `DISPLACE_SELF`, NOT
`DISPLACE_TARGET`). `self_move_signature` (src/classes.rs:275) is the shared shell:
`WeaponArchetype::Movement`, `SELF` pattern, no arc, point-blank band, **free-fire**
(`advances_turn: false`). The five builders supply the mode:
- `synthetic_slip` (wanderer) — `TRACTOR_SWAP` (trade places with the ship ahead).
- `synthetic_ram` (ronin) — `BURN` forward (collision billed by `resolve_self_move`).
- `synthetic_phase` (shadow) — `SLIP` (pass through the ship ahead).
- `synthetic_throw` (jujitsuka) — `BURN` with `direction: Aft` (hurl the ship behind).
- `synthetic_swap_toss` (chainmaster) — `TRACTOR_SWAP` (the faithful single bow-side
  swap subset; the doc's two-sided fore-AND-aft swap has no single-effect form today).

These **mirror catalog_canonical's inflation exactly** (see
[`catalog_canonical.md`](catalog_canonical.md)'s `inflate_effect` DISPLACE_SELF arm),
so the hand-built demo registry and the catalog load path produce identical behaviour.
**Worked examples:** `slip_and_swap_toss_are_tractor_swaps` (src/classes.rs:480),
`ram_burns_forward_throw_burns_aft` (src/classes.rs:491), `phase_is_slip_movement`
(src/classes.rs:503), `signature_builders_match_their_ids` (src/classes.rs:454).

### `fn synthetic_broadside_sweep() -> Action` (src/classes.rs:385) — Aegis, provisional

**Intent:** Aegis's both-flanks-then-pivot signature — two effects in declaration
order: (1) `DAMAGE 3` through the `BROADSIDE` pattern (fires the first occupant in
*both* lane directions when the broadside arc bears, so up to two ships eat 3); (2)
`REORIENT { to: Flip }` (after the volley the hull flips stance-preserving, re-presenting
the broadside the other way for next round — "the answer to being flanked is to flank
back and keep flanking"). Heat 4 / cooldown 5, a heavy commit; requires `BroadsideArc`.
DAMAGE resolves **before** the reorient (lands on who's in the line at fire time, then
the flip happens). Unlike the five canonical signatures it **advances the turn**
(it's a weapon, not a free maneuver). **Worked example:**
`broadside_sweep_fires_both_flanks_then_flips` (src/classes.rs:524, pins exactly
DAMAGE-then-flip + the broadside arc + the heavy cost).

**Cross-references:** `ClassDef`s land in `Catalog::classes` (the catalog/canonical
path produces the same five via [`catalog_canonical`](catalog_canonical.md)); the
signature `Action`s are registered by `DemoContent::register_class_signatures`
([`input.md`](input.md)) and dispatched by the resolver's effect pipeline
([`resolve.md`](resolve.md)) — `DISPLACE_SELF` modes run through `resolve_self_move`,
the Sweep's `DAMAGE`+`REORIENT` through the normal effect dispatch.
