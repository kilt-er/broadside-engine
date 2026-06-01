# `src/mesh_import.rs` — glTF `.glb` → `HullMesh` import

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/mesh_import.rs`](../LINE_BY_LINE.md#srcmesh_importrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

There are **two ship-geometry producers** that meet at the same
[`HullMesh`](loft.md) boundary:

1. [`loft.rs`](loft.md) — `loft_hull(&ShipDesign)` from the simple plan+section loft
   editor (Stage 1 of [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)).
2. **This module** — `load_glb(bytes)` from the full **CAD editor**
   (`ShipEditor/broadside-cad-editor.html`), a parametric feature tree
   (sketch/extrude/mirror/array). The engine does **not** replay that tree — the
   CAD tool exports a **baked mesh** as glTF binary (`.glb`) via Three.js's
   `GLTFExporter`, and this module reads it.

Both emit the **same `HullMesh` shape**, so the one geometry-source-agnostic loft
render path consumes either. *One renderer, two producers.* No TS analog — a new
asset-import path for the render pivot. **No feature gate** — pure parsing (no
`wgpu`), so it compiles and tests headless on CI alongside the loft math.

### Decisions baked in (locked by bruce / the lead)

- **Format = glTF binary `.glb`**, read via the `gltf` crate (CAD side is ~15 lines
  of `GLTFExporter`; glTF future-proofs hierarchy/materials/moving-parts).
- **Tri-soup, not indexed** — glTF primitives are usually indexed; this **expands**
  to a flat triangle soup so the output is byte-for-byte the shape `loft.rs` emits
  (positions/normals in lockstep, 3 verts per face). Flat shading wants per-face
  normals anyway, so the expansion isn't waste.
- **House-style posterize wins** — the engine **ignores** any per-ship `bands`/`res`
  the tool wrote (the game applies its house style uniformly so every ship reads
  consistently). Per-ship **light** (`laz`/`lel`) **is** honoured (it's part of the
  authored look), read from the glTF scene `extras`.

### What is NOT here

No GPU upload, no posterize, no camera — those live in the renderer's
[`loft_gpu`](loft_gpu.md), which consumes this `ImportedShip` and applies the
per-group [`MeshMaterial`] colours via [`vertex_colors`](#fn-importedshipvertex_colors---vecf32-4-srcmesh_importrs146).
That per-group colouring is the one render-side coupling (the loft POC used a single
base colour); coordinated at this boundary.

---

## The types (src/mesh_import.rs:54–129)

- `DEFAULT_LAZ_DEG = -50` / `DEFAULT_LEL_DEG = 60` (src/mesh_import.rs:54) — the
  house-default light, matching the loft POC's `setLight`, used when a `.glb` carries
  no `laz`/`lel`.
- `MeshMaterial` (src/mesh_import.rs:61) — a material group's flat appearance: linear
  RGBA `color` (glTF `baseColorFactor`), `emissive` (`emissiveFactor` + a reserved w
  intensity hint; glow parts carry non-zero emissive), and `unlit` (glTF
  `KHR_materials_unlit` / the CAD tool's `MeshBasicMaterial` engine-glow — the render
  path skips Lambert for these). `Default` is glTF's opaque mid-grey, lit, no emissive.
- `ImportLight` (src/mesh_import.rs:92) — per-ship authored `{ laz_deg, lel_deg }`.
  Note **no `bands`/`res` field** — that's how the "house style overrides those" rule
  is enforced *by the type*.
- `GroupRange` (src/mesh_import.rs:111) — a contiguous `[start, start+len)` run of
  **vertices** (multiples of 3, since tri-soup) sharing a `material` index.
- `ImportedShip` (src/mesh_import.rs:120) — the fully-imported result: the `mesh`
  (tri-soup), the deduplicated `materials`, the `group_ranges` tying spans to
  materials, and the `light`.

### `fn ImportedShip::vertex_colors(&self) -> Vec<[f32; 4]>` (src/mesh_import.rs:146)

**Intent:** Flatten `group_ranges × materials[].color` into one colour **per vertex**,
in vertex order. This is the **side channel** the loft render path consumes:
`HullMesh` stays geometry-only (positions+normals) and uniform across both producers,
so per-vertex colour lives here, not on the mesh. The renderer's `loft_gpu.upload(mesh,
colors)` takes this slice for the CAD path; the loft path passes empty/uniform-grey.
Vertices not covered by any group fall back to the default colour so the slice is
always exactly `mesh.positions.len()` long (1:1 indexable). **Worked example:**
`vertex_colors_flatten_groups_to_per_vertex_slice` (src/mesh_import.rs:611).

---

## `enum ImportError` (src/mesh_import.rs:167)

`Gltf(gltf::Error)` (the crate rejected the bytes), `NoGeometry` (parsed but no
drawable mesh / no POSITION), `UnsupportedTopology` (a non-triangle primitive — the
CAD exporter only emits `TRIANGLES`, so anything else is a producer bug surfaced
rather than silently mis-rendered). `#[non_exhaustive]`, with `Display`/`source`/
`From<gltf::Error>`.

