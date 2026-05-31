# Review: src/save.rs (Phase-3 per-run save/load)

Reviewer audit (task #9, extended pass — formalizing the #79 spot-approval).
Rust-native Phase-3 module. Status: **APPROVE.** No findings.

## Verified

- **Format: JSON, not postcard** — justified. postcard can't encode internally-tagged enums, and `Orientation` is `#[serde(tag = "stance")]`. JSON costs no new deps, matches the MetaProgression precedent, round-trips byte-stable with the TS-shaped catalog. The size win of a binary format on a ~5 KB save is immaterial. Documented in the module + Cargo.toml.
- **Atomicity** — `save_to_disk` writes via `<path>.tmp` + `fs::rename` (cross-platform atomic replace), so a crash mid-write never leaves a partial save. Parent dirs created if missing. Tested (no tmp file left behind on the happy path).
- **load_from_disk** — Ok(Some) on parse, Ok(None) if file absent (first-run), Err(Decode) on corrupt/incompatible bytes. The None-vs-Err split is the right ergonomics for the caller.
- **SaveError** — `#[non_exhaustive]`, separates Io / Encode / Decode so callers distinguish "can't write" from "can't read this back." Implements Error + Display + source(). From<io::Error>. Clean.
- **delete_save** — idempotent (Ok if already absent), does NOT touch the meta save (separate lifecycle — deleting a run save on Game-Over must not reset cross-run progression). Correct separation, mirrors meta.rs's opposite-direction guarantee.
- **Run carries player: Ship** — the #75 follow-up. Persists hull/heat/subsystems/statuses/salvage across encounter boundaries so an alt-tab+kill-process can't save-scum a fresh full-hull ship. Run lost Copy/Eq/Hash (Ship has heap fields) but kept Clone+PartialEq; nothing depended on the dropped bounds. 5 round-trip tests.

## Note

- A test comment (save.rs:277) still says "postcard cannot decode as a Run" though the format is now JSON — cosmetic only, the test writes garbage bytes and asserts Decode error regardless of format. Lead parked it to fold into the next save.rs touch (same class as the lib.rs:41 comment architect already fixed). Non-blocking.
