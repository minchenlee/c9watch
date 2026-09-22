/**
 * Store for subagent invocations detected by parsing parent session JSONL.
 *
 * No hook required: the backend (`get_subagents` Tauri command) re-scans each
 * project's JSONL files for Agent/Task tool_use entries and reports which
 * still lack a matching tool_result. This is a desktop-only store today;
 * WebSocket clients do not invent a live-id payload for an endpoint the
 * protocol does not expose.
 */

import { writable, derived, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { sessions } from './sessions';
import { isTauri } from '../ws';
import { providerOf, providerSessionKey } from '../provider';

export type SubagentStatus = 'running' | 'completed';

export interface SubagentInfo {
	id: string;
	agentType: string;
	description: string;
	startedAt: string;
	completedAt: string | null;
	parentSessionId: string;
	status: SubagentStatus;
	provider?: 'claudeCode' | 'codex' | 'cursor' | 'pi' | 'opencode';
	sessionId?: string;
}

/** Raw map: parent session id -> subagents */
export const subagentsBySession = writable<Map<string, SubagentInfo[]>>(new Map());

/** How long to keep completed subagents visible after they finish, in ms. */
const COMPLETED_RETENTION_MS = 60_000;

/**
 * Derived view: same map but with stale completed subagents filtered out.
 * Components should consume this rather than the raw store.
 */
export const visibleSubagentsBySession = derived(subagentsBySession, ($map) => {
	const out = new Map<string, SubagentInfo[]>();
	const now = Date.now();
	for (const [pid, list] of $map.entries()) {
		const filtered = list.filter((s) => {
			if (s.status === 'running') return true;
			if (!s.completedAt) return true;
			const completedMs = new Date(s.completedAt).getTime();
			return now - completedMs < COMPLETED_RETENTION_MS;
		});
		if (filtered.length > 0) {
			out.set(pid, filtered);
		}
	}
	return out;
});

/**
 * Backstop: if nothing has triggered a refresh in this long, do one anyway.
 * Covers a subagent finishing while its parent session's own status never
 * changes (so the sessions-store trigger below wouldn't otherwise catch it).
 * The timer is reset by every completed refresh, from whatever triggered
 * it, so it only ever fires during a genuinely quiet period — it's never
 * racing or duplicating the sessions-store trigger, which is what lets it
 * run this tight without adding back the redundant-refresh cost a fixed,
 * independent interval had.
 */
const BACKSTOP_MS = 5000;

let backstopTimer: ReturnType<typeof setTimeout> | null = null;
let refreshInFlight = false;
let refreshPending = false;
let generation = 0;
let initialized = false;

function scheduleBackstop() {
	if (backstopTimer !== null) clearTimeout(backstopTimer);
	backstopTimer = setTimeout(requestRefresh, BACKSTOP_MS);
}

function requestRefresh() {
	if (!isTauri()) return;
	if (refreshInFlight) {
		refreshPending = true;
		return;
	}
	void refreshOnce();
}

async function refreshOnce() {
	if (!isTauri()) return;
	if (refreshInFlight) {
		refreshPending = true;
		return;
	}
	refreshInFlight = true;
	refreshPending = false;
	const requestGeneration = generation;
	try {
		// The backend only stat/cache-checks session files that are either
		// currently live or already known to have something relevant, so it
		// needs to know which sessions are live right now. We already have
		// that here — it's exactly what triggered this refresh — so pass it
		// along instead of making the backend spawn a second `claude agents
		// --json` per poll to re-derive the same set.
		// The Rust command accepts raw Claude Code IDs only. Provider-scoped
		// keys such as `claudeCode:<id>` belong to the response/UI map, and
		// other providers must not widen the filesystem scan's live set.
		const sessionIds = get(sessions)
			.filter((s) => providerOf(s) === 'claudeCode')
			.map((s) => s.id);
		const raw = await invoke<Record<string, SubagentInfo[]>>('get_subagents', { sessionIds });
		const m = new Map<string, SubagentInfo[]>();
		for (const [k, v] of Object.entries(raw)) {
			// Accept the provider-scoped key from current backends and normalize
			// older Claude-only payloads that used the raw session ID.
			m.set(k.includes(':') ? k : providerSessionKey('claudeCode', k), v);
		}
		if (requestGeneration === generation) subagentsBySession.set(m);
	} catch {
		// Backend may be unavailable during startup or teardown; ignore.
	} finally {
		refreshInFlight = false;
		const rerun = initialized && (refreshPending || requestGeneration !== generation);
		refreshPending = false;
		if (rerun) {
			// Teardown/reinitialize can overlap the old request. Make the new
			// generation own a refresh instead of relying on the old finally to
			// schedule a timer that it is no longer allowed to schedule.
			requestRefresh();
		} else if (initialized && requestGeneration === generation) {
			scheduleBackstop();
		}
	}
}

/**
 * Start polling for subagent updates. Triggered by the sessions store update
 * (re-fetch when sessions change), with a reset-on-activity backstop so a
 * quiet period still gets refreshed — see `scheduleBackstop`.
 */
export function initializeSubagentPolling() {
	if (initialized) return;
	initialized = true;
	// Re-fetch when the sessions list changes — that's our cheapest signal that
	// new transcript entries may have appeared.
	const unsub = sessions.subscribe(() => {
		requestRefresh();
	});
	// Svelte subscriptions synchronously receive the current value, so the
	// subscription above performs the initial fetch and coalesces it naturally.
	// Return a teardown for tests/HMR.
	return () => {
		generation++;
		initialized = false;
		refreshPending = false;
		if (backstopTimer !== null) {
			clearTimeout(backstopTimer);
			backstopTimer = null;
		}
		unsub();
		subagentsBySession.set(new Map());
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, SubagentInfo[]> {
	return get(subagentsBySession);
}
