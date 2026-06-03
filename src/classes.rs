//! Canonical ship-class roster (#50) + their Signature [`Action`]s.
//!
//! Architect's task #62 step 1 landed [`crate::types::ClassDef`] +
//! [`crate::types::ClassAffinity`]. This module provides the **content**
//! half: the player class roster the demo seeds, and the Signature actions
//! each class's `signature` field references.
//!
//! ## The roster (#50 — replaces the Phase-2 placeholders)
//!
//! The canonical roster is six ship classes — the five from the analysis
//! doc (`broadside-analysis.html` CLASSES, lines 1143-1165, transcribed
//! verbatim for set1/set2/signature/affinity/passive) plus the
//! broadside-native Aegis:
//!
//! - **corvette** Corvette "Slipstream" (Flexible)     — Slip
//! - **prowship** Ram "Ironprow" (BowOn)               — Ram
//! - **runner**   Blockade Runner "Wraith" (Broadside) — Phase (+passive)
//! - **tug**      Salvage Tug "Capstan" (BowOn)        — Throw
//! - **carrier**  Carrier "Broadside Bay" (Broadside)  — Swap Toss
//! - **aegis**    Battleship "Aegis" (Broadside)       — Broadside Sweep
//!
//! ## Naming (#66 — ship-archetype reflavor, bruce-approved 2026-06-02)
//!
//! The roster was reflavored OFF the Shogun-Showdown hero corollaries
//! (the old ids wanderer/ronin/shadow/jujitsuka/chainmaster, with quoted
//! SS nicknames Drifter/Ronin/Shade/Anvil/Tessen) ONTO naval-combat ship
//! archetypes whose identity reads from each class's mechanical signature.
//! **Mechanics are unchanged** — affinity, set1/set2 loadouts, signature,
//! passive, heat/cd are all identical; this was a pure identity/naming layer.
//!
//! Aegis (bruce's hand-painted art) folds in as the broadside-native 6th
//! ship class rather than a standalone new-vs-reskin question — the
//! offensive inverse of the enemy AI's "maximise threatened lane-ends"
//! directive (fire both flanks, then come about and present them again).
//!
//! The signature-ability ids (slip/ram/phase/throw/swap_toss/broadside_sweep)
//! were deliberately LEFT as-is: they name maneuvers, not heroes, and the
//! resolver dispatches by them.
//!
//! The earlier Phase-2 placeholders (Vanguard/Wraith/Bulwark, task #62)
//! are retired by this roster.
//!
//! ## Catalog vs content split
//!
//! - [`ClassDef`] (in `types.rs`) is the wire shape — id, name, affinity,
//!   set1/set2 action-id lists, signature action id, optional passive
//!   prose, flavour `desc`. Lives in `Catalog::classes`.
//! - This module is the **runtime registration step**: each Signature is a
//!   real [`Action`] that must be present in `Content::action(id)` for the
//!   resolver to dispatch it. `DemoContent::register_class_signatures`
//!   (in `src/input.rs`) inserts every Signature builder below into the
//!   action registry.
//!
//! ## Canonical signature semantics (the five #97-aligned self-moves)
//!
//! The doc's five signatures are self-relative maneuvers (see the #84/#97
//! fix in `catalog_canonical.rs` — the canonical export tags them
//! `pattern: SELF` and they resolve as DISPLACE_SELF, NOT DISPLACE_TARGET):
//!
//! - **slip** — trade places with the ship directly ahead → DISPLACE_SELF
//!   TRACTOR_SWAP.
//! - **ram** — shove the ship ahead, collision damage → DISPLACE_SELF BURN
//!   forward (collision billed by `resolve_self_move`).
//! - **phase** — pass through the ship ahead → DISPLACE_SELF SLIP.
//! - **throw** — hurl the ship behind you → DISPLACE_SELF BURN aft.
//! - **swap_toss** — swap the cells fore and aft → DISPLACE_SELF
//!   TRACTOR_SWAP (the faithful single-swap subset; the doc's two-sided
//!   fore-AND-aft swap has no single-effect representation today).
//!
//! These mirror the catalog-canonical inflation exactly so the demo
//! (hand-built `DemoContent`, which doesn't load the full catalog) serves
//! the same behaviour the catalog path produces.

use crate::types::{
    Action, ActionCost, Arc as TArc, ClassAffinity, ClassDef, Effect, LaneEnd, MovementMode,
    RangeBand, ReorientTo, Targeting, TargetingPattern, WeaponArchetype,
};

