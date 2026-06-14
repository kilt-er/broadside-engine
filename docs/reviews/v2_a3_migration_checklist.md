# V2 review checklist — A3 expand-contract spatial migration (`cell:usize → Pos`)

**Status:** PRE-STAGED (A3 in progress; V2 commits arrive as a SEQUENCE — lead pings per commit).
**Scope:** the highest-blast-radius change in v2 — migrating the whole engine off the 1-D
`cell:usize` / `LaneEnd` / `Orientation` / `RangeBand` surface onto `grid.rs`
(`Pos`/`Dir8`/`Dir4`/`Axis`/`Facing`/`Range`). V1 already APPROVED the target type surface;
V2 verifies the **migration preserves 1-D semantics 1:1** and leaves the dimension-free spine
untouched.

This is a living checklist: tick each item per migration commit. Because the architect is
choreographing an **EXPAND → (migrate consumers) → CONTRACT** sequence (not a single flip),
the crate must stay green at every commit. Anchors are stable function/field names; line
numbers below are pre-migration (`v2` @ `066f9a6`-era) and WILL drift — re-grep per commit.

---

## Per-commit verdict log

### `e2d0403` — A3.1 EXPAND (additive 2D fields) — **APPROVE**

Additive-only: Ship `+pos/+facing`, Projectile `+pos/+heading8`, Hazard `+pos`, FireEvent
`+from_pos/+to_pos`, ShipSpawn `+pos/+facing`, HookContext `+source_pos/+target_pos`, Targeting
`+range_band/+optimal_range`, new `Threat`/`ThreatKind`. Nothing deleted. Verified:
- §0 green: **614 tests pass** (343 lib + 271 integration), 0 failed. clippy `--all-targets`'s
  7 warnings are **provably NOT in this commit** — all in `src/geometry2d.rs:227,251-256` (R1's
  lane). A3.1's files are clean.
- §6 fixtures: `catalog_example` + `catalog_smoke` green — pre-v2 JSON parses unchanged. serde
  renames (`rangeBand`/`optimalRange`/`heading8`) are on NEW fields only; the 1-D
  `band`/`optimalBand`/`heading` wire keys are untouched → byte-stable for surviving fields. ✓
- Threat transient (NOT in `BoardSnapshot`): correct, matches the `fire_events` precedent and
  the blueprint's "recomputed each turn from `resolve_targeting(queued)`". `Board.threats`
  rightly deferred (no consumer until R8). ✓
- Construction fills audited: **genuine carry-throughs where a pos exists** — `runs.rs`
  `boss/fallback_ship_for_spawn` carry `spawn.pos`/`spawn.facing`; `input.rs` projectile spawn
  carries `owner.pos`. Transitional `(0,0)`/`Bow(Dir4::S)` defaults at true source points (the
  `spawn()` helper, placeholder sectors → deferred to C4). ✓

  **CORRECTION (supersedes my e2d0403 take — `ba7d68f`):** In my e2d0403 review I praised
  `apply_effect`'s `Hazard.pos = Pos::from_index(c).unwrap_or(origin)` (and the bin's `make_ship`
  `Ship.pos`) as "the V1-endorsed inverse." **I was wrong**, and the lead overruled it. V1
  endorsed `from_index` as the inverse of `to_index` **within the 2-D grid's own flat-index
  space** (a `CELLS`-length board). It is NOT a valid map from a **1-D lane index** (`cell:
  usize`, lane length 5/7/9) to a 2-D `Pos` — those are different coordinate spaces, so
  `from_index(lane_cell)` encodes a meaningless position (lane cell 7 → `{col:2,row:1}`). The
  lead ruled "no valid bijection — don't derive one from the other (that fiction bites
  mid-migrate)," and `ba7d68f` replaced both derivations with plain transitional defaults
  (`Pos::new(0,0)`). **Strictly safer** (no implied mapping); producers set a real `Pos` when
  they migrate (R/D-series). Tests still green. The carry-throughs above (`spawn.pos`/`owner.pos`)
  are unchanged — those pass through an already-2-D value, they don't fake a 1-D→2-D map. My
  overall e2d0403 APPROVE stands (the fiction was inert — nothing reads `pos` yet), but this
  one line of my *reasoning* was mistaken; HEAD = e2d0403 + ba7d68f is the correct state.
