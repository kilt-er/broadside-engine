# `src/audio.rs` — EventBus-driven sound via kira

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/audio.rs`](../LINE_BY_LINE.md#srcaudiors) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the sound layer (task #80). It subscribes closures to five gameplay hooks
on the [`EventBus`](types.md) — `OnDamageDealt` / `OnLethal` / `OnVent` /
`OnReorient` / `OnChainKill` — and plays a short sample when each fires. Samples
are **procedurally synthesized placeholders** by default; if a real `.ogg` lands
in `assets/sounds/<slug>.ogg`, the loader transparently uses it instead (the same
degraded-mode pattern [`sprites.rs`](sprites.md) uses for ship PNGs).

It is **renderer-tier and feature-gated**: it consumes engine events but never
mutates engine state, runs synchronously on the bus's calling thread (no separate
audio task), and lives behind the `audio` Cargo feature (off by default; the demo
binary turns it on). So the resolver/content slices never drag in the audio
backend. No TS analog — sound is Rust-side.

### Hook → sample table

| Hook | Slug | Sound |
|---|---|---|
| `OnDamageDealt` | `hit` | short noise burst |
| `OnLethal` | `explosion` | low sine + noise, decay |
| `OnVent` | `vent` | filtered hiss |
| `OnReorient` | `reorient` | two-tone chirp |
| `OnChainKill` | `chain_kill` | rising arpeggio |

---

## `struct AudioState` (src/audio.rs:84)

**Intent:** Owns the kira `AudioManager` plus a fixed 5-slot table of preloaded
`StaticSoundData` (one per slug, indexed by the `HIT`/`EXPLOSION`/… consts at
src/audio.rs:63). Wrapped in `Rc<RefCell<>>` by the bin so each bus closure gets a
cheap handle.

### `fn AudioState::new(asset_dir) -> Option<Self>` (src/audio.rs:95)

Opens the audio backend; **returns `None`** if the device fails to open (headless
CI, no driver) — the bin treats that as "audio disabled this session" and skips the
bus install (a non-crash degraded mode). Populates the five slots via
`load_or_fallback` (procedural default, real `.ogg` if present).

### `fn AudioState::play(idx)` (src/audio.rs:116)

Fire-and-forget: clone the sample, `manager.play`, log at `debug` on backend error
(the device can unplug mid-play).

---

## `fn install_on_bus(board, audio)` (src/audio.rs:127)

**Intent:** Register the five per-hook closures on `board.bus`. Each clones the
`Rc<RefCell<AudioState>>` and registers a `move` closure that calls `play(<slug
index>)` on its hook. **Must be called after every Restart** because `Board::bus`
is rebuilt with the rest of the board state — which is exactly what
[`broadside.rs`](broadside.md)'s `reinstall_audio` does.

**Cross-references:** Subscribes to [`EventBus`](types.md) hooks emitted by
[`resolve.rs`](resolve.md) (`OnDamageDealt`, `OnLethal`, etc.). Called by
`broadside.rs::App::new` and `reinstall_audio`.

---

## Asset loader (src/audio.rs:164–188)

`sound_path` builds `<dir>/sounds/<slug>.ogg`. `load_or_fallback` tries
`StaticSoundData::from_file`; a `NotFound` IO error falls back to the procedural
sample silently (logged at `debug`), other errors fall back with a `warn`. Pinned by
`sound_path_format` (src/audio.rs:387).

---

## Procedural sample synthesis (src/audio.rs:190–339)

Five short mono-f32 waveforms wrapped in kira's `StaticSoundData` via `build_sound`
(src/audio.rs:202, duplicates the mono signal to both stereo channels). These are
audible placeholders, not real sound design — they let bruce hear which hook fired:

- `procedural_hit` (231) — ~150 ms noise burst, exponential decay.
- `procedural_explosion` (247) — ~600 ms 55 Hz sine + noise, decay.
- `procedural_vent` (265) — ~400 ms hiss with a fake filter sweep.
- `procedural_reorient` (284) — ~200 ms 220→880 Hz chirp.
- `procedural_chain_kill` (299) — ~400 ms A4/C#5/E5 rising arpeggio.

`procedural_sample` (src/audio.rs:220) is the slug→fn dispatch used by tests.
`WangLcg` (src/audio.rs:319) is a deterministic Wang-hash + xorshift32 RNG (same
pattern as `atlas.rs`'s starfield) so the noise is identical every build — keeping
audio regression-testable.

**Worked examples:** `wang_lcg_is_deterministic` (src/audio.rs:345),
`procedural_hit_has_correct_length` (src/audio.rs:363, 150 ms × 44100 = 6615
frames), `procedural_samples_all_finite` (src/audio.rs:372, NaN/inf guard so kira's
mixer never chokes).
