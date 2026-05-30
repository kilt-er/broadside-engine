//! Input plumbing: framework-agnostic key enum, the canonical key->intent
//! mapping for the demo, and a small content layer that registers the
//! synthetic actions (move/flip/vent) the queue needs to flow them through
//! the normal resolver pipeline.
//!
//! This module is `pub` so the demo binary (`src/bin/broadside.rs`) can
//! drive it from winit events without the library having to depend on
//! winit. The bin owns the `winit::KeyCode -> input::Key` translation; a
//! helper for that translation lives behind `feature = "render"` so the
//! lib-only build stays winit-free.
//!
//! ## Roles
//!
//! - [`Key`] — framework-agnostic key identity. One variant per binding the
//!   tutorial overlay advertises.
//! - [`Intent`] — what the player meant by pressing that key. Per team-lead's
//!   Phase 1 spec.
//! - [`key_to_intent`] — the canonical mapping. Digit keys are gated by the
//!   ship's actual mount count, so D3 with two mounts returns `None`.
//! - [`intent_to_action_id`] — converts an Intent into the action id the
//!   resolver's queue understands. Synthetic ids start with `__` so they
//!   can't collide with real catalog actions.
//! - [`DemoContent`] — a small `Content` impl pre-loaded with the
//!   synthetic actions and the demo's mount weapons (`pulse_laser`,
//!   `torpedo`). The bin can construct one of these and pass it to
//!   `resolve_round` directly.
//! - [`tutorial_lines`] — the one-line-per-binding strings the renderer
//!   draws as a top-of-screen overlay.

use std::collections::HashMap;

use crate::resolve::Content;
use crate::types::{
    Action, ActionCost, Arc as TArc, Effect, MovementMode, Projectile, RangeBand,
    ReorientTo, Ship, Targeting, TargetingPattern, WeaponArchetype,
};

/* =========================================================================
 * Framework-agnostic key identity.
 * ====================================================================== */

/// One variant per binding advertised by [`tutorial_lines`]. The bin maps
/// `winit::keyboard::KeyCode` onto this enum; the lib never depends on
/// winit. Adding a binding goes here, then in [`key_to_intent`], then in
/// [`tutorial_lines`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Left,
    Right,
    Tab,
    /// Letter V. Vent heat.
    V,
    /// Digit 1.
    D1,
    /// Digit 2.
    D2,
    /// Digit 3.
    D3,
    /// Letter R. Commit-turn alias for Space.
    R,
    Space,
    Enter,
}

/* =========================================================================
 * Intent — what the player meant.
 * ====================================================================== */

/// What the player meant by a keypress. Variants match team-lead's Phase 1
/// spec at task #43. `QueueAction(id)` carries the canonical action id the
/// queue should accumulate; synthetic intents map to fixed ids via
/// [`intent_to_action_id`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Push a real action onto the ship's queue. The id refers to a mount's
    /// weapon (e.g. `"pulse_laser"`).
    QueueAction(String),
    /// Push the synthetic `__move_left` action — DISPLACE_SELF in the
    /// `Aft` lane direction by 1 cell via THRUST.
    MoveLeft,
    /// Push the synthetic `__move_right` action — DISPLACE_SELF in the
    /// `Fore` lane direction by 1 cell via THRUST.
    MoveRight,
    /// Push the synthetic `__reorient_flip` action — REORIENT::Flip.
    ReorientFlip,
    /// Push the synthetic `__vent` action — VENT_HEAT 3, recharge cooldowns.
    Vent,
    /// Resolve the current round (call `resolve_round`).
    CommitTurn,
    /// Restart the scene (rebuild Board from scratch).
    Restart,
}

/* =========================================================================
 * The mapping.
 * ====================================================================== */

