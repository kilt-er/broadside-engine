# Review: src/mesh_import.rs (glTF .glb -> HullMesh loader, e8fd83f)

Reviewer soundness pass. Data-only (no GPU, no feature gate, headless). The CAD
producer half of the "one renderer, two producers" seam. Status: **APPROVE.**
No findings. 8 tests green (no feature flags needed).

## HullMesh contract match with loft.rs (PASS — the seam holds)

`load_glb` emits `HullMesh { positions, normals }` — the EXACT type loft.rs
produces: positions/normals in lockstep, 3 verts per face, flat tri-soup. The
`face_normal` helper (mesh_import.rs:363) is a verbatim copy of loft.rs's — same
`(b−a)×(c−a)` cross product, same `+y` degenerate fallback, same 1e-8 threshold.
So both producers meet at a byte-identical mesh shape; the renderer's loft path
is genuinely geometry-source-agnostic. Indexed glTF primitives are expanded to
tri-soup (read_indices, else 0..n) so downstream sees one shape regardless of
source indexing.

## House-style enforcement (PASS — TYPE-enforced, not runtime)

`ImportLight` has ONLY `laz_deg` / `lel_deg` — there is structurally no `bands`
or `res` field. The `honors_scene_light_extras` test feeds a glb whose extras
carry `{"laz":30,"lel":45,"bands":4,"res":{...}}` and asserts laz/lel are honored
while bands/res are simply unrepresentable in the returned type. This is the
strongest form of enforcement — a malicious/old glb CANNOT override the house
320/8 because there's nowhere to put it. House style lives in loft_gpu consts,
never read from the glb.

## Per-ship light (PASS)

`read_scene_light` reads `laz`/`lel` (degrees) from the default scene's `extras`
(raw JSON via the gltf `extras` feature), per-field `unwrap_or(default)` to
DEFAULT_LAZ_DEG(-50) / DEFAULT_LEL_DEG(60) — the POC's setLight values. Lenient:
no scene, no extras, or unparseable JSON all fall back to house default, never
panic. Tested both honored + fallback.

## Correctness details (all PASS)

- **Material dedup**: linear position-search; two primitives with an identical MeshMaterial collapse to one entry, both groups point at the same index. Tested.
- **group_ranges**: vertex spans `[start, start+len)` (multiples of 3, tri-soup), each tagged with its dedup'd material index, in draw order. `vertex_colors()` flattens them to a per-vertex `[f32;4]` slice exactly `positions.len()` long (uncovered verts -> default grey) so the renderer indexes 1:1. Tested.
- **Topology**: non-`Triangles` mode -> `ImportError::UnsupportedTopology` (surfaced, not silently mis-rendered). Matches "CAD exporter only emits TRIANGLES."
- **Error paths**: garbage bytes -> `ImportError::Gltf` via `?`; parsed-but-empty -> `NoGeometry`; missing NORMAL synthesizes flat face normals. `#[non_exhaustive]` error enum with Display + source.

## Test fixture is a REAL load path (PASS — meaningful coverage)

`build_glb` hand-rolls a genuine binary glTF 2.0 container: 12-byte header (magic "glTF", version 2, le total length), JSON chunk + BIN chunk, 4-byte alignment, real accessors/bufferViews/materials (componentType 5126 VEC3 / 5125 SCALAR). `load_glb` runs the ACTUAL `gltf::import_slice` over these bytes — not a mock. So all 8 tests exercise the production parse path, giving real coverage before the son's CAD export exists. The fixture emits exactly the shape the Three.js GLTFExporter will (indexed TRIANGLES, POSITION+NORMAL, per-prim PBR).

## Dependency

`gltf = { version = "1", features = ["import","utils","extras","KHR_materials_unlit"] }` — mature crate, sane caret pin on the current stable major, minimal feature set matching exactly what's used (extras for scene light, KHR_materials_unlit for `mat.unlit()`, import/utils for slice + readers). Non-optional by design (lead-locked). Reasonable.
