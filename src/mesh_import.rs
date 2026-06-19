//! Import a CAD-authored ship mesh (glTF binary, `.glb`) into the engine's
//! [`crate::loft::HullMesh`] geometry + per-group material colours.
//!
//! ## Where this fits
//!
//! The son's Broadside CAD editor (`ShipEditor/broadside-cad-editor.html`) is a
//! **parametric feature tree** (sketch / extrude / mirror / array), not the
//! plan+section profiles that [`crate::ship_design::ShipDesign`] /
//! [`crate::loft`] consume. The engine does **not** replay that tree — instead
//! the CAD tool exports a **baked mesh** (it already bakes geometry in its
//! `generatePieces()` step) as `.glb` via Three.js's `GLTFExporter`, and this
//! module reads that mesh.
//!
//! The output is the **same [`HullMesh`]** the loft path emits, so both
//! geometry sources — `loft_hull(&ShipDesign)` (the simple plan+section editor)
//! and `load_glb(bytes)` (the full CAD tool) — meet at the `HullMesh` boundary
//! and feed the one geometry-source-agnostic loft render path. One renderer,
//! two producers.
//!
//! ## Decisions baked into this module (locked by bruce / the lead)
//!
//! - **Format = glTF binary (`.glb`).** Read with the `gltf` crate. The CAD
//!   side is ~15 lines (`GLTFExporter`), the Rust side is a mature crate, and
//!   glTF future-proofs hierarchy / materials / moving parts.
//! - **Tri-soup, not indexed.** glTF primitives are usually indexed; we
//!   **expand** to a flat triangle soup so the result is byte-for-byte the
//!   same shape [`crate::loft`] emits (`positions` / `normals` in lockstep,
//!   3 verts per face). Flat shading wants per-face normals anyway, so the
//!   expansion is not waste, and downstream sees exactly one mesh shape.
//! - **House-style posterize wins.** The engine **ignores** any per-ship
//!   `bands` / `res` the tool wrote — the game applies its house style
//!   (320 / bands-8) uniformly so every ship reads consistently on the lane.
//!   Per-ship **light** (`laz` / `lel`) **is** honoured: it is part of the
//!   ship's authored look. Light is read from the glTF scene `extras`
//!   (`{ "laz": <deg>, "lel": <deg> }`); absent extras fall back to the house
//!   default ([`DEFAULT_LAZ_DEG`] / [`DEFAULT_LEL_DEG`], the POC's values).
//! - **No feature gate.** Pure parsing, no `wgpu`, so this compiles and
//!   tests headless on CI next to the loft math.
//!
//! ## What is NOT here
//!
//! No GPU upload, no posterize, no camera — those live in the renderer's
//! `loft_gpu`, which consumes the [`HullMesh`] this produces and applies the
//! per-group [`MeshMaterial`] colours via [`ImportedShip::group_ranges`]. That
//! per-group colouring is the one render-side coupling (the loft POC used a
//! single base colour); it is the renderer's small hull-shader extension,
//! coordinated at this boundary.

use crate::loft::HullMesh;

/// House-default light azimuth / elevation (degrees), used when a `.glb`
/// carries no `laz` / `lel` in its scene `extras`. Matches the loft POC's
/// `setLight` (`laz = -50`, `lel = 60`).
pub const DEFAULT_LAZ_DEG: f32 = -50.0;
pub const DEFAULT_LEL_DEG: f32 = 60.0;

/// A material group's flat appearance, lifted from the glTF material. The
/// loft render path tints each [`ImportedShip::group_ranges`] span with the
/// matching entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshMaterial {
    /// Linear RGBA base colour (glTF `pbrMetallicRoughness.baseColorFactor`).
    pub color: [f32; 4],
    /// Linear RGB emissive + a w-channel intensity hint (glTF
    /// `emissiveFactor`; w is always `1.0` here, reserved for a future
    /// `KHR_materials_emissive_strength` read). Glow parts (canopy / gun /
    /// battery / engine in the CAD tool) carry non-zero emissive.
    pub emissive: [f32; 4],
    /// `true` when the source material is unlit (glTF `KHR_materials_unlit`,
    /// or the CAD tool's `MeshBasicMaterial` engine-glow). The render path
    /// skips Lambert shading for these and draws the flat colour.
    pub unlit: bool,
}

