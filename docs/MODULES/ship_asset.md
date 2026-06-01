# `src/ship_asset.rs` — ship-geometry selector / loader

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/ship_asset.rs`](../LINE_BY_LINE.md#srcship_assetrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **"one loader over two producers" join**. A Broadside ship's geometry
enters the engine two ways (see [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)), and
they deliberately meet at the same [`HullMesh`](loft.md) boundary:

- **Loft path** — a [`ShipDesign`](ship_design.md) `.json` from the loft editor →
  [`loft::loft_hull`](loft.md). Procedural hulls have no authored per-vertex colour,
  so the colour slice is a uniform house grey.
- **CAD path** — a baked `.glb` from the CAD editor → [`mesh_import::load_glb`](mesh_import.md),
  whose per-group materials flatten to a per-vertex colour slice via
  `ImportedShip::vertex_colors`.

This module lets a caller say *"give me the renderable geometry for this ship asset"*
without caring which producer it came from. It is **pure data plumbing**: it dispatches
to the two existing functions and normalizes both to the same `(HullMesh, Vec<[f32; 4]>)`
shape the render path consumes. **No rendering, no GPU, no bin wiring** — the GPU upload
([`loft_gpu`](loft_gpu.md)) and spawn integration live in the renderer's slice; this loader
just decides *which producer* and returns plain data. No TS analog — the asset-selection
layer of the render pivot.

### The returned shape

`ShipGeometry { mesh, colors }` — geometry plus one colour per tri-soup vertex
(`colors.len() == mesh.positions.len()`, 1:1 indexable). The renderer feeds the colour
slice to `loft_gpu.upload(mesh, &colors)` for **both** producers — the loft path's
uniform-grey slice and the CAD path's per-material slice ride the identical channel.

---

## `const DEFAULT_HULL_COLOR` (src/ship_asset.rs:44)

House hull grey (`0xb4c6e0`, matching the loft POC's hull albedo so a loft hull reads the
same in-engine as in the editor preview) for procedurally-lofted hulls that carry no
authored colour. A compile-time guard at src/ship_asset.rs:176 asserts its **alpha is
1.0** — the loft path has no cut-out, so a translucent default would silently make
procedural hulls vanish in the posterize pass (which discards `a < 0.5`). That assert is a
small but sharp example of encoding an invariant as a hard compile error.

## `enum ShipAssetKind` (src/ship_asset.rs:49)

`LoftDesign` (a loft-editor `.json`) or `CadMesh` (a CAD `.glb`/`.gltf`). The caller either
knows the kind or lets `kind_from_extension` infer it.

## `enum AssetError` (src/ship_asset.rs:59)

`Design` / `Mesh` (wrapping whichever producer ran) / `UnknownExtension`.
`#[non_exhaustive]`, with `Display` / `Error::source` that forwards to the wrapped producer
error.

## `struct ShipGeometry` (src/ship_asset.rs:94)

The normalized result: `{ mesh, colors }`. `into_parts()` (src/ship_asset.rs:102)
destructures into the `(mesh, colors)` tuple `loft_gpu.upload` consumes
(`into_parts_yields_the_upload_tuple`, src/ship_asset.rs:254).

---

## The loaders (src/ship_asset.rs:114–171)

### `fn load_bytes(kind, bytes) -> Result<ShipGeometry, AssetError>` (src/ship_asset.rs:114)

**Intent:** Dispatch on an explicit `kind` over in-memory bytes. `LoftDesign` → parse a
`ShipDesign` and `from_loft_design` it (uniform grey); `CadMesh` → `load_glb` and flatten
its materials via `vertex_colors`. Both normalize to `ShipGeometry`.

### `fn load_path(path) -> Result<ShipGeometry, AssetError>` (src/ship_asset.rs:132)

**Intent:** Load from a file, **inferring** the producer from the extension via
`kind_from_extension` (`.json` → loft, `.glb`/`.gltf` → CAD). The loft branch uses
`ShipDesign::load_from_path`; the CAD branch slurps the bytes and reuses `load_glb` (I/O
failure maps to a glTF import error via the `From` impl). An unknown extension is
`AssetError::UnknownExtension`.

### `fn from_loft_design(design) -> ShipGeometry` (src/ship_asset.rs:156)

**Intent:** Loft a parsed `ShipDesign` and pair it with a uniform house-grey colour slice.
Exposed so a caller that already holds a parsed design (e.g. from a catalog) can skip the
bytes round-trip.

### `fn kind_from_extension(path) -> Option<ShipAssetKind>` (src/ship_asset.rs:164)

Case-insensitive extension → kind (`json`→loft, `glb`/`gltf`→CAD, else `None`).

**Cross-references:** Dispatches to [`loft::loft_hull`](loft.md) +
[`ship_design::ShipDesign`](ship_design.md) (loft path) and
[`mesh_import::load_glb`](mesh_import.md) (CAD path); its `ShipGeometry` feeds the
renderer's [`loft_gpu`](loft_gpu.md) `upload`. The join that makes "one render path, two
producers" a single call for the caller.

**Worked examples:** `loft_branch_lofts_and_uniform_greys` (src/ship_asset.rs:204, all
verts the house grey, identical to dispatching the parsed design),
`loft_branch_surfaces_design_errors` (src/ship_asset.rs:216),
`cad_branch_imports_glb_and_flattens_colors` (src/ship_asset.rs:222, pins the CAD branch
routes to `load_glb` — a non-glb is a `Mesh` error, not `Design`),
`extension_dispatch_is_case_insensitive` (src/ship_asset.rs:237),
`unknown_extension_is_an_error` (src/ship_asset.rs:248).

**Test-coverage note (reviewer #26, `docs/reviews/ship_asset.md`):** the CAD branch's worked
example pins only the **routing** (garbage → `Mesh` error, not `Design`), *not* a full
valid-`.glb` → `ShipGeometry` round-trip — deliberately, to avoid duplicating `mesh_import`'s
test-only glTF fixture. Acceptable scoping: the CAD branch beyond the already-well-tested
`load_glb` is just `vertex_colors()` (thoroughly tested in [`mesh_import`](mesh_import.md),
including the 1:1-length invariant) plus the struct pairing. Reviewer **approved** with no
findings.
