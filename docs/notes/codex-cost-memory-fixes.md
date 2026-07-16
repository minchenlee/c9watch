# Codex Cost and Memory follow-up plan

Date: 2026-07-13
Branch: `feature/codex-monitoring`
PR: #112

## Goal

Complete the manual-test follow-up for the Cost and Memory tabs without changing the already-working Monitor and History behavior.

## Track 1: CLI verification

- Use the feature branch CLI to verify Codex sessions appear in `list`, `history`, and `search`.
- Check provider, surface, and agent-kind metadata where exposed.
- Read-only only; report exact commands and results.

## Track 2: Cost

- Estimate Codex USD cost from the public OpenAI API Standard short-context pricing table.
- Source: <https://developers.openai.com/api/docs/pricing>, checked 2026-07-13.
- Required model rates per 1M tokens:
  - `gpt-5.6-sol`: input 5.00, cached input 0.50, output 30.00.
  - `gpt-5.6-terra`: input 2.50, cached input 0.25, output 15.00.
  - `gpt-5.6-luna`: input 1.00, cached input 0.10, output 6.00.
  - `gpt-5.5`: input 5.00, cached input 0.50, output 30.00.
  - `gpt-5.4`: input 2.50, cached input 0.25, output 15.00.
  - `gpt-5.4-mini`: input 0.75, cached input 0.075, output 4.50.
  - `gpt-5.4-nano`: input 0.20, cached input 0.02, output 1.25.
  - `gpt-5.3-codex`: input 1.75, cached input 0.175, output 14.00.
- Treat cost as an estimate. Do not imply it is the user's ChatGPT/Codex subscription bill.
- Do not double-count cached input or reasoning tokens.
- Unknown models must remain unpriced.
- Use blue for Codex usage in Cost visualizations and keep Claude Code visually distinct.
- Keep each session's token/cost value on the same row as its badge, prompt, date, and model.

## Track 3: Memory

- Add Codex memory discovery from the real `~/.codex/memories` layout.
- Do not read rollout transcripts into the Memory tab. Prefer the durable top-level memory documents used by Codex.
- Add provider metadata to memory project/group records while remaining backward compatible with Claude Code data.
- Make the global `All | Claude Code | Codex` filter actually filter Memory data.
- Show a provider badge and provider-correct empty state/path/action text.
- Preserve existing Claude Code memory behavior.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml`
- `./node_modules/.bin/svelte-check --tsconfig ./tsconfig.json`
- CLI smoke checks for `list`, `history`, and `search`.
- Add focused Rust/frontend tests for pricing, Codex memory discovery, and filtering helpers where practical.

## Integration rules

- Each implementation track works in its own worktree and commits locally.
- Do not push or open another PR. The PM will integrate into PR #112 after both tracks pass review.
- Stop and report on conflicts; do not resolve unexpected conflicts independently.
