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

/// A ship at column `col` on **row 0** with bearing `facing`, one Forward mount
/// loaded with "w". Upholds invariant A (`cell == pos.to_index()`); on row 0
/// `pos.to_index() == col`, so the cell asserts below read directly as columns.
/// (#22 2-D migration: every M-scenario lives on row 0 so the BEAM bears E/W
/// along the row and the flak `+/-1` neighbours stay in-bounds and spatial.)
fn ship(id: &str, faction: Faction, col: usize, hull: i32, facing: broadside_engine::grid::Facing) -> Ship {
    let pos = broadside_engine::grid::Pos::new(col, 0);
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing,
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

/// Place ships on the fixed len-CELLS 5x4 grid at `cells[pos.to_index()]`
/// (invariant A). `size = COLS` so the flak `+/-1` bound (`board.size`) admits
/// every row-0 column.
fn board_2d(ships: Vec<Ship>) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    for s in ships {
        let idx = s.pos.to_index();
        assert!(cells[idx].is_none(), "two ships share cell {idx}");
        cells[idx] = Some(s);
    }
    Board {
        size: broadside_engine::grid::COLS,
        cells,
        ordnance: Vec::new(),
        hazards: (0..broadside_engine::grid::CELLS).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
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
    fn damage_modifier(&self, attacker: &Ship, band: broadside_engine::grid::Range, _board: &Board) -> i32 {
        // Marksman: +1 when firing at the farthest 2-D band (Range::Far, the #34
        // 2-D successor of the 1-D "Long"), from the ATTACKER's fittings only.
        match &self.marksman_on {
            Some(id) if *id == attacker.id && band == broadside_engine::grid::Range::Far => 1,
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

// #22 2-D: all on row 0 (cell index == column), op faces E. Content's M1 intent
// preserved exactly: a@(1,0) Bow(E) flak_burst raw 4 fires E; first ship is
// t@(2,0) (primary 4 -> hull 1). Flak splashes the hit cell's E-W neighbours
// (1,0) and (3,0): a itself (Player friendly self-splash, 5->4) and foe@(3,0)
// (5->4). On row 0 the flak `+/-1` indices stay in-bounds (< COLS) and ARE the
// spatial E-W neighbours, so the still-1-D splash lands correctly here.
#[test]
fn m1_flak_burst_splashes_both_neighbours_including_an_ally() {
    use broadside_engine::grid::{Dir4, Facing};
    let mut a = ship("a", Faction::Player, 1, 5, Facing::Bow(Dir4::E));
    a.queue.clear();
    let t = ship("t", Faction::Enemy, 2, 5, Facing::Bow(Dir4::W));
    let foe = ship("foe", Faction::Enemy, 3, 5, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t, foe]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 1, "primary: 4 raw onto the armour-0 target t (5 -> 1)");
    assert_eq!(
        hull_at(&b, 1),
        4,
        "flak is faction-blind: it splashes the firing Player a@(1,0) (a neighbour of the hit cell) — friendly-fire (5 -> 4)",
    );
    assert_eq!(hull_at(&b, 3), 4, "flak splashes the other neighbour foe@(3,0) too (5 -> 4)");
}

// #22 2-D + flak-2d: same row-0 layout. The splash neighbour foe@(3,0) has its
// hit-facing zone armoured 1, fully absorbing the 1 splash. flak-2d: the splash
// now routes through `apply_damage_2d`, which reads the 2-D `facing` + the 2-D
// `direction_to`. The splash arrives FROM the hit cell @(2,0) = WEST of foe@(3,0);
// foe faces W (Bow(W)), so an incoming-from-W hit lands on foe's BOW zone. Give
// BOW armour 1 -> foe takes 0. Proves the splash routes through absorb_shield.
// (Pre-flak-2d this read the 1-D orientation -> stern zone; the 2-D splash reads
// the real facing, hence bow here.)
#[test]
fn m1_flak_splash_is_shield_mediated() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 1, 5, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 2, 5, Facing::Bow(Dir4::W));
    let mut foe = ship("foe", Faction::Enemy, 3, 5, Facing::Bow(Dir4::W));
    // #103 Model A: a bow shield POOL of 1 (charge 1) soaks the 1-point flak
    // splash entirely (the splash routes through absorb_shield, not raw hull).
    foe.shield_profile.bow = ShieldFace { armour: 1, charge: 1 };
    let mut b = board_2d(vec![a, t, foe]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 2), 1, "primary still lands full (5 -> 1)");
    assert_eq!(
        hull_at(&b, 3),
        5,
        "foe bow pool 1 soaks the 1 flak splash entirely — splash routes through absorb_shield, not raw hull",
    );
}

