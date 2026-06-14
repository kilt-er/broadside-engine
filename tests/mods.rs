//! Weapon-mod integration suite — content's M1-M7 scenarios (#54).
//!
//! The 7 weapon-mods (commit 1619bac, `Action.r#mod` dispatch) already have
//! one inline per-mod unit test each in `resolve.rs`. This suite is the
//! INTEGRATION layer content specced: each mod driven through the public
//! `apply_instant_action` (the full gate + effect + bookkeeping pipeline),
//! exercising interactions the units don't — friendly-fire splash,
//! shield-mediated splash, riders landing through full shield absorption,
//! cost-paid-once + between-pass re-targeting, lock-then-double across two
//! shots, any-lethal cooldown recharge, enemy-fired symmetry, and mod +
//! subsystem-modifier stacking order.
//!
//! Convention (content's): all ships have all-zero shield faces unless a
//! face is explicitly armoured; collision/raw arithmetic is therefore exact.
//! A "modded action" is a DAMAGE action with `r#mod = Some("<mod_id>")`.
//!
//! Autoloader (#7) is deliberately ABSENT: its only effect (turn-advance
//! override) is observable solely at the `input.rs` dispatch layer via
//! `action_advances_turn`, which `input.rs` does not call yet — there is no
//! resolver-observable behavior to assert until that seam lands (parked
//! follow-up). `resolve.rs`'s inline `mod_autoloader_overrides_advances_turn_for_dispatch`
//! already covers the seam function itself.

use broadside_engine::resolve::{apply_instant_action, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, EventBus, Effect, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, StatusKind, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

fn zero_profile() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 0, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 0, charge: 0 },
        starboard: ShieldFace { armour: 0, charge: 0 },
    }
}

fn ship(id: &str, faction: Faction, cell: usize, hull: i32) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: zero_profile(),
        mounts: vec![Mount { id: "m1".into(), arc: Arc::Forward, weapon: "w".into() }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

fn board(size: usize, cells: Vec<Option<Ship>>) -> Board {
    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/// A forward DAMAGE action carrying an optional mod. `raw` damage, no band
/// falloff (Some(false)) so the landed number is exactly `raw` minus armour
/// — keeps every M-scenario's arithmetic legible. Wide band so range never
/// gates the shot.
fn damage_action(id: &str, raw: i32, r#mod: Option<&str>, cooldown_max: i32, heat: i32) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat, cooldown_max, advances_turn: true },
        targeting: Targeting {
            range_band: vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: raw, band_falloff: Some(false) }],
        r#mod: r#mod.map(|s| s.to_string()),
        icon: None,
    }
}

/// Content holding a set of actions by id. Optionally emits a Marksman-style
/// +1 damage modifier at Long for one named attacker (M7).
struct ModContent {
    actions: HashMap<String, Action>,
    marksman_on: Option<String>,
}
impl ModContent {
    fn new(actions: Vec<Action>) -> Self {
        ModContent {
            actions: actions.into_iter().map(|a| (a.id.clone(), a)).collect(),
            marksman_on: None,
        }
    }
    fn with_marksman(mut self, ship_id: &str) -> Self {
        self.marksman_on = Some(ship_id.into());
        self
    }
}
impl Content for ModContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        unreachable!("mod tests fire beams, not ordnance");
    }
    fn damage_modifier(&self, attacker: &Ship, band: RangeBand, _board: &Board) -> i32 {
        // Marksman: +1 when firing at Long, from the ATTACKER's fittings only.
        match &self.marksman_on {
            Some(id) if *id == attacker.id && band == RangeBand::Long => 1,
            _ => 0,
        }
    }
}

fn hull_at(board: &Board, cell: usize) -> i32 {
    board.cells[cell].as_ref().expect("ship present").hull
}

fn has_status(board: &Board, cell: usize, kind: StatusKind) -> bool {
    board.cells[cell]
        .as_ref()
        .map(|s| s.statuses.iter().any(|st| st.kind == kind))
        .unwrap_or(false)
}

/* =========================================================================
 * M1 — flak_burst: adjacency + friendly-fire + shield-mediation.
 * ====================================================================== */

