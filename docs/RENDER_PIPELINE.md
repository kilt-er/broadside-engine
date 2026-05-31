# Broadside — Ship Render Pipeline (Decision Record)

*Status: adopted, POC in flight. This is the design/decision record for how Broadside
ships are rendered. It supersedes the 2-sprite (side/top blend) approach described in
[`SPRITE_SPEC.md`](SPRITE_SPEC.md). Sources: `docs/BROADSIDE_RENDER_PIPELINE_HANDOFF.md`
and the working browser tool `docs/broadside-loft-editor.html`.*

---

## The decision, in one paragraph

Ships are rendered as **live 3D, styled to read as pixel art** — a low-poly hull
**lofted** from 2D profiles, drawn by an **orthographic ¾-view camera** into a **low-resolution
offscreen buffer** with nearest-neighbor sampling, then run through a **posterize** shader that
quantizes the result into a few flat color bands. This is the Dead Cells / HD-2D approach: the
*source* is 3D, the *look* is limited-palette 2D. It replaces the previous plan of painting
side and top PNG sprites per ship and blending between them as the camera angle scrubs.

### The one correction every implementer must read first

> **Bruce chose CONTINUOUS LIVE MOTION, not discrete frame-stepping.** The handoff doc
> (`BROADSIDE_RENDER_PIPELINE_HANDOFF.md`) repeatedly frames the goal as "discrete
> frame-stepped 3D" / "stop-motion look" / "step yaw/pose to discrete values" (its lines
> 7-8, 78-79, 137-141). **That framing is superseded.** The ship renders its *actual pose
> every frame* — smooth yaw as it turns, smooth pitch as the camera scrubs — exactly like
> the loft editor's own preview loop does (`requestAnimationFrame` → `setCam()` →
> render, every frame, with continuous pointer-driven yaw/pitch; see
> `broadside-loft-editor.html:578-594`). There is **no sprite interpolation and no
> frame-stepping**. Do not port the frame-stepped model — render the live pose.
>
> Why the handoff said otherwise: frame-stepping was floated as a *simplification* ("simpler
> than smooth motion, not harder"). Bruce decided the continuous render reads better and is
> worth the (small) extra cost — it's the same per-frame "model + pose + camera → posterized
> buffer" operation either way; continuous just feeds it a smoothly-varying pose instead of
> snapped values.

---

## The 4-stage pipeline

This is the sequence to reproduce in the Rust + wgpu engine. The loft editor implements all
four; the reference function names below point at `broadside-loft-editor.html`.

### Stage 1 — Loft the hull (pure math; ports almost line-for-line)

The hull is built by **lofting**: a 2D cross-section profile is swept along the ship's length,
scaled at each station by a top-down plan outline, with an optional per-station height
multiplier. Three editable inputs drive it:

- **PLAN** — `[x (0..1 along length, prow=1), halfWidth (0..1)]` points (the top-down outline).
- **SECTION** — `[z (0..1 half-width), y (-1..1 height)]` points, ordered top-dorsal → chine →
  belly (the cross-section).
- **HEIGHTPROF** (optional) — a per-x height multiplier, traced from an imported side-view
  silhouette; defaults to flat `1.0` when no image is imported.

Construction (`buildHull`, `broadside-loft-editor.html:442`; section sampling
`sampleSection:437`, height `sampleHeightProf:289`):

1. Turn each PLAN point into a **station** at world x, carrying its half-width and a
   HEIGHTPROF-sampled height multiplier (`:446`).
2. At each station build a **ring** of vertices: sample SECTION at `SECN` steps for the right
   side (top→belly), then mirror back (belly→top) for the left, skipping the duplicate
   end points. Each ring vertex is `[station.x, sectionY · H · heightMult, sectionZ ·
   halfWidth]` (`ringPts`, `:451-459`).
3. **Stitch consecutive rings** into triangles — two tris per quad around the loop between
   ring *i* and ring *i+1* (`:463-464`).
4. Compute normals. The tool uses `computeVertexNormals` (`:467`); flat/face normals also
   suit the faceted low-poly look.

The default profiles produce a Star Destroyer "dagger": needle prow widening to a broad flat
stern. **Attached parts** are simple primitives positioned in ship space, layered on the
lofted hull (`rebuild`, `:471-526`): a stepped dorsal command tower + bridge + sensor domes
near the stern; a cluster of glowing engine bells in the stern face; a spinal prow gun;
broadside battery blisters along both chines; and scattered greeble boxes whose density implies
hull scale.

**Port note:** this stage is pure arithmetic — it's a direct Rust port producing vertex +
index buffers once at load. The engine's `ShipDesign` serde type (architect, task #110) is
being defined to carry exactly these profiles + settings so the engine can load a design the
loft editor saved as `.json`.

### Stage 2 — Orthographic ¾ camera

- **Orthographic** projection (not perspective) — keeps the clean flat-pixel read.
- Fixed **pitch** (look-down angle, ~26-32° default; the tool defaults to 26°).
- **Yaw** rotates the ship to its stance. The tool's four canonical stance snaps
  (`broadside-loft-editor.html:123-126`): `right` ≈ 28° and `left` ≈ 152° are bow-on (long axis
  across the screen, 180° apart); `fore` ≈ 118° and `aft` ≈ 298° are broadside (hull yawed 90°,
  flanks bearing on the lane).
- Camera placement (`setCam`, `:562-568`): orbit position from yaw `y` and pitch `p` at radius
  `r` — `pos = (r·cos(p)·sin(y), r·sin(p), r·cos(p)·cos(y))`, `lookAt(0,0,0)`, with the ortho
  frustum sized from the target aspect and a zoom factor.

**Continuous, not snapped (see the correction above):** the four stance angles are *reference
anchors*, not the only renderable yaws. In-engine, the ship's actual orientation drives yaw
every frame, sweeping smoothly between stances as it turns.

### Stage 3 — Low-resolution offscreen target

- Render the 3D scene into an **offscreen texture** at the sprite's native resolution
  (the tool offers 120-480 px wide; **160×100 is the default**).
- **Nearest-neighbor** filtering, **no MSAA** (`WebGLRenderTarget` with
  `NearestFilter` min+mag, `broadside-loft-editor.html:528`). The pixel-art crispness comes
  from rendering *small* and upscaling with nearest-neighbor — it is a downsample, not
  hand-drawn pixels.
- The upscaled result is then drawn to screen. **In the engine this becomes "the ship is just a
  texture"** that the existing 2D compositor ([`hud.rs`](MODULES/hud.md) /
  [`gfx.rs`](MODULES/gfx.md)) blits into the lane like any other sprite source.

