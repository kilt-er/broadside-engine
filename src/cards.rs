//! Field-kit Cards runtime layer (Phase 2 task #63).
//!
//! Cards are tempo-free consumables a ship carries in its `FieldKit`. Each
//! play consumes one charge from the per-ship inventory and resolves
//! through a synthetic action whose only effect is `Effect::BOARD { note }`
//! — the [`crate::resolve::Content::apply_board_effect`] dispatch then
//! interprets the note string and applies the card's board-wide behavior.
//!
//! ## Why the BOARD-note indirection
//!
//! A card's behavior (lock every enemy, breach every enemy, clear every
//! enemy's queue, …) doesn't fit any of the existing `Effect` variants —
//! they all target one or a few cells, not "every ship matching predicate
//! P." Rather than grow `Effect` with a new variant per card, the team
//! agreed (lead's #63 brief) to keep the `Effect::BOARD { note: String }`
//! shape and dispatch by note in the content layer. New cards = new
//! match arm in [`apply_card_effect`]; no resolver or type-surface change.
//!
//! The TS engine's `Effect::BOARD` variant carries the same `{ note }`
//! shape (see `engine/types.ts`), so the dispatch indirection matches the
//! canonical reference.
//!
//! ## Storage
//!
//! Until architect adds `Ship::field_kit`, per-ship card inventories live
//! on [`crate::input::DemoContent`] via [`FieldKitRegistry`]. The lead
//! pre-authorized this content-side placeholder. The Card catalog itself
//! lives in [`CardCatalog`] (also on DemoContent for now); a future
//! `Catalog::fieldkit: Vec<Card>` upgrade reads into the same shape.
//!
//! ## Charges, not turns
//!
//! Each card has a `cost` (per-play charges) and the per-ship inventory
//! tracks `charges_remaining`. A play that has `charges_remaining < cost`
//! is rejected at the input layer ([`try_play_card`]); a card with
//! `charges_remaining == 0` after a play stays in the inventory at
//! zero — it serializes as a "spent" card. Refilling at sector boundaries
//! is deferred to Phase 3.

use std::collections::HashMap;

use crate::types::{Board, Faction, StatusKind};

/// Catalog shape of a field-kit Card. Until architect upgrades
/// `Catalog::fieldkit` to a typed list, this is content-side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// Catalog id; the BOARD `note` string is conventionally the same id
    /// (e.g. card id `"mass_lock"` → `Effect::BOARD { note: "mass_lock" }`).
    pub id: String,
    pub name: String,
    /// Charges consumed per play. Placeholder cards use `1`.
    pub cost: u32,
}

/// A per-ship card inventory entry: a card id and its current charges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardCharge {
    pub card_id: String,
    pub charges: u32,
}

/// Per-ship inventory. Until `Ship::field_kit` lands, this hangs off
/// `DemoContent` keyed by ship id.
#[derive(Clone, Debug, Default)]
pub struct FieldKit {
    pub cards: Vec<CardCharge>,
}

impl FieldKit {
    pub fn new() -> Self {
        Self { cards: Vec::new() }
    }

    /// Grant `charges` of `card_id` to this kit. If the card is already
    /// in the inventory, charges accumulate.
    pub fn grant(&mut self, card_id: impl Into<String>, charges: u32) {
        let id = card_id.into();
        for c in self.cards.iter_mut() {
            if c.card_id == id {
                c.charges = c.charges.saturating_add(charges);
                return;
            }
        }
        self.cards.push(CardCharge {
            card_id: id,
            charges,
        });
    }

    /// Find an inventory entry by id.
    pub fn find(&self, card_id: &str) -> Option<&CardCharge> {
        self.cards.iter().find(|c| c.card_id == card_id)
    }

    /// Mutable variant of [`find`] — for charge decrement.
    pub fn find_mut(&mut self, card_id: &str) -> Option<&mut CardCharge> {
        self.cards.iter_mut().find(|c| c.card_id == card_id)
    }
}

/// `ship_id` → [`FieldKit`]. The DemoContent owns one.
#[derive(Clone, Debug, Default)]
pub struct FieldKitRegistry {
    pub by_ship: HashMap<String, FieldKit>,
}

impl FieldKitRegistry {
    pub fn new() -> Self {
        Self {
            by_ship: HashMap::new(),
        }
    }

    /// Convenience: grant a card to a ship; creates the FieldKit if absent.
    pub fn grant(&mut self, ship_id: impl Into<String>, card_id: impl Into<String>, charges: u32) {
        self.by_ship
            .entry(ship_id.into())
            .or_default()
            .grant(card_id, charges);
    }

    pub fn for_ship(&self, ship_id: &str) -> Option<&FieldKit> {
        self.by_ship.get(ship_id)
    }

