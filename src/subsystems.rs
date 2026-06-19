//! Runtime subsystem layer for the Phase 2 demo.
//!
//! Catalog data ([`crate::types::SubsystemDef`]) is the wire shape; this
//! module is the **behavioral** layer that turns "this ship has Marksman
//! installed" into actual damage / heat / status modifications.
//!
//! ## Why a content-side registry, not the EventBus
//!
//! The EventBus exists and has subsystem-shaped hooks
//! ([`crate::types::Hook::OnDamageDealt`], [`crate::types::Hook::OnTurnEnd`],
//! …). The natural-looking design would be: each subsystem registers an
//! `FnMut(&mut HookContext)` closure on the bus at install time, and
//! `apply_damage` / `end_of_turn` emit the corresponding hooks.
//!
//! Two problems with that route:
//!
//! 1. **Pipeline ordering.** [`crate::resolve::apply_damage`] runs step 2
//!    (`apply_modifiers`) **before** any `OnDamageDealt` emit. The
//!    `OnDamageDealt` hook in the current resolver fires once at the END of
//!    `execute_queue`, well after the modifier step. A subsystem that
//!    wanted to add `+1` to a Long-range hit couldn't subscribe to a hook
//!    fired before step 2 without reordering the emit, which is a
//!    pipeline change the role boundary forbids.
//!
//! 2. **State ownership.** Each closure would need to know which ships
//!    have which subsystems installed. That means each closure captures a
//!    `Rc<RefCell<Registry>>`. Multiply by N subsystems × M hooks and
//!    you've got a Rc graph with surprising lifetimes — and the closures
//!    can't be moved across threads (which would matter if the renderer
//!    ever needed concurrent access).
//!
//! The cleaner shape: subsystems live on the [`crate::resolve::Content`]
//! impl as plain data. The resolver calls [`Content::damage_modifier`] at
//! pipeline step 2 and [`Content::on_turn_end`] at the end-of-turn point.
//! Each method walks the installed-subsystem list for the relevant ship
//! and does the math directly. No closures, no `Rc`, no aliasing puzzles.
//!
//! This is exactly the same shape the lead approved for `damage_modifier`
//! in task #6 — extended to a second method.
//!
//! ## Registry shape
//!
//! [`Installations`] is `HashMap<ship_id -> Vec<SubsystemId>>`. The Content
//! impl owns one. Look up a ship's installed subsystems by id; the order
//! within the Vec doesn't matter (subsystem effects are commutative —
//! Marksman + Point-Blank Doctrine commute, HeatSink + HeatSink would
//! stack additively if they could collide).
//!
//! ## Catalog-vs-runtime split
//!
//! [`SubsystemId`] is the catalog id ("marksman", "point_blank_doctrine"
//! …). The runtime behavior is keyed by the same id in
//! [`damage_modifier_for`] / [`on_turn_end_for`] — these are the two
//! dispatch points. If a future subsystem needs a hook we haven't wired
//! yet, add a new Content trait method + a new dispatch arm; do NOT push
//! behavior into the catalog data shape.

use std::collections::HashMap;

use crate::grid::Range;
use crate::types::{Board, Ship};

/// Catalog id of an installed subsystem. Matches
/// [`crate::types::SubsystemDef::id`]. We keep it as a typed wrapper so a
/// future "subsystem trees / variants" feature can swap the underlying
/// storage without breaking callers.
pub type SubsystemId = String;

/// `ship_id` → vector of installed [`SubsystemId`]s. The Content impl owns
/// one; the resolver borrows it through Content trait calls.
#[derive(Debug, Default, Clone)]
pub struct Installations {
    pub by_ship: HashMap<String, Vec<SubsystemId>>,
}

impl Installations {
    pub fn new() -> Self {
        Self {
            by_ship: HashMap::new(),
        }
    }

    /// Install `subsystem_id` on the ship with the given id. Order within
    /// the vec doesn't matter for the current three subsystems; future
    /// stacking-with-priority rules would extend this.
    pub fn install(&mut self, ship_id: impl Into<String>, subsystem_id: impl Into<SubsystemId>) {
        self.by_ship
            .entry(ship_id.into())
            .or_default()
            .push(subsystem_id.into());
    }

