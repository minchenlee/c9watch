/**
 * Centralized frontend feature flags.
 *
 * These are compile-time constants intentionally set to their shipping
 * defaults. Flip a flag here (and rebuild) to opt into the corresponding
 * behavior — there is no runtime UI toggle.
 */

/**
 * PM/worker orchestration UI (HUMANS/WORKERS toggle, WORKER/PM badges,
 * worker title prefix, and the Workers side panel).
 *
 * Disabled by default: Claude Code and Codex now provide native agent
 * spawning, so c9watch focuses on monitoring rather than presenting its own
 * orchestration workflow. When off, every session is still shown normally —
 * sessions carrying legacy `workerOf` metadata render as ordinary sessions —
 * and the native Subagents panel is unaffected.
 *
 * Pairs with the Rust `pm-orchestration` Cargo feature, which gates the CLI.
 */
export const PM_ORCHESTRATION_ENABLED = false;
