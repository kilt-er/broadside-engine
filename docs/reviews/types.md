# Review: src/types.rs vs engine/types.ts

Reviewer audit (task #9). Canonical reference: `_drive_pull/broadside-engine/engine/types.ts`.
Status: **APPROVE — faithful type surface.** No missing fields, no wrong types, no missed variants.

## Tagged unions (the structurally-tricky part)

- **Effect** (types.rs:407-474) — `#[serde(tag = "kind")]` internally-tagged, all 9 TS variants present: DAMAGE / APPLY_STATUS / DISPLACE_TARGET / DISPLACE_SELF / REORIENT / SPAWN_ORDNANCE / VENT_HEAT / DEPLOY / BOARD. Field renames correct (`bandFalloff`, `rechargeCooldowns`). DAMAGE.band_falloff is `Option<bool>` with the documented three-valued semantics (None/Some(true) apply falloff, Some(false) bypasses) matching TS `bandFalloff?: boolean` + the resolver's strict-`=== false` predicate. The resolver's `matches!(e, DAMAGE { band_falloff: Some(false), .. })` is the correct port; tested (#14, #17).
- **Orientation** (types.rs:75-79) — `#[serde(tag = "stance", rename_all = "camelCase")]`, `BowOn { bow: LaneEnd }` | `Broadside`. Exact match to TS `{ stance: "bowOn"; bow }` | `{ stance: "broadside" }`. This is the internally-tagged enum that drove the save.rs JSON-not-postcard decision (postcard can't encode it) — correct call, documented in save.rs.

## DISPLACE_SELF.direction — Rust-port extension (justified)

DISPLACE_SELF carries an extra `direction: Option<LaneEnd>` not in TS (types.rs:437-451). `skip_serializing_if = "Option::is_none"` so it round-trips byte-stable with TS catalogs that omit it; `None` = canonical orientation-derived behavior. Added for lane-relative player controls (#50). Faithful default + non-breaking on the wire. APPROVE as an extension, not drift.

## Enums (variant-complete vs TS string unions)

- HullZone (bow/stern/port/starboard), RangeBand (pointBlank/close/mid/long/extreme), Arc (forward/broadsideArc/turret/rear), Faction (player/enemy) — all camelCase-renamed, exact.
- StatusKind (types.rs:291) — hullBreach/systemsOffline/targetLock/shieldsUp — all 4 from TS types.ts:88-92.
- Trait (types.rs:305) — all 10 (Pursuit, Agile, ReactorBreach, BurnHard, Anchored, EliteAgile, EliteAnchored, TwinLinked, ReactiveShield, Voidtouched) 1:1 with TS, TitleCase on the wire (no rename), matching the TS string union casing.
- DisplaceMode (push/pull/swap lowercase), ReorientTo (bowOn/broadside/flip), DeployHazardKind (mine/drone), MovementMode (THRUST/BURN/SLIP/JUMP/TRACTOR_SWAP screaming-snake) — all match their TS literal sets.

## Structural improvement over TS (sound)

- **ShieldProfile** (types.rs:228) is a named-field struct (bow/stern/port/starboard: ShieldFace) with face()/face_mut() accessors, vs TS `Record<HullZone, ShieldFace>`. Stronger than the TS: a missing zone is a deserialize error (tested #12) where TS would allow a partial record. Net safety win, no behavioral drift.
- HazardKind (mine/drone/debris) vs DeployHazardKind (mine/drone) — the widen at DEPLOY time (drone/mine -> HazardKind) is correct; debris is board-spawned, never deployed.

## Notes

- Field renames on Ship (maxHull, heatMax, lockedOut, shieldProfile) and Action (cooldownMax, advancesTurn, optimalBand, requiresArc, facingRelative, hitsAll) all match the TS camelCase wire names.
- SubsystemDef.unlock_salvage uses `#[serde(default)]` (no skip) so null round-trips (tested #11) — matches TS `unlockSalvage: number | null`.

No findings. types.rs is the structural equivalent of types.ts.
