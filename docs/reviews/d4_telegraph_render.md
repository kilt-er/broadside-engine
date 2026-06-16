# Review D4 — enemy-intent telegraph render (`ac573fb`, `hud.rs`)

**Status:** ✅ **APPROVE** (static read; **no cargo run** — held per the lead's zero-builds
directive while Bruce tests; renderer reports build + 30 hud tests + clippy green, which I trust
rather than re-run). The single-source contract holds: the renderer reads `board.threats`
directly, with **no second targeting path**.

---

## The single-source check (my mandate) — HOLDS

`push_threats_2d` (`hud.rs:354`) iterates `for threat in &board.threats` (line 356) — the
resolver's ThreatMap, populated by R8 via `resolve_targeting_2d` (the single source, V4-at-R8).
The ONLY `resolve_targeting` token in `hud.rs` is a **doc comment** (line 330) naming the
provenance; there is **no `resolve_targeting` call** anywhere in the renderer. So the telegraph
the player sees is a literal render of the same `Threat` structs the shot resolves from — it
**cannot desync** from where shots land. This is exactly the no-renderer-side-second-path property
V4 enforces. ✓

## Pure render over board state — confirmed

- Each threat → a fill polygon by `ThreatKind` (`THREAT_FILL` / `_LETHAL` / `_DISPLACE` /
  `_STATUS` / `_OTHER`) + an intent beam `source`→`pos`. No state mutation, no logic that could
  diverge from the resolver.
- **Lethal read** (`:359-364`): a read-only `amount >= s.hull` check on the cell's current
  occupant, for fill-color only. Correct use of `Threat.kind`'s `Damage { amount }` — which
  V4-at-R8 confirmed R8 stores as the projected PRE-mitigation total (so "lethal flash" is a raw-
  threat cue, matching the blueprint; the player's facing/shields are exactly what they reposition
  to change). Reads `board.cells`, computes nothing spatial.
- **Self-targeted beam skip** (`:378`, `threat.source != threat.pos`): consistent with
  `paint_threats`'s own self-paint skip — no degenerate zero-length beam.
- **Draw order:** grid → threats → ships (`compose_scene_2d:326-328`), so fills sit UNDER hulls —
  matches the blueprint's "red fill UNDER a ship = positional threat."

## Observation (non-blocking, style)

- The lethal check uses `board.cells.get(threat.pos.to_index()).and_then(...)` rather than
  `board.ship_at(threat.pos)`. Equivalent under invariant A (slot == pos.to_index()), so correct
  — but `ship_at` is the idiomatic occupancy accessor and would stay correct if the indexing ever
  changed. Pure style; fine as-is.

---

## Verdict

**APPROVE.** D4 is pure render over `board.threats` with no second targeting path — the
single-source telegraph contract holds end-to-end into the renderer (resolve → paint → render all
key off the one ThreatMap). Fill-by-ThreatKind + source→target intent beam + lethal cue, drawn
under ships. Renderer's build/tests/clippy green (not re-run here per zero-builds hold). When the
build window opens I can optionally spot-run the hud tests to convert this to "confirmed," but the
single-source property is conclusive from the read.

---

*Cross-ref: V4-at-R8 (`ff3728c`, `board.threats` = the single-source paint this renders); V4
(`072f1b7`, no-second-path mandate). Reviewed under the CONTRACT-deferred / zero-builds hold —
static only. D4 @ `ac573fb`.*
