//! Serde shape for the **loft editor's** ship-design `.json` format.
//!
//! The browser tool at `docs/broadside-loft-editor.html` lets an artist trace
//! a hull's plan (top-down half-outline) and section (cross-section), tweak a
//! handful of render settings, and **save the design as JSON** (its
//! `collectDesign()` at `broadside-loft-editor.html:718`). This module is the
//! Rust mirror of that payload so the engine can loft a ship from the *design
//! data* rather than from baked sprite PNGs — the asset format the 3D render
//! path consumes.
//!
//! Pure data: **no rendering here.** The loft/render POC (task #109) owns the
//! geometry generation and the wgpu side; this module only parses and
//! re-serializes the design so that path graduates straight into "engine loads
//! `.json`".
//!
//! ## Wire schema (verbatim from `collectDesign()`)
//!
//! ```jsonc
//! {
//!   "format": "broadside-ship",
//!   "version": 1,
//!   "plan":    [[x, halfWidth], ...],          // stern(x=0) -> prow(x=1)
//!   "section": [[z, y], ...],                  // top -> belly half cross-section
//!   "heightProfile": [[x, mult], ...] | null,  // side-view height, null = flat 1.0
//!   "settings": {
//!     "pitch": 26, "yaw": 28, "zoom": 1, "stretch": 2.0, "hscale": 0.7,
//!     "sup": true, "greeb": 0.6, "bands": 4, "laz": -50, "lel": 60,
//!     "res": { "w": 160, "h": 100 }
//!   },
//!   "grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }
//! }
//! ```
//!
//! ## Type choices
//!
//! - **Geometry points are `[f64; 2]` arrays on the wire.** The editor stores
//!   `plan` / `section` / `heightProfile` as arrays of two-element number
//!   arrays (`PLAN.map(p=>[+p[0],+p[1]])` at `broadside-loft-editor.html:747`).
//!   [`Point2`] is a newtype over `[f64; 2]` with `#[serde(transparent)]` so it
//!   round-trips as a bare `[x, y]` array, not a `{ "x":…, "y":… }` object —
//!   keeping the JSON byte-identical to what the tool emits. Named `.x()` /
//!   `.y()` accessors carry the per-list semantics (plan: x-along-length /
//!   half-width; section: z-half-width / y-height; heightProfile:
//!   x-along-length / height-multiplier).
//!
//! - **`heightProfile` is `Option<Vec<Point2>>`.** The tool writes literal
//!   `null` when no side-view image has been traced (`HEIGHTPROF=null`), which
//!   it treats as "flat 1.0 height everywhere". `None` must round-trip *as*
//!   JSON `null` (the field is always present in `collectDesign`'s output), so
//!   there is **no** `skip_serializing_if` here — mirrors the
//!   `SubsystemDef::unlock_salvage` `number | null` precedent in
//!   [`crate::types`]. `#[serde(default)]` is kept defensively for a future
//!   catalog that drops the key.
//!
//! - **`grade` values are `f64`.** The tool's `collectDesign` stores the live
//!   uniform `.value`s (`U.uHue.value` etc.), which are numbers after the
//!   slider `oninput` coercion (`(+e.target.value)/360`), with numeric defaults
//!   (`uHue:{value:0.0}` at `broadside-loft-editor.html:532`). They serialize
//!   as JSON numbers, so `f64` is exact.
//!
//! - **`res` is `{ w, h }` u32.** Pixel dimensions of the target sprite; the
//!   tool sets these from integer button data-attributes (`res:{w:160,h:100}`).
//!
//! ## Version tolerance
//!
//! [`ShipDesign::load_from_json`] parses **any** `version` value — it does not
//! reject a future schema at the struct level, so a v2 file with extra fields
//! still loads its v1 subset (serde ignores unknown fields by default). Callers
//! that care can check [`ShipDesign::is_known_version`] and warn / migrate. The
//! `format` tag is likewise parsed, not enforced, at the type level; a helper
//! [`ShipDesign::has_expected_format`] lets a caller mirror the tool's "this
//! isn't a Broadside ship design — load anyway?" confirm.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The `format` discriminator string the loft tool writes
/// (`broadside-loft-editor.html:720`).
pub const EXPECTED_FORMAT: &str = "broadside-ship";

