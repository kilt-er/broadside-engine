# Broadside — Ship Render Pipeline: Handoff for Claude Code

## Purpose of this document
A browser tool (`broadside-loft-editor.html`) was built to design capital-ship
sprites by lofting a low-poly 3D hull and rendering it as crisp ¾-view pixel art.
The goal now is to port the **rendering technique** into the from-scratch Rust +
wgpu Broadside engine so ships can be rendered as **discrete frame-stepped 3D**
(stop-motion look) rather than interpolated 2D sprites.

This document summarizes what the tool does so the technique can be reimplemented
natively. **The HTML file does not port directly — the *pipeline concept* and two
small pieces of math/shader code are what transfer.**

---

## The pipeline (4 stages)

The tool produces its look through this sequence. This is the thing to reproduce
in-engine:

1. **Low-poly 3D ship model** — a lofted hull plus attached superstructure,
   engines, guns, and greeble detail.
2. **Orthographic camera at a fixed ¾ pitch** — looks down on the ship from
   above-and-to-the-side. Yaw rotates the ship; pitch is the look-down angle.
3. **Render to a low-resolution offscreen buffer** with **nearest-neighbor**
   sampling and **no anti-aliasing** (e.g. 160×100). This is what creates the
   pixel-art crispness — it's a downsample, not hand-drawn pixels.
4. **Posterize pass** — a fragment shader quantizes the rendered colors into a
   small number of flat bands (2–5 tones) for the limited-palette look, after an
   optional color-grade (hue/sat/brightness/contrast/gamma).

Reference touchstone: this is the same general approach as Dead Cells / HD-2D
titles — 3D source, rendered to look like 2D pixel art.

---

## Stage 1 detail — the loft (pure math, ports almost line-for-line)

The hull is built by **lofting**: a 2D cross-section profile is swept along the
ship's length, scaled at each station by a 2D plan (top-down width) outline.

Two editable profiles drive it:
- **PLAN**: array of `[x (0..1 along length, prow=1), halfWidth (0..1)]`
- **SECTION**: array of `[z (0..1 half-width), y (-1..1 height)]`, ordered
  top-dorsal → chine → belly.
- **HEIGHTPROF** (optional): per-x height multiplier, traced from an imported
  side-view silhouette.

Construction algorithm (this is the part to port to Rust):
- For each PLAN point, make a **station** at world x, with that half-width and a
  height multiplier sampled from HEIGHTPROF.
- At each station, build a **ring** of vertices by sampling SECTION at N steps for
  the right side (top→belly), then mirroring back (belly→top) for the left side.
  Each ring vertex = `[station.x, sectionY * H * heightMult, sectionZ * halfWidth]`.
- **Stitch consecutive rings** into triangles (two tris per quad between ring i and
  ring i+1, around the loop).
- Compute vertex normals (or use flat/face normals — flat shading is what the tool
  uses and it suits the faceted low-poly look).

Default profiles produce a Star Destroyer "dagger": needle prow widening to a
broad flat stern, long and shallow.

Attached parts (layered on the lofted hull, all simple primitives positioned in
ship space): stepped command tower + bridge + sensor domes near the stern;
cluster of glowing engine bells in the broad stern face; spinal prow gun;
broadside battery blisters along both chines; scattered small greeble boxes
(windows/panels) whose density implies hull scale.

---

## Stage 2 detail — camera

- **Orthographic** projection (not perspective) — keeps the clean flat-pixel read.
- Fixed **pitch** (look-down angle, ~26–32° default). Adjustable.
- **Yaw** rotates the ship to the stance. Four canonical stances:
  - `right` / `left` = bow-on (long axis across screen), 180° apart.
  - `fore` / `aft` = broadside (hull yawed 90°, both flanks bear on the lane).
- For frame-stepped animation: step yaw/pose to **discrete** values and render each
  — no interpolation needed. This is simpler than smooth motion, not harder.

---

## Stage 3 detail — low-res target

- Render the 3D scene into an **offscreen texture** at the sprite's native
  resolution (120–480 px wide options in the tool; 160×100 is the default).
- Use **nearest-neighbor** filtering, **no MSAA**. The crispness comes from
  rendering small and upscaling with nearest-neighbor.
