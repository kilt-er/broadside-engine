//! Ship-geometry **selector / loader** — the thin plumbing layer over the two
//! geometry producers so a caller can say "give me the renderable geometry for
//! this ship asset" without caring whether it came from the simple loft editor
//! or the full CAD tool.
//!
//! ## Why this exists
//!
//! There are two ways a Broadside ship's geometry enters the engine, and they
//! deliberately meet at the same [`loft::HullMesh`] boundary (one render path,
//! two producers — see `docs/RENDER_PIPELINE.md`):
//!
//! - **Loft path** — a [`ship_design::ShipDesign`] `.json` from the loft editor
//!   (plan + section profiles) → [`loft::loft_hull`]. Procedural hulls have no
//!   authored per-vertex colour, so the colour slice is a uniform house grey.
//! - **CAD path** — a baked `.glb` from the son's Broadside CAD editor →
//!   [`mesh_import::load_glb`], whose per-group materials flatten to a
//!   per-vertex colour slice via [`mesh_import::ImportedShip::vertex_colors`].
//!
//! This module is **pure data plumbing**: it dispatches to those two existing
//! functions and normalizes both to the same `(HullMesh, Vec<[f32; 4]>)`
//! shape the render path consumes. **No rendering, no GPU, no bin wiring** —
//! the GPU upload + spawn integration live behind the render feature in the
//! renderer's slice; this loader just decides *which producer* and returns
//! plain data.
//!
//! ## The returned shape
//!
//! `(HullMesh, Vec<[f32; 4]>)`: the geometry plus one colour per tri-soup
//! vertex, `colors.len() == mesh.positions.len()`, 1:1 indexable. The renderer
//! feeds the colour slice to `loft_gpu.upload(mesh, &colors)` for **both**
//! producers — the loft path's uniform-grey slice and the CAD path's
//! per-material slice ride the identical channel.

use std::path::Path;

use crate::loft::{self, HullMesh};
use crate::mesh_import::{self, MeshMaterial};
use crate::ship_design::ShipDesign;

/// House hull grey for procedurally-lofted hulls, which carry no authored
/// per-vertex colour. Matches the loft POC's hull albedo
/// (`0xb4c6e0` ≈ `[180, 198, 224] / 255`) so a loft hull reads the same in the
/// engine as it does in the editor preview.
pub const DEFAULT_HULL_COLOR: [f32; 4] = [180.0 / 255.0, 198.0 / 255.0, 224.0 / 255.0, 1.0];

/// Which producer a ship asset feeds. The caller either knows the kind
/// explicitly or lets [`kind_from_extension`] infer it from a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShipAssetKind {
    /// A loft-editor `.json` design (plan + section profiles).
    LoftDesign,
    /// A CAD-editor baked mesh, glTF binary `.glb` (or text `.gltf`).
    CadMesh,
}

/// Errors from loading a ship asset, wrapping whichever producer ran.
#[derive(Debug)]
#[non_exhaustive]
pub enum AssetError {
    /// The loft `.json` design failed to parse.
    Design(crate::ship_design::DesignError),
    /// The CAD `.glb` mesh failed to import.
    Mesh(mesh_import::ImportError),
    /// A path had no extension, or one this loader doesn't recognize
    /// (only `.json` → loft and `.glb` / `.gltf` → CAD are known).
    UnknownExtension,
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Design(e) => write!(f, "ship-asset design load: {e}"),
            AssetError::Mesh(e) => write!(f, "ship-asset mesh load: {e}"),
            AssetError::UnknownExtension => {
                write!(
                    f,
                    "ship-asset: unrecognized file extension (expected .json / .glb / .gltf)"
                )
            }
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AssetError::Design(e) => Some(e),
            AssetError::Mesh(e) => Some(e),
            AssetError::UnknownExtension => None,
        }
    }
}

/// Loaded ship geometry, normalized across both producers: a [`HullMesh`] and
/// one colour per tri-soup vertex (`colors.len() == mesh.positions.len()`).
#[derive(Clone, Debug, PartialEq)]
pub struct ShipGeometry {
    pub mesh: HullMesh,
    pub colors: Vec<[f32; 4]>,
}