/// The design-schema version this module was written against
/// (`DESIGN_VERSION = 1` at `broadside-loft-editor.html:717`).
pub const KNOWN_VERSION: u32 = 1;

/// A single 2-D point as the loft tool stores it: a bare `[x, y]` JSON array.
///
/// `#[serde(transparent)]` makes this serialize/deserialize as the inner
/// `[f64; 2]` directly, so `plan`, `section`, and `heightProfile` round-trip
/// byte-for-byte with the tool's `[[x, hw], ...]` shape. The two components
/// mean different things per list — see the field docs on [`ShipDesign`] and
/// the [`Point2::x`] / [`Point2::y`] accessors.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Point2(pub [f64; 2]);

impl Point2 {
    /// First component. For `plan` this is x-along-length (stern 0 → prow 1);
    /// for `section` it is z (half-width 0..1); for `heightProfile` it is
    /// x-along-length.
    pub fn x(self) -> f64 {
        self.0[0]
    }

    /// Second component. For `plan` this is half-width (0..1); for `section`
    /// it is y-height (-1..1, top → belly); for `heightProfile` it is the
    /// height multiplier (~0..1.5).
    pub fn y(self) -> f64 {
        self.0[1]
    }
}

impl From<[f64; 2]> for Point2 {
    fn from(p: [f64; 2]) -> Self {
        Point2(p)
    }
}

impl From<(f64, f64)> for Point2 {
    fn from((x, y): (f64, f64)) -> Self {
        Point2([x, y])
    }
}

/// Target sprite pixel dimensions (`settings.res` — `{ w, h }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub w: u32,
    pub h: u32,
}

/// Render / loft settings the tool stores under `settings`. Field names match
/// the JSON keys verbatim (`broadside-loft-editor.html:722-725`); a couple are
/// terse in the tool, so the doc comments spell them out.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Camera pitch in degrees.
    pub pitch: f64,
    /// Camera yaw in degrees.
    pub yaw: f64,
    /// Camera zoom multiplier.
    pub zoom: f64,
    /// Length stretch applied to the hull (`2.0` = double-length default).
    pub stretch: f64,
    /// Height scale applied to the section profile.
    pub hscale: f64,
    /// Superstructure toggle (`sup` in the tool) — whether the raised deck
    /// block is generated.
    pub sup: bool,
    /// Greeble density, 0..1 (surface detail amount).
    pub greeb: f64,
    /// Number of posterization / shading bands.
    pub bands: u32,
    /// Light azimuth in degrees (`laz`).
    pub laz: f64,
    /// Light elevation in degrees (`lel`).
    pub lel: f64,
    /// Output sprite resolution.
    pub res: Resolution,
}

/// Post-process colour grade (`grade` — HSV/contrast/gamma uniforms).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    /// Hue rotation, 0..1 (the tool divides the 0..360° slider by 360).
    pub hue: f64,
    /// Saturation multiplier (1.0 = unchanged).
    pub sat: f64,
    /// Brightness multiplier (1.0 = unchanged).
    pub bri: f64,
    /// Contrast multiplier (1.0 = unchanged).
    pub con: f64,
    /// Gamma (1.0 = unchanged; the shader does `pow(col, 1/gam)`).
    pub gam: f64,
}

/// A complete loft-editor ship design — the parsed form of the tool's saved
/// `.json`. Round-trips byte-stable with `collectDesign()`'s output (modulo
/// JSON whitespace, which is not semantically meaningful).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShipDesign {
    /// Format discriminator. The tool always writes `"broadside-ship"`; parsed
    /// not enforced here (see [`ShipDesign::has_expected_format`]).
    pub format: String,
    /// Schema version. The tool writes `1`; parsed permissively (see
    /// [`ShipDesign::is_known_version`]).
    pub version: u32,
    /// Top-down half-outline: `[x-along-length (stern 0 → prow 1), half-width]`
    /// points.
    pub plan: Vec<Point2>,
    /// Cross-section half-profile: `[z (half-width 0..1), y (height -1..1,
    /// top → belly)]` points.
    pub section: Vec<Point2>,
    /// Side-view height profile: `[x-along-length, height-mult]` points, or
    /// `null` for flat 1.0 height everywhere. See the module note on why this
    /// has no `skip_serializing_if`.
    #[serde(default, rename = "heightProfile")]
    pub height_profile: Option<Vec<Point2>>,
    /// Render / loft settings.
    pub settings: Settings,
    /// Post-process colour grade.
    pub grade: Grade,
}

