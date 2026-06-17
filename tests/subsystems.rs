//! Subsystem integration tests — drive the registry through the full
//! resolver pipeline.
//!
//! `src/subsystems.rs` has unit tests for `damage_modifier_for` and
//! `on_turn_end_for` called directly. This file pins that the SAME
//! behaviours are observable when the resolver drives them through the
//! `Content` trait — i.e., `DemoContent::damage_modifier` is wired
//! correctly into `apply_modifiers` (resolve.rs:1115) and
//! `DemoContent::on_turn_end` is wired into `end_of_turn`, called in
//! the right pipeline-order positions, and actually mutate observable
//! state.
//!
//! Per audit #67 (commit c441295), damage_modifier fires from the
//! ATTACKER's installed subsystems, not the target's. Marksman / PBD
//! installs in this file are on the attacker; an inverse test pins the
//! "no bonus when installed on the target" semantics.
//!
//! Reference: subsystems.rs:106-185, resolve.rs:1105-1117.

use broadside_engine::input::DemoContent;
use broadside_engine::resolve::{apply_damage, resolve_round};
use broadside_engine::subsystems::{HEAT_SINK, MARKSMAN, POINT_BLANK_DOCTRINE};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, Hook, HookContext, LaneEnd, Mount,
    Orientation, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern,
    WeaponArchetype,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// Naked ship at `cell` (no armour, no charge) so the damage arithmetic
/// reflects only falloff + subsystem modifiers, not directional shielding.
fn naked_ship(id: &str, faction: Faction, cell: usize, hull: i32) -> Ship {
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
        shield_profile: ShieldProfile {
            bow: ShieldFace { armour: 0, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 0, charge: 0 },
            starboard: ShieldFace { armour: 0, charge: 0 },
        },
        mounts: vec![Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "pulse_laser".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// Board with `ships` placed at their declared cells, padded to `size`.
fn board_with(size: usize, ships: Vec<Ship>) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..size).map(|_| None).collect();
    for s in ships {
        let c = s.cell;
        cells[c] = Some(s);
    }
    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/// `bandFalloff: false` weapon at a given `optimal` band so the test
/// controls exactly which band the hit lands at without falloff scaling
/// the raw damage.
fn raw_weapon(optimal: RangeBand, amount: i32) -> Action {
    Action {
        id: "raw".into(),
        name: "Raw".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
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
            optimal_band: optimal,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount, band_falloff: Some(false) }],
        r#mod: None,
        icon: None,
    }
}

fn damage_log(board: &mut Board) -> Rc<RefCell<Vec<(usize, i32)>>> {
    let log: Rc<RefCell<Vec<(usize, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    let inner = Rc::clone(&log);
    board.bus.on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
        if let (Some(c), Some(a)) = (ctx.target_cell, ctx.amount) {
            inner.borrow_mut().push((c, a));
        }
    });
    log
}

/* =========================================================================
 * Marksman — +1 at Long, 0 elsewhere, through the Content trait
 * ====================================================================== */

/// Without Marksman installed, raw 3 damage at long range lands 3.
/// With Marksman installed on the TARGET, the damage_modifier adds +1
/// at Long band only — total 4. The +1 comes from
/// `Content::damage_modifier` being called inside `apply_modifiers` at
/// step 2 of the pipeline (resolve.rs:1115).
///
/// Per audit #67 (commit c441295), subsystem damage bonuses fire from
/// the ATTACKER's installed subsystems, not the target's. Marksman is
/// installed on the attacker; the bonus applies when the attacker
/// fires at Long range. Inverting the install — Marksman on the target
/// — must NOT produce a bonus; that case is pinned by
/// `marksman_is_not_attached_to_target_side` below.
#[test]
fn marksman_adds_one_damage_at_long_through_content_trait() {
    // Baseline: no Marksman.
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 5, 10),
        ],
    );
    let log = damage_log(&mut board);
    let content = DemoContent::default();
    apply_damage(5, 3, 0, &raw_weapon(RangeBand::Long, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(5, 3)], "baseline: no modifier, 3 lands");

    // With Marksman installed on the ATTACKER.
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 5, 10),
        ],
    );
    let log = damage_log(&mut board);
    let mut content = DemoContent::default();
    content.install_subsystem("attacker", MARKSMAN);
    apply_damage(5, 3, 0, &raw_weapon(RangeBand::Long, 3), &mut board, &content);
    assert_eq!(
        *log.borrow(),
        vec![(5, 4)],
        "Marksman on attacker: +1 at Long range -> 4 lands",
    );
}