impl Default for MeshMaterial {
    fn default() -> Self {
        // glTF's default material: opaque mid-grey, no emissive, lit. Mirrors
        // the spec's fallback for a primitive with no material index.
        Self {
            color: [0.8, 0.8, 0.8, 1.0],
            emissive: [0.0, 0.0, 0.0, 1.0],
            unlit: false,
        }
    }
}

/// Per-ship authored light direction, read from the glTF scene `extras`.
/// Azimuth / elevation in degrees, same convention as the loft editor's
/// `setLight`. House-style `bands` / `res` are deliberately **not** here —
/// the engine overrides those.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImportLight {
    pub laz_deg: f32,
    pub lel_deg: f32,
}

impl Default for ImportLight {
    fn default() -> Self {
        Self {
            laz_deg: DEFAULT_LAZ_DEG,
            lel_deg: DEFAULT_LEL_DEG,
        }
    }
}

/// A material group within [`ImportedShip::mesh`]: a contiguous run of
/// `[start, start+len)` **vertices** (not triangles) that share `material`
/// (an index into [`ImportedShip::materials`]). Because the mesh is tri-soup,
/// `start` and `len` are always multiples of 3.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupRange {
    pub start: usize,
    pub len: usize,
    pub material: usize,
}

/// A fully-imported ship: the tri-soup hull geometry, the distinct material
/// appearances, and the per-group vertex spans tying them together.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedShip {
    /// Triangle-soup geometry, identical in shape to [`crate::loft`] output.
    pub mesh: HullMesh,
    /// Distinct material appearances, indexed by [`GroupRange::material`].
    pub materials: Vec<MeshMaterial>,
    /// Vertex spans, in draw order, each tagged with its material index.
    pub group_ranges: Vec<GroupRange>,
    /// Per-ship authored light (honoured); house-style bands/res are ignored.
    pub light: ImportLight,
}

impl ImportedShip {
    /// Flatten `group_ranges` × `materials[].color` into one `[f32; 4]` colour
    /// per vertex of [`ImportedShip::mesh`], in vertex order.
    ///
    /// This is the **side channel** the loft render path consumes:
    /// [`crate::loft::HullMesh`] stays geometry-only (positions + normals) and
    /// uniform across both producers (loft + CAD), so per-vertex colour lives
    /// here with the materials, not on the mesh. The renderer's
    /// `loft_gpu.upload(mesh, colors)` takes this slice for the CAD path; the
    /// loft path passes an empty / uniform-grey slice instead.
    ///
    /// Vertices not covered by any [`GroupRange`] (should not happen for a
    /// well-formed import — every primitive emits a group) fall back to the
    /// [`MeshMaterial::default`] colour so the slice is always exactly
    /// `mesh.positions.len()` long and the renderer can index it 1:1.
    pub fn vertex_colors(&self) -> Vec<[f32; 4]> {
        let vcount = self.mesh.positions.len();
        let mut colors = vec![MeshMaterial::default().color; vcount];
        for g in &self.group_ranges {
            let color = self
                .materials
                .get(g.material)
                .map(|m| m.color)
                .unwrap_or_else(|| MeshMaterial::default().color);
            let end = (g.start + g.len).min(vcount);
            for c in &mut colors[g.start.min(vcount)..end] {
                *c = color;
            }
        }
        colors
    }
}

/// Errors from importing a `.glb`.
#[derive(Debug)]
#[non_exhaustive]
pub enum ImportError {
    /// The `gltf` crate rejected the bytes (malformed container, bad JSON,
    /// unsupported extension, missing buffer, …).
    Gltf(gltf::Error),
    /// The file parsed as glTF but carried no drawable geometry (no mesh, or
    /// a primitive with no POSITION accessor).
    NoGeometry,
    /// A primitive used a non-triangle topology (points / lines / strips /
    /// fans). The CAD exporter only emits `TRIANGLES`; anything else is a
    /// producer bug we surface rather than silently mis-render.
    UnsupportedTopology,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Gltf(e) => write!(f, "glTF import error: {e}"),
            ImportError::NoGeometry => write!(f, "glTF carried no drawable geometry"),
            ImportError::UnsupportedTopology => {
                write!(f, "glTF primitive used a non-triangle topology")
            }
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImportError::Gltf(e) => Some(e),
            _ => None,
        }
    }
}

