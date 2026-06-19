//! EventBus-driven audio playback via `kira`.
//!
//! Subscribes to the five gameplay hooks bruce wants soundified:
//! `OnDamageDealt` / `OnLethal` / `OnVent` / `OnReorient` / `OnChainKill`.
//! Each hook plays a short, procedurally-synthesized placeholder sample;
//! when real audio assets drop into `assets/sounds/<name>.ogg`, the
//! [`AudioState`] loader transparently picks those up in preference to
//! the procedural fallback.
//!
//! ## Why renderer-owned
//!
//! The audio layer is a *renderer-tier* concern by the same logic that
//! lets the renderer own its own sprite pipeline: it consumes engine
//! events but does not modify engine state, runs synchronously on the
//! bus's calling thread (no separate audio task), and is feature-gated
//! so the resolver / content slices don't drag in the audio backend.
//! The `audio` Cargo feature is OFF by default; the demo binary turns
//! it on.
//!
//! ## Hook coverage (per team-lead, task #80)
//!
//! | Hook              | Sample slug      | Behavior |
//! |-------------------|------------------|----------|
//! | `OnDamageDealt`   | `"hit"`          | Short noise burst, sharp attack/decay |
//! | `OnLethal`        | `"explosion"`    | Low-freq sine + noise, exponential decay |
//! | `OnVent`          | `"vent"`         | Filtered hissing, ~400ms |
//! | `OnReorient`      | `"reorient"`     | Two-tone chirp |
//! | `OnChainKill`     | `"chain_kill"`   | Rising sine arpeggio |
//!
//! Per-hook closures register on the bus via [`install_on_bus`]; each
//! Restart re-installs since `Board::bus` is wiped with the rest of
//! the board state.
//!
//! ## Asset-override loader
//!
//! [`AudioState::new`] looks for `assets/sounds/<slug>.ogg` for each of
//! the five slugs above. Found files replace the corresponding
//! procedural sample. Missing files are silently skipped — the
//! procedural placeholder stays in place. This is the same
//! degraded-mode pattern used by [`crate::sprites`] for ship PNGs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
use kira::sound::FromFileError;
use kira::{AudioManager, AudioManagerSettings, DefaultBackend};

use crate::types::{Board, Hook, HookContext};

/// Default sample rate for procedural placeholder sounds. 44100 is the
/// industry standard; matches what `cpal` defaults to on most desktop
/// devices so no resampling happens before the audio device.
const SAMPLE_RATE: u32 = 44100;

/// Five named placeholder sound slugs. Order matches the
/// hook-coverage table in the module docstring.
const SLUGS: [&str; 5] = ["hit", "explosion", "vent", "reorient", "chain_kill"];

/// Index of each slug into `AudioState::samples`.
const HIT: usize = 0;
const EXPLOSION: usize = 1;
const VENT: usize = 2;
const REORIENT: usize = 3;
const CHAIN_KILL: usize = 4;

/// Owns the kira `AudioManager` plus a fixed-size table of preloaded
/// sound samples (one per slug). Cheap to `clone` via `Rc<RefCell<>>`
/// so each EventBus closure gets its own handle without ownership
/// gymnastics.
///
/// The five sample slots are populated at construction:
///   1. Start with the procedural fallback for each slot
///      ([`procedural_sample`]).
///   2. Walk `assets/sounds/<slug>.ogg`; on success, replace the slot.
///
/// Errors loading the kira `AudioManager` (no audio device, missing
/// driver, etc.) collapse to `None` — the bin treats that as
/// "audio disabled this session" and skips the bus install. This is a
/// non-crash degraded mode; same shape as the procedural-sprite
/// fallback.
pub struct AudioState {
    manager: AudioManager<DefaultBackend>,
    samples: [StaticSoundData; 5],
}

impl AudioState {
    /// Initialize the audio backend and preload the five samples. Pass
    /// `asset_dir.join("sounds")` for the asset override path; missing
    /// files are skipped and the procedural fallback stays in place.
    /// Returns `None` if the audio device fails to open (headless CI,
    /// missing driver, etc.).
    pub fn new(asset_dir: &Path) -> Option<Self> {
        let manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("audio disabled: failed to open audio device: {e}");
                return None;
            }
        };
        let samples = [
            load_or_fallback(asset_dir, SLUGS[HIT], procedural_hit),
            load_or_fallback(asset_dir, SLUGS[EXPLOSION], procedural_explosion),
            load_or_fallback(asset_dir, SLUGS[VENT], procedural_vent),
            load_or_fallback(asset_dir, SLUGS[REORIENT], procedural_reorient),
            load_or_fallback(asset_dir, SLUGS[CHAIN_KILL], procedural_chain_kill),
        ];
        Some(Self { manager, samples })
    }

    /// Trigger sample `idx`. Logs a debug-level message on backend
    /// error (which can happen mid-play if the audio device unplugs)
    /// but otherwise is fire-and-forget.
    fn play(&mut self, idx: usize) {
        let sample = self.samples[idx].clone();
        if let Err(e) = self.manager.play(sample) {
            log::debug!("audio play failed for slug index {idx}: {e}");
        }
    }
}