/// Inverse of the above: Marksman installed on the TARGET (defender)
/// must NOT produce a bonus. Pins the attacker-side semantics from
/// audit #67 — if a future port accidentally re-flips back to
/// defender-side, this test fails immediately.
#[test]
fn marksman_on_target_does_not_modify_damage() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 5, 10),
        ],
    );
    let log = damage_log(&mut board);
    let mut content = DemoContent::default();
    content.install_subsystem("target", MARKSMAN);
    apply_damage(5, 3, 0, &raw_weapon(RangeBand::Long, 3), &mut board, &content);
    assert_eq!(
        *log.borrow(),
        vec![(5, 3)],
        "Marksman is attacker-side; installing it on the target must NOT add the bonus",
    );
}

/// Marksman does NOT add at a non-Far band. Same setup as the
/// attacker-side install above but firing at a CLOSE distance (distance 2
/// from attacker cell 0). The install is on the ATTACKER per audit #67;
/// installing on the target would make this test pass for the wrong
/// reason (no bonus from any band) so the install side matters.
///
/// #34 note: Marksman now keys the 2-D [`broadside_engine::grid::Range::Far`]
/// (the 1-D `apply_damage` path maps its `RangeBand` up to the 3-band 2-D
/// `Range` — `PointBlank->Adjacent`, `Close->Near`, `Mid|Long|Extreme->Far`).
/// Distance 3+ all fold into `Far`, so the "non-firing" gap is now `Adjacent`
/// (d<=1) and `Near` (d==2). We fire at distance 2 (`Near`) — a band where
/// Marksman genuinely does NOT contribute — so the gate is still under test.
#[test]
fn marksman_is_band_gated() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 2, 10),
        ],
    );
    let log = damage_log(&mut board);
    let mut content = DemoContent::default();
    content.install_subsystem("attacker", MARKSMAN);
    // Distance 0->2 = Close (1-D) -> Near (2-D). Marksman fires only at Far.
    apply_damage(2, 3, 0, &raw_weapon(RangeBand::Close, 3), &mut board, &content);
    assert_eq!(
        *log.borrow(),
        vec![(2, 3)],
        "Marksman on attacker must NOT add at Near (distance 2); raw 3 lands unchanged",
    );
}

/* =========================================================================
 * Point-Blank Doctrine — +2 at PointBlank, 0 elsewhere
 * ====================================================================== */

/// PBD adds +2 at PointBlank distance only (d <= 1). Raw 3 -> 5 lands.
/// Install is on the ATTACKER per audit #67.
#[test]
fn point_blank_doctrine_adds_two_at_point_blank_through_content_trait() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 1, 10),
        ],
    );
    let log = damage_log(&mut board);
    let mut content = DemoContent::default();
    content.install_subsystem("attacker", POINT_BLANK_DOCTRINE);
    // Distance 0->1 = PointBlank.
    apply_damage(1, 3, 0, &raw_weapon(RangeBand::PointBlank, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(1, 5)], "PBD on attacker: +2 at PointBlank -> 5 lands");
}

/* =========================================================================
 * Marksman + PBD — stack at their respective bands; do not cross-pollinate
 * ====================================================================== */

/// Both subsystems installed on the ATTACKER (per audit #67): at Long
/// range, Marksman contributes +1; at PointBlank, PBD contributes +2;
/// at Mid, neither.
#[test]
fn marksman_and_pbd_cooperate_at_their_respective_bands() {
    let make_board = || {
        board_with(
            7,
            vec![
                naked_ship("attacker", Faction::Player, 0, 10),
                naked_ship("target", Faction::Enemy, 5, 10),
            ],
        )
    };
    let mut content = DemoContent::default();
    content.install_subsystem("attacker", MARKSMAN);
    content.install_subsystem("attacker", POINT_BLANK_DOCTRINE);

    // Long range: Marksman applies, PBD doesn't.
    let mut board = make_board();
    let log = damage_log(&mut board);
    apply_damage(5, 3, 0, &raw_weapon(RangeBand::Long, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(5, 4)], "at Long: +1 from Marksman only");

    // PointBlank range: PBD applies, Marksman doesn't. Place target at cell 1.
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 1, 10),
        ],
    );
    let log = damage_log(&mut board);
    apply_damage(1, 3, 0, &raw_weapon(RangeBand::PointBlank, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(1, 5)], "at PointBlank: +2 from PBD only");
}

