# `src/save.rs` — per-run save / load

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/save.rs`](../LINE_BY_LINE.md#srcsavers) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

A roguelike campaign needs to survive the application closing mid-run. This
module persists **one** concern: the in-progress [`Run`](types.md) — the active
campaign state (current sector + encounter indices, salvage, the
defeated/victorious end-state flags, and the player [`Ship`](types.md) carried
across encounter boundaries). It is a small impl block adding three methods to
`Run` (`save_to_disk`, `load_from_disk`, `delete_save`) plus a `SaveError` enum.

It deliberately does **not** persist cross-run progression (unlocked subsystems,
total-salvage milestones). That lives in [`MetaProgression`](meta.md) and is
saved separately, on a different lifecycle. The split is load-bearing: deleting a
per-run save on Game-Over must **not** wipe the player's permanent unlocks. The
module docstring captures this with a table:

| concern | scope | format | when written |
|---|---|---|---|
| `Run` (this file) | one active run | JSON | per turn-commit, etc |
| `MetaProgression` (meta.rs) | cross-run | JSON | on encounter win, etc |

There is no TS analog — `demo.ts` has no persistence. This is Rust/Phase-3-only.

### Drift — JSON, not postcard

Task #79's brief asked for `postcard`. The module ships **JSON** instead, and the
docstring (src/save.rs:20-31) explains why: postcard cannot encode
internally-tagged enums, and [`Orientation`](types.md) is `#[serde(tag = "stance")]`
so the JSON catalog round-trips byte-stable with the TS engine. Adding
per-format serialize shims to `Orientation` would touch every catalog test in the
repo; CBOR would add a dependency for a ~5 KB save where the size win is
immaterial. JSON costs no new deps and matches the `MetaProgression` precedent.
The format asymmetry the brief anticipated turned out not to be load-bearing.

---

## `enum SaveError` (src/save.rs:69)

**Intent:** Three failure modes, kept distinct so a caller can react
appropriately. `Encode` vs `Decode` is the meaningful split: an encode failure
("couldn't write") suggests prompting the user or falling back to memory-only; a
decode failure ("couldn't make sense of the file we read") suggests prompting to
delete the save and start fresh.

Line 68: `#[non_exhaustive]` — leaves room for a future `Migration` variant.
Line 72: `Io(io::Error)` — read/write/remove failed (disk full, permissions).
Line 76: `Encode(serde_json::Error)` — serde rejected our in-memory `Run`
(shouldn't happen in practice; a malformed derive is a compile error first).
Line 81: `Decode(serde_json::Error)` — serde rejected the bytes on disk (newer
schema with no migration story yet, a partial write, or external corruption).

Line 84-102: `Display` one-liners and an `Error::source` impl returning the
wrapped cause. Line 104-106: `From<io::Error>` so `?` lifts I/O errors. Note
there is **no** blanket `From<serde_json::Error>` — that would be ambiguous
between `Encode` and `Decode`, so each call site maps explicitly with
`.map_err(SaveError::Encode)` / `.map_err(SaveError::Decode)`.

---

## `fn Run::save_to_disk(&self, path) -> Result<(), SaveError>` (src/save.rs:113)

**Intent:** Serialize the run to `path` as pretty-printed JSON, writing
**atomically** so a crash mid-write can never leave a partial save file.

Line 115-119: create the parent directory if it's missing and non-empty, so the
caller doesn't have to bootstrap the save folder.

Line 120: `serde_json::to_vec_pretty(self).map_err(SaveError::Encode)?` —
serialize to bytes; an (unexpected) serialize failure becomes `Encode`.

Line 121-123: the **atomic write**. Write the bytes to a sibling
`<path>.tmp` file, then `fs::rename(tmp, path)`. `fs::rename` is the
cross-platform atomic-replace primitive: the destination either has the complete
old contents or the complete new contents, never a half-written file.

**Cross-references:** Called by the demo bin on turn-commit / encounter
transitions. Uses `tmp_path_for` for the tmp filename.

**Worked example** (`save_writes_atomically_no_tmp_file_left_behind`,
src/save.rs:287): after a successful save the tmp file has been renamed away, so
only the final path exists — no cruft left in the save directory.

---

## `fn Run::load_from_disk(path) -> Result<Option<Run>, SaveError>` (src/save.rs:132)

**Intent:** Read a saved run. The `Option` return encodes the first-run case as
a non-error: `Ok(None)` means "no save exists yet," distinct from `Err` ("a save
exists but is broken").

Line 134-136: missing file → `Ok(None)`. Line 137: read the bytes. Line 138:
`serde_json::from_slice(...).map_err(SaveError::Decode)?` — a parse failure is a
`Decode` error (corrupt or incompatible schema). Line 139: `Ok(Some(run))`.

**Cross-references:** Called by the demo bin at startup. The bin treats `None` as
"start a fresh run," `Some` as "resume."

**Worked examples:** `save_then_load_roundtrips_a_run` (src/save.rs:242) — a
populated `Run` survives the round trip intact; `load_returns_none_when_no_file`
(src/save.rs:255) — missing file yields `None`; `corrupted_save_returns_decode_error`
(src/save.rs:273) — garbage bytes yield `SaveError::Decode`.

---

## `fn Run::delete_save(path) -> Result<(), SaveError>` (src/save.rs:147)

**Intent:** Remove the save file, **idempotently**. Intended for Game-Over: the
run is over, so burn the save and relaunch starts fresh. Crucially, it does
**not** touch the meta-progression save.

Line 149-153: `fs::remove_file`; an `Ok` or a `NotFound` error both return
`Ok(())` (already-absent is success), any other I/O error becomes `SaveError::Io`.

**Cross-references:** Called by the demo bin on run end. Does **not** touch
[`MetaProgression`](meta.md) — see the module docstring's lifecycle table.

**Worked example** (`delete_is_idempotent`, src/save.rs:263): deleting twice in a
row both succeed.

---

## `fn tmp_path_for(path: &Path) -> PathBuf` (src/save.rs:159)

**Intent:** Compute the temporary path used during the atomic-rename save: same
directory, same stem, with `.tmp` appended to the existing extension
(`run.json` → `run.json.tmp`) or `tmp` if the path had no extension. Keeping the
tmp file in the same directory guarantees the rename is same-filesystem (a rename
across filesystems is not atomic).

**Cross-references:** Called by `save_to_disk`; also used by the
`save_writes_atomically` test to assert the tmp file was cleaned up.
