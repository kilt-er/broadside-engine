//! Phase 2 class scaffold (task #62 step 2): three placeholder
//! [`ClassDef`]s + their Signature [`Action`]s.
//!
//! Architect's task #62 step 1 landed [`crate::types::ClassDef`] +
//! [`crate::types::ClassAffinity`] on origin (`f57c060`). This module
//! provides the **content** half: three placeholder classes the demo can
//! seed, and the Signature actions each one's `signature` field
//! references.
//!
//! ## Catalog vs content split
//!
//! - [`ClassDef`] (in `types.rs`) is the wire shape — id, name, affinity,
//!   set1/set2 action-id lists, signature action id, optional passive
//!   prose, flavour `desc`. Lives in `Catalog::classes`.
//! - This module is the **runtime registration step**: each Signature is
//!   a real [`Action`] that needs to be present in `Content::action(id)`
//!   for the resolver to dispatch it. `DemoContent::default` calls
//!   [`DemoContent::register_class_signatures`] (added in
//!   `src/input.rs`) which inserts all three Signatures into the
//!   action registry.
//!
//! ## Signature semantics
//!
//! Per the analysis HTML's "free-fire" framing, a Signature is dispatched
//! through `Ship::klass` rather than through one of the ship's mounts —
//! it's the class-specific hero ability. The action def is still a normal
//! [`Action`] (cost, targeting, effects); the only thing that makes it
//! a "Signature" is the ClassDef pointing at its id. There's **no input
//! wiring yet**: pressing a key to fire the Signature is deferred per
//! task #62's "defer the input wiring; just have the Action defs in
//! place." When that lands, the dispatch becomes:
//!
//! ```text
//! key press -> Intent::FireSignature
//!           -> look up player.klass -> ClassDef
//!           -> queue ClassDef.signature (the action id)
//!           -> regular execute_queue / apply_instant_action path
//! ```
//!
//! ## Why these three classes
//!
//! Task #62 names them: Frigate "Vanguard", Scout "Wraith", Gunboat
//! "Bulwark". These are **task-spec** names — they don't match the
//! analysis HTML's class roster (`wanderer`, `ronin`, `shadow`, etc.).
//! The canonical roster will replace these when the real catalog export
//! lands; until then these three exercise the three [`ClassAffinity`]
//! variants:
//!
//! - **Vanguard** (`Flexible`) — Overcharge: heat-intensive alpha strike
//! - **Wraith**  (`BowOn`)    — Phase Drift: JUMP + targetLock combo
//! - **Bulwark** (`Broadside`) — Broadside Volley: bidirectional fire
//!
//! Each Signature action id starts with the class's flavour name
//! (`overcharge`, `phase_drift`, `broadside_volley`) for log readability;
//! they're not prefixed with `__` because they're real catalog actions,
//! not synthetic player-input shells.

use crate::types::{
    Action, ActionCost, Arc as TArc, ClassAffinity, ClassDef, Effect, MovementMode, RangeBand,
    ReorientTo, StatusKind, Targeting, TargetingPattern, WeaponArchetype,
};

/* =========================================================================
 * Canonical class + signature ids.
 *
 * Adding a new class goes here, in [`placeholder_classes`], in the
 * matching `synthetic_*_signature` builder, and (the runtime side) in
 * `DemoContent::register_class_signatures` in input.rs.
 * ====================================================================== */

pub const CLASS_VANGUARD: &str = "vanguard";
pub const CLASS_WRAITH: &str = "wraith";
pub const CLASS_BULWARK: &str = "bulwark";

/// Aegis — the first broadside-native PLAYER class (bruce's hand-painted
/// `aegis_*.png` art; the bin already sets `player.klass = "aegis"`). NOT
/// in the canonical analysis-doc roster (which is the five Shogun-derived
/// classes wanderer/ronin/shadow/jujitsuka/chainmaster) — Aegis is an
/// additive 6th class built on the art. Identity is content's
/// doc-grounded "Option A: Sweep" (the lead approved proceeding on the rec
/// since the doc is silent on Aegis): an aggressive both-flanks broadside
/// bruiser whose signature fires both lane-ends then sweeps the hull
/// around. If bruce later rules Aegis is a RESKIN of a canonical broadside
/// class (chainmaster/shadow) rather than a new one, this entry is cheap to
/// retire — it's one ClassDef + one synthetic action.
pub const CLASS_AEGIS: &str = "aegis";