/// Canonical key bindings for the Phase 1 demo (task #43).
///
/// | Key            | Intent                                       |
/// |----------------|----------------------------------------------|
/// | `Left`         | [`Intent::MoveLeft`]                         |
/// | `Right`        | [`Intent::MoveRight`]                        |
/// | `Tab`          | [`Intent::ReorientFlip`]                     |
/// | `V`            | [`Intent::Vent`]                             |
/// | `D1` / `D2` / `D3` | [`Intent::QueueAction`] of `ship.mounts[N].weapon`, **only if** `N < mounts.len()`. `None` otherwise. |
/// | `R`, `Space`   | [`Intent::CommitTurn`]                       |
/// | `Enter`        | [`Intent::Restart`]                          |
///
/// Returns `None` for an unbound key OR for a digit key past the ship's
/// mount count. `content` is accepted in the signature so a future binding
/// can resolve content-dependent intents (e.g. a "queue a class signature"
/// key that looks up the ship's class); today it is unused, kept in the
/// signature for forward compatibility per renderer's proposal.
pub fn key_to_intent(key: Key, ship: &Ship, _content: &dyn Content) -> Option<Intent> {
    match key {
        Key::Left => Some(Intent::MoveLeft),
        Key::Right => Some(Intent::MoveRight),
        Key::Tab => Some(Intent::ReorientFlip),
        Key::V => Some(Intent::Vent),
        Key::D1 => mount_action(ship, 0).map(Intent::QueueAction),
        Key::D2 => mount_action(ship, 1).map(Intent::QueueAction),
        Key::D3 => mount_action(ship, 2).map(Intent::QueueAction),
        Key::R | Key::Space => Some(Intent::CommitTurn),
        Key::Enter => Some(Intent::Restart),
    }
}

fn mount_action(ship: &Ship, idx: usize) -> Option<String> {
    ship.mounts.get(idx).map(|m| m.weapon.clone())
}

/// Convert an [`Intent`] into the action id the resolver's queue
/// understands. Synthetic ids start with `__` so they can't collide with
/// real catalog entries.
///
/// Returns `None` for control-flow intents ([`Intent::CommitTurn`],
/// [`Intent::Restart`]) — those are not queued; the caller handles them
/// directly.
pub fn intent_to_action_id(intent: &Intent) -> Option<&str> {
    match intent {
        Intent::QueueAction(id) => Some(id.as_str()),
        Intent::MoveLeft => Some(SYNTHETIC_MOVE_LEFT),
        Intent::MoveRight => Some(SYNTHETIC_MOVE_RIGHT),
        Intent::ReorientFlip => Some(SYNTHETIC_REORIENT_FLIP),
        Intent::Vent => Some(SYNTHETIC_VENT),
        Intent::CommitTurn | Intent::Restart => None,
    }
}

/* =========================================================================
 * Synthetic action ids + builders.
 *
 * These exist so the synthetic intents (move/flip/vent) can flow through
 * the normal `execute_queue` pipeline without the resolver special-casing
 * any of them. The bin builds a [`DemoContent`] that knows these ids and
 * returns the canonical Action records.
 * ====================================================================== */

pub const SYNTHETIC_MOVE_LEFT: &str = "__move_left";
pub const SYNTHETIC_MOVE_RIGHT: &str = "__move_right";
pub const SYNTHETIC_REORIENT_FLIP: &str = "__reorient_flip";
pub const SYNTHETIC_VENT: &str = "__vent";

/// All five-band coverage so the resolver's "is this band allowed?"
/// gate never rejects a synthetic move/vent at unexpected ranges.
fn all_bands() -> Vec<RangeBand> {
    vec![
        RangeBand::PointBlank,
        RangeBand::Close,
        RangeBand::Mid,
        RangeBand::Long,
        RangeBand::Extreme,
    ]
}

fn self_targeting() -> Targeting {
    Targeting {
        pattern: TargetingPattern::SELF,
        band: all_bands(),
        optimal_band: RangeBand::PointBlank,
        requires_arc: None,
        facing_relative: false,
        hits_all: false,
    }
}

fn zero_cost() -> ActionCost {
    ActionCost { heat: 0, cooldown_max: 0, advances_turn: true }
}

/// Synthetic "move one cell left" action — **lane-relative**. Player
/// presses Left → ship advances one cell toward the aft end of the lane
/// (lower cell index) regardless of which way the bow is pointing.
///
/// Wired via `Effect::DISPLACE_SELF::direction = Some(LaneEnd::Aft)`, the
/// Rust-port extension added by architect in task #50 step 1. With
/// `direction: Some(...)`, `resolve_self_move` ignores `ship.orientation`
/// and uses the absolute lane direction; that gives the player a
/// predictable 2D-control scheme (Left always moves leftward on screen)
/// while leaving AI / scripted movement free to keep the original
/// orientation-relative behavior by passing `direction: None`.
pub fn synthetic_move_left() -> Action {
    Action {
        id: SYNTHETIC_MOVE_LEFT.into(),
        name: "Move Left".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(crate::types::LaneEnd::Aft),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic "move one cell right" action — **lane-relative**. Player
/// presses Right → ship advances one cell toward the fore end of the lane
/// (higher cell index) regardless of bow direction. See
/// [`synthetic_move_left`] for the design rationale.
pub fn synthetic_move_right() -> Action {
    Action {
        id: SYNTHETIC_MOVE_RIGHT.into(),
        name: "Move Right".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(crate::types::LaneEnd::Fore),
        }],
        r#mod: None,
        icon: None,
    }
}

