//! JSON-backed save/load for the in-progress [`crate::types::Run`].
//!
//! # Scope and lifecycle
//!
//! This module persists ONE concern: the per-run state. That's the active
//! [`Run`] — current sector + encounter indices, salvage, end-state flags,
//! and the player Ship (carried across encounter boundaries by design;
//! see `Run`'s docstring for why `player: Ship` lives on `Run` itself).
//!
//! Cross-run progression — unlocked subsystems, total-salvage milestones —
//! lives in [`crate::meta::MetaProgression`] and is persisted separately.
//! Deleting a per-run save (e.g. on Game-Over) MUST NOT touch the meta
//! save. The two lifecycles diverge:
//!
//! | concern         | scope             | format | when written           |
//! |-----------------|-------------------|--------|------------------------|
//! | `Run` (here)    | one active run    | JSON   | per turn-commit, etc   |
//! | `MetaProgression` (meta.rs) | cross-run       | JSON   | on encounter win, etc  |
//!
//! ## Why JSON, not postcard
//!
//! Task #79's brief asked for postcard. Postcard cannot encode
//! internally-tagged enums (a documented format limitation), and
//! [`crate::types::Orientation`] is `#[serde(tag = "stance")]` so the
//! JSON catalog round-trips byte-stable with the TS engine. Adding
//! per-format serialize shims to `Orientation` would be invasive (it's
//! exercised by every catalog test in the repo). CBOR (`ciborium`)
//! would work but adds a dep for a ~5 KB save where the size win is
//! immaterial. JSON Just Works, costs no new deps, and matches the
//! `MetaProgression` precedent — the format asymmetry the brief
//! anticipated isn't load-bearing here.
//!
//! ## Atomicity
//!
//! [`Run::save_to_disk`] writes via a tmp file + rename so a crash
//! mid-write never leaves a partial save file. The rename is `std::fs`'s
//! cross-platform atomic-replace.
//!
//! ## Path discovery
//!
//! No path policy here; callers pick the path. The demo bin picks
//! something like `<exe_dir>/run.bin`; future platform-app-data dirs
//! (`%APPDATA%\Broadside\` on Windows, `~/.config/broadside/` on Linux)
//! land via a small `directories` / `etcetera` adapter when needed.
//!
//! ## API surface
//!
//! All three operations are methods on [`Run`]:
//! - [`Run::save_to_disk`] — serialize `self` to `path` (atomic write).
//! - [`Run::load_from_disk`] — return `Ok(Some(run))` on a parsed save,
//!   `Ok(None)` if the file doesn't exist (first-run case).
//! - [`Run::delete_save`] — remove the file, idempotent.

use std::fs;
use std::io;
use std::path::Path;

use crate::types::Run;

/// Errors from the save/load round trip.
///
/// `Encode` and `Decode` are separated so callers can distinguish a
/// "we couldn't write to disk" failure (likely actionable: prompt the
/// user, fall back to memory-only) from a "we couldn't make sense of
/// the file we just read" failure (likely actionable: prompt to delete
/// the save and start fresh).
#[derive(Debug)]
#[non_exhaustive]
pub enum SaveError {
    /// Couldn't read/write/remove the save file (disk full, permissions,
    /// etc.). Wraps the underlying `io::Error`.
    Io(io::Error),
    /// serde_json rejected our in-memory `Run`. Should not happen in
    /// practice — serde derives don't fail unless the type itself is
    /// malformed, which is a compile error first.
    Encode(serde_json::Error),
    /// serde_json rejected the bytes on disk. Either the file is from a
    /// newer schema (no migration story yet — Phase 3 is pre-1.0), the
    /// file is partially written (atomic rename failed somehow), or
    /// the bytes were corrupted by something outside our control.
    Decode(serde_json::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "save io error: {e}"),
            SaveError::Encode(e) => write!(f, "save encode error: {e}"),
            SaveError::Decode(e) => write!(f, "save decode error: {e}"),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveError::Io(e) => Some(e),
            SaveError::Encode(e) => Some(e),
            SaveError::Decode(e) => Some(e),
        }
    }
}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        SaveError::Io(e)
    }
}

impl Run {
    /// Serialize `self` to `path` as pretty-printed JSON. Writes atomically
    /// through a `<path>.tmp` file + rename so a crash mid-write cannot
    /// leave a partially-written save on disk. Parent directories are
    /// created if missing.
    pub fn save_to_disk(&self, path: impl AsRef<Path>) -> Result<(), SaveError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(SaveError::Encode)?;
        let tmp = tmp_path_for(path);
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read a saved [`Run`] from `path`. Returns:
    /// - `Ok(Some(run))` on a successful parse,
    /// - `Ok(None)` if the file doesn't exist (first-run case),
    /// - `Err(SaveError::Decode)` if the file is unreadable as a `Run`
    ///   (corrupt or from an incompatible schema version).
    pub fn load_from_disk(path: impl AsRef<Path>) -> Result<Option<Run>, SaveError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        let run: Run = serde_json::from_slice(&bytes).map_err(SaveError::Decode)?;
        Ok(Some(run))
    }

    /// Remove the save file at `path`. Idempotent — `Ok(())` if the file
    /// is already absent. Intended for Game-Over: the run is over,
    /// burn the save so relaunch starts a fresh run.
    ///
    /// Does NOT touch the meta-progression save. See module docstring.
    pub fn delete_save(path: impl AsRef<Path>) -> Result<(), SaveError> {
        let path = path.as_ref();
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SaveError::Io(e)),
        }
    }
}