pub const SIG_OVERCHARGE: &str = "overcharge";
pub const SIG_PHASE_DRIFT: &str = "phase_drift";
pub const SIG_BROADSIDE_VOLLEY: &str = "broadside_volley";
pub const SIG_BROADSIDE_SWEEP: &str = "broadside_sweep";

/// All three placeholder Signature action ids, in class order
/// (Vanguard → Wraith → Bulwark).
pub const PLACEHOLDER_SIGNATURE_IDS: &[&str] = &[
    SIG_OVERCHARGE,
    SIG_PHASE_DRIFT,
    SIG_BROADSIDE_VOLLEY,
];

/* =========================================================================
 * The three ClassDefs.
 * ====================================================================== */

/// Build the three placeholder [`ClassDef`]s the demo seeds into the
/// catalog. Replaces / complements `Catalog::classes` content when no
/// real export is loaded.
pub fn placeholder_classes() -> Vec<ClassDef> {
    vec![vanguard(), wraith(), bulwark()]
}

/// Frigate "Vanguard" — Flexible affinity, Overcharge signature.
///
/// The starter class. No strong stance bias (Flexible) so the player
/// can experiment with both bow-on and broadside as they learn. The
/// Signature is an alpha-strike heat-bomb: high single-shot damage
/// for a heat cost that locks the next turn out (forcing a Vent).
pub fn vanguard() -> ClassDef {
    ClassDef {
        id: CLASS_VANGUARD.into(),
        name: "Frigate \"Vanguard\"".into(),
        affinity: ClassAffinity::Flexible,
        unlock: Some("Unlocked by default".into()),
        // Demo mount weapons. Real loadouts replace these when the
        // canonical catalog lands.
        set1: vec!["pulse_laser".into(), "torpedo".into()],
        set2: vec!["pulse_laser".into(), "broadside_battery".into()],
        signature: SIG_OVERCHARGE.into(),
        passive: None,
        desc: "Starter frigate. Balanced loadout, no strong stance bias. \
               Overcharge dumps a single high-damage shot at the cost of \
               near-certain lockout — pairs with a Vent on the next turn."
            .into(),
    }
}

/// Scout "Wraith" — BowOn affinity, Phase Drift signature.
///
/// Bow-on specialist with a positional / control signature: JUMP
/// forward (bow-direction blink) and target-lock whatever was in the
/// forward arc before the jump. The lock fires on the FIRST follow-up
/// hit per the canonical targetLock status, so the Scout pairs the
/// signature with a queued spinal/beam to cash the doubled hit.
pub fn wraith() -> ClassDef {
    ClassDef {
        id: CLASS_WRAITH.into(),
        name: "Scout \"Wraith\"".into(),
        affinity: ClassAffinity::BowOn,
        unlock: None, // available from start
        set1: vec!["pulse_laser".into(), "blink_drive".into()],
        set2: vec!["particle_lance".into(), "afterburner".into()],
        signature: SIG_PHASE_DRIFT.into(),
        passive: Some(
            "Lock applied by Phase Drift carries through the jump — the \
             Scout's next-turn shot finds its mark even if the target tries \
             to reorient out of the spinal line."
                .into(),
        ),
        desc: "Light, bow-on skirmisher. Phase Drift blinks the ship \
               forward and target-locks the bow-arc threat; chain a Pulse \
               Laser into the locked target for a doubled-damage spike."
            .into(),
    }
}

/// Gunboat "Bulwark" — Broadside affinity, Broadside Volley signature.
///
/// Broadside specialist. The Signature fires both lane directions
/// simultaneously when the hull is turned across the lane, so it shines
/// when the player is pinched between threats — the directional shield
/// favours the broadside stance against flanking, and the Signature
/// punishes both flanks at once.
pub fn bulwark() -> ClassDef {
    ClassDef {
        id: CLASS_BULWARK.into(),
        name: "Gunboat \"Bulwark\"".into(),
        affinity: ClassAffinity::Broadside,
        unlock: None,
        set1: vec!["broadside_battery".into(), "flak_battery".into()],
        set2: vec!["railgun_broadside".into(), "grav_snare".into()],
        signature: SIG_BROADSIDE_VOLLEY.into(),
        passive: None,
        desc: "Heavy gunboat. Broadside Volley fires every broadside mount \
               in both lane directions when the hull bears — the answer to \
               being flanked is to flank back. Strong with bow-shield armour \
               left for chip damage; weak on heat economy."
            .into(),
    }
}