impl From<gltf::Error> for ImportError {
    fn from(e: gltf::Error) -> Self {
        ImportError::Gltf(e)
    }
}

/// Import a baked ship mesh from glTF binary (`.glb`) bytes. Also accepts the
/// text `.gltf` form with an embedded data-URI buffer — `gltf::import_slice`
/// handles both — so a self-contained design loads from a byte slice with no
/// external `.bin` sidecar.
///
/// Geometry from every triangle primitive in every mesh is expanded to a flat
/// tri-soup and concatenated into one [`HullMesh`]; each primitive becomes a
/// [`GroupRange`] tagged with its (deduplicated) [`MeshMaterial`]. Per-ship
/// light is read from the scene `extras`; house-style bands/res are ignored.
pub fn load_glb(bytes: &[u8]) -> Result<ImportedShip, ImportError> {
    let (doc, buffers, _images) = gltf::import_slice(bytes)?;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut materials: Vec<MeshMaterial> = Vec::new();
    let mut group_ranges: Vec<GroupRange> = Vec::new();

    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                return Err(ImportError::UnsupportedTopology);
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));

            // POSITION is mandatory for drawable geometry.
            let pos_iter = match reader.read_positions() {
                Some(p) => p,
                None => continue,
            };
            let prim_positions: Vec<[f32; 3]> = pos_iter.collect();
            if prim_positions.is_empty() {
                continue;
            }

            // The glb's NORMAL attribute is DELIBERATELY IGNORED. The CAD
            // exporter (Three.js) writes smooth/averaged vertex normals — the
            // vendored broadside-ship.glb has 112 of 172 faces smooth-shaded —
            // which through the posterize pass reads as a lumpy "iceberg" with
            // contour banding instead of crisp facets. We always recompute
            // FLAT per-face normals from the positions so a CAD import yields
            // the exact same flat-shaded tri-soup the loft path emits. This
            // enforces the faceted house look on ANY import regardless of how
            // the source tool shaded it — the same "house style wins over the
            // producer" stance we take on bands/res. (At ~172 tris flat
            // shading is crisp, not blobby.)

            // Expand to tri-soup using indices when present, else assume the
            // positions are already a triangle list.
            let indices: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..prim_positions.len() as u32).collect(),
            };

            let group_start = positions.len();
            let material = dedup_material(&mut materials, &prim);

            for tri in indices.chunks_exact(3) {
                let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                let a = prim_positions[ia];
                let b = prim_positions[ib];
                let c = prim_positions[ic];
                positions.extend_from_slice(&[a, b, c]);
                // One flat normal per face, shared by its three verts — the
                // glb's stored (often smooth) normals are discarded.
                let fn_ = face_normal(a, b, c);
                normals.extend_from_slice(&[fn_, fn_, fn_]);
            }

            let group_len = positions.len() - group_start;
            if group_len > 0 {
                group_ranges.push(GroupRange {
                    start: group_start,
                    len: group_len,
                    material,
                });
            }
        }
    }

    if positions.is_empty() {
        return Err(ImportError::NoGeometry);
    }

    let light = read_scene_light(&doc);

    Ok(ImportedShip {
        mesh: HullMesh { positions, normals },
        materials,
        group_ranges,
        light,
    })
}

/// Pull a [`MeshMaterial`] from a primitive, returning its index in
/// `materials` (deduplicated so two primitives sharing a glTF material share
/// one [`MeshMaterial`]).
fn dedup_material(materials: &mut Vec<MeshMaterial>, prim: &gltf::Primitive) -> usize {
    let m = material_of(prim);
    if let Some(i) = materials.iter().position(|existing| *existing == m) {
        i
    } else {
        materials.push(m);
        materials.len() - 1
    }
}

