# Broadside Rust Quality Review

- **Date:** 2026-06-19
- **Branch / tip:** v2 @ `3a8ae7a`
- **Reviewer:** `broadside-reviewer` (read-only), task #148
- **Toolchain:** rustc 1.95.0, clippy 0.1.95, rustfmt 1.9.0, miri installed; edition 2021
- **Raw logs:** `engine/_caps/clippy_pedantic_render.log`, `clippy_pedantic_default.log`, `fmt_check.log`
- **Status:** review only. No code changed. Execution of the plan below is HELD for owner prioritization.

## TL;DR

The codebase is in GOOD idiomatic shape for a game engine. Error handling at IO boundaries is exemplary (textbook API-Guidelines C-GOOD-ERR), `unsafe` is essentially absent and what exists is documented, the resolver core is panic-light, and rustdoc coverage is excellent. The gap to a "raise-the-bar" config is almost entirely (a) cosmetic pedantic lints, (b) **rustfmt has never been run**, and (c) **no `[lints]` table** so nothing stricter than default clippy is gated. None of the findings are correctness bugs.

## Measured gap (with the tooling we already have)

| Check | Result |
|---|---|
| clippy default-level (today's CI gate) | GREEN, 0 warnings |
| clippy pedantic+nursery+cargo, default features (pure logic) | ~1237 warnings |
| clippy pedantic+nursery+cargo, `--all-targets --features render,runtime` | ~2405 warnings |
| `cargo fmt --check` | NOT clean — 1184 diff hunks across nearly every file |

Top recurring pedantic/nursery categories (render run): 504 `doc_markdown` (missing backticks) · 211 `suboptimal_flops` · 181 `module_name_repetitions` · 177 `too_long_first_doc_paragraph` · 209 `must_use_candidate` · ~336 `cast_*` (precision/truncation/wrap/sign) · 100 `missing_const_for_fn` · 49 `map_unwrap_or` · 49 `float_cmp` · 48 `items_after_statements` · 23 `uninlined_format_args` · 23 `missing_errors_doc` · 21 `too_many_lines`.

`float_cmp` (the only category that could touch determinism) is **render-only** (loft_gpu/projector/mesh_import/hud/vfx/etc.) — **zero** in `geometry2d`/`resolve`. The damage path is integer (#104), so strict-fp-compare is a render cosmetic, not a combat-correctness risk.

## Strengths (non-obvious — keep these, do NOT "fix")

1. **Error types are textbook** (C-GOOD-ERR). `catalog.rs:35`, `save.rs:69`, `meta.rs:91`, `mesh_import.rs:167`, `ship_asset.rs:59`, `background.rs:236` each define a dedicated error enum + `Display` + `std::error::Error` with `source()` chaining + `From` conversions so `?` composes. Hand-rolled, no `thiserror`, fully correct. Path args use `impl AsRef<Path>`.
2. **`unsafe` is minimal + justified.** Only two hand-written `unsafe` blocks (`broadside.rs:2082`, `:2098` — Win32 `SetThreadExecutionState` FFI), both with proper `// SAFETY:` comments. All GPU/bytemuck work goes through safe wrappers (`cast_slice`, `bytes_of`, `#[derive(Pod, Zeroable)]`) — no hand-rolled `transmute`. **miri value = LOW** (the FFI call is opaque to it; skip unless `mesh_import` grows raw-byte parsing).
3. **Resolver core is panic-light.** Non-test panics in `resolve.rs`: 7, all defensible (5 `.expect()` re-finding an occupant just located; 2 `unreachable!()` on nested-match arms the outer arm constrained). `types.rs`/`catalog.rs`/`geometry2d.rs`/`ai.rs`: ZERO non-test panics. The 25 `unwrap`s in `broadside.rs` are in BINARY code where the Book permits it.
4. **Excellent rustdoc** — module-level docs with intra-doc links (C-CRATE-DOC, C-LINK). The 504 `doc_markdown` hits are missing-backtick nits on rich prose, not missing docs.

## Findings (prioritized)

### HIGH
- **H1 — rustfmt has never been run** (1184 hunks; `cargo fmt --check` red tree-wide). *Fix:* one `cargo fmt` sweep + commit, then add `rust-toolchain.toml`. **CAUTION: touches ~every file — must be a SINGLE atomic commit on a quiet tree** or it collides with every open WIP. Source: official style guide.
- **H2 — no `[lints]` table** (`Cargo.toml`). Nothing past default clippy is gated, so the 2405 pedantic findings can't regress-guard. *Fix:* adopt the curated table below. Source: Clippy book / Cargo `[lints]`.

### MEDIUM
- **M1 — find-then-index double-lookup + `expect`, repeated** (`resolve.rs:577-579` + 4 siblings: `:2554,:2753,:2877,:2977`). `find_cell_by_id` returns the index, then `board.cells[idx].as_ref().expect(...)`. *Fix:* finder returns `Option<(usize, &Ship)>` so the borrow + index come back together and the `expect` disappears. Correct today but fragile + repeated 5×. Source: C-INTERMEDIATE + Rust Book ch9.
- **M2 — `unreachable!()` in nested match** (`resolve.rs:2846,:2950`). *Fix:* match the two modes directly in the outer arm (the outer arm is already `Push | Pull`). Source: Rust Design Patterns (make impossible states unrepresentable).
- **M3 — `map(..).unwrap_or(..)`** (49 sites). *Fix:* `map_or` / `is_some_and`. Mechanical. Source: clippy.
- **M4 — ~100 `missing_const_for_fn` + ~209 `must_use_candidate`.** Apply JUDICIOUSLY — `const fn` on the public lib surface is a semver hazard. Source: C-CTOR + clippy.

### LOW
- **L1 — out-parameter accumulator** `fn(out: &mut Vec<DrawCommand>, ...)` across the render layer (~30 fns). Technically C-NO-OUT, but a deliberate single-buffer batching choice; `&mut Vec` is correct (they push). NOT a bug. The read-only `&Vec`→`&[T]` anti-pattern is essentially ABSENT (verified).
- **L2 — cosmetic pedantic:** `items_after_statements` (48), `too_many_lines` (21, hud/resolve/gfx), `similar_names` (18), `uninlined_format_args` (23).
- **L3 — C-METADATA partial:** `Cargo.toml` has license/repository/description; missing keywords, categories, readme, rust-version.
- **L4 — stale artifact:** `engine/clippy.log` (committed, May 30, misleading). Recommend `git rm clippy.log` + gitignore `_caps/`.

## Raise-the-bar config (proposed)

### `Cargo.toml` `[lints]` (adopt incrementally)
```toml
[lints.rust]
unsafe_op_in_unsafe_fn = "warn"
missing_debug_implementations = "warn"

[lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }
# ALLOW the noisy / false-positive-prone ones:
module_name_repetitions  = "allow"  # intentional (types::TypeKind etc.)
must_use_candidate       = "allow"  # opt in case-by-case
cast_precision_loss      = "allow"  # render math, knowingly lossy f32
cast_possible_truncation = "allow"  # pixel/index casts, bounded
cast_sign_loss           = "allow"
cast_possible_wrap       = "allow"
similar_names            = "allow"
too_many_lines           = "allow"  # tackle structurally, not by lint
missing_errors_doc       = "allow"  # error types already self-documenting
missing_panics_doc       = "allow"
struct_excessive_bools   = "allow"
suboptimal_flops         = "allow"  # mul_add alters bit-exact FP; render has visual oracles
option_if_let_else       = "allow"  # readability-subjective
```
Leaves ENABLED the high-value lints that found real wins: `doc_markdown`, `map_unwrap_or`, `uninlined_format_args`, `items_after_statements`, `manual_midpoint`, `redundant_closure`, `match_same_arms`, `missing_const_for_fn`, `float_cmp` (render-only, good to track).

### `rust-toolchain.toml`
```toml
[toolchain]
channel = "1.95.0"
components = ["clippy", "rustfmt", "miri"]
```

### `rustfmt.toml`
```toml
edition = "2021"
# defaults; most knobs are nightly-gated — enforcement is the win, not tuning
```

### MSRV + supply chain
- Set `rust-version = "1.95"` in `[package]`; document an N-2 policy.
- `cargo install cargo-deny` + `deny.toml` (advisories + MIT/Apache/Unicode/BSD allowlist) wired into CI. LOW urgency (few, reputable deps) but cheap insurance. `cargo-audit` is the lighter alternative.

## Phased plan

**Phase 0 — quick wins (~½ day, mechanical, low-risk).** `cargo fmt` whole tree + `rustfmt.toml` + `rust-toolchain.toml` (H1, ATOMIC single-agent commit on a quiet tree). `git rm clippy.log` + gitignore `_caps/` (L4). MSRV + metadata (L3).

**Phase 1 — adopt `[lints]`, clear kept lints (~1–1.5 days).** Land the `[lints]` table (H2). `cargo clippy --fix` the safe auto-fixables (`uninlined_format_args`, `map_unwrap_or`, `redundant_closure`, `manual_midpoint`, `items_after_statements`), then sweep `doc_markdown` (504 backtick nits, semi-automatable). Gate `--all-targets -D warnings` after.

**Phase 2 — targeted refactors (deeper, per-owner, not en masse).** M1 finder→`(idx, &Ship)` (resolver owner + tester confirms `run_action` suite green). M2 kill the two `unreachable!()`. M4 selective `const fn`/`must_use` on the stable type surface (judicious re: semver). `too_many_lines` → opportunistic function splits.

Every Phase-2 item is owner-implemented and should name its test coverage before landing (esp. M1, resolver-touching). The reviewer can re-review diffs as they come.