- No premature reader: grep confirms `.range_band`/`.optimal_range` have **no resolver reader**
  yet (targeting still gates on 1-D `band`), so the `#[serde(default)]` → **empty** `range_band`
  Vec is inert and cannot make weapons fire-at-no-range. ✓

**Forward-notes carried to MIGRATE/CONTRACT (not A3.1 blockers):**
1. **`facing` ⇄ `orientation` will DISAGREE mid-migrate.** Every spawn currently gets
   `default_facing = Bow(Dir4::S)` regardless of its 1-D `orientation` (so a `Broadside` ship
   has `facing: Bow(S)`). Fine now (no one reads `facing`), but the MIGRATE commit that first
   reads `facing` MUST derive it from `orientation` (or vice versa) at the same time — do NOT
   leave both independently defaulted once a consumer exists. **Watch this at the facing-reader
   migrate commit.**
2. **Deferred items to not-forget at CONTRACT:** `Board.threats`, `Board.level`,
   `DISPLACE_SELF.dir8`, the temp suffixes (`heading8`→`heading`, `range_band`→`band`,
   `optimal_range`→`optimal_band`), and removal of all four `default_*` fns + the 1-D
   fields/enums (`LaneEnd`/`Orientation`/`RangeBand`). Cross-checked against §1/§2/§8.

---

## 0. Green-at-every-commit (expand-contract invariant)

- [ ] Each commit compiles the **whole crate** (`cargo build` + `cargo build --features render`).
      A half-migrated `usize`↔`Pos` state that doesn't compile breaks the shared tree.
- [ ] Each commit keeps the existing test suite green (or migrates the affected tests **in the
      same commit** — see §6). `cargo test` via **PowerShell** (Git Bash's link.exe shadows the
      MSVC linker — known team gotcha).
- [ ] `cargo clippy --all-targets` stays green (CI uses `--all-targets`; lib-only runs miss
      test-module lints).
- [ ] EXPAND commits add the new `Pos` field/path **alongside** the old `usize` one (or behind a
      conversion) without deleting the old; CONTRACT commits remove the old only once every
      consumer is migrated. Verify no commit deletes a 1-D path that still has live readers.

---

## 1. Type-surface field swaps (the EXPAND core)

Each of these `cell: usize → cell: Pos` / `orientation → Facing` / heading swaps must be
mechanical and total. Confirm the field type changed AND every constructor/reader updated:

- [ ] `Ship.cell: usize → Pos` (`types.rs:207`). Every `ship.cell` reader migrated.
- [ ] `Ship.orientation: Orientation → Facing` (`types.rs:209`). Every `.orientation` reader
      migrated; `BowOn{bow:LaneEnd}` → `Bow(Dir4)`, `Broadside` → `Broadside(Axis)`.