/// Errors from parsing a ship-design `.json`.
#[derive(Debug)]
#[non_exhaustive]
pub enum DesignError {
    /// Couldn't read the file from disk (missing, permissions, etc.).
    Io(std::io::Error),
    /// serde_json rejected the bytes as a [`ShipDesign`] (malformed JSON, a
    /// missing required field, or a type mismatch).
    Parse(serde_json::Error),
}

impl std::fmt::Display for DesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesignError::Io(e) => write!(f, "ship-design io error: {e}"),
            DesignError::Parse(e) => write!(f, "ship-design parse error: {e}"),
        }
    }
}

impl std::error::Error for DesignError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DesignError::Io(e) => Some(e),
            DesignError::Parse(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for DesignError {
    fn from(e: std::io::Error) -> Self {
        DesignError::Io(e)
    }
}

impl ShipDesign {
    /// Parse a [`ShipDesign`] from in-memory JSON bytes (e.g. an `include_bytes!`
    /// asset or a network payload). Version-tolerant: any `version` parses;
    /// unknown extra fields are ignored by serde.
    pub fn load_from_json(bytes: &[u8]) -> Result<ShipDesign, DesignError> {
        serde_json::from_slice(bytes).map_err(DesignError::Parse)
    }

    /// Read and parse a [`ShipDesign`] from a `.json` file on disk.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<ShipDesign, DesignError> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::load_from_json(&bytes)
    }

    /// Serialize back to pretty-printed JSON, matching the 2-space indent the
    /// tool uses (`JSON.stringify(collectDesign(), null, 2)`).
    pub fn to_json_pretty(&self) -> Result<String, DesignError> {
        serde_json::to_string_pretty(self).map_err(DesignError::Parse)
    }

    /// Whether `format` is the expected `"broadside-ship"` tag. Callers can
    /// mirror the tool's "this isn't a Broadside ship design — load anyway?"
    /// confirm (`broadside-loft-editor.html:740`) rather than hard-failing.
    pub fn has_expected_format(&self) -> bool {
        self.format == EXPECTED_FORMAT
    }

    /// Whether `version` is the schema version this build understands
    /// ([`KNOWN_VERSION`]). A `false` here is a hint to warn / migrate, not a
    /// parse failure — newer files still load their compatible subset.
    pub fn is_known_version(&self) -> bool {
        self.version == KNOWN_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tool's default dagger hull, transliterated to the saved-JSON shape.
    /// Mirrors `collectDesign()` over the default `ST` / `PLAN` / `SECTION`
    /// (`broadside-loft-editor.html:276-286`, defaults at line 278). Whitespace
    /// is irrelevant; field set and values match a real "Save Design" click on
    /// a fresh tool load.
    const SAMPLE: &str = r#"{
        "format": "broadside-ship",
        "version": 1,
        "plan": [[0, 0.04], [0.2, 0.18], [0.6, 0.22], [1, 0]],
        "section": [[0, 1], [0.5, 0.2], [1, -1]],
        "heightProfile": null,
        "settings": {
            "pitch": 26, "yaw": 28, "zoom": 1, "stretch": 2.0, "hscale": 0.7,
            "sup": true, "greeb": 0.6, "bands": 4, "laz": -50, "lel": 60,
            "res": { "w": 160, "h": 100 }
        },
        "grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }
    }"#;

    #[test]
    fn parses_the_sample_design() {
        let d = ShipDesign::load_from_json(SAMPLE.as_bytes()).expect("sample parses");
        assert!(d.has_expected_format());
        assert!(d.is_known_version());
        assert_eq!(d.plan.len(), 4);
        assert_eq!(d.section.len(), 3);
        assert!(d.height_profile.is_none());
        // Settings landed on the right fields.
        assert_eq!(d.settings.pitch, 26.0);
        assert_eq!(d.settings.bands, 4);
        assert!(d.settings.sup);
        assert_eq!(d.settings.res, Resolution { w: 160, h: 100 });
        // Grade defaults.
        assert_eq!(d.grade.sat, 1.0);
        assert_eq!(d.grade.gam, 1.0);
    }

    #[test]
    fn point2_is_a_bare_array_on_the_wire() {
        // A Point2 must serialize as [x, y], not {"x":…,"y":…} — otherwise the
        // plan/section round-trip would diverge from the tool's output.
        let p = Point2([0.6, 0.22]);
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, "[0.6,0.22]");
        let back: Point2 = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.x(), 0.6);
        assert_eq!(back.y(), 0.22);
    }

    #[test]
    fn round_trips_through_serialize() {
        let d = ShipDesign::load_from_json(SAMPLE.as_bytes()).unwrap();
        let json = d.to_json_pretty().unwrap();
        let back = ShipDesign::load_from_json(json.as_bytes()).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn height_profile_none_round_trips_as_json_null() {
        // None must serialize AS null (not be omitted): the tool always writes
        // the key, and applyDesign reads `Array.isArray(d.heightProfile)`.
        let d = ShipDesign::load_from_json(SAMPLE.as_bytes()).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            json.contains(r#""heightProfile":null"#),
            "expected explicit heightProfile:null, got {json}",
        );
    }

    #[test]
    fn height_profile_some_parses_and_round_trips() {
        let with_hp = SAMPLE.replace(
            r#""heightProfile": null"#,
            r#""heightProfile": [[0, 1], [0.5, 1.3], [1, 0.8]]"#,
        );
        let d = ShipDesign::load_from_json(with_hp.as_bytes()).unwrap();
        let hp = d.height_profile.as_ref().expect("Some height profile");
        assert_eq!(hp.len(), 3);
        assert_eq!(hp[1].y(), 1.3);
        // Round-trips equal.
        let json = d.to_json_pretty().unwrap();
        let back = ShipDesign::load_from_json(json.as_bytes()).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn missing_height_profile_key_defaults_to_none() {
        // Defensive: a future tool version that drops the key entirely should
        // still parse (serde default), even though the current tool always
        // emits it.
        let no_hp = SAMPLE.replace(r#""heightProfile": null,"#, "");
        let d = ShipDesign::load_from_json(no_hp.as_bytes()).unwrap();
        assert!(d.height_profile.is_none());
    }

    #[test]
    fn future_version_still_parses_but_flags_unknown() {
        // Version tolerance: a v2 file (with an unknown extra field) loads its
        // compatible subset; is_known_version() reports the mismatch so a
        // caller can warn / migrate.
        let v2 = SAMPLE
            .replace(r#""version": 1"#, r#""version": 2"#)
            .replace(
                r#""grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }"#,
                r#""grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }, "futureField": 42"#,
            );
        let d = ShipDesign::load_from_json(v2.as_bytes()).expect("v2 still parses");
        assert_eq!(d.version, 2);
        assert!(!d.is_known_version(), "v2 should report unknown-version");
        assert!(d.has_expected_format());
    }

    #[test]
    fn wrong_format_parses_but_flags_format() {
        // The format tag is parsed, not enforced at the type level — mirrors
        // the tool's "load anyway?" confirm. A caller decides whether to bail.
        let wrong = SAMPLE.replace(
            r#""format": "broadside-ship""#,
            r#""format": "some-other-tool""#,
        );
        let d = ShipDesign::load_from_json(wrong.as_bytes()).expect("still parses");
        assert!(!d.has_expected_format());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = ShipDesign::load_from_json(b"{ not json").expect_err("should fail");
        assert!(matches!(err, DesignError::Parse(_)));
    }

    #[test]
    fn missing_required_field_is_a_parse_error() {
        // Dropping `settings` (a required field) must fail at parse, not
        // silently default — the loft path needs real camera/res values.
        let no_settings = SAMPLE.replace(
            r#""settings": {
            "pitch": 26, "yaw": 28, "zoom": 1, "stretch": 2.0, "hscale": 0.7,
            "sup": true, "greeb": 0.6, "bands": 4, "laz": -50, "lel": 60,
            "res": { "w": 160, "h": 100 }
        },"#,
            "",
        );
        let err = ShipDesign::load_from_json(no_settings.as_bytes())
            .expect_err("missing settings should reject");
        assert!(matches!(err, DesignError::Parse(_)));
    }
}
