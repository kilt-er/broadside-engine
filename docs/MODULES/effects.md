# `src/effects.rs` — VFX effect data schema

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/effects.rs`](../LINE_BY_LINE.md#srceffectsrs) section of `LINE_BY_LINE.md`,
and the data partner of [`vfx.md`](vfx.md).*

**Mirrors:** no TS analog. New in the Rust port (E1/E2, commits `545b939` /
`adab6e0`).

---

## Why this module exists

This is the **serde schema that turns the hardcoded VFX constants of
[`src/vfx.rs`](vfx.md) into authored DATA.** Both the game runtime and the standalone
`broadside_vfx_editor` app read the **same** `EffectCatalog` JSON, so an effect tuned
in the editor is byte-for-byte the effect the game plays. Defining it here, in the
engine crate, keeps a single source of truth — the same pattern as
[`ship_design`](ship_design.md) / [`save`](save.md) / [`types`](types.md), all
data-only serde living in the engine.

### Pure data, non-gated

The module is **not** behind the `render` feature: plain serde with no `wgpu`, no GPU,
no [`gfx`](gfx.md) types. So it compiles on the default (logic-only) build, and the
editor reads it without pulling the render stack. It also has **no dependency on
`vfx`** — it stands alone; `vfx` is the *consumer* (it reads these structs through
`VfxConfig`), not the other way round.

### Behaviour-identical by default — the load-bearing invariant

Every parameter mirrors a literal currently hardcoded in `vfx.rs`, and every
`Default` impl / `#[serde(default = "…")]` fn reproduces that literal **exactly**. So
a catalog built from defaults — or an empty / partial JSON file (every field is
`#[serde(default)]`) — yields precisely today's look. The data layer changes the
game's visuals only once someone edits a value. `defaults_match_vfx_constants`
(src/effects.rs:594) is the cross-module pin that fails if a `vfx.rs` constant and its
schema default ever drift apart.

---

## Color newtypes (src/effects.rs:41–52)

- `Rgb(pub [f32; 3])` (src/effects.rs:47) and `Rgba(pub [f32; 4])` (src/effects.rs:52)
  are `#[serde(transparent)]` newtypes, so the JSON wire shape is a **bare array**
  (`[r, g, b]` / `[r, g, b, a]`) — matching the [`ship_design::Point2`](ship_design.md)
  convention — while the Rust type stays distinct from a raw `[f32; N]`.

## `struct EffectCatalog` (src/effects.rs:60)

The top-level file — what an editor save / bundled game asset *is*: a flat
`effects: Vec<EffectDef>` (src/effects.rs:63), id-keyed at lookup time.

- `from_json_str(s)` (src/effects.rs:68) / `to_json_string(&self)` (src/effects.rs:73)
  — parse from / serialize to (pretty) JSON; the editor's load/save path.
- `get(id) -> Option<&EffectDef>` (src/effects.rs:79) — linear find by `id`.

`empty_catalog_round_trips` (src/effects.rs:584) pins JSON symmetry; `catalog_lookup_by_id`
(src/effects.rs:659) pins `get`.

## `struct EffectDef` (src/effects.rs:86)

One authored effect: a stable lookup `id: String` (e.g. `"player_beam"`) plus
`#[serde(flatten)] kind: EffectKind`. The `flatten` keeps the on-wire object flat —
`{ "id": …, "kind": …, <the kind's params> }` — rather than nesting the params under a
`kind` object. `effect_def_serializes_with_kind_tag` (src/effects.rs:629) pins both the
flattened `id` and the `"kind"` discriminator coexisting.

## `enum EffectKind` (src/effects.rs:100)

The six effect families the `vfx` pool produces today, **internally tagged** by a
`#[serde(tag = "kind")]` field so the JSON is self-describing and forward-extensible (a
new family is a new variant; old catalogs keep parsing). Each variant wraps a params
struct whose fields map 1:1 to constants in `vfx.rs`:

| Variant | Params struct | Mirrors in `vfx.rs` |
|---|---|---|
| `ShotBeam` | `ShotBeam` (src/effects.rs:131) | `emit_shot_beam` + `archetype_beam_style` + `faction_beam_tint` |
| `HitFlash` | `HitFlash` (src/effects.rs:162) | `HIT_COLOR` + `emit_flash` (peak 16) |
| `Explosion` | `Explosion` (src/effects.rs:186) | `EXPLOSION_COLOR` + `EXPLOSION_SECS` + `emit_explosion` (shell/core/ignition, peak 30) |
| `Trail` | `Trail` (src/effects.rs:227) | `TRAIL_COLOR` + `TRAIL_SECS` + `emit_beam` |
| `TelegraphFire` | `TelegraphFire` (src/effects.rs:245) | `TELEGRAPH_COLOR` + `TELEGRAPH_FIRE_SECS` + `emit_telegraph_fire` (slot offset −96) |
| `ParticleBurst` | `ParticleBurst` (src/effects.rs:269) | `ParticlePool::spawn_burst` + `advance` drag |

