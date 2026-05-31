//! Subsystem integration tests — drive the registry through the full
//! resolver pipeline.
//!
//! `src/subsystems.rs` has unit tests for `damage_modifier_for` and
//! `on_turn_end_for` called directly. This file pins that the SAME
//! behaviours are observable when the resolver drives them through the
//! `Content` trait — i.e., `DemoContent::damage_modifier` is wired
//! correctly into `apply_modifiers` (resolve.rs:1031) and
//! `DemoContent::on_turn_end` is wired into `end_of_turn`
//! (resolve.rs:442), called in the right pipeline-order positions, and
//! actually mutate observable state.
//!
//! Reference: subsystems.rs:106-185, resolve.rs:1015-1033, 442.

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
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
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
        bus: EventBus::default(),
        destroys_this_window: 0,
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
/// step 2 of the pipeline (resolve.rs:1031).
///
/// The `_target` argument to damage_modifier_for is the receiving ship,
/// and the Installations registry is keyed by ship id. Marksman is
/// installed on the TARGET (defender), because the analysis-doc
/// semantics describe defensive subsystems — the ship installs a
/// subsystem that boosts ITS modifier at certain bands. (If the team
/// later flips this to attacker-side, this test will catch it.)
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

    // With Marksman installed on target.
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
        vec![(5, 4)],
        "Marksman: +1 at Long range -> 4 lands",
    );
}

/// Marksman does NOT add at non-Long bands. Same setup as above but
/// firing at Mid range (distance 3 from attacker cell 0).
#[test]
fn marksman_is_band_gated() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("attacker", Faction::Player, 0, 10),
            naked_ship("target", Faction::Enemy, 3, 10),
        ],
    );
    let log = damage_log(&mut board);
    let mut content = DemoContent::default();
    content.install_subsystem("target", MARKSMAN);
    // Distance 0->3 = Mid band. Marksman fires only at Long.
    apply_damage(3, 3, 0, &raw_weapon(RangeBand::Mid, 3), &mut board, &content);
    assert_eq!(
        *log.borrow(),
        vec![(3, 3)],
        "Marksman must NOT add at Mid; raw 3 lands unchanged",
    );
}

/* =========================================================================
 * Point-Blank Doctrine — +2 at PointBlank, 0 elsewhere
 * ====================================================================== */

/// PBD adds +2 at PointBlank distance only (d <= 1). Raw 3 -> 5 lands.
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
    content.install_subsystem("target", POINT_BLANK_DOCTRINE);
    // Distance 0->1 = PointBlank.
    apply_damage(1, 3, 0, &raw_weapon(RangeBand::PointBlank, 3), &mut board, &content);
    assert_eq!(*log.borrow(), vec![(1, 5)], "PBD: +2 at PointBlank -> 5 lands");
}

/* =========================================================================
 * Marksman + PBD — stack at their respective bands; do not cross-pollinate
 * ====================================================================== */

/// Both subsystems installed on the target: at Long range, Marksman
/// contributes +1; at PointBlank, PBD contributes +2; at Mid, neither.
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
    content.install_subsystem("target", MARKSMAN);
    content.install_subsystem("target", POINT_BLANK_DOCTRINE);

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