/// Install the per-hook closures on `board.bus`. Call this once after
/// every Restart since `Board::bus` is rebuilt with the rest of the
/// board state.
pub fn install_on_bus(board: &mut Board, audio: Rc<RefCell<AudioState>>) {
    {
        let a = Rc::clone(&audio);
        board
            .bus
            .on(Hook::OnDamageDealt, move |_ctx: &mut HookContext| {
                a.borrow_mut().play(HIT);
            });
    }
    {
        let a = Rc::clone(&audio);
        board.bus.on(Hook::OnLethal, move |_ctx: &mut HookContext| {
            a.borrow_mut().play(EXPLOSION);
        });
    }
    {
        let a = Rc::clone(&audio);
        board.bus.on(Hook::OnVent, move |_ctx: &mut HookContext| {
            a.borrow_mut().play(VENT);
        });
    }
    {
        let a = Rc::clone(&audio);
        board
            .bus
            .on(Hook::OnReorient, move |_ctx: &mut HookContext| {
                a.borrow_mut().play(REORIENT);
            });
    }
    {
        let a = Rc::clone(&audio);
        board
            .bus
            .on(Hook::OnChainKill, move |_ctx: &mut HookContext| {
                a.borrow_mut().play(CHAIN_KILL);
            });
    }
}

/* =============================================================================
 * Asset loader: try real file first, fall back to procedural synthesis.
 * ============================================================================= */

fn sound_path(asset_dir: &Path, slug: &str) -> PathBuf {
    asset_dir.join("sounds").join(format!("{slug}.ogg"))
}

fn load_or_fallback(
    asset_dir: &Path,
    slug: &str,
    fallback: fn() -> StaticSoundData,
) -> StaticSoundData {
    let path = sound_path(asset_dir, slug);
    match StaticSoundData::from_file(&path) {
        Ok(data) => {
            log::info!("audio: loaded {}", path.display());
            data
        }
        Err(FromFileError::IoError(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            log::debug!(
                "audio: no asset at {}, using procedural fallback",
                path.display()
            );
            fallback()
        }
        Err(e) => {
            log::warn!("audio: failed to load {}: {e:?}", path.display());
            fallback()
        }
    }
}

/* =============================================================================
 * Procedural placeholder sample synthesis.
 *
 * Five short waveforms generated as mono f32 PCM, then wrapped in kira's
 * StaticSoundData. None of these are real sound design — they're audible
 * placeholders so bruce can hear the bus firing and identify which hook
 * triggered. Replace with real .ogg in assets/sounds/ when ready.
 * ============================================================================= */

/// Build a StaticSoundData from a mono f32 sample buffer. Both stereo
/// channels carry the same signal (kira 0.10 frames are `kira::Frame
/// { left: f32, right: f32 }`).
fn build_sound(samples: Vec<f32>) -> StaticSoundData {
    let frames: Arc<[kira::Frame]> = samples
        .into_iter()
        .map(|s| kira::Frame { left: s, right: s })
        .collect::<Vec<_>>()
        .into();
    StaticSoundData {
        sample_rate: SAMPLE_RATE,
        frames,
        settings: StaticSoundSettings::default(),
        slice: None,
    }
}

/// Procedural-sample dispatch table. Used by `procedural_sample` on
/// debug builds + test scaffolding; production goes through
/// `load_or_fallback`.
#[allow(dead_code)]
fn procedural_sample(slug: &str) -> StaticSoundData {
    match slug {
        "hit" => procedural_hit(),
        "explosion" => procedural_explosion(),
        "vent" => procedural_vent(),
        "reorient" => procedural_reorient(),
        "chain_kill" => procedural_chain_kill(),
        _ => build_sound(Vec::new()),
    }
}

/// Short noise burst, ~150ms. Sharp attack, exponential decay.
fn procedural_hit() -> StaticSoundData {
    let n = (SAMPLE_RATE as f32 * 0.15) as usize;
    let mut rng = WangLcg::new(0x1234_5678);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let env = (-6.0 * t).exp(); // exponential decay
        let noise = (rng.next_f32() - 0.5) * 2.0;
        out.push(noise * env * 0.5);
    }
    build_sound(out)
}

/// Low-freq sine + noise, ~600ms. Exponential decay; reads as an
/// explosion thump-with-tail.
fn procedural_explosion() -> StaticSoundData {
    let n = (SAMPLE_RATE as f32 * 0.60) as usize;
    let mut rng = WangLcg::new(0xdead_beef);
    let mut out = Vec::with_capacity(n);
    let freq = 55.0; // low A
    for i in 0..n {
        let t = i as f32 / n as f32;
        let phase = 2.0 * std::f32::consts::PI * freq * (i as f32 / SAMPLE_RATE as f32);
        let sine = phase.sin();
        let noise = (rng.next_f32() - 0.5) * 2.0;
        let env = (-3.5 * t).exp();
        out.push((sine * 0.6 + noise * 0.4) * env * 0.7);
    }
    build_sound(out)
}

