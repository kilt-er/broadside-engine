# `src/gfx.rs` — Module Companion

*A self-contained walkthrough of the wgpu render layer. The same content as the
[`gfx.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcgfxrs), but scoped:
this file assumes you only care about how a `Vec<DrawCommand>` becomes pixels on
screen. Read this if you are about to add a new sprite shape, tune a pipeline,
change the offscreen target, or debug a missing draw.*

**Source commit:** stabilized at `95b94a6` (animation tweens shipped, scene is
locked in).
**Mirrors:** Ported from `GameEngine/mvp/src/gfx.rs`; four called-out structural
deltas. See **Drift** below.
**Design anchor:** Task #7 (Slice A wgpu scaffold) + #28–#30 (atlas/hud/demo-board
slices) + #46 (tweens) + #58 (camera-revolves model) + #64 (sprite spec).

---

## Why this file exists

Broadside renders through a **two-pass virtual-resolution model**:

1. Everything draws into a fixed **1320×480 offscreen target**. Every game pixel
   is one texel here. Coordinates flow straight from `perspective::cell_to_screen`
   in y-down virtual pixels.
2. The offscreen is then **integer-scale-blitted to the swapchain**, centered with
   letterboxing. The largest scale that fits is used (1× on 1320×480 windows, 2×
   on 2640×960, etc.). Black bars fill the leftover area. On a window *smaller*
   than 1320×480 the scale is still clamped to 1, which means the offscreen
   extends past the visible swapchain — by design, we never downscale game pixels.

This means: **game art is pixel-art crisp regardless of window size**, and the
renderer has one fixed coordinate system to think about (1320×480 virtual pixels).
No per-frame projection math; the view uniform is set once at startup.

Three things to know up front:

1. **Capacity ceilings are hard and silent.** Sprites: 4096. Polygons: 256.
   Textured ships: 16. Overflow drops commands silently with a per-frame
   `log::warn!`. The symptom of "scene blows the cap" is "stuff stops appearing,"
   not a panic.
2. **One render pass per pipeline-batch in the offscreen, one blit pass to the
   swapchain.** No depth buffer; the draw-command list must be in back-to-front
   order. The `hud.rs` compositor is responsible for ordering.
3. **The view UBO is owned by SpritePipeline, borrowed by the other three.**
   Construction order in `Gfx::new` is sprites → polygons → textured-ships →
   blit. If you ever drop `SpritePipeline`, the others dangle.

---

## The frame loop (high level)

```
   hud::compose_scene(&board, ...) -> Vec<DrawCommand>    // back-to-front order
                          │
                          ▼
   Gfx::render(&commands):
     Phase 1 — collect-and-batch:
       walk commands once,
       split into contiguous (Sprite | Polygon | TexturedShip) batches,
       upload three instance buffers (sprites / polygons / ship corners),
       write per-ship BlendUniform for each textured ship,
       ensure per-ship bind group cached for each (slot, side_slug, top_slug).
     Phase 2 — encode:
       Pass 1: scene → offscreen target (Rgba8UnormSrgb, 1320×480)
         clear to deep-space ink (#080c14, pre-linear)
         for batch in batches:
           switch pipeline as variant changes
           draw(0..6, batch.start..batch.start+count)
       Pass 2: offscreen → swapchain
         clear to letterbox black
         blit pipeline + blit_uniform
         draw(0..6, 0..1) — single full-screen quad
     submit one encoder, present.
```

That's the entire hot path. Everything else (`Gfx::new`, the four pipeline
`::new`s, `update_blit_uniform`, ship-sprite loading) is setup or response to
window events.

---

## The four pipelines

Each pipeline is a `wgpu::RenderPipeline` plus its bind groups, instance buffer,
and any pipeline-specific resources. All four are built in `Gfx::new` in this
order — load-bearing because of view-UBO ownership:

### `SpritePipeline` — instanced rotated quads

| | |
|---|---|
| **Bound at** | `@group(0)`: view UBO, atlas texture, atlas sampler |
| **Vertex buffers** | Buffer 0: `QuadVertex` (6 unit-quad verts, static). Buffer 1: `SpriteInstance` (per-instance, dynamic). |
| **Instance shape** | `pos`, `half_size`, `color`, `uv_min`, `uv_max`, **`rotation_rad`** (the only per-instance rotation in the whole renderer) |
| **Target** | `OFFSCREEN_FORMAT = Rgba8UnormSrgb` with `ALPHA_BLENDING` |
| **Owns** | The shared view UBO. **This is load-bearing** — the other three pipelines borrow it. |
| **Capacity** | `MAX_SPRITES = 4096` |

The bread-and-butter pipeline. Used for HUD elements, parallax planes, ordnance
sprites, chevrons — anything that's a rectangle sampling from the procedural atlas.

### `PolygonPipeline` — instanced explicit-corner quads

| | |
|---|---|
| **Bound at** | `@group(0)`: view UBO (borrowed), atlas texture, atlas sampler |
| **Vertex buffers** | Just the per-instance `PolygonInstance` (no static quad buffer; the shader pulls corners from the instance via `vertex_index`) |
| **Instance shape** | Four explicit corners (`p0..p3`, CCW screen y-down), `color`, `uv_min`, `uv_max`. **No rotation field** — rotated polygons precompute rotated corners on the CPU. |
| **Target** | Same as SpritePipeline |
| **Capacity** | `MAX_POLYGONS = 256` |

For shapes that the rotation-around-center `SpriteInstance` can't represent without
pixel staircase — primarily the lane plate parallelogram and ship-face polygons.

### `TexturedShipPipeline` — per-ship side/top blend

| | |
|---|---|
| **Bound at** | `@group(0)`: view UBO (borrowed). `@group(1)`: per-ship `BlendUniform`, side texture, top texture, shared sampler. |
| **Vertex buffer** | Per-instance four explicit corners (no rotation), 32 bytes/instance |
| **Instance shape** | `p0..p3`, `blend_t: f32`, `side: SpriteSlug`, `top: SpriteSlug` |
| **Target** | Same as the other offscreen pipelines |
| **Capacity** | `MAX_TEXTURED_SHIPS = 16` |
| **Pre-allocated** | One `BlendUniform` buffer per slot (16 of them), pre-built at startup |

The view-angle blend. Fragment shader does `out = mix(side_px, top_px, blend_t)`
where `blend_t = sin(view_angle)`. At θ=0 you see only the side sprite; at θ=π/2
only the top; at θ=π/4 the top dominates ~70/30 (the SPRITE_SPEC notes this is
intentional — the camera is already looking more down than across).

**Each textured ship gets its own draw call.** Same `(side, top)` slug pair at
different slots cannot share a bind group because each slot has its own pre-
allocated `BlendUniform` buffer.

### `BlitPipeline` — integer-scale offscreen → swapchain

| | |
|---|---|
| **Bound at** | `@group(0)`: `BlitUniform { ndc_min, ndc_max }`, the offscreen texture, a nearest-filtering sampler |
| **Vertex buffer** | None — the shader generates a six-vertex quad from `vertex_index` |
| **Target** | The swapchain's sRGB format |
| **Capacity** | Always one quad per frame |

The only pipeline that touches the swapchain. Samples the 1320×480 offscreen with
nearest-neighbor filtering and draws it at the largest integer scale that fits the
window, centered with letterboxing.

---

## The `DrawCommand` enum — the hud↔gfx contract

```rust
#[derive(Copy, Clone, Debug)]
pub enum DrawCommand {
    Sprite(SpriteInstance),
    Polygon(PolygonInstance),
    TexturedShip(TexturedShipInstance),
}
```

`hud::compose_scene` produces a `Vec<DrawCommand>` in back-to-front order.
`Gfx::render` consumes it. The enum is `Copy`, which is why `TexturedShipInstance`
uses `SpriteSlug` (inline 32-byte storage) instead of `String` for the texture
identifiers.

**Batching policy:** contiguous same-variant runs become single GPU draws. Pipeline
switches between variants are explicit pipeline rebinds. `TexturedShip` always
batches at one — each ship has its own bind group, no draw-call merging possible.

---

## The capacity-ceiling story

All three caps share the same overflow behavior: silent drop + per-frame `log::warn!`
on first overflow.

| Buffer        | Capacity |
|---------------|---------:|
| Sprites       | 4096     |
| Polygons      | 256      |
| Textured ships| 16       |

All three are **hard pre-allocated ceilings, not high-water marks**. The instance
buffers are sized once at startup in each pipeline's `::new` and never reallocate.
Bumping a constant only costs one extra VRAM allocation at startup.

The numbers were set in Slice A and have generous headroom over the current demo
board (~100–150 sprite instances + ~30 polygons per frame). **If a future scene
blows the cap, the symptom is "stuff stops appearing" — not "panic."** Worth
knowing if you're investigating a missing draw.

---

## Key startup decisions encoded in `Gfx::new`

| Decision | Where set | Why |
|---|---|---|
| Power preference: `HighPerformance` | line 586 | wgpu hint — prefer discrete GPU when present |
| Surface format: first sRGB option, else first available | line 607 | sRGB needed for the linear-blend correctness story below |
| Present mode: `AutoVsync` | line 618 | wgpu picks the platform-appropriate vsync mode |
| Offscreen format: `Rgba8UnormSrgb` | const at line 58 | sRGB so the atlas's per-cell colors composite correctly |
| Clear color pre-converted to linear | const at lines 63-68 | wgpu interprets `wgpu::Color` linearly when target is sRGB; if we passed `(0x08, 0x0c, 0x14, 0xff)` directly the actual on-screen ink would be wrong |
| Atlas sampler: nearest filter, ClampToEdge | line 673 | Pixel-art crispness; no bleed between adjacent atlas glyphs |
| Blit sampler: nearest filter, ClampToEdge | inside `BlitPipeline::new` | Same — the integer-scale blit must not blur |
| View UBO ownership: `SpritePipeline` | construction order at line 684 | First-built; others borrow `&sprites.view_ubo` |

---

## The letterbox math (`update_blit_uniform`)

Given current swapchain `(w, h)`:

```
   scale = max(1, min(w / VIRTUAL_W, h / VIRTUAL_H))   // integer
   scaled_w = scale × VIRTUAL_W
   scaled_h = scale × VIRTUAL_H
   offset_x = (w - scaled_w) / 2     // centering offset
   offset_y = (h - scaled_h) / 2
```

Convert `(offset, offset + scaled)` corners to NDC via `pixel/dim × 2 − 1` (y
flipped because NDC y-up vs swapchain y-down). Write to `BlitUniform`.

Recomputed on every `resize` / `reconfigure`. **Not per-frame** — the swapchain
dimensions only change on resize.

---

## Ship-sprite loader

`Gfx` carries two HashMaps:

- **`ship_sprites: HashMap<String, ShipSpriteEntry>`** — loaded PNG textures
  keyed by `<class>_<stance>_<view>` slug. Populated by
  `try_load_ship_sprites(asset_dir)` at startup.
- **`ship_bg_cache: HashMap<(u32, SpriteSlug, SpriteSlug), wgpu::BindGroup>`** —
  per-slot bind groups built on demand. **Keyed by `(slot_idx, side, top)`** —
  the `slot_idx` is in the key because each slot has its own pre-allocated
  `BlendUniform` buffer.

Cache invalidation: `ship_bg_cache.clear()` runs at the top of
`try_load_ship_sprites` because the underlying texture views may have changed.

Fallback path: if `ensure_ship_bind_group` finds either texture slug missing from
`ship_sprites`, it does **not** populate the cache entry. The render loop checks
the cache and skips that ship's textured draw, but the procedural polygons emitted
alongside (by `hud::push_ship`'s fallback path) stay visible. Missing art is a
non-crash degraded mode.

---

## Drift from `GameEngine/mvp/src/gfx.rs`

Four called-out structural deltas (quoted from the module rustdoc):

1. **Virtual resolution: 1320×480** with pixel coords flowing straight from
   `perspective::cell_to_screen`. The source engine used an NDC half-extent world.
2. **Y-down convention** throughout — the vertex shader does
   `ndc_y = 1.0 - pixel.y * view.px_to_ndc.y`. Matches `perspective` and SPRITE_SPEC.
3. **Procedural atlas from `crate::atlas`** rather than the source's humanoid set.
4. **Per-instance `rotation_rad` on `SpriteInstance`**, added vs the source so
   axis-aligned HUD and lane-aligned sprites share one pipeline.

**New decisions for the Broadside port:**

- Hard capacity ceilings with silent truncation (4096 / 256 / 16).
- View UBO ownership on `SpritePipeline` (construction-order coupling).
- `ship_bg_cache` keyed on `(slot_idx, side, top)` not `(side, top)`.
- `BlitPipeline` is the only pipeline touching the swapchain's sRGB format.
- Pre-converted-linear `CLEAR` color (Slice-A papercut).
- No depth buffer — painter's algorithm by construction.
- No `#[cfg(test)]` module in `gfx.rs` itself; coverage via `atlas.rs` helpers +
  visual confirmation + procedural-fallback safety net.

---

## Render modes — grid pitch (`G`) / grid mode (`T`) / ship tilt (#139/#140/#142)

`gfx.rs` owns three process-global atomics that re-pitch the board at runtime (so every
projector call site shares one value). **Two control axes:**

- **`G` — grid PITCH step** (`cycle_grid_pitch`, `grid_pitch_t() ∈ [0,1]`, `GRID_PITCH_STEPS = 8`):
  the *amount* of pitch, chase-cam (`0`) → near-top-down.
- **`T` — grid MODE** (`cycle_grid_mode`, `GRID_MODES = 3`): which projection the pitch arc
  feeds — **0 DRAWBRIDGE** (`ProjectorConfig::with_pitch`, constant footprint, balloons),
  **1 STRETCH-CURVED** (`with_stretch`, stretches to a uniform top-down square, curved
  columns), **2 STRETCH-STRAIGHT** (`with_stretch_straight`, same stretch with straight
  columns). `grid_mode_tag()` → `""`/`"STRETCH"`/`"STRAIGHT"`; `grid_stretch_on()` /
  `grid_stretch_straight()` are back-compat shims for the headless `capture` env.

At pitch step 0 all three modes reduce to the perspective base, so the default frame is
byte-identical (the no-regression gate). The bin's `scene_projector()` reads both and picks
the `with_*` projection for all board-space draws.

**Ship-plane tilt** (`loft_pitch_deg`): the 3-D hulls must stay parallel to the raising grid
plane, so the loft-camera pitch lerps from `loft_gpu::CAMERA_PITCH_DEG` (`20°`, chase-cam) to
`LOFT_PITCH_TOPDOWN_DEG` (`82°`) off `grid_pitch_t()` — one global, independent of grid mode,
parameterized into `loft_gpu::render_ship_lit_framed`. The live player + enemy ships are the
real Aegis **GLB mesh** via [`mesh_import`](mesh_import.md) rendered by the loft pass (enemies
tinted red), NOT sprites — the baked 15-facing wheel is the inactive fallback (see the
[v5 render contract](../BROADSIDE_RENDER_CONTRACT.md)). Full walkthrough:
[Render modes in LINE_BY_LINE](../LINE_BY_LINE.md#render-modes--grid-pitch-g--grid-mode-t--ship-tilt-139140142).

---

## Tests

`gfx.rs` itself has no `#[cfg(test)]` module — GPU pipelines are tested via
integration runs (the demo binary) plus visual confirmation. The 14 inline tests
referenced in adjacent module surface are in
[`src/atlas.rs`](atlas.md) (still pending).

Coverage strategy:
- **Atlas UV math:** tested in `atlas.rs`.
- **Pipeline construction:** if `Gfx::new` panics, the demo binary won't start —
  smoke-tested every time someone runs the renderer.
- **Per-frame draw dispatch:** validated visually + by the
  `procedural-fallback-on-missing-texture` safety net.
- **Letterbox math:** no automated test today; relies on window-resize playtest.

---

## Cross-references

- **Pixel-space transforms:** [`src/perspective.rs`](perspective.md).
- **Sprite atlas:** [`src/atlas.rs`](atlas.md) (pending). The atlas is procedurally
  generated at startup; layout documented in [`SPRITE_SPEC.md`](../SPRITE_SPEC.md).
- **Scene composition:** [`src/hud.rs`](hud.md) (pending). Hud emits the
  `Vec<DrawCommand>` `Gfx::render` consumes.
- **Window + event loop:** [`src/bin/broadside.rs`](../LINE_BY_LINE.md#srcbinbroadsidesrs)
  (pending). Owns the winit `ApplicationHandler` that drives `Gfx::render` each
  frame.
- **Sprite spec:** [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) — canvas
  dimensions, ship bbox vs view-angle math, PNG filename conventions, atlas slot
  layout.
- **Design anchors:** tasks #7, #28–#30, #46, #58, #64.