    pub fn for_ship_mut(&mut self, ship_id: &str) -> Option<&mut FieldKit> {
        self.by_ship.get_mut(ship_id)
    }
}

/// Catalog of Cards keyed by id. Used by the Content layer to look up
/// a card's per-play cost when validating a play.
#[derive(Clone, Debug, Default)]
pub struct CardCatalog {
    pub by_id: HashMap<String, Card>,
}

impl CardCatalog {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    pub fn insert(&mut self, card: Card) {
        self.by_id.insert(card.id.clone(), card);
    }

    pub fn get(&self, id: &str) -> Option<&Card> {
        self.by_id.get(id)
    }
}

/* =========================================================================
 * Default placeholder cards
 * ====================================================================== */

pub const CARD_MASS_LOCK: &str = "mass_lock";
pub const CARD_MASS_BREACH: &str = "mass_breach";
pub const CARD_SENSOR_PULSE: &str = "sensor_pulse";

/// Catalog ids of the three placeholder cards used by the demo.
pub const PLACEHOLDER_CARD_IDS: &[&str] = &[CARD_MASS_LOCK, CARD_MASS_BREACH, CARD_SENSOR_PULSE];

/// Build the three placeholder cards. Cost = 1 charge per play.
pub fn placeholder_catalog() -> CardCatalog {
    let mut cat = CardCatalog::new();
    cat.insert(Card {
        id: CARD_MASS_LOCK.into(),
        name: "Mass Lock".into(),
        cost: 1,
    });
    cat.insert(Card {
        id: CARD_MASS_BREACH.into(),
        name: "Mass Breach".into(),
        cost: 1,
    });
    cat.insert(Card {
        id: CARD_SENSOR_PULSE.into(),
        name: "Sensor Pulse".into(),
        cost: 1,
    });
    cat
}

/// Grant 1 charge of every placeholder card to `ship_id`. Used by the
/// demo Board setup so the player ship has cards to test with.
pub fn grant_placeholder_kit(reg: &mut FieldKitRegistry, ship_id: &str) {
    for id in PLACEHOLDER_CARD_IDS {
        reg.grant(ship_id, *id, 1);
    }
}

/* =========================================================================
 * Dispatch — the BOARD effect arm calls this through Content.
 * ====================================================================== */

/// Apply a card's board-wide effect by note string. New cards extend the
/// match arm here; new mass behaviors don't need a new `Effect` variant.
///
/// `source_cell` is the cell of the ship that played the card. The
/// placeholder cards don't actually need it (they apply to "every enemy
/// ship of the OPPOSITE faction"), but keeping it in the signature lets
/// future cards key off the player vs enemy distinction at the source.
pub fn apply_card_effect(note: &str, source_cell: usize, board: &mut Board) {
    // Source faction determines who is "every enemy" for the mass cards.
    // If the source cell is empty (the card-playing ship just died?), we
    // assume Player as the operator — the demo flow never plays cards
    // from a destroyed ship, and the alternative would silently no-op.
    let source_faction = board.cells[source_cell]
        .as_ref()
        .map(|s| s.faction)
        .unwrap_or(Faction::Player);
    let target_faction = match source_faction {
        Faction::Player => Faction::Enemy,
        Faction::Enemy => Faction::Player,
    };

    match note {
        // Apply targetLock (duration 1) to every ship of the opposite
        // faction. Duration 1 because targetLock is "next incoming hit
        // doubled" and the status is consumed on hit — duration only
        // matters if the player doesn't follow up.
        CARD_MASS_LOCK => {
            for cell in 0..board.cells.len() {
                let Some(s) = board.cells[cell].as_mut() else {
                    continue;
                };
                if s.faction != target_faction {
                    continue;
                }
                add_or_extend(s, StatusKind::TargetLock, 1);
            }
        }

        // Apply hullBreach (duration 3) to every enemy ship. Stacks
        // duration with any existing breach (max), not amount — matches
        // the analysis HTML's status semantics.
        CARD_MASS_BREACH => {
            for cell in 0..board.cells.len() {
                let Some(s) = board.cells[cell].as_mut() else {
                    continue;
                };
                if s.faction != target_faction {
                    continue;
                }
                add_or_extend(s, StatusKind::HullBreach, 3);
            }
        }

        // Clear every enemy ship's queued actions. Doesn't apply a
        // status; just drains queues. The visible-threat invariant for
        // the AI's next turn is that decide_enemy_action re-fills
        // queues, so this is one-turn relief, not a permanent silence.
        CARD_SENSOR_PULSE => {
            for cell in 0..board.cells.len() {
                let Some(s) = board.cells[cell].as_mut() else {
                    continue;
                };
                if s.faction != target_faction {
                    continue;
                }
                s.queue.clear();
            }
        }

        // Unknown note — silently no-op. New cards add a match arm above.
        _ => {}
    }
}

