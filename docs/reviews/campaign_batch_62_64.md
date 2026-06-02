# Review: #62 campaign-wire / #63 CapitalDef+salvage / #64 tile redesign + #65 note

Reviewer pass on the night's campaign batch. Status: **all APPROVE / reviewed-clean.**
Plus an analysis note on #65 (the generated-campaign stalemate — not my fix, but it
implicates the #60 generator I GO'd + a real robustness gap).

## dd8d357 — #64 tile redesign (hud + bin) — APPROVE

- Atomic: exactly hud.rs + bin/broadside.rs, one commit. Clean.
- ENEMY TELEGRAPH read-only (the cross-teammate-consistency check): `push_enemy_telegraph` is documented "stateless — straight from the enemy's current queue," takes `&[AbilityTile]` (a data snapshot) + emits DrawCommands. grep for &mut / .push / .clear / .pop / insert on enemy queues in hud = NONE. The renderer reads enemy.queue as read-only render input, never mutates it — the resolver-owns-queue invariant holds (the AI fills queues the resolver runs; the HUD only displays them).
- Player AbilityHud below↔above queue animation: `advance` reads `queued_index` (Some → target phase 1.0, None → 0.0) and lerps `phase` per slot — reads queue state, doesn't write it. Cooldown-on tiles hidden for enemy, shown dimmed for player. Placeholder atlas-glyph icons (first-pass, bruce-eyeball).
- Bin edit is the tile-assembly + Playing-branch calls (the bin owns Content for names + the live Ships for queue/cooldown). Consistent with the #53 pattern I approved.

## 3211342 — #62 campaign-wire — APPROVE

App::new: `generate_campaign(catalog, patrol_tier)` when a catalog loaded, else `placeholder_sectors()` fallback. Isolated +15/-3 in broadside.rs. The generator I GO'd (#60) is now the live campaign source. Clean drop-in.

## 33a4bb3 + 4622de8 — #63 CapitalDef + salvage rename — APPROVE

- CapitalDef typed (Catalog.capitals: Vec<CapitalDef>), 6 canonical fields. Isolated to types.rs + the Catalog field; no runs/bin/hud/save touch.
- The sp1/sp7 → salvage_p1/salvage_p7 rename is FIELD-NAME-ONLY: `#[serde(rename = "sP1")]` / `#[serde(rename = "sP7")]` preserve the wire keys (types.rs:921/928), so the catalog JSON is unchanged — no runtime collision, no re-export needed. The rename corrects a real semantic mislabel (these are salvage REWARDS, not strength/hull — the old docstring called them "strength at tier," which was wrong). salvage_p1 Option (null in catalog), salvage_p7 #[serde(default)]→0. Round-trip tests cover the null-sP1 case. Good catch by content on the semantic.

## #65 — generated sector-2 stalemate (NOT my fix; analysis + robustness flag)

The run_loop integration test finds 2.1_e0 (tier-2 generated encounter) doesn't terminate in 64 rounds → fight_to_completion panics. Owner: content (enemy scaling) + bruce (balance). NOT a reviewer fix. But two observations from my seat:

1. This implicates the #60 generator I GO'd — but it is NOT a generator CORRECTNESS bug. The generator produces a valid, non-panicking encounter (my #60 pass verified the cell/pool/sampling math). The stalemate is a BALANCE/positional emergent property: tier-2 enemies (or a no-bears positional standoff) vs the fixed test-player loadout. My GO was for generator soundness, which holds; playability balance is a separate, correctly-routed concern. Flagging so the GO isn't misread as "the campaign is balanced."

2. ROBUSTNESS GAP worth a design ruling (the part that's genuinely engine-side, not just balance): the task notes fight_to_completion has no draw/timeout outcome. In the TEST that's a panic, fine. But the underlying condition — a positional stalemate where neither side bears (both bow-on facing away, or out-of-arc) so no damage is ever dealt — is a REAL resolver state the live game could reach. The canonical TS resolver has no stalemate breaker either (it's turn-driven, relies on the player acting). For the engine: a real run can't infinite-loop because a human acts each turn, BUT an all-AI matchup or an AFK player vs a non-bearing enemy makes zero progress forever. Recommend (bruce/architect call, not now): a hard round-cap → forced outcome (draw/retreat) as a safety net, OR confirm the design guarantees progress (e.g. heat forces a vent, statuses tick down — but neither kills a full-hull non-bearing ship). Not blocking the batch; logging it as a latent engine robustness item the stalemate surfaced.

Net: #62/#63/#64 clean for bruce's eyeball. #65 is correctly content+bruce's; my note is (a) the #60 GO was soundness not balance, and (b) the stalemate hints at a latent no-progress-guarantee gap in the resolver loop worth a future ruling.