#[test]
fn m1_flak_burst_splashes_both_neighbours_including_an_ally() {
    // Content's M1 intent: primary takes 4 (→1), flak splashes BOTH lane-
    // neighbours of the hit cell by 1, one of them a friendly (friendly-fire
    // confirmed). Content's literal cells put an ally BETWEEN attacker and
    // target, but flak rides a BEAM (first-ship-in-path), so the BEAM would
    // hit that intervening ally as the primary, not the target. Repositioned
    // to a BEAM-legal layout that preserves every stated assertion:
    //   a@1 (Player, flak_burst raw 4) fires Fore; FIRST ship it meets is
    //   t@2 (Enemy h5) → primary 4 → t.hull 1. Flak splashes the hit cell's
    //   neighbours @1 and @3: a@1 itself (Player → friendly self-splash, 5→4)
    //   and foe@3 (Enemy h5 → 5→4). Both neighbours splashed; one friendly.
    let mut a = ship("a", Faction::Player, 1, 5);
    a.queue.clear();
    let t = ship("t", Faction::Enemy, 2, 5);
    let foe = ship("foe", Faction::Enemy, 3, 5);
    let mut b = board(5, vec![None, Some(a), Some(t), Some(foe), None]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 1, "primary: 4 raw onto the armour-0 target t (5 -> 1)");
    assert_eq!(
        hull_at(&b, 1),
        4,
        "flak is faction-blind: it splashes the firing Player a@1 (a neighbour of the hit cell) — friendly-fire (5 -> 4)",
    );
    assert_eq!(hull_at(&b, 3), 4, "flak splashes the other neighbour foe@3 too (5 -> 4)");
}

#[test]
fn m1_flak_splash_is_shield_mediated() {
    // BEAM-legal layout (same as above): a@1 fires Fore, first ship is t@2
    // (primary). The splash neighbour foe@3 has its hit-facing zone armoured
    // 1, fully absorbing the 1 splash. The splash arrives at foe@3 FROM the
    // hit cell @2 (the Aft direction relative to foe, since 2 < 3) → foe's
    // STERN zone (bow=Fore). Give stern armour 1 → foe takes 0.
    let a = ship("a", Faction::Player, 1, 5);
    let t = ship("t", Faction::Enemy, 2, 5);
    let mut foe = ship("foe", Faction::Enemy, 3, 5);
    foe.shield_profile.stern = ShieldFace { armour: 1, charge: 0 };
    let mut b = board(5, vec![None, Some(a), Some(t), Some(foe), None]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 1, "primary still lands full (5 -> 1)");
    assert_eq!(
        hull_at(&b, 3),
        5,
        "foe bow armour 1 absorbs the 1 flak splash entirely — splash routes through absorb_shield, not raw hull",
    );
}

/* =========================================================================
 * M2 — twin_linked: cost-once + between-pass re-target.
 * ====================================================================== */

