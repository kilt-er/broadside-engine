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

> **v2 realtime-3D update (#70–#75).** The LIVE player render no longer poses the hull
> from `ShipPose`/`Orientation`. It is now a **flat ground-plane chase-cam billboard**
> driven by the player's `Dir4` **facing**: `gfx` computes a single ground-yaw with
> [`chase_cam_ground_yaw_deg`](#realtime-3d-chase-cam-the-live-player-render-7075) (below) and
> feeds it straight to `render_ship`. The `MODEL_YAW_*` stance consts and the
> `ShipPose` orientation tween described in the next two sections are the **pre-realtime-3D**
> path; they survive only in this module's own unit tests (and `ShipPose` still exists as
> animation scaffold `gfx` keeps per-ship), but they no longer choose the live player yaw.
> See the **Drift** note at the end and the new realtime-3D section. The GLB Aegis reaches
> this pipeline through [`mesh_import.rs`](mesh_import.md) → `upload_imported`; the baked
> 15-facing sprite sheet is the **fallback** (render-contract v5).

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
  `REORIENT_SECS` (**0.28** since #52 — a crisp ~quarter-second swing; the tween takes the
  shortest path between stance yaws, and the player's reorient is now a clean 90° bow-on↔
  broadside toggle with no 180° over-spin).
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

## Camera + matrix math

`camera_view_proj_zoom(yaw_rad, pitch_rad, aspect, target_y, half)` (src/loft_gpu.rs:1044) —
the orthographic ¾ camera, port of the editor's `setCam`: eye orbits at radius 30 from
yaw/pitch around the look-AT point `(0, target_y, 0)` (orbiting the *target*, not the world
origin, so a hull whose mass doesn't straddle `y=0` still frames centred), ortho frustum sized
from `half`. The gameplay framing passes `half = HALF_EXTENT` (**7.0**, up from the POC's 5.0 —
it clears the broadside ship's vertical projection, #49) and `target_y = mesh.center_y`. The
fixed-zoom design call still holds: **one framing zoom across all ships preserves their TRUE
relative scale** (no per-ship fudge; bruce dials true scale at the asset source). `look_at`
(src/loft_gpu.rs:1138), `ortho`, `mul4`, `normalize3`, `identity4` are the column-major
right-handed matrix helpers (clip z 0..1 for wgpu). `camera_view_proj_is_finite`
(src/loft_gpu.rs:1308) is the smoke guard.

> **Drift (#73).** `camera_view_proj` (the *no-zoom* version this section once documented) now
> lives only in `src/bin/loft_poc.rs` (the spike), not in the engine. The engine's `render_ship`
> path goes through `camera_view_proj_zoom`. The note above is the current shape.

---

## Realtime-3D chase-cam: the live player render (#70–#75)

**Intent:** Render the player's Aegis as a real 3-D hull seated on the tactical grid, oriented
by its `Dir4` facing, with the bow aimed up-lane toward the **vanishing point** — the
"ship-from-behind" chase-cam read — while keeping the hull **flat on the ground plane** (Bruce's
hard requirement: no barrel-roll) and limited-palette posterized like everything else.

The whole live render path is **facing-driven**, and the camera-yaw computation lives in one
pure, CPU-tested function so the sign that "burned ~5 screenshot reviews" can never regress:

### `const CHASE_CAM_BASE_YAW_DEG` (src/loft_gpu.rs:1076) = `270.0`

The loft camera orbits about `+Y`; the hull's length runs along `+X` (prow `+X`, stern/engines
`−X`). `270°` (i.e. `−90`) puts the **stern toward the viewer with the bow up-lane** toward the
vanishing point — the stern-on chase view, which is the facing-`N` base case.

### `pub fn chase_cam_ground_yaw_deg(aim_at, facing_yaw_deg, cfg) -> f32` (src/loft_gpu.rs:1104)

**THE single source** for the player loft hull's ground-plane camera yaw, shared by the live
renderer ([`gfx.rs`](gfx.md):2050) **and** the deterministic bow gate, so verification tests the
REAL render path. It composes **three flat terms** (no roll — only the ground heading turns):

1. `CHASE_CAM_BASE_YAW_DEG` (270 = stern-on, bow up-lane).
2. `facing_yaw_deg` — the tactical-facing offset from
   [`hud::loft_facing_ground_yaw`](hud.md): `N = 0`, `E = +90`, `S = 180`, `W = −90`, so the
   four cardinals read as distinct flat poses.
3. The **lane-aim convergence** `psi` — so an off-centre ship's bow banks *toward* the vanishing
   point (converges with the lane) instead of pointing parallel to the screen. `alpha` is the
   screen angle from the cell centre straight up to the VP (`(vp.x − ax).atan2(ay − vp.y)`),
   then `psi = atan(tan(alpha)·sin(pitch))` lands the up-lane component on the VP under the
   chase pitch.

`aim_at` is the ship's **CELL-centre** screen point (`q.aim_at` = `q.center`, the cell centroid),
*not* the dragged-down hero quad — that keeps the convergence small and correct.

**The SIGN** (the regression magnet, now PINNED): decreasing the camera yaw from 270 banks the
bow LEFT on screen; increasing banks it RIGHT (verified against the real ortho camera). A ship
*right* of the VP (`aim_at.x > vp.x`) must bank LEFT toward the VP ⇒ the yaw must DECREASE; there
`alpha < 0` so `psi < 0`, hence the formula **ADDs** `psi` (`+psi`). The pre-#73 code used
`−psi`, which pushed off-centre bows *away* from the VP.

### The live blit (in `gfx.rs`, not this module)

`gfx` computes `base_yaw = chase_cam_ground_yaw_deg(q.aim_at, q.facing_yaw_deg, &cfg)`, renders
the hull into the shared loft target at that yaw via `render_ship`, then **blits the posterized
output onto the lane quad UN-rotated** — the nose-aim lives entirely in the 3-D yaw; the hull
stays flat on the grid. The fixed house key light relights the yawed hull in world space
automatically (the hull turns *in 3D*; it does not spin on screen).

### The live bow gate (the verification that was missing)

`live_loft_bow_points_correctly_all_facings_and_columns` (src/loft_gpu.rs:1354) is the
deterministic gate over **all 4 cardinals × columns 0/2/4**. It projects a hull-local bow point
(`+X`) and the hull centre through the **exact same ortho loft camera** (`camera_view_proj_zoom`
at `CAMERA_PITCH_DEG`/`HALF_EXTENT`, via the `bow_loft_ndc` helper) posed by the **same yaw
formula**, and asserts the projected bow sits:
- `N` → ABOVE centre (NDC +y = up-lane) **and** banked toward the VP-x at off-centre columns
  (col 0 → right, col 4 → left — never toward the screen edge: the `N`-convergence regression),
- `S` → BELOW (toward the camera), `E` → screen-RIGHT, `W` → screen-LEFT.

**Why this gate matters (the deep Drift below):** the earlier `#70` bow oracle tested
`projector::camera_perspective` — the scene-space **pinhole**, a *different* camera — so it passed
green while the live ortho bow pointed the wrong way at off-centre columns. The fix (commit
`f6208d0`) was both to correct the `+psi` sign and to rewrite the gate to replicate **this**
ortho loft camera + the blit's sign-preserving NDC→screen map. See
[`perspective.md`](perspective.md) for the scene-space "plan A" degenerate finding in full.

**Cross-references:** facing → `facing_yaw_deg` is [`hud::loft_facing_ground_yaw`](hud.md); the
yaw feeds `render_ship` here and the blit in [`gfx.md`](gfx.md); `vanishing_point` / `aim_at` cell
centre come from [`perspective.md`](perspective.md) (the projector); rotating the facing that
drives all of this is the [`resolve.md`](resolve.md) REORIENT-rotate arm.

---

## `fn imported_vertex_attrs(ship)` (src/loft_gpu.rs:928)

**Intent:** Expand an `ImportedShip`'s per-group materials into per-vertex albedo +
emissive (both parallel to `mesh.positions`): `colors[i]` = material base RGB,
`emissive[i]` = `[er, eg, eb, unlit-flag]`. Vertices outside every group, or with an
out-of-range material index, fall back to default hull grey + no emissive + lit. **Pure —
unit-tested headless**; `upload_imported` is the thin GPU wrapper. **Worked example:**
`imported_colors_expand_groups_and_fall_back_to_grey` (src/loft_gpu.rs:1026) — red lit
group, green unlit-glow group, ungrouped verts → grey.

`upload_imported_tinted(device, ship, tint)` (src/loft_gpu.rs:856) is a variant that multiplies
every vertex albedo by a per-channel `tint` (emissive untouched), used to recolour the shared CAD
hull a distinct hue for the player so it reads apart from the enemy fleet.

---

## Drift — the stance-yaw / `ShipPose` path vs. the live realtime-3D render

The **Constants + stance yaws** and **`struct ShipPose`** sections above describe the
*pre-realtime-3D* render model: a fixed camera, the ship's stance defined by rotating its model by
`MODEL_YAW_*` (fore `−28` / aft `−152` / broadside `−118`), and `ShipPose` tweening that
orientation yaw over `REORIENT_SECS`. That model was correct when a ship's render came from its
`Orientation`.

The v2 realtime-3D path (#70–#75) **superseded it for the LIVE player render**:

- The live yaw is now the **facing-driven ground-yaw** from `chase_cam_ground_yaw_deg` (above),
  computed in `gfx` from the player's `Dir4` facing + cell position, and passed straight to
  `render_ship`. The `MODEL_YAW_*` consts and `orientation_yaw_deg` are no longer consulted to
  pose the live player — they survive only in this module's own unit tests.
- `ShipPose` still **exists** as animation scaffold (`gfx` keeps a `HashMap<id, ShipPose>` and
  ticks `advance` for idle/redraw gating, and `ensure_loft_pose` still calls `reorient_to`), but
  its tweened *yaw* is not what reaches the live blit — the ground-yaw is. Treat the stance-yaw
  sections as historical context for the model, current for the *spike* (`loft_poc.rs`), and
  read the **Realtime-3D chase-cam** section for the live player.
- `render_ship` now also takes a `center_y` argument (the mesh's vertical centroid, fed to
  `camera_view_proj_zoom`'s `target_y`) — newer than the signature the `struct LoftGpu` section
  records. The in-engine docstring on `render_ship` (src/loft_gpu.rs:872) still says the yaw comes
  from `ShipPose::yaw_deg`; for the live player it actually comes from `chase_cam_ground_yaw_deg`.