fn add_or_extend(ship: &mut crate::types::Ship, kind: StatusKind, duration: i32) {
    if let Some(existing) = ship.statuses.iter_mut().find(|s| s.kind == kind) {
        existing.duration = existing.duration.max(duration);
    } else {
        ship.statuses.push(crate::types::Status {
            kind,
            duration,
            face: None,
        });
    }
}

/* =========================================================================
 * Try-play wrapper — used by the input layer before queuing the synthetic.
 * ====================================================================== */

/// Outcome of attempting to play a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayResult {
    /// Charge was decremented; caller should queue the synthetic action.
    Played,
    /// Card not in the ship's inventory.
    NotCarried,
    /// Card is in inventory but insufficient charges.
    InsufficientCharges,
    /// Unknown card id (not in the CardCatalog).
    UnknownCard,
}

/// Validate and consume one play's charges from the ship's inventory.
/// Returns [`PlayResult::Played`] on success; the caller is responsible
/// for pushing the synthetic action onto the ship's queue.
///
/// Splitting validation from queueing means the queue stays a pure
/// `Vec<String>` of action ids — no card-specific resource bookkeeping
/// leaks into the resolver.
pub fn try_play_card(
    reg: &mut FieldKitRegistry,
    cat: &CardCatalog,
    ship_id: &str,
    card_id: &str,
) -> PlayResult {
    let Some(card) = cat.get(card_id) else {
        return PlayResult::UnknownCard;
    };
    let Some(kit) = reg.for_ship_mut(ship_id) else {
        return PlayResult::NotCarried;
    };
    let Some(entry) = kit.find_mut(card_id) else {
        return PlayResult::NotCarried;
    };
    if entry.charges < card.cost {
        return PlayResult::InsufficientCharges;
    }
    entry.charges -= card.cost;
    PlayResult::Played
}

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::types::{EventBus, LaneEnd, Orientation, Ship};
    use std::collections::HashMap as Map;

    fn make_ship(id: &str, faction: Faction, cell: usize) -> Ship {
        Ship {
            id: id.into(),
            faction,
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hull: 5,
            max_hull: 5,
            heat: 0,
            heat_max: 6,
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
    fn field_kit_grant_then_find() {
        let mut kit = FieldKit::new();
        kit.grant("mass_lock", 1);
        kit.grant("mass_lock", 1); // accumulates
        kit.grant("mass_breach", 2);
        assert_eq!(kit.find("mass_lock").map(|c| c.charges), Some(2));
        assert_eq!(kit.find("mass_breach").map(|c| c.charges), Some(2));
        assert!(kit.find("unknown").is_none());
    }

    #[test]
    fn registry_grants_to_named_ship_only() {
        let mut reg = FieldKitRegistry::new();
        reg.grant("p", "mass_lock", 1);
        reg.grant("p", "mass_breach", 1);
        reg.grant("enemy", "mass_lock", 1);
        assert_eq!(reg.for_ship("p").unwrap().cards.len(), 2);
        assert_eq!(reg.for_ship("enemy").unwrap().cards.len(), 1);
        assert!(reg.for_ship("unknown").is_none());
    }

    #[test]
    fn grant_placeholder_kit_yields_three_cards() {
        let mut reg = FieldKitRegistry::new();
        grant_placeholder_kit(&mut reg, "p");
        let kit = reg.for_ship("p").expect("kit created");
        assert_eq!(kit.cards.len(), 3);
        for id in PLACEHOLDER_CARD_IDS {
            assert!(
                kit.find(id).is_some(),
                "placeholder card `{id}` missing from kit",
            );
        }
    }

    #[test]
    fn try_play_unknown_card_rejected() {
        let mut reg = FieldKitRegistry::new();
        reg.grant("p", "mass_lock", 1);
        let cat = placeholder_catalog();
        assert_eq!(
            try_play_card(&mut reg, &cat, "p", "bogus_card"),
            PlayResult::UnknownCard,
        );
    }

    #[test]
    fn try_play_not_carried_rejected() {
        let mut reg = FieldKitRegistry::new();
        let cat = placeholder_catalog();
        assert_eq!(
            try_play_card(&mut reg, &cat, "p", CARD_MASS_LOCK),
            PlayResult::NotCarried,
        );
    }

    #[test]
    fn try_play_insufficient_charges_rejected() {
        let mut reg = FieldKitRegistry::new();
        reg.grant("p", CARD_MASS_LOCK, 0); // zero charges
        let cat = placeholder_catalog();
        assert_eq!(
            try_play_card(&mut reg, &cat, "p", CARD_MASS_LOCK),
            PlayResult::InsufficientCharges,
        );
    }

    #[test]
    fn try_play_success_decrements_charges() {
        let mut reg = FieldKitRegistry::new();
        reg.grant("p", CARD_MASS_LOCK, 2);
        let cat = placeholder_catalog();
        assert_eq!(
            try_play_card(&mut reg, &cat, "p", CARD_MASS_LOCK),
            PlayResult::Played,
        );
        let kit = reg.for_ship("p").unwrap();
        assert_eq!(kit.find(CARD_MASS_LOCK).unwrap().charges, 1);
    }

    #[test]
    fn mass_lock_applies_target_lock_to_every_enemy() {
        let player = make_ship("p", Faction::Player, 0);
        let scout = make_ship("scout", Faction::Enemy, 1);
        let gunboat = make_ship("gunboat", Faction::Enemy, 4);
        let mut board = empty_board(vec![player, scout, gunboat]);
        apply_card_effect(CARD_MASS_LOCK, 0, &mut board);
        // Both enemies have TargetLock.
        for cell in [1, 4] {
            let s = board.cells[cell].as_ref().unwrap();
            assert!(
                s.statuses
                    .iter()
                    .any(|st| st.kind == StatusKind::TargetLock),
                "enemy at cell {cell} should be target-locked",
            );
        }
        // Player is NOT.
        let p = board.cells[0].as_ref().unwrap();
        assert!(
            p.statuses
                .iter()
                .all(|st| st.kind != StatusKind::TargetLock),
            "player must NOT be locked by their own card",
        );
    }

    #[test]
    fn mass_breach_applies_hull_breach_to_every_enemy() {
        let player = make_ship("p", Faction::Player, 0);
        let scout = make_ship("scout", Faction::Enemy, 1);
        let mut board = empty_board(vec![player, scout]);
        apply_card_effect(CARD_MASS_BREACH, 0, &mut board);
        let s = board.cells[1].as_ref().unwrap();
        let breach = s
            .statuses
            .iter()
            .find(|st| st.kind == StatusKind::HullBreach);
        assert!(breach.is_some(), "scout should be breached");
        assert_eq!(breach.unwrap().duration, 3);
    }

    #[test]
    fn sensor_pulse_clears_every_enemy_queue() {
        let player = make_ship("p", Faction::Player, 0);
        let mut scout = make_ship("scout", Faction::Enemy, 1);
        scout.queue = vec!["pulse_laser".into(), "torpedo".into()];
        let mut gunboat = make_ship("gunboat", Faction::Enemy, 4);
        gunboat.queue = vec!["beam_cannon".into()];
        let mut board = empty_board(vec![player, scout, gunboat]);
        apply_card_effect(CARD_SENSOR_PULSE, 0, &mut board);
        assert!(board.cells[1].as_ref().unwrap().queue.is_empty());
        assert!(board.cells[4].as_ref().unwrap().queue.is_empty());
    }

    #[test]
    fn sensor_pulse_does_not_clear_player_queue() {
        let mut player = make_ship("p", Faction::Player, 0);
        player.queue = vec!["pulse_laser".into()];
        let scout = make_ship("scout", Faction::Enemy, 1);
        let mut board = empty_board(vec![player, scout]);
        apply_card_effect(CARD_SENSOR_PULSE, 0, &mut board);
        assert_eq!(
            board.cells[0].as_ref().unwrap().queue,
            vec!["pulse_laser".to_string()],
            "card played by player should not clear player's own queue",
        );
    }

    #[test]
    fn enemy_played_card_targets_player() {
        // Source = enemy at cell 1; mass_breach should breach the PLAYER
        // (faction symmetry, not enemy-only).
        let player = make_ship("p", Faction::Player, 0);
        let scout = make_ship("scout", Faction::Enemy, 1);
        let mut board = empty_board(vec![player, scout]);
        apply_card_effect(CARD_MASS_BREACH, 1, &mut board); // source = enemy
        let p = board.cells[0].as_ref().unwrap();
        assert!(
            p.statuses
                .iter()
                .any(|st| st.kind == StatusKind::HullBreach),
            "player should be breached by enemy-played mass_breach",
        );
        let s = board.cells[1].as_ref().unwrap();
        assert!(
            s.statuses
                .iter()
                .all(|st| st.kind != StatusKind::HullBreach),
            "enemy who played the card must NOT breach themselves",
        );
    }

    #[test]
    fn unknown_note_is_silent_no_op() {
        let player = make_ship("p", Faction::Player, 0);
        let scout = make_ship("scout", Faction::Enemy, 1);
        let mut board = empty_board(vec![player, scout]);
        apply_card_effect("bogus_card", 0, &mut board);
        // No status, no queue change.
        assert!(board.cells[1].as_ref().unwrap().statuses.is_empty());
    }
}
