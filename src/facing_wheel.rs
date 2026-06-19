//! v2 ship **facing wheel** — the 15-facing orientation set from
//! `ShipEditor/BROADSIDE_RENDER_CONTRACT_v2.md` §5, replacing the old 4 stances.
//!
//! A ship's *visual* orientation is one of **15 baked facings**: 3 hull-rotation
//! **fans** (`Left −90°`, `Forward 0°`, `Right +90°`) × **5 lane aims** (the
//! Background viewer's per-lane nose-turn). The engine NEVER rotates a hull
//! sprite (the lights are baked in world space — see contract §3); it SWAPS to
//! the pre-lit facing the wheel selects.
//!
//! This module is **pure** (no GPU, no assets): it maps `(fan, lane) → index`
//! `0..15`, indexes back to `(fan, lane)`, names the per-facing sprite slug, and
//! carries the editor's yaw formula (for reference + the mirror-symmetry check).
//! It is fully unit-tested independent of any baked sheet, so it lands before the
//! art does; `hud` + `sprites` consume it to pick + draw the right frame.
//!
//! **Visual ≠ tactical (contract §5 callout):** these 15 answer ONLY "which way
//! is the hull pointing." Bow-on vs broadside, and which shield arc a hit lands
//! on, are a *runtime geometry* calc from positions + facings (the resolver's
//! `geometry2d`), NOT a property of the sprite. This module does not touch that.

/// The board's lane count (mirrors [`crate::grid::COLS`] = 5). The wheel has one
/// aim per lane.
pub const LANES: usize = 5;
/// The centre lane index (`SHIP_CENTER` in the contract): lane 2 of 0..4. The
/// centre lane's yaw never moves when `NOSE_TURN` changes.
pub const SHIP_CENTER: usize = 2;
/// Total baked facings: 3 fans × [`LANES`].
pub const FACING_COUNT: usize = 3 * LANES; // 15

/// Editor bake defaults (contract §5). `BASE_YAW` + `(lane − centre)·NOSE_TURN`
/// is the per-lane heading; `+ fan·90°` offsets the whole 5-lane fan. These are
/// the values the SPRITE was baked at — the engine only needs them to (a) sanity
/// the mirror-symmetry option and (b) document which yaw each index is.
pub const BASE_YAW_DEG: f32 = 0.0;
pub const NOSE_TURN_DEG: f32 = 15.0;

/// One of the three discrete hull-rotation fans (contract §5). NOT a continuous
/// wheel — the hull snaps to forward / left / right; the lane indexes the aim
/// within the fan. There is deliberately **no 180° (backward) fan**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fan {
    /// −90° group (yaws −120..−60): hull turned to port.
    Left,
    /// 0° group (yaws −30..+30): hull pointing up-lane.
    Forward,
    /// +90° group (yaws +60..+120): hull turned to starboard.
    Right,
}

impl Fan {
    /// The `r` multiplier in `facingYaw = laneYaw + r·90` (contract §5).
    pub const fn rotation(self) -> i32 {
        match self {
            Fan::Left => -1,
            Fan::Forward => 0,
            Fan::Right => 1,
        }
    }

    /// Fan-major ordering (Left, Forward, Right) → 0,1,2; the facing index is
    /// `fan_ord·LANES + lane`, matching the contract's group table order.
    const fn ordinal(self) -> usize {
        match self {
            Fan::Left => 0,
            Fan::Forward => 1,
            Fan::Right => 2,
        }
    }
}

/// A resolved baked facing: which fan + which lane aim. Maps 1:1 to a baked
/// sprite frame (one of [`FACING_COUNT`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facing15 {
    pub fan: Fan,
    /// Lane aim `0..LANES`.
    pub lane: usize,
}

impl Facing15 {
    /// Build from a fan + lane, clamping the lane into `0..LANES` (defensive — a
    /// caller deriving the lane from a board column on a wider/narrower grid
    /// can't produce an out-of-range frame).
    pub fn new(fan: Fan, lane: usize) -> Self {
        Self {
            fan,
            lane: lane.min(LANES - 1),
        }
    }

    /// The flat sprite-sheet index `0..FACING_COUNT` (fan-major: Left 0..4,
    /// Forward 5..9, Right 10..14 — the contract's group-table order).
    pub fn index(self) -> usize {
        self.fan.ordinal() * LANES + self.lane
    }

    /// Inverse of [`index`]: the `(fan, lane)` a flat index denotes. `None` if
    /// out of range.
    pub fn from_index(index: usize) -> Option<Self> {
        if index >= FACING_COUNT {
            return None;
        }
        let fan = match index / LANES {
            0 => Fan::Left,
            1 => Fan::Forward,
            _ => Fan::Right,
        };
        Some(Self {
            fan,
            lane: index % LANES,
        })
    }

