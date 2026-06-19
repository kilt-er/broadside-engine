//! Headless offscreen capture — renders ONE frame of the real combat board to a
//! PNG, with no window or display. The permanent "give the team eyes" tool: an
//! AI agent (or anyone) can Read the PNG to SEE exactly what the renderer
//! produces, instead of needing a live window (which a headless/locked machine
//! can't provide).
//!
//! Run: `cargo run --bin capture --features render,runtime -- bugs/current_state.png`
//! With NO path arg it defaults to `<crate>/../bugs/current_state.png` (the
//! Broadside-root `bugs/` dir), resolved from `CARGO_MANIFEST_DIR` so it works
//! from ANY cwd — `cargo run --bin capture` argless lands the PNG in the right
//! place whether launched from the workspace root or the engine crate. A passed
//! path (relative or absolute) is used verbatim. The parent dir is created if
//! missing, so capture never fails with a "path not found" os-error.
//!
//! Renders through the SAME path the game uses — [`Gfx::new_headless`] builds the
//! identical sprite/polygon/loft pipelines + parallax background, the player gets
//! the real Aegis loft hull + enemies the CAD hull, and
//! [`hud::compose_scene_2d_with`] composes the scene — so the capture is a
//! faithful image of the live frame, then [`Gfx::capture_png`] reads the offscreen
//! back to disk (stripping the wgpu 256-byte row padding).
//!
//! The board matches the campaign spawn layout (player front-centre Bow(N),
//! enemies fanned centre-out across the back row Bow(S)) so the captured frame
//! shows the real bow-to-bow start — the orientation the team needs to see.

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::Gfx;
use broadside_engine::grid::{Dir4, Facing, Pos, COLS};
use broadside_engine::hud::compose_scene_2d_with;
use broadside_engine::projector::ProjectorConfig;
use broadside_engine::runs::{enemy_spawn_facing, player_spawn_facing, player_start_pos};
use broadside_engine::types::{Board, EventBus, Faction, LaneEnd, Mount, Orientation, Ship};