    /// Slice of installed subsystem ids for the named ship. Empty if the
    /// ship has none.
    pub fn for_ship(&self, ship_id: &str) -> &[SubsystemId] {
        self.by_ship
            .get(ship_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/* =========================================================================
 * Behavioral dispatch.
 *
 * Two entry points, one per Content trait method:
 *   damage_modifier_for(installed, attacker, band, board) -> i32
 *   on_turn_end_for(installed, board)
 *
 * Each walks the list of installed subsystem ids and sums / applies
 * effects. New subsystems get a new match arm here (and in
 * [`SUBSYSTEM_IDS`] for catalog round-trip).
 * ====================================================================== */

/// Marksman's per-hit damage bonus. Analysis HTML reads
/// "Marksman: +1 damage at long range" — encoded as a flat `+1` whenever
/// `band == Range::Far` (the farthest 2-D band; #34 maps the 1-D "long range"
/// onto the 3-band Chebyshev model).
pub const MARKSMAN: &str = "marksman";

/// Point-Blank Doctrine's per-hit damage bonus. Analysis-doc-aligned:
/// `+2` whenever `band == Range::Adjacent` (point-blank = the nearest 2-D
/// band). Synergizes with bow-on stance.
pub const POINT_BLANK_DOCTRINE: &str = "point_blank_doctrine";

/// HeatSink's end-of-turn effect: subtract one extra heat from the
/// owning ship beyond the base passive dissipation. Stacks with itself
/// (two HeatSinks => `-2` extra). Stacks with the base `-1` dissipation
/// applied by [`crate::resolve::end_of_turn`] (so a ship with HeatSink
/// dissipates 2 heat per turn instead of 1).
pub const HEAT_SINK: &str = "heat_sink";

/// Canonical list of subsystem ids the placeholder DemoContent knows
/// about. Adding a new subsystem: add the const above, the entry here,
/// and an arm in both dispatch fns.
pub const SUBSYSTEM_IDS: &[&str] = &[MARKSMAN, POINT_BLANK_DOCTRINE, HEAT_SINK];

/// Step 2 of the damage pipeline: walk every subsystem installed on the
/// **attacker** and sum its per-hit damage bonus. Called by the
/// `Content::damage_modifier` impl on [`crate::input::DemoContent`].
///
/// `band` is the post-falloff 2-D [`Range`] bucket (#34: the 3-band Chebyshev
/// `Adjacent`/`Near`/`Far`, NOT the legacy 1-D `RangeBand`). The bonus is
/// additive — for the current three subsystems, only Marksman and Point-Blank
/// Doctrine contribute and they contribute at most one band each. Returns 0 if
/// no subsystem on the attacker matches the band.
///
/// **1-D -> 2-D band mapping (#34):** the v1 5-band subsystem flavour folds onto
/// the 3 v2 bands: "point-blank" (PBD) keys [`Range::Adjacent`], "long range"
/// (Marksman) keys [`Range::Far`]. This keeps both subsystems LIVE in 2-D — the
/// pre-#34 `Range -> RangeBand` shim collapsed `Far -> Mid`, so a `Long`-keyed
/// Marksman could never fire (no 2-D band mapped to `Long`).
///
/// **Direction (audit #67):** modifiers are attacker-side. The analysis
/// HTML's catalog descs all read "+1 damage **when firing**" / "**when
/// striking**" — attacker-frame verbs. Pre-audit code consulted the
/// target's subsystems and tests passed because each Phase 2 demo
/// installed the same subsystem set on both sides. Caller must pass the
/// attacker's installed list (not the target's).
pub fn damage_modifier_for(
    installed: &[SubsystemId],
    _attacker: &Ship,
    band: Range,
    _board: &Board,
) -> i32 {
    let mut bonus = 0;
    for id in installed {
        match id.as_str() {
            MARKSMAN if band == Range::Far => bonus += 1,
            POINT_BLANK_DOCTRINE if band == Range::Adjacent => bonus += 2,
            _ => {}
        }
    }
    bonus
}

/// End-of-turn pass. Walks every ship's installed subsystems and applies
/// the OnTurnEnd-shaped effects (today: HeatSink). Called by
/// [`crate::resolve::end_of_turn`] AFTER the base passive heat dissipation
/// and BEFORE the `OnTurnEnd` event-bus emit — so subscribers see the
/// already-cooled heat, matching the TS pipeline.
///
/// Currently the only OnTurnEnd subsystem is HeatSink. Future variants
/// (e.g. an EliteHeatSink that subtracts 2) would extend the dispatch
/// match inside the per-ship loop below.
pub fn on_turn_end_for(installations: &Installations, board: &mut Board) {
    for cell in 0..board.cells.len() {
        let Some(ship_id) = board.cells[cell].as_ref().map(|s| s.id.clone()) else {
            continue;
        };
        let extra_dissipation: i32 = installations
            .for_ship(&ship_id)
            .iter()
            .map(|id| if id == HEAT_SINK { 1 } else { 0 })
            .sum();
        if extra_dissipation == 0 {
            continue;
        }
        if let Some(s) = board.cells[cell].as_mut() {
            // Floor at 0 so HeatSink can't pull heat negative.
            s.heat = (s.heat - extra_dissipation).max(0);
            // If the dissipation drops the ship below heat_max, clear
            // lockout — matching the same invariant `end_of_turn` enforces
            // for the base dissipation.
            if s.heat < s.heat_max {
                s.locked_out = false;
            }
        }
    }
}

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::types::{EventBus, Faction, LaneEnd, Orientation, Ship};
    use std::collections::HashMap as Map;

    fn naked_ship(id: &str, cell: usize, heat: i32, heat_max: i32) -> Ship {
        Ship {
            id: id.into(),
            faction: Faction::Player,
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hull: 10,
            max_hull: 10,
            heat,
            heat_max,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: Vec::new(),
            queue: Vec::new(),
            cooldowns: Map::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    fn empty_board(ships: Vec<Ship>) -> Board {
        let size = ships.iter().map(|s| s.cell + 1).max().unwrap_or(1).max(1);
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

    #[test]
    fn installations_install_and_lookup() {
        let mut reg = Installations::new();
        reg.install("p", MARKSMAN);
        reg.install("p", HEAT_SINK);
        reg.install("enemy", MARKSMAN);
        assert_eq!(
            reg.for_ship("p"),
            &[MARKSMAN.to_string(), HEAT_SINK.to_string()]
        );
        assert_eq!(reg.for_ship("enemy"), &[MARKSMAN.to_string()]);
        assert!(reg.for_ship("unknown").is_empty());
    }

    #[test]
    fn marksman_only_adds_at_far() {
        // #34: Marksman ("long range") keys the farthest 2-D band, Range::Far.
        let attacker = naked_ship("p", 0, 0, 6);
        let board = empty_board(vec![attacker.clone()]);
        let installed = vec![MARKSMAN.to_string()];
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Adjacent, &board),
            0
        );
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Near, &board),
            0
        );
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Far, &board),
            1
        );
    }

    #[test]
    fn point_blank_doctrine_only_adds_at_adjacent() {
        // #34: Point-Blank Doctrine keys the nearest 2-D band, Range::Adjacent.
        let attacker = naked_ship("p", 0, 0, 6);
        let board = empty_board(vec![attacker.clone()]);
        let installed = vec![POINT_BLANK_DOCTRINE.to_string()];
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Adjacent, &board),
            2
        );
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Near, &board),
            0
        );
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Far, &board),
            0
        );
    }

    #[test]
    fn multiple_subsystems_stack_additively() {
        let attacker = naked_ship("p", 0, 0, 6);
        let board = empty_board(vec![attacker.clone()]);
        // Two Marksmen, two PBDs (the catalog wouldn't normally let this
        // happen — they're maxLevel-bounded — but the runtime layer must
        // sum without surprise).
        let installed = vec![
            MARKSMAN.to_string(),
            MARKSMAN.to_string(),
            POINT_BLANK_DOCTRINE.to_string(),
            POINT_BLANK_DOCTRINE.to_string(),
        ];
        // At Adjacent: only PBD applies, 2×2 = 4 bonus.
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Adjacent, &board),
            4
        );
        // At Far: only Marksman applies, 2×1 = 2 bonus.
        assert_eq!(
            damage_modifier_for(&installed, &attacker, Range::Far, &board),
            2
        );
    }

    #[test]
    fn heat_sink_dissipates_one_extra_heat_per_turn_end() {
        let ship = naked_ship("p", 0, 4, 6);
        let mut board = empty_board(vec![ship]);
        let mut reg = Installations::new();
        reg.install("p", HEAT_SINK);
        on_turn_end_for(&reg, &mut board);
        assert_eq!(
            board.cells[0].as_ref().unwrap().heat,
            3,
            "heat 4 -> 3 via one HeatSink"
        );
    }

    #[test]
    fn heat_sink_clears_lockout_when_dropping_below_max() {
        let mut ship = naked_ship("p", 0, 6, 6);
        ship.locked_out = true;
        let mut board = empty_board(vec![ship]);
        let mut reg = Installations::new();
        reg.install("p", HEAT_SINK);
        on_turn_end_for(&reg, &mut board);
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 5);
        assert!(!p.locked_out, "heat below heat_max must clear lockout");
    }

    #[test]
    fn heat_sink_floors_at_zero() {
        let ship = naked_ship("p", 0, 0, 6);
        let mut board = empty_board(vec![ship]);
        let mut reg = Installations::new();
        reg.install("p", HEAT_SINK);
        on_turn_end_for(&reg, &mut board);
        assert_eq!(
            board.cells[0].as_ref().unwrap().heat,
            0,
            "must not go negative"
        );
    }

    #[test]
    fn heat_sink_stacks() {
        let ship = naked_ship("p", 0, 5, 6);
        let mut board = empty_board(vec![ship]);
        let mut reg = Installations::new();
        reg.install("p", HEAT_SINK);
        reg.install("p", HEAT_SINK);
        on_turn_end_for(&reg, &mut board);
        assert_eq!(
            board.cells[0].as_ref().unwrap().heat,
            3,
            "two HeatSinks -> -2 extra"
        );
    }

    #[test]
    fn ship_without_subsystems_is_untouched() {
        let ship = naked_ship("p", 0, 4, 6);
        let mut board = empty_board(vec![ship]);
        let reg = Installations::new();
        on_turn_end_for(&reg, &mut board);
        assert_eq!(board.cells[0].as_ref().unwrap().heat, 4);
    }
}