/* =========================================================================
 * Canonical class + signature ids.
 *
 * Adding a class: add the const, the builder, append to
 * [`canonical_classes`], add the `synthetic_*` signature builder, and
 * register it in `DemoContent::register_class_signatures` (input.rs).
 * ====================================================================== */

// #66 ship-archetype ids (the SS-hero corollaries — wanderer/ronin/shadow/
// jujitsuka/chainmaster — were retired here). aegis is unchanged.
pub const CLASS_CORVETTE: &str = "corvette";
pub const CLASS_PROWSHIP: &str = "prowship";
pub const CLASS_RUNNER: &str = "runner";
pub const CLASS_TUG: &str = "tug";
pub const CLASS_CARRIER: &str = "carrier";
/// Aegis — broadside-native 6th class (bruce's art); see [`aegis`].
pub const CLASS_AEGIS: &str = "aegis";

pub const SIG_SLIP: &str = "slip";
pub const SIG_RAM: &str = "ram";
pub const SIG_PHASE: &str = "phase";
pub const SIG_THROW: &str = "throw";
pub const SIG_SWAP_TOSS: &str = "swap_toss";
pub const SIG_BROADSIDE_SWEEP: &str = "broadside_sweep";

/// Every Signature action id this module synthesizes, in roster order.
pub const SIGNATURE_IDS: &[&str] = &[
    SIG_SLIP,
    SIG_RAM,
    SIG_PHASE,
    SIG_THROW,
    SIG_SWAP_TOSS,
    SIG_BROADSIDE_SWEEP,
];

/* =========================================================================
 * The class roster.
 * ====================================================================== */

/// Build the full player-class roster the demo seeds into the catalog: the
/// five canonical classes from the analysis doc plus the broadside-native
/// Aegis. Replaces the retired Phase-2 placeholders.
///
/// (Name kept as `placeholder_classes` for caller stability — the bin and
/// the input.rs signature-coverage test consume it by this name. It is no
/// longer placeholders; it's the canonical roster.)
pub fn placeholder_classes() -> Vec<ClassDef> {
    canonical_classes()
}

/// The canonical roster: corvette, prowship, runner, tug, carrier, aegis.
pub fn canonical_classes() -> Vec<ClassDef> {
    vec![
        corvette(),
        prowship(),
        runner(),
        tug(),
        carrier(),
        aegis(),
    ]
}

/// Corvette "Slipstream" (`corvette`) — Flexible, Slip. The default-unlocked
/// starter: a light, agile picket hull with no strong stance bias — a balanced
/// beam + broadside opener. Doc CLASSES line 1144-1147 (reflavored #66 from
/// Frigate "Drifter" / `wanderer`).
pub fn corvette() -> ClassDef {
    ClassDef {
        id: CLASS_CORVETTE.into(),
        name: "Corvette \"Slipstream\"".into(),
        affinity: ClassAffinity::Flexible,
        unlock: Some("Unlocked by default".into()),
        set1: vec!["broadside_battery".into(), "pulse_laser".into()],
        set2: vec!["railgun_broadside".into(), "grav_snare".into()],
        signature: SIG_SLIP.into(),
        passive: None,
        desc: "A light, agile picket hull with no strong stance bias; a \
               balanced beam + broadside opener. Slip trades places with the \
               ship directly ahead — thread the line without spending the turn."
            .into(),
    }
}

/// Ram "Ironprow" (`prowship`) — BowOn, Ram. A reinforced bow-on hull built to
/// collide; its strong front IS the weapon. Doc line 1148-1151 (reflavored #66
/// from Destroyer "Ronin" / `ronin`).
pub fn prowship() -> ClassDef {
    ClassDef {
        id: CLASS_PROWSHIP.into(),
        name: "Ram \"Ironprow\"".into(),
        affinity: ClassAffinity::BowOn,
        unlock: Some("Defeat The Twins".into()),
        set1: vec!["particle_lance".into(), "blink_drive".into()],
        set2: vec!["railgun_broadside".into(), "tractor_toss".into()],
        signature: SIG_RAM.into(),
        passive: None,
        desc: "A reinforced bow-on hull built to collide — the strong front is \
               the weapon. Ram shoves the ship ahead, dealing collision damage \
               on impact; collision perks shine."
            .into(),
    }
}