pub fn synthetic_reorient_flip() -> Action {
    Action {
        id: SYNTHETIC_REORIENT_FLIP.into(),
        name: "Reorient".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::REORIENT { to: ReorientTo::Flip }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic vent. Dumps 3 heat and recharges cooldowns — matches the
/// catalog `vent` action shape from the analysis HTML's Defensive
/// archetype row.
pub fn synthetic_vent() -> Action {
    Action {
        id: SYNTHETIC_VENT.into(),
        name: "Vent".into(),
        archetype: WeaponArchetype::Defensive,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::VENT_HEAT {
            amount: 3,
            recharge_cooldowns: Some(true),
        }],
        r#mod: None,
        icon: None,
    }
}

/* =========================================================================
 * DemoContent — a minimal Content impl for the Phase 1 binary.
 *
 * Holds a HashMap<id, Action> and serves both the synthetic actions above
 * and a starter pulse_laser / torpedo so the demo's player has something
 * to queue with D1 / D2.
 * ====================================================================== */

/// A pre-built `Content` impl for the demo binary. Loaded with the four
/// synthetic actions and a starter pulse_laser / torpedo. Real catalog
/// content will replace this once the JSON export lands.
pub struct DemoContent {
    pub actions: HashMap<String, Action>,
}

impl DemoContent {
    /// Empty registry. Most callers want [`DemoContent::default`].
    pub fn empty() -> Self {
        Self { actions: HashMap::new() }
    }

    /// Insert or replace an action by id.
    pub fn insert(&mut self, action: Action) {
        self.actions.insert(action.id.clone(), action);
    }

    /// Register all four synthetic actions used by [`key_to_intent`].
    pub fn register_synthetics(&mut self) {
        self.insert(synthetic_move_left());
        self.insert(synthetic_move_right());
        self.insert(synthetic_reorient_flip());
        self.insert(synthetic_vent());
    }
}

impl Default for DemoContent {
    /// Pre-loaded with the four synthetics plus the demo board's two mount
    /// weapons (`pulse_laser`, `torpedo`). Matches the player setup in
    /// `bin/broadside.rs::render_example_board`.
    fn default() -> Self {
        let mut c = Self::empty();
        c.register_synthetics();

        // pulse_laser — close-range forward beam.
        c.insert(Action {
            id: "pulse_laser".into(),
            name: "Pulse Laser".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
                optimal_band: RangeBand::Close,
                requires_arc: Some(TArc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE { amount: 4, band_falloff: None }],
            r#mod: None,
            icon: None,
        });

        // torpedo — spawn an ordnance projectile.
        c.insert(Action {
            id: "torpedo".into(),
            name: "Torpedo".into(),
            archetype: WeaponArchetype::Ordnance,
            cost: ActionCost { heat: 2, cooldown_max: 2, advances_turn: true },
            targeting: Targeting {
                pattern: TargetingPattern::ORDNANCE,
                band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid, RangeBand::Long],
                optimal_band: RangeBand::Mid,
                requires_arc: Some(TArc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::SPAWN_ORDNANCE { projectile: "torpedo".into() }],
            r#mod: None,
            icon: None,
        });

        c
    }
}

impl Content for DemoContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }

    fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile {
        // Minimal hardcoded table: matches the analysis-doc descriptions
        // for `torpedo` (slow, high payload) and `missile_salvo` (fast,
        // light payload). Unknown kinds fall back to a 0-damage dummy so
        // the demo doesn't crash on a typo.
        match kind {
            "torpedo" => Projectile {
                id: format!("{}-torp-{}", owner.id, owner.cell),
                kind: "torpedo".into(),
                cell: owner.cell,
                heading: match owner.orientation {
                    crate::types::Orientation::BowOn { bow } => bow,
                    crate::types::Orientation::Broadside => crate::types::LaneEnd::Fore,
                },
                speed: 1,
                hull: 1,
                payload: vec![Effect::DAMAGE { amount: 4, band_falloff: Some(false) }],
                owner_faction: owner.faction,
            },
            "missile" => Projectile {
                id: format!("{}-msl-{}", owner.id, owner.cell),
                kind: "missile".into(),
                cell: owner.cell,
                heading: match owner.orientation {
                    crate::types::Orientation::BowOn { bow } => bow,
                    crate::types::Orientation::Broadside => crate::types::LaneEnd::Fore,
                },
                speed: 2,
                hull: 1,
                payload: vec![Effect::DAMAGE { amount: 2, band_falloff: Some(false) }],
                owner_faction: owner.faction,
            },
            _ => Projectile {
                id: format!("{}-unknown-{}", owner.id, owner.cell),
                kind: kind.into(),
                cell: owner.cell,
                heading: crate::types::LaneEnd::Fore,
                speed: 1,
                hull: 1,
                payload: vec![],
                owner_faction: owner.faction,
            },
        }
    }
}

