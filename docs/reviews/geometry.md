# Review: src/geometry.rs vs engine/geometry.ts

Reviewer audit (task #9). Canonical reference: `_drive_pull/broadside-engine/engine/geometry.ts`.
Status: **APPROVE — clean 1:1 port.** No divergences.

## Verified faithful (every pure function)

- **range_band** (geometry.rs:49-62) — d<=1 PointBlank, d==2 Close, d<=4 Mid, d<=6 Long, else Extreme. Exact match to geometry.ts:30-37. Extreme is unbounded-above in both.
- **band_falloff** (geometry.rs:66-72) — factor table `[1, 0.66, 0.5, 0.33, 0.2][min(delta,4)]`, multiply, floor, max(0). Matches geometry.ts:41-45. Rust does `(raw as f64 * factor).floor() as i32`; TS does `Math.floor(raw * factor)`. Both IEEE-754 f64, same constants, so identical results (e.g. floor(4*0.66)=floor(2.64)=2). The band_index match (geometry.rs:38-46) is an exhaustive enum match = compile-time drift guard if a RangeBand variant is added.
- **direction_to** (geometry.rs:20-26) — `b >= a -> Fore` else Aft. The `a == b -> Fore` edge (easy to miss) matches TS `b >= a ? "fore" : "aft"` and is pinned by a test.
- **opposite, distance** — trivial, match.
- **facing_zone** (geometry.rs:88-105) — bowOn: incoming==bow -> Bow else Stern; broadside: fore -> Starboard, aft -> Port. Exact match to geometry.ts:61-66, including the deterministic fore=starboard/aft=port split.
- **arc_bears** (geometry.rs:114-123) — turret always true; forward = bowOn && toward==bow; rear = bowOn && toward==opposite(bow); broadsideArc = Broadside (fires both ways). Exact match to geometry.ts:74-86.
- **bears** (geometry.rs:128-133) — None arc -> always true (SELF/arc-less); else arc_bears against direction_to(ship.cell, target). Matches geometry.ts:90-93.
- **absorb_shield** (geometry.rs:141-150) — dmg<=0 -> 0 (charge NOT consumed); charge>0 -> consume one, return 0; else max(0, dmg-armour). Mutates the face. Exact match to geometry.ts:101-108, including the "non-positive damage doesn't consume charge" subtlety (pinned by test).
- **default_shield_profile** (geometry.rs:157-164) — bow{armour:2,charge:0}, stern{0,0}, port{1,0}, starboard{1,0}. Exact match to geometry.ts:112-119.

## Notes

- in_band (geometry.rs:75-77) exists in both; the resolver uses an inline `in_allowed_band` equivalent — no behavioral difference.
- 17 inline unit tests cover every function including the edge cases (direction_to equal-cells, absorb_shield non-positive-damage charge preservation, range_band boundaries 0-8). Strong coverage; tester's tests/geometry.rs + proptest.rs add the property layer.

geometry.rs is the cleanest port in the engine. Nothing to change.
