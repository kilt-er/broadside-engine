# Review — overnight render batch (e60516f + 4) — read-only, static

**Status:** ✅ **APPROVE all five.** Static read only — **no cargo** (zero-builds hold; Bruce
mid-test). Committers report build + clippy + 30 hud tests green pre-hold; trusted, not re-run.
Findings: correctness + single-source clean across the batch.

---

## PRIORITY — `e60516f` GPU Background into `gfx::render` (lead's blind change) — APPROVE

The lead's three flagged concerns, all verified conclusively from the code:

1. **Render-pass ordering — CORRECT.** The bg block (`gfx.rs:1540`) draws into `self.offscreen_view`
   with `Some(CLEAR)`, submits its own encoder, then sets `cleared = true` — and it runs BEFORE the
   scene/loft loop. Both offscreen-writing paths then honor `cleared`:
   - scene runs: `flush_scene_run` `load = if cleared { Load } else { Clear(CLEAR) }` (`:1572`),
     target `offscreen_view` (`:1586`);
   - loft blit: same `if cleared { Load } else { Clear }` (`:1692`), target `offscreen_view`
     (`:1706`).
   So the first scene run LOADs onto the bg, every later run + every loft blit LOADs too
   (`cleared` set true after each, `:1649`). The bg is **never clobbered** by the scene OR the
   loft blit. (The loft `render_ship` draws into the loft's OWN internal target
   `self.loft.output_view()`, not the offscreen — irrelevant to the bg.) This fully answers
   "not clobbering the bg or the loft pre-pass/blit."
2. **Borrow safety — CLEAN.** The bg uses `self.background.as_ref()` (immutable borrow) + a
   SEPARATE `bg_encoder`, submitted independently before the scene encoders. No aliased `&self`
   borrows across the bg draw and the scene draw; the borrow drops before the loop.
3. **Degradation — CLEAN, never panic/black.** `Background::new` builds the 20-slot queue with
   solid-ink fallbacks FIRST; `load_manifest` failure is `match`ed → `log::warn` ("using fallback
   bands"), NOT propagated/`unwrap`/panic (`:925-928`). `background` is always `Some(...)`. A
   missing/failed manifest keeps the fallback bands. A `None` background (not constructed today)
   would `if let Some(bg)` skip the bg draw → scene clears the offscreen itself (`cleared` stays
   false → first scene run does `Clear`) — still no panic, no black-unless-empty.

**Non-blocking note:** the bg adds one more per-frame command-buffer submit (bg, then each scene
run, then each loft ship — several submits/frame). Functionally correct (GPU executes in
submission order, all Load onto one offscreen) and consistent with the PRE-EXISTING per-loft-ship
separate-submit pattern (`:1654-1656`, deliberate to avoid clobbering shared loft uniforms). Not a
regression e60516f introduced; a future single-encoder consolidation is an optional perf nicety,
not a correctness issue.

---

## `f6a36c1` — player weapon-arc legibility cue (the single-source-risk one) — APPROVE

`push_weapon_arcs_2d` (`hud.rs:541`) outlines cells the player's weapons bear on. The single-source
property holds: for each cell it computes `dir = from_to(player.pos, cell)` (grid helper) then
`player.mounts.iter().any(|m| arc_bears(player.facing, m.arc, dir))` — using the resolver's
**`geometry2d::arc_bears`** (the cardinal-exact firing gate, V3/`f1db141`) as the single bearing
source. **No reimplementation of arc/cone geometry, no `resolve_targeting` recompute**; pure render
(`outline_cell_2d`). Immutable `&Ship`/`&Board`, early-returns on no-player / empty-mounts.

**Important distinction (correct, not a violation):** this cue shows ARC COVERAGE (every cell in a
bearing direction, range/occupancy-agnostic), which is broader than where a shot actually lands
(`resolve_targeting_2d` also applies `in_band` + first-occupant stop). The doc comment says so
explicitly ("the ARC half; per-weapon RANGE-band dimming is a follow-up"). This is a distinct,
clearly-labeled legibility affordance built ON the shared `arc_bears` primitive — NOT a competing
copy of the targeting resolution. The single-source mandate is about the threat/firing RESOLUTION
being one path (it is: resolve_targeting_2d); an arc-coverage hint reusing `arc_bears` is
consistent with it. ✓ (The `ship_at` style nit + this RANGE-band follow-up both land in the
renderer's next hud.rs commit per their plan.)

---

## `51d840d` (hull bars), `a238264` (queue tiles) — APPROVE

Pure render-over-board-state, both `fn(out, ship: &Ship, cfg)` immutable, `push_*` only:
- **51d840d:** fill fraction = `hull / max_hull` + green→amber→red ramp; skips degenerate
  `max_hull <= 0`; drawn in a per-ship overlay pass AFTER all hulls (depth-sorted, bars on top).
- **a238264:** one glyph tile per `ship.queue` entry in fire order (weapon-archetype glyph),
  no-op on empty queue; same overlay pass.
No mutation, no logic beyond the read, no targeting. Fine.

## `e02d669` — bin wires player → Aegis loft (#51) — APPROVE (bin-only)

12-line `broadside.rs` change routing the player ship through the loft path. Bin wiring; the loft
RENDER path itself was covered under D3/D4. No engine/contract surface touched.

---

## Verdict

**APPROVE all five.** e60516f's render-pass ordering / borrow safety / fallback degradation are
correct; f6a36c1 reuses `arc_bears` as the single bearing source (arc-coverage cue, not a second
targeting path); the hull-bar / queue-tile / bin-wiring commits are clean pure-render / wiring. If
clean is what you wanted before Bruce leans on them — it's clean. **Caveat: static review only (no
cargo per the hold); I trust the committers' green builds. Optional spot-confirm-run of the gfx +
hud tests when the build window opens.**

---

*Reviewed under the CONTRACT-deferred / zero-builds hold — static only. Cross-ref: D4
(`ea3cc36`, the sibling telegraph render); V3 (`arc_bears` cardinal-exact, reused by f6a36c1).
Batch @ e60516f / e02d669 / 51d840d / a238264 / f6a36c1.*