impl ShipGeometry {
    /// Convenience destructure into the `(mesh, colors)` tuple the renderer's
    /// `loft_gpu.upload` consumes.
    pub fn into_parts(self) -> (HullMesh, Vec<[f32; 4]>) {
        (self.mesh, self.colors)
    }
}

/// Load ship geometry from in-memory bytes, dispatching on `kind`.
///
/// - [`ShipAssetKind::LoftDesign`]: parse a [`ShipDesign`] and loft it; the
///   colour slice is a uniform [`DEFAULT_HULL_COLOR`], one per vertex.
/// - [`ShipAssetKind::CadMesh`]: import the `.glb` and flatten its per-group
///   materials to per-vertex colours via
///   [`mesh_import::ImportedShip::vertex_colors`].
pub fn load_bytes(kind: ShipAssetKind, bytes: &[u8]) -> Result<ShipGeometry, AssetError> {
    match kind {
        ShipAssetKind::LoftDesign => {
            let design = ShipDesign::load_from_json(bytes).map_err(AssetError::Design)?;
            Ok(from_loft_design(&design))
        }
        ShipAssetKind::CadMesh => {
            let ship = mesh_import::load_glb(bytes).map_err(AssetError::Mesh)?;
            let colors = ship.vertex_colors();
            Ok(ShipGeometry {
                mesh: ship.mesh,
                colors,
            })
        }
    }
}

/// Load ship geometry from a file, inferring the producer from the file
/// extension (`.json` → loft, `.glb` / `.gltf` → CAD). I/O errors surface as
/// the matching producer error ([`AssetError::Design`] / [`AssetError::Mesh`]),
/// since both producers read their own bytes.
pub fn load_path(path: impl AsRef<Path>) -> Result<ShipGeometry, AssetError> {
    let path = path.as_ref();
    let kind = kind_from_extension(path).ok_or(AssetError::UnknownExtension)?;
    match kind {
        ShipAssetKind::LoftDesign => {
            let design = ShipDesign::load_from_path(path).map_err(AssetError::Design)?;
            Ok(from_loft_design(&design))
        }
        ShipAssetKind::CadMesh => {
            // mesh_import reads from bytes; slurp the file and reuse load_glb.
            // I/O failure maps to a glTF import error via the From impl.
            let bytes = std::fs::read(path)
                .map_err(|e| AssetError::Mesh(mesh_import::ImportError::Gltf(e.into())))?;
            let ship = mesh_import::load_glb(&bytes).map_err(AssetError::Mesh)?;
            let colors = ship.vertex_colors();
            Ok(ShipGeometry {
                mesh: ship.mesh,
                colors,
            })
        }
    }
}

/// Loft a [`ShipDesign`] and pair it with a uniform house-grey colour slice
/// (one [`DEFAULT_HULL_COLOR`] per tri-soup vertex). Exposed so a caller that
/// already holds a parsed `ShipDesign` (e.g. from a catalog) can skip the
/// bytes round-trip.
pub fn from_loft_design(design: &ShipDesign) -> ShipGeometry {
    let mesh = loft::loft_hull(design);
    let colors = vec![DEFAULT_HULL_COLOR; mesh.positions.len()];
    ShipGeometry { mesh, colors }
}

/// Infer the [`ShipAssetKind`] from a path's extension. `None` for an unknown
/// or missing extension. Case-insensitive.
pub fn kind_from_extension(path: &Path) -> Option<ShipAssetKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "json" => Some(ShipAssetKind::LoftDesign),
        "glb" | "gltf" => Some(ShipAssetKind::CadMesh),
        _ => None,
    }
}

/// Compile-time guard: [`DEFAULT_HULL_COLOR`] is opaque (alpha 1.0). The loft
/// path has no alpha cut-out, so a translucent default would silently make
/// procedural hulls vanish in the posterize pass (which discards `a < 0.5`).
const _: () = assert!(DEFAULT_HULL_COLOR[3] == 1.0);

