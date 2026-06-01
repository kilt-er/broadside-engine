# `src/loft_gpu.rs` — the in-engine loft render pipeline (GPU)

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/loft_gpu.rs`](../LINE_BY_LINE.md#srcloft_gpurs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **GPU side** of the render pivot — the in-engine lift of the validated
`loft_poc` spike. It takes a [`HullMesh`](loft.md) (from *either* producer —
[`loft.rs`](loft.md) or [`mesh_import.rs`](mesh_import.md)) and renders it as crisp
¾-view posterized pixel art, in two passes:

1. **Depth-tested 3D pass** — orthographic ¾ camera + flat Lambert (key + fill +
   ambient), per-vertex albedo, into a low-res offscreen colour + depth target. This
   is the **only depth-using pipeline in the engine** — the 2D compositor in
   [`gfx.rs`](gfx.md) stays `depth_stencil: None`, and the depth texture lives
   entirely inside this module.
2. **Posterize pass** — a WGSL port of the loft editor's GLSL frag (grade → quantize
   to `BANDS` → discard `a < 0.5`), nearest-sampled, producing an RGBA texture with a
   transparent cut-out background.

That posterized texture is what the existing `gfx` 2D compositor blits into the lane
via its `TexturedShip` path — **so the rest of the renderer never sees 3D or depth.**
This realizes Stages 2-4 of [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md) (Stage 1 is
[`loft.rs`](loft.md)). No TS analog. House style is locked engine-wide: `LOW_W`×`LOW_H`
(320×200) internal, `BANDS` (8).

### The continuous-motion decision, realized

The module docstring (src/loft_gpu.rs:19-30) is the concrete embodiment of
[`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)'s continuous-not-frame-stepped decision:
in-game the ship holds its **gameplay facing** — yaw comes from the ship's
`Orientation`, not the POC's demo auto-orbit. The only continuous motion is a
low-amplitude **idle** (bob/sway/roll so a resting ship reads as alive) plus an
**active reorient tween** that rotates yaw smoothly through real 3D when the ship flips
bow-on↔broadside — the headline win over sprite blending. Pitch is the existing
camera-angle scrubber, passed in.

---

## Constants + stance yaws (src/loft_gpu.rs:38–86)

`LOW_W`/`LOW_H` (320×200) and `BANDS` (8) — the locked house style. `CAMERA_AZIMUTH_DEG`
is **0** and the camera is **static** — stance is defined in ONE place, the *model* yaw,
so there's no two-place camera/model split to get the sign wrong. `CAMERA_PITCH_DEG`
(26°) owns the ¾ look-down.

The `MODEL_YAW_*` consts (src/loft_gpu.rs:74) are the **key correctness anchor**: the POC
orbited its *camera* to yaw P (fore 28 / aft 152 / broadside 118) over a fixed hull;
with the camera fixed at azimuth 0, rotating the *model* by −P gives the identical image
(orbiting camera +P about Y ≡ rotating world −P), so Fore = −28, Aft = −152, Broadside =
−118. The broadside −118° is the #37 fix — at model ±90 the hull was pure side-on (only
the ~4u beam showing → "too thin"); −118° swings its 12u length to the ¾ so it reads as
a real ship. `orientation_yaw_deg` (src/loft_gpu.rs:80) maps `Orientation` → base yaw.

---

## `struct ShipPose` (src/loft_gpu.rs:92)

**Intent:** Per-ship animated pose state the renderer keeps between frames — resting
`orientation`, an optional in-flight `tween` `(from, to, elapsed, dur)`, and an `idle_t`
phase. **Pure state + math, no GPU**, so it's unit-testable headless.

