# Review: src/runs.rs (Phase-3 run-loop logic)

Reviewer audit (task #9, extended pass). Rust-native Phase-3 module. Audited
for internal correctness + consistency with the canonical campaign mechanics.
Status: **APPROVE with 1 Phase-3 gap flagged** (unwired patrol-tier scaling —
incompleteness, not a correctness bug).

## Verified

- **encounter_outcome** — Lost (no player) takes precedence over Won (no enemy); both-empty -> Lost. Single scan, no alloc. Correct.
- **advance_after_win** — AlreadyEnded guard for defeated/victorious; `next_enc = completed_encounters + 1`; <sector.encounters.len() -> NextEncounter (increments completed); else next sector exists -> NextSector (idx+1, reset completed); else -> Victorious. Out-of-bounds sector -> victorious no-op. Increment semantics correct (completed = count of finished encounters). Well-tested.
- **build_encounter_board** — player normalized to cell 0, spawns skip cell-0/off-board/occupied, hp_override sets both hull AND max_hull, hazards placed within bounds. Defensive against malformed custom sectors (won't panic). Correct.
- **canonical_lane_size** — 0-4->5, 5-6->7, else->9. Matches the analysis doc's 5/7/9 lane lengths.
- **boss_ship_for_spawn / fallback_ship_for_spawn** — boss: hull 14, 3 mounts, ReactorBreach, bow armour 3+charge 1; fallback: hull 3, 1 Forward mount. Both apply hp_override LAST so tier-scaling can override. Tested (incl. hp_override honored).
- **placeholder_sectors** — 3 sectors, progressively harder via enemy count + trait variety. Boss at final encounter with is_boss: true. Self-contained.

## PHASE-3 GAP (FLAGGED — unwired difficulty scaling, NOT a correctness bug)

`Sector.patrol_tier` is authored (1/2/3 across the three placeholder sectors,
runs.rs:445/473/511) but NEVER flows into `Board.patrol`. `build_encounter_board`
(runs.rs:206) takes `(encounter, player, class_to_ship)` — no sector/tier param —
and hardcodes `patrol: 1` (runs.rs:266). Every other Board construction site
hardcodes patrol: 1 too. The bin's spawn dispatch (broadside.rs:454-465) routes
warlord->boss / else->fallback at FIXED hull, never computing hp_override from
the tier.

Consequence: the canonical patrol mechanic ("1..7 global difficulty tier",
types.ts; EnemyDef.hull5 = effective hull at patrol 5+) is dormant. I confirmed
`resolve.rs` reads neither `board.patrol` nor `hull5` (grep: zero matches), so
nothing scales enemy difficulty by sector tier. Enemies are the same hull in
sector 3 as sector 1 (the placeholder sectors compensate with more enemies /
trait variety, so the demo still ramps — just not via hull scaling).

This is Phase-3 incompleteness: the data + types exist (design intends tier
scaling) but no code path connects sector.patrol_tier -> board.patrol -> enemy
hull5. Fix when tier scaling is scheduled: thread patrol_tier through
build_encounter_board, set board.patrol from it, and apply the hull5 bump at
patrol>=5 in the spawn synthesizers. Not blocking the demo; flagging so it's a
tracked gap, not a silent omission. (Same shape as the band-falloff finding:
designed mechanic with no consumer yet.)
