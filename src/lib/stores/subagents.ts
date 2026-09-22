/**
 * Store for subagent invocations detected by parsing parent session JSONL.
 *
 * No hook required: the backend (`get_subagents` Tauri command) re-scans each
 * project's JSONL files for Agent/Task tool_use entries and reports which
 * still lack a matching tool_result.
 */

import { writable, derived, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { sessions } from './sessions';
import { isTauri } from '../ws';
import { providerSessionKey } from '../provider';

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
let generation = 0;
let initialized = false;

function scheduleBackstop() {
	if (backstopTimer !== null) clearTimeout(backstopTimer);
	backstopTimer = setTimeout(refreshOnce, BACKSTOP_MS);
}

async function refreshOnce() {
	if (!isTauri() || refreshInFlight) return;
	refreshInFlight = true;
	const requestGeneration = generation;
	try {
		// The backend only stat/cache-checks session files that are either
		// currently live or already known to have something relevant, so it
		// needs to know which sessions are live right now. We already have
		// that here — it's exactly what triggered this refresh — so pass it
		// along instead of making the backend spawn a second `claude agents
		// --json` per poll to re-derive the same set.
		const sessionIds = get(sessions).map((s) => s.id);
		const raw = await invoke<Record<string, SubagentInfo[]>>('get_subagents', { sessionIds });
			const m = new Map<string, SubagentInfo[]>();
			for (const [k, v] of Object.entries(raw)) {
				// Accept the provider-scoped key from current backends and normalize
				// older Claude-only payloads that used the raw session ID.
				m.set(k.includes(':') ? k : providerSessionKey('claudeCode', k), v);
			}
		if (requestGeneration === generation) subagentsBySession.set(m);
	} catch {
		// Backend may be unavailable in non-Tauri contexts; ignore.
	} finally {
		refreshInFlight = false;
		if (requestGeneration === generation) scheduleBackstop();
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
		refreshOnce();
	});
	// Initial fetch (also schedules the first backstop, in its `finally`).
	refreshOnce();
	// Return a teardown for tests/HMR.
	return () => {
		generation++;
		initialized = false;
		if (backstopTimer !== null) {
			clearTimeout(backstopTimer);
			backstopTimer = null;
		}
		unsub();
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, SubagentInfo[]> {
	return get(subagentsBySession);
}