/// Aegis — Broadside affinity, Broadside Sweep signature. The first
/// broadside-native PLAYER class (bruce's hand-painted art), content's
/// doc-grounded "Option A: Sweep" identity.
///
/// The aggressive both-flanks bruiser. Where the placeholder Bulwark just
/// fires both lane-ends, Aegis's Sweep fires both flanks AND sweeps the
/// hull around (REORIENT flip) — turning the defensive stance-flip the
/// player is otherwise forced into by enemy lane-end pressure into an
/// OFFENSIVE identity. It's the mechanical inverse of the enemy AI's job
/// ("maximise distinct threatened lane-ends"): when pincered, you turn
/// broadside and punish both sides at once, then re-present.
///
/// Loadouts reuse the canonical broadside actions: set1 close two-way
/// pressure (broadside_battery + flak_battery), set2 range + pull-into-line
/// (railgun_broadside + grav_snare). Affinity Broadside so the directional
/// shield favours the committed stance.
pub fn aegis() -> ClassDef {
    ClassDef {
        id: CLASS_AEGIS.into(),
        name: "Aegis".into(),
        affinity: ClassAffinity::Broadside,
        unlock: Some("Unlocked by default".into()),
        set1: vec!["broadside_battery".into(), "flak_battery".into()],
        set2: vec!["railgun_broadside".into(), "grav_snare".into()],
        signature: SIG_BROADSIDE_SWEEP.into(),
        passive: None,
        desc: "Broadside-native bruiser. Broadside Sweep fires both lane-ends \
               at once, then sweeps the hull around to re-present the guns — \
               the answer to being flanked is to flank back and keep flanking. \
               Strong port/starboard armour rewards committing to the turn; \
               the bow points off-lane at nothing while you do."
            .into(),
    }
}

/* =========================================================================
 * The three Signature Actions.
 *
 * Each is a real [`Action`] that the resolver dispatches like any other
 * — heat / cooldown / arc / band gates apply. The "Signature" framing
 * lives in the [`ClassDef::signature`] pointer, not in the Action shape.
 * ====================================================================== */

/// Overcharge — Vanguard's alpha-strike. High-damage forward beam at
/// short-to-mid range, heat cost is at the lockout threshold so it
/// almost always forces a Vent next turn (matching the "alpha strike"
/// framing in the task spec).
///
/// Targeting: BEAM (first-occupant in bow direction), Forward arc
/// (must be bow-on facing the target), allowed bands close/mid/long
/// with optimal close. Effects: a single DAMAGE 6, no band-falloff
/// override — falloff applies normally per the canonical pipeline.
pub fn synthetic_overcharge() -> Action {
    Action {
        id: SIG_OVERCHARGE.into(),
        name: "Overcharge".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 5, cooldown_max: 4, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
            ],
            optimal_band: RangeBand::Close,
            requires_arc: Some(TArc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 6, band_falloff: None }],
        r#mod: None,
        icon: None,
    }
}

/// Phase Drift — Wraith's positional combo. Two effects in declaration
/// order:
///
/// 1. `DISPLACE_SELF { mode: JUMP, distance: 2, direction: None }`. JUMP
///    ignores the path entirely; direction None falls back to ship
///    orientation (so the Wraith blinks "forward" relative to its bow).
/// 2. `APPLY_STATUS { status: TargetLock, duration: 1 }` against the
///    targeting cells (the forward-arc BEAM first-occupant — resolved
///    BEFORE the jump, so the lock lands on whoever was in arc at the
///    start of the turn, even if the jump moves the Wraith out of arc).
///
/// Heat is moderate (2) so the Wraith can follow up with a queued Pulse
/// Laser on the next turn to cash the lock's doubled-damage bonus.
pub fn synthetic_phase_drift() -> Action {
    Action {
        id: SIG_PHASE_DRIFT.into(),
        name: "Phase Drift".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 2, cooldown_max: 3, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
            ],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(TArc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![
            // JUMP first so the lock applies to the *pre-jump* target.
            Effect::DISPLACE_SELF {
                mode: MovementMode::JUMP,
                distance: 2,
                direction: None,
            },
            Effect::APPLY_STATUS {
                status: StatusKind::TargetLock,
                duration: 1,
            },
        ],
        r#mod: None,
        icon: None,
    }
}