/// Blockade Runner "Wraith" (`runner`) — Broadside, Phase, + passive. The only
/// class with a passive layered on the signature; a broadside skirmisher that
/// refuses to be pinned. Doc line 1152-1156 (reflavored #66 from Phantom
/// "Shade" / `shadow`).
pub fn runner() -> ClassDef {
    ClassDef {
        id: CLASS_RUNNER.into(),
        name: "Blockade Runner \"Wraith\"".into(),
        affinity: ClassAffinity::Broadside,
        unlock: Some("Defeat The Warlord".into()),
        set1: vec!["broadside_battery".into(), "tractor_beam".into()],
        set2: vec!["particle_lance".into(), "blink_drive".into()],
        signature: SIG_PHASE.into(),
        passive: Some(
            "When moving, the Blockade Runner advances as far as possible in \
             the chosen direction."
                .into(),
        ),
        desc: "A broadside skirmisher that refuses to be pinned — the only \
               class with a passive layered on its signature. Phase passes \
               through the ship directly ahead."
            .into(),
    }
}

/// Salvage Tug "Capstan" (`tug`) — BowOn, Throw. A reversed-stance brawler that
/// fights over its stern, hauling and heaving mass into kills. Doc line
/// 1157-1160 (reflavored #66 from Monitor "Anvil" / `jujitsuka`).
pub fn tug() -> ClassDef {
    ClassDef {
        id: CLASS_TUG.into(),
        name: "Salvage Tug \"Capstan\"".into(),
        affinity: ClassAffinity::BowOn,
        unlock: Some("Defeat the Flagship on Patrol 2".into()),
        set1: vec!["repulsor".into(), "scatter_laser".into()],
        set2: vec!["beam_cannon".into(), "grav_snare".into()],
        signature: SIG_THROW.into(),
        passive: None,
        desc: "A reversed-stance brawler that fights over its stern, hauling \
               and heaving mass into kills. Throw hurls the ship behind you, \
               dealing collision damage."
            .into(),
    }
}

/// Carrier "Broadside Bay" (`carrier`) — Broadside, Swap Toss. Ordnance-heavy
/// broadside hull; multi-target launches reliably trigger chain subsystems.
/// Doc line 1161-1164 (reflavored #66 — dropped the "Tessen" SS nickname).
pub fn carrier() -> ClassDef {
    ClassDef {
        id: CLASS_CARRIER.into(),
        name: "Carrier \"Broadside Bay\"".into(),
        affinity: ClassAffinity::Broadside,
        unlock: Some("Defeat the Flagship on Patrol 3".into()),
        set1: vec!["heavy_torpedo".into(), "afterburner".into()],
        set2: vec!["flak_battery".into(), "missile_salvo".into()],
        signature: SIG_SWAP_TOSS.into(),
        passive: None,
        desc: "Ordnance-heavy broadside hull; multi-target launches reliably \
               trigger chain subsystems. Swap Toss swaps the cells directly \
               fore and aft to open a new firing lane for the next salvo."
            .into(),
    }
}