---

## `fn load_glb(bytes: &[u8]) -> Result<ImportedShip, ImportError>` (src/mesh_import.rs:216)

**Intent:** Import a baked ship mesh from `.glb` (or text `.gltf` with an embedded
data-URI buffer — `gltf::import_slice` handles both, so a self-contained design loads
from a byte slice with no external `.bin`).

Line 224-280: for every triangle primitive in every mesh — reject non-`Triangles`
topology (line 226); read POSITION (mandatory, skip if absent); read NORMAL (optional
— the CAD exporter always writes it, but synthesize flat face normals if absent, line
264-267); expand to tri-soup using indices when present else assume a triangle list
(line 248-251); each primitive becomes a `GroupRange` tagged with its deduplicated
material (line 254, 271-278). Line 282-284: empty geometry → `NoGeometry`. Line 286:
read the scene light. The expansion at line 256-269 walks index triples, pushing 3
positions + (real or synthesized) normals per triangle — that's the indexed→tri-soup
conversion that keeps the output identical to loft's shape.

**Cross-references:** Produces an `ImportedShip` whose `mesh` feeds the renderer's
[`loft_gpu`](loft_gpu.md) (same boundary as [`loft_hull`](loft.md)); calls
`dedup_material`, `material_of`, `read_scene_light`, `face_normal`. **Worked
examples:** `loads_a_single_material_quad` (src/mesh_import.rs:557),
`expands_indexed_geometry_to_tri_soup` (src/mesh_import.rs:573),
`two_primitives_yield_two_groups_and_materials` (src/mesh_import.rs:587),
`garbage_bytes_are_a_gltf_error` (src/mesh_import.rs:683).

---

## Helpers (src/mesh_import.rs:299–377)

- `dedup_material` (src/mesh_import.rs:299) — return a material's index in `materials`,
  deduplicated so two primitives sharing a glTF material share one `MeshMaterial`
  (`shared_material_is_deduplicated`, src/mesh_import.rs:662).
- `material_of` (src/mesh_import.rs:311) — translate a glTF material to `MeshMaterial`
  (base colour, emissive, unlit); a primitive with no material index → the glTF default.
- `read_scene_light` (src/mesh_import.rs:330) — read `{ laz, lel }` (degrees) from the
  default scene's `extras` (raw JSON, parsed leniently), falling back to the house
  default when absent/unparseable; `bands`/`res` in the same blob are **intentionally
  ignored**. `honors_scene_light_extras` (src/mesh_import.rs:639) +
  `missing_extras_falls_back_to_house_default_light` (src/mesh_import.rs:654) pin both
  halves.
- `face_normal` (src/mesh_import.rs:363) — `(b−a)×(c−a)` normalized, `+y` fallback for
  degenerate tris; mirrors [`loft.rs`](loft.md)'s `face_normal` so synthesized normals
  match the loft path's convention.

*(The test module hand-rolls a minimal glTF 2.0 `.glb` writer (`build_glb`,
src/mesh_import.rs:396) so `load_glb` is testable today without the CAD tool's export
— a deterministic fixture emitting exactly the indexed-TRIANGLES + POSITION/NORMAL +
PBR-material shape the `GLTFExporter` will.)*