/// Broadside Volley — Bulwark's bidirectional fire. Targeting pattern
/// `BROADSIDE` returns the first occupant in BOTH lane directions when
/// the broadside arc bears, so a single DAMAGE 4 effect lands on each.
///
/// Heat cost is high (4) and cooldown long (4) — the Volley is the
/// Bulwark's heavy commit, not a per-turn option. Requires the
/// broadside stance to bear.
pub fn synthetic_broadside_volley() -> Action {
    Action {
        id: SIG_BROADSIDE_VOLLEY.into(),
        name: "Broadside Volley".into(),
        archetype: WeaponArchetype::Broadside,
        cost: ActionCost { heat: 4, cooldown_max: 4, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BROADSIDE,
            band: vec![
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
            ],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(TArc::BroadsideArc),
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 4, band_falloff: None }],
        r#mod: None,
        icon: None,
    }
}

/// Broadside Sweep — Aegis's signature. The both-flanks-then-pivot move.
///
/// Two effects in declaration order:
/// 1. `DAMAGE 3` through the `BROADSIDE` pattern — fires the first occupant
///    in BOTH lane directions when the broadside arc bears, so up to two
///    ships (one per flank) eat 3.
/// 2. `REORIENT { to: Flip }` — after the volley the hull flips
///    stance-preserving (bow↔stern), re-presenting the broadside the other
///    way for next round. This is the "sweep": it both keeps Aegis
///    committed broadside (rather than drifting back to bow-on) AND is the
///    visible telegraph that distinguishes Aegis from a plain
///    `broadside_volley` reskin.
///
/// Heat 4 / cooldown 5 — a heavy commit like the other signatures, not a
/// per-turn option. Requires the BroadsideArc to bear (broadside stance).
/// The DAMAGE resolves BEFORE the reorient, so the hit lands on whoever was
/// in the broadside line at fire time, then the flip happens.
pub fn synthetic_broadside_sweep() -> Action {
    Action {
        id: SIG_BROADSIDE_SWEEP.into(),
        name: "Broadside Sweep".into(),
        archetype: WeaponArchetype::Broadside,
        cost: ActionCost { heat: 4, cooldown_max: 5, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BROADSIDE,
            band: vec![RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(TArc::BroadsideArc),
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![
            // Fire both flanks first...
            Effect::DAMAGE { amount: 3, band_falloff: None },
            // ...then sweep the hull around to re-present the guns.
            Effect::REORIENT { to: ReorientTo::Flip },
        ],
        r#mod: None,
        icon: None,
    }
}

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_classes_yields_three_distinct_class_defs() {
        let classes = placeholder_classes();
        assert_eq!(classes.len(), 3);
        let ids: std::collections::HashSet<&str> =
            classes.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(CLASS_VANGUARD));
        assert!(ids.contains(CLASS_WRAITH));
        assert!(ids.contains(CLASS_BULWARK));
        assert_eq!(ids.len(), 3, "class ids must be unique");
    }

    /// The `signature` field on each ClassDef must point at the matching
    /// canonical Signature id. Reviewer's audit will check the same:
    /// if a `signature` references an unknown action id, the resolver
    /// silently no-ops the Signature press, which the player reads as
    /// the class being broken.
    #[test]
    fn every_class_def_signature_id_matches_canonical() {
        let cs = placeholder_classes();
        let by_id: std::collections::HashMap<&str, &ClassDef> =
            cs.iter().map(|c| (c.id.as_str(), c)).collect();
        assert_eq!(by_id[CLASS_VANGUARD].signature, SIG_OVERCHARGE);
        assert_eq!(by_id[CLASS_WRAITH].signature, SIG_PHASE_DRIFT);
        assert_eq!(by_id[CLASS_BULWARK].signature, SIG_BROADSIDE_VOLLEY);
    }

    /// Each class covers a distinct affinity so the three together
    /// exercise the full [`ClassAffinity`] enum. If a future variant
    /// gets added to ClassAffinity, this list extends.
    #[test]
    fn three_classes_cover_three_affinities() {
        let cs = placeholder_classes();
        let mut affs: Vec<ClassAffinity> = cs.iter().map(|c| c.affinity).collect();
        affs.sort_by_key(|a| match a {
            ClassAffinity::Flexible => 0,
            ClassAffinity::BowOn => 1,
            ClassAffinity::Broadside => 2,
        });
        assert_eq!(
            affs,
            vec![
                ClassAffinity::Flexible,
                ClassAffinity::BowOn,
                ClassAffinity::Broadside,
            ],
        );
    }

    /// Every Signature action id starts WITHOUT the `__` synthetic
    /// prefix — Signatures are real catalog actions, not synthetic
    /// player-input shells, so they collide-safe naturally.
    #[test]
    fn signature_ids_are_not_synthetic_prefixed() {
        for id in PLACEHOLDER_SIGNATURE_IDS {
            assert!(
                !id.starts_with("__"),
                "Signature id `{id}` must not use the synthetic `__` prefix",
            );
        }
    }

    #[test]
    fn overcharge_is_a_high_heat_alpha_strike() {
        let a = synthetic_overcharge();
        assert_eq!(a.id, SIG_OVERCHARGE);
        // High heat — task spec says it forces a Vent next turn. The
        // demo player's `heat_max` is 6; cost 5 puts the player one shot
        // away from lockout from a cold start, which is the "alpha
        // strike" framing.
        assert!(a.cost.heat >= 4, "Overcharge must cost enough heat to be a commit");
        // Forward arc — bow-on only.
        assert_eq!(a.targeting.requires_arc, Some(TArc::Forward));
        // Single DAMAGE effect with a high amount.
        let dmg: i32 = a.effects.iter().filter_map(|e| match e {
            Effect::DAMAGE { amount, .. } => Some(*amount),
            _ => None,
        }).sum();
        assert!(dmg >= 5, "Overcharge must deal at least 5 raw damage");
    }

    #[test]
    fn phase_drift_has_jump_then_target_lock_in_that_order() {
        let a = synthetic_phase_drift();
        // Effects in declaration order: DISPLACE_SELF::JUMP first,
        // APPLY_STATUS::TargetLock second. The order matters: pre-jump
        // targeting selects the locked cells, then the jump moves the
        // ship out — see module docstring for the rationale.
        assert!(matches!(
            a.effects[0],
            Effect::DISPLACE_SELF { mode: MovementMode::JUMP, .. }
        ));
        assert!(matches!(
            a.effects[1],
            Effect::APPLY_STATUS { status: StatusKind::TargetLock, .. }
        ));
        assert_eq!(a.effects.len(), 2, "Phase Drift should have exactly two effects");
    }

    #[test]
    fn broadside_volley_requires_broadside_arc() {
        let a = synthetic_broadside_volley();
        // Requires the BroadsideArc to bear — i.e., the ship must be
        // turned broadside. Forward / Rear arcs do NOT bear; Turret
        // ships could theoretically fire it but no Bulwark mount is
        // Turret-arc'd in the placeholder loadouts.
        assert_eq!(a.targeting.requires_arc, Some(TArc::BroadsideArc));
        // BROADSIDE pattern fires both lane directions when bearing.
        assert_eq!(a.targeting.pattern, TargetingPattern::BROADSIDE);
    }

    /* ---- Aegis (#50, first broadside-native player class) ---------- */

    #[test]
    fn aegis_is_a_broadside_class_with_the_sweep_signature() {
        let a = aegis();
        assert_eq!(a.id, CLASS_AEGIS);
        assert_eq!(a.affinity, ClassAffinity::Broadside);
        assert_eq!(a.signature, SIG_BROADSIDE_SWEEP);
        // Loadouts reference the canonical broadside actions (the same ids
        // the catalog ships); both sets are 2 actions like every class.
        assert_eq!(a.set1.len(), 2);
        assert_eq!(a.set2.len(), 2);
        assert!(a.set1.contains(&"broadside_battery".to_string()));
    }

    #[test]
    fn broadside_sweep_fires_both_flanks_then_flips() {
        let a = synthetic_broadside_sweep();
        assert_eq!(a.id, SIG_BROADSIDE_SWEEP);
        // BROADSIDE pattern + BroadsideArc → fires both lane-ends when the
        // hull bears.
        assert_eq!(a.targeting.pattern, TargetingPattern::BROADSIDE);
        assert_eq!(a.targeting.requires_arc, Some(TArc::BroadsideArc));
        // Effects in order: DAMAGE first (lands on who's in the line at fire
        // time), then the stance-preserving flip — the "sweep" that
        // distinguishes Aegis from a plain broadside_volley.
        assert!(matches!(a.effects[0], Effect::DAMAGE { .. }));
        assert!(matches!(
            a.effects[1],
            Effect::REORIENT { to: ReorientTo::Flip }
        ));
        assert_eq!(a.effects.len(), 2, "Sweep is exactly DAMAGE then flip");
        // A heavy commit, not a per-turn option.
        assert!(a.cost.heat >= 4 && a.cost.cooldown_max >= 4);
    }
}