/// Build a `types::Ship` at `pos`/`facing` (mirrors the bin's make_ship: 2-D
/// pos/facing drive the render; legacy 1-D cell/orientation kept consistent).
fn make_ship(id: &str, faction: Faction, pos: Pos, facing: Facing) -> Ship {
    let orientation = match facing {
        Facing::Bow(Dir4::N) => Orientation::BowOn { bow: LaneEnd::Fore },
        Facing::Bow(Dir4::S) => Orientation::BowOn { bow: LaneEnd::Aft },
        _ => Orientation::Broadside,
    };
    Ship {
        id: id.to_string(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation,
        facing,
        hull: 5,
        max_hull: 5,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: vec![Mount {
            id: "m1".into(),
            arc: broadside_engine::types::Arc::Forward,
            weapon: "pulse_laser".into(),
        }],
        queue: Vec::new(),
        cooldowns: std::collections::HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// The campaign-layout board: player front row Bow(N) at `player_col`, enemies
/// centre-out across the back row Bow(S) — the real bow-to-bow start. `player_col`
/// lets the capture place the player OFF-CENTER (e.g. col 0/4) to expose
/// lane-dependent pose bugs that a centred (col 2, zero lane-yaw) shot masks.
fn capture_board(player_col: usize, player_row: usize, player_facing: Facing) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    let place = |cells: &mut Vec<Option<Ship>>, s: Ship| {
        let idx = s.pos.to_index();
        cells[idx] = Some(s);
    };
    let ppos = Pos::new(
        player_col.min(COLS - 1),
        player_row.min(broadside_engine::grid::ROWS - 1),
    );
    // (#70) Player facing is an ARG so the capture reproduces a MOVED/REORIENTED
    // live player (the old frozen spawn-facing masked the chase-cam pose bug).
    // Post-decouple the loft hull no longer rotates with facing — so a capture
    // at the same column should look identical across facings (that's the fix);
    // the arg lets us PROVE it + match any live case.
    let mut player = make_ship("player", Faction::Player, ppos, player_facing);
    // (#66) class "aegis" so the baked aegis_* sprites are selected (the renderer
    // keys sprite_path on klass); the bin sets the player class likewise.
    player.klass = Some("aegis".to_string());
    player.shield_profile.bow.charge = 2;
    player.shield_profile.port.charge = 1;
    place(&mut cells, player);
    let mid = COLS / 2;
    place(&mut cells, make_ship("enemy-2", Faction::Enemy, Pos::new(mid, 0), enemy_spawn_facing()));
    place(&mut cells, make_ship("enemy-3", Faction::Enemy, Pos::new(mid - 1, 0), enemy_spawn_facing()));
    place(&mut cells, make_ship("enemy-5", Faction::Enemy, Pos::new(mid + 1, 0), enemy_spawn_facing()));

    Board {
        size: COLS,
        cells,
        ordnance: Vec::new(),
        hazards: (0..broadside_engine::grid::CELLS).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // A passed path (relative or absolute) is used verbatim; with no arg, default
    // to the Broadside-root `bugs/` dir resolved from the crate manifest so the
    // capture lands in the right place from ANY cwd (the bin's cwd is the engine
    // crate, which has no `bugs/` subdir — that was the os-error-3 on the argless
    // run). `CARGO_MANIFEST_DIR` is `<…>/Broadside/engine`; `../bugs` is the dir
    // every agent + the lead Reads.
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bugs/current_state.png")
            .to_string_lossy()
            .into_owned()
    });

    let mut gfx = pollster::block_on(Gfx::new_headless());

    // Load the baked ship sprites the playable bin loads (fallback path).
    let loaded = gfx.try_load_ship_sprites(std::path::Path::new("assets"));
    log::info!("capture: loaded {loaded} ship sprite(s)");

    // (#70) Install the player's faithful Aegis GLB exactly as the playable bin
    // does (broadside.rs install_player_glb) so the capture faithfully shows the
    // LIVE-3D player ship, not the flat-box placeholder.
    const AEGIS_GLB: &[u8] = include_bytes!("../../assets/ships/Aegis.glb");
    match gfx.install_player_glb(AEGIS_GLB) {
        Ok(()) => log::info!("capture: player Aegis hull installed from Aegis.glb"),
        Err(e) => log::warn!("capture: Aegis.glb import failed ({e}); player falls back to sprite/flat-box"),
    }
    // (#89/#93) ENEMIES = the same Aegis hull, STEEL-GREY-tinted (matches the live
    // bin's ENEMY_TINT) so the capture shows the oncoming grey enemy ships (apart
    // from the RED player), not flat boxes.
    match gfx.install_enemy_glb(AEGIS_GLB) {
        Ok(()) => log::info!("capture: enemy Aegis hull (steel-grey) installed from Aegis.glb"),
        Err(e) => log::warn!("capture: enemy Aegis.glb import failed ({e}); enemies fall back to CAD/2D"),
    }

    // (#76) Optional BROADSIDE_SHIP_RES=N env cycles the SHIP loft res forward N
    // steps before the capture, so a headless shot can verify the live ship-res
    // change (160x100 -> 220x138 -> 320x200 -> 480x300) renders correctly. Default
    // 0 = the baseline res. Mirrors the live `,`/`.` control via
    // `Gfx::cycle_loft_res`.
    if let Ok(n) = std::env::var("BROADSIDE_SHIP_RES") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let (w, h) = gfx.cycle_loft_res(true);
                log::info!("capture: cycled ship res -> {w}x{h}");
            }
        }
    }

    // (#76) Optional BROADSIDE_SCENE_RES=N env cycles the WHOLE-SCENE (offscreen)
    // res forward N steps before the capture (320x180 -> 480x270 -> 640x360),
    // mirroring the live `;`/`'` control via `Gfx::cycle_scene_res`. Default 0 =
    // the 480x270 baseline, which the pixel-identity gate diffs against the
    // pre-scene-res reference PNG. The projector `cfg` below is built AFTER this so
    // it reprojects to whatever scene size we land on.
    if let Ok(n) = std::env::var("BROADSIDE_SCENE_RES") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let (w, h) = gfx.cycle_scene_res(true);
                log::info!("capture: cycled scene res -> {w}x{h}");
            }
        }
    }

    // Optional 2nd arg = player column (0..COLS-1) so the capture can place the
    // player OFF-CENTER to expose lane-dependent pose bugs (a centred shot at the
    // zero-lane-yaw col 2 masks a mirrored lane-aim sign). Defaults to the
    // campaign spawn column.
    let player_col = std::env::args()
        .nth(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| player_start_pos().col);
    // Optional 3rd arg = player FACING (n/s/e/w) so the capture reproduces a
    // REORIENTED live player (the frozen spawn-facing masked the chase-cam pose
    // bug). Default = the campaign spawn facing.
    let player_facing = match std::env::args().nth(3).as_deref() {
        Some("n") | Some("N") => Facing::Bow(Dir4::N),
        Some("s") | Some("S") => Facing::Bow(Dir4::S),
        Some("e") | Some("E") => Facing::Bow(Dir4::E),
        Some("w") | Some("W") => Facing::Bow(Dir4::W),
        _ => player_spawn_facing(),
    };
    // Optional 4th arg = player ROW (0..ROWS-1) so the capture can place the
    // player at a MOVED-FORWARD/BACK cell (row 0 = far/back, row 3 = front
    // spawn) — reproduces what a forward (N) move actually draws, which a
    // frozen-spawn-row capture can't show. Default = the campaign spawn row.
    let player_row = std::env::args()
        .nth(4)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| player_start_pos().row);
    let mut board = capture_board(player_col, player_row, player_facing);
    log::info!("capture: player at col {player_col} row {player_row}, facing {player_facing:?}");

    // (#90 yellow-square repro) Optional BROADSIDE_QUEUE_DEMO=1 queues the player's
    // mount weapon (what pressing `1` does) so the queue-driven overlays render —
    // reproduces Bruce's "yellow square covers the ship when I queue an ability".
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        if let Some(p) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            let w = p.mounts.first().map(|m| m.weapon.clone()).unwrap_or_default();
            // Queue it three times — match Bruce queuing several abilities.
            for _ in 0..3 {
                p.queue.push(w.clone());
            }
            log::info!("capture: QUEUE demo — player queued '{w}' x{}", p.queue.len());
        }
        // (#129) Give the enemies a REVEALED queue so the top-left ENEMY INFO panel
        // shows queue icons (the live path fills enemy.queue when the AI telegraphs;
        // here we inject it so the static capture shows the learned-queue state). Knock
        // one enemy's hull + shields down so the panel's hull/shield bars read PARTIAL.
        for (n, e) in board
            .cells
            .iter_mut()
            .flatten()
            .filter(|s| s.faction == Faction::Enemy)
            .enumerate()
        {
            let w = e.mounts.first().map(|m| m.weapon.clone()).unwrap_or_default();
            if !w.is_empty() {
                // First enemy telegraphs two shots; second telegraphs one — varied queue.
                let count = if n == 0 { 2 } else { 1 };
                for _ in 0..count {
                    e.queue.push(w.clone());
                }
            }
            if n == 0 {
                e.hull = (e.max_hull / 2).max(1); // partial hull bar
                e.shield_profile.bow.charge = 0; // partial shield bar
            }
        }
        log::info!("capture: QUEUE demo — injected enemy revealed-queues for the info panel");
        // The LIVE queue runs the enemy world-phase (apply_intent → run_world_phase),
        // which paints board.threats (enemy intent). A capture's manual queue-push
        // does NOT, so simulate it: an enemy threat on the PLAYER's NEAR cell — the
        // largest cell quad on screen — to test whether the threat FILL is the
        // field-covering warm square Bruce sees (#78 was a red-slab version of this).
        use broadside_engine::types::{Threat, ThreatKind};
        let ppos = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
            .unwrap_or_else(|| Pos::new(2, 3));
        board.threats.push(Threat {
            pos: ppos,
            kind: ThreatKind::Damage { amount: 9 }, // ≥ hull ⇒ LETHAL fill (brightest)
            source: Pos::new(COLS / 2, 0),
        });
        log::info!("capture: QUEUE demo — injected a LETHAL threat on the player cell {ppos:?}");
    }

    // (#90) Optional BROADSIDE_VFX_DEMO=1 injects sample combat RESULTS so the
    // headless capture can verify the new fire/impact/destruction VFX: a player→
    // enemy HIT beam + impact, an enemy→player MISS beam (dimmer), and one enemy
    // marked at zero hull (destruction burst). Off by default so normal captures
    // (incl. the pixel-identity gate) are unaffected.
    let mut vfx_demo_kill: Option<Pos> = None;
    if std::env::var("BROADSIDE_VFX_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::types::{FireEvent, WeaponArchetype};
        let ppos = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
            .unwrap_or_else(|| Pos::new(2, 3));
        let mid = COLS / 2;
        let tgt = Pos::new(mid, 0); // centre back-row enemy
        board.fire_events.push(FireEvent {
            from_cell: ppos.to_index(),
            to_cell: tgt.to_index(),
            from_pos: ppos,
            to_pos: tgt,
            archetype: WeaponArchetype::Beam,
            attacker_faction: Faction::Player,
            hit: true,
        });
        board.fire_events.push(FireEvent {
            from_cell: tgt.to_index(),
            to_cell: ppos.to_index(),
            from_pos: tgt,
            to_pos: ppos,
            archetype: WeaponArchetype::Beam,
            attacker_faction: Faction::Enemy,
            hit: false, // a miss — renders dimmer
        });
        // REMOVE the struck enemy (mirrors the resolver's destroy() take()), then
        // remember its cell so we append a kill-burst to the commands below — the
        // faithful post-kill frame (the bin drives the burst off a prev-vs-current
        // id diff, which a single static capture can't reproduce).
        if let Some(slot) = board.cells.get_mut(tgt.to_index()) {
            *slot = None;
        }
        vfx_demo_kill = Some(tgt);
        // (#101) Knock a SURVIVING enemy down to half hull BEFORE compose, so the
        // hull bar renders PARTIAL — then we flash it (full intensity) after compose
        // to show the damage-flash + min-size bar legibility win.
        if let Some(victim) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Enemy)
        {
            if victim.max_hull > 0 {
                victim.hull = (victim.max_hull / 2).max(1);
            }
            // (#107) Half-charge the victim's shields so its lane SHIELD bar reads
            // partial under the hull bar.
            for f in [
                &mut victim.shield_profile.bow,
                &mut victim.shield_profile.stern,
                &mut victim.shield_profile.port,
                &mut victim.shield_profile.starboard,
            ] {
                f.charge = (f.armour + 1) / 2;
            }
        }
        // (#107) Half-charge the PLAYER's shields too so the bottom-HUD SHIELD bar
        // (below HULL) reads partial in the capture.
        if let Some(p) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            for f in [
                &mut p.shield_profile.bow,
                &mut p.shield_profile.stern,
                &mut p.shield_profile.port,
                &mut p.shield_profile.starboard,
            ] {
                f.charge = (f.armour + 1) / 2;
            }
        }
        log::info!("capture: VFX demo — player HIT + enemy MISS + a kill (burst) at {tgt:?}");
    }

    // (#70) Sync the player's loft pose so the loft pre-pass has a pose to render
    // (mirrors the playable bin's per-frame sync_loft_pose).
    for s in board.cells.iter().flatten() {
        gfx.sync_loft_pose(&s.id, s.orientation);
    }

    // (#76) Project to the LIVE scene size (default 480x270 == ProjectorConfig
    // ::default(); a BROADSIDE_SCENE_RES cycle above reprojects to the new canvas).
    let cfg = ProjectorConfig::for_scene(
        broadside_engine::gfx::scene_w() as f32,
        broadside_engine::gfx::scene_h() as f32,
    );
    let mut commands = compose_scene_2d_with(&board, &cfg, &gfx);
    // (#127) SALVAGE readout — the live bin draws this in its Playing overlay (not
    // inside compose_scene_2d), so append it here with a representative value so the
    // capture shows its NEW bottom-left position under the HULL/SHLD bars.
    broadside_engine::hud::push_salvage_hud(&mut commands, 137);
    // (#134) Debug readouts — also bin-overlay-only; append so the capture shows
    // their NEW bottom-right position (POS/FACE + SHIP/SCENE res).
    if let Some(p) = board.cells.iter().flatten().find(|s| s.faction == Faction::Player) {
        broadside_engine::hud::push_player_readout(&mut commands, p.pos, p.facing);
    }
    broadside_engine::hud::push_res_readout(&mut commands, gfx.loft_res(), gfx.scene_res());
    // (#90) VFX-demo kill burst at the destroyed enemy's cell — the bin emits this
    // off its prev-vs-current id diff each frame; here we append it directly so the
    // headless demo shows the post-kill burst.
    if let Some(killed) = vfx_demo_kill {
        broadside_engine::hud::push_destruction_at(&mut commands, &[killed], &cfg);
        // (#119) Procedural explosion particle burst at the killed cell — the bin
        // seeds this on its kill detection; here we seed + advance it a few frames
        // so the single static capture catches the spray MID-flight (spread out),
        // not all stacked at the centre.
        {
            let c = broadside_engine::projector::grid_cell_quad(killed, &cfg).center;
            let mut pool = broadside_engine::vfx::ParticlePool::new();
            pool.spawn_burst(c, 22, [1.0, 0.72, 0.32, 1.0], 0.55);
            for _ in 0..6 {
                pool.advance(1.0 / 60.0);
            }
            pool.emit(&mut commands);
            log::info!("capture: #119 explosion particle burst at killed cell {killed:?}");
        }
        // (#101) Flash the hull bar of the SURVIVING enemy we knocked to half hull
        // (above, before compose, so its bar already renders PARTIAL) at full
        // intensity — the moment-of-hit pop — so the capture shows the new
        // damage-flash + min-size bar (the "I don't see the enemy health bar
        // dropping" legibility win).
        if let Some(victim) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Enemy)
        {
            broadside_engine::hud::push_hull_flash_2d(&mut commands, victim, 1.0, &cfg);
            // (#106) Floating damage number above the same enemy (demo amount).
            broadside_engine::hud::push_damage_number_2d(&mut commands, victim, 3, 1.0, &cfg);
            log::info!("capture: #101/#106 hull-flash + damage number on surviving enemy {} at {:?}", victim.id, victim.pos);
        }
    }
    // (#98/#100) With QUEUE_DEMO, append a representative ability-tile row so the
    // headless shot shows the layout (damage # top-left, key # bottom-right,
    // cooldown ticks) AND the #100 cues. The capture has no Content, so hand-build
    // tiles in varied states:
    //   * slot 1 = QUEUED + bears: amber border + order badge "2", no veil;
    //   * slot 2 = resting, on cooldown (grey ticks), not queued;
    //   * slot 3 = QUEUED but CAN'T BEAR (Bruce's press-3 bug): amber border +
    //     order badge "1" + dark veil + red slash = "won't fire from here";
    //   * slot 5 = resting, ready (white border), not queued.
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::hud::{AbilityIcon, AbilityTile};
        let tiles = vec![
            AbilityTile { slot: '1', icon: AbilityIcon::Beam, damage: 3, range: 1, cooldown: 0, cooldown_max: 0, queued_index: Some(1), can_fire: true, arc: Some('F') },
            AbilityTile { slot: '2', icon: AbilityIcon::Ordnance, damage: 6, range: 3, cooldown: 0, cooldown_max: 0, queued_index: None, can_fire: false, arc: Some('F') },
            AbilityTile { slot: '3', icon: AbilityIcon::Defensive, damage: 5, range: 2, cooldown: 0, cooldown_max: 3, queued_index: Some(0), can_fire: false, arc: Some('B') },
            AbilityTile { slot: '5', icon: AbilityIcon::Defensive, damage: 0, range: 0, cooldown: 0, cooldown_max: 0, queued_index: None, can_fire: true, arc: None },
        ];
        broadside_engine::hud::push_ability_tiles_2d(&mut commands, &tiles);
        // (#128) Player QUEUE panel (top-right) from the same demo tiles — slots 1+3
        // are queued (index 1 + 0), so the panel shows them in fire order and the
        // hand tiles 1+3 hollow out. Lets the capture show the hand->queue move.
        broadside_engine::hud::push_player_queue_panel_2d(&mut commands, &tiles);
        // (#129) Enemy INFO panel (top-left) from the board's enemies — shows each
        // enemy's hull + shield + the revealed queue injected above.
        broadside_engine::hud::push_enemy_info_panel_2d(&mut commands, &board);
        // (#122) Player targeting telegraph demo: show the cyan preview of where a
        // queued weapon would strike. The live bin resolves the cells via
        // resolve_targeting_2d; here we hand-pick a target cell forward of the
        // player so the headless shot shows the cyan target outline + aim line.
        if let Some(ppos) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
        {
            // A cell two rows up-lane (toward the enemies) as the demo target.
            let tgt = Pos::new(ppos.col, ppos.row.saturating_sub(2));
            broadside_engine::hud::push_player_targeting_2d(&mut commands, ppos, &[tgt], &cfg);
            log::info!("capture: #122 player targeting telegraph demo {ppos:?} -> {tgt:?}");
        }
    }
    // (#90 yellow-square repro) With QUEUE_DEMO, dump any LARGE draw command (a
    // sprite half-size > 60px or a polygon spanning > 120px in either axis) + its
    // colour — pinpoints the giant fill covering the ship/field without eyeballing.
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::gfx::DrawCommand;
        // A "yellow-ish" tint: high R+G, low B (gold/amber/warm-white).
        let yellowish = |c: &[f32; 4]| c[0] > 0.7 && c[1] > 0.6 && c[2] < 0.6 && c[3] > 0.2;
        for (i, c) in commands.iter().enumerate() {
            match c {
                DrawCommand::Sprite(s) if yellowish(&s.color) => {
                    log::info!(
                        "YELLOW sprite #{i}: pos {:?} half {:?} color {:?}",
                        s.pos, s.half_size, s.color
                    );
                }
                DrawCommand::Sprite(s) if s.half_size[0] > 40.0 || s.half_size[1] > 40.0 => {
                    log::info!(
                        "BIG sprite #{i}: pos {:?} half {:?} color {:?}",
                        s.pos, s.half_size, s.color
                    );
                }
                DrawCommand::Polygon(p) if yellowish(&p.color) => {
                    log::info!(
                        "YELLOW polygon #{i}: p0 {:?} p2 {:?} color {:?}",
                        p.p0, p.p2, p.color
                    );
                }
                DrawCommand::Polygon(p) => {
                    let wspan = (p.p1[0] - p.p0[0]).abs().max((p.p2[0] - p.p3[0]).abs());
                    let hspan = (p.p3[1] - p.p0[1]).abs().max((p.p2[1] - p.p1[1]).abs());
                    if wspan > 120.0 || hspan > 120.0 {
                        log::info!(
                            "BIG polygon #{i}: p0 {:?} p2 {:?} (span {wspan:.0}x{hspan:.0}) color {:?}",
                            p.p0, p.p2, p.color
                        );
                    }
                }
                _ => {}
            }
        }
        log::info!("capture: {} total draw commands", commands.len());
    }
    // Ensure the output dir exists so the save never trips os-error-3 on a fresh
    // checkout / unusual cwd (the default `bugs/` dir, or any custom path).
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("capture: could not create {}: {e}", parent.display());
            }
        }
    }
    match gfx.capture_png(&commands, std::path::Path::new(&path)) {
        Ok(()) => log::info!("capture: wrote {path}"),
        Err(e) => {
            log::error!("capture failed: {e}");
            std::process::exit(1);
        }
    }
}
