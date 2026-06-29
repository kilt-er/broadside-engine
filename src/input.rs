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
    Action, ActionCost, Arc as TArc, Effect, MovementMode, Projectile, RangeBand, ReorientTo, Ship,
    Targeting, TargetingPattern, WeaponArchetype,
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
    /// Up arrow. Move one cell N (toward row 0 / the far enemies) — #18.
    Up,
    /// Down arrow. Move one cell S (toward the player's front row) — #18.
    Down,
    Tab,
    /// Letter Q. Rotate the ship a quarter-turn LEFT (counter-clockwise) — #75.
    Q,
    /// Letter E. Rotate the ship a quarter-turn RIGHT (clockwise) — #75.
    E,
    /// Letter V. Vent heat.
    V,
    /// Letter W. WAIT — pass the turn: hold position + facing and let the world
    /// advance one turn (#126, turn-based model). Bruce moves with the ARROW
    /// keys, so W is free; mnemonic "Wait", sits by the QWE cluster.
    W,
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
    /// Push the synthetic `__move_left` action — `DISPLACE_SELF` in the
    /// `Aft` lane direction by 1 cell via THRUST.
    MoveLeft,
    /// Push the synthetic `__move_right` action — `DISPLACE_SELF` in the
    /// `Fore` lane direction by 1 cell via THRUST.
    MoveRight,
    /// v2 (#18): push the synthetic `__move_up` action — `DISPLACE_SELF` one cell
    /// N (toward row 0 / the far enemies) via THRUST. Functional once resolver
    /// R6's `resolve_self_move` reads `direction_2d`.
    MoveUp,
    /// v2 (#18): push the synthetic `__move_down` action — `DISPLACE_SELF` one
    /// cell S (toward the player's front row) via THRUST.
    MoveDown,
    /// Push the synthetic `__reorient_flip` action — `REORIENT::Flip`.
    ReorientFlip,
    /// v2 (#75): push the synthetic `__rotate_left` action — `REORIENT::RotateLeft`,
    /// turning the player's FACING a quarter-turn counter-clockwise (N→W→S→E). The
    /// hull rotates on screen + the firing arcs follow (both key off `facing`).
    RotateLeft,
    /// v2 (#75): push the synthetic `__rotate_right` action — `REORIENT::RotateRight`,
    /// a quarter-turn clockwise (N→E→S→W).
    RotateRight,
    /// Push the synthetic `__vent` action — `VENT_HEAT` 3, recharge cooldowns.
    Vent,
    /// Play a field-kit Card by id (task #63). Caller validates +
    /// decrements charges via `Content::try_play_card`, then pushes the
    /// synthetic `__card_<id>` action onto the queue. The action's only
    /// effect is `Effect::BOARD { note: <card_id> }` which dispatches
    /// through `Content::apply_board_effect`.
    PlayCard(String),
    /// Resolve the current round (call `resolve_round`).
    CommitTurn,
    /// WAIT — pass the turn (#126, turn-based model): the player takes no
    /// action of their own; the bin just advances the world one turn
    /// (`run_world_phase`). Control-flow like [`CommitTurn`] (not queued):
    /// [`intent_to_action_id`] returns `None` for it; the bin handles it.
    Wait,
    /// Restart the scene (rebuild Board from scratch).
    Restart,
}

/* =========================================================================
 * The mapping.
 * ====================================================================== */

