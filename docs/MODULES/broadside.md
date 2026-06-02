# `src/bin/broadside.rs` — the runnable demo binary

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/bin/broadside.rs`](../LINE_BY_LINE.md#srcbinbroadsidesrs) section of
`LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **only executable** in the crate — everything else is library. It
opens a window via `winit`, owns the wgpu renderer ([`gfx.rs`](gfx.md)), holds
the live [`Board`](types.md) + [`Run`](types.md), and wires keyboard input
through the engine's pure `input` → `Intent` → resolver pipeline so the library
never has to know `winit` exists. The initial scene mirrors the TS
`render-example.ts` (7-cell lane, player at cell 0, four enemies).

Its job is **orchestration, not game logic**: it translates `winit` keycodes to
engine `Key`s, asks the engine what each key means, applies the resulting
`Intent` to the board, runs the Phase-3 campaign state machine
(`DemoState`), drives the per-ship movement tweens, and composes + renders each
frame. All the actual combat/movement/AI math lives in the library
([`resolve.rs`](resolve.md), [`runs.rs`](runs.md), etc.).

No direct TS analog — `demo.ts` was a headless console script. This is the
interactive winit/wgpu front end the Rust port grew.

### Controls (canonical map in `input::key_to_intent`)

`1`/`2`/`3` queue mount actions; `5`/`6`/`7` play field-kit cards;
`←`/`→` move; `Tab` reorient-flip; `V` vent; `R`/`Space` commit turn;
`Enter` restart (and the only key the run-end overlays accept); `[`/`]` cycle
camera angle through `[0,15,30,45,60,75,90]°`; `Esc` exits. While the
EncounterComplete overlay is up, `1`/`2`/`3` are overloaded as
repair / upgrade / continue.

---

## `fn keycode_to_key(code: KeyCode) -> Option<Key>` (src/bin/broadside.rs:75)

**Intent:** Translate a `winit::KeyCode` into the engine's `input::Key`. Lives in
the bin precisely so the **library never imports winit**. One match arm per
advertised binding; everything else → `None` (key ignored). Pinned exhaustively
by `keycode_translation_covers_every_binding` (src/bin/broadside.rs:861) and
`keycode_translation_returns_none_for_unbound` (src/bin/broadside.rs:878).

---

## `fn apply_intent(intent, board, content, initial_board) -> bool` (src/bin/broadside.rs:112)

**Intent:** Apply one `Intent` to the board under **Shogun-Showdown turn
semantics** — *every input advances time* (runs phases 2-4 via
`run_world_phase`). Returns `true` if the visible state changed (the renderer
redraws on `true`). This is the heart of the bin's game loop.

Line 119-122: **Restart** short-circuits — it never advances time, it discards
and rebuilds the whole board from `initial_board()`. Line 126-128: every other
intent needs the player; if the player is gone (defeat), only Restart is legal,
everything else no-ops.

The `match intent`:
- **Instant intents** `MoveLeft`/`MoveRight`/`Vent`: resolve the synthetic action
  id, clone its `Action` from `content`, apply it atomically via
  `apply_instant_action`, then `run_world_phase`. These take effect *on the press*.
- **`ReorientFlip`** — its **own arm** since #52 (commit 83503f6). bruce wanted Tab
  to be a **90° turn that toggles bow-on ↔ broadside and stops perpendicular**, NOT
  the 180° bow Fore↔Aft about-face the static `__reorient_flip` synthetic
  (`ReorientTo::Flip`) encodes. So the bin reads the player's current orientation,
  picks the target stance (bow-on → Broadside, broadside → BowOn), and **overrides
  the synthetic's REORIENT effect** with that target — the synthetic still supplies
  the action's name/cost/targeting. **Bin-local**: no resolve.rs / AI change (enemy
  reorient still uses its own action def). Reaching bow-Aft via control is a
  deferred follow-up. (Pairs with loft_gpu's `REORIENT_SECS` drop to 0.28 — the
  tween already takes the shortest path, now a clean 90° with no 180° over-spin.)
- **`PlayCard`** (151-170): validate + decrement charges via
  `content.try_play_card`; on `Played`, run the synthetic `__card_<id>` action
  instantly then advance the world. Other `PlayResult`s (UnknownCard /
  NotCarried / InsufficientCharges) → `false`.
- **`QueueAction`** (175-182): push the action id to the player's queue (via
  `append_to_player_queue`) — **not fired here**; the player commits later. Time
  still advances.
- **`CommitTurn`** (186-190): fire whatever is queued (empty queue = Wait) via
  `fire_player_queue`, then world phase.

