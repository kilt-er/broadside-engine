# Broadside v2 — Architecture Blueprint

Source: the Ultracode design panel (2026-06-14, 13 Opus agents — understand → 4 design angles → adversarial critique → synthesis). Base = CLEAN-SLATE, grafted with SHOGUN-FAITHFUL's telegraph-as-first-class-data, RENDER-FIRST's single-number depth cursor, REUSE-MAX's surgical-amputation framing. This is the source of truth the team builds from.

## One sentence
Keep the **dimension-free engine spine** (four-phase round + fire-then-decide telegraph, the damage-pipeline ORDER, heat/cooldown economy, EventBus/Content/status seams, `absorb_shield`, the catalog loader + run state machine, the loft 3D + atlas + blit render stack) and **REPLACE only the spatial layer** (`cell:usize → Pos{col,row}`, `LaneEnd → Dir8`, the 8 targeting patterns → 2D grid templates, 1D movement/ordnance → 2D, perspective.rs → a 5×4 projector, 1320×480-LINEAR → 480×270-×4-NEAREST, plus a net-new 20-layer parallax background).

## Decisive fact (verified in source)
The engine is already two engines welded: a **WHAT/WHEN** engine that is geometry-free, and a **WHERE** engine that is 100% 1D. `run_action`/`apply_effect` consume only a `Vec` of cells, so re-deriving geometry+targeting behind a stable `Pos`/`Dir8` type surface IS the whole spatial job. **~65% of resolve.rs and ALL of catalog.rs / runs.rs / loft_gpu.rs / atlas.rs carry over.** This is a **FORK-IN-PLACE** of the existing crate at `C:\Users\bruce\Broadside\engine` (NOT a new repo); Cargo.toml already pins edition 2021 + hecs 0.10 / wgpu 26 / winit 0.30, feature-gated.

