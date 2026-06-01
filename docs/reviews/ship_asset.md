# Review: src/ship_asset.rs (unified ship-geometry selector, 18316df)

Reviewer soundness pass (task #26 data-half). Data-only plumbing over the two
HullMesh producers; no GPU, no feature gate. Status: **APPROVE.** No findings.
6 tests green (both branches). One optional test-coverage suggestion (non-blocking).

## Verified

- **Dispatch routing**: `.json` → LoftDesign → loft_hull + uniform DEFAULT_HULL_COLOR slice; `.glb`/`.gltf` → CadMesh → load_glb + vertex_colors(). `kind_from_extension` lowercases the ext (case-insensitive, tested .JSON/.GLB), unknown/missing → None → AssetError::UnknownExtension. `load_bytes` and `load_path` dispatch identically. load_path's CAD arm slurps the file and maps I/O failure to Mesh(Gltf(io)) — reasonable since mesh_import is bytes-based.
- **Opaque-default const-assert** (ship_asset.rs:176): `assert!(DEFAULT_HULL_COLOR[3] == 1.0)`. Sharp catch — the posterize pass discards `a < 0.5`, so a translucent loft-hull default would silently vanish. Pinned at compile time with the rationale documented. Good defensive instinct.
- **Output contract**: ShipGeometry { mesh, colors } with colors.len() == positions.len() — guaranteed for the CAD path by vertex_colors() (verified in mesh_import review) and for the loft path by `vec![grey; positions.len()]`. into_parts() yields the exact (mesh, colors) tuple loft_gpu.upload consumes. Both producers ride the identical channel — the "one render path, two producers" seam holds at the data layer too.
- **AssetError**: #[non_exhaustive], Design/Mesh/UnknownExtension, Display + source() wrapping the producer errors. Clean.

## The deliberate CAD happy-path coverage gap (ACCEPTABLE, optional close)

Architect flagged it: the CAD happy-path (valid .glb → ShipGeometry) is NOT
exercised here — to avoid exposing/duplicating mesh_import's test-only build_glb.
`cad_branch_imports_glb_and_flattens_colors` pins only the ROUTING (garbage →
Mesh error, not Design error).

Assessment: **acceptable scoping.** The CAD branch logic beyond the well-tested
load_glb is exactly two trivial moves — `ship.vertex_colors()` (thoroughly tested
in mesh_import, incl. the 1:1-length invariant) + the struct pairing. The routing
test covers the dispatch decision + error mapping. Stopping there is defensible.

IF they want to close it (my lean: cheap enough to be worth it), the clean path
does NOT need build_glb: `ImportedShip` has all-`pub` fields (mesh / materials /
group_ranges / light), so a ship_asset test can construct a tiny ImportedShip
directly and assert the mesh+colors pairing — no shared fixture, no widening of
mesh_import's public surface. ~10 lines. Flagged to tester as a nice-to-have, not
a blocker.

## Minor (non-blocking)

The `cad_branch_imports_glb_and_flattens_colors` test BODY is a rambling comment
that explains at length what it "can't" do before doing the one-line routing
assert. It reads like a thinking-out-loud note. Tightening it to just the routing
assertion + a one-line "happy path covered in mesh_import" pointer would read
cleaner. Cosmetic.

## Scope

ship_asset.rs only (pub mod, no cfg gate, headless). No foreign files touched.
