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
`Explosion` (burst at a destroyed ship), `Trail` (fading ordnance streak). The per-kind
lifetimes and palette are **no longer bare module consts** — they were lifted into the
authored `VfxConfig` (see "Data layer" below); the old `BEAM_SECS` / `HIT_FLASH_SECS`
(0.30) / `EXPLOSION_SECS` (0.55) / `TRAIL_SECS` (0.35) literals now survive as the
`Default` values of the matching [`effects`](effects.md) structs, so the look is
unchanged. Note the **telegraph cue is not a transient Effect** — it pulses live while
the intent is queued, emitted straight from the current board (no lifetime).

---

## Data layer — `VfxConfig` + the `effects` schema (src/vfx.rs:140–201)

**Intent:** drive the look/timing from *authored data* instead of hardcoded
constants, so the standalone Broadside VFX editor can tune effects and the game plays
exactly what was tuned. The schema itself lives in [`src/effects.rs`](effects.md)
(pure serde, `EffectCatalog` / `EffectDef`); this module is its **consumer**.

- `struct VfxConfig` (src/vfx.rs:147) — a bundle of the six per-family
  [`effects`](effects.md) structs (`ShotBeam`, `HitFlash`, `Explosion`, `Trail`,
  `TelegraphFire`, `ParticleBurst`). Its `Default` is each field's `Default`, which
  reproduces the original `vfx.rs` constants **exactly** — so a default `VfxConfig` is
  behaviour-identical to the pre-data look.
- `default_vfx_config()` (src/vfx.rs:170) — a process-wide `OnceLock<VfxConfig>`, the
  **single source** both the windowed `vfx` path *and* the live 2-D HUD beams
  ([`hud::push_fire_2d`](hud.md) via `archetype_beam_style` / `faction_beam_tint`)
  read their styling from, so the two beam paths cannot diverge.
- `CombatVfx { …, cfg: VfxConfig }` (src/vfx.rs:178) now carries a config;
  `CombatVfx::with_config(cfg)` (src/vfx.rs:196) is the **editor's injection point**.
  `observe` styles each `ShotBeam` through `self.cfg.shot_beam` (src/vfx.rs:220–225):
  `archetype_beam_style(&self.cfg.shot_beam, archetype)` for `(thickness, life)` and
  `faction_beam_tint(&self.cfg.shot_beam, faction)` for the tint — both now take a
  `&ShotBeam` cfg arg (src/vfx.rs:449/460) rather than reading module consts.

**Behaviour-identical guarantee:** because every `effects` default equals the old
literal, none of this changes a pixel until someone authors a non-default catalog. The
`effects` module's `defaults_match_vfx_constants` test is the cross-module pin.

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

### `fn advance(&mut self, dt) -> bool` (src/vfx.rs:314) + `is_active` (src/vfx.rs:323)

`advance` ages every effect **by the measured wall-clock `dt`** (the #178 measured-dt
seam — the bin passes the real elapsed seconds since the last frame, not a per-turn
beat), drops expired, and returns `true` while any survive (the redraw-keepalive signal
so the caller keeps the loop running until the juice settles). This is what makes the
animated beam travel and the expanding explosion (below) play out over *real seconds*
regardless of how fast the turn resolves. `is_active` is the read-only twin.
`advance_expires_effects` (src/vfx.rs:1006) pins the lifetime cull.

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

## Draw helpers (src/vfx.rs:399–667)

All render as flat-colour quads via the atlas's `SOLID_WHITE` cell. Each takes the
effect's eased lifetime `t = age/dur` (so #178's wall-clock `advance` drives the
animation) plus the matching [`effects`](effects.md) cfg struct:

- `emit_beam` (src/vfx.rs:403) — a thin rotated rectangle from `from`→`to` along the
  lane (`rotation_rad = atan2(dy, dx)`), fading + thinning over its life. Used for the
  **`Trail`** (ordnance exhaust streak).
- **`emit_shot_beam` (src/vfx.rs:479) — #178 step 2, animated beam travel.** Two phases
  over wall-clock `t`: **TRAVEL** (`t < cfg.travel_frac`) eases a bright bolt-HEAD from
  muzzle→target (ease-out), drawing muzzle→head as the body plus an over-bright leading
  **tip** (`tip_len_frac` / `tip_thickness_mul`), so the shot visibly *crosses* the
  lane; **STRIKE+FADE** (`t ≥ travel_frac`) draws the full attacker→target beam and
  fades+thins it over the remaining life. Archetype `thickness` + faction `color`; a
  miss (`dim`) renders at `miss_alpha` instead of `hit_alpha`.
