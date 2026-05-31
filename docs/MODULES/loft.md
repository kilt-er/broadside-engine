# `src/loft.rs` — pure-math hull lofting (`ShipDesign` → `HullMesh`)

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/loft.rs`](../LINE_BY_LINE.md#srcloftrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is **Stage 1 of the ship render pipeline** (see
[`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)): it turns a
[`ShipDesign`](ship_design.md)'s 2D profiles into a 3D triangle-soup mesh. A 2D
cross-section is swept along the ship's length, scaled at each station by the
top-down plan outline, with an optional per-station height multiplier — producing
the low-poly Star Destroyer "dagger" the loft editor makes by default.

The defining constraint: **no GPU here, on purpose.** This module is pure
arithmetic — no `wgpu`, no `feature = "render"` gate — so it runs and is unit-tested
headless on CI. The GPU side (uploading the mesh, the depth-tested 3D pass, the
posterize pass) lives in the renderer's `loft_gpu` module and *consumes* the
`HullMesh` this produces. Keeping the math standalone makes it testable,
deterministic, and camera-independent (same hull whatever the view does), so it's
safe to productionize **ahead of** the visual-POC verdict. It pairs with
[`ship_design.rs`](ship_design.md) (the data) — together they are the
design-data → geometry seam of the render pivot.

**Mirrors:** ported faithfully from the standalone POC's `loft` mod
(`src/bin/loft_poc.rs`), itself a line-for-line port of the loft editor's
`buildHull()` / `sampleSection()` (`docs/broadside-loft-editor.html`). The one
substantive change from the POC: where the POC hardcoded the dagger profiles as
`[f32;2]` consts, this module drives everything from a loaded `ShipDesign`, reusing
[`ship_design::Point2`](ship_design.md) so the editor's saved `.json` flows straight
through.

### Coordinate convention (matches the three.js source)

`x` = length (prow toward `+x`), `y` = height (dorsal `+y`), `z` = half-width
(port/starboard). Plan points are `[x (0..1 stern→prow), halfWidth]`; section points
`[z (0..1), y (-1..1)]` top→chine→belly; height profile `[x, heightMult]`.

---

## `const DEFAULT_SEC_N` + `struct LoftParams` (src/loft.rs:49, 55)

`DEFAULT_SEC_N = 10` is the section ring resolution (the editor's `SECN`); each ring
ends up with `2·sec_n − 2` vertices (right side top→belly, then the mirrored left
side, skipping the shared top/belly endpoints). `LoftParams { stretch, hscale, sec_n }`
scales the loft — `stretch`/`hscale` come from the design's `settings`, `sec_n`
defaults. `Default` is the editor's default state (`stretch 2.0`, `hscale 0.7`).

## `struct HullMesh` (src/loft.rs:84)

**Intent:** A lofted hull as a flat-shaded **triangle soup** — `positions` and
`normals` run in lockstep, three vertices per triangle, all three sharing that
face's normal (so the faceted low-poly look survives upload with **no index buffer
and no vertex-normal averaging**). Invariant: `positions.len() == normals.len()`,
both a multiple of 3; upload as a non-indexed vertex buffer and draw
`positions.len()` vertices. `tri_count()` (src/loft.rs:91) is `len / 3`.

---

## `fn loft_hull(design: &ShipDesign) -> HullMesh` (src/loft.rs:100)

**Intent:** The thin wrapper — unpack `plan` / `section` / `height_profile` and the
`stretch` / `hscale` settings from the design (sec_n uses `DEFAULT_SEC_N`; the design
format doesn't carry one), then defer to `loft_from_profiles`. This is the entry
point the engine calls on a loaded `.json`.

**Cross-references:** consumes a [`ShipDesign`](ship_design.md); output fed to the
renderer's `loft_gpu` (Stage 2-4 of [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)).
**Worked example:** `loft_hull_unpacks_a_ship_design` (src/loft.rs:402) — a
JSON-parsed design lofts the same mesh as calling `loft_from_profiles` with the
unpacked fields.

## `fn loft_from_profiles(plan, section, height, params) -> HullMesh` (src/loft.rs:121)

**Intent:** The lower-level loft. Mirrors the POC's `build_hull`.

Line 129-131: `l` = half the stretched length (a plan x of 0.5 → world x 0
amidships; 0 → stern −l; 1 → prow +l), `h` = hscale, `sec_n` floored at 3. Line
140-147: each plan point becomes a **station** — world x, half-width, and the
height-profile multiplier sampled at that x. Line 152-164: `ring_pts` builds one
station's vertex ring — sample the section at `sec_n` steps for the right (+z) side
top→belly, then mirror the interior points for the left (−z) side (skipping the
shared top/belly), each vertex `[station.x, y·h·hm, z·width]`. Line 172-174: a
surface needs ≥2 stations and a non-empty ring, else return an empty mesh (the
degenerate guard). Line 177-181: `push_tri` appends a triangle's 3 positions + its
shared face normal. Line 184-191: **stitch consecutive rings** — two triangles per
quad around the loop between ring *s* and *s+1*.

**Cross-references:** called by `loft_hull`; calls `sample_section`,
`sample_height_prof`, `face_normal`. **Worked examples:**
`vertex_count_matches_ring_stitch_formula` (src/loft.rs:291, pins
`(stations−1)·ringN·2·3` verts), `prow_is_narrower_than_stern` (src/loft.rs:329, the
dagger reads as a dagger), `stretch_scales_length` (src/loft.rs:356, ×2 stretch →
×2 x-extent), `degenerate_single_station_yields_empty_mesh` (src/loft.rs:431).

---

## Sampling + math helpers (src/loft.rs:199–260)

- `sample_height_prof(height, x)` (src/loft.rs:199) — piecewise-linear over the
  height profile (clamping out-of-range x to the ends), or flat `1.0` when no profile
  (the editor's no-traced-image default). `height_profile_scales_height`
  (src/loft.rs:381) pins a 2.0 profile doubling the y-extent.
- `sample_section(section, t)` (src/loft.rs:227) — piecewise-linear across the
  section for `t ∈ 0..=1`, returning `(z-half-width-factor, y-height)`; maps `t` onto
  `[0, n−1]` index space and lerps the bracketing pair.
- `lerp` (src/loft.rs:239); `face_normal(a,b,c)` (src/loft.rs:246) — normalized
  `(b−a)×(c−a)`, falling back to `+y` for degenerate (zero-area) triangles so the
  result is always unit-length. `all_normals_are_unit_length` (src/loft.rs:309) and
  `loft_is_deterministic` (src/loft.rs:322) pin those invariants — determinism
  matters because the mesh feeds the structural-determinism harness and must be
  bit-stable build-to-build.
