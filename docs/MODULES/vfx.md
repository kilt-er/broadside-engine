# `src/vfx.rs` — combat-juice event → VFX framework

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/vfx.rs`](../LINE_BY_LINE.md#srcvfxrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **combat juice** layer (#51 / Phase D): it turns combat *events* into
transient visuals — weapon-fire beams, ordnance trails, hit flashes, destroy
explosions, and the telegraphed-enemy-intent cue. The *look* is deliberately
first-pass (flat-colour quads with eased fades); bruce iterates the art later, same
as the ship render. The durable value is the **plumbing**: event → effect → draw.

### The defining constraint — read-only state diff, NEVER the resolver

The architectural heart of this module (docstring src/vfx.rs:9-22): VFX must read
combat events **read-only** and must **not re-enter the resolver in a callback** —
the EventBus "no chained emit" invariant. This module satisfies that **by
construction: there is no callback at all.** It never subscribes to the
[`EventBus`](types.md) and never calls a resolver function. Instead it **diffs the
current [`Board`](types.md) against the previous frame's snapshot** and *infers* what
happened:
- a ship's `hull` dropped → hit flash (+ a beam from its nearest live opponent),
- a ship `id` vanished → destroy explosion at its last cell,
- an ordnance moved → a fading trail along its step,
- an enemy holds a queued action → a telegraph cue above it.

Pure state + math, **no GPU, no `wgpu` types**, so the whole module is unit-testable
headless. The caller ([`hud`](hud.md) / the bin) advances effect lifetimes each frame
and asks for draw commands. No TS analog — combat juice is a Rust-side render-tier
concern.

---

## Effect types (src/vfx.rs:44–117)

`Effect { kind, age, dur }` (src/vfx.rs:44) is one transient with an eased 0→1
lifetime (`t() = age/dur` clamped; `alive()` = `age < dur`). `EffectKind`
(src/vfx.rs:51) is the transient shapes: **`ShotBeam`** (the EXACT attacker→target
line, latched from the resolver's `board.fire_events` — #59; styled by archetype,
tinted by faction, dimmed on a miss), `HitFlash` (expanding flash on a hit ship),
`Explosion` (burst at a destroyed ship), `Trail` (fading ordnance streak). The per-kind lifetimes are tunable consts
(`BEAM_SECS` 0.22, `HIT_FLASH_SECS` 0.30, `EXPLOSION_SECS` 0.55, `TRAIL_SECS` 0.35).
The placeholder palette consts (src/vfx.rs:113-117) are readable flat tones. Note the
**telegraph cue is not a transient Effect** — it pulses live while the intent is
queued, emitted straight from the current board (no lifetime).

## `struct Snapshot` (src/vfx.rs:83)

The per-frame diff baseline: `ships: HashMap<id → (hull, cell, faction)>` +
`ordnance: HashMap<id → cell>`. `Snapshot::of(board)` (src/vfx.rs:91) builds it cheap
from the board. This is the "previous frame" the next frame diffs against — the
mechanism that replaces an EventBus subscription.

---

## `struct CombatVfx` (src/vfx.rs:107)

**Intent:** The live VFX state — the active `effects: Vec<Effect>` + the `prev:
Option<Snapshot>` for diffing. Render-owned; the bin advances it each frame.

### `fn observe(&mut self, board)` (src/vfx.rs:126)

**Intent:** Diff `board` against the previous frame and spawn effects for the
changes; **read-only over `board`.** Call once per frame *before* `advance`. Takes
the prev snapshot, diffs into new effects, stores the current as the next baseline.
The **first** `observe` establishes the baseline and spawns nothing
(`first_observe_spawns_nothing`, src/vfx.rs:377).

### Two sources: exact shots (#59) + the board diff

**Exact shots (`observe`, src/vfx.rs:166-186).** Before the diff, `observe` latches the
resolver's exact `board.fire_events` into styled `ShotBeam` effects — one per
[`FireEvent`](types.md) (`from_cell`/`to_cell` straight through, archetype → thickness,
faction → tint, miss → `dim`). It spawns the batch **once per round** via a `fire_sig`
guard (`fire_events_sig`, src/vfx.rs:324 — a rolling hash of the event list; spawn only
when this frame's sig differs from last frame's, since the list persists across redraws).
Read-only: the resolver owns clear+repopulate; the VFX COPIES and animates with its own
fade timers, never mutating `board.fire_events`.

**`fn diff(&mut self, prev, cur)` (src/vfx.rs:190) — the board-diff source.** Ships: a
`hull` drop → `HitFlash` on the hit cell (the impact **only** — the shot LINE now comes
from the exact `ShotBeam` above, so the diff no longer fabricates a beam from a guessed
attacker); a vanished `id` → `Explosion` at its last known cell. Ordnance: a projectile
whose cell changed → `Trail` along the step.

### `fn advance(&mut self, dt) -> bool` (src/vfx.rs:196) + `is_active` (src/vfx.rs:205)

`advance` ages every effect, drops expired, returns `true` while any survive (the
redraw-keepalive signal so the caller keeps the loop running until the juice
settles); `is_active` is the read-only twin. `advance_expires_effects`
(src/vfx.rs:437) pins the lifetime cull.

### `fn emit(&self, out, board, lane)` (src/vfx.rs:213)

**Intent:** Append draw commands for every active transient + the **live telegraph
cues** (read from `board` — a red marker above any enemy with a non-empty queue).
The caller controls where in the command stream this runs (juice above ships, below
modal overlays). `telegraph_emits_for_enemy_with_queue` (src/vfx.rs:452) pins the
cue.

**Cross-references:** `observe`/`advance`/`emit` are driven by the bin's frame loop
([`broadside.rs`](broadside.md)); `emit` produces [`DrawCommand`](gfx.md)s the
[`gfx`](gfx.md) compositor renders, positioned via
[`perspective::fractional_cell_to_screen`](perspective.md). The **read-only-diff
design** is the deliberate counterpart to the resolver's EventBus invariant
([`resolve.md`](resolve.md) / [`types.md`](types.md)'s "no chained emit").

---

## Draw helpers (src/vfx.rs:258–320)

All render as flat-colour quads via the atlas's `SOLID_WHITE` cell:
- `emit_beam` (src/vfx.rs:258) — a thin rotated rectangle from `from`→`to` along the
  lane (`rotation_rad = atan2(dy, dx)`), fading + thinning over its life. Used for
  both `ShotBeam` and `Trail`.
- `emit_flash` (src/vfx.rs:287) — an expanding (ease-out grow), fading axis-aligned
  square centred on a cell; `peak` size differs for hit (16) vs explosion (30).
- `emit_telegraph` (src/vfx.rs:310) — a small red marker well above the ship
  silhouette (`lane.center_y − 96`); first-pass chevron-bar, the per-intent icon set
  is a later art pass.

**Worked examples:** `hull_drop_spawns_hit_flash_only` (src/vfx.rs:569 — the diff now
spawns the impact flash only, no fabricated beam), `fire_event_spawns_exact_shot_beam`
(src/vfx.rs:585 — a `board.fire_events` entry latches into a `ShotBeam`),
`vanished_ship_spawns_explosion` (src/vfx.rs:634), `ordnance_step_spawns_trail`
(src/vfx.rs:645), `first_observe_spawns_nothing` (src/vfx.rs:556).

---

## Status / drift

First-pass plumbing (#51), upgraded by #59. The beam is now the resolver's **exact**
attacker→target shot (`board.fire_events` → `ShotBeam`), not the old nearest-opponent
guess — that heuristic guessed the attacker, couldn't draw multi-target fan-out, and
missed shield-fully-absorbed hits; #59 makes it always correct. The flat-quad look is
still placeholder pending bruce's art iteration. The event-sourcing-by-state-diff (vs a
resolver hook) remains the durable architectural decision — note `ShotBeam` is **read
from** `board.fire_events`, still NOT an EventBus subscription, so the "no chained emit"
property holds. The `FireEvent.hit` flag is wired through to `dim` but is always `true`
today (reserved for the #81 dodge-whiff miss path).
