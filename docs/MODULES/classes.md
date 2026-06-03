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

### The roster (#50 — canonical 5 + Aegis; #66 ship-archetype reflavor)

Five canonical classes whose **mechanics** are transcribed verbatim from the analysis
doc (`broadside-analysis.html` CLASSES, lines 1143-1165), plus the broadside-native
Aegis:

| id | name | affinity | signature | unlock |
|---|---|---|---|---|
| `corvette` | Corvette "Slipstream" | Flexible | Slip | default |
| `prowship` | Ram "Ironprow" | BowOn | Ram | Defeat The Twins |
| `runner` | Blockade Runner "Wraith" | Broadside | Phase (+passive) | Defeat The Warlord |
| `tug` | Salvage Tug "Capstan" | BowOn | Throw | Defeat the Flagship P2 |
| `carrier` | Carrier "Broadside Bay" | Broadside | Swap Toss | Defeat the Flagship P3 |
| `aegis` | Battleship "Aegis" | Broadside | Broadside Sweep | default |

This retires the Phase-2 placeholders (Vanguard/Wraith/Bulwark, task #62).

> **#66 reflavor (bruce-approved 2026-06-02).** The roster was reflavored off the
> Shogun-Showdown hero corollaries (old ids `wanderer`/`ronin`/`shadow`/`jujitsuka`/
> `chainmaster`, with quoted SS nicknames Drifter/Ronin/Shade/Anvil/Tessen) onto
> naval-combat ship archetypes whose identity reads from each class's mechanical
> signature. **Mechanics are unchanged** — affinity, set1/set2 loadouts, signature,
> passive, and heat/cooldown are all identical; this was a pure identity/naming
> layer. The **signature-ability ids** (slip/ram/phase/throw/swap_toss/broadside_sweep)
> were deliberately left as-is — they name maneuvers, not heroes, and the resolver
> dispatches by them. The reflavor also **folds Aegis into the canonical roster as
> the broadside-native 6th ship class** ("Battleship Aegis"), resolving the earlier
> new-vs-reskin question: it's neither — a peer broadside ship.

---

## Ids + roster builders (src/classes.rs:87–139)

The `CLASS_*` and `SIG_*` consts (src/classes.rs:89-102) name the six classes and six
signatures; `SIGNATURE_IDS` lists every synthesized signature in roster order. The
ship-archetype `CLASS_*` ids (`corvette`/`prowship`/`runner`/`tug`/`carrier`, plus the
unchanged `aegis`) replace the retired SS-hero ids — see the #66 reflavor note above.
`canonical_classes()` (src/classes.rs:130) returns the six `ClassDef`s;
`placeholder_classes()` (src/classes.rs:125) is a **stability alias** that returns the
same — the name is kept because the bin and the input.rs signature-coverage test
consume it by that name (it is no longer placeholders).

Each class builder (`corvette` src/classes.rs:145, `prowship` :165, `runner` :186,
`tug` :210, `carrier` :230) transcribes its doc CLASSES row: affinity, set1/set2
action-id lists, signature id, optional passive (only `runner` has one — "advance as
far as possible"), and flavour desc. **Worked examples:**
`canonical_roster_has_six_distinct_classes` (src/classes.rs:435),
`affinities_cover_all_three_variants` (src/classes.rs:487),
`every_signature_id_is_synthesized` (src/classes.rs:458, pins that no class points at a
signature this module doesn't build — else the resolver silently no-ops the press).

### `fn aegis() -> ClassDef` (src/classes.rs:260)

**Intent:** The broadside-native *player* class — content's doc-grounded "Option A:
Sweep" identity, folded into the canonical roster as the 6th ship class by #66
(bruce-approved). The design hook: where a plain broadside battery just fires both
lane-ends, Aegis's Sweep fires both flanks **and** comes about (REORIENT flip) to
re-present the guns — a battleship's rolling broadside. It turns the defensive
stance-flip the player is otherwise forced into by enemy lane-end pressure into an
**offensive** identity: the mechanical inverse of the enemy AI's "maximise distinct
threatened lane-ends" directive. **Worked example:**
`aegis_is_a_broadside_class_with_the_sweep_signature` (src/classes.rs:531).

---

## Signature actions

### The five canonical self-moves (src/classes.rs:292–399)

The doc's five signatures are **self-relative maneuvers** (the #84/#97 fix — the
canonical export tags them `pattern: SELF` and they resolve as `DISPLACE_SELF`, NOT
`DISPLACE_TARGET`). `self_move_signature` (src/classes.rs:292) is the shared shell:
`WeaponArchetype::Movement`, `SELF` pattern, no arc, point-blank band, **free-fire**
(`advances_turn: false`). The five builders supply the mode:
- `synthetic_slip` (corvette) — `TRACTOR_SWAP` (trade places with the ship ahead).
- `synthetic_ram` (prowship) — `BURN` forward (collision billed by `resolve_self_move`).
- `synthetic_phase` (runner) — `SLIP` (pass through the ship ahead).
- `synthetic_throw` (tug) — `BURN` with `direction: Aft` (hurl the ship behind).
- `synthetic_swap_toss` (carrier) — `TRACTOR_SWAP` (the faithful single bow-side
  swap subset; the doc's two-sided fore-AND-aft swap has no single-effect form today).

These **mirror catalog_canonical's inflation exactly** (see
[`catalog_canonical.md`](catalog_canonical.md)'s `inflate_effect` DISPLACE_SELF arm),
so the hand-built demo registry and the catalog load path produce identical behaviour.
**Worked examples:** `slip_and_swap_toss_are_tractor_swaps` (src/classes.rs:498),
`ram_burns_forward_throw_burns_aft` (src/classes.rs:509), `phase_is_slip_movement`
(src/classes.rs:521), `signature_builders_match_their_ids` (src/classes.rs:472).

### `fn synthetic_broadside_sweep() -> Action` (src/classes.rs:402) — Aegis

**Intent:** Aegis's both-flanks-then-pivot signature — two effects in declaration
order: (1) `DAMAGE 3` through the `BROADSIDE` pattern (fires the first occupant in
*both* lane directions when the broadside arc bears, so up to two ships eat 3); (2)
`REORIENT { to: Flip }` (after the volley the hull flips stance-preserving, re-presenting
the broadside the other way for next round — "the answer to being flanked is to flank
back and keep flanking"). Heat 4 / cooldown 5, a heavy commit; requires `BroadsideArc`.
DAMAGE resolves **before** the reorient (lands on who's in the line at fire time, then
the flip happens). Unlike the five canonical signatures it **advances the turn**
(it's a weapon, not a free maneuver). **Worked example:**
`broadside_sweep_fires_both_flanks_then_flips` (src/classes.rs:542, pins exactly
DAMAGE-then-flip + the broadside arc + the heavy cost).

**Cross-references:** `ClassDef`s land in `Catalog::classes` (the catalog/canonical
path produces the same five via [`catalog_canonical`](catalog_canonical.md)); the
signature `Action`s are registered by `DemoContent::register_class_signatures`
([`input.md`](input.md)) and dispatched by the resolver's effect pipeline
([`resolve.md`](resolve.md)) — `DISPLACE_SELF` modes run through `resolve_self_move`,
the Sweep's `DAMAGE`+`REORIENT` through the normal effect dispatch.