    /// The world-space yaw (degrees) this facing was baked at:
    /// `facingYaw = BASE_YAW + (lane − centre)·NOSE_TURN + fan·90`
    /// (contract §5). Used for the mirror-symmetry check + documentation; the
    /// engine selects by index, not by yaw.
    pub fn yaw_deg(self) -> f32 {
        let lane_yaw = BASE_YAW_DEG + (self.lane as f32 - SHIP_CENTER as f32) * NOSE_TURN_DEG;
        lane_yaw + self.fan.rotation() as f32 * 90.0
    }

    /// The MIRROR partner across screen-X (contract §5 mirror option): the Left
    /// fan is the Right fan flipped (yaw negated), and the lane mirrors about the
    /// centre. Returns `(base_facing, flip_x)` — if `flip_x`, draw `base_facing`'s
    /// sprite horizontally flipped instead of baking this one. Forward-fan facings
    /// mirror within the Forward fan (lane mirror). Only valid to USE if the baked
    /// lighting is left/right-symmetric (a side key light breaks it).
    pub fn mirror_source(self) -> (Facing15, bool) {
        let mirror_lane = (LANES - 1) - self.lane;
        match self.fan {
            // Left is drawn as Right, flipped.
            Fan::Left => (Facing15::new(Fan::Right, mirror_lane), true),
            // Forward's left half mirrors to its right half.
            Fan::Forward => (Facing15::new(Fan::Forward, mirror_lane), true),
            // Right is its own source (the baked half).
            Fan::Right => (self, false),
        }
    }
}

/// The sprite slug for a ship `class` at `facing` — the atlas/loader key.
/// Two-digit zero-padded index keeps the 15 frames lexically ordered:
/// `"{class}_f{index:02}"` (e.g. `aegis_f07`). The matching baked PNG is
/// `assets/sprites/<slug>.png`.
pub fn facing_slug(class: &str, facing: Facing15) -> String {
    format!("{class}_f{:02}", facing.index())
}