- `emit_flash` (src/vfx.rs:547) — the hit spark: an ease-out-growing, fading
  axis-aligned square centred on a cell (`peak_px` 16 by default).
- **`emit_explosion` (src/vfx.rs:574) — #178 step 1, expanding wall-clock explosion.**
  Three eased `SOLID_WHITE` layers over `t`: an **expanding orange shell** (the blast
  front, grows `shell_grow_base`→`+span` ease-out while fading), a **hot yellow core**
  (smaller, shrinks+fades ~2× faster, cut off at `core_life_frac`), and a brief white
  **ignition flash** (over-bright, gone by `flash_life_frac` ≈ 0.25). Bruce's "an
  explosion can run in real time," replacing the old static pop. The `ParticlePool`
  burst the bin seeds on the same kill layers debris on top.
- `emit_telegraph_fire` (src/vfx.rs:628) — #70 discharge pop: a quick expanding red
  flash at the telegraph slot (`lane.center_y + slot_offset_px`) when an enemy spends
  its readied action. (Keeps its own literal `[1.0,0.42,0.38]` tint, distinct from the
  steady cue's `TelegraphFire.color`.)
- `emit_telegraph` (src/vfx.rs:652) — the steady red marker above an enemy holding a
  queued action; first-pass bar, per-intent icon set is a later art pass.

### `struct ParticlePool` (src/vfx.rs:710) — screen-space debris

A separate, screen-space spray the bin seeds on kills (layered over `emit_explosion`).
`spawn_burst(center, n, color, dur)` (src/vfx.rs:742) seeds `n` particles flying
outward with a **deterministic** radial spread (an FNV-style fold of a per-particle
counter — no RNG dependency, so headless capture/tests are reproducible, same trick as
`fire_events_sig`); speed/size/jitter ranges come from the [`ParticleBurst`](effects.md)
cfg. `advance(dt)` (src/vfx.rs:778) integrates `pos += vel·dt`, ages, applies a light
`drag` so the spray settles, and drops the expired. `emit` (src/vfx.rs:795) pushes one
shrinking, fading `SOLID_WHITE` quad per live particle. `with_config` (src/vfx.rs:731)
is the editor-injection twin of `CombatVfx::with_config`.

> **Torpedo cell-to-cell lerp (#178 step 3) lives in [`hud`](hud.md), not here.** The
> ordnance *exhaust* is the `Trail` effect above, but the projectile **sprite** itself
> is eased between its previous and current cell over a ~lerp window by
> `hud::lerp_cell_quad` (hud.rs:455) / the per-projectile interpolated draw centre
> (hud.rs:441), drawn at hud.rs:1329. See [`hud.md`](hud.md).

**Worked examples:** `hull_drop_spawns_hit_flash_only` (the diff spawns the impact
flash only, no fabricated beam), `fire_event_spawns_exact_shot_beam` (a
`board.fire_events` entry latches into a `ShotBeam`), `vanished_ship_spawns_explosion`,
`ordnance_step_spawns_trail`, `first_observe_spawns_nothing`, `advance_expires_effects`,
plus the `ParticlePool` set (`spawn_burst` seeds exactly N, deterministic spread,
`advance` expiry) in `#[cfg(test)] mod tests` (src/vfx.rs:1006+).

---

## Status / drift

First-pass plumbing (#51), upgraded by #59 (exact shots) and **#178 (real-time
animation)**. The beam is the resolver's **exact** attacker→target shot
(`board.fire_events` → `ShotBeam`), not the old nearest-opponent guess — that heuristic
guessed the attacker, couldn't draw multi-target fan-out, and missed
shield-fully-absorbed hits; #59 makes it always correct. **#178** then made the look
*move*: the beam travels (TRAVEL→STRIKE), the explosion expands over wall-clock seconds,
and ordnance lerps cell-to-cell with an exhaust trail — all driven by the measured-`dt`
`advance` seam, none of it tied to the turn beat. The flat-quad palette is still
placeholder pending bruce's art iteration, but it is no longer static. The
event-sourcing-by-state-diff (vs a resolver hook) remains the durable architectural
decision — `ShotBeam` is **read from** `board.fire_events`, still NOT an EventBus
subscription, so the "no chained emit" property holds. The `FireEvent.hit` flag is
wired through to `dim` but is always `true` today (reserved for the #81 dodge-whiff miss
path).