- `reorient_to(to)` (src/loft_gpu.rs:123) — begin a smooth reorient, tweening from the
  *current displayed* yaw (so mid-tween re-flips don't snap) to `to`'s base yaw over
  `REORIENT_SECS` (0.45).
- `advance(dt)` (src/loft_gpu.rs:138) — advance idle + any tween; clears the tween when
  elapsed ≥ dur.
- `yaw_deg_no_idle` (src/loft_gpu.rs:152) — base or `smoothstep`-eased tween yaw (the
  tween origin, so re-flips are continuous); `yaw_deg` (src/loft_gpu.rs:164) adds the
  low-amplitude idle roll; `idle_bob` (src/loft_gpu.rs:171) the vertical pixel nudge the
  caller adds to screen-y; `is_animating` (src/loft_gpu.rs:177) gates redraw requests.

**Worked examples:** `orientation_yaws_are_distinct_per_stance` (src/loft_gpu.rs:956,
pins the −28/−152/−118 magnitudes + the #37 broadside fix),
`reorient_tweens_then_settles` (src/loft_gpu.rs:983), `idle_advances_and_stays_bounded`
(src/loft_gpu.rs:1009).

---

## GPU layout + the uniform-size guards (src/loft_gpu.rs:201–247)

`Vertex` (src/loft_gpu.rs:203) is pos + flat normal + per-vertex albedo + emissive, each
`vec3` padded to a 16-byte slot for unambiguous std-layout / `bytemuck::Pod`. **`emissive.w`
doubles as the unlit flag** (1.0 = unlit/flat, 0.0 = Lambert) so no extra attribute is
needed. `SceneUniform` carries the two mat4s + light dirs + ambient. `PostUniform`
(src/loft_gpu.rs:231) is `bands` + **three scalar f32 pads** — deliberately *not* a
`vec3<f32>`, which under WGSL uniform layout would make the struct 32 bytes vs 16 and
trip wgpu's late-min-binding-size check (the "Encoder is invalid" trap fixed twice in the
POC/gfx history).

The `const _: () = assert!(size_of::<...>() == N)` guards (src/loft_gpu.rs:244-247) make
any Rust/WGSL struct-size mismatch a **hard compile error**, killing that whole
invalid-encoder bug class before it can reach the GPU — a standing rule for this
pipeline.

## The shaders (src/loft_gpu.rs:249–337)

`HULL_SHADER` — vertex transforms by `model`/`view_proj`; the fragment branches on
`emissive.w > 0.5` (unlit → flat `color + emissive`, clamped so posterize bands it
cleanly) else does flat Lambert (key + cool-tinted fill + ambient) and **adds emissive
after Lambert** so glow surfaces (canopy/gun/battery/engine) stay bright at any facing,
clamped into [0,1] so the posterize stays a banded glow not a white blowout.
`POST_SHADER` — a full-screen triangle, samples the low-res texture, **discards `a < 0.5`**
(preserves the cut-out so the compositor blits only the silhouette), and quantizes
`floor(c·bands + 0.5)/bands`.

---

## `struct LoftGpu` (src/loft_gpu.rs:342)

**Intent:** Owns the two pipelines + the offscreen targets (low-res scene colour, depth,
final posterized output) + the uniform buffers/bind groups. Produces a posterized RGBA
texture view per render that `gfx` feeds to its `TexturedShip` blit.

- `new(device)` (src/loft_gpu.rs:365) — build everything: the scene UBO + hull pipeline
  (depth-test `Less`, depth-write on, **no cull** so a closed loft/imported mesh can't
  show holes), the three `LOW_W`×`LOW_H` textures, and the posterize pipeline (nearest
  sampler, `depth_stencil: None`).
- `output_view` / `output_size` (src/loft_gpu.rs:622, 627) — the posterized output for
  the gfx compositor.
- `upload_hull(device, mesh, colors, emissive)` (src/loft_gpu.rs:641) — pack a `HullMesh`
  + parallel per-vertex albedo + emissive into a vertex buffer (empty colors → default
  hull grey; empty emissive → no glow + lit, the loft path's procedural hulls).
  **Separate from render** so the caller uploads once per ship design and re-renders every
  frame as the pose animates.
- `upload_imported(device, ship)` (src/loft_gpu.rs:674) — the CAD path: expand an
  [`ImportedShip`](mesh_import.md)'s per-group materials onto per-vertex albedo + emissive
  (via `imported_vertex_attrs`) and delegate to `upload_hull`. **Both geometry sources
  reach the GPU through one path.**
- `render_ship(queue, encoder, vbuf, vcount, yaw_deg)` (src/loft_gpu.rs:688) — the
  two-pass render. Camera is fixed (azimuth/pitch consts), the ship's stance comes from
  rotating its *model* by `yaw_deg` (from `ShipPose::yaw_deg`), so no camera args. Writes
  the scene + bands uniforms, records pass 1 (hull → low-res scene colour, transparent
  clear = cut-out, depth cleared to 1.0) then pass 2 (posterize → output, cut-out
  preserved via discard). Records into the caller's encoder; the caller submits.

**Cross-references:** consumes [`HullMesh`](loft.md) from both producers + the
[`ImportedShip`](mesh_import.md) materials; its output texture is blit by
[`gfx.rs`](gfx.md)'s `TexturedShip` path; `ShipPose` is driven by the ship's
[`Orientation`](types.md). Realizes Stages 2-4 of
[`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md).

---

## Camera + matrix math (src/loft_gpu.rs:804–919)

`camera_view_proj` (src/loft_gpu.rs:804) — orthographic ¾, port of the editor's `setCam`:
eye orbits at radius 30 from yaw/pitch, `look_at` origin, ortho frustum sized from
`HALF_EXTENT` (5.0). The `HALF_EXTENT` comment (src/loft_gpu.rs:812-818) records a real
design call: **one fixed framing zoom across all ships preserves their TRUE relative
scale** (the 7.75u CAD ship renders ~65% of the 12u dagger, as authored — no per-ship
fudge; bruce dials true scale at the asset source). `rotation_y` (src/loft_gpu.rs:840),
`look_at` (src/loft_gpu.rs:850), `ortho` (src/loft_gpu.rs:883), `mul4` (src/loft_gpu.rs:907),
`normalize3` are the column-major right-handed matrix helpers (clip z 0..1 for wgpu).
`camera_view_proj_is_finite` (src/loft_gpu.rs:1020) is the smoke guard.

## `fn imported_vertex_attrs(ship)` (src/loft_gpu.rs:928)

**Intent:** Expand an `ImportedShip`'s per-group materials into per-vertex albedo +
emissive (both parallel to `mesh.positions`): `colors[i]` = material base RGB,
`emissive[i]` = `[er, eg, eb, unlit-flag]`. Vertices outside every group, or with an
out-of-range material index, fall back to default hull grey + no emissive + lit. **Pure —
unit-tested headless**; `upload_imported` is the thin GPU wrapper. **Worked example:**
`imported_colors_expand_groups_and_fall_back_to_grey` (src/loft_gpu.rs:1026) — red lit
group, green unlit-glow group, ungrouped verts → grey.