/// Translate a glTF material into our flat [`MeshMaterial`]. A primitive with
/// no material index gets the glTF default.
fn material_of(prim: &gltf::Primitive) -> MeshMaterial {
    let mat = prim.material();
    if mat.index().is_none() {
        return MeshMaterial::default();
    }
    let pbr = mat.pbr_metallic_roughness();
    let color = pbr.base_color_factor();
    let e = mat.emissive_factor();
    MeshMaterial {
        color,
        emissive: [e[0], e[1], e[2], 1.0],
        unlit: mat.unlit(),
    }
}

/// Read `{ laz, lel }` (degrees) from the default scene's `extras`, falling
/// back to the house default when absent or unparseable. The CAD exporter
/// writes these so the engine can honour the ship's authored light direction;
/// `bands` / `res` in the same blob are intentionally ignored (house style).
fn read_scene_light(doc: &gltf::Document) -> ImportLight {
    let scene = match doc.default_scene().or_else(|| doc.scenes().next()) {
        Some(s) => s,
        None => return ImportLight::default(),
    };
    // With the `extras` feature, `extras()` is `&Option<Box<RawValue>>`.
    let raw = match scene.extras() {
        Some(r) => r,
        None => return ImportLight::default(),
    };
    // `extras` is raw JSON (a boxed RawValue); parse it leniently.
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(raw.get());
    let v = match parsed {
        Ok(v) => v,
        Err(_) => return ImportLight::default(),
    };
    let default = ImportLight::default();
    ImportLight {
        laz_deg: v
            .get("laz")
            .and_then(|x| x.as_f64())
            .map(|x| x as f32)
            .unwrap_or(default.laz_deg),
        lel_deg: v
            .get("lel")
            .and_then(|x| x.as_f64())
            .map(|x| x as f32)
            .unwrap_or(default.lel_deg),
    }
}