- [ ] `Projectile.cell: usize → Pos`, `Projectile.heading: LaneEnd → Dir8` (`types.rs:555-556`).
- [ ] `Hazard.cell: usize → Pos` (`types.rs:184`).
- [ ] `FireEvent.from_cell / to_cell: usize → Pos` (`types.rs:169-170`). Renderer beam draw
      (#59) depends on these — confirm hud.rs consumer migrated (§5).
- [ ] `HookContext.source_cell / target_cell: Option<usize> → Option<Pos>` (`types.rs:659-660`).
      These are board indices, not pointers — the no-aliasing contract must survive the swap.
- [ ] `ShipSpawn.cell: usize → Pos`, `ShipSpawn.orientation → Facing` (`runs.rs` / `types.rs:1097-1098`).
- [ ] `EnemyDef` / `CapitalDef` / `SectorDef` — confirm whether any spatial field changes (lane
      sizes, sector `lane: u8`); blueprint reshapes `Board` (drop `size`, add `level`+`threats`).

---

## 2. Board reshape

- [ ] `Board.size: usize` **dropped** (blueprint A3: "Board reshape … -size"). Every `board.size`
      reader (grep showed `.size` across resolve/hud/runs) migrated to `COLS`/`ROWS`/`CELLS`
      constants from `grid.rs`. **This is the highest-count single rename — audit each site:**
      a `0..size` loop that silently becomes `0..CELLS` must still mean the same cells.
- [ ] `Board.cells: Vec<Option<Ship>>` length is now exactly `CELLS` (20). Confirm
      `cells.len() == CELLS` invariant established at construction and that the **row-major
      index order is preserved** (already V1-confirmed: `to_index = row*COLS+col` matches the
      live faction-scan `cells.iter().find_map(...)` and find-by-id `cells.iter().position(...)`).
- [ ] `Board.level` added (campaign cursor / background focus_target, decision #3).
- [ ] `Board.threats` added (`Threat`/`ThreatKind` — ThreatMap is first-class Board state).
- [ ] `BoardSnapshot` mirror updated to match (drop `size`, add the new persistable fields);
      `From<&Board>` and `into_board` both updated. `SaveState` still round-trips.

---

## 3. resolve.rs — spatial bodies migrated, SPINE untouched

The blueprint's whole thesis: `run_action`/`apply_effect` consume only a `Vec` of cells, so the
spatial swap should NOT touch the four-phase round, the heat/cooldown gate, the damage-pipeline
ORDER, or the EventBus contract. Guard both halves:

**Migrated (signature/body changes expected):**
- [ ] `resolve_targeting(a, board, ship_cell: usize) -> Vec<usize>` (`resolve.rs:681`) →
      takes `Pos`, returns `Vec<Pos>`. This is the **single targeting source** — V4/V_telegraph
      will enforce no second path. Confirm all 8 pattern arms migrated (R3).
- [ ] `apply_damage(...)` (`resolve.rs:791`) — `atk_cell`/`target_cell` → `Pos`.
- [ ] `advance_projectile(...)` (`resolve.rs:581`) — 2D stepping along `Dir8` heading (R5).
- [ ] `first_target_toward(board, ship_cell, end: LaneEnd) -> Option<usize>` (`resolve.rs:1285`)
      → 2D walk along a `Dir8`/`Pos`. The signed-probe edge-case comments (`resolve.rs:1225-1236`,
      the `direction_to(0,-1)` underflow note) must be re-derived for 2D — **watch this one**, it
      was a 1-D-specific underflow workaround that has no direct 2D analog.
- [ ] `destroy(cell: usize, ...)` (`resolve.rs:1353`) — `Pos`. ReactorBreach adjacent-splash
      (the direct-call-not-emit path, `resolve.rs:337`-era) now uses `neighbors(pos)` /
      Chebyshev-adjacent, not `cell±1`. Confirm "adjacent" semantics preserved (1-D ±1 → 2-D
      8-neighbours is a DELIBERATE widening — flag if it should stay 4-neighbour).
- [ ] geometry import line (`resolve.rs:33`) re-points from `crate::geometry` 1-D fns to the
      rewritten 2-D ones (R1). `range_band`/`band_falloff`/`facing_zone`/`direction_to`/`bears`/
      `absorb_shield`/`opposite` all still resolve.

**Spine — MUST be byte-for-byte semantically unchanged (V5 overlap):**
- [ ] **Damage-pipeline ORDER unchanged** (`apply_damage`, `resolve.rs:791+`): step 1 band
      falloff (`:809-816`) → step 2 subsystem modifiers → step 3 target-lock ×2 → step 4
      directional shield via `facing_zone`+`absorb_shield` (`:837-841`) → hull. The ONLY changes
      should be `range_band`/`facing_zone`/`direction_to` now taking `Pos`/`Dir8`. No step
      reordered, added, or removed.
- [ ] **`band_falloff: Some(false)` bypass** still `effects.some(...)`-style (one DAMAGE effect
      with the flag disables falloff for the whole call) — `resolve.rs:811`. The 3-band
      `[1.0,0.6,0.3]` table is R1's; here just confirm the bypass predicate survives.
- [ ] **EventBus `mem::take` wrapper** (`resolve.rs:165-168`) **untouched**. The no-chained-emit
      invariant (callbacks must not re-emit) does not depend on geometry — confirm the spatial
      swap doesn't accidentally route a hook through a stale `ctx.board.bus`.
- [ ] **Emit sites & order** unchanged: `onDamageDealt` per action, `onChainKill` at window end,
      `onLethal` in destroy, `onReorient`, `onVent`, `onTurnEnd`. Same hooks, same points.
- [ ] **Chain-kill window** (`destroys_this_window` reset at executeQueue entry + ordnance phase)
      unchanged by the swap.
- [ ] **Heat/cooldown/lockout gate** in execute_queue unchanged.

---

## 4. Facing semantics — 1-D → 2-D faithfulness

The 1-D `facing_zone` (fore→starboard / aft→port tiebreak) has **no direct 2-D analog** — the
blueprint mandates a NEW `facing_zone(Facing, Dir8) -> HullZone` quadrant table (R2, reviewed
separately under V3 — the corrected table is pinned in
[`v3_facing_zone_table.md`](v3_facing_zone_table.md); **note blueprint line 30 was INVERTED for
the Broadside case** — on-axis → Bow/Stern, perpendicular → Port/Starboard, per the lead's
ruling and `grid.rs::Axis`). For V2, confirm only that the migration doesn't silently drop
facing:
- [ ] Every place that read `Orientation::BowOn{bow}` now reads `Facing::Bow(Dir4)` and every
      `Orientation::Broadside` now reads `Facing::Broadside(Axis)` — no arm collapsed/lost.
- [ ] `arc_bears` / `bears` (mount-can-fire gate) migrated to `Facing`+`Dir8`; the
      Forward/Rear/Turret/BroadsideArc semantics preserved (forward fires out the bow cardinal,
      broadsideArc only when `Broadside`, etc.).
- [ ] `REORIENT` effect (`ReorientTo{BowOn,Broadside,Flip}`) still maps onto `Facing` — `Flip`
      (180° stance-preserving) now uses `Dir4::opposite` / `Axis` correctly.
- [ ] `forward_axis()` is the renderer/resolver shared contract — confirm hud.rs bow-arrow uses
      it (don't let the renderer re-derive facing independently).

## 4b. Over-extension (decision #7) preserved

- [ ] The Far-band min-range deadzone survives: a `Far` weapon still cannot hit `Adjacent`. If
      A3 touches `in_band`/targeting, confirm the deadzone isn't silently dropped (this is a
      DESIGN-RULED positional check — inert Far enemies at close range are INTENDED, not a bug).

---

## 5. Renderer consumers (render feature)

- [ ] `hud.rs` (`.cell`/`.orientation`/`.size` × ~54 hits) migrated: ship sort, beam endpoints
      (`FireEvent.from/to_cell`), bow-arrow (`forward_axis`). Builds with `--features render`.
- [ ] `perspective.rs` (being replaced by D2's 5×4 projector) — confirm the old 1-D projector is
      removed/replaced atomically, not left dangling referencing dead `size`.
- [ ] `vfx.rs` (~39 hits), `loft_gpu.rs` (~12) — confirm any cell/orientation reads migrated.
- [ ] `input.rs` (~34 hits): lane-relative movement (Left→Aft / Right→Fore, the
      `DISPLACE_SELF.direction` extension at `types.rs:478`) re-mapped to 2-D col movement.
      **Watch:** the "after reorient, Left moves rightward" surprise the 1-D `direction` field
      solved — confirm the 2-D port keeps screen-Left = decreasing `col` regardless of facing.

---

## 6. JSON fixtures + assets (same-commit update)

Blueprint A3: "Update every JSON fixture in the same commit." Saves are WIPED for v2 (decision
#5), so no real save migration — but catalog/spawn/fixture shapes change:
- [ ] `assets/broadside.catalog.json` + `assets/broadside.catalog.example.json` — any `cell` /
      `orientation` / lane-size fields updated to the 2-D shape; `catalog.rs` loader still parses
      (`tests/catalog_smoke.rs`, `tests/catalog_example.rs` green).
- [ ] `assets/ships/broadside-ship-library.json` — confirm shape impact (likely none if it's
      pure geometry/design, but check for `orientation`).
- [ ] Inline test fixtures across `tests/*.rs` (combat_loop, pipeline, displacement, projectile,
      run_loop, event_chain, demo_scenarios, determinism, damage_extra) that construct ships at a
      `cell:` literal or `orientation:` — migrated to `Pos`/`Facing` literals in the same commit.
- [ ] The big in-file `#[cfg(test)]` suite in `resolve.rs` (~2200+ test lines, many `cell:`/
      `direction_to`/`band_falloff` sites) — **tester's lane** (hold same-file follow-ups), but
      V2 confirms they compile + pass post-migration.
- [ ] serde wire-shape: `Facing` keeps the `tag="stance"` discriminator (V1-confirmed); confirm
      no fixture hand-writes the old `{"stance":"bowOn","bow":"fore"}` without updating to the
      `Bow(Dir4)` shape.

---

## 7. Semantic-drift watch (the "silently dropped 1-D semantics" net)

Specific 1-D behaviors that are easy to lose in a mechanical swap — each needs a conscious 2-D
decision, flag if dropped without one:

> **Firing-direction contract (resolver-confirmed, 2026-06-14 — the V4/V5 authority).** Two
> direction notions with different arities, reconciled, no second targeting path:
> - **Firing-ray direction = cardinal (4-way):** `first_target_toward` walks a cardinal,
>   `arc_bears` gates a cardinal cone (consistent with decision #9's 4-cardinal facing).
> - **Damage `incoming_from` = 8-way** via `direction_to`. For every DIRECT hit (beam / spinal /
>   broadside) the target sits ON the cardinal firing ray, so `incoming_from = opposite(firing-
>   cardinal)` — **always a cardinal** (proof: cardinal step keeps same col or same row, so the
>   back-direction is the exact opposite cardinal). The diagonal cases of `direction_to` are
>   exercised ONLY by **BLAST splash** (neighbour cells off the firing ray) and **ordnance
>   impacts** (own `heading8` geometry) — and that's **intended**: `facing_zone` is total over all
>   8, so an off-axis splash legitimately lands on whatever face the diagonal presents (a BLAST can
>   hit the primary on one face and a splashed neighbour on another). V4 confirms `resolve_targeting`
>   stays the SOLE cell-selection path; V5 confirms the `direction_to → incoming_from` wiring at R4.

- [ ] **`direction_to(a,b)` equal-cells → Fore** (1-D quirk, `geometry.rs:20`). The 2-D
      `direction_to(a,a) → None` (and `from_to(a,a) → None`). Anywhere the 1-D code relied on
      equal→Fore (e.g. self-target bears) must handle the new `None` explicitly — don't let
      `None` silently become "no bearing."
- [ ] **`distance` metric change**: 1-D `abs_diff` → 2-D **Chebyshev**. Any code comparing raw
      distances (AI closing logic, ordnance range) must use the new metric consistently.
- [ ] **Range band count 5→3**: `pointBlank/close/mid/long/extreme` → `Adjacent/Near/Far`. Any
      catalog `band`/`optimalBand` referencing the old 5 bands must be remapped (content's job,
      but V2 flags un-remapped refs that would fail to deserialize).
- [ ] **±1 adjacency → 8-neighbour**: ReactorBreach splash, BLAST pattern "first + two
      neighbours", any `cell±1` arithmetic. 1-D had exactly 2 neighbours; 2-D has up to 8.
      Confirm each is a deliberate widening. **BLAST is resolved:** the resolver confirms BLAST
      splash hitting off-ray neighbours with a diagonal `incoming_from` is INTENDED (per the
      firing-direction contract above) — a BLAST may land its splash on a different face than the
      primary. Still confirm ReactorBreach splash + BLAST's "3 contiguous cells" get a deliberate
      2-D footprint shape (4- vs 8-neighbour) at the R-series commit.
- [ ] **Projectile multi-cell speed**: 1-D `speed` cells/turn along the lane → 2-D along a
      `Dir8`. Confirm the path-walk + shoot-down-mid-flight still works per cell stepped.

---

## 8. Sign-off gates

- [ ] EXPAND commit(s) reviewed → glossary option (a): add v2 terms alongside v1, marked v1/v2
      (lead will cue). [separate W-task]
- [ ] All consumer-migration commits reviewed green.
- [ ] CONTRACT commit reviewed → old 1-D surface fully removed, no dead `LaneEnd`/`size`/
      `Orientation` readers remain; glossary cleaned to v2-only (option b).
- [ ] Final: `cargo test` (PowerShell) + `cargo clippy --all-targets` green on lib AND
      `--features render`; full V2 verdict to team-lead.

---

*Anchors are function/field names (stable); line numbers are pre-migration and drift — re-grep
each commit. Cross-ref: V3 = `facing_zone` table, V4 = telegraph single-source, V5 =
damage-pipeline/EventBus guard (overlaps §3 spine items here).*