/* =========================================================================
 * M2 — twin_linked: cost-once + between-pass re-target.
 * ====================================================================== */

// #22 2-D (row 0, op faces E): a@(0,0) Bow(E) raw 3 cd_max 4 twin_linked, t@(2,0)
// h10. Effects twice -> t.hull 4. Heat charged once -> a.heat 3. Cooldown once.
#[test]
fn m2_twin_linked_applies_twice_but_pays_cost_once() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 2, 10, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t]);
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

// #22 2-D (row 0, op faces E): a@(0,0) Bow(E) raw 3 twin_linked. t1@(1,0) h3 (dies
// to pass 1), t2@(2,0) h5. Pass 1 kills t1; pass 2 re-resolves targeting against
// the new board -> first bearing target along the E ray is now t2 -> 3 (5 -> 2).
#[test]
fn m2_twin_linked_second_pass_retargets_after_first_pass_kill() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let t1 = ship("t1", Faction::Enemy, 1, 3, Facing::Bow(Dir4::W));
    let t2 = ship("t2", Faction::Enemy, 2, 5, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t1, t2]);
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

// #22 2-D (row 0): a@(0,0) Bow(E) raw 2 incendiary; t@(1,0) Bow(W) h5. The hit
// arrives from the W (col 0 < 1); t faces W, so a shot dead ahead of its bow
// lands on the BOW zone — armour it to 5 to absorb all hull damage. t.hull stays
// 5, but HullBreach(3) lands on contact.
#[test]
fn m3_incendiary_rider_lands_even_when_shield_eats_all_hull_damage() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let mut t = ship("t", Faction::Enemy, 1, 5, Facing::Bow(Dir4::W));
    // #103 Model A: a FULL bow pool (charge 5) soaks the 2 raw entirely.
    t.shield_profile.bow = ShieldFace { armour: 5, charge: 5 };
    let mut b = board_2d(vec![a, t]);
    let content = ModContent::new(vec![damage_action("w", 2, Some("incendiary"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 1), 5, "full bow pool (5) soaks the 2 raw — no hull lost");
    assert!(
        has_status(&b, 1, StatusKind::HullBreach),
        "incendiary rider lands on CONTACT regardless of shield absorption",
    );
}

// #22 2-D (row 0): same as M3 incendiary — hit lands on t's BOW (it faces W into
// the incoming W shot); bow armour 5 absorbs the hull damage, the rider lands.
#[test]
fn m3_emp_charge_rider_lands_through_shield() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let mut t = ship("t", Faction::Enemy, 1, 5, Facing::Bow(Dir4::W));
    // #103 Model A: a FULL bow pool (charge 5) soaks the 2 raw entirely.
    t.shield_profile.bow = ShieldFace { armour: 5, charge: 5 };
    let mut b = board_2d(vec![a, t]);
    let content = ModContent::new(vec![damage_action("w", 2, Some("emp_charge"), 0, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(hull_at(&b, 1), 5, "full bow pool soaks the hull damage");
    assert!(
        has_status(&b, 1, StatusKind::SystemsOffline),
        "emp_charge applies SystemsOffline on contact",
    );
}

/* =========================================================================
 * M4 — targeting_laser: lock on hit, doubles the NEXT hit, lock consumed.
 * ====================================================================== */

// #22 2-D (row 0): a@(0,0) Bow(E) raw 2 targeting_laser; t@(1,0) h10. Shot 1 ->
// t.hull 8 + TargetLock. Shot 2 (plain raw 2) -> doubled to 4 by the lock ->
// t.hull 4, lock consumed.
#[test]
fn m4_targeting_laser_lock_doubles_the_following_hit() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 1, 10, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t]);
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

// #22 2-D (row 0): a@(0,0) Bow(E) raw 9 cd_max 5 precision_core; t@(1,0) h3 ->
// destroyed (overkill) -> a.cooldowns[w] == 0 (not 5).
#[test]
fn m5_precision_core_recharges_cooldown_to_zero_on_a_kill() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 1, 3, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t]);
    let content = ModContent::new(vec![damage_action("w", 9, Some("precision_core"), 5, 1)]);

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert!(b.cells[1].is_none(), "9 dmg overkills the 3-hull target");
    assert_eq!(
        b.cells[0].as_ref().unwrap().cooldowns.get("w").copied(),
        Some(0),
        "precision_core recharges cd to 0 on ANY lethal hit (overkill counts)",
    );
}

