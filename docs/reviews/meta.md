# Review: src/meta.rs (Phase-3 cross-run meta-progression)

Reviewer audit (task #9, extended pass). Rust-native Phase-3 module — no TS
counterpart. Audited for internal correctness + salvage-math consistency with
the lead's brief ("1-3 per enemy, weighted by hull").
Status: **APPROVE.** No findings.

## Verified

- **salvage_for_destroyed** — hull<=3 -> 1, <=6 -> 2, else 3. Exact match to the brief's tier table. Boundaries (3/4, 6/7) tested on both sides.
- **salvage_for_encounter_win** — sums per-enemy salvage over the spawn list (correct source: cells are empty post-win, the spawn list captures who died), honours `hp_override` for the effective max_hull (so a tier-scaled enemy is valued at its actual fielded hull), then `saturating_mul(2)` on `is_boss`. Boss-doubling tested (hull-10 -> 3 -> x2 = 6).
- **accumulate_into_meta edge-trigger** (meta.rs:301-309) — the unlock condition `prev_total < threshold && new_total >= threshold` is the correct edge-detector: fires exactly once when the running total crosses, never re-fires on a later accumulate. The dup-guard (`!unlocked.contains`) is belt-and-suspenders. Tested: single-cross, multi-threshold-in-one-jump (10+25 crossed by a 26-salvage run), idempotent-already-unlocked, below-threshold-no-unlock.
- **Saturating arithmetic** throughout (salvage add, boss mul, total roll) — no overflow panics. Tested at u32::MAX-1.
- **Thresholds strictly ascending** (invariant test at line 605) — accumulate's correctness depends on the ladder being monotonic; pinned.
- **Persistence** — load-missing-returns-default (first-run safe), save-creates-parent-dir, round-trip. JSON via serde, separate file/lifecycle from the per-run save (deleting a run save doesn't reset progression) — correct separation, documented.

## Notes

- accumulate runs on EVERY run end (win OR loss) by design ("rewards engagement over win-rate") — intentional, documented. Idempotency is the caller's responsibility (double-firing the run-end event doubles salvage); flagged in the docstring. Worth a tester assertion at the bin level that the run-end event fires once, but that's integration-layer, not meta.rs.
- The `overcharge` unlock id collides with the classes.rs placeholder Signature id — see docs/reviews/subsystems.md for the cross-module flag. meta.rs's usage (subsystem) matches the canonical catalog; the placeholder is the one that should rename.