/* =========================================================================
 * HeatSink — one extra heat dissipation per turn end, through Content trait
 * ====================================================================== */

/// Without HeatSink, end_of_turn dissipates exactly 1 heat. With
/// HeatSink, the Content::on_turn_end hook dissipates 1 EXTRA on top of
/// the base. Net: heat 4 -> 2 in a HeatSink turn (1 base + 1 extra).
///
/// resolve.rs:442 calls `content.on_turn_end(board)` BEFORE the
/// `OnTurnEnd` bus emit and AFTER the base passive dissipation. So
/// subsystem state should be observable on the board right after
/// resolve_round returns.
#[test]
fn heat_sink_adds_one_extra_dissipation_per_resolve_round() {
    // Baseline: no HeatSink.
    let mut player = naked_ship("p", Faction::Player, 0, 10);
    player.heat = 4;
    let mut board = board_with(7, vec![player]);
    let content = DemoContent::default();
    resolve_round(&mut board, &content);
    let p = board.cells[0].as_ref().expect("player");
    assert_eq!(p.heat, 3, "baseline: heat 4 -> 3 (one base dissipation)");

    // With HeatSink.
    let mut player = naked_ship("p", Faction::Player, 0, 10);
    player.heat = 4;
    let mut board = board_with(7, vec![player]);
    let mut content = DemoContent::default();
    content.install_subsystem("p", HEAT_SINK);
    resolve_round(&mut board, &content);
    let p = board.cells[0].as_ref().expect("player");
    assert_eq!(p.heat, 2, "with HeatSink: heat 4 -> 2 (1 base + 1 extra)");
}

/// Two HeatSinks dissipate 1+2 = 3 heat per resolve_round. The HeatSink
/// behavior at subsystems.rs:166-185 explicitly stacks, so the integration
/// path must preserve that.
#[test]
fn two_heat_sinks_stack_additively_through_resolve_round() {
    let mut player = naked_ship("p", Faction::Player, 0, 10);
    player.heat = 5;
    let mut board = board_with(7, vec![player]);
    let mut content = DemoContent::default();
    content.install_subsystem("p", HEAT_SINK);
    content.install_subsystem("p", HEAT_SINK);
    resolve_round(&mut board, &content);
    let p = board.cells[0].as_ref().expect("player");
    assert_eq!(p.heat, 2, "heat 5 -> 2 (1 base + 2 extra from stacked HeatSinks)");
}

/// HeatSink can pull a ship out of lockout when the dropped heat falls
/// below heat_max. This is the lockout-clear path through the resolver.
#[test]
fn heat_sink_clears_lockout_after_resolve_round() {
    let mut player = naked_ship("p", Faction::Player, 0, 10);
    player.heat = 6;
    player.heat_max = 6;
    player.locked_out = true;
    let mut board = board_with(7, vec![player]);
    let mut content = DemoContent::default();
    content.install_subsystem("p", HEAT_SINK);
    resolve_round(&mut board, &content);
    let p = board.cells[0].as_ref().expect("player");
    // Base dissipation: 6 -> 5 (now below heat_max, lockout clears).
    // HeatSink: 5 -> 4.
    assert_eq!(p.heat, 4, "1 base + 1 HeatSink");
    assert!(!p.locked_out, "heat below heat_max must clear lockout");
}

/* =========================================================================
 * Ship without subsystems is untouched — sanity for the empty-registry path
 * ====================================================================== */

#[test]
fn empty_registry_doesnt_perturb_damage_or_heat() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 5, 10),
        ],
    );
    let log = damage_log(&mut board);
    let content = DemoContent::default();
    apply_damage(5, 3, 0, &raw_weapon(RangeBand::Long, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(5, 3)], "no Installations = no damage modifier");

    // And end_of_turn dissipates the base 1 heat with no extra.
    let mut player = naked_ship("p", Faction::Player, 0, 10);
    player.heat = 4;
    let mut board = board_with(7, vec![player]);
    resolve_round(&mut board, &content);
    let p = board.cells[0].as_ref().unwrap();
    assert_eq!(p.heat, 3, "no HeatSink, base dissipation only");
}
