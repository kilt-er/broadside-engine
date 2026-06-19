# Broadside — Core Gameplay Loop (CANONICAL)

> This is the authoritative spec for how a turn works in Broadside. It was
> ratified by Bruce on 2026-06-19. If any code or doc contradicts it, this wins.
> Read this before touching the turn loop, the AI, or the input handling.

## One sentence

Broadside is **turn-based, like chess. There is NO real-time clock or timer.**

## The loop

- Every entity — the **player and each enemy** — has its **own** ability queue,
  its **own** cooldowns, and makes its **own** independent decisions. "Independent"
  means each enemy decides for itself (it is not a hive mind); it does **not** mean
  enemies run on a separate clock.

- A **turn** happens when the **player takes exactly one action.** The four
  player turn-actions, **each of which costs a turn:**
  1. **Move** one cell.
  2. **Queue** one ability (adds 1 ability to the player's queue).
  3. **Dequeue-all / fire** — execute the whole queue sequentially: the 1st queued
     ability hits the first valid target along its path, then the 2nd resolves
     against the now-updated board, and so on. (This is Space / the old
     `CommitTurn`.)
  4. **Wait** — pass. (Bound to `W`.)

- The instant the player takes **any one** of those four actions, **every enemy
  also takes one action** — its own independently-decided move / queue / fire /
  wait, respecting its own cooldowns — and **every cooldown (player + all enemies)
  ticks down 1.**

- **Enemies queue before they fire**, exactly like the player. An enemy **cannot
  fire on the turn it first decides** — it telegraphs/queues one turn, then fires
  the next. (The `#67` telegraph-one-turn-ahead contract *is* this rule: the
  player sees the shot coming one turn before it lands.) Enemies also **move /
  reposition** (close to range, flank) — they do not just sit and fire.

- **Cooldown:** an ability with recharge `N` is usable again `N` actions (turns)
  after it fires. You can only queue an ability that is **off** cooldown; the
  cooldown **starts when the ability fires.** A recharging weapon can't be queued,
  which forces a move or a wait.

## The one exception — FREE actions

The **field-kit cards (slots 5 / 6 / 7, `PlayCard`)** are **free actions.**
Playing a card applies its effect but does **NOT** cost a turn: no world advance,
no enemy action, no cooldown tick. Everything else costs a turn.

## The core tension (the point of the game)

Each turn you choose **one** thing: **move** (dodge out of harm's way, or get into
a firing position) **or** build/unleash your queue (**queue** / **dequeue-all**).
You cannot do both in a turn. That choice is the whole game.

## What it is NOT (do not re-introduce)

- **NOT real-time.** There is no timer. (A real-time per-enemy clock was built and
  then reverted — `#124` built it, `#126` reverted it. Do not bring it back.)
- The player's **spacebar/dequeue fires only the player's queue.** It does **not**
  trigger the enemies' queues. Enemies act because **the turn advanced** (any of
  the four player actions advances the world), not because the player "committed."
  - Historical wrong turns to avoid repeating: the engine once fired enemies
    *only* on `CommitTurn`; `#97` once made queuing a *free, no-turn* action. Both
    contradict this model — **all four** player actions are turns; cards are the
    only free action.

## Worked example — 1 player, 2 enemies, 10 turns

Setup: Player **Cannon** (recharge 2). Enemy 1 **Blaster** (recharge 2). Enemy 2
**Lance** (recharge 3). All start **out of range** and unqueued. Cooldown columns
show the value *after* the turn (`-` = ready). The turn an ability fires it is set
to its recharge and starts counting down on the following turns.

| Turn | Player | Enemy 1 | Enemy 2 | Cannon | Blaster | Lance |
|------|--------|---------|---------|--------|---------|-------|
| 1  | Move (close)            | Move (close)         | Move (close)          | - | - | - |
| 2  | Queue Cannon (in range) | Queue Blaster        | Move (still closing)  | - | - | - |
| 3  | Dequeue -> fire at E1    | Dequeue -> fire at P | Queue Lance (in range)| 2 | 2 | - |
| 4  | Move (recharging)       | Move (flank)         | Dequeue -> fire at P  | 1 | 1 | 3 |
| 5  | Move (reposition)       | Wait (recharging)    | Move (reposition)     | - | - | 2 |
| 6  | Queue Cannon            | Queue Blaster        | Move                  | - | - | 1 |
| 7  | Dequeue -> fire at E1    | Dequeue -> fire at P | Move (close back in)  | 2 | 2 | - |
| 8  | Move (shift to face E2) | Wait (recharging)    | Queue Lance           | 1 | 1 | - |
| 9  | Move                    | Move (flank)         | Dequeue -> fire at P  | - | - | 3 |
| 10 | Queue Cannon            | Queue Blaster        | Move (reposition)     | - | - | 2 |

Notes the table demonstrates:
- Nobody fires on turn 1 — all three spend it closing.
- Each enemy runs its own move -> queue -> fire -> recharge rhythm on its own
  cooldown (E1 on a 2-turn loop, E2 slower on its 3-turn Lance), deciding for
  itself whether to move or wait while its weapon recharges.
- Every single action — including a plain **move** or **wait** — is one turn that
  advances all three entities and ticks every cooldown by 1.

## Implementation

Each of the four player turn-actions calls `resolve::run_world_phase(board,
content)` **once** = one turn (advance ordnance, every enemy takes one action via
`tick_enemy`, then `end_of_turn` ticks all cooldowns / heat / shield-regen).
`PlayCard` (5/6/7) does **not** call it. There is no real-time loop in the bin.
`Wait` is `Intent::Wait`, bound to `W`.

Damage / shield math is a separate spec: integer band falloff + per-face
depleting/recharging shield pools — see the combat-model notes / `geometry2d.rs`
+ `resolve.rs::end_of_turn`.
