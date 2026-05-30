//! Content catalog loader. Reads `assets/broadside.catalog.json` (the JSON
//! exported by the analysis doc's "Copy JSON" button) into the typed surface
//! declared in [`crate::types`].
//!
//! The JSON shape is canonical — see `broadside-engine/engine/types.ts` and
//! the `Catalog` definition there. This module only exposes thin convenience
//! wrappers; type-driven deserialization handles the rest.

use std::fs;
use std::io;
use std::path::Path;

use crate::types::Catalog;

/// Errors loading a catalog from disk.
#[derive(Debug)]
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
pub fn load_from_path(path: impl AsRef<Path>) -> Result<Catalog, LoadError> {
    let bytes = fs::read(path)?;
    let catalog: Catalog = serde_json::from_slice(&bytes)?;
    Ok(catalog)
}

/// Decode an in-memory JSON byte slice (useful for embedded test fixtures).
pub fn load_from_bytes(bytes: &[u8]) -> Result<Catalog, LoadError> {
    Ok(serde_json::from_slice(bytes)?)
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
}