/// Hissing noise band-passed to mid frequencies (cheap fake: white
/// noise multiplied by a slow oscillator to imitate filter sweep).
fn procedural_vent() -> StaticSoundData {
    let n = (SAMPLE_RATE as f32 * 0.40) as usize;
    let mut rng = WangLcg::new(0xcafe_babe);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        // Attack 30ms, sustain, decay last 200ms.
        let env = if t < 0.075 {
            t / 0.075
        } else if t > 0.5 {
            (1.0 - t) / 0.5
        } else {
            1.0
        };
        let noise = (rng.next_f32() - 0.5) * 2.0;
        let sweep = (2.0 * std::f32::consts::PI * 4.0 * t).sin() * 0.3 + 0.7;
        out.push(noise * env * sweep * 0.35);
    }
    build_sound(out)
}

/// Two-tone chirp: low pitch sliding up to mid, ~200ms. Reads as a
/// quick mechanical reorient sound.
fn procedural_reorient() -> StaticSoundData {
    let n = (SAMPLE_RATE as f32 * 0.20) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let freq = 220.0 + 660.0 * t; // sweep A3 → ~A5
        let phase = 2.0 * std::f32::consts::PI * freq * (i as f32 / SAMPLE_RATE as f32);
        let env = (1.0 - t).powi(2);
        out.push(phase.sin() * env * 0.4);
    }
    build_sound(out)
}

/// Rising sine arpeggio (root / third / fifth), ~400ms. The "combo"
/// signal — fires when a single damage event triggers a cascade.
fn procedural_chain_kill() -> StaticSoundData {
    let n = (SAMPLE_RATE as f32 * 0.40) as usize;
    let mut out = Vec::with_capacity(n);
    let freqs = [440.0_f32, 554.37, 659.25]; // A4 / C#5 / E5
    for i in 0..n {
        let t = i as f32 / n as f32;
        let note_idx = ((t * 3.0).floor() as usize).min(2);
        let freq = freqs[note_idx];
        let phase = 2.0 * std::f32::consts::PI * freq * (i as f32 / SAMPLE_RATE as f32);
        let local_t = (t * 3.0).fract();
        let env = (1.0 - local_t).powi(2);
        out.push(phase.sin() * env * 0.4);
    }
    build_sound(out)
}

/// Deterministic Wang-hash LCG for procedural-noise generation. Same
/// pattern as `atlas.rs` uses for the starfield — fixed seed → same
/// noise every build → visual+audio regression testing remains
/// deterministic. Returns f32 in `[0, 1)`.
struct WangLcg(u32);

impl WangLcg {
    fn new(seed: u32) -> Self {
        // Wang hash to avoid bad-seed patterns at low entropy.
        let mut x = seed;
        x = (x ^ 61).wrapping_mul(0x27d4eb2d);
        x = x ^ (x >> 16);
        x = x.wrapping_mul(0x85ebca6b);
        Self(x.max(1))
    }
    fn next_f32(&mut self) -> f32 {
        // xorshift32, then convert to f32 in [0, 1).
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32) / (u32::MAX as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wang_lcg_is_deterministic() {
        let mut a = WangLcg::new(42);
        let mut b = WangLcg::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_f32(), b.next_f32());
        }
    }

    #[test]
    fn wang_lcg_outputs_in_unit_interval() {
        let mut rng = WangLcg::new(1);
        for _ in 0..1000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v), "got {v} outside [0,1)");
        }
    }

    #[test]
    fn procedural_hit_has_correct_length() {
        let s = procedural_hit();
        // 150ms at 44100Hz = 6615 frames.
        let expected = (SAMPLE_RATE as f32 * 0.15) as usize;
        assert_eq!(s.frames.len(), expected);
        assert_eq!(s.sample_rate, SAMPLE_RATE);
    }

    #[test]
    fn procedural_samples_all_finite() {
        // Smoke check: every procedural sample should produce only
        // finite f32 values across both stereo channels. Catches NaN
        // / inf introduced by future math regressions before kira's
        // mixer chokes on them.
        for slug in &SLUGS {
            let sd = procedural_sample(slug);
            for f in sd.frames.iter() {
                assert!(f.left.is_finite(), "non-finite left sample in {slug}");
                assert!(f.right.is_finite(), "non-finite right sample in {slug}");
            }
        }
    }

    #[test]
    fn sound_path_format() {
        let p = sound_path(Path::new("assets"), "explosion");
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(s, "assets/sounds/explosion.ogg");
    }
}
