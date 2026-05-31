# `src/hud.rs` — Module Companion

*A self-contained walkthrough of the scene compositor. The same content as the
[`hud.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srchudrs), but scoped:
this file assumes you only care about how a `Board` becomes a `Vec<DrawCommand>`
for the renderer. Read this if you are about to add a HUD overlay, tune the
parallax, change the silhouette morph, or add a new font glyph.*

**Source commit:** stabilized through Phase 3 Slice E. 1455 lines.
**Mirrors:** No TS analog. Scene composition is a Rust-port concern from day one.
**No Drift section** by design — same convention as [atlas.md](atlas.md).
**Design anchor:** Tasks #29 (Slice D — compose scene) + #45 (win/lose overlays) +
#46 (tween animations) + #58 (single silhouette + bow morph) + #59 (parallax
responds to view_angle) + #77 / #78 (between-encounter screen + salvage HUD).

---

## Why this file exists

`hud.rs` is the **scene compositor**: it walks the `Board` and emits a back-to-
front `Vec<DrawCommand>` that `gfx.rs::Gfx::render` consumes. Every draw call
the renderer makes originates here. The contract between hud and gfx is:

- `hud.rs::compose_scene_tweened(...)` returns `Vec<DrawCommand>` in
  back-to-front z-order.
- `gfx.rs::Gfx::render(&commands)` walks the list, switches pipelines as
  variants change, and submits the frame.

There is no depth buffer; the list is authoritative. Reordering anything in
`compose_scene_tweened` reorders what the player sees.

Five things to know up front:

1. **The render order is documented at the top of the file** (rustdoc lines
   12–24). Read it before touching any `push_*` function. The ordering — sky
   parallax → floor parallax → lane → range ticks → hazards → ships → ordnance
   → heat bars → shield pips → queue glyphs → status badges → end-state
   overlays — is the canonical z-order.
2. **`compose_scene_tweened` is the canonical entry point.** `compose_scene`
   and `compose_scene_with` are thin shims that forward defaults. The bin
   calls `compose_scene_tweened` directly; the others exist for tests.
3. **The view-angle scrubber drives a camera-revolves morph.** Ship
   silhouettes use `height × cos(θ) + depth_dim × sin(θ)` for vertical
   extent; parallax planes foreshorten with the same trig. At θ=0 you see a
   pure side view; at θ=π/2 you see a pure top-down. Default is 45°.
4. **The starfield uses a Wang-hash PRNG, not the atlas.** The atlas's
   `PARALLAX_FAR_STARS` / `PARALLAX_MID_STARS` cells are **vestigial** —
   never referenced from `hud.rs`. Stars are painted per-frame as
   SOLID_WHITE quads scattered by Thomas Wang-style integer hashes.
   (Functions named `lcg_*` for brevity; the underlying primitive is not
   actually a linear-congruential generator — see LBL for the distinction.)
5. **Text rendering is inline 5×7 bitmap, not atlas-packed.** `push_glyph_5x7`
   at the bottom of the file holds a hand-rolled `match` arm with 7 rows of
   5 bits per supported glyph. Used by `push_centered_banner` for every modal
   overlay. Supported set: A, C, D, E, F, G, I, L, M, N, O, P, R, S, T, U, V,
   Y, 0–9, `-`, `:`, space.

---

## The three compose_scene shims

```
compose_scene(board, lane, view_angle_rad)
   └─► compose_scene_with(board, lane, view_angle_rad, &EmptySpriteRegistry)
          └─► compose_scene_tweened(board, lane, view_angle_rad, sprites,
                                    &TweenState::default())
                  // ← the real implementation. Everything else forwards
                  //   default values for one extra argument.
```

| Entry point                  | Adds                              | Used by                 |
|------------------------------|-----------------------------------|-------------------------|
| `compose_scene`              | nothing                           | unit tests, demos       |
| `compose_scene_with`         | a `SpriteRegistry` for PNG ships  | hud's integration tests |
| `compose_scene_tweened`      | a `TweenState` for ship cells     | the bin (canonical)     |

### `TweenState`

```rust
pub struct TweenState {
    pub visual_cells: HashMap<String, f32>,
}
```

Per-ship fractional cell-position overrides keyed by `Ship::id`. The bin captures
previous cell positions before each input mutation and lerps `prev → current`
over ~200ms using ease-out, producing a `TweenState` per frame. **This is what
makes movement feel animated under the otherwise-instant Shogun-Showdown turn
semantics.** All five per-ship overlay helpers consume the same tweened cell,
so heat bars / shield pips / queue glyphs / status badges track the smoothed
silhouette, not the jumped logical position.

`TweenState::cell_for(ship)` returns `visual_cells[ship.id]` if present, else
falls back to `ship.cell as f32`.

---

## The render order

`compose_scene_tweened` builds the draw list in this fixed order:

```
   1. push_parallax(out, lane, view_angle_rad)
   2. push_lane(out, lane)
   3. push_range_band_ticks(out, board, lane)
   4. push_hazards(out, board, lane)
   5. for ship in ships:
        push_ship(out, ship, visual_cell, lane, view_angle_rad, sprites)
   6. for proj in board.ordnance:
        push_projectile(out, proj, lane)
   7. for ship in ships:    // second pass — all HUD overlays
        push_heat_bar / shield_pips / queue_glyphs / status_badges
   8. push_view_angle_overlay(out, view_angle_rad)
   // End-state overlays are NOT pushed here — the bin drives them.
```

**End-state overlays moved out of `compose_scene_tweened` in Phase 3.** Through
#45 the module auto-pushed `push_end_state_overlay(out, win_state(board))`,
but Phase 3's between-encounter screens introduced overlay states beyond what
`win_state(&Board)` can describe ("encounter complete, awaiting path
choice"). The bin now calls `push_end_state_overlay` /
`push_run_defeated_overlay` / `push_between_encounter_overlay` /
`push_salvage_hud` directly after the compose call.

---

## The camera-revolves morph

The `view_angle_rad: f32` parameter drives a unified rotation model across
ships and parallax. The formula appears verbatim in both the silhouette code
and the parallax code:

```
silhouette total_h    = height × cos(θ) + depth_dim × sin(θ)
back-wall vertical    = (canvas above lane) × cos(θ)
floor vertical        = (canvas below lane) × sin(θ)
chevron alpha (BowOn) = sin(θ)
side/top blend (PNG)  = sin(θ)
```

At `θ = 0` (pure side view):
- Silhouette is `height` tall (the side-view extent).
- Back wall fills the upper half; floor collapses to a line.
- Chevron is invisible; the silhouette's bow taper carries direction.

At `θ = π/2` (pure top-down):
- Silhouette is `depth_dim` tall (the top-down extent — beam for BowOn,
  length for Broadside).
- Floor fills the lower half; back wall collapses.
- Chevron is fully opaque; the silhouette is a rectangle with no taper, so
  the chevron is the only readable bow indicator.

At the default `θ = π/4` ≈ 45°, both terms contribute and both parallax
planes are partially visible.

The lane line never moves — it's the horizon between the two planes at every
angle. The silhouette is centered on the lane (half above, half below) so
the lane bisects the ship vertically at every angle.

---

## The starfield: hud.rs LCG, not atlas cells

Two atlas cells exist for stars (`PARALLAX_FAR_STARS`, `PARALLAX_MID_STARS`)
but `hud.rs` does not reference them. The actual on-screen starfield is
painted per-frame via a **Wang-hash-based LCG**:

```rust
fn wang_hash(mut x: u32) -> u32 {
    x = (x ^ 61).wrapping_mul(0x27D4_EB2D);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x
}

fn lcg_canvas_pos(seed: u32, rect: [f32; 4]) -> (f32, f32) { ... }
fn lcg_unit(seed: u32) -> f32 { ... }
```

`push_parallax` calls `lcg_canvas_pos(i ^ MAGIC_CONSTANT, sky_band)` for 60
far stars and 24 mid stars; alpha varies via `lcg_unit(i ^ DIFFERENT_MAGIC)`
so each star twinkles slightly without animation.

**Why Wang hash and not `rand`?**

1. **Determinism.** The same seed always produces the same star positions.
   Visual-regression tests that compare rendered frames don't need to seed
   anything; the hash *is* the seed.
2. **Zero-dep.** No `rand` crate; no per-frame RNG state. The seed is the
   input parameter and the function is pure.

The Rust functions are named `lcg_*` for brevity but the underlying primitive
is `wang_hash` (Thomas Wang's variant) — not a classical linear-congruential
generator. The naming is sloppy; the technique is correct.

**The two starfield atlas cells are vestigial.** Renderer flagged this as a
future cleanup. Three options exist (remove the cells, document the
divergence, switch the starfield to atlas-tiled) — option 2 is current state
and the documentation reflects that.

---

## The inline 5×7 bitmap font

`push_glyph_5x7(out, ch, x, y, pixel, color)` is a hand-rolled `match` arm
over the character literal returning `[u8; 7]` of 5-bit rows. Each lit bit
emits one SOLID_WHITE atlas-sampled quad at `pixel × pixel` size.

**Supported characters:** A, C, D, E, F, G, I, L, M, N, O, P, R, S, T, U, V,
Y, 0–9, `-`, `:`, space. Sparse by design — only what overlay banners need.
Unknown characters render as blank (5×7 of zeros) without error.

**Pixel sizes by use:**

| Use                          | `pixel` | Effective glyph size |
|------------------------------|--------:|---------------------|
| Title banners (DEFEATED)     | 5.0     | 25×35 px            |
| Encounter-complete banner    | 3.0     | 15×21 px            |
| Sub-banners (RESTART text)   | 2.5     | 12.5×17.5 px        |
| Salvage HUD counter          | 2.0     | 10×14 px            |

**Why inline, not atlas-packed?**

1. **Variable size.** Different banners want different `pixel` scales; atlas-
   packed would force one canonical size + bilinear filtering or per-size
   atlas slots.
2. **Sparse character set.** ~30 characters total. Atlas-packing would consume
   half the atlas grid for what fits cleanly in a `match` arm.
3. **Build-time changes.** Adjusting a glyph or adding a character is a
   one-line code change. No atlas regeneration friction.

To add a character: extend the `match` arm in `push_glyph_5x7`. No test pin
on font coverage; the symptom of a missing glyph is "banner has gaps."

---

## Per-ship HUD overlays

Five helpers, all calling `ship_bbox` to size against the current silhouette
and all taking `visual_cell: f32` so they ride along with the tween. Drawn in
the second per-ship pass of `compose_scene_tweened`:

| Helper              | What it draws                                              |
|---------------------|-----------------------------------------------------------|
| `push_heat_bar`     | Horizontal bar above ship: fill ratio = heat/heat_max. Lockout-red when `locked_out`. |
| `push_shield_pips`  | Small gold pips below the heat bar, one per active shield charge. |
| `push_queue_glyphs` | Stack of archetype glyphs above ship, one per queued action; bottom = next to fire. |
| `push_status_badges`| Atlas status badges drawn at fixed offsets; multiple stack horizontally. |
| (chevron, inside `push_ship`) | Bow direction overlay. Alpha = `sin(view_angle)` — invisible side-on, full top-down. |

All five use `fractional_cell_to_screen` with the tweened cell, so when a
ship moves between cells the HUD overlays slide with the silhouette rather
than snapping.

---

## End-state and between-encounter overlays

The bin pushes one of these on top of `compose_scene_tweened`'s output based
on its current demo state:

| Overlay                              | When                                                |
|--------------------------------------|-----------------------------------------------------|
| `push_end_state_overlay(state)`      | Phase-1 single-encounter win/lose.                  |
| `push_run_defeated_overlay(salvage)` | Phase-3 run defeat (DEFEATED + total salvage).      |
| `push_between_encounter_overlay(...)`| `EncounterComplete` or `RunComplete` between-encounter. |
| `push_salvage_hud(salvage)`          | Always during `Playing` state — top-right counter.  |

`BetweenEncounterChoice` is a 2-variant enum with carried state:

- `EncounterComplete { sector_idx: usize, salvage: u32 }` — banner shows
  `"ENCOUNTER COMPLETE - SECTOR N"` (where `N = sector_idx + 1`) plus a
  `"SALVAGE: N"` row and the 3-choice row (`1 REPAIR  2 UPGRADE  3 CONTINUE`).
- `RunComplete { salvage: u32 }` — campaign-end overlay; surfaces `salvage`
  as `"TOTAL SALVAGE: N"`.

`win_state(&Board)` derives `Victory` / `Defeat` / `Playing` from the board.
Note: `RunComplete` in `BetweenEncounterChoice` is **distinct** from
`WinState::Victory` — Victory fires on any single encounter win;
`RunComplete` is the campaign-end overlay only.

**`push_run_defeated_overlay(out, salvage)` is the bin's `DemoState::RunDefeated`
path** — distinct from Phase-1's `push_end_state_overlay(out, WinState::Defeat)`,
which is still public surface but no longer touched by the bin's run-defeat
flow since salvage surfacing landed (#88/#89). The older overlay remains
available for callers that don't carry salvage state.

---

## Procedural silhouettes vs textured PNGs

`push_ship` dispatches one of two paths based on whether the `SpriteRegistry`
has both `side` and `top` PNGs registered for the ship's `class_stance`:

| Path                     | When                                       | Emits                              |
|--------------------------|--------------------------------------------|------------------------------------|
| Textured PNG             | `sprites.has_pair(class, stance)`          | `TexturedShipInstance`             |
| Procedural silhouette    | otherwise                                  | One filled polygon + 4 edge sprites|

The procedural path uses `push_bow_on_silhouette` (5-vertex polygon with
triangular bow taper that collapses with `cos(view_angle)`) or
`push_broadside_silhouette` (stubbier rectangle, no taper).

**The procedural path always emits a chevron overlay** when
`sin(view_angle) > 0.05`. **The textured path skips the chevron** because the
painted PNGs own bow direction via sprite asymmetry. Heat bars / shield pips
/ queue glyphs / status badges draw on top regardless of which path was
taken.

---

## Cross-references

- **Atlas glyphs:** [`src/atlas.rs`](atlas.md). All non-text sprites
  (parallax patches, queue glyphs, status badges, telegraph icons, chevron,
  ordnance) sample from the atlas. SOLID_WHITE is the workhorse for tinted-
  quad paths.
- **Pixel-space transforms:** [`src/perspective.rs`](perspective.md).
  `cell_to_screen` and `fractional_cell_to_screen` are the only screen-space
  primitives this file uses.
- **DrawCommand consumer:** [`src/gfx.rs`](gfx.md). `Gfx::render` consumes
  the `Vec<DrawCommand>` this module produces.
- **Texture loader:** [`src/sprites.rs`](../LINE_BY_LINE.md#srcspritesrs)
  (pending walkthrough). Loads PNG ships for the textured-quad path.
- **Driving the view angle:** [`src/bin/broadside.rs`](../LINE_BY_LINE.md#srcbinbroadsidesrs)
  (pending). The bin owns the `view_angle_rad` scrubber state and passes it
  into every `compose_scene_tweened` call.
- **Canonical visual reference:** [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md)
  for the silhouette dimensions table and the side/top blend math.
- **Domain terms:** *Range band*, *Bow-on / Broadside*, *Hull zone*, *Heat
  lockout* in the [glossary](../GLOSSARY.md).
