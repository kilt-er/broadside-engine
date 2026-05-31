# Review: src/loft.rs (pure-math ShipDesign -> HullMesh loft)

Reviewer audit. Rust-native render-pipeline stage-1; no TS counterpart. Audited
for internal correctness + faithfulness to the POC `build_hull` / loft-editor
`buildHull()` reference. Status: **APPROVE.** No findings.

## Verified vs the POC (src/bin/loft_poc.rs, the validated reference)

Line-for-line faithful port. Confirmed identical:
- `l = 6.0 * params.stretch / 2.0` (loft.rs:129 == loft_poc.rs:112) — half-length; plan x 0.5 -> world x 0 (amidships), 0 -> stern (-l), 1 -> prow (+l).
- Ring vertex count `2·sec_n − 2` — right side top→belly (sec_n pts) + mirrored interior belly→top skipping the two shared endpoints (sec_n−2 pts). Matches the documented editor SECN scheme.
- Quad stitch `j = (i + 1) % n`, two tris per quad, `(stations−1)` gaps — identical indices AND winding to loft_poc.rs:157-159 (`push_tri(ra[i], ra[j], rb[i])` + `push_tri(ra[j], rb[j], rb[i])`). Winding preserved => face normals point the same way as the validated POC.
- face_normal `(b−a)×(c−a)` normalized, degenerate -> +y fallback (always unit). Matches POC guard.
- sample_section maps t∈[0,1] onto [0,n−1] index space, lerp in-bracket, clamps i to n−2. sample_height_prof piecewise-linear, clamps OOR to ends, flat 1.0 when absent.

The one documented deviation (drive plan/section/settings from a loaded ShipDesign instead of POC hardcoded consts) is exactly as the docstring claims — `loft_hull` unpacks ShipDesign, `loft_from_profiles` is the shared core.

## Soundness

- **No unsafe, no wgpu, no feature gate** — pure arithmetic, headless-testable, deterministic. The GPU upload lives in the renderer's loft_gpu (out of scope here). Correct separation: the math productionizes ahead of the visual POC verdict.
- HullMesh invariant `positions.len() == normals.len()`, multiple of 3, flat-shaded triangle soup (3 verts/tri share the face normal) — verified by the vertex-count test.

## Tests (8, all green, genuinely behavioral)

vertex-count-matches-formula, all-normals-unit-length, deterministic, prow-narrower-than-stern (the dagger reads as a dagger), stretch-doubles-length, height-profile-doubles-height, ShipDesign-round-trip-equals-profiles, degenerate-single-station-empty-no-panic. These assert geometric PROPERTIES, not just shape counts — the right depth for asset-gen math.