### Stage 4 — Posterize pass

A full-screen pass over the low-res texture (`postMat` ShaderMaterial,
`broadside-loft-editor.html:530-560`). Per pixel:

1. **Discard if alpha < 0.5** (`:544`) — keeps the background cut-out/transparent, so composited
   ships never carry a background color into the lane.
2. **Optional color grade** (`:546-554`): RGB→HSV, shift hue, scale saturation, back to RGB;
   multiply brightness; apply contrast around 0.5; apply gamma. Grade is applied **before**
   banding.
3. **Posterize** (`:556`): `q = floor(color · bands + 0.5) / bands` per channel, with `bands`
   typically 2-5 (the tool also offers "full" = 16).

The tool's shader is GLSL and **translates directly to WGSL** for wgpu.

### The render loop (proof of "continuous")

The tool's loop (`broadside-loft-editor.html:578-581`) is the canonical two-pass structure:

```
loop():
  setCam()                                  // recompute camera from current yaw/pitch
  render(scene, cam)   → offscreen rt        // stage 1-3: 3D hull into low-res target
  render(postScene, postCam) → surface       // stage 4: posterize the rt to screen
```

It runs **every animation frame**, and yaw/pitch update continuously from pointer drag
(`:587-591`). That per-frame "current pose → posterized buffer" cadence is exactly the engine's
target — just driven by the ship's live orientation instead of the mouse.

---

## Porting to Rust + wgpu

Already in hand (the wgpu engine, see [`gfx.rs`](MODULES/gfx.md)): device, queue, render
pipelines, shader compilation, surface presentation, and the virtual-res offscreen + integer
blit scaffold. Four pieces to add for the 3D ship path:

1. **Geometry upload** — port the loft math (Stage 1) to Rust; emit vertex + index buffers per
   ship, once at load. Pure arithmetic, direct port.
2. **Depth buffer + 3D pipeline** — *the one piece that may be new.* A solid 3D hull needs a
   depth texture + a pipeline with depth-test enabled so near faces occlude far ones. 2D sprite
   blitting runs `depth_stencil: None`. **Quick check:** grep the engine's wgpu code for a
   `RenderPipeline` built with `depth_stencil: Some(...)` — if none exists, the engine is 2D-only
   today and this is the new work (~a day in standard wgpu).
3. **Offscreen render target at sprite resolution** — render the ship into a small texture (e.g.
   160×100) instead of the surface. Straightforward in wgpu; the virtual-res scaffold already
   demonstrates offscreen targets.
4. **Posterize pass** — a second pipeline sampling the low-res texture and quantizing to the
   palette. WGSL port of the Stage 4 GLSL.

The 3D-rendered ship texture then slots into the existing 2D compositor as just another sprite
source — the lane, HUD, parallax, and overlay layers are unchanged.

---

## Phased adoption plan

1. **Standalone POC (in flight, task #109).** A self-contained Rust + wgpu program: port the
   loft math → render with the ortho ¾ camera to a low-res target → posterize → display in a
   window, rendering the **live continuous pose** (smooth yaw/pitch). Does not touch the main
   engine. The bar it must clear: *the continuous render reads well as pixel art in motion* (no
   shimmer/crawl that would have argued for frame-stepping).
2. **Prove it reads.** Evaluate the POC against the 2-sprite look. If continuous 3D wins
   (expected), proceed; the decision to go continuous rather than frame-stepped is validated
   here.
3. **Lift into the engine.** Move the loft module + the two shaders (3D hull pipeline +
   posterize) into Broadside behind the render feature, producing ship textures the compositor
   consumes.
4. **Load designs from the tool.** The engine reads the loft editor's saved `.json` via the
   `ShipDesign` serde type (architect, task #110) — profiles + settings + grade. Designing a new
   ship becomes: open the tool, loft it, save `.json`, drop it in `assets/`.
5. **Retire the 2D sprite path.** The side/top PNG pair + the blend in
   [`sprites.rs`](MODULES/sprites.md) / the compositor become legacy and are removed.

---

## Superseded / legacy

- **[`SPRITE_SPEC.md`](SPRITE_SPEC.md)** and the **side/top sprite-pair blend**
  ([`sprites.rs`](MODULES/sprites.md): `load_sprite_pair`, `mirror_horizontal`, `rotate_90_cw`,
  the `SpriteView::{Side,Top}` blend) describe the **previous** rendering approach. They remain
  accurate for the code that exists *today* and the demo still runs on them, but they are the
  legacy path — superseded by this pipeline once Phase 5 lands. New ship art should be authored
  as loft designs, not painted side/top PNGs.
- The **"discrete frame-stepped"** framing throughout
  `BROADSIDE_RENDER_PIPELINE_HANDOFF.md` is superseded by the continuous-motion decision
  recorded above. The handoff remains the canonical reference for the *pipeline math and shader
  code*; only its motion model is out of date.