## Locked decisions (Bruce, 2026-06-14)
1. **Render:** 480×270 frame, ×4 NEAREST upscale → 1920×1080 (16:9), replacing 1320×480-LINEAR.
2. **Combat:** full 2D redesign on a 5×4 grid (5 columns lateral × 4 rows depth).
3. **Campaign:** 20 parallax layers = 20 levels; clear-to-advance; `Run.layer` (0..19) doubles as BOTH campaign cursor AND background `focus_target`. Clear → `focus_target += 1`.
4. **Defense:** positioning PRIMARY (dodge out of telegraphed cells) + shields/armour SECONDARY buffer (`absorb_shield` reused verbatim). Telegraphs mandatory.
5. **Saves/JSON:** WIPE saves for v2 (no real saves exist). Fixtures update in the atomic type commit.
6. **Range falloff:** 3-band Chebyshev `Range{Adjacent, Near, Far}`, falloff `[1.0, 0.6, 0.3]` (tune in playtest).
7. **Over-extension SURVIVES in 2D:** Far-band weapons cannot hit adjacent cells (a min-range deadzone), so closing past a long-range enemy remains a real positional play — and a Far enemy the player has closed on goes inert until distance reopens. Telegraph + AI honor this.
8. **Rows = dodge; clear = kill all:** the 4 rows are pure tactical dodge space (moving back is a dodge, never required for progress); a level clears by ELIMINATING its enemies; inter-level advance (the `focus_target` bump) is automatic on clear.
9. **Facing:** keep the two stances `Facing{ Bow(Dir4) / Broadside(Axis) }` at 4 cardinals (preserves ClassAffinity + REORIENT). 8-way facing deferred.
10. **Background art:** ship a solid-ink-per-empty-layer fallback now (manifest/PNGs don't exist yet); wire the editor's real export when available — read parallax constants from the manifest, never hardcode.

## The single best idea (graft, kept verbatim)
**ThreatMap is first-class `Board` state, computed by running the REAL `resolve_targeting` against each enemy's QUEUED action** — so painted threat cells cannot desync from where the shot actually lands (correctness from reuse). The deferred `FireEvent.hit:false` (present-but-unwired today) becomes load-bearing: when the player vacates a threatened cell, the queued shot resolves to empty and emits a `hit:false` beam INTO the cell they left — the dodge-whiff read-and-react payoff.

## Defense + telegraph (full)
- **PRIMARY = positioning.** Enemies commit their NEXT action one full phase before it fires (the existing fire-then-decide world phase persistently leaves `enemy.queue` holding the next intent), so on the player's turn the complete next-turn threatened set is known and displayed. Player reads → steps to a safe cell, eats the hit on shields, or pre-empts by killing the threat. **Dodge-whiff:** vacating a threatened cell → enemy's queued shot resolves onto an empty cell → `FireEvent{hit:false}` → renderer draws a whiffed beam into the vacated cell. (NOTE: `hit:false` is NOT emitted today — resolve.rs is occupied-only — this is a deliberate NEW resolver emission.)
- **SECONDARY = shields/armour buffer, REUSE VERBATIM.** `ShieldProfile`/`ShieldFace` (4 zones: permanent armour + consumable charge) and `absorb_shield` are exactly the "hits you can't dodge" buffer. Facing still matters: an undodged hit lands on a `HullZone` via the rewritten 2D `facing_zone`, so which strong face you present is the buffer-management decision. `ShieldsUp` status + `REORIENT` are the secondary toolkit.
- **Two orthogonal render channels:** red fill UNDER a ship = positional threat (move); gold pip ON the ship = absorb buffer (fallback), one per held charge, positioned by zone.
- **MANDATORY 2D `facing_zone` quadrant TABLE** (correctness-critical, pin + unit-test BEFORE the rewrite — the current 1D fore→starboard/aft→port tiebreak has no 2D analog): `facing_zone(facing: Facing, incoming_from: Dir8) -> HullZone`. Bow(dir): incoming within ±45° of dir → Bow; within ±45° of opposite → Stern; the two perpendicular sectors → Port/Starboard (deterministic by left/right of the bow vector). Broadside(axis): the two on-axis directions → Port/Starboard, the two off-axis → Bow/Stern; diagonals snap to the nearest face by signed angle. The renderer's bow-arrow MUST encode the SAME forward axis.
- **Telegraph invariants (tester):** (a) threat-cells == hit-cells **under a no-op player** (resolve_targeting is board-state-dependent, so an ally drift or the player's own move can legitimately shift it — assert only under no-op); (b) **liveness:** every world phase leaving an enemy alive must populate ≥1 Threat OR a visible non-damage telegraph (queued move-arrow / reorient / vent); (c) dodge → `hit:false` emitted into the vacated cell.
- **Moving threats:** in-flight ordnance needs its own projector (project next-N cells along heading) feeding the same `Board.threats` (resolve_targeting is action-keyed; projectiles aren't actions).

## Reuse vs replace
- **REUSE:** ~65% of resolve.rs (four-phase round, executeQueue heat/cooldown gate, damage-pipeline ORDER, EventBus/`mem::take` contract, Content/status seams), `absorb_shield`/`default_shield_profile`, ALL of catalog.rs / runs.rs (run state machine, spawn pool) / loft_gpu.rs / atlas.rs.
- **REPLACE:** geometry.rs, the 8 targeting bodies, movement, ordnance stepping, `decide_enemy_action`, perspective.rs, `Board`, `cell:usize`→`Pos`, `LaneEnd`→`Dir8`.
- **NEW:** grid.rs (Pos/Dir8/Facing/Range), `Threat`/ThreatKind + ThreatMap population, the parallax background module, the `hit:false` dodge-whiff emission, the 5×4 perspective projector.
- **Bruce-ruled:** saves wiped (decision 5).

## Lane-by-lane build plan

SEQUENCING PRINCIPLE: the type surface lands FIRST as ONE atomic signature commit (a half-migrated `usize`↔`Pos` state won't compile and breaks the shared tree), then pure geometry, then resolver bodies behind stable types, with content `todo!()` seams kept compiling throughout. Shared-tree rules: atomic pathspec commits, inline `-m`, never `checkout` shared files, hold same-file `#[cfg(test)]` follow-ups for the tester.

**ARCHITECT** — Cargo.toml, lib.rs, types.rs, grid.rs, catalog.rs
- A1 ✅ decisions resolved (this doc). Fork-in-place confirmed.
- A2 [dep A1] grid.rs: `Pos`, `Dir8`, `Facing{Bow(Dir4)/Broadside(Axis)}`, `Range`, helpers (offset/index/from_to/rotate/neighbors), const dims, derives + serde, unit-tested. Ping reviewer + doc-writer on land.
- A3 [dep A2 + reviewer V1] THE ATOMIC TYPE-SURFACE COMMIT: `Ship.cell→Pos`, `orientation→Facing`; Projectile/Hazard/FireEvent/HookContext/ShipSpawn `cell→Pos`; Board reshape (Vec len-20-by-Pos, +level, +threats, -size) + BoardSnapshot mirror + SaveState; `Threat`/`ThreatKind`; drop dead lane fields. MUST compile the whole crate in ONE commit (coordinate the window). Update every JSON fixture in the same commit.
- A4 [dep A3] catalog.rs re-point + loader still parses; A5 hand-off pings to resolver/content/renderer.

**RESOLVER** — geometry.rs, resolve.rs spatial bodies
- R1 [dep A2] geometry.rs REPLACE over Pos/Dir8 (opposite/direction_to/distance/range_band Chebyshev/in_band/band_falloff/arc_bears 2D cone). KEEP `absorb_shield` verbatim. Pure + testable — ping tester on land.
- R2 [dep R1] `facing_zone` 2D quadrant TABLE + tests (correctness-critical, before damage wiring).
- R3 [dep A3] 8 targeting bodies → 2D templates (SELF/BOLT/LANCE/SWEEP/BLAST/COLUMN/DEPLOYED_CELL/ORDNANCE), seam returns `Vec<Pos>`.
- R4 [dep R2,R3] apply_damage: wire 2D Range falloff (step 1) + 2D facing_zone (step 4) into the UNCHANGED pipeline order.
- R5 [dep R3] advance_projectile 2D + ordnance threat projector. R6 movement bodies (or signatures + legal-move helper). R7 the `hit:false` dodge-whiff emission. R8 ThreatMap population (cache resolve_targeting(queued) into Board.threats after decide_enemy_action; clear at phase-0 boundary).

**CONTENT** — AI, displacement, modifiers, chain, spawn gen
- C1 [dep A3,R3] `decide_enemy_action` REPLACE: 2D ladder (fire-else-close-else-reorient-else-vent), readable queued threat, honor over-extension (decision 7). Rewrite, not port.
- C2 [dep C1,R8] `enemy_initiative` cross-enemy threat-SPREAD (the #74 feature, finally real in 2D: assign enemies to distinct bearings so dodging all threats at once is a non-trivial puzzle). Biggest gameplay risk — budget iteration.
- C3 displacement/modifier/splash/chain bodies. C4 spawn gen 2D (re-key pool 12→20, enemies back rows / player front-center, activate hull5 tier seam). C5 [HELD on campaign 12→20 authoring] decision deferred.

**RENDERER** — gfx.rs, atlas.rs, hud.rs, grid projector, background module
- D1 [dep A2] gfx.rs: VIRTUAL_W/H → 480×270, blit → fixed ×4 NEAREST + letterbox. Independent — land early.
- D2 [dep A2] grid.rs perspective projector: `grid_cell_quad(Pos)` + per-row depth_scale. Pin the Pos↔screen contract with architect.
- D3 [dep D2] hud.rs back-to-front draw, row-descending ship sort. D4 loft dest-quad driven by depth-scale, raise MAX_TEXTURED_SHIPS→20, bow-arrow axis == resolver Facing.
- D5 [parallel, no hard dep] background module: manifest+PNG loader + `visible_layers` (spec §4 math) + bg pass + SOLID-INK fallback per empty layer. Wire `focus_target=Run.layer`, `player_pos=column`.
- D6 [dep A3,R8] threatened-cell render: red fill under ships by ThreatKind + lethal flash; gold shield pips per zone; whiff beam on `FireEvent.hit:false`.

**TESTER** — tests/, CI
- T1 grid property/table tests. T2 geometry tests (Chebyshev bands, the new [1.0,0.6,0.3] falloff, the facing_zone table every Dir8×Facing). T3 targeting templates. T4 the telegraph regressions (threat==hit under no-op; liveness; dodge→hit:false). T5 AI tests (fire-else-close; close reduces Chebyshev distance; spread ≥2 distinct bearings). T6 keep clippy --all-targets green; cargo via PowerShell.

**REVIEWER** — V1 grid type surface (before A3 locks it); V2 the atomic type+JSON commit (highest blast radius); V3 facing_zone table; V4 telegraph single-source-of-truth + hit:false (enforce: NO second targeting path for telegraphs); V5 guard the damage-pipeline ORDER + EventBus contract through the rewrite.

**DOC-WRITER** — W1 grid.rs; W2 2D geometry + facing_zone table (write the "why"); W3 telegraph/ThreatMap single-source contract + no-op invariant; W4 2D AI ladder + threat-spread (#74 revived); W5 [HELD] classes.rs pending Aegis ClassDef.

## Critical path
A2 → V1 → **A3 (atomic type commit, the bottleneck)** → R1/R3 + C1 + D2/D4/D6 fan out. Independent-of-A3 work that starts immediately: renderer D1 (480×270) + D5 (background).