// #22 2-D (row 0): a@(0,0) Bow(E) raw 9 cd_max 5 precision_core; t@(1,0) h10 ->
// survives at 1 -> a.cooldowns[w] == 5 (normal cooldown, no recharge).
#[test]
fn m5_precision_core_does_not_recharge_on_a_non_lethal_hit() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 5, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 1, 10, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t]);
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

// #22 2-D (row 0): mirror M2a with an Enemy attacker firing on a Player.
// e@(0,0) Bow(E) raw 3 twin_linked vs player@(2,0) h10 -> player.hull 4; e.heat
// charged once (3).
#[test]
fn m6_enemy_fired_twin_linked_behaves_identically() {
    use broadside_engine::grid::{Dir4, Facing};
    let e = ship("e", Faction::Enemy, 0, 5, Facing::Bow(Dir4::E));
    let player = ship("player", Faction::Player, 2, 10, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![e, player]);
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

// #22 2-D (row 0): Marksman (+1 at the FAR band — the #34 2-D successor of the
// 1-D "Long") installed on the ATTACKER a@(0,0) Bow(E). Raw 4 flak_burst fired E.
// The BEAM reaches t@(3,0) as its FIRST target (cells (1,0),(2,0) empty);
// distance 3 = Far -> Marksman +1. Primary t takes 4 + 1 = 5 (10 -> 5). The flak
// splashes the hit cell's E-W neighbours (2,0)[empty] and (4,0)=n -> n takes 1
// with NO +1 (the splash's attacker is the hit cell @(3,0), no Marksman). So the
// subsystem modifier hits the PRIMARY only, not the splash. (Row 0 keeps every
// flak `+/-1` index in-bounds + spatial.)
#[test]
fn m7_marksman_modifier_applies_to_primary_not_flak_splash() {
    use broadside_engine::grid::{Dir4, Facing};
    let a = ship("a", Faction::Player, 0, 10, Facing::Bow(Dir4::E));
    let t = ship("t", Faction::Enemy, 3, 10, Facing::Bow(Dir4::W));
    let n = ship("n", Faction::Enemy, 4, 5, Facing::Bow(Dir4::W));
    let mut b = board_2d(vec![a, t, n]);
    let content = ModContent::new(vec![damage_action("w", 4, Some("flak_burst"), 0, 1)])
        .with_marksman("a");

    apply_instant_action("a", content.action("w").unwrap(), &mut b, &content);

    assert_eq!(
        hull_at(&b, 3),
        5,
        "primary: 4 raw + 1 Marksman (Far) = 5 (10 -> 5)",
    );
    assert_eq!(
        hull_at(&b, 4),
        4,
        "flak splash is 1 with NO Marksman bonus (the splash's attacker is the hit cell, not a) — modifier hits PRIMARY only (5 -> 4)",
    );
}