**Drift — SS turn model.** Pre-Shogun-Showdown, moves/cards were queued and only
fired on commit; now they're instant and the queue holds only `QueueAction`
weapon ids. The tests embed this contrast (e.g. `move_intent_advances_ship_instantly`,
src/bin/broadside.rs:898, asserts the queue stays empty after a move).

**Cross-references:** Calls into [`resolve.rs`](resolve.md)'s
`apply_instant_action` / `fire_player_queue` / `run_world_phase` and
[`input`](../LINE_BY_LINE.md#srcinputrs)'s `intent_to_action_id` /
`synthetic_card_action_id`. **Worked examples:**
`queue_action_intent_appends_to_player_queue` (885),
`commit_turn_runs_resolve_round` (917), `play_card_intent_fires_instantly_and_decrements_charges`
(972), `play_card_intent_rejected_when_card_absent` (1019).

## `fn append_to_player_queue(board, action_id) -> bool` (src/bin/broadside.rs:198)

Find the player's cell, push `action_id` onto its `queue`. `false` if there's no
player. Helper for the `QueueAction` arm.

---

## Initial scene + ships (src/bin/broadside.rs:221–323)

- `demo_lane` (221) — just `DEFAULT_LANE`; the flat horizontal model needs no
  per-binary tuning.
- `fresh_content` (230) — build `DemoContent` with the player's Phase-2 loadout
  pre-installed: HeatSink + Point-Blank Doctrine subsystems and one charge of
  each placeholder card. Called on startup **and** every Restart so charges
  refill.
- `render_example_board` (240) — the TS-mirrored startup/Restart scene: 7-cell
  lane, player at 0, enemies at 2/3/5/6. Each enemy gets a Forward `pulse_laser`
  so the AI has something to queue (an unarmed enemy looks inert).
- `player_ship` (265) — player at the given cell, bow-fore, bow shield armour 2 /
  charge 1, two forward mounts (`pulse_laser`, `torpedo`), `klass = "aegis"` (the
  sprite-only hook for bruce's hand-painted `aegis_*.png`; the `TODO` notes combat
  math doesn't depend on the slug yet).
- `enemy_ship` (294) / `make_ship` (304) — the enemy and base ship constructors.

---

## `const CAMERA_ANGLE_STEPS_DEG` / `DEFAULT_INDEX` / `TWEEN_DURATION_MS` (src/bin/broadside.rs:333, 334, 338)

Seven scrub angles `[0,15,30,45,60,75,90]°` (default index 3 = 45°, the isometric
middle); the ~200 ms tween duration that reads crisp at 60 Hz.

## `struct TweenAnchor` (src/bin/broadside.rs:344)

Per-ship "where did this ship visually start the tween from, and when" — a
`from_cell: f32` (fractional, so an in-flight tween can re-anchor without a snap)
and a `started_at: Instant`.

## `enum DemoState` (src/bin/broadside.rs:358)

The Phase-3 modal state machine: `Playing` (normal turn loop), `EncounterComplete`
(1/2/3 = repair/upgrade/continue), `RunComplete` (Enter restarts the run),
`RunDefeated` (player destroyed; Enter restarts). The three non-Playing states are
modal overlays that gate input — distinct from per-encounter `WinState`.

## `struct App` (src/bin/broadside.rs:374)

The whole demo state: `window`, `gfx`, `board`, `lane`, `content`,
`camera_angle_idx`, `tween_anchors` (keyed by ship id), `sectors` (built once from
`placeholder_sectors`, **not** rebuilt on Restart), `run`, `demo_state`, and an
optional shared `audio` state (behind the `audio` feature; `None` if the backend
failed to open — headless CI, no driver).

---

## `impl App` (src/bin/broadside.rs:406)

- `new` (407) — initialize the struct, then (behind `audio`) try to open the
  audio backend and install it on the board's bus.
- `fresh_player_ship` (441) — `player_ship(0)`; subsystems live on `content`, so
  they carry over for free across encounters.
- `build_current_board` (489) — build the current encounter's board via
  [`build_encounter_board`](runs.md). The spawn closure is a **three-way priority
  dispatch** (since #115 catalog enemy synthesis): `class_id == "warlord"` →
  [`boss_ship_for_spawn`](runs.md) (the hand-tuned hull-14 boss, richer than the
  catalog's plain warlord); else
  [`enemy_ship_from_catalog_at_tier`](catalog.md) — real hull/mounts/**traits** from
  the canonical `enemies[]`, so the AI's Pursuit/BurnHard/Agile nudges fire; else
  [`fallback_ship_for_spawn`](runs.md) if the catalog is absent or the class_id isn't
  in `enemies[]` (graceful degrade). It threads the current sector's `patrol_tier`
  into the synthesizer's dormant difficulty seam. `None` if the run has no current
  encounter.
- `restart_run` (471) — reset run + content + board to sector-0/encounter-0,
  clear tweens, re-install audio. Called from the run-end overlays.
- `award_encounter_salvage` (668) — **the first live salvage accrual** (the old flat
  per-enemy path was never bin-wired). Fires on the `EncounterOutcome::Won` transition
  (before advancing, so `current_encounter` still points at the won encounter). A
  capital/boss encounter pays the doc-canonical tier-scaled [`CapitalDef`](types.md)
  salvage via [`salvage_for_capital_encounter`](meta.md); any other encounter falls back
  to per-enemy salvage. Only fires when a catalog is loaded (the placeholder campaign has
  no capitals, so nothing to reward → skip). Inlines
  [`award_run_salvage_with_catalog`](meta.md)'s rule rather than calling it — it computes
  `earned` under immutable borrows (`catalog`/`enc`/`sectors`), then applies it to
  `self.run` under the mutable borrow, sidestepping a `self.catalog` + `self.run`
  double-borrow.
- `apply_path_choice` (486) — the EncounterComplete 1/2/3 handler: `D1` repairs
  +2 hull (stays in the overlay), `D2` is the upgrade placeholder, `D3` continues
  via `advance_after_win` (branching on `AdvanceResult` to load the next board or
  promote to `RunComplete`).
- `reinstall_audio` (546 / 552) — re-subscribe audio hooks on the rebuilt board
  bus; a no-op stub when the `audio` feature is off.
- **Tween machinery** (561–627): `snapshot_visual_cells` (capture each ship's
  current fractional render position *before* a mutation), `record_tween_anchors`
  (after the mutation, plant an anchor wherever a ship's logical cell now differs
  from its pre-mutation visual cell — and evict anchors for ships that vanished),
  `tween_state` (each frame, ease-out-quad interpolate `from_cell → ship.cell`,
  dropping expired anchors), `has_active_tween` (any anchor still live → keep
  redrawing). This is why a move animates smoothly and a rapid double-tap doesn't
  stutter (it re-anchors from the in-flight visual cell).

---

## `impl ApplicationHandler for App` (src/bin/broadside.rs:630)

The winit event loop.

- `resumed` (631) — create the window at `VIRTUAL_W × VIRTUAL_H`, block on
  `Gfx::new`, then `try_load_ship_sprites("assets")` (missing PNGs silently
  fall back to procedural).
- `window_event` (651) — the dispatcher:
  - `CloseRequested` → exit; `Resized` → `gfx.resize`.
  - `KeyboardInput` (667): **edge-triggered** (only on press, ignore repeats).
    `Esc` exits. `[`/`]` cycle the camera angle (handled *before* the key→intent
    lookup so they stay a renderer-owned binding, not part of the content key
    map). Then the `DemoState` gate (698): `EncounterComplete` accepts only
    1/2/3, `RunComplete`/`RunDefeated` accept only Enter, everything else
    swallowed. In `Playing`: a mid-encounter defeat (`win_state == Defeat`)
    promotes to `RunDefeated`; otherwise snapshot the player, run
    `key_to_intent`, then `apply_intent`. On a change, record tween anchors and
    run `encounter_outcome` to maybe transition to `EncounterComplete` /
    `RunDefeated`, then `request_redraw`.
  - `RedrawRequested` (773): compute the camera angle + tween state under `&self`,
    then borrow `gfx` mutably; `hud::compose_scene_tweened` builds the instance
    list, `push_salvage_hud` adds the counter (Playing only), and the matching
    `DemoState` overlay is pushed on top (the bin owns the overlay decision since
    #77 — compose no longer auto-pushes). `gfx.render` with the standard
    `Lost`/`Outdated` → reconfigure, `OutOfMemory` → exit handling. If a tween is
    still active, request another redraw (otherwise the loop sleeps until the
    next input).

**Cross-references:** Drives [`hud`](hud.md) (compose + overlays), [`gfx`](gfx.md)
(render), [`runs`](runs.md) (outcome + advancement), [`audio`](../LINE_BY_LINE.md#srcaudiors).

## `fn main()` (src/bin/broadside.rs:837)

Init logging, create the winit `EventLoop` with `ControlFlow::Poll`, build the
`App`, run. The `Poll` control flow (vs `Wait`) keeps the loop spinning so
in-flight tweens animate; the redraw path then only re-requests while a tween is
live, so a static scene still lets the loop idle between inputs.
