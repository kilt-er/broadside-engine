# Review: src/catalog_canonical.rs (loose canonical -> strict transformer)

Reviewer audit (task #9). This module has no direct TS counterpart — the TS
engine consumes the strict shape directly. The reference is the canonical
"Copy JSON" export (assets/broadside.catalog.json) + the analysis-doc prose.
Status: **APPROVE.** Signature bug already fixed (#97, verified). One design
limitation surfaced (not a correctness defect). Two inference notes.

## Verified

- **Action transform** (catalog_canonical.rs:134-212) — heat->cost.heat, cd->cost.cooldownMax, !freeplay->cost.advancesTurn, band->optimalBand, pattern/arc direct, hits_all default false, facingRelative forced true. All correct renames. Missing required fields punt the action via filter_map (lose one action, not the whole load) — defensible.
- **rewrite_self_relative_signature** (catalog_canonical.rs:397) + DISPLACE_SELF inflate arm — the #97 fix. slip/swap_toss->DISPLACE_SELF{TRACTOR_SWAP}, ram/throw->DISPLACE_SELF{BURN} (throw aft), phase->SLIP. DAMAGE dropped on ram/throw (collision billing owns it). Verified correct + behavior-tested through the resolver.
- **transform_subsystem** — unlock->unlockSalvage rename, level default 1. Matches the strict SubsystemDef. Tested.
- **transform_class** + signature_id_from_prose + normalize_action_ref — affinity bow-on->bowOn, set1/set2 display-name->id via case-folded lookup, signature prose->snake_case id. Unmapped refs pass through (resolver skips, doesn't fail load). Tested incl. em-dash/ASCII-dash/no-dash cases.
- **inflate_effect** — all 9 effect kinds inflate to strict tagged records; already-object effects pass through (hybrid catalogs work); unknown kinds preserve {kind} so serde surfaces the drift. SPAWN_ORDNANCE/VENT_HEAT/DEPLOY/BOARD/REORIENT defaults are sensible and commented.

## DESIGN LIMITATION (surface to bruce — NOT a correctness bug)

`targeting.band` is seeded with ONLY the optimal band (catalog_canonical.rs:194:
`band: [optimal_band]`). Confirmed against the raw export: every canonical action
carries a single SCALAR `band` string (pulse_laser "close", beam_cannon "mid",
...), zero actions carry a multi-band list. So the transformer isn't dropping
data — the allowed-bands list does not exist in the canonical export format.

Consequence: a canonically-loaded action can fire ONLY at its optimal range.
`resolve_targeting`'s in_allowed_band gate rejects every non-optimal band, so:
1. The band-falloff system (geometry.rs band_falloff table 0.66/0.5/0.33/0.2) is
   effectively DEAD for canonical actions — every shot that fires is delta-0
   (full damage), because you can't legally fire at any off-optimal band.
2. Weapons are far more range-restricted than the TS design intends (TS
   Targeting.band is a real RangeBand[] allowed-list).

This only bites canonical-loaded content; hand-authored strict catalogs (the
demo's inline actions) specify wider band arrays and exercise falloff normally.
Fix is a FORMAT decision, not a code bug: either the export grows a real
allowed-bands list, or the transformer infers a band WINDOW around the optimal
(e.g. optimal +/- 1) so falloff has room to operate. bruce/design call.

## Inference notes (low priority)

- DAMAGE amount inference (beam/broadside = heat+2, ordnance = 0, displacement/control = 2, else heat.max(1)) is a heuristic with no canonical numbers to check against — the export carries no per-effect amounts. Flagged as "tune in playtest" in-code. Acceptable; not verifiable against a reference because no reference exists.
- APPLY_STATUS archetype defaults (ordnance/displacement/control -> systemsOffline, else hullBreach) are likewise heuristic. Same status: acceptable, unverifiable, documented.

No code changes required. The signature path is fixed and the rest is faithful;
the band-seeding limitation is the one thing worth a design ruling.