/// Canonical key bindings for the Phase 1+2 demo (tasks #43, #63).
///
/// (#165 Bruce) TANK CONTROLS — movement is bow-relative, NOT absolute strafe:
///
/// | Key            | Intent                                                   |
/// |----------------|----------------------------------------------------------|
/// | `Up`           | move FORWARD along the bow (absolute move = `ship`'s bow Dir4) |
/// | `Down`         | move REVERSE (opposite the bow)                          |
/// | `Left`         | [`Intent::RotateLeft`] (rotate bow CCW — same as `Q`)    |
/// | `Right`        | [`Intent::RotateRight`] (rotate bow CW — same as `E`)    |
/// | `Q` / `E`      | [`Intent::RotateLeft`] / [`Intent::RotateRight`]         |
/// | `Tab`          | [`Intent::ReorientFlip`]                                 |
/// | `V`            | [`Intent::Vent`]                                         |
/// | `W`            | [`Intent::Wait`] (pass the turn)                         |
/// | `D1` / `D2` / `D3` | [`Intent::QueueAction`] of `ship.mounts[N].weapon`, **only if** `N < mounts.len()`. `None` otherwise. |
/// | `D5` / `D6` / `D7` | [`Intent::PlayCard`] of the Nth card id in the ship's [`crate::cards::FieldKit`], **only if** that slot exists in `content`. `None` otherwise. |
/// | `R`, `Space`   | [`Intent::CommitTurn`]                                   |
/// | `Enter`        | [`Intent::Restart`]                                      |
///
/// There is NO lateral strafe: to change column you ROTATE then move FORWARD
/// (Bruce's tank-controls ruling). Forward/reverse resolve to one of the absolute
/// `Move{Up,Down,Left,Right}` intents picked from the ship's current bow facing, so
/// the resolver path (`SYNTHETIC_MOVE_*` → `resolve_self_move_2d`) is unchanged — only
/// WHICH cardinal a forward/reverse press maps to depends on facing.
///
/// Returns `None` for an unbound key OR for a digit key past the ship's
/// mount / card count. `content` is queried for the ship's card inventory
/// (the runtime `FieldKit` lives on Content until architect lands
/// `Ship::field_kit`); ship is still consulted for mounts.
pub fn key_to_intent(key: Key, ship: &Ship, content: &dyn Content) -> Option<Intent> {
    match key {
        // (#165) Tank controls: Up = forward along the bow, Down = reverse.
        Key::Up => Some(move_intent_for_dir4(forward_dir4(ship.facing))),
        Key::Down => Some(move_intent_for_dir4(forward_dir4(ship.facing).opposite())),
        // Left/Right now ROTATE (no strafe); same as Q/E.
        Key::Left | Key::Q => Some(Intent::RotateLeft),
        Key::Right | Key::E => Some(Intent::RotateRight),
        Key::Tab => Some(Intent::ReorientFlip),
        Key::V => Some(Intent::Vent),
        Key::W => Some(Intent::Wait),
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

/// (#165 tank controls) The Dir4 a ship moves when it goes FORWARD — its bow
/// direction. A `Bow(dir)` faces `dir`; a `Broadside(axis)` hull has no single bow,
/// so forward defaults to the axis's canonical positive cardinal (N for NS, E for EW)
/// — a safe total fallback (the player only ever holds a cardinal `Bow` facing via
/// the rotate controls; Broadside is the enemy flank stance). Reverse is `.opposite()`.
///
/// `pub(crate)` so the enemy AI (#166) reuses the SAME forward semantics the
/// player's tank controls do — one definition of "which way is forward" shared by
/// the render/input half and the AI half (no second copy to drift).
pub(crate) const fn forward_dir4(facing: crate::grid::Facing) -> crate::grid::Dir4 {
    use crate::grid::{Axis, Dir4, Facing};
    match facing {
        Facing::Bow(dir) => dir,
        Facing::Broadside(Axis::NorthSouth) => Dir4::N,
        Facing::Broadside(Axis::EastWest) => Dir4::E,
    }
}

/// (#165) Map a movement [`crate::grid::Dir4`] to the absolute-move [`Intent`] that
/// steps that way — so a facing-relative forward/reverse press reuses the existing
/// `SYNTHETIC_MOVE_*` → `resolve_self_move_2d` path (only the CHOSEN cardinal varies).
const fn move_intent_for_dir4(dir: crate::grid::Dir4) -> Intent {
    use crate::grid::Dir4;
    match dir {
        Dir4::N => Intent::MoveUp,
        Dir4::E => Intent::MoveRight,
        Dir4::S => Intent::MoveDown,
        Dir4::W => Intent::MoveLeft,
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
/// [`Intent::Wait`], [`Intent::Restart`]) — those are not queued; the caller
/// handles them directly. Also returns `None` for [`Intent::PlayCard`] because card
/// plays need a separate validation + charge-decrement step the caller
/// performs via [`Content::try_play_card`]; on success the caller then
/// pushes [`synthetic_card_action_id`] manually.
pub const fn intent_to_action_id(intent: &Intent) -> Option<&str> {
    match intent {
        Intent::QueueAction(id) => Some(id.as_str()),
        Intent::MoveLeft => Some(SYNTHETIC_MOVE_LEFT),
        Intent::MoveRight => Some(SYNTHETIC_MOVE_RIGHT),
        Intent::MoveUp => Some(SYNTHETIC_MOVE_UP),
        Intent::MoveDown => Some(SYNTHETIC_MOVE_DOWN),
        Intent::ReorientFlip => Some(SYNTHETIC_REORIENT_FLIP),
        Intent::RotateLeft => Some(SYNTHETIC_ROTATE_LEFT),
        Intent::RotateRight => Some(SYNTHETIC_ROTATE_RIGHT),
        Intent::Vent => Some(SYNTHETIC_VENT),
        // PlayCard: caller validates + decrements via Content::try_play_card
        // first, then pushes synthetic_card_action_id(card_id) manually.
        Intent::PlayCard(_) | Intent::CommitTurn | Intent::Wait | Intent::Restart => None,
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
/// v2 (C1 infra + #18): move toward row 0 (away from the player / toward the
/// far enemies). N on the grid (`Dir4::N`, decreasing row). Resolver-served via
/// `resolver_ai_move` for AI, and the player's depth-up key.
pub const SYNTHETIC_MOVE_UP: &str = "__move_up";
/// v2 (C1 infra + #18): move toward the player's front row (increasing row).
/// S on the grid (`Dir4::S`). The AI's primary CLOSE direction.
pub const SYNTHETIC_MOVE_DOWN: &str = "__move_down";
pub const SYNTHETIC_REORIENT_FLIP: &str = "__reorient_flip";
/// v2 (#75): rotate the player's FACING a quarter-turn counter-clockwise via
/// `REORIENT::RotateLeft`. Registered on `DemoContent` (like reorient/vent) so the
/// queued action resolves through the normal `execute_queue` pipeline.
pub const SYNTHETIC_ROTATE_LEFT: &str = "__rotate_left";
/// v2 (#75): rotate the player's FACING a quarter-turn clockwise.
pub const SYNTHETIC_ROTATE_RIGHT: &str = "__rotate_right";
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

/// v2 (A3 EXPAND): 2-D all-band coverage, the [`crate::grid::Range`] mirror of
/// [`all_bands`]. Used to fill the additive `Targeting::range_band` on the demo
/// actions during the migration.
fn all_ranges() -> Vec<crate::grid::Range> {
    use crate::grid::Range;
    vec![Range::Adjacent, Range::Near, Range::Far]
}

fn self_targeting() -> Targeting {
    Targeting {
        pattern: TargetingPattern::SELF,
        band: all_bands(),
        optimal_band: RangeBand::PointBlank,
        // v2 (A3 EXPAND): 2-D range mirror of the 1-D bands above.
        range_band: all_ranges(),
        optimal_range: crate::grid::Range::Adjacent,
        requires_arc: None,
        facing_relative: false,
        hits_all: false,
    }
}

const fn zero_cost() -> ActionCost {
    ActionCost {
        heat: 0,
        cooldown_max: 0,
        advances_turn: true,
    }
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
            // v2 (#18): move-left = decreasing column = Dir4::W. The 1-D
            // `direction` stays as the transition fallback; `direction_2d` is
            // the real 2-D direction resolver R6's resolve_self_move reads.
            direction_2d: Some(crate::grid::Dir4::W),
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
            // v2 (#18): move-right = increasing column = Dir4::E.
            direction_2d: Some(crate::grid::Dir4::E),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic "move one cell up" action (v2, #18 / C1 infra) — toward row 0
/// (away from the player, toward the far enemies). `direction_2d: Some(Dir4::N)`
/// is the real 2-D direction; the 1-D `direction` is a transition fallback (the
/// lane had no depth axis, so Fore is an arbitrary stand-in there). Functional
/// once resolver R6's `resolve_self_move` reads `direction_2d`; the AI's CLOSE/
/// back-off (`__move_up` for back-off) and the player's depth-up key both use it.
pub fn synthetic_move_up() -> Action {
    Action {
        id: SYNTHETIC_MOVE_UP.into(),
        name: "Move Up".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(crate::types::LaneEnd::Fore),
            direction_2d: Some(crate::grid::Dir4::N),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic "move one cell down" action (v2, #18 / C1 infra) — toward the
/// player's front row (increasing row). `direction_2d: Some(Dir4::S)` is the
/// real direction (1-D `direction` is the transition fallback). The AI's
/// primary CLOSE direction; functional once resolver R6 reads `direction_2d`.
pub fn synthetic_move_down() -> Action {
    Action {
        id: SYNTHETIC_MOVE_DOWN.into(),
        name: "Move Down".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(crate::types::LaneEnd::Aft),
            direction_2d: Some(crate::grid::Dir4::S),
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
        effects: vec![Effect::REORIENT {
            to: ReorientTo::Flip,
        }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic rotate-LEFT (#75): `REORIENT::RotateLeft` turns the player's `facing`
/// a quarter-turn counter-clockwise (and re-derives `orientation`). Zero-cost,
/// self-targeted, all-bands — same instant-apply shape as the move/flip actions.
pub fn synthetic_rotate_left() -> Action {
    Action {
        id: SYNTHETIC_ROTATE_LEFT.into(),
        name: "Rotate Left".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::REORIENT {
            to: ReorientTo::RotateLeft,
        }],
        r#mod: None,
        icon: None,
    }
}

/// Synthetic rotate-RIGHT (#75): `REORIENT::RotateRight`, a quarter-turn clockwise.
pub fn synthetic_rotate_right() -> Action {
    Action {
        id: SYNTHETIC_ROTATE_RIGHT.into(),
        name: "Rotate Right".into(),
        archetype: WeaponArchetype::Movement,
        cost: zero_cost(),
        targeting: self_targeting(),
        effects: vec![Effect::REORIENT {
            to: ReorientTo::RotateRight,
        }],
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
/// synthetic actions and a starter `pulse_laser` / torpedo. Real catalog
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
#[derive(Debug)]
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

    /// Merge a loaded [`crate::types::Catalog`]'s actions into this content so
    /// the resolver's fire path can resolve CATALOG weapon ids (#49a). Without
    /// this, catalog-synthesized enemies mount ids like `beam_cannon` /
    /// `railgun_broadside` that the demo's hardcoded action set doesn't serve →
    /// `content.action(id)` returns `None` → the enemy AI's fire-gate skips the
    /// weapon → enemies never fire. The catalog actions already carry their
    /// 2-D `range_band` (derived at load by `catalog::load_from_bytes`), so this
    /// is purely a wiring step — no band authoring.
    ///
    /// **Does NOT clobber existing actions** (insert-if-absent): the demo's
    /// hand-tuned player weapons (`pulse_laser` / torpedo / `broadside_battery`,
    /// with their explicit demo bands) and the synthetics take precedence over a
    /// same-id catalog entry. So merging the catalog adds the *missing* enemy
    /// weapons without changing the player's loadout behavior.
    pub fn install_catalog_actions(&mut self, catalog: &crate::types::Catalog) {
        for action in &catalog.actions {
            self.actions
                .entry(action.id.clone())
                .or_insert_with(|| action.clone());
        }
    }

    /// Register the synthetic actions used by [`key_to_intent`] (the four
    /// cardinal moves + reorient + rotate-left/right + vent).
    pub fn register_synthetics(&mut self) {
        self.insert(synthetic_move_left());
        self.insert(synthetic_move_right());
        self.insert(synthetic_move_up());
        self.insert(synthetic_move_down());
        self.insert(synthetic_reorient_flip());
        self.insert(synthetic_rotate_left());
        self.insert(synthetic_rotate_right());
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
        effects: vec![Effect::BOARD {
            note: card_id.into(),
        }],
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
        // #184: cooldown 2 (was 0). Every real weapon must load-and-fire: a
        // cd-0 gun re-queues and fires every single turn (the AI lasered the
        // player to death with no reload window). The value MUST be 2, not 1:
        // one round is fire THEN end_of_turn, and end_of_turn decrements every
        // cooldown the SAME round (resolve.rs), so cd 1 ticks straight back to 0
        // and re-fires next turn (the bug, unchanged). cd 2 leaves the cooldown
        // at 1 after that round's EOT, so the NEXT round's fire-gate blocks it —
        // fire, one reload turn, fire = every other turn. A weapon here fires
        // once every cooldown_max turns. The synthetic maneuvers (__move_*/
        // __rotate_*/__vent, via `zero_cost`) keep cd 0 — they are not weapons.
        // Mirrors the catalog `pulse_laser` cd.
        c.insert(Action {
            id: "pulse_laser".into(),
            name: "Pulse Laser".into(),
            archetype: WeaponArchetype::Beam,
            cost: ActionCost {
                heat: 1,
                cooldown_max: 2,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::BEAM,
                band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
                optimal_band: RangeBand::Close,
                // v2 (A3 EXPAND): 2-D range mirror of the 1-D bands above.
                range_band: vec![crate::grid::Range::Adjacent, crate::grid::Range::Near],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: Some(TArc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE {
                amount: 4,
                band_falloff: None,
            }],
            r#mod: None,
            icon: None,
        });

        // torpedo — spawn an ordnance projectile.
        c.insert(Action {
            id: "torpedo".into(),
            name: "Torpedo".into(),
            archetype: WeaponArchetype::Ordnance,
            cost: ActionCost {
                heat: 2,
                cooldown_max: 2,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::ORDNANCE,
                band: vec![
                    RangeBand::PointBlank,
                    RangeBand::Close,
                    RangeBand::Mid,
                    RangeBand::Long,
                ],
                optimal_band: RangeBand::Mid,
                // v2 (A3 EXPAND): 2-D range mirror of the 1-D bands above.
                range_band: vec![
                    crate::grid::Range::Adjacent,
                    crate::grid::Range::Near,
                    crate::grid::Range::Far,
                ],
                optimal_range: crate::grid::Range::Near,
                requires_arc: Some(TArc::Forward),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::SPAWN_ORDNANCE {
                projectile: "torpedo".into(),
            }],
            r#mod: None,
            icon: None,
        });

        // broadside_battery (#49) — the player's 3rd mount (key 3). A BROADSIDE-
        // arc gun: only bears when the hull is turned broadside (teaches the
        // REORIENT mechanic). Mirrors the canonical catalog `broadside_battery`
        // exactly (no invented numbers): archetype broadside, heat 3 / cd 4,
        // band close, pattern BROADSIDE, arc broadsideArc; DAMAGE amount = the
        // loader's derivation for a heat-3 broadside gun (`heat + 2` = 5, see
        // catalog_canonical::inflate_effect). 2-D band: close → [Adjacent, Near]
        // (#176 fix; matches the catalog `close` derive in
        // `catalog::expand_band_2d` / `catalog_canonical::derive_range_2d_set`).
        // This was Near-ONLY (distance 2), which made the broadside WHIFF on a
        // side-on enemy at distance 1 (adjacent) — the arc bore correctly but the
        // band excluded the touching cell, so a flank-on adjacent enemy took 0
        // damage. `close` is "touching out to near", so Adjacent must be in the
        // set. DemoContent must serve it or key 3 queues an unknown id and
        // no-ops — this is the Content half of #49 (the mount is in
        // broadside.rs::player_ship).
        c.insert(Action {
            id: "broadside_battery".into(),
            name: "Broadside Battery".into(),
            archetype: WeaponArchetype::Broadside,
            cost: ActionCost {
                heat: 3,
                cooldown_max: 4,
                advances_turn: true,
            },
            targeting: Targeting {
                pattern: TargetingPattern::BROADSIDE,
                band: vec![RangeBand::Close],
                optimal_band: RangeBand::Close,
                // #176: close → [Adjacent, Near] (was [Near] only, which whiffed
                // on a flank-on enemy at distance 1). Mirror of the catalog derive.
                range_band: vec![crate::grid::Range::Adjacent, crate::grid::Range::Near],
                optimal_range: crate::grid::Range::Adjacent,
                requires_arc: Some(TArc::BroadsideArc),
                facing_relative: true,
                hits_all: false,
            },
            effects: vec![Effect::DAMAGE {
                amount: 5,
                band_falloff: None,
            }],
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
        band: crate::grid::Range,
        board: &crate::types::Board,
    ) -> i32 {
        // Audit #67: subsystem damage bonuses fire from the attacker's
        // installed fittings, not the target's. #34: `band` is the 2-D Range.
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
        spawn_ordnance(kind, owner)
    }
}

/* =========================================================================
 * Ordnance spawn table (#42).
 *
 * The real per-kind projectile stats for every catalog ordnance weapon
 * that emits a `SPAWN_ORDNANCE` effect. Authored here (in the content
 * lane) rather than in the catalog JSON because the `Projectile` shape —
 * speed / hull / payload / heading — is engine-runtime, not part of the
 * weapon's wire `Action` (the analysis-doc catalog only carries the
 * launcher's `{ cost, targeting, effects: ["SPAWN_ORDNANCE"] }`; the
 * projectile it spawns is a separate entity the resolver advances).
 *
 * ## Which kinds land here
 *
 * `Effect::SPAWN_ORDNANCE { projectile }` carries a kind string that the
 * canonical transformer defaults to the launching action's id
 * (`catalog_canonical.rs`: `m.insert("projectile", action_id)`). So the
 * `kind` argument is one of the ordnance action ids whose `effects`
 * include `SPAWN_ORDNANCE`: per the committed catalog that is exactly
 * `torpedo`, `missile_salvo`, and `heavy_torpedo`. (`mine_layer` is
 * `DEPLOY`, NOT `SPAWN_ORDNANCE` — it becomes a hazard via the DEPLOY arm,
 * never a travelling projectile, so it is deliberately absent here.)
 *
 * ## Per-kind stats (analysis HTML "Ordnance & Field Kit", lines 991-996)
 *
 * - **torpedo** — "a slow projectile entity that advances one cell per
 *   turn ... does no damage on launch — the enemy must dodge it, shoot it
 *   down, or eat it." → speed 1, a solid 4-damage payload, hull 1 (one
 *   point-defense hit breaks it up).
 * - **missile_salvo** — "multiple fast projectile entities ... many small
 *   impacts." We model the salvo as ONE fast, fragile entity carrying a
 *   light payload (the multi-impact "chain / target-lock trigger" flavor
 *   is the launcher's job — `flak_battery`/`targeting_laser` mods — not the
 *   projectile's): speed 2, 2 damage, hull 1.
 * - **heavy_torpedo** — "a slow, high-payload entity that detonates with a
 *   systems-offline pulse. Heavy heat; a capital-ship opener." → speed 1,
 *   a heavy 6-damage payload PLUS a `SystemsOffline` status rider on
 *   impact (the in-payload analog of the launcher's `APPLY_STATUS`
 *   effect), hull 2 (tougher to shoot down than a light torpedo).
 *
 * Unknown kinds fall back to a 0-damage, speed-1 dummy so a typo'd
 * projectile id degrades gracefully (flies, hits, does nothing) instead of
 * panicking mid-combat.
 *
 * ## Heading (the 2-D fix)
 *
 * `advance_projectile_2d` (R5) steps the projectile along its
 * [`Projectile::heading8`] (a `Dir8`) each ordnance phase. The pre-#42 stub
 * hardcoded `Dir8::N` for every projectile — correct only for a player
 * facing N (into the screen, toward the back-row enemies); an ENEMY torpedo
 * would have flown AWAY from the player. We derive the heading from the
 * OWNER's facing, matching the resolver's own arc-less ORDNANCE bearing
 * convention in `resolve::bearing_cardinals`: a `Bow(dir)` launches along
 * the bow; a `Broadside(axis)` launches along the axis's
 * increasing-coordinate direction (a stable "ahead" for a hull with no
 * single bow). The legacy 1-D `heading` (`LaneEnd`) is kept consistent for
 * the dead-for-live 1-D ordnance path until CONTRACT.
 * ====================================================================== */

/// Build the [`Projectile`] for ordnance `kind` launched by `owner`. See the
/// module-level table comment above for the per-kind stats and the heading
/// derivation. Public within the crate so the catalog-backed `Content` impl
/// (and tests) reuse the single authoritative table.
pub fn spawn_ordnance(kind: &str, owner: &Ship) -> Projectile {
    let heading8 = ordnance_heading8(owner);
    // Keep the legacy 1-D heading consistent (dead-for-live; CONTRACT drops it).
    let heading = match owner.orientation {
        crate::types::Orientation::BowOn { bow } => bow,
        crate::types::Orientation::Broadside => crate::types::LaneEnd::Fore,
    };
    // Shared shell; per-kind arms override speed / hull / payload / id tag.
    let base = |tag: &str, speed: u32, hull: i32, payload: Vec<Effect>| Projectile {
        id: format!("{}-{tag}-{}", owner.id, owner.cell),
        kind: kind.into(),
        cell: owner.cell,
        pos: owner.pos,
        heading,
        heading8,
        speed,
        hull,
        payload,
        owner_faction: owner.faction,
    };
    match kind {
        "torpedo" => base(
            "torp",
            1,
            1,
            vec![Effect::DAMAGE {
                amount: 4,
                band_falloff: Some(false),
            }],
        ),
        // `missile` kept as an alias of `missile_salvo` for the demo's
        // hand-built loadout / older fixtures that spawn "missile".
        "missile_salvo" | "missile" => base(
            "msl",
            2,
            1,
            vec![Effect::DAMAGE {
                amount: 2,
                band_falloff: Some(false),
            }],
        ),
        "heavy_torpedo" => base(
            "htorp",
            1,
            2,
            vec![
                Effect::DAMAGE {
                    amount: 6,
                    band_falloff: Some(false),
                },
                // The "systems-offline pulse" the desc calls out, as an
                // on-impact rider (mirrors the launcher's APPLY_STATUS).
                Effect::APPLY_STATUS {
                    status: crate::types::StatusKind::SystemsOffline,
                    duration: 3,
                },
            ],
        ),
        _ => base("unknown", 1, 1, vec![]),
    }
}

/// The [`crate::grid::Dir8`] an ordnance entity launched by `owner` travels.
/// Matches `resolve::bearing_cardinals`'s arc-less ORDNANCE convention: the
/// bow direction for a `Bow` stance, the axis's increasing-coordinate
/// direction for a `Broadside` stance (a stable "ahead" with no single bow).
const fn ordnance_heading8(owner: &Ship) -> crate::grid::Dir8 {
    use crate::grid::Facing;
    match owner.facing {
        Facing::Bow(dir) => dir.to_dir8(),
        Facing::Broadside(axis) => axis.dirs().0.to_dir8(),
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
pub const fn tutorial_lines() -> &'static [&'static str] {
    &[
        "every input advances time",
        "[arrows] move (instant)",
        "[Q/E] rotate (instant)",
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
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
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
            tail: None,
        }
    }

    #[test]
    fn key_to_intent_is_tank_controls() {
        // (#165) Tank controls: Left/Right ROTATE (no strafe); Up/Down are FORWARD/
        // REVERSE along the ship's bow (facing-relative).
        let c = DemoContent::default();
        let mut p = player_with_mounts(0);

        // Left/Right rotate regardless of facing (same as Q/E).
        assert_eq!(key_to_intent(Key::Left, &p, &c), Some(Intent::RotateLeft));
        assert_eq!(key_to_intent(Key::Right, &p, &c), Some(Intent::RotateRight));
        assert_eq!(key_to_intent(Key::Q, &p, &c), Some(Intent::RotateLeft));
        assert_eq!(key_to_intent(Key::E, &p, &c), Some(Intent::RotateRight));

        // Forward/reverse follow the bow. Cover all four cardinals: Up = move toward
        // the bow's Dir4, Down = the opposite.
        use crate::grid::{Dir4, Facing};
        for (bow, fwd, rev) in [
            (Dir4::N, Intent::MoveUp, Intent::MoveDown),
            (Dir4::S, Intent::MoveDown, Intent::MoveUp),
            (Dir4::E, Intent::MoveRight, Intent::MoveLeft),
            (Dir4::W, Intent::MoveLeft, Intent::MoveRight),
        ] {
            p.facing = Facing::Bow(bow);
            assert_eq!(
                key_to_intent(Key::Up, &p, &c),
                Some(fwd),
                "bow {bow:?} forward"
            );
            assert_eq!(
                key_to_intent(Key::Down, &p, &c),
                Some(rev),
                "bow {bow:?} reverse"
            );
        }
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
        assert_eq!(
            key_to_intent(Key::D2, &p, &c),
            None,
            "ship has 1 mount; D2 is out of range"
        );
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
        assert_eq!(
            intent_to_action_id(&Intent::MoveLeft),
            Some(SYNTHETIC_MOVE_LEFT)
        );
        assert_eq!(
            intent_to_action_id(&Intent::MoveRight),
            Some(SYNTHETIC_MOVE_RIGHT)
        );
        assert_eq!(
            intent_to_action_id(&Intent::ReorientFlip),
            Some(SYNTHETIC_REORIENT_FLIP)
        );
        assert_eq!(
            intent_to_action_id(&Intent::RotateLeft),
            Some(SYNTHETIC_ROTATE_LEFT)
        );
        assert_eq!(
            intent_to_action_id(&Intent::RotateRight),
            Some(SYNTHETIC_ROTATE_RIGHT)
        );
        assert_eq!(intent_to_action_id(&Intent::Vent), Some(SYNTHETIC_VENT));
        // Synthetic ids must use the `__` prefix so they cannot collide
        // with real catalog action ids (which are unprefixed snake_case).
        for id in [
            SYNTHETIC_MOVE_LEFT,
            SYNTHETIC_MOVE_RIGHT,
            SYNTHETIC_REORIENT_FLIP,
            SYNTHETIC_ROTATE_LEFT,
            SYNTHETIC_ROTATE_RIGHT,
            SYNTHETIC_VENT,
        ] {
            assert!(
                id.starts_with("__"),
                "synthetic id `{id}` must start with __ to avoid catalog collisions"
            );
        }
    }

    /// (#75) Q/E map to the rotate intents (Bruce can remap the keys; the
    /// intent wiring is what the rotation mechanic rides on).
    #[test]
    fn q_and_e_are_rotate_left_right() {
        let p = player_with_mounts(0);
        let c = DemoContent::default();
        assert_eq!(key_to_intent(Key::Q, &p, &c), Some(Intent::RotateLeft));
        assert_eq!(key_to_intent(Key::E, &p, &c), Some(Intent::RotateRight));
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
        assert!(c.action(SYNTHETIC_ROTATE_LEFT).is_some());
        assert!(c.action(SYNTHETIC_ROTATE_RIGHT).is_some());
        assert!(c.action(SYNTHETIC_VENT).is_some());
    }

    #[test]
    fn demo_content_serves_demo_mount_weapons() {
        let c = DemoContent::default();
        assert!(c.action("pulse_laser").is_some());
        assert!(c.action("torpedo").is_some());
        // #49: the broadside-arc 3rd mount must be served or key 3 no-ops.
        assert!(c.action("broadside_battery").is_some());
    }

    /// (#176 repro) The player's `broadside_battery` must bear on a flank-on
    /// enemy at distance 1 (Adjacent), not just distance 2 (Near). Bruce: "if
    /// the SIDE of my ship is pointed right at enemies I am doing no damage."
    /// Root cause was `range_band: [Near]` (distance exactly 2) on the `DemoContent`
    /// def — the arc bore the perpendicular flank correctly, but the band excluded
    /// the adjacent (touching) cell, so a side-on enemy at distance 1 yielded an
    /// EMPTY target set (0 damage). The fix widens the band to `[Adjacent, Near]`
    /// (the catalog `close` derive). This pins that a Bow(N) player at (2,2) now
    /// targets a due-EAST enemy at (3,2) (Chebyshev distance 1).
    #[test]
    fn broadside_battery_bears_on_adjacent_flank_enemy_176() {
        use crate::grid::{Dir4, Facing, Pos};
        use crate::resolve::resolve_targeting_2d;
        use crate::types::{Board, EventBus};

        let content = DemoContent::default();
        let action = content
            .action("broadside_battery")
            .expect("broadside_battery served by DemoContent")
            .clone();

        // Player Bow(N) at (2,2): the broadside fires out the E/W flanks (the
        // cardinals perpendicular to the bow). Mount arc is BroadsideArc so the
        // gate matches firing.
        let mut player = player_with_mounts(0);
        player.pos = Pos::new(2, 2);
        player.cell = Pos::new(2, 2).to_index();
        player.facing = Facing::Bow(Dir4::N);

        // Enemy due EAST at distance 1 (adjacent / touching the player's flank).
        let mut enemy = player_with_mounts(0);
        enemy.id = "e".into();
        enemy.faction = Faction::Enemy;
        enemy.pos = Pos::new(3, 2);
        enemy.cell = Pos::new(3, 2).to_index();
        enemy.facing = Facing::Bow(Dir4::W);

        // 20-cell (5x4) board; place both ships at their flat indices.
        let (player_idx, enemy_idx) = (player.cell, enemy.cell);
        let mut cells: Vec<Option<Ship>> = (0..crate::grid::CELLS).map(|_| None).collect();
        cells[player_idx] = Some(player);
        cells[enemy_idx] = Some(enemy);
        let board = Board {
            size: crate::grid::COLS,
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells,
            ordnance: vec![],
            hazards: (0..crate::grid::CELLS).map(|_| vec![]).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        };

        let hit = resolve_targeting_2d(&action, &board, Pos::new(2, 2));
        assert_eq!(
            hit,
            vec![Pos::new(3, 2)],
            "broadside_battery must bear on the adjacent (dist-1) flank enemy due E; \
             before #176 the [Near]-only band returned EMPTY here",
        );
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
        assert!(
            lines.iter().any(|l| l.contains("5/6/7")),
            "card binding must be advertised"
        );
        assert!(lines.iter().any(|l| l.contains("Tab")));
        assert!(lines.iter().any(|l| l.contains('V')));
        assert!(lines.iter().any(|l| l.contains("R/Space")));
        assert!(lines.iter().any(|l| l.contains("Enter")));
        assert!(lines.iter().any(|l| l.contains("Esc")));
    }

    /// Task #62 — `DemoContent::default` registers every placeholder class
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

    /// End-to-end sanity: queue a synthetic, run `resolve_round`, see the
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
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells: vec![Some(player), None, None, None, None],
            ordnance: vec![],
            hazards: (0..5).map(|_| vec![]).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
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
            bow: ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: ShieldFace {
                armour: 0,
                charge: 0,
            },
        };

        let mut board = Board {
            size: 7,
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells: vec![Some(attacker), None, None, None, None, Some(target), None],
            ordnance: vec![],
            hazards: (0..7).map(|_| vec![]).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        };

        // Weapon: 4 raw, bandFalloff: false so the modifier delta is
        // unambiguous. Distance 5 -> Long.
        let mut weapon = synthetic_vent(); // grab a free-cost shell
        weapon.id = "test_weapon".into();
        weapon.effects = vec![Effect::DAMAGE {
            amount: 4,
            band_falloff: Some(false),
        }];

        // Baseline: no Marksman installed. Hull should drop by 4.
        let mut content = DemoContent::default();
        apply_damage(5, 4, 0, &weapon, &mut board, &content);
        assert_eq!(
            board.cells[5].as_ref().unwrap().hull,
            6,
            "no marksman: 4 lands"
        );

        // Reset hull, install Marksman on the ATTACKER, fire again. Hull
        // should drop by 5. Per audit #67, Marksman is attacker-side: the
        // bonus comes from the firing ship's fittings, not the target's.
        board.cells[5].as_mut().unwrap().hull = 10;
        content.install_subsystem("p", MARKSMAN);
        apply_damage(5, 4, 0, &weapon, &mut board, &content);
        assert_eq!(
            board.cells[5].as_ref().unwrap().hull,
            5,
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
            board.cells[5].as_ref().unwrap().hull,
            6,
            "marksman on TARGET must NOT apply (audit #67 direction pin)",
        );
    }

    /// End-to-end: `HeatSink` installed on the owning ship subtracts one
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
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells: vec![Some(player), None, None],
            ordnance: vec![],
            hazards: (0..3).map(|_| vec![]).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        };

        let mut content = DemoContent::default();
        content.install_subsystem("p", HEAT_SINK);

        end_of_turn(&mut board, &content);
        // Base -1 (passive) + HeatSink -1 = -2 total. 5 -> 3.
        assert_eq!(
            board.cells[0].as_ref().unwrap().heat,
            3,
            "HeatSink stacks with passive dissipation"
        );
    }

    /* ---- Phase 2 field-kit Card integration --------------------------- */

    /// End-to-end: `try_play_card` decrements charges, push the synthetic
    /// id, run `execute_queue`, see the board-wide effect land.
    #[test]
    fn mass_lock_card_play_through_execute_queue() {
        use crate::cards::{PlayResult, CARD_MASS_LOCK};
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
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells: vec![Some(player), Some(enemy_a), None, Some(enemy_b), None],
            ordnance: vec![],
            hazards: (0..5).map(|_| vec![]).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        };

        let mut content = DemoContent::default();
        content.grant_placeholder_kit("p");

        // Step 1: validate + decrement charges.
        assert_eq!(
            content.try_play_card("p", CARD_MASS_LOCK),
            PlayResult::Played
        );
        assert_eq!(
            content
                .field_kits
                .for_ship("p")
                .unwrap()
                .find(CARD_MASS_LOCK)
                .unwrap()
                .charges,
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
                s.statuses
                    .iter()
                    .any(|st| st.kind == StatusKind::TargetLock),
                "enemy at cell {cell} should be target-locked",
            );
        }
        let p = board.cells[0].as_ref().unwrap();
        assert!(p
            .statuses
            .iter()
            .all(|st| st.kind != StatusKind::TargetLock));
    }

    /// Replaying a depleted card returns `InsufficientCharges`; the
    /// synthetic should NOT be queued in that case. (This test
    /// documents the caller contract; the bin enforces it.)
    #[test]
    fn second_play_of_one_charge_card_rejected() {
        use crate::cards::{PlayResult, CARD_MASS_BREACH};
        let mut content = DemoContent::default();
        content.grant_placeholder_kit("p");
        assert_eq!(
            content.try_play_card("p", CARD_MASS_BREACH),
            PlayResult::Played
        );
        assert_eq!(
            content.try_play_card("p", CARD_MASS_BREACH),
            PlayResult::InsufficientCharges,
        );
    }

    /// `key_to_intent`'s D5 returns the first card id from the ship's kit.
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

    /// `key_to_intent`'s D5 returns None when the ship has no kit at all.
    #[test]
    fn key_d5_returns_none_without_kit() {
        let p = player_with_mounts(0);
        let content = DemoContent::default(); // no kit granted
        assert_eq!(key_to_intent(Key::D5, &p, &content), None);
    }

    /// `PlayCard` intent does NOT route through `intent_to_action_id`
    /// (which returns None) — callers must invoke `try_play_card` +
    /// `synthetic_card_action_id` separately.
    #[test]
    fn intent_to_action_id_returns_none_for_play_card() {
        let i = Intent::PlayCard("mass_lock".into());
        assert_eq!(intent_to_action_id(&i), None);
    }

    /// `synthetic_card_action_id` uses the `__card_` prefix.
    #[test]
    fn synthetic_card_action_id_format() {
        assert_eq!(synthetic_card_action_id("mass_lock"), "__card_mass_lock");
        assert!(synthetic_card_action_id("anything").starts_with("__"));
    }

    /* ---- #42 ordnance spawn table ------------------------------------ */

    /// Sum of DAMAGE amounts in a projectile's payload (the on-impact hit).
    fn payload_damage(p: &Projectile) -> i32 {
        p.payload
            .iter()
            .filter_map(|e| match e {
                Effect::DAMAGE { amount, .. } => Some(*amount),
                _ => None,
            })
            .sum()
    }

    /// Every catalog ordnance kind that emits `SPAWN_ORDNANCE` (torpedo,
    /// `missile_salvo`, `heavy_torpedo`) spawns a real projectile with its
    /// authored per-kind stats — not the old 0-damage dummy. This is the #42
    /// regression: pre-fix only "torpedo"/"missile" had stats, so a catalog
    /// `missile_salvo` / `heavy_torpedo` launch produced an inert 0-damage
    /// entity.
    #[test]
    fn spawn_table_covers_every_catalog_ordnance_kind() {
        let content = DemoContent::default();
        let owner = player_with_mounts(0); // bow Dir4::S, faction Player

        // torpedo — slow (1), solid payload (4), fragile (1).
        let torp = content.spawn_projectile("torpedo", &owner);
        assert_eq!(torp.kind, "torpedo");
        assert_eq!(torp.speed, 1, "torpedo advances one cell per turn");
        assert_eq!(
            payload_damage(&torp),
            4,
            "torpedo carries a 4-damage payload"
        );
        assert_eq!(
            torp.hull, 1,
            "a light torpedo breaks up on one point-defense hit"
        );

        // missile_salvo — fast (2), light payload (2).
        let msl = content.spawn_projectile("missile_salvo", &owner);
        assert_eq!(msl.kind, "missile_salvo");
        assert_eq!(msl.speed, 2, "missiles are fast");
        assert_eq!(payload_damage(&msl), 2, "missile salvo is a light payload");

        // heavy_torpedo — slow (1), heavy payload (6) + a SystemsOffline rider.
        let heavy = content.spawn_projectile("heavy_torpedo", &owner);
        assert_eq!(heavy.kind, "heavy_torpedo");
        assert_eq!(heavy.speed, 1, "heavy torpedo is slow");
        assert_eq!(
            payload_damage(&heavy),
            6,
            "heavy torpedo is a heavy payload"
        );
        assert_eq!(heavy.hull, 2, "heavy torpedo is tougher to shoot down");
        assert!(
            heavy.payload.iter().any(|e| matches!(
                e,
                Effect::APPLY_STATUS {
                    status: crate::types::StatusKind::SystemsOffline,
                    ..
                }
            )),
            "heavy torpedo detonates with a systems-offline pulse",
        );

        // Unknown kind -> graceful 0-damage dummy (no panic).
        let dud = content.spawn_projectile("not_a_real_kind", &owner);
        assert_eq!(payload_damage(&dud), 0, "unknown kind is an inert dummy");
    }

    /// The projectile heading is derived from the OWNER's facing (the #42
    /// 2-D fix), NOT a hardcoded `Dir8::N`. A player facing into the screen
    /// (Bow N) launches N (toward the back-row enemies); an enemy facing the
    /// player (Bow S) launches S (toward the player) — so an enemy torpedo
    /// flies AT the player instead of away.
    #[test]
    fn spawn_heading_follows_owner_facing() {
        use crate::grid::{Dir4, Dir8, Facing};
        let content = DemoContent::default();

        let mut player = player_with_mounts(0);
        player.facing = Facing::Bow(Dir4::N);
        let p_torp = content.spawn_projectile("torpedo", &player);
        assert_eq!(p_torp.heading8, Dir8::N, "a bow-N owner launches northward");

        let mut enemy = player_with_mounts(0);
        enemy.id = "e".into();
        enemy.faction = Faction::Enemy;
        enemy.facing = Facing::Bow(Dir4::S);
        let e_torp = content.spawn_projectile("torpedo", &enemy);
        assert_eq!(
            e_torp.heading8,
            Dir8::S,
            "a bow-S enemy launches toward the player (S), not the hardcoded N",
        );
        assert_eq!(
            e_torp.owner_faction,
            Faction::Enemy,
            "ownership carries through"
        );
    }
}