#[test]
fn m2_twin_linked_applies_twice_but_pays_cost_once() {
    // a@0 (h5, heat0, heat_max6), raw 3, cd_max 4, twin_linked. t@2 h10.
    // Effects twice → t.hull 4. Heat charged once → a.heat 3. Cooldown set
    // once → a.cooldowns[w] == 4.
    let a = ship("a", Faction::Player, 0, 5);
    let t = ship("t", Faction::Enemy, 2, 10);
    let mut b = board(5, vec![Some(a), None, Some(t), None, None]);
    let content = ModContent::new(vec![damage_action("w", 3, Some("twin_linked"), 4, 3)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 4, "twin_linked lands 3 twice = 6 (10 -> 4)");
    let attacker = b.cells[0].as_ref().expect("attacker alive");
    assert_eq!(attacker.heat, 3, "heat paid ONCE (3), not per-volley (would be 6)");
    assert_eq!(
        attacker.cooldowns.get("w").copied(),
        Some(4),
        "cooldown set once to cd_max",
    );
}

#[test]
fn m2_twin_linked_second_pass_retargets_after_first_pass_kill() {
    // a@0 raw 3 twin_linked. t1@1 h3 (dies to pass 1), t2@2 h5. Pass 1 kills
    // t1; pass 2 re-resolves targeting against the new board → first target
    // toward the bow is now t2 → t2 takes 3 (5 -> 2).
    let a = ship("a", Faction::Player, 0, 5);
    let t1 = ship("t1", Faction::Enemy, 1, 3);
    let t2 = ship("t2", Faction::Enemy, 2, 5);
    let mut b = board(5, vec![Some(a), Some(t1), Some(t2), None, None]);
    let content = ModContent::new(vec![damage_action("w", 3, Some("twin_linked"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert!(b.cells[1].is_none(), "pass 1 kills t1 (3 dmg vs 3 hull)");
    assert_eq!(
        hull_at(&b, 2),
        2,
        "pass 2 re-resolves targeting onto t2 (the new first-bearing target) and lands 3 (5 -> 2)",
    );
}

/* =========================================================================
 * M3 — incendiary / emp: riders land on CONTACT, through full shield absorption.
 * ====================================================================== */

#[test]
fn m3_incendiary_rider_lands_even_when_shield_eats_all_hull_damage() {
    // a@0 raw 2 incendiary; t@1 h5. The hit arrives FROM cell 0 = the Aft
    // direction (0 < 1), so on a bow=Fore ship it lands on the STERN zone —
    // armour that face to 5 to absorb all hull damage. t.hull stays 5, but
    // HullBreach(3) lands on contact.
    let a = ship("a", Faction::Player, 0, 5);
    let mut t = ship("t", Faction::Enemy, 1, 5);
    t.shield_profile.stern = ShieldFace { armour: 5, charge: 0 };
    let mut b = board(3, vec![Some(a), Some(t), None]);
    let content = ModContent::new(vec![damage_action("w", 2, Some("incendiary"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 1), 5, "bow armour 5 fully absorbs the 2 raw — no hull lost");
    assert!(
        has_status(&b, 1, StatusKind::HullBreach),
        "incendiary rider lands on CONTACT regardless of shield absorption",
    );
}

#[test]
fn m3_emp_charge_rider_lands_through_shield() {
    let a = ship("a", Faction::Player, 0, 5);
    let mut t = ship("t", Faction::Enemy, 1, 5);
    // Hit arrives from the Aft direction → Stern zone (see M3 incendiary note).
    t.shield_profile.stern = ShieldFace { armour: 5, charge: 0 };
    let mut b = board(3, vec![Some(a), Some(t), None]);
    let content = ModContent::new(vec![damage_action("w", 2, Some("emp_charge"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 1), 5, "shield absorbs the hull damage");
    assert!(
        has_status(&b, 1, StatusKind::SystemsOffline),
        "emp_charge applies SystemsOffline on contact",
    );
}

/* =========================================================================
 * M4 — targeting_laser: lock on hit, doubles the NEXT hit, lock consumed.
 * ====================================================================== */

#[test]
fn m4_targeting_laser_lock_doubles_the_following_hit() {
    // a@0 raw 2 targeting_laser; t@1 h10. Shot 1 → t.hull 8 + TargetLock.
    // Shot 2 (plain raw 2) → doubled to 4 by the lock → t.hull 4, lock gone.
    let a = ship("a", Faction::Player, 0, 5);
    let t = ship("t", Faction::Enemy, 1, 10);
    let mut b = board(3, vec![Some(a), Some(t), None]);
    let content = ModContent::new(vec![
        damage_action("laser", 2, Some("targeting_laser"), 0, 1),
        damage_action("plain", 2, None, 0, 1),
    ]);

    apply_instant_action("a", content.action("laser").unwrap(), &mut b, &content);
    assert_eq!(hull_at(&b, 1), 8, "first shot lands 2 (10 -> 8)");
    assert!(has_status(&b, 1, StatusKind::TargetLock), "targeting_laser applies TargetLock on hit");

    apply_instant_action("a", content.action("plain").unwrap(), &mut b, &content);
    assert_eq!(hull_at(&b, 1), 4, "TargetLock doubles the next 2-dmg hit to 4 (8 -> 4)");
    assert!(
        !has_status(&b, 1, StatusKind::TargetLock),
        "TargetLock is consumed by the hit it doubled",
    );
}

/* =========================================================================
 * M5 — precision_core: any-lethal (incl. overkill) recharges cd→0; non-lethal doesn't.
 * ====================================================================== */

#[test]
fn m5_precision_core_recharges_cooldown_to_zero_on_a_kill() {
    // a@0 raw 9 cd_max 5 precision_core; t@1 h3 → destroyed (overkill) →
    // a.cooldowns[w] == 0 (not 5).
    let a = ship("a", Faction::Player, 0, 5);
    let t = ship("t", Faction::Enemy, 1, 3);
    let mut b = board(3, vec![Some(a), Some(t), None]);
    let content = ModContent::new(vec![damage_action("w", 9, Some("precision_core"), 5, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert!(b.cells[1].is_none(), "9 dmg overkills the 3-hull target");
    assert_eq!(
        b.cells[0].as_ref().unwrap().cooldowns.get("w").copied(),
        Some(0),
        "precision_core recharges cd to 0 on ANY lethal hit (overkill counts)",
    );
}

#[test]
fn m5_precision_core_does_not_recharge_on_a_non_lethal_hit() {
    // a@0 raw 9 cd_max 5 precision_core; t@1 h10 → survives at 1 →
    // a.cooldowns[w] == 5 (normal cooldown, no recharge).
    let a = ship("a", Faction::Player, 0, 5);
    let t = ship("t", Faction::Enemy, 1, 10);
    let mut b = board(3, vec![Some(a), Some(t), None]);
    let content = ModContent::new(vec![damage_action("w", 9, Some("precision_core"), 5, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 1), 1, "target survives at 1 (10 - 9)");
    assert_eq!(
        b.cells[0].as_ref().unwrap().cooldowns.get("w").copied(),
        Some(5),
        "no kill => cooldown set to cd_max, NOT recharged",
    );
}

/* =========================================================================
 * M6 — enemy-fired symmetry: mods are faction-agnostic.
 * ====================================================================== */

#[test]
fn m6_enemy_fired_twin_linked_behaves_identically() {
    // Mirror M2a with an Enemy attacker firing on a Player. e@0 (Enemy) raw 3
    // twin_linked vs player@2 (Player) h10 → player.hull 4; e.heat charged
    // once (3).
    let e = ship("e", Faction::Enemy, 0, 5);
    let player = ship("player", Faction::Player, 2, 10);
    let mut b = board(5, vec![Some(e), None, Some(player), None, None]);
    let content = ModContent::new(vec![damage_action("w", 3, Some("twin_linked"), 4, 3)]);

    apply_instant_action("e", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 4, "enemy twin_linked lands 3 twice on the player (10 -> 4)");
    assert_eq!(
        b.cells[0].as_ref().unwrap().heat,
        3,
        "enemy pays heat once too — mods are faction-agnostic",
    );
}

/* =========================================================================
 * M7 — mod + subsystem-modifier stacking: Marksman hits the PRIMARY only,
 *      not the flak splash.
 * ====================================================================== */

#[test]
fn m7_marksman_modifier_applies_to_primary_not_flak_splash() {
    // Marksman (+1 at Long) installed on the ATTACKER a@0. Raw 4 flak_burst
    // fired at LONG range. The BEAM must reach t as its FIRST target, so there
    // is NO ship between a@0 and t@5 (distance 5 = Long), and the splash
    // neighbour n sits on t's FAR side at @6 (so it doesn't intercept the
    // beam). Primary t@5 takes 4 + 1 (Marksman) = 5 (10 -> 5). The flak
    // splashes the hit cell's neighbour n@6 for 1 — with NO +1, because the
    // splash's attacker is the hit cell @5 (no Marksman). So the subsystem
    // modifier hits the PRIMARY only, not the splash.
    let a = ship("a", Faction::Player, 0, 10);
    let t = ship("t", Faction::Enemy, 5, 10);
    let n = ship("n", Faction::Enemy, 6, 5);
    let mut b = board(7, vec![Some(a), None, None, None, None, Some(t), Some(n)]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)])
        .with_marksman("a");

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(
        hull_at(&b, 5),
        5,
        "primary: 4 raw + 1 Marksman (Long) = 5 (10 -> 5)",
    );
    assert_eq!(
        hull_at(&b, 6),
        4,
        "flak splash is 1 with NO Marksman bonus (the splash's attacker is the hit cell, not a) — modifier hits PRIMARY only (5 -> 4)",
    );
}