- The final result is then drawn (upscaled, nearest-neighbor) to the screen — in
  the engine, this becomes "the ship is just a texture" that the existing 2D
  compositor blits into the lane.

---

## Stage 4 detail — posterize shader

A full-screen pass over the low-res texture. Per pixel:
1. Discard if alpha < 0.5 (keeps transparent background — important: exported/
   composited sprites must stay cut-out, not on a background color).
2. Optional color grade: convert to HSV, shift hue, scale saturation; back to RGB;
   multiply brightness; apply contrast around 0.5; apply gamma.
3. **Posterize**: `q = floor(color * bands + 0.5) / bands` per channel.

The tool's shader is GLSL. **It translates directly to WGSL** for wgpu. Grade is
applied *before* banding (grade the color, then quantize).

---

## Porting notes for the Rust + wgpu engine

What's already in hand (wgpu engine): device, queue, render pipelines, shader
compilation, surface presentation.

Four pieces to add for a 3D ship render path:

1. **Geometry upload** — port the loft math (above) into Rust; output vertex +
   index buffers per ship. Done once at load. Pure arithmetic, direct port.

2. **Depth buffer + 3D pipeline** — *the one piece that may be new.* 2D sprite
   blitting typically runs with `depth_stencil: None`. A solid 3D hull needs a
   depth texture + a pipeline with depth-test enabled so near faces occlude far
   ones. Standard wgpu; ~a day if it doesn't exist yet.
   - **Quick check whether it already exists:** search the engine's wgpu code for
     any `RenderPipeline` created with `depth_stencil: Some(...)`. If present, 3D
     depth drawing already happens somewhere. If every pipeline is
     `depth_stencil: None`, the engine is 2D-only today and this is the new work.

3. **Offscreen render target at sprite resolution** — render the ship into a small
   texture (e.g. 160×100) instead of the surface. Straightforward in wgpu.

4. **Posterize pass** — second pipeline sampling the low-res texture, quantizing to
   the palette. WGSL port of the tool's GLSL fragment shader.

Then the 3D-rendered ship texture slots into the existing 2D compositor as just
another sprite source.

### Decision already made
Render **live 3D in-engine** (not pre-baked sprite sheets), specifically to get
**discrete frame-stepped motion**. Frame-stepping makes the renderer simpler: it
only needs "given model + pose + camera → one posterized pixel buffer." No
tweening, interpolation, or sub-frame timing.

### Suggested first milestone (proof-of-concept)
A standalone Rust + wgpu program: generate one lofted ship via the ported loft
math → render with the ortho ¾ camera to a low-res target → posterize → display
spinning through discrete yaw steps in a window. Self-contained, doesn't touch the
main engine. If it reads right, lift the loft module + the two shaders into
Broadside.

---

## Source artifact
`broadside-loft-editor.html` — the working browser tool. Open it and read the
**Docs tab** (sections 1–11) for the full feature/behavior reference. Key code to
extract for the port:
- the loft construction (`buildHull` / `sampleSection` / `sampleHeightProf`)
- the posterize fragment shader (in the `postMat` ShaderMaterial)
- the orthographic camera setup (`setCam`) and the four stance yaw angles
  (right ≈ 28°, left ≈ 152°, fore ≈ 118°, aft ≈ 298°).

## Current tool feature set (for reference / parity if useful)
Loft editing (plan + section, drag/add/delete points); side-view image import
(auto-trace height profile + faded tracing underlay; height+length only — width and
cross-section are user-supplied); capital-ship controls (length stretch, height
scale, greeble density, superstructure toggle); orbit/zoom + 4 stance snaps;
¾ pitch; steerable key light (azimuth/elevation); palette bands (2–5/full);
color grade (hue/sat/brightness/contrast/gamma); background picker; PNG export
(current view or all four stances, 1–8× scale, transparent bg); save/load design
as JSON (profiles + settings + grade; the import *image* is not stored, only its
traced profile).

## Known limits / not yet built
Free-form vertex editing; import orientation toggle (prow-left vs -right);
sprite-sheet (packed) export; live multi-user collaboration (needs a backend).