/// Map the PLAYER ship's board orientation + column to its baked facing
/// (contract §5 wheel). Per the lead's calls:
///   - **aim lane = the ship's OWN board `column`** (0..LANES-1): the hull banks
///     to align with the lane it's IN as it moves left/right (position-driven,
///     NOT target-driven).
///   - **fan = the hull's own-forward board direction**: bow up-lane (`N`) =
///     Forward; bow toward higher col (`E`) = Right; lower col (`W`) = Left.
///     `Broadside` = the hull turned to a flank: along the lane axis
///     (`NorthSouth`) reads Forward; across it (`EastWest`) = the turned (Right)
///     fan. There is NO toward-camera/backward facing in the 15-set, so a bow
///     pointing at the camera (`S`) defensively falls back to Forward (it should
///     not occur for the player, who faces up-lane).
///
/// PLAYER ONLY: enemies face the camera (bow-toward-us), which the player-centric
/// 15-set has no view for — they stay on the flat-box placeholder pending a
/// separate enemy bake (lead escalated to Bruce). Do NOT route enemies here.
pub fn player_facing15(facing: crate::grid::Facing, column: usize) -> Facing15 {
    use crate::grid::{Axis, Dir4, Facing};
    let fan = match facing {
        Facing::Bow(Dir4::N) => Fan::Forward,
        Facing::Bow(Dir4::E) => Fan::Right,
        Facing::Bow(Dir4::W) => Fan::Left,
        Facing::Bow(Dir4::S) => Fan::Forward, // no backward facing; shouldn't occur
        Facing::Broadside(Axis::NorthSouth) => Fan::Forward, // hull aligned with the lane
        Facing::Broadside(Axis::EastWest) => Fan::Right, // turned across the lane
    };
    Facing15::new(fan, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrips_for_all_15() {
        let mut seen = [false; FACING_COUNT];
        for fan in [Fan::Left, Fan::Forward, Fan::Right] {
            for lane in 0..LANES {
                let f = Facing15::new(fan, lane);
                let i = f.index();
                assert!(i < FACING_COUNT, "index {i} in range");
                assert!(!seen[i], "index {i} unique");
                seen[i] = true;
                assert_eq!(Facing15::from_index(i), Some(f), "roundtrip at {i}");
            }
        }
        assert!(seen.iter().all(|&s| s), "all 15 indices covered");
    }

    #[test]
    fn index_layout_is_fan_major() {
        // Left 0..4, Forward 5..9, Right 10..14.
        assert_eq!(Facing15::new(Fan::Left, 0).index(), 0);
        assert_eq!(Facing15::new(Fan::Left, 4).index(), 4);
        assert_eq!(Facing15::new(Fan::Forward, 0).index(), 5);
        assert_eq!(Facing15::new(Fan::Forward, SHIP_CENTER).index(), 7);
        assert_eq!(Facing15::new(Fan::Right, 0).index(), 10);
        assert_eq!(Facing15::new(Fan::Right, 4).index(), 14);
    }

    #[test]
    fn from_index_rejects_out_of_range() {
        assert_eq!(Facing15::from_index(FACING_COUNT), None);
        assert_eq!(Facing15::from_index(99), None);
    }

    #[test]
    fn new_clamps_lane() {
        assert_eq!(Facing15::new(Fan::Forward, 99).lane, LANES - 1);
    }

    #[test]
    fn yaw_matches_contract_table() {
        // Forward fan at the shipped defaults: -30,-15,0,+15,+30.
        let fwd: Vec<f32> = (0..LANES)
            .map(|l| Facing15::new(Fan::Forward, l).yaw_deg())
            .collect();
        assert_eq!(fwd, vec![-30.0, -15.0, 0.0, 15.0, 30.0]);
        // Right fan = forward + 90: 60,75,90,105,120.
        let right: Vec<f32> = (0..LANES)
            .map(|l| Facing15::new(Fan::Right, l).yaw_deg())
            .collect();
        assert_eq!(right, vec![60.0, 75.0, 90.0, 105.0, 120.0]);
        // Left fan = forward - 90: -120,-105,-90,-75,-60.
        let left: Vec<f32> = (0..LANES)
            .map(|l| Facing15::new(Fan::Left, l).yaw_deg())
            .collect();
        assert_eq!(left, vec![-120.0, -105.0, -90.0, -75.0, -60.0]);
        // Centre lane is yaw 0 in the forward fan regardless of NOSE_TURN sign.
        assert_eq!(Facing15::new(Fan::Forward, SHIP_CENTER).yaw_deg(), 0.0);
    }

    #[test]
    fn mirror_maps_left_to_flipped_right_and_negates_yaw() {
        // Left/lane0 (yaw -120) mirrors to Right/lane4 (yaw +120), flipped.
        let (src, flip) = Facing15::new(Fan::Left, 0).mirror_source();
        assert!(flip);
        assert_eq!(src, Facing15::new(Fan::Right, 4));
        assert_eq!(
            Facing15::new(Fan::Left, 0).yaw_deg(),
            -src.yaw_deg(),
            "mirror negates the baked yaw"
        );
        // Right is its own (baked) source.
        let (rsrc, rflip) = Facing15::new(Fan::Right, 2).mirror_source();
        assert!(!rflip);
        assert_eq!(rsrc, Facing15::new(Fan::Right, 2));
        // Forward mirrors within itself about the centre.
        let (fsrc, fflip) = Facing15::new(Fan::Forward, 1).mirror_source();
        assert!(fflip);
        assert_eq!(fsrc, Facing15::new(Fan::Forward, 3));
    }

    #[test]
    fn facing_slug_is_zero_padded_indexed() {
        assert_eq!(
            facing_slug("aegis", Facing15::new(Fan::Left, 0)),
            "aegis_f00"
        );
        assert_eq!(
            facing_slug("aegis", Facing15::new(Fan::Forward, SHIP_CENTER)),
            "aegis_f07"
        );
        assert_eq!(
            facing_slug("aegis", Facing15::new(Fan::Right, 4)),
            "aegis_f14"
        );
    }

    #[test]
    fn player_facing_uses_own_column_and_own_forward_fan() {
        use crate::grid::{Axis, Dir4, Facing};
        // Bow up-lane (N) → Forward fan, lane = own column.
        assert_eq!(
            player_facing15(Facing::Bow(Dir4::N), 2),
            Facing15::new(Fan::Forward, 2)
        );
        assert_eq!(
            player_facing15(Facing::Bow(Dir4::N), 0),
            Facing15::new(Fan::Forward, 0)
        );
        // Bow toward higher col (E) → Right; lower col (W) → Left.
        assert_eq!(
            player_facing15(Facing::Bow(Dir4::E), 4),
            Facing15::new(Fan::Right, 4)
        );
        assert_eq!(
            player_facing15(Facing::Bow(Dir4::W), 1),
            Facing15::new(Fan::Left, 1)
        );
        // Broadside: aligned with the lane (N-S) reads Forward; across (E-W) turned.
        assert_eq!(
            player_facing15(Facing::Broadside(Axis::NorthSouth), 3),
            Facing15::new(Fan::Forward, 3)
        );
        assert_eq!(
            player_facing15(Facing::Broadside(Axis::EastWest), 3),
            Facing15::new(Fan::Right, 3)
        );
        // Bow toward camera (S) — no backward facing in the 15-set → Forward fallback.
        assert_eq!(
            player_facing15(Facing::Bow(Dir4::S), 2),
            Facing15::new(Fan::Forward, 2)
        );
        // Column past the lane count clamps (defensive, via Facing15::new).
        assert_eq!(player_facing15(Facing::Bow(Dir4::N), 99).lane, LANES - 1);
    }
}