/* =========================================================================
 * Tutorial overlay.
 * ====================================================================== */

/// One short line per binding for the top-of-screen tutorial overlay.
/// Renderer draws these stacked. Lines are intentionally terse — they need
/// to fit in a HUD strip, not a documentation page.
///
/// Adding a binding: add the key to [`Key`], the arm to [`key_to_intent`],
/// and one line here in the same commit so the three stay in sync.
pub fn tutorial_lines() -> &'static [&'static str] {
    &[
        "[1/2/3] queue mount",
        "[</>] move left/right",
        "[Tab] flip",
        "[V] vent",
        "[R/Space] commit turn",
        "[Enter] restart",
        "[Esc] quit",
    ]
}

/* =========================================================================
 * Tests.
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::types::{Arc as TArc, Faction, LaneEnd, Mount, Orientation};
    use std::collections::HashMap as Map;

    fn player_with_mounts(mount_count: usize) -> Ship {
        let mounts: Vec<Mount> = (0..mount_count)
            .map(|i| Mount {
                id: format!("m{i}"),
                arc: TArc::Forward,
                weapon: format!("weapon_{i}"),
            })
            .collect();
        Ship {
            id: "p".into(),
            faction: Faction::Player,
            cell: 0,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            hull: 10,
            max_hull: 10,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts,
            queue: Vec::new(),
            cooldowns: Map::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    #[test]
    fn key_to_intent_left_is_move_left() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::Left, &p, &c), Some(Intent::MoveLeft));
        assert_eq!(key_to_intent(Key::Right, &p, &c), Some(Intent::MoveRight));
    }

    #[test]
    fn key_to_intent_tab_is_reorient_flip() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::Tab, &p, &c), Some(Intent::ReorientFlip));
    }

    #[test]
    fn key_to_intent_v_is_vent() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::V, &p, &c), Some(Intent::Vent));
    }

    #[test]
    fn key_to_intent_digits_resolve_to_mount_weapons() {
        let p = player_with_mounts(3);
        let c = DemoContent::default();
        assert_eq!(
            key_to_intent(Key::D1, &p, &c),
            Some(Intent::QueueAction("weapon_0".into())),
        );
        assert_eq!(
            key_to_intent(Key::D2, &p, &c),
            Some(Intent::QueueAction("weapon_1".into())),
        );
        assert_eq!(
            key_to_intent(Key::D3, &p, &c),
            Some(Intent::QueueAction("weapon_2".into())),
        );
    }

    #[test]
    fn key_to_intent_out_of_range_digits_return_none() {
        let p = player_with_mounts(1);
        let c = DemoContent::default();
        assert_eq!(
            key_to_intent(Key::D1, &p, &c),
            Some(Intent::QueueAction("weapon_0".into())),
        );
        assert_eq!(key_to_intent(Key::D2, &p, &c), None,
            "ship has 1 mount; D2 is out of range");
        assert_eq!(key_to_intent(Key::D3, &p, &c), None);
    }

    #[test]
    fn key_to_intent_commit_aliases() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::R, &p, &c), Some(Intent::CommitTurn));
        assert_eq!(key_to_intent(Key::Space, &p, &c), Some(Intent::CommitTurn));
    }

    #[test]
    fn key_to_intent_enter_is_restart() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::Enter, &p, &c), Some(Intent::Restart));
    }

    #[test]
    fn intent_to_action_id_synthetics_use_double_underscore() {
        assert_eq!(intent_to_action_id(&Intent::MoveLeft), Some(SYNTHETIC_MOVE_LEFT));
        assert_eq!(intent_to_action_id(&Intent::MoveRight), Some(SYNTHETIC_MOVE_RIGHT));
        assert_eq!(intent_to_action_id(&Intent::ReorientFlip), Some(SYNTHETIC_REORIENT_FLIP));
        assert_eq!(intent_to_action_id(&Intent::Vent), Some(SYNTHETIC_VENT));
        // Synthetic ids must use the `__` prefix so they cannot collide
        // with real catalog action ids (which are unprefixed snake_case).
        for id in [
            SYNTHETIC_MOVE_LEFT, SYNTHETIC_MOVE_RIGHT,
            SYNTHETIC_REORIENT_FLIP, SYNTHETIC_VENT,
        ] {
            assert!(id.starts_with("__"),
                "synthetic id `{id}` must start with __ to avoid catalog collisions");
        }
    }

    #[test]
    fn intent_to_action_id_queue_action_passes_through() {
        assert_eq!(
            intent_to_action_id(&Intent::QueueAction("pulse_laser".into())),
            Some("pulse_laser"),
        );
    }

    #[test]
    fn intent_to_action_id_control_flow_is_none() {
        assert_eq!(intent_to_action_id(&Intent::CommitTurn), None);
        assert_eq!(intent_to_action_id(&Intent::Restart), None);
    }

    #[test]
    fn demo_content_serves_every_synthetic() {
        let c = DemoContent::default();
        assert!(c.action(SYNTHETIC_MOVE_LEFT).is_some());
        assert!(c.action(SYNTHETIC_MOVE_RIGHT).is_some());
        assert!(c.action(SYNTHETIC_REORIENT_FLIP).is_some());
        assert!(c.action(SYNTHETIC_VENT).is_some());
    }

    #[test]
    fn demo_content_serves_demo_mount_weapons() {
        let c = DemoContent::default();
        assert!(c.action("pulse_laser").is_some());
        assert!(c.action("torpedo").is_some());
    }

    #[test]
    fn synthetic_actions_are_free_and_uncooldowned() {
        // Synthetics must not eat heat or impose a cooldown — they are
        // player-input ergonomics, not catalog weapons. A non-zero cost
        // would mean the player runs out of "move" charges, which is not
        // the Phase 1 design.
        for a in [
            synthetic_move_left(),
            synthetic_move_right(),
            synthetic_reorient_flip(),
            synthetic_vent(),
        ] {
            assert_eq!(a.cost.heat, 0, "{} must be free-heat", a.id);
            assert_eq!(a.cost.cooldown_max, 0, "{} must be uncooldowned", a.id);
        }
    }

    #[test]
    fn tutorial_lines_cover_every_binding() {
        // Sanity: one tutorial line per key variant (minus exit, which is
        // wired by the bin itself before key_to_intent gets a chance).
        let lines = tutorial_lines();
        assert!(lines.iter().any(|l| l.contains("1/2/3")));
        assert!(lines.iter().any(|l| l.contains("Tab")));
        assert!(lines.iter().any(|l| l.contains("V")));
        assert!(lines.iter().any(|l| l.contains("R/Space")));
        assert!(lines.iter().any(|l| l.contains("Enter")));
        assert!(lines.iter().any(|l| l.contains("Esc")));
    }

    /// End-to-end sanity: queue a synthetic, run resolve_round, see the
    /// effect land. Demonstrates the queue->execute_queue path works for
    /// player-input synthetics without any pipeline bypass.
    #[test]
    fn synthetic_vent_flows_through_execute_queue() {
        use crate::resolve::execute_queue;
        use crate::types::{Board, EventBus};

        let mut player = player_with_mounts(1);
        player.heat = 4;
        player.locked_out = true;
        player.queue = vec![SYNTHETIC_VENT.into()];

        let mut board = Board {
            size: 5,
            cells: vec![Some(player), None, None, None, None],
            ordnance: vec![],
            hazards: (0..5).map(|_| vec![]).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        };

        let content = DemoContent::default();
        execute_queue(0, &mut board, &content);

        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 1, "synthetic vent should dump 3 heat (4 -> 1)");
        assert!(!p.locked_out, "vent should clear lockout");
        assert!(p.queue.is_empty(), "queue should be drained after execute");
    }
}
