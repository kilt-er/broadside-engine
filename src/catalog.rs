//! Content catalog loader. Reads `assets/broadside.catalog.json` (the JSON
//! exported by the analysis doc's "Copy JSON" button) into the typed surface
//! declared in [`crate::types`].
//!
//! The JSON shape is canonical — see `broadside-engine/engine/types.ts` and
//! the `Catalog` definition there. This module only exposes thin convenience
//! wrappers; type-driven deserialization handles the rest.
//!
//! # Catalog → Content seam
//!
//! [`crate::types::Catalog::actions`] is a `Vec<Action>` to match the JSON
//! wire shape (a JSON array). The TS resolver's `Content.actions` is
//! `Record<string, Action>`, i.e. an `id -> Action` map. The resolver crate
//! should build a `HashMap<String, Action>` from `catalog.actions` **once
//! at startup** and own that as its `Content` struct — not re-scan the
//! `Vec` per fire, and not duplicate-store the map on `Catalog`. This
//! file is the load shape; the resolver crate defines the runtime indexing.

use std::fs;
use std::io;
use std::path::Path;

use crate::types::Catalog;

/// Errors loading a catalog from disk.
///
/// Marked `#[non_exhaustive]` so downstream `match`es get a warning when a
/// new variant (e.g. a `BadSchema(String)` validation failure) is added.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error reading catalog: {e}"),
            LoadError::Parse(e) => write!(f, "parse error in catalog json: {e}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) => Some(e),
            LoadError::Parse(e) => Some(e),
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self { LoadError::Io(e) }
}
impl From<serde_json::Error> for LoadError {
    fn from(e: serde_json::Error) -> Self { LoadError::Parse(e) }
}

/// Read the catalog JSON file at `path` and decode it into a [`Catalog`].
///
/// Tries the strict shape first (the engine's native format); on parse
/// failure falls back to the canonical / design-doc-export shape via
/// [`crate::catalog_canonical::from_canonical_value`]. Either shape lands
/// in the same `Catalog` struct after this call — the resolver doesn't
/// see the format split.
///
/// The auto-detect path means tester's `tests/catalog_smoke.rs` and the
/// demo bin's startup loader both accept the canonical JSON bruce
/// exported from the analysis doc without per-caller plumbing.
pub fn load_from_path(path: impl AsRef<Path>) -> Result<Catalog, LoadError> {
    let bytes = fs::read(path)?;
    load_from_bytes(&bytes)
}

/// Decode an in-memory JSON byte slice (useful for embedded test fixtures).
/// Same auto-detect dispatch as [`load_from_path`].
pub fn load_from_bytes(bytes: &[u8]) -> Result<Catalog, LoadError> {
    // Strict shape first — fast path for engine-emitted JSON.
    if let Ok(c) = serde_json::from_slice::<Catalog>(bytes) {
        return Ok(c);
    }
    // Fallback: parse to a loose Value and run the canonical transformer.
    let v: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(crate::catalog_canonical::from_canonical_value(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimum-viable catalog that should round-trip: enough fields to exercise
    /// the trickier serde shapes (tagged Effect, Orientation, RangeBand casing).
    /// Empty arrays for the placeholder sections (`capitals` etc.) are accepted
    /// via `#[serde(default)]` on the Catalog struct.
    const MINIMAL_CATALOG_JSON: &str = r#"
{
  "meta": {
    "schema": "broadside.v0",
    "lane": [0,1,2,3,4,5,6],
    "newAxes": ["range","orientation","ordnance","heat"],
    "bands": ["pointBlank","close","mid","long","extreme"]
  },
  "actions": [
    {
      "id": "pulse_laser",
      "name": "Pulse Laser",
      "archetype": "beam",
      "cost": { "heat": 1, "cooldownMax": 0, "advancesTurn": true },
      "targeting": {
        "pattern": "BEAM",
        "band": ["pointBlank","close","mid"],
        "optimalBand": "close",
        "requiresArc": "forward",
        "facingRelative": true,
        "hitsAll": false
      },
      "effects": [
        { "kind": "DAMAGE", "amount": 4 }
      ]
    }
  ],
  "mods": [],
  "subsystems": [],
  "statuses": [],
  "enemies": [],
  "patrols": [{ "n": 1, "mod": "baseline" }]
}
"#;

    #[test]
    fn loads_minimal_catalog() {
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        assert_eq!(cat.meta.schema, "broadside.v0");
        assert_eq!(cat.actions.len(), 1);
        assert_eq!(cat.actions[0].id, "pulse_laser");
    }

    #[test]
    fn placeholder_sections_default_to_empty_when_absent() {
        // Reviewer m4 + m3 follow-up: confirm `#[serde(default)]` on the
        // placeholder `unknown[]`-typed sections actually omits cleanly.
        // MINIMAL_CATALOG_JSON intentionally omits capitals/classes/fieldkit/
        // sectors/commendations entirely; if the defaults regress, this test
        // will fail with `missing field` parse errors.
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        assert!(cat.capitals.is_empty());
        assert!(cat.classes.is_empty());
        assert!(cat.fieldkit.is_empty());
        assert!(cat.sectors.is_empty());
        assert!(cat.commendations.is_empty());
    }
}
