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
    /// Digit 5 — play first field-kit Card (task #63).
    D5,
    /// Digit 6 — play second field-kit Card.
    D6,
    /// Digit 7 — play third field-kit Card.
    D7,
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
    /// Play a field-kit Card by id (task #63). Caller validates +
    /// decrements charges via `Content::try_play_card`, then pushes the
    /// synthetic `__card_<id>` action onto the queue. The action's only
    /// effect is `Effect::BOARD { note: <card_id> }` which dispatches
    /// through `Content::apply_board_effect`.
    PlayCard(String),
    /// Resolve the current round (call `resolve_round`).
    CommitTurn,
    /// Restart the scene (rebuild Board from scratch).
    Restart,
}

/* =========================================================================
 * The mapping.
 * ====================================================================== */

/// Canonical key bindings for the Phase 1+2 demo (tasks #43, #63).
///
/// | Key            | Intent                                       |
/// |----------------|----------------------------------------------|
/// | `Left`         | [`Intent::MoveLeft`]                         |
/// | `Right`        | [`Intent::MoveRight`]                        |
/// | `Tab`          | [`Intent::ReorientFlip`]                     |
/// | `V`            | [`Intent::Vent`]                             |
/// | `D1` / `D2` / `D3` | [`Intent::QueueAction`] of `ship.mounts[N].weapon`, **only if** `N < mounts.len()`. `None` otherwise. |
/// | `D5` / `D6` / `D7` | [`Intent::PlayCard`] of the Nth card id in the ship's [`crate::cards::FieldKit`], **only if** that slot exists in `content`. `None` otherwise. |
/// | `R`, `Space`   | [`Intent::CommitTurn`]                       |
/// | `Enter`        | [`Intent::Restart`]                          |
///
/// Returns `None` for an unbound key OR for a digit key past the ship's
/// mount / card count. `content` is queried for the ship's card inventory
/// (the runtime FieldKit lives on Content until architect lands
/// `Ship::field_kit`); ship is still consulted for mounts.
pub fn key_to_intent(key: Key, ship: &Ship, content: &dyn Content) -> Option<Intent> {
    match key {
        Key::Left => Some(Intent::MoveLeft),
        Key::Right => Some(Intent::MoveRight),
        Key::Tab => Some(Intent::ReorientFlip),
        Key::V => Some(Intent::Vent),
        Key::D1 => mount_action(ship, 0).map(Intent::QueueAction),
        Key::D2 => mount_action(ship, 1).map(Intent::QueueAction),
        Key::D3 => mount_action(ship, 2).map(Intent::QueueAction),
        Key::D5 => content.card_at(&ship.id, 0).map(Intent::PlayCard),
        Key::D6 => content.card_at(&ship.id, 1).map(Intent::PlayCard),
        Key::D7 => content.card_at(&ship.id, 2).map(Intent::PlayCard),
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
/// directly. Also returns `None` for [`Intent::PlayCard`] because card
/// plays need a separate validation + charge-decrement step the caller
/// performs via [`Content::try_play_card`]; on success the caller then
/// pushes [`synthetic_card_action_id`] manually.
pub fn intent_to_action_id(intent: &Intent) -> Option<&str> {
    match intent {
        Intent::QueueAction(id) => Some(id.as_str()),
        Intent::MoveLeft => Some(SYNTHETIC_MOVE_LEFT),
        Intent::MoveRight => Some(SYNTHETIC_MOVE_RIGHT),
        Intent::ReorientFlip => Some(SYNTHETIC_REORIENT_FLIP),
        Intent::Vent => Some(SYNTHETIC_VENT),
        // PlayCard: caller validates + decrements via Content::try_play_card
        // first, then pushes synthetic_card_action_id(card_id) manually.
        Intent::PlayCard(_) | Intent::CommitTurn | Intent::Restart => None,
    }
}

/// The synthetic-action id assigned to a card play. Conventionally
/// `"__card_<card_id>"` — the `__` prefix prevents collision with real
/// catalog actions, and the card id keeps the id readable in logs.
///
/// Callers: after [`Content::try_play_card`] returns
/// [`crate::cards::PlayResult::Played`], push the result of this function
/// onto the ship's queue. `execute_queue` will then look up the registered
/// `Action { id: __card_<id>, effects: [BOARD { note: <id> }] }` and the
/// BOARD arm dispatches via `Content::apply_board_effect`.
pub fn synthetic_card_action_id(card_id: &str) -> String {
    format!("__card_{card_id}")
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
///
/// Carries a [`crate::subsystems::Installations`] registry so subsystems
/// installed on demo ships drive `Content::damage_modifier` /
/// `Content::on_turn_end`. See the `subsystems` module docstring for
/// why the registry lives here rather than on `Board`.
///
/// Also carries a [`crate::cards::FieldKitRegistry`] + [`crate::cards::CardCatalog`]
/// so per-ship card inventories live with the content layer until
/// architect lands `Ship::field_kit` (task #63 follow-up).
pub struct DemoContent {
    pub actions: HashMap<String, Action>,
    pub installations: crate::subsystems::Installations,
    pub card_catalog: crate::cards::CardCatalog,
    pub field_kits: crate::cards::FieldKitRegistry,
}

impl DemoContent {
    /// Empty registry. Most callers want [`DemoContent::default`].
    pub fn empty() -> Self {
        Self {
            actions: HashMap::new(),
            installations: crate::subsystems::Installations::new(),
            card_catalog: crate::cards::CardCatalog::new(),
            field_kits: crate::cards::FieldKitRegistry::new(),
        }
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

    /// Register the synthetic action shells that field-kit Cards
    /// dispatch through. One synthetic action per card id:
    /// `__card_<id>` whose only effect is `Effect::BOARD { note: <id> }`.
    /// `execute_queue` looks them up by id like any other action; the
    /// BOARD arm then routes through `Content::apply_board_effect`.
    pub fn register_card_synthetics(&mut self) {
        for id in crate::cards::PLACEHOLDER_CARD_IDS {
            self.insert(card_synthetic_action(id));
        }
    }

    /// Register the three placeholder class Signature actions
    /// (Overcharge, Phase Drift, Broadside Volley) so the resolver can
    /// dispatch them when the `Ship::klass` lookup finds a matching
    /// [`crate::types::ClassDef`]. Task #62 step 2; input wiring for
    /// "press a key to fire the Signature" is deferred — these defs
    /// just exist in the action registry, ready for that later step.
    pub fn register_class_signatures(&mut self) {
        // The canonical roster's five self-move signatures (#50/#97) ...
        self.insert(crate::classes::synthetic_slip());
        self.insert(crate::classes::synthetic_ram());
        self.insert(crate::classes::synthetic_phase());
        self.insert(crate::classes::synthetic_throw());
        self.insert(crate::classes::synthetic_swap_toss());
        // ... plus Aegis's broadside-native signature (#50).
        self.insert(crate::classes::synthetic_broadside_sweep());
    }

    /// Install a subsystem on a ship by id. Phase 2 convenience for the
    /// demo board's startup; the catalog flow will replace this once
    /// `SubsystemDef` is wired through.
    pub fn install_subsystem(
        &mut self,
        ship_id: impl Into<String>,
        subsystem_id: impl Into<crate::subsystems::SubsystemId>,
    ) {
        self.installations.install(ship_id, subsystem_id);
    }

    /// Grant a card to a ship's field-kit. Phase 2 convenience; same
    /// migration path as subsystems.
    pub fn grant_card(
        &mut self,
        ship_id: impl Into<String>,
        card_id: impl Into<String>,
        charges: u32,
    ) {
        self.field_kits.grant(ship_id, card_id, charges);
    }

    /// Grant 1 charge of every placeholder card to `ship_id`.
    pub fn grant_placeholder_kit(&mut self, ship_id: &str) {
        crate::cards::grant_placeholder_kit(&mut self.field_kits, ship_id);
    }
}

/// Build the synthetic `Action` shell that delivers a card's BOARD
/// effect through `execute_queue`. The action has zero cost (cards are
/// tempo-free), SELF targeting (no arc / band gate), and a single
/// `Effect::BOARD { note: <card_id> }` payload.
fn card_synthetic_action(card_id: &str) -> Action {
    Action {
        id: synthetic_card_action_id(card_id),
        name: format!("Play {card_id}"),
        archetype: WeaponArchetype::Defensive,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::BOARD { note: card_id.into() }],
        r#mod: None,
        icon: None,
    }
}

impl Default for DemoContent {
    /// Pre-loaded with the four player-input synthetics, the three card
    /// synthetics (task #63), the three class Signatures (task #62),
    /// the placeholder card catalog, plus the demo board's two mount
    /// weapons (`pulse_laser`, `torpedo`). Matches the player setup in
    /// `bin/broadside.rs::render_example_board`.
    fn default() -> Self {
        let mut c = Self::empty();
        c.register_synthetics();
        c.register_card_synthetics();
        c.register_class_signatures();
        c.card_catalog = crate::cards::placeholder_catalog();

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

    fn damage_modifier(
        &self,
        attacker: &Ship,
        band: crate::types::RangeBand,
        board: &crate::types::Board,
    ) -> i32 {
        // Audit #67: subsystem damage bonuses fire from the attacker's
        // installed fittings, not the target's.
        let installed = self.installations.for_ship(&attacker.id);
        crate::subsystems::damage_modifier_for(installed, attacker, band, board)
    }

    fn on_turn_end(&self, board: &mut crate::types::Board) {
        crate::subsystems::on_turn_end_for(&self.installations, board);
    }

    fn apply_board_effect(&self, note: &str, source_cell: usize, board: &mut crate::types::Board) {
        crate::cards::apply_card_effect(note, source_cell, board);
    }

    fn card_at(&self, ship_id: &str, idx: usize) -> Option<String> {
        let kit = self.field_kits.for_ship(ship_id)?;
        let entry = kit.cards.get(idx)?;
        if entry.charges == 0 {
            return None;
        }
        Some(entry.card_id.clone())
    }

    fn try_play_card(&mut self, ship_id: &str, card_id: &str) -> crate::cards::PlayResult {
        crate::cards::try_play_card(&mut self.field_kits, &self.card_catalog, ship_id, card_id)
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
        "every input advances time",
        "[</>] move (instant)",
        "[Tab] flip (instant)",
        "[V] vent (instant)",
        "[1/2/3] queue mount",
        "[5/6/7] play card (instant)",
        "[R/Space] release queue",
        "[ [ ] ] rotate camera",
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
        assert!(lines.iter().any(|l| l.contains("5/6/7")), "card binding must be advertised");
        assert!(lines.iter().any(|l| l.contains("Tab")));
        assert!(lines.iter().any(|l| l.contains("V")));
        assert!(lines.iter().any(|l| l.contains("R/Space")));
        assert!(lines.iter().any(|l| l.contains("Enter")));
        assert!(lines.iter().any(|l| l.contains("Esc")));
    }

    /// Task #62 — DemoContent::default registers every placeholder class
    /// Signature action, so a future "press S to fire signature" intent
    /// can `content.action(class.signature)` and get back a real Action.
    #[test]
    fn demo_content_registers_every_placeholder_signature() {
        let c = DemoContent::default();
        for class in crate::classes::placeholder_classes() {
            assert!(
                c.action(&class.signature).is_some(),
                "DemoContent must serve ClassDef::signature `{}` for class `{}`",
                class.signature,
                class.id,
            );
        }
    }

    /// End-to-end sanity: queue a synthetic, run resolve_round, see the
    /// effect land. Demonstrates the queue->execute_queue path works for
    /// player-input synthetics without any pipeline bypass.
    #[test]
    fn synthetic_vent_flows_through_execute_queue() {
        use crate::resolve::fire_player_queue;
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
        fire_player_queue("p", &mut board, &content);

        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.heat, 1, "synthetic vent should dump 3 heat (4 -> 1)");
        assert!(!p.locked_out, "vent should clear lockout");
        assert!(p.queue.is_empty(), "queue should be drained after execute");
    }

    /* ---- Phase 2 subsystem integration ------------------------------- */

    /// End-to-end: Marksman installed on the **attacker** adds +1 to a
    /// Long-range hit, routed through the canonical damage pipeline via
    /// `Content::damage_modifier`. Pin against a future regression that
    /// drops the registry lookup OR re-inverts the audit #67 fix.
    #[test]
    fn marksman_subsystem_adds_one_through_apply_damage() {
        use crate::resolve::apply_damage;
        use crate::subsystems::MARKSMAN;
        use crate::types::{Board, EventBus, ShieldFace, ShieldProfile};

        // Attacker "p" at cell 0, target at cell 5 (Long range). Target
        // has armour 0 on every face so the modifier change shows up in
        // hull cleanly.
        let attacker = player_with_mounts(0); // id "p"
        let mut target = player_with_mounts(0);
        target.id = "target".into();
        target.cell = 5;
        target.shield_profile = ShieldProfile {
            bow: ShieldFace { armour: 0, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 0, charge: 0 },
            starboard: ShieldFace { armour: 0, charge: 0 },
        };

        let mut board = Board {
            size: 7,
            cells: vec![
                Some(attacker), None, None, None, None, Some(target), None,
            ],
            ordnance: vec![],
            hazards: (0..7).map(|_| vec![]).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        };

        // Weapon: 4 raw, bandFalloff: false so the modifier delta is
        // unambiguous. Distance 5 -> Long.
        let mut weapon = synthetic_vent(); // grab a free-cost shell
        weapon.id = "test_weapon".into();
        weapon.effects = vec![Effect::DAMAGE { amount: 4, band_falloff: Some(false) }];

        // Baseline: no Marksman installed. Hull should drop by 4.
        let mut content = DemoContent::default();
        apply_damage(5, 4, 0, &weapon, &mut board, &content);
        assert_eq!(board.cells[5].as_ref().unwrap().hull, 6, "no marksman: 4 lands");

        // Reset hull, install Marksman on the ATTACKER, fire again. Hull
        // should drop by 5. Per audit #67, Marksman is attacker-side: the
        // bonus comes from the firing ship's fittings, not the target's.
        board.cells[5].as_mut().unwrap().hull = 10;
        content.install_subsystem("p", MARKSMAN);
        apply_damage(5, 4, 0, &weapon, &mut board, &content);
        assert_eq!(
            board.cells[5].as_ref().unwrap().hull, 5,
            "marksman on ATTACKER: 4 base + 1 attacker-side mod at Long = 5 lands"
        );

        // Negative case: Marksman on the TARGET should NOT add. Reset
        // hull, drop the attacker's Marksman, install one on the target
        // instead. Hull should drop by exactly 4 again.
        board.cells[5].as_mut().unwrap().hull = 10;
        content = DemoContent::default();
        content.install_subsystem("target", MARKSMAN);
        apply_damage(5, 4, 0, &weapon, &mut board, &content);
        assert_eq!(
            board.cells[5].as_ref().unwrap().hull, 6,
            "marksman on TARGET must NOT apply (audit #67 direction pin)",
        );
    }

    /// End-to-end: HeatSink installed on the owning ship subtracts one
    /// extra heat at end-of-turn, stacking with the canonical -1.
    #[test]
    fn heatsink_subsystem_doubles_dissipation_per_turn() {
        use crate::resolve::end_of_turn;
        use crate::subsystems::HEAT_SINK;
        use crate::types::{Board, EventBus};

        let mut player = player_with_mounts(0);
        player.heat = 5;
        let mut board = Board {
            size: 3,
            cells: vec![Some(player), None, None],
            ordnance: vec![],
            hazards: (0..3).map(|_| vec![]).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        };

        let mut content = DemoContent::default();
        content.install_subsystem("p", HEAT_SINK);

        end_of_turn(&mut board, &content);
        // Base -1 (passive) + HeatSink -1 = -2 total. 5 -> 3.
        assert_eq!(board.cells[0].as_ref().unwrap().heat, 3,
            "HeatSink stacks with passive dissipation");
    }

    /* ---- Phase 2 field-kit Card integration --------------------------- */

    /// End-to-end: try_play_card decrements charges, push the synthetic
    /// id, run execute_queue, see the board-wide effect land.
    #[test]
    fn mass_lock_card_play_through_execute_queue() {
        use crate::cards::{CARD_MASS_LOCK, PlayResult};
        use crate::resolve::fire_player_queue;
        use crate::types::{Board, EventBus, Faction, StatusKind};

        let player = player_with_mounts(0);
        // Two enemies; mass_lock applies TargetLock to both.
        let mut enemy_a = player_with_mounts(0);
        enemy_a.id = "ea".into();
        enemy_a.faction = Faction::Enemy;
        enemy_a.cell = 1;
        let mut enemy_b = player_with_mounts(0);
        enemy_b.id = "eb".into();
        enemy_b.faction = Faction::Enemy;
        enemy_b.cell = 3;

        let mut board = Board {
            size: 5,
            cells: vec![Some(player), Some(enemy_a), None, Some(enemy_b), None],
            ordnance: vec![],
            hazards: (0..5).map(|_| vec![]).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        };

        let mut content = DemoContent::default();
        content.grant_placeholder_kit("p");

        // Step 1: validate + decrement charges.
        assert_eq!(content.try_play_card("p", CARD_MASS_LOCK), PlayResult::Played);
        assert_eq!(
            content.field_kits.for_ship("p").unwrap().find(CARD_MASS_LOCK).unwrap().charges,
            0,
            "charge decremented from 1 to 0",
        );

        // Step 2: queue the synthetic and run execute_queue.
        let synth_id = synthetic_card_action_id(CARD_MASS_LOCK);
        if let Some(p) = board.cells[0].as_mut() {
            p.queue.push(synth_id);
        }
        fire_player_queue("p", &mut board, &content);

        // Step 3: both enemies should be target-locked, player should not.
        for cell in [1, 3] {
            let s = board.cells[cell].as_ref().unwrap();
            assert!(
                s.statuses.iter().any(|st| st.kind == StatusKind::TargetLock),
                "enemy at cell {cell} should be target-locked",
            );
        }
        let p = board.cells[0].as_ref().unwrap();
        assert!(p.statuses.iter().all(|st| st.kind != StatusKind::TargetLock));
    }

    /// Replaying a depleted card returns InsufficientCharges; the
    /// synthetic should NOT be queued in that case. (This test
    /// documents the caller contract; the bin enforces it.)
    #[test]
    fn second_play_of_one_charge_card_rejected() {
        use crate::cards::{CARD_MASS_BREACH, PlayResult};
        let mut content = DemoContent::default();
        content.grant_placeholder_kit("p");
        assert_eq!(content.try_play_card("p", CARD_MASS_BREACH), PlayResult::Played);
        assert_eq!(
            content.try_play_card("p", CARD_MASS_BREACH),
            PlayResult::InsufficientCharges,
        );
    }

    /// key_to_intent's D5 returns the first card id from the ship's kit.
    #[test]
    fn key_d5_returns_first_card_intent() {
        use crate::cards::CARD_MASS_LOCK;
        let p = player_with_mounts(0);
        let mut content = DemoContent::default();
        content.grant_placeholder_kit("p");
        let intent = key_to_intent(Key::D5, &p, &content);
        // Placeholder kit grants cards in order: mass_lock, mass_breach,
        // sensor_pulse. D5 -> slot 0 -> mass_lock.
        assert_eq!(intent, Some(Intent::PlayCard(CARD_MASS_LOCK.into())));
    }

    /// key_to_intent's D5 returns None when the ship has no kit at all.
    #[test]
    fn key_d5_returns_none_without_kit() {
        let p = player_with_mounts(0);
        let content = DemoContent::default(); // no kit granted
        assert_eq!(key_to_intent(Key::D5, &p, &content), None);
    }

    /// PlayCard intent does NOT route through intent_to_action_id
    /// (which returns None) — callers must invoke try_play_card +
    /// synthetic_card_action_id separately.
    #[test]
    fn intent_to_action_id_returns_none_for_play_card() {
        let i = Intent::PlayCard("mass_lock".into());
        assert_eq!(intent_to_action_id(&i), None);
    }

    /// synthetic_card_action_id uses the `__card_` prefix.
    #[test]
    fn synthetic_card_action_id_format() {
        assert_eq!(synthetic_card_action_id("mass_lock"), "__card_mass_lock");
        assert!(synthetic_card_action_id("anything").starts_with("__"));
    }
}