### Notable params

- **`ShotBeam.per_archetype: Vec<BeamStyle>`** (src/effects.rs:134) — the
  `archetype_beam_style` table, one `BeamStyle { archetype, thickness, life_secs }`
  (src/effects.rs:119) row per [`WeaponArchetype`](types.md) (7 rows). `enemy_tint` /
  `player_tint` are the `faction_beam_tint` colours; `travel_frac` (0.4) is the
  fraction of life in the TRAVEL phase before STRIKE+FADE; `hit_alpha` (0.95) /
  `miss_alpha` (0.45) are the hit vs dim-miss base alphas; `tip_*` style the bright
  leading tip.
- **`Explosion`** carries three eased layers as life-fractions: `core_life_frac`
  (0.55) and `flash_life_frac` (0.25) cut the core and ignition-flash off early
  inside the 0.55 s shell.
- **`ParticleBurst.dur_jitter: [f32; 2]`** (src/effects.rs:295) — stored as
  `(base, span) = (0.7, 0.6)`: a particle lives `dur * (0.7 ..= 1.3)`. `speed_*` and
  `size_*` are the spawn min/max; `drag` (2.0) is the per-second velocity falloff in
  `advance`'s `1 − 2·dt`.

## The defaults block (src/effects.rs:301–494)

A `default_*` fn per `#[serde(default = "…")]` field, each returning the **exact**
literal hardcoded in `vfx.rs` today (most are `const fn`; `default_beam_styles` builds
the 7-row table). serde needs a path-callable fn per field; these are those. The
`impl Default` for each params struct (src/effects.rs:498–578) delegates to the same
fns, so a struct built in code matches the serde-default path exactly.

> **Maintenance contract:** keep these in lock-step with `vfx.rs` until the
> param-lifting is fully done. The cross-module pin is `defaults_match_vfx_constants`
> — if you change a `vfx.rs` look constant, change the matching `default_*` here (or
> the test goes red).

---

## How `vfx` consumes this (the bridge)

[`vfx::VfxConfig`](vfx.md) (src/vfx.rs:147) is a bundle of these six params structs;
its `Default` is each field's `Default`, so it reproduces the old look. The
process-wide `default_vfx_config()` (src/vfx.rs:170) feeds **both** the windowed `vfx`
beams and the live 2-D HUD beams, so they cannot diverge. The VFX editor builds a
`VfxConfig` from an `EffectCatalog` and injects it via `CombatVfx::with_config`
(src/vfx.rs:196). See [`vfx.md`](vfx.md) "Data layer."

## Worked examples (the `#[cfg(test)] mod tests`, src/effects.rs:580)

- `empty_catalog_round_trips` (src/effects.rs:584) — `Default` catalog → JSON → back,
  equal and empty.
- `defaults_match_vfx_constants` (src/effects.rs:594) — the cross-module invariant:
  e.g. `ShotBeam::default().enemy_tint == Rgb([0.98, 0.34, 0.30])`, the beam table has
  7 rows, `ParticleBurst::default().count == 22`.
- `effect_def_serializes_with_kind_tag` (src/effects.rs:629) — internally-tagged JSON
  carries `"kind":"ShotBeam"` alongside the flattened `"id"`.
- `partial_json_fills_defaults` (src/effects.rs:643) — `{ "id": "spark", "kind":
  "HitFlash" }` with no params parses, filling every field from defaults
  (omitted `color` → `Rgb([1.0, 0.86, 0.45])`). This is why partial authoring is safe.
- `catalog_lookup_by_id` (src/effects.rs:659) — `get` finds present ids, returns
  `None` for missing.

---

## Status / drift

E1 landed the schema (`545b939`); E2 lifted the `vfx.rs` params into `VfxConfig` and
wired `CombatVfx::with_config` (`adab6e0`). The module is data-complete for today's
six families. **No behaviour change** is intended or possible from a default catalog —
the data layer is inert until someone authors non-default values. Forward extension is
a new `EffectKind` variant (old catalogs keep parsing via the internal tag).
