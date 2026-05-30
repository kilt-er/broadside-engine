//! Property-based tests for the narrow places where enumeration is unsafe.
//!
//! Two cases earn proptest:
//!
//! - **`band_falloff`** — the input space (`raw: i32`) is unbounded, so an
//!   enumerated table cannot exhaustively catch overflow or sign-bug
//!   regressions. The property is "result is in `[0, max(raw, 0)]` for every
//!   `(actual, optimal)` pair".
//! - **`absorb_shield`** — the state machine has three integer dimensions
//!   (`dmg`, `armour`, `charge`) and one observable side-effect (charge
//!   decrement). Property: charge decreases by exactly 0 or 1, returned
//!   damage is non-negative and never exceeds `dmg`, and consumption only
//!   happens when `dmg > 0`.
//!
//! Everything else in the geometry surface is finite and enumerated in
//! `tests/geometry.rs`. We deliberately do NOT sprinkle proptest there.

use broadside_engine::geometry::{absorb_shield, band_falloff};
use broadside_engine::types::{RangeBand, ShieldFace};
use proptest::prelude::*;

/// The five RangeBand variants in canonical declaration order. Mirrors the
/// private `BAND_ORDER` in `src/geometry.rs:39-45`; kept local so the
/// integration test doesn't depend on an exported constant.
const ALL_BANDS: [RangeBand; 5] = [
    RangeBand::PointBlank,
    RangeBand::Close,
    RangeBand::Mid,
    RangeBand::Long,
    RangeBand::Extreme,
];

/// Strategy producing one of the five `RangeBand` variants uniformly.
fn any_band() -> impl Strategy<Value = RangeBand> {
    prop_oneof![
        Just(RangeBand::PointBlank),
        Just(RangeBand::Close),
        Just(RangeBand::Mid),
        Just(RangeBand::Long),
        Just(RangeBand::Extreme),
    ]
}

/// Index of `b` in [`ALL_BANDS`]. Used by monotonicity property below.
fn band_idx(b: RangeBand) -> usize {
    ALL_BANDS.iter().position(|&x| x == b).expect("ALL_BANDS covers every variant")
}

proptest! {
    /// `band_falloff` must return a value in `[0, max(raw, 0)]` for any
    /// `(raw, actual, optimal)`. The factor table never goes above 1.0 and
    /// the function clamps at zero, so this is the strongest invariant
    /// that holds for the full TS-port equivalence.
    ///
    /// `raw` is constrained to a range that, when multiplied by 1.0 in
    /// f64, still fits cleanly in i32 — this avoids testing the f64-to-i32
    /// saturation behaviour of `as i32`, which is allowed to be lossy and
    /// is not the property under test.
    #[test]
    fn band_falloff_result_is_bounded_by_max_zero_raw(
        raw in -1_000_000_i32..1_000_000_i32,
        actual in any_band(),
        optimal in any_band(),
    ) {
        let out = band_falloff(raw, actual, optimal);
        prop_assert!(out >= 0, "band_falloff({raw}, {actual:?}, {optimal:?}) returned negative {out}");
        let ceiling = raw.max(0);
        prop_assert!(
            out <= ceiling,
            "band_falloff({raw}, {actual:?}, {optimal:?}) = {out} exceeds ceiling {ceiling}",
        );
    }

    /// When `actual == optimal`, the falloff factor is exactly 1.0 and the
    /// function must return non-negative raw unchanged. (Negative raw is
    /// clamped to 0.)
    #[test]
    fn band_falloff_self_pair_returns_clamped_raw(
        raw in -1_000_000_i32..1_000_000_i32,
        band in any_band(),
    ) {
        let out = band_falloff(raw, band, band);
        prop_assert_eq!(out, raw.max(0));
    }

    /// Monotonicity in delta: for fixed `raw >= 0` and fixed `optimal`, the
    /// returned damage is non-increasing as `actual` moves further from
    /// `optimal`. This pins that the factor table `[1, 0.66, 0.5, 0.33, 0.2]`
    /// is monotonically non-increasing — a typo that swapped two entries
    /// (e.g. `[1, 0.5, 0.66, ...]`) would make this property fail.
    ///
    /// The integration suite at `tests/geometry.rs` covers monotonicity for
    /// `raw == 10`; this generalises to every non-negative raw.
    #[test]
    fn band_falloff_is_monotonically_non_increasing_in_delta(
        raw in 0_i32..1_000_000_i32,
        optimal in any_band(),
    ) {
        let opt_idx = band_idx(optimal) as i32;
        // For every pair of bands (a, b), if a is closer to optimal than b,
        // then band_falloff at a must be >= band_falloff at b.
        for &a in &ALL_BANDS {
            let d_a = (band_idx(a) as i32 - opt_idx).unsigned_abs();
            let r_a = band_falloff(raw, a, optimal);
            for &b in &ALL_BANDS {
                let d_b = (band_idx(b) as i32 - opt_idx).unsigned_abs();
                if d_a <= d_b {
                    let r_b = band_falloff(raw, b, optimal);
                    prop_assert!(
                        r_a >= r_b,
                        "monotonicity broken at raw={raw}, optimal={optimal:?}: \
                         delta {d_a} ({a:?}) -> {r_a}, delta {d_b} ({b:?}) -> {r_b}",
                    );
                }
            }
        }
    }

    /// `absorb_shield` invariants:
    ///
    /// 1. The returned damage is non-negative.
    /// 2. The returned damage is at most `dmg` (no amplification — armour
    ///    only subtracts).
    /// 3. `charge` decreases by exactly 0 or 1.
    /// 4. A charge is consumed iff `dmg > 0` AND `charge > 0` (i.e. when a
    ///    positive hit lands on a face that has charge to spend).
    ///
    /// Pairing all four properties in one test cuts proptest runtime over
    /// splitting them.
    #[test]
    fn absorb_shield_invariants(
        dmg in -10_000_i32..10_000_i32,
        armour in 0_i32..1_000_i32,
        charge in 0_i32..1_000_i32,
    ) {
        let mut face = ShieldFace { armour, charge };
        let initial_charge = face.charge;
        let initial_armour = face.armour;
        let out = absorb_shield(&mut face, dmg);

        prop_assert!(out >= 0, "absorb_shield returned negative {out}");
        prop_assert!(out <= dmg.max(0), "absorb_shield returned {out} > dmg {dmg}");
        prop_assert_eq!(face.armour, initial_armour, "armour must be permanent");

        let consumed = initial_charge - face.charge;
        prop_assert!(
            consumed == 0 || consumed == 1,
            "charge changed by {consumed}, expected 0 or 1 (initial={initial_charge}, final={})",
            face.charge,
        );

        let expect_consumed = dmg > 0 && initial_charge > 0;
        prop_assert_eq!(
            consumed == 1,
            expect_consumed,
            "charge consumption mismatch: dmg={}, charge={}, consumed={}",
            dmg, initial_charge, consumed,
        );
    }

    /// When `dmg > 0` and `charge == 0`, returned damage must be exactly
    /// `max(0, dmg - armour)`. This is the armour-arithmetic branch in
    /// isolation, scanned across the full parameter range.
    #[test]
    fn absorb_shield_armour_arithmetic_when_no_charge(
        dmg in 1_i32..10_000_i32,
        armour in 0_i32..10_000_i32,
    ) {
        let mut face = ShieldFace { armour, charge: 0 };
        let out = absorb_shield(&mut face, dmg);
        prop_assert_eq!(out, (dmg - armour).max(0));
    }
}