/// Normalized face normal of `(a, b, c)` via `(b−a) × (c−a)`. Degenerate
/// triangles fall back to `+y`. Mirrors [`crate::loft`]'s `face_normal`.
fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-8 {
        [0.0, 1.0, 0.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but valid binary `.glb` in memory: one mesh with the
    /// given primitives, each `(positions, indices, base_color, emissive)`,
    /// plus optional scene `extras` JSON. Hand-rolled rather than depending on
    /// the son's not-yet-existing export, so `load_glb` is testable today.
    ///
    /// This is a deterministic glTF 2.0 writer scoped to the test module — it
    /// emits exactly the shape the CAD `GLTFExporter` will (indexed TRIANGLES,
    /// POSITION + NORMAL, per-primitive PBR material).
    /// One primitive for [`build_glb`]: `(positions, indices, base_color,
    /// emissive)`. Aliased to keep the `build_glb` signature readable and
    /// clippy's `type_complexity` lint quiet.
    type PrimSpec<'a> = (&'a [[f32; 3]], &'a [u32], [f32; 4], [f32; 3]);

    fn build_glb(prims: &[PrimSpec], scene_extras: Option<&str>) -> Vec<u8> {
        use std::io::Write;

        // ---- pack all binary buffers (positions, normals, indices) ----
        let mut bin: Vec<u8> = Vec::new();
        let mut buffer_views = String::new();
        let mut accessors = String::new();
        let mut meshes_prims = String::new();
        let mut materials_json = String::new();

        let mut view_i = 0usize;
        let mut acc_i = 0usize;

        let align4 = |b: &mut Vec<u8>| {
            while !b.len().is_multiple_of(4) {
                b.push(0)
            }
        };

        for (pi, (positions, indices, color, emissive)) in prims.iter().enumerate() {
            // The fixture writes DELIBERATELY BOGUS normals (constant +Y on
            // every vertex) to prove load_glb discards the glb's NORMAL and
            // recomputes flat per-face normals from positions. A real CAD glb
            // carries smooth/averaged normals; bogus-but-present is the same
            // "don't trust the stored normal" condition, sharper for testing.
            let bogus = [0.0f32, 1.0, 0.0];
            let mut normals = vec![bogus; positions.len()];
            for tri in indices.chunks_exact(3) {
                let (a, b, c) = (
                    positions[tri[0] as usize],
                    positions[tri[1] as usize],
                    positions[tri[2] as usize],
                );
                let _ = (a, b, c); // positions are read for the loader's recompute, not here
                for &vi in tri {
                    normals[vi as usize] = bogus;
                }
            }

            // POSITION view/accessor
            let pos_off = bin.len();
            for p in positions.iter() {
                for c in p {
                    bin.write_all(&c.to_le_bytes()).unwrap();
                }
            }
            let pos_len = bin.len() - pos_off;
            align4(&mut bin);
            let (pmin, pmax) = bounds(positions);
            buffer_views +=
                &format!(r#"{{"buffer":0,"byteOffset":{pos_off},"byteLength":{pos_len}}},"#,);
            let pos_acc = acc_i;
            accessors += &format!(
                r#"{{"bufferView":{view_i},"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}},"#,
                positions.len(),
                pmin[0],
                pmin[1],
                pmin[2],
                pmax[0],
                pmax[1],
                pmax[2],
            );
            view_i += 1;
            acc_i += 1;

            // NORMAL view/accessor
            let nrm_off = bin.len();
            for n in &normals {
                for c in n {
                    bin.write_all(&c.to_le_bytes()).unwrap();
                }
            }
            let nrm_len = bin.len() - nrm_off;
            align4(&mut bin);
            buffer_views +=
                &format!(r#"{{"buffer":0,"byteOffset":{nrm_off},"byteLength":{nrm_len}}},"#,);
            let nrm_acc = acc_i;
            accessors += &format!(
                r#"{{"bufferView":{view_i},"componentType":5126,"count":{},"type":"VEC3"}},"#,
                normals.len(),
            );
            view_i += 1;
            acc_i += 1;

            // INDICES view/accessor (u32 = componentType 5125)
            let idx_off = bin.len();
            for &ix in indices.iter() {
                bin.write_all(&ix.to_le_bytes()).unwrap();
            }
            let idx_len = bin.len() - idx_off;
            align4(&mut bin);
            buffer_views +=
                &format!(r#"{{"buffer":0,"byteOffset":{idx_off},"byteLength":{idx_len}}},"#,);
            let idx_acc = acc_i;
            accessors += &format!(
                r#"{{"bufferView":{view_i},"componentType":5125,"count":{},"type":"SCALAR"}},"#,
                indices.len(),
            );
            view_i += 1;
            acc_i += 1;

            materials_json += &format!(
                r#"{{"pbrMetallicRoughness":{{"baseColorFactor":[{},{},{},{}]}},"emissiveFactor":[{},{},{}]}},"#,
                color[0], color[1], color[2], color[3], emissive[0], emissive[1], emissive[2],
            );
            meshes_prims += &format!(
                r#"{{"attributes":{{"POSITION":{pos_acc},"NORMAL":{nrm_acc}}},"indices":{idx_acc},"material":{pi},"mode":4}},"#,
            );
        }

        let bin_len = bin.len();
        let extras_field = scene_extras
            .map(|e| format!(r#","extras":{e}"#))
            .unwrap_or_default();

        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]{extras_field}}}],"nodes":[{{"mesh":0}}],"meshes":[{{"primitives":[{}]}}],"materials":[{}],"accessors":[{}],"bufferViews":[{}],"buffers":[{{"byteLength":{bin_len}}}]}}"#,
            trim_comma(&meshes_prims),
            trim_comma(&materials_json),
            trim_comma(&accessors),
            trim_comma(&buffer_views),
        );

        // ---- assemble the .glb container (12-byte header + 2 chunks) ----
        let mut json_bytes = json.into_bytes();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        let mut bin_bytes = bin;
        while !bin_bytes.len().is_multiple_of(4) {
            bin_bytes.push(0);
        }

        let total = 12 + 8 + json_bytes.len() + 8 + bin_bytes.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF"); // magic
        glb.extend_from_slice(&2u32.to_le_bytes()); // version
        glb.extend_from_slice(&(total as u32).to_le_bytes()); // total length
                                                              // JSON chunk
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        // BIN chunk
        glb.extend_from_slice(&(bin_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_bytes);
        glb
    }

    fn trim_comma(s: &str) -> &str {
        s.strip_suffix(',').unwrap_or(s)
    }

    fn bounds(p: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in p {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        (lo, hi)
    }

    /// A single quad (2 tris) on the XZ plane, one material.
    fn one_quad_glb(color: [f32; 4], emissive: [f32; 3], extras: Option<&str>) -> Vec<u8> {
        let positions: &[[f32; 3]] = &[
            [-1.0, 0.0, -1.0],
            [1.0, 0.0, -1.0],
            [1.0, 0.0, 1.0],
            [-1.0, 0.0, 1.0],
        ];
        let indices: &[u32] = &[0, 1, 2, 0, 2, 3];
        build_glb(&[(positions, indices, color, emissive)], extras)
    }

    #[test]
    fn loads_a_single_material_quad() {
        let glb = one_quad_glb([0.7, 0.78, 0.88, 1.0], [0.0, 0.0, 0.0], None);
        let ship = load_glb(&glb).expect("quad glb parses");
        // 2 tris -> 6 tri-soup verts.
        assert_eq!(ship.mesh.positions.len(), 6);
        assert_eq!(ship.mesh.normals.len(), 6);
        assert_eq!(ship.mesh.tri_count(), 2);
        // One material, one group spanning all 6 verts.
        assert_eq!(ship.materials.len(), 1);
        assert_eq!(ship.group_ranges.len(), 1);
        assert_eq!(
            ship.group_ranges[0],
            GroupRange {
                start: 0,
                len: 6,
                material: 0
            }
        );
        assert_eq!(ship.materials[0].color, [0.7, 0.78, 0.88, 1.0]);
        assert!(!ship.materials[0].unlit);
    }

    #[test]
    fn expands_indexed_geometry_to_tri_soup() {
        // The quad shares verts via indices (4 positions, 6 indices). After
        // import the mesh must be a flat soup of 6 distinct vertices.
        let glb = one_quad_glb([0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0], None);
        let ship = load_glb(&glb).unwrap();
        assert_eq!(
            ship.mesh.positions.len(),
            6,
            "indexed quad expands to 6 verts"
        );
        // Every normal unit-length (flat shading on the XZ quad -> +/- Y).
        for n in &ship.mesh.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "normal {n:?} not unit");
        }
    }

    #[test]
    fn discards_glb_normals_and_recomputes_flat() {
        // #43: the iceberg-banding fix. The fixture writes BOGUS +Y normals on
        // every vertex (build_glb above). A TILTED triangle has a true face
        // normal that is NOT +Y, so if load_glb kept the glb's NORMAL the
        // loaded normal would read [0,1,0]; because we recompute flat per-face
        // normals from positions, it must read the true geometric normal.
        let positions: &[[f32; 3]] = &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 1.0], // lifts + tilts the tri out of the XZ plane
        ];
        let indices: &[u32] = &[0, 1, 2];
        let glb = build_glb(
            &[(positions, indices, [0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0])],
            None,
        );
        let ship = load_glb(&glb).unwrap();

        let expected = face_normal(positions[0], positions[1], positions[2]);
        // The true normal is NOT the bogus +Y the fixture stored — proves the
        // glb normal was discarded, not passed through.
        assert!(
            (expected[1] - 1.0).abs() > 1e-3
                || expected[0].abs() > 1e-3
                || expected[2].abs() > 1e-3,
            "test setup: tilted tri normal should differ from bogus +Y, got {expected:?}",
        );
        for n in &ship.mesh.normals {
            assert!(
                (n[0] - expected[0]).abs() < 1e-4
                    && (n[1] - expected[1]).abs() < 1e-4
                    && (n[2] - expected[2]).abs() < 1e-4,
                "expected recomputed flat normal {expected:?}, got {n:?} (glb normal not discarded?)",
            );
        }
    }

    #[test]
    fn two_primitives_yield_two_groups_and_materials() {
        let positions: &[[f32; 3]] = &[[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0]];
        let indices: &[u32] = &[0, 1, 2];
        let glb = build_glb(
            &[
                (positions, indices, [0.7, 0.78, 0.88, 1.0], [0.0, 0.0, 0.0]),
                (positions, indices, [1.0, 0.54, 0.28, 1.0], [0.2, 0.07, 0.0]),
            ],
            None,
        );
        let ship = load_glb(&glb).unwrap();
        assert_eq!(ship.materials.len(), 2);
        assert_eq!(ship.group_ranges.len(), 2);
        assert_eq!(
            ship.group_ranges[0],
            GroupRange {
                start: 0,
                len: 3,
                material: 0
            }
        );
        assert_eq!(
            ship.group_ranges[1],
            GroupRange {
                start: 3,
                len: 3,
                material: 1
            }
        );
        // Second material carries emissive (a glow part).
        assert_eq!(ship.materials[1].emissive, [0.2, 0.07, 0.0, 1.0]);
    }

    #[test]
    fn vertex_colors_flatten_groups_to_per_vertex_slice() {
        // Two single-tri primitives with distinct colours -> 6 tri-soup verts,
        // first 3 the first colour, last 3 the second.
        let positions: &[[f32; 3]] = &[[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0]];
        let indices: &[u32] = &[0, 1, 2];
        let c0 = [0.7, 0.78, 0.88, 1.0];
        let c1 = [1.0, 0.54, 0.28, 1.0];
        let glb = build_glb(
            &[
                (positions, indices, c0, [0.0, 0.0, 0.0]),
                (positions, indices, c1, [0.0, 0.0, 0.0]),
            ],
            None,
        );
        let ship = load_glb(&glb).unwrap();
        let colors = ship.vertex_colors();
        // Exactly one colour per mesh vertex, indexable 1:1.
        assert_eq!(colors.len(), ship.mesh.positions.len());
        assert_eq!(colors.len(), 6);
        assert_eq!(&colors[0..3], &[c0, c0, c0]);
        assert_eq!(&colors[3..6], &[c1, c1, c1]);
    }

    #[test]
    fn honors_scene_light_extras() {
        let glb = one_quad_glb(
            [0.5, 0.5, 0.5, 1.0],
            [0.0, 0.0, 0.0],
            Some(r#"{"laz":30,"lel":45,"bands":4,"res":{"w":220,"h":138}}"#),
        );
        let ship = load_glb(&glb).unwrap();
        // Per-ship light honored...
        assert_eq!(ship.light.laz_deg, 30.0);
        assert_eq!(ship.light.lel_deg, 45.0);
        // ...but bands/res in the same blob are NOT surfaced (house style).
        // ImportLight has no bands/res field — this is enforced by the type.
    }

    #[test]
    fn missing_extras_falls_back_to_house_default_light() {
        let glb = one_quad_glb([0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0], None);
        let ship = load_glb(&glb).unwrap();
        assert_eq!(ship.light.laz_deg, DEFAULT_LAZ_DEG);
        assert_eq!(ship.light.lel_deg, DEFAULT_LEL_DEG);
    }

    #[test]
    fn shared_material_is_deduplicated() {
        // Two primitives with the SAME material collapse to one MeshMaterial,
        // two groups both pointing at material 0.
        let positions: &[[f32; 3]] = &[[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0]];
        let indices: &[u32] = &[0, 1, 2];
        let same = [0.6, 0.6, 0.7, 1.0];
        let glb = build_glb(
            &[
                (positions, indices, same, [0.0, 0.0, 0.0]),
                (positions, indices, same, [0.0, 0.0, 0.0]),
            ],
            None,
        );
        let ship = load_glb(&glb).unwrap();
        assert_eq!(ship.materials.len(), 1, "identical materials dedup to one");
        assert_eq!(ship.group_ranges.len(), 2);
        assert_eq!(ship.group_ranges[0].material, 0);
        assert_eq!(ship.group_ranges[1].material, 0);
    }

    #[test]
    fn garbage_bytes_are_a_gltf_error() {
        let err = load_glb(b"not a glb at all").expect_err("should fail");
        assert!(matches!(err, ImportError::Gltf(_)));
    }
}
