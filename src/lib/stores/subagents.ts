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

let pollHandle: ReturnType<typeof setInterval> | null = null;
let refreshInFlight = false;
let generation = 0;

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
	}
}

/**
 * Start polling for subagent updates. Triggered by the sessions store update
 * (re-fetch when sessions change) and on a slower fixed interval as a backstop.
 */
export function initializeSubagentPolling() {
	if (pollHandle !== null) return;
	// Re-fetch when the sessions list changes — that's our cheapest signal that
	// new transcript entries may have appeared.
	const unsub = sessions.subscribe(() => {
		refreshOnce();
	});
	// Fixed-interval backstop in case sessions are quiet but a long-running
	// subagent finishes mid-cycle (the parent session's own status can stay
	// unchanged across a subagent's whole run, so the sessions-store trigger
	// above won't always catch it). 20s rather than 4s: the primary trigger
	// already covers the common case at roughly the main poll's ~3.5s
	// cadence, so a tight fixed interval here was mostly firing redundant,
	// near-duplicate refreshes rather than adding real responsiveness.
	pollHandle = setInterval(refreshOnce, 20_000);
	// Initial fetch
	refreshOnce();
	// Return a teardown for tests/HMR.
	return () => {
		generation++;
		if (pollHandle !== null) {
			clearInterval(pollHandle);
			pollHandle = null;
		}
		unsub();
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, SubagentInfo[]> {
	return get(subagentsBySession);
}