/// Battleship "Aegis" (`aegis`) — Broadside, Broadside Sweep. The
/// broadside-native PLAYER class (bruce's hand-painted art; the bin sets
/// `player.klass = "aegis"`). Folded into the canonical roster as the 6th
/// ship class by #66 (bruce-approved), resolving the earlier new-vs-reskin
/// question: it's neither — a peer broadside ship, content's "Option A: Sweep"
/// identity.
///
/// The aggressive both-flanks bruiser: where a plain broadside battery just
/// fires both lane-ends, Aegis's Sweep fires both flanks AND comes about
/// (REORIENT flip) to re-present the guns — a battleship's rolling broadside.
/// Turns the defensive stance-flip the player is otherwise forced into by
/// enemy lane-end pressure into an OFFENSIVE identity: the mechanical inverse
/// of the enemy AI's "maximise distinct threatened lane-ends" directive.
pub fn aegis() -> ClassDef {
    ClassDef {
        id: CLASS_AEGIS.into(),
        name: "Battleship \"Aegis\"".into(),
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
 * Signature Actions.
 *
 * Each is a real [`Action`] the resolver dispatches like any other — heat /
 * cooldown / arc / band gates apply. The five canonical signatures mirror
 * the catalog-canonical inflation (catalog_canonical.rs, #97) so the demo's
 * hand-built registry serves identical behaviour to the catalog path.
 * ====================================================================== */

/// Build a free-fire SELF-pattern displacement action shell (the shared
/// shape of slip / ram / phase / throw / swap_toss): pattern SELF, no arc,
/// point-blank band, free-fire (does not advance the turn). The caller
/// supplies the id, name, cost, and the DISPLACE_SELF effect.
fn self_move_signature(
    id: &str,
    name: &str,
    heat: i32,
    cooldown_max: i32,
    effect: Effect,
) -> Action {
    Action {
        id: id.into(),
        name: name.into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat, cooldown_max, advances_turn: false },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![effect],
        r#mod: None,
        icon: None,
    }
}

/// Slip (corvette) — trade places with the ship directly ahead.
/// DISPLACE_SELF TRACTOR_SWAP. Doc heat 1 / cd 5, free-fire.
pub fn synthetic_slip() -> Action {
    self_move_signature(
        SIG_SLIP,
        "Slip",
        1,
        5,
        Effect::DISPLACE_SELF { mode: MovementMode::TRACTOR_SWAP, distance: 1, direction: None },
    )
}

/// Ram (prowship) — shove the ship ahead, collision damage on impact.
/// DISPLACE_SELF BURN forward; `resolve_self_move` bills the collision when
/// the burn is blocked by the ship ahead. Doc heat 2 / cd 6.
pub fn synthetic_ram() -> Action {
    self_move_signature(
        SIG_RAM,
        "Ram",
        2,
        6,
        Effect::DISPLACE_SELF { mode: MovementMode::BURN, distance: 2, direction: None },
    )
}

/// Phase (runner) — pass through the ship directly ahead. DISPLACE_SELF
/// SLIP (skip occupants, land in the first free cell beyond). Doc heat 1 /
/// cd 5.
pub fn synthetic_phase() -> Action {
    self_move_signature(
        SIG_PHASE,
        "Phase",
        1,
        5,
        Effect::DISPLACE_SELF { mode: MovementMode::SLIP, distance: 2, direction: None },
    )
}

/// Throw (tug) — hurl the ship behind you, collision damage.
/// DISPLACE_SELF BURN toward the stern (`direction: Aft` overrides the
/// bow-relative step). Doc heat 2 / cd 6.
pub fn synthetic_throw() -> Action {
    self_move_signature(
        SIG_THROW,
        "Throw",
        2,
        6,
        Effect::DISPLACE_SELF {
            mode: MovementMode::BURN,
            distance: 2,
            direction: Some(LaneEnd::Aft),
        },
    )
}

/// Swap Toss (carrier) — swap the cells directly fore and aft.
/// DISPLACE_SELF TRACTOR_SWAP (the faithful single bow-side swap subset; the
/// two-sided fore-AND-aft swap has no single-effect representation today).
/// Doc heat 2 / cd 7.
pub fn synthetic_swap_toss() -> Action {
    self_move_signature(
        SIG_SWAP_TOSS,
        "Swap Toss",
        2,
        7,
        Effect::DISPLACE_SELF { mode: MovementMode::TRACTOR_SWAP, distance: 1, direction: None },
    )
}

/// Broadside Sweep — Aegis's signature. The both-flanks-then-pivot move.
///
/// Two effects in declaration order:
/// 1. `DAMAGE 3` through the `BROADSIDE` pattern — fires the first occupant
///    in BOTH lane directions when the broadside arc bears, so up to two
///    ships (one per flank) eat 3.
/// 2. `REORIENT { to: Flip }` — after the volley the hull flips
///    stance-preserving (bow↔stern), re-presenting the broadside the other
///    way for next round. The "sweep": keeps Aegis committed broadside AND
///    is the visible telegraph distinguishing it from a plain both-ends
///    battery.
///
/// Heat 4 / cooldown 5 — a heavy commit. Requires the BroadsideArc to bear.
/// DAMAGE resolves BEFORE the reorient (lands on who's in the line at fire
/// time, then the flip happens).
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
            Effect::DAMAGE { amount: 3, band_falloff: None },
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
    use std::collections::{HashMap, HashSet};

    #[test]
    fn canonical_roster_has_six_distinct_classes() {
        let cs = canonical_classes();
        assert_eq!(cs.len(), 6);
        let ids: HashSet<&str> = cs.iter().map(|c| c.id.as_str()).collect();
        for id in [
            CLASS_CORVETTE, CLASS_PROWSHIP, CLASS_RUNNER, CLASS_TUG,
            CLASS_CARRIER, CLASS_AEGIS,
        ] {
            assert!(ids.contains(id), "roster missing `{id}`");
        }
        assert_eq!(ids.len(), 6, "class ids must be unique");
    }

    #[test]
    fn placeholder_classes_aliases_the_canonical_roster() {
        // The bin + input.rs coverage test consume `placeholder_classes`;
        // it now returns the canonical roster.
        assert_eq!(placeholder_classes().len(), canonical_classes().len());
    }

    /// Every ClassDef's `signature` must point at a Signature id this module
    /// synthesizes — otherwise the resolver silently no-ops the press.
    #[test]
    fn every_signature_id_is_synthesized() {
        let synthesized: HashSet<&str> = SIGNATURE_IDS.iter().copied().collect();
        for c in canonical_classes() {
            assert!(
                synthesized.contains(c.signature.as_str()),
                "class `{}` signature `{}` is not in SIGNATURE_IDS",
                c.id, c.signature,
            );
        }
    }

    /// Each canonical signature id maps to a builder; pin the id↔builder
    /// agreement.
    #[test]
    fn signature_builders_match_their_ids() {
        let builders: HashMap<&str, Action> = [
            (SIG_SLIP, synthetic_slip()),
            (SIG_RAM, synthetic_ram()),
            (SIG_PHASE, synthetic_phase()),
            (SIG_THROW, synthetic_throw()),
            (SIG_SWAP_TOSS, synthetic_swap_toss()),
            (SIG_BROADSIDE_SWEEP, synthetic_broadside_sweep()),
        ].into_iter().collect();
        for (id, action) in &builders {
            assert_eq!(&action.id, id, "builder id mismatch for `{id}`");
        }
    }

    #[test]
    fn affinities_cover_all_three_variants() {
        let affs: HashSet<ClassAffinity> =
            canonical_classes().iter().map(|c| c.affinity).collect();
        assert!(affs.contains(&ClassAffinity::Flexible));
        assert!(affs.contains(&ClassAffinity::BowOn));
        assert!(affs.contains(&ClassAffinity::Broadside));
    }

    /* ---- the five canonical self-move signatures (#97-aligned) ------ */

    #[test]
    fn slip_and_swap_toss_are_tractor_swaps() {
        for a in [synthetic_slip(), synthetic_swap_toss()] {
            assert!(matches!(
                a.effects[0],
                Effect::DISPLACE_SELF { mode: MovementMode::TRACTOR_SWAP, .. }
            ), "{} should be DISPLACE_SELF TRACTOR_SWAP", a.id);
            assert!(!a.cost.advances_turn, "{} is free-fire", a.id);
        }
    }

    #[test]
    fn ram_burns_forward_throw_burns_aft() {
        assert!(matches!(
            synthetic_ram().effects[0],
            Effect::DISPLACE_SELF { mode: MovementMode::BURN, direction: None, .. }
        ));
        assert!(matches!(
            synthetic_throw().effects[0],
            Effect::DISPLACE_SELF { mode: MovementMode::BURN, direction: Some(LaneEnd::Aft), .. }
        ));
    }

    #[test]
    fn phase_is_slip_movement() {
        assert!(matches!(
            synthetic_phase().effects[0],
            Effect::DISPLACE_SELF { mode: MovementMode::SLIP, .. }
        ));
    }

    /* ---- Aegis (#50) ------------------------------------------------ */

    #[test]
    fn aegis_is_a_broadside_class_with_the_sweep_signature() {
        let a = aegis();
        assert_eq!(a.id, CLASS_AEGIS);
        assert_eq!(a.affinity, ClassAffinity::Broadside);
        assert_eq!(a.signature, SIG_BROADSIDE_SWEEP);
        assert_eq!(a.set1.len(), 2);
        assert_eq!(a.set2.len(), 2);
        assert!(a.set1.contains(&"broadside_battery".to_string()));
    }

    #[test]
    fn broadside_sweep_fires_both_flanks_then_flips() {
        let a = synthetic_broadside_sweep();
        assert_eq!(a.id, SIG_BROADSIDE_SWEEP);
        assert_eq!(a.targeting.pattern, TargetingPattern::BROADSIDE);
        assert_eq!(a.targeting.requires_arc, Some(TArc::BroadsideArc));
        assert!(matches!(a.effects[0], Effect::DAMAGE { .. }));
        assert!(matches!(
            a.effects[1],
            Effect::REORIENT { to: ReorientTo::Flip }
        ));
        assert_eq!(a.effects.len(), 2, "Sweep is exactly DAMAGE then flip");
        assert!(a.cost.heat >= 4 && a.cost.cooldown_max >= 4);
    }
}