/// Compute the tmp path used during atomic-rename saves. Same parent
/// directory as `path`, same stem, suffix `.tmp`.
fn tmp_path_for(path: &Path) -> std::path::PathBuf {
    let mut tmp = path.to_path_buf();
    let new_ext = match tmp.extension() {
        Some(ext) => {
            let mut combined = ext.to_os_string();
            combined.push(".tmp");
            combined
        }
        None => std::ffi::OsString::from("tmp"),
    };
    tmp.set_extension(new_ext);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Arc, Faction, LaneEnd, Mount, Orientation, ShieldFace, ShieldProfile, Ship, Status,
        StatusKind, Trait,
    };
    use std::collections::HashMap;

    fn sample_player() -> Ship {
        Ship {
            id: "player".into(),
            faction: Faction::Player,
            cell: 0,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hull: 4,
            max_hull: 5,
            heat: 2,
            heat_max: 6,
            locked_out: false,
            shield_profile: ShieldProfile {
                bow: ShieldFace {
                    armour: 2,
                    charge: 1,
                },
                stern: ShieldFace {
                    armour: 0,
                    charge: 0,
                },
                port: ShieldFace {
                    armour: 1,
                    charge: 0,
                },
                starboard: ShieldFace {
                    armour: 1,
                    charge: 0,
                },
            },
            mounts: vec![Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            }],
            queue: vec!["pulse_laser".into()],
            cooldowns: {
                let mut m = HashMap::new();
                m.insert("torpedo".into(), 2);
                m
            },
            statuses: vec![Status {
                kind: StatusKind::TargetLock,
                duration: 1,
                face: None,
            }],
            traits: vec![Trait::Agile],
            klass: Some("wanderer".into()),
        }
    }

    fn sample_run() -> Run {
        let mut run = Run::new(sample_player());
        run.current_sector_idx = 1;
        run.completed_encounters = 2;
        run.salvage = 13;
        run
    }

    /// Generate a unique tmpdir-scoped save path. We avoid pulling in
    /// `tempfile` for one test module and rely on `std::env::temp_dir()`
    /// plus a per-test suffix (the test's name) for isolation.
    fn tmp_save_path(suffix: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Mix in process id so concurrent test runs (e.g. on a shared
        // CI machine) don't collide on the same file.
        let pid = std::process::id();
        p.push(format!("broadside_save_test_{pid}_{suffix}.bin"));
        // Pre-clean if a previous run left this around (test crash, etc).
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn save_then_load_roundtrips_a_run() {
        let path = tmp_save_path("roundtrips");
        let run = sample_run();

        run.save_to_disk(&path).expect("save");
        let loaded = Run::load_from_disk(&path).expect("load");
        assert_eq!(loaded.as_ref(), Some(&run));

        // Cleanup.
        let _ = Run::delete_save(&path);
    }

    #[test]
    fn load_returns_none_when_no_file() {
        let path = tmp_save_path("none");
        // Don't write anything.
        let loaded = Run::load_from_disk(&path).expect("load no-file");
        assert!(loaded.is_none());
    }

    #[test]
    fn delete_is_idempotent() {
        let path = tmp_save_path("delete_idempotent");
        let run = sample_run();
        run.save_to_disk(&path).expect("save");
        Run::delete_save(&path).expect("first delete");
        // Second delete on a now-absent file is Ok, not an error.
        Run::delete_save(&path).expect("second delete is idempotent");
    }

    #[test]
    fn corrupted_save_returns_decode_error() {
        let path = tmp_save_path("corrupt");
        // Write bytes that postcard cannot decode as a Run.
        std::fs::write(&path, b"this is not a postcard-encoded Run").expect("write garbage");
        let err = Run::load_from_disk(&path).expect_err("should fail to decode");
        match err {
            SaveError::Decode(_) => {}
            other => panic!("expected Decode error, got {other:?}"),
        }
        // Cleanup.
        let _ = Run::delete_save(&path);
    }

    #[test]
    fn save_writes_atomically_no_tmp_file_left_behind() {
        let path = tmp_save_path("atomic");
        let run = sample_run();
        run.save_to_disk(&path).expect("save");

        // Save succeeded → the tmp file should have been renamed away,
        // leaving only `path`. This prevents accidental cruft in the
        // save directory on a happy path.
        let tmp = tmp_path_for(&path);
        assert!(path.exists(), "final save file should exist");
        assert!(!tmp.exists(), "tmp file should have been renamed away");

        // Cleanup.
        let _ = Run::delete_save(&path);
    }
}