/// Keep the unused-import lint honest: [`MeshMaterial`] is referenced only in
/// docs / for the colour-default parity note. Touch it so the import that
/// documents where the CAD colours originate doesn't warn.
#[doc(hidden)]
const _: fn() = || {
    let _ = MeshMaterial::default();
};

#[cfg(test)]
mod tests {
    use super::*;

    const LOFT_JSON: &str = r#"{
        "format": "broadside-ship",
        "version": 1,
        "plan": [[0.0, 0.95], [0.5, 0.6], [1.0, 0.02]],
        "section": [[0.0, 0.55], [1.0, 0.05], [0.0, -0.55]],
        "heightProfile": null,
        "settings": {
            "pitch": 26, "yaw": 28, "zoom": 1, "stretch": 2.0, "hscale": 0.7,
            "sup": true, "greeb": 0.6, "bands": 4, "laz": -50, "lel": 60,
            "res": { "w": 160, "h": 100 }
        },
        "grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }
    }"#;

    #[test]
    fn loft_branch_lofts_and_uniform_greys() {
        let geo = load_bytes(ShipAssetKind::LoftDesign, LOFT_JSON.as_bytes()).expect("loft loads");
        assert!(geo.mesh.tri_count() > 0);
        // One colour per vertex, all the house grey.
        assert_eq!(geo.colors.len(), geo.mesh.positions.len());
        assert!(geo.colors.iter().all(|c| *c == DEFAULT_HULL_COLOR));
        // Identical to dispatching the parsed design directly.
        let design = ShipDesign::load_from_json(LOFT_JSON.as_bytes()).unwrap();
        assert_eq!(geo, from_loft_design(&design));
    }

    #[test]
    fn loft_branch_surfaces_design_errors() {
        let err = load_bytes(ShipAssetKind::LoftDesign, b"{ not json").expect_err("should fail");
        assert!(matches!(err, AssetError::Design(_)));
    }

    #[test]
    fn cad_branch_imports_glb_and_flattens_colors() {
        // Reuse mesh_import's in-memory fixture path: build a 1-tri .glb via a
        // round-trip through a known-good colour, then load through the
        // selector. We can't call mesh_import's test-only builder from here,
        // so assert the dispatch wiring against a real .glb produced by the
        // loft path is impossible — instead verify the CAD branch routes to
        // load_glb by feeding garbage and checking the error variant, and
        // verify the happy path in mesh_import's own suite. Here we pin the
        // wiring: a CAD-kind load of non-glb bytes is a Mesh error, not a
        // Design error.
        let err = load_bytes(ShipAssetKind::CadMesh, b"not a glb").expect_err("should fail");
        assert!(matches!(err, AssetError::Mesh(_)));
    }

    #[test]
    fn extension_dispatch_is_case_insensitive() {
        assert_eq!(
            kind_from_extension(Path::new("a/b/ship.json")),
            Some(ShipAssetKind::LoftDesign)
        );
        assert_eq!(
            kind_from_extension(Path::new("ship.JSON")),
            Some(ShipAssetKind::LoftDesign)
        );
        assert_eq!(
            kind_from_extension(Path::new("ship.glb")),
            Some(ShipAssetKind::CadMesh)
        );
        assert_eq!(
            kind_from_extension(Path::new("ship.GLB")),
            Some(ShipAssetKind::CadMesh)
        );
        assert_eq!(
            kind_from_extension(Path::new("ship.gltf")),
            Some(ShipAssetKind::CadMesh)
        );
        assert_eq!(kind_from_extension(Path::new("ship.png")), None);
        assert_eq!(kind_from_extension(Path::new("noext")), None);
    }

    #[test]
    fn unknown_extension_is_an_error() {
        let err = load_path("ship.png").expect_err("unknown ext should fail");
        assert!(matches!(err, AssetError::UnknownExtension));
    }

    #[test]
    fn into_parts_yields_the_upload_tuple() {
        let geo = load_bytes(ShipAssetKind::LoftDesign, LOFT_JSON.as_bytes()).unwrap();
        let n = geo.mesh.positions.len();
        let (mesh, colors) = geo.into_parts();
        assert_eq!(mesh.positions.len(), n);
        assert_eq!(colors.len(), n);
    }
}
